//! CLI dispatch: applet routing and shared helpers.

use std::io::Write;
use std::str::FromStr;

use anyhow::bail;

use crate::cli::{
    self, Applet, CleanTarget, GlsaCommand, LogCommand, MaintCommand, NewsCommand, QueryCommand,
};
use crate::crossdev;
use crate::ebuild;
use crate::emerge;
use crate::error::Result;
use crate::vdb::open_cli_vdb;
use crate::{binpkg, maint, pkg, query, regen, search, select, setup, use_flags, vdb};

/// Dispatch one parsed invocation to its applet or the default emerge path.
pub(crate) async fn run(cli: &cli::Cli) -> Result<()> {
    match &cli.applet {
        Some(applet) => run_applet(applet, cli).await,
        None => {
            // `-c`/`--depclean` with no atoms is its primary mode ("clean
            // everything unreachable from @world"); `-r`/`--resume` takes
            // its package list from the saved state, never the command
            // line — both are legitimate bare (no-atom) invocations, unlike
            // every other bare invocation, which needs at least one atom to
            // act on.
            if cli.atoms.is_empty() && !cli.depclean && !cli.resume {
                eprintln!("em: no atoms or applet specified. Use --help for usage.");
                std::process::exit(1);
            }
            emerge::run_emerge(cli).await
        }
    }
}
async fn run_applet(applet: &Applet, globals: &cli::Cli) -> Result<()> {
    match applet {
        // Internal helper shim entry point: run the helper and exit with its
        // status (the shim's caller — `find -exec`/`xargs` — checks it).
        Applet::Helper { name, args } => {
            std::process::exit(portage_repo::run_helper(name, args).await);
        }
        Applet::Worker {
            ebuild,
            cpv,
            use_flags,
            work_base,
            root,
            distdir,
            config_root,
            sysroot,
            eprefix,
            broot,
            self_contained_bootstrap,
            binpkg,
            force_verify_signature,
            buildpkg,
            quiet,
            activity_job_id,
            activity_parent_job_id,
            activity_live_root,
            activity_side,
            activity_reemit_path,
        } => {
            ebuild::run_install_worker(ebuild::InstallWorker {
                ebuild_path: ebuild,
                cpv_str: cpv,
                use_flags_str: use_flags,
                work_base,
                root,
                distdir: distdir.as_deref(),
                roots: ebuild::RootContext {
                    config_root: config_root.as_deref().map(camino::Utf8Path::new),
                    sysroot: sysroot.as_deref().map(camino::Utf8Path::new),
                    eprefix: eprefix.as_deref().map(camino::Utf8Path::new),
                    broot: broot.as_deref().map(camino::Utf8Path::new),
                    self_contained_bootstrap: *self_contained_bootstrap,
                },
                binpkg: binpkg.as_deref(),
                force_verify_signature: *force_verify_signature,
                buildpkg: *buildpkg,
                quiet: *quiet,
                activity_job_id: activity_job_id.as_deref(),
                activity_parent_job_id: activity_parent_job_id.as_deref(),
                activity_live_root: activity_live_root.as_deref(),
                activity_side: activity_side.as_deref(),
                activity_reemit_path: activity_reemit_path.as_deref(),
            })
            .await
        }
        Applet::Ebuild {
            ebuild_path,
            phase,
            work_dir,
        } => {
            let repo_override = globals.repo.as_deref();
            let roots = globals.roots();
            let broot = globals.broot();
            ebuild::run(
                ebuild_path,
                phase,
                work_dir.as_deref(),
                repo_override,
                roots.merge_root(),
                ebuild::RootContext {
                    config_root: roots.config(),
                    sysroot: roots.build_sysroot(),
                    eprefix: roots.eprefix(),
                    broot: Some(broot.merge_root()),
                    self_contained_bootstrap: false,
                },
            )
            .await
        }
        Applet::Maint { command } => run_maint(command, globals).await,
        Applet::Portageq { command, args } => {
            eprintln!("portageq: command={} args={:?}", command, args);
            bail!("not implemented: portageq")
        }
        Applet::Sync { repos } => maint::sync::run(repos, globals).await,
        Applet::Depclean { atoms } => crate::depclean::run_with_targets(globals, atoms).await,
        Applet::Regen {
            repos,
            output,
            repos_dir,
            jobs,
            dedup,
        } => {
            regen::run(
                globals,
                repos,
                &globals.repo_path(),
                repos_dir.as_deref(),
                output.clone(),
                *jobs,
                *dedup,
            )
            .await
        }
        Applet::Quickpkg {
            atoms,
            include_config,
            include_unmodified_config,
        } => {
            let yn = |s: &str, flag: &str| -> Result<bool> {
                match s.trim().to_ascii_lowercase().as_str() {
                    "y" | "yes" | "true" | "1" => Ok(true),
                    "n" | "no" | "false" | "0" => Ok(false),
                    _ => bail!("{flag}: expected y|n, got {s:?}"),
                }
            };
            crate::quickpkg::run(
                globals,
                &crate::quickpkg::QuickpkgOpts {
                    atoms: atoms.clone(),
                    include_config: yn(include_config, "--include-config")?,
                    include_unmodified_config: yn(
                        include_unmodified_config,
                        "--include-unmodified-config",
                    )?,
                },
            )
            .await
        }
        Applet::Mirror { args } => {
            eprintln!("mirror: args={:?}", args);
            bail!("not implemented: mirror")
        }
        Applet::Pkg { command } => pkg::run(command, globals),
        Applet::Query { command } => run_query(command, globals).await,
        Applet::Clean { target } => run_clean(target),
        Applet::Use {
            add,
            remove,
            make_conf,
        } => use_flags::run(add, remove, make_conf.as_deref()),
        Applet::Revdep { args } => {
            eprintln!("revdep: args={:?}", args);
            bail!("not implemented: revdep")
        }
        Applet::Read { args } => {
            eprintln!("read: args={:?}", args);
            bail!("not implemented: read")
        }
        Applet::News { command } => run_news(command),
        Applet::Glsa { command } => run_glsa(command),
        Applet::Log { command } => run_log(command, globals),
        Applet::Grep { pattern, paths } => {
            eprintln!("grep: pattern={} paths={:?}", pattern, paths);
            bail!("not implemented: grep")
        }
        Applet::Search {
            all,
            desc,
            name_only,
            homepage,
            pattern,
        } => {
            search::run(
                &globals.search_repos(),
                pattern.as_deref(),
                *all,
                *desc,
                *name_only,
                *homepage,
            )
            .await
        }
        Applet::Atom { atoms } => {
            run_atom(atoms);
            Ok(())
        }
        Applet::Select { command } => select::run(command, globals).await,
        Applet::Active { command } => crate::active::run(command.as_ref(), globals),
        Applet::Setup => setup::bootstrap(&globals.roots()),
        Applet::Crossdev(args) => crossdev::run(args, globals).await,
        Applet::Toolchain(args) => crossdev::toolchain(args, globals).await,
        Applet::Stages(args) => crossdev::stage1(args, globals).await,
        Applet::Dispatch => {
            eprintln!("dispatch-conf");
            bail!("not implemented: dispatch-conf")
        }
        Applet::Etc => {
            eprintln!("etc-update");
            bail!("not implemented: etc-update")
        }
        Applet::Env => maint::env::env_update(globals.roots().merge_root()),
    }
}

