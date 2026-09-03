//! CLI dispatch: applet routing and shared helpers

use std::io::Write;
use std::str::FromStr;

use anyhow::{Context, bail};

use crate::cli::{
    self, Applet, CleanTarget, EmergeModeArgs, LogCommand, MaintCommand, QueryCommand,
};
use crate::crossdev;
use crate::ebuild;
use crate::emerge;
use crate::error::Result;
use crate::vdb::open_cli_vdb;
use crate::{binpkg, maint, pkg, query, regen, search, select, setup, use_flags, vdb};

/// Dispatch one parsed invocation to its applet or the default emerge path
///
/// `None` is leftover when default-subcommand emerge did not fire (`em -p`,
/// `em --info`, `em -r`). `--info` wins only for empty-atom emerge; a named
/// applet always wins, and `em --info firefox` emerges `firefox`.
pub(crate) async fn run(cli: &cli::Cli) -> Result<()> {
    match &cli.applet {
        Some(Applet::Emerge(args)) => {
            if cli.info && args.atoms.is_empty() {
                return crate::info::run(cli).await;
            }
            emerge::run_emerge(cli).await
        }
        Some(applet) => run_applet(applet, cli).await,
        None => {
            if cli.info {
                return crate::info::run(cli).await;
            }
            if cli.mode() != EmergeModeArgs::default() {
                return emerge::run_emerge(cli).await;
            }
            crate::style::error_line!("no atoms or applet specified. Use --help for usage.");
            std::process::exit(1);
        }
    }
}
async fn run_applet(applet: &Applet, globals: &cli::Cli) -> Result<()> {
    match applet {
        // Internal helper shim entry point: run the helper and exit with its
        // status (the shim's caller — `find -exec`/`xargs` — checks it).
        Applet::Helper(h) => {
            std::process::exit(portage_repo::run_helper(&h.name, &h.args).await);
        }
        Applet::Worker(w) => {
            let worker_extra_path: Vec<camino::Utf8PathBuf> = w
                .extra_path
                .iter()
                .flat_map(|p| p.split(':'))
                .map(camino::Utf8PathBuf::from)
                .collect();
            ebuild::run_install_worker(ebuild::InstallWorker {
                ebuild_path: &w.ebuild,
                cpv_str: &w.cpv,
                use_flags_str: &w.use_flags,
                work_base: &w.work_base,
                root: &w.root,
                distdir: w.distdir.as_deref(),
                roots: ebuild::RootContext {
                    config_root: w.worker_config_root.as_deref().map(camino::Utf8Path::new),
                    sysroot: w.sysroot.as_deref().map(camino::Utf8Path::new),
                    eprefix: w.eprefix.as_deref().map(camino::Utf8Path::new),
                    broot: w.broot.as_deref().map(camino::Utf8Path::new),
                    self_contained_bootstrap: w.self_contained_bootstrap,
                    extra_path: &worker_extra_path,
                },
                binpkg: w.binpkg.as_deref(),
                force_verify_signature: w.force_verify_signature,
                buildpkg: w.buildpkg,
                quiet: globals.quiet,
                activity_job_id: w.activity_job_id.as_deref(),
                activity_parent_job_id: w.activity_parent_job_id.as_deref(),
                activity_live_root: w.activity_live_root.as_deref(),
                activity_side: w.activity_side.as_deref(),
                activity_reemit_path: w.activity_reemit_path.as_deref(),
            })
            .await
        }
        Applet::Ebuild(a) => {
            let repo_override = globals.repo.as_deref();
            let roots = globals.roots();
            let broot = globals.host_roots();
            ebuild::run(
                &a.ebuild_path,
                &a.phase,
                a.work_dir.as_deref(),
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
        Applet::Maint(a) => run_maint(&a.command, globals).await,
        Applet::Portageq(_) => bail!("not implemented: portageq"),
        Applet::Sync(a) => maint::sync::run(&a.repos, globals).await,
        Applet::Depclean(a) => {
            let merge_flags = globals.merge_flags();
            crate::depclean::run_with_targets(globals, &a.atoms, &merge_flags).await
        }
        Applet::Regen(a) => {
            regen::run(
                globals,
                &a.repos,
                &globals.repo_path(),
                a.repos_dir.as_deref(),
                a.output.clone(),
                a.jobs,
                a.dedup,
            )
            .await
        }
        Applet::Quickpkg(a) => {
            crate::quickpkg::run(
                globals,
                &crate::quickpkg::QuickpkgOpts {
                    atoms: a.atoms.clone(),
                    include_config: a.include_config,
                    include_unmodified_config: a.include_unmodified_config,
                },
            )
            .await
        }
        Applet::MirrorDist(a) => {
            let deletion_delay = humantime::parse_duration(&a.deletion_delay)
                .with_context(|| format!("--deletion-delay {:?}", a.deletion_delay))?;
            crate::mirrordist::run(
                globals,
                &crate::mirrordist::MirrorDistOpts {
                    repo: a.repo.clone(),
                    repos_dir: a.repos_dir.clone(),
                    distfiles: a.distfiles.clone(),
                    jobs: a.jobs,
                    delete: a.delete,
                    deletion_delay,
                    deletion_db: a.deletion_db.clone(),
                    success_log: a.success_log.clone(),
                    failure_log: a.failure_log.clone(),
                    scheduled_deletion_log: a.scheduled_deletion_log.clone(),
                    whitelist_from: a.whitelist_from.clone(),
                    verify_existing_digest: a.verify_existing_digest,
                    gentoo_mirrors_fallback: a.gentoo_mirrors_fallback,
                    delete_allow_incomplete: a.delete_allow_incomplete,
                },
            )
            .await
        }
        Applet::Pkg(a) => pkg::run(&a.command, globals).await,
        Applet::Query(a) => run_query(&a.command, globals).await,
        Applet::Clean(a) => run_clean(globals, &a.target).await,
        Applet::Use(a) => {
            use_flags::run(
                globals,
                &use_flags::UseOpts {
                    add: &a.add,
                    subtract: &a.subtract,
                    drop: &a.drop,
                    dry_run: a.dry_run,
                    expand: a.expand.as_deref(),
                    list_expand: a.list_expand,
                    info: &a.info,
                    global: a.global,
                    local_desc: a.local_desc,
                    make_conf: a.make_conf.as_deref(),
                },
            )
            .await
        }
        Applet::Revdep(a) => crate::revdep::run(globals, a.library.as_deref()).await,
        Applet::Read(a) => {
            crate::elog::run_read(globals, a.package.as_deref(), a.list, a.limit, a.delete).await
        }
        Applet::Log(a) => run_log(&a.command, globals),
        Applet::Grep(_) => bail!("not implemented: grep"),
        Applet::Search(a) => {
            search::run(
                &globals.search_repos(),
                a.pattern.as_deref(),
                a.all,
                a.desc,
                a.name_only,
                a.homepage,
            )
            .await
        }
        Applet::Atom(a) => {
            run_atom(&a.atoms);
            Ok(())
        }
        Applet::Select(a) => select::run(&a.command, globals).await,
        Applet::Active(a) => crate::active::run(a.command.as_ref(), globals),
        Applet::Setup(args) => setup::run(globals, args).await,
        Applet::Crossdev(args) => crossdev::run(args, globals).await,
        Applet::Toolchain(args) => crossdev::toolchain(args, globals).await,
        Applet::Stages(args) => crossdev::stage1(args, globals).await,
        Applet::Etc(a) => crate::etc::run(globals, a.command.as_ref(), &a.opts).await,
        Applet::Env(_) => maint::env::env_update(globals.roots().merge_root()),
        Applet::Emerge(_) => emerge::run_emerge(globals).await,
    }
}

async fn run_maint(command: &MaintCommand, globals: &cli::Cli) -> Result<()> {
    match command {
        MaintCommand::Binhost => maint::binhost::run(globals).await,
        MaintCommand::Binpkg { action } => maint::binpkg::run(action, globals).await,
        MaintCommand::Cleanconfmem => {
            // Not a stub: portage's config tracker (`/var/lib/portage/config`)
            // records which protected files a user has already merged, and
            // `em` never writes one — so there is nothing here to go stale.
            println!("em keeps no config-memory file; nothing to clean.");
            Ok(())
        }
        MaintCommand::Cleanresume { fix } => {
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
        MaintCommand::Logs { fix, older_than } => {
            let roots = globals.roots();
            let work_base = crate::ebuild::default_work_base(roots.relocate_root());
            maint::logs::run(&work_base, older_than.as_deref(), *fix, "Run with --fix")
        }
        MaintCommand::Merges => bail!(
            "em maint merges needs a failed-merge registry, which em does not keep yet — \
             a failed package is reported at the end of the run and in its build log instead"
        ),
        MaintCommand::Movebin => {
            let pkgdir = crate::binpkg::resolve_pkgdir(globals).await;
            let resolved = globals.repo_path();
            maint::movebin::run(camino::Utf8Path::new(&resolved), &pkgdir)
        }
        MaintCommand::Moveinst => {
            let vdb = open_cli_vdb(globals)?;
            let resolved = globals.repo_path();
            let repo_path = camino::Utf8Path::new(&resolved);
            maint::moveinst::run(repo_path, &vdb)
        }
        MaintCommand::RegenUse { output } => {
            let resolved = globals.repo_path();
            let repo_path = camino::Utf8Path::new(&resolved);
            maint::regen_use::run(repo_path, output.as_deref())
        }
        MaintCommand::Revisions { repos } => {
            let roots = globals.roots();
            maint::revisions::run(repos, roots.target())
        }
        MaintCommand::Sync { repos } => maint::sync::run(repos, globals).await,
        MaintCommand::World { fix } => {
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
            // Query depgraph keeps its own flatten; do not overlay Cli MergeFlags here.
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
                extra_package_use: &[],
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
        QueryCommand::Has { field, value } => {
            let vdb = open_cli_vdb(globals)?;
            query::has::run(&vdb, field, value.as_deref())
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

async fn run_clean(globals: &cli::Cli, target: &CleanTarget) -> Result<()> {
    crate::clean::run(globals, target).await
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
