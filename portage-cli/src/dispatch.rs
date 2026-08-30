//! CLI dispatch: applet routing and shared helpers

use std::io::Write;
use std::str::FromStr;

use anyhow::{Context, bail};

use crate::cli::{self, Applet, CleanTarget, LogCommand, MaintCommand, QueryCommand};
use crate::crossdev;
use crate::ebuild;
use crate::emerge;
use crate::error::Result;
use crate::vdb::open_cli_vdb;
use crate::{binpkg, maint, pkg, query, regen, search, select, setup, use_flags, vdb};

/// Dispatch one parsed invocation to its applet or the default emerge path
///
/// `None` only reaches here for a genuinely applet-less invocation
/// (`em --info`, `em -p` with nothing else) — [`crate::cli::parse_cli_from`]
/// resolves any invocation carrying atoms or emerge-mode flags into
/// `Applet::Emerge` before this ever runs.
pub(crate) async fn run(cli: &cli::Cli) -> Result<()> {
    match &cli.applet {
        Some(Applet::Emerge(args)) => emerge::run_emerge(cli, args).await,
        Some(applet) => run_applet(applet, cli).await,
        None => {
            if cli.info {
                return crate::info::run(cli).await;
            }
            eprintln!("em: no atoms or applet specified. Use --help for usage.");
            std::process::exit(1);
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
            extra_path,
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
            let worker_extra_path: Vec<camino::Utf8PathBuf> = extra_path
                .iter()
                .flat_map(|p| p.split(':'))
                .map(camino::Utf8PathBuf::from)
                .collect();
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
                    extra_path: &worker_extra_path,
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
            ..
        } => {
            let repo_override = globals.repo.as_deref();
            let roots = globals.roots();
            let broot = globals.host_roots();
            ebuild::run(
                ebuild_path,
                phase,
                work_dir.as_deref(),
                repo_override,
                roots.merge_root(),
                ebuild::RootContext {
                    config_root: roots.config(),
                    sysroot: roots.build_sysroot(),
                    eprefix: roots.build_eprefix(),
                    broot: Some(broot.merge_root()),
                    self_contained_bootstrap: false,
                    extra_path: &[],
                },
            )
            .await
        }
        Applet::Maint { command, .. } => run_maint(command, globals).await,
        Applet::Portageq { command, args } => {
            eprintln!("portageq: command={} args={:?}", command, args);
            bail!("not implemented: portageq")
        }
        Applet::Sync { repos, .. } => maint::sync::run(repos, globals).await,
        Applet::Depclean {
            atoms, merge_flags, ..
        } => crate::depclean::run_with_targets(globals, atoms, merge_flags).await,
        Applet::Regen {
            repos,
            output,
            repos_dir,
            jobs,
            dedup,
            ..
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
            ..
        } => {
            crate::quickpkg::run(
                globals,
                &crate::quickpkg::QuickpkgOpts {
                    atoms: atoms.clone(),
                    include_config: *include_config,
                    include_unmodified_config: *include_unmodified_config,
                },
            )
            .await
        }
        Applet::MirrorDist {
            repo,
            repos_dir,
            distfiles,
            jobs,
            delete,
            deletion_delay,
            deletion_db,
            success_log,
            failure_log,
            scheduled_deletion_log,
            whitelist_from,
            verify_existing_digest,
            gentoo_mirrors_fallback,
            delete_allow_incomplete,
            ..
        } => {
            let deletion_delay = humantime::parse_duration(deletion_delay)
                .with_context(|| format!("--deletion-delay {deletion_delay:?}"))?;
            crate::mirrordist::run(
                globals,
                &crate::mirrordist::MirrorDistOpts {
                    repo: repo.clone(),
                    repos_dir: repos_dir.clone(),
                    distfiles: distfiles.clone(),
                    jobs: *jobs,
                    delete: *delete,
                    deletion_delay,
                    deletion_db: deletion_db.clone(),
                    success_log: success_log.clone(),
                    failure_log: failure_log.clone(),
                    scheduled_deletion_log: scheduled_deletion_log.clone(),
                    whitelist_from: whitelist_from.clone(),
                    verify_existing_digest: *verify_existing_digest,
                    gentoo_mirrors_fallback: *gentoo_mirrors_fallback,
                    delete_allow_incomplete: *delete_allow_incomplete,
                },
            )
            .await
        }
        Applet::Pkg { command, .. } => pkg::run(command, globals).await,
        Applet::Query { command, .. } => run_query(command, globals).await,
        Applet::Clean { target } => run_clean(target),
        Applet::Use {
            add,
            subtract,
            drop,
            dry_run,
            expand,
            list_expand,
            info,
            global,
            local_desc,
            make_conf,
            ..
        } => {
            use_flags::run(
                globals,
                &use_flags::UseOpts {
                    add,
                    subtract,
                    drop,
                    dry_run: *dry_run,
                    expand: expand.as_deref(),
                    list_expand: *list_expand,
                    info,
                    global: *global,
                    local_desc: *local_desc,
                    make_conf: make_conf.as_deref(),
                },
            )
            .await
        }
        Applet::Revdep { library, .. } => crate::revdep::run(globals, library.as_deref()).await,
        Applet::Read {
            package,
            list,
            limit,
            delete,
            ..
        } => crate::elog::run_read(globals, package.as_deref(), *list, *limit, *delete).await,
        Applet::Log { command, .. } => run_log(command, globals),
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
            ..
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
        Applet::Select { command, .. } => select::run(command, globals).await,
        Applet::Active { command, .. } => crate::active::run(command.as_ref(), globals),
        Applet::Setup(args) => setup::run(globals, args).await,
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
        Applet::Env { .. } => maint::env::env_update(globals.roots().merge_root()),
        Applet::Emerge(args) => emerge::run_emerge(globals, args).await,
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
                crate::style::warn_line!("ebuild availability not checked: {e:#}");
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
            let repo_path = std::path::PathBuf::from(globals.repo_path());
            let repo = crate::repo_open::open(&repo_path)?;
            // So bare-name atoms can resolve to an overlay-only package, not
            // just the main repo — see `query::resolve_atom`'s doc.
            let set = crate::repo_open::repo_set_from_conf(
                repo,
                &globals.roots(),
                globals.repo.is_none(),
            );
            query::depends::run(&set, vdb.as_ref(), query::ResolveMode::Error, atom).await
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
            let roots = globals.roots();
            // So bare-name atoms can resolve to an overlay-only package, not
            // just the main repo — see `query::resolve_atom`'s doc.
            let set = crate::repo_open::repo_set_from_conf(repo, &roots, globals.repo.is_none());
            let parsed = query::resolve_atoms(atom, &set, vdb.as_ref(), query::ResolveMode::Error);
            let atoms: Vec<query::depgraph::TargetAtom> = parsed
                .iter()
                .map(|d| query::depgraph::TargetAtom::explicit(d.to_string()))
                .collect();
            if atoms.is_empty() {
                // Same reasoning as emerge.rs's atoms.is_empty() check: each
                // failed atom already printed its own warning above.
                return Err(crate::error::NoValidAtoms.into());
            }
            // See `DepgraphOpts::host_merge_root`: `Cli::host_roots()` stays
            // overlay-aware under `--target` substitution, unlike `roots`.
            let host_roots = globals.host_roots();
            // `equery depgraph` has no `MergeFlags` of its own (its inline
            // fields above are the whole surface) — an empty `MergeFlags`
            // here, not a top-level one, since `Cli` no longer carries one to
            // read outside `Applet::Emerge`/the staged-build applets.
            let merge_flags = cli::MergeFlags::default();
            let binpkg_index = binpkg::open_local_index_for_preview(globals, &merge_flags).await;
            let outcome = query::depgraph::depgraph(query::depgraph::DepgraphOpts {
                set,
                atoms: &atoms,
                // Read-only query: it never records anything in the world
                // file, so only literal `@selected` membership bolds a row —
                // exactly how a `--oneshot` merge renders.
                world_additions: &[],
                arch: &globals.arch,
                format: *format,
                verbose: globals.verbose,
                empty: *emptytree,
                // equery depgraph is read-only: it reports autounmask candidates
                // (mask/keyword/USE fixes) but must never write them to
                // /etc/portage — that's `em`'s job, not a query command's.
                autounmask_write: false,
                autounmask_persist: query::depgraph::AutounmaskPersist::Never,
                ask: false,
                autosolve_use: *autosolve_use,
                autounmask_widen: false,
                roots: &roots,
                host_merge_root: host_roots.merge_root(),
                onlydeps: *onlydeps,
                with_bdeps: *with_bdeps,
                root_deps_rdeps: *root_deps,
                deep: depgraph_flags.deep,
                update: false,
                newuse: depgraph_flags.newuse,
                changed_use: depgraph_flags.changed_use,
                noreplace: false,
                nodeps: false,
                extra_use_override: None,
                sysroot_override: None,
                binpkg_index: binpkg_index.as_ref(),
                exclude: &merge_flags.exclude,
                resume_completed: std::collections::HashSet::new(),
                complete_graph: false,
                quiet: false,
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
            let repo_path = std::path::PathBuf::from(globals.repo_path());
            let repo = crate::repo_open::open(&repo_path)?;
            // So bare-name atoms can resolve to an overlay-only package, not
            // just the main repo — see `query::resolve_atom`'s doc.
            let set = crate::repo_open::repo_set_from_conf(
                repo,
                &globals.roots(),
                globals.repo.is_none(),
            );
            query::which::run(&set, vdb.as_ref(), query::ResolveMode::Error, atom)
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

/// Live sessions from the real merge root plus `em regen`'s own XDG activity
/// root (see `xdg::regen_activity_root`'s doc — regen's activity bus doesn't
/// live under the merge root, unlike a real merge's).
fn load_live_sessions(roots: &portage_resolve::Roots) -> crate::activity::LiveProjection {
    let mut proj = crate::activity::load_live_from_disk(roots.merge_root());
    proj.merge(crate::activity::load_live_from_disk(
        &crate::xdg::regen_activity_root(),
    ));
    proj
}

fn run_log(command: &Option<LogCommand>, globals: &cli::Cli) -> Result<()> {
    let roots = globals.roots();
    match command {
        None | Some(LogCommand::Current) => {
            let proj = load_live_sessions(&roots);
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
            let proj = load_live_sessions(&roots);
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
                let (remaining, blockers) = s.remaining_for_eta(&store);
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
            Err(e) => crate::style::error_line!("'{raw}': {e}"),
        }
    }
}