async fn run_maint(command: &Option<MaintCommand>, globals: &cli::Cli) -> Result<()> {
    match command {
        None => bail!("not implemented: emaint (no subcommand)"),
        Some(MaintCommand::All) => bail!("not implemented: emaint all"),
        Some(MaintCommand::Binhost) => maint::binhost::run(globals).await,
        Some(MaintCommand::Binpkg { action }) => maint::binpkg::run(action, globals).await,
        Some(MaintCommand::Cleanconfmem) => bail!("not implemented: emaint cleanconfmem"),
        Some(MaintCommand::Cleanresume { fix }) => {
            let roots = globals.roots();
            let report = maint::resume::cleanresume(roots.merge_root(), *fix)?;
            if report.is_empty() {
                println!("No saved resume lists.");
            } else {
                for msg in &report {
                    println!("{msg}");
                }
                // `cleanresume` already appends a "Cleared …" line when
                // `--fix` actually wrote; only nudge the check-only path.
                if !*fix {
                    println!("Run with --fix to discard them.");
                }
            }
            Ok(())
        }
        Some(MaintCommand::Logs) => bail!("not implemented: emaint logs"),
        Some(MaintCommand::Merges) => bail!("not implemented: emaint merges"),
        Some(MaintCommand::Movebin) => bail!("not implemented: emaint movebin"),
        Some(MaintCommand::Moveinst) => {
            let vdb = open_cli_vdb(globals)?;
            let resolved = globals.repo_path();
            let repo_path = camino::Utf8Path::new(&resolved);
            maint::moveinst::run(repo_path, &vdb)
        }
        Some(MaintCommand::RegenUse { output }) => {
            let resolved = globals.repo_path();
            let repo_path = camino::Utf8Path::new(&resolved);
            maint::regen_use::run(repo_path, output.as_deref())
        }
        Some(MaintCommand::Revisions { repos }) => {
            let roots = globals.roots();
            maint::revisions::run(repos, roots.target())
        }
        Some(MaintCommand::Sync { repos }) => maint::sync::run(repos, globals).await,
        Some(MaintCommand::World { fix }) => {
            let vdb = open_cli_vdb(globals)?;
            let roots = globals.roots();
            let resolved = globals.repo_path();
            // Without the tree the check can only see the VDB, so an entry
            // whose ebuild is gone or masked here passes silently.
            let tree = maint::world::TreeView::load(
                camino::Utf8Path::new(&resolved),
                &roots,
                &globals.arch,
                globals.repo.is_none(),
            )
            .await
            .map_err(|e| {
                eprintln!("warning: ebuild availability not checked: {e:#}");
            })
            .ok();
            maint::world::run(&vdb, *fix, roots.target(), tree.as_ref())
        }
    }
}

async fn run_query(command: &QueryCommand, globals: &cli::Cli) -> Result<()> {
    match command {
        QueryCommand::Belongs { file } => {
            let vdb = open_cli_vdb(globals)?;
            vdb::query_belongs(&vdb, file);
            Ok(())
        }
        QueryCommand::Check { atom } => {
            let vdb = open_cli_vdb(globals)?;
            query::check::run(&vdb, atom)
        }
        QueryCommand::Depends { atom } => {
            let vdb = open_cli_vdb(globals).ok();
            query::depends::run(
                &std::path::PathBuf::from(globals.repo_path()),
                vdb.as_ref(),
                query::ResolveMode::Error,
                atom,
            )
        }
        QueryCommand::Depgraph {
            atom,
            format,
            autosolve_use,
            depgraph_flags,
            emptytree,
            onlydeps,
            with_bdeps,
            root_deps,
        } => {
            let resolved = globals.repo_path();
            let repo_path = camino::Utf8Path::new(&resolved);
            if !repo_path.is_dir() {
                bail!("repo not found at {resolved}");
            }
            let repo = crate::repo_open::open(repo_path.as_std_path())?;
            let vdb = open_cli_vdb(globals).ok();
            let parsed = query::resolve_atoms(atom, &repo, vdb.as_ref(), query::ResolveMode::Error);
            let atoms: Vec<query::depgraph::TargetAtom> = parsed
                .iter()
                .map(|d| query::depgraph::TargetAtom::explicit(d.to_string()))
                .collect();
            if atoms.is_empty() {
                bail!("equery depgraph: no valid atoms");
            }
            let roots = globals.roots();
            // See `DepgraphOpts::host_merge_root`: `Cli::broot()` stays
            // overlay-aware under `--target` substitution, unlike `roots`.
            let host_roots = globals.broot();
            let binpkg_index =
                binpkg::open_local_index_for_preview(globals, &globals.merge_flags).await;
            let outcome = query::depgraph::depgraph(query::depgraph::DepgraphOpts {
                repo_path,
                atoms: &atoms,
                arch: &globals.arch,
                format: *format,
                verbose: globals.verbose,
                empty: *emptytree || globals.merge_flags.emptytree,
                // equery depgraph is read-only: it reports autounmask candidates
                // (mask/keyword/USE fixes) but must never write them to
                // /etc/portage — that's `em`'s job, not a query command's.
                autounmask_write: false,
                autosolve_use: *autosolve_use || globals.merge_flags.autosolve_use,
                multi_repo: globals.repo.is_none(),
                roots: &roots,
                host_merge_root: host_roots.merge_root(),
                onlydeps: *onlydeps || globals.merge_flags.onlydeps,
                with_bdeps: *with_bdeps || globals.merge_flags.with_bdeps,
                root_deps_rdeps: *root_deps || globals.merge_flags.root_deps,
                deep: depgraph_flags.deep || globals.depgraph_flags.deep,
                update: globals.merge_flags.update,
                newuse: depgraph_flags.newuse || globals.depgraph_flags.newuse,
                changed_use: depgraph_flags.changed_use || globals.depgraph_flags.changed_use,
                noreplace: globals.merge_flags.noreplace,
                nodeps: globals.nodeps,
                extra_use_override: None,
                binpkg_index: binpkg_index.as_ref(),
                exclude: &globals.merge_flags.exclude,
                resume_completed: std::collections::HashSet::new(),
                complete_graph: globals.merge_flags.complete_graph,
            })
            .await?;
            if outcome.exit_code != 0 {
                std::process::exit(outcome.exit_code);
            }
            Ok(())
        }
        QueryCommand::Files { atom } => {
            let vdb = open_cli_vdb(globals)?;
            vdb::query_files(&vdb, atom);
            Ok(())
        }
        QueryCommand::Has { atom } => {
            let vdb = open_cli_vdb(globals)?;
            query::has::run(&vdb, atom)
        }
        QueryCommand::Hasuse { flag } => {
            query::hasuse::run(&std::path::PathBuf::from(globals.repo_path()), flag)
        }
        QueryCommand::Keywords { atom } => {
            let vdb = open_cli_vdb(globals).ok();
            query::keywords::run(
                &std::path::PathBuf::from(globals.repo_path()),
                vdb.as_ref(),
                query::ResolveMode::Error,
                atom,
            )
        }
        QueryCommand::List { installed, pattern } => {
            if *installed {
                let vdb = open_cli_vdb(globals)?;
                query::list::run_installed(&vdb, pattern);
                Ok(())
            } else {
                query::list::run(&std::path::PathBuf::from(globals.repo_path()), pattern)
            }
        }
        QueryCommand::Meta { atom } => {
            let vdb = open_cli_vdb(globals).ok();
            query::meta::run(
                &std::path::PathBuf::from(globals.repo_path()),
                vdb.as_ref(),
                query::ResolveMode::Error,
                atom,
            )
        }
        QueryCommand::Size { atom } => {
            let vdb = open_cli_vdb(globals)?;
            vdb::query_size(&vdb, atom)
        }
        QueryCommand::Uses { atom } => {
            let vdb = open_cli_vdb(globals).ok();
            query::uses::run(
                &std::path::PathBuf::from(globals.repo_path()),
                vdb.as_ref(),
                query::ResolveMode::Error,
                atom,
            )
        }
        QueryCommand::Which { atom } => {
            let vdb = open_cli_vdb(globals).ok();
            query::which::run(
                &std::path::PathBuf::from(globals.repo_path()),
                vdb.as_ref(),
                query::ResolveMode::Error,
                atom,
            )
        }
    }
}

fn run_clean(target: &Option<CleanTarget>) -> Result<()> {
    match target {
        None => bail!("not implemented: eclean (no target)"),
        Some(CleanTarget::Dist) => bail!("not implemented: eclean dist"),
        Some(CleanTarget::Pkg) => bail!("not implemented: eclean pkg"),
    }
}

fn run_news(command: &Option<NewsCommand>) -> Result<()> {
    match command {
        None => bail!("not implemented: news (no subcommand)"),
        Some(NewsCommand::Count) => bail!("not implemented: news count"),
        Some(NewsCommand::List) => bail!("not implemented: news list"),
        Some(NewsCommand::Read { id }) => {
            eprintln!("news: read {:?}", id);
            bail!("not implemented: news read")
        }
        Some(NewsCommand::Purge) => bail!("not implemented: news purge"),
    }
}

fn run_glsa(command: &Option<GlsaCommand>) -> Result<()> {
    match command {
        None => bail!("not implemented: glsa (no subcommand)"),
        Some(GlsaCommand::List) => bail!("not implemented: glsa list"),
        Some(GlsaCommand::Check { ids }) => {
            eprintln!("glsa: check {:?}", ids);
            bail!("not implemented: glsa check")
        }
        Some(GlsaCommand::Fix { ids }) => {
            eprintln!("glsa: fix {:?}", ids);
            bail!("not implemented: glsa fix")
        }
    }
}

fn run_log(command: &Option<LogCommand>, globals: &cli::Cli) -> Result<()> {
    let roots = globals.roots();
    match command {
        None | Some(LogCommand::Current) => {
            let proj = crate::activity::load_live_from_disk(roots.merge_root());
            let now = crate::activity::ActivityEvent::now();
            print!("{}", crate::activity::format_current(&proj, now));
            Ok(())
        }
        Some(LogCommand::List { limit }) => {
            let store = crate::activity::DurationStore::load(roots.merge_root());
            print!(
                "{}",
                crate::activity::format_list(&store, limit.map(|n| n as usize))
            );
            Ok(())
        }
        Some(LogCommand::Time { atom }) => {
            let store = crate::activity::DurationStore::load(roots.merge_root());
            print!("{}", crate::activity::format_time(&store, atom.as_deref()));
            Ok(())
        }
        Some(LogCommand::Predict) => {
            let proj = crate::activity::load_live_from_disk(roots.merge_root());
            let active = proj.active();
            if active.is_empty() {
                bail!("log predict: no ongoing activity session");
            }
            let store = crate::activity::DurationStore::load(roots.merge_root());
            for s in active {
                println!(
                    "session {}  root={}  done {}/{}  inflight {}",
                    s.job_id,
                    s.merge_root,
                    s.completed,
                    s.plan_total,
                    s.inflight.len()
                );
                let jobs = s.flags.jobs.unwrap_or(1);
                let (remaining, blockers) = s.remaining_for_eta();
                let eta = if !s.plan.is_empty() && blockers.len() == remaining.len() {
                    crate::activity::estimate_remaining_with_blockers(
                        &store, &remaining, &blockers, jobs, 15,
                    )
                } else {
                    // Legacy sessions without a stored plan: inflight + unknown pad.
                    let mut eta = crate::activity::estimate_remaining(&store, &remaining, jobs, 15);
                    let accounted = s.completed + s.failed + s.inflight.len() as u32;
                    let unknown_slots = s.plan_total.saturating_sub(accounted);
                    if unknown_slots > 0 {
                        if let Some(g) = store.global_median_seconds(30) {
                            eta.serial_seconds += g * unknown_slots as f64;
                            eta.wall_seconds = eta.serial_seconds / jobs.max(1) as f64;
                            eta.unknown += unknown_slots;
                        } else {
                            eta.unknown += unknown_slots;
                        }
                    }
                    eta
                };
                let mut out = anstream::stdout();
                let _ = write!(out, "{}", crate::activity::format_eta(&eta));
                let _ = out.flush();
            }
            Ok(())
        }
    }
}
fn run_atom(atoms: &[String]) {
    for raw in atoms {
        match portage_atom::Dep::from_str(raw) {
            Ok(dep) => println!("{dep}"),
            Err(e) => eprintln!("error: '{}': {}", raw, e),
        }
    }
}
