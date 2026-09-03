//! CLI dispatch: applet routing and shared helpers
//!
//! Named applets go through generated [`usage::RunAsyncWith`] on [`Applet`].
//! The root `Option` is decided here: generated dispatch cannot.

use std::io::Write;
use std::str::FromStr;

use anyhow::{Context, bail};
use usage::{RunAsyncWith, RunWith};

use crate::cli::{
    self, ActiveArgs, Applet, AtomArgs, CleanArgs, CrossdevArgs, DepcleanArgs, EbuildArgs,
    EmergeArgs, EmergeModeArgs, EnvArgs, EtcArgs, GrepArgs, HelperArgs, LogArgs, LogCommand,
    MaintArgs, MaintCommand, MirrorDistArgs, PkgArgs, PortageqArgs, QueryArgs, QueryCommand,
    QuickpkgArgs, ReadArgs, RegenArgs, RevdepArgs, SearchArgs, SelectArgs, SetupArgs, StagesArgs,
    SyncArgs, ToolchainArgs, UseArgs, WorkerArgs, CompletionArgs,
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
    if cli.info
        && match &cli.applet {
            None => true,
            Some(Applet::Emerge(a)) => a.atoms.is_empty(),
            Some(_) => false,
        }
    {
        return crate::info::run(cli).await;
    }
    // Clone: overlay selectors (`merge_flags()`, `roots()`, …) still read `cli.applet`.
    match cli.applet.clone() {
        Some(applet) => applet.run_async_with(cli).await,
        None => {
            if cli.mode() != EmergeModeArgs::default() {
                return emerge::run_emerge(cli).await;
            }
            crate::style::error_line!("no atoms or applet specified. Use --help for usage.");
            std::process::exit(1);
        }
    }
}

impl RunAsyncWith<&cli::Cli> for HelperArgs {
    type Output = Result<()>;

    async fn run_async_with(self, _cli: &cli::Cli) -> Self::Output {
        // Internal helper shim: run the helper and exit with its status
        // (the shim's caller — `find -exec`/`xargs` — checks it).
        std::process::exit(portage_repo::run_helper(&self.name, &self.args).await);
    }
}

impl RunAsyncWith<&cli::Cli> for WorkerArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        let worker_extra_path: Vec<camino::Utf8PathBuf> = self
            .extra_path
            .iter()
            .flat_map(|p| p.split(':'))
            .map(camino::Utf8PathBuf::from)
            .collect();
        ebuild::run_install_worker(ebuild::InstallWorker {
            ebuild_path: &self.ebuild,
            cpv_str: &self.cpv,
            use_flags_str: &self.use_flags,
            work_base: &self.work_base,
            root: &self.root,
            distdir: self.distdir.as_deref(),
            roots: ebuild::RootContext {
                config_root: self
                    .worker_config_root
                    .as_deref()
                    .map(camino::Utf8Path::new),
                sysroot: self.sysroot.as_deref().map(camino::Utf8Path::new),
                eprefix: self.eprefix.as_deref().map(camino::Utf8Path::new),
                broot: self.broot.as_deref().map(camino::Utf8Path::new),
                self_contained_bootstrap: self.self_contained_bootstrap,
                extra_path: &worker_extra_path,
            },
            binpkg: self.binpkg.as_deref(),
            force_verify_signature: self.force_verify_signature,
            buildpkg: self.buildpkg,
            quiet: cli.quiet,
            activity_job_id: self.activity_job_id.as_deref(),
            activity_parent_job_id: self.activity_parent_job_id.as_deref(),
            activity_live_root: self.activity_live_root.as_deref(),
            activity_side: self.activity_side.as_deref(),
            activity_reemit_path: self.activity_reemit_path.as_deref(),
        })
        .await
    }
}

impl RunAsyncWith<&cli::Cli> for EbuildArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        let repo_override = cli.repo.as_deref();
        let roots = cli.roots();
        let broot = cli.host_roots();
        ebuild::run(
            &self.ebuild_path,
            &self.phase,
            self.work_dir.as_deref(),
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
}

impl RunAsyncWith<&cli::Cli> for MaintArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        run_maint(&self.command, cli).await
    }
}

impl RunAsyncWith<&cli::Cli> for PortageqArgs {
    type Output = Result<()>;

    async fn run_async_with(self, _cli: &cli::Cli) -> Self::Output {
        bail!("not implemented: portageq")
    }
}

impl RunAsyncWith<&cli::Cli> for SyncArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        maint::sync::run(&self.repos, cli).await
    }
}

impl RunAsyncWith<&cli::Cli> for DepcleanArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        let merge_flags = cli.merge_flags();
        crate::depclean::run_with_targets(cli, &self.atoms, &merge_flags).await
    }
}

impl RunAsyncWith<&cli::Cli> for RegenArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        regen::run(
            cli,
            &self.repos,
            &cli.repo_path(),
            self.repos_dir.as_deref(),
            self.output.clone(),
            self.jobs,
            self.dedup,
        )
        .await
    }
}

impl RunAsyncWith<&cli::Cli> for QuickpkgArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        crate::quickpkg::run(
            cli,
            &crate::quickpkg::QuickpkgOpts {
                atoms: self.atoms.clone(),
                include_config: self.include_config,
                include_unmodified_config: self.include_unmodified_config,
            },
        )
        .await
    }
}

impl RunAsyncWith<&cli::Cli> for MirrorDistArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        let deletion_delay = humantime::parse_duration(&self.deletion_delay)
            .with_context(|| format!("--deletion-delay {:?}", self.deletion_delay))?;
        crate::mirrordist::run(
            cli,
            &crate::mirrordist::MirrorDistOpts {
                repo: self.repo.clone(),
                repos_dir: self.repos_dir.clone(),
                distfiles: self.distfiles.clone(),
                jobs: self.jobs,
                delete: self.delete,
                deletion_delay,
                deletion_db: self.deletion_db.clone(),
                success_log: self.success_log.clone(),
                failure_log: self.failure_log.clone(),
                scheduled_deletion_log: self.scheduled_deletion_log.clone(),
                whitelist_from: self.whitelist_from.clone(),
                verify_existing_digest: self.verify_existing_digest,
                gentoo_mirrors_fallback: self.gentoo_mirrors_fallback,
                delete_allow_incomplete: self.delete_allow_incomplete,
            },
        )
        .await
    }
}

impl RunAsyncWith<&cli::Cli> for QueryArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        run_query(&self.command, cli).await
    }
}

impl RunAsyncWith<&cli::Cli> for CleanArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        crate::clean::run(cli, &self.target).await
    }
}

impl RunAsyncWith<&cli::Cli> for UseArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        use_flags::run(
            cli,
            &use_flags::UseOpts {
                add: &self.add,
                subtract: &self.subtract,
                drop: &self.drop,
                dry_run: self.dry_run,
                expand: self.expand.as_deref(),
                list_expand: self.list_expand,
                info: &self.info,
                global: self.global,
                local_desc: self.local_desc,
                make_conf: self.make_conf.as_deref(),
            },
        )
        .await
    }
}

impl RunAsyncWith<&cli::Cli> for PkgArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        pkg::run(&self.command, cli).await
    }
}

impl RunAsyncWith<&cli::Cli> for RevdepArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        crate::revdep::run(cli, self.library.as_deref()).await
    }
}

impl RunAsyncWith<&cli::Cli> for ReadArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        crate::elog::run_read(
            cli,
            self.package.as_deref(),
            self.list,
            self.limit,
            self.delete,
        )
        .await
    }
}

impl RunAsyncWith<&cli::Cli> for GrepArgs {
    type Output = Result<()>;

    async fn run_async_with(self, _cli: &cli::Cli) -> Self::Output {
        bail!("not implemented: grep")
    }
}

impl RunAsyncWith<&cli::Cli> for SearchArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        search::run(
            &cli.search_repos(),
            self.pattern.as_deref(),
            self.all,
            self.desc,
            self.name_only,
            self.homepage,
        )
        .await
    }
}

impl RunAsyncWith<&cli::Cli> for SelectArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        select::run(&self.command, cli).await
    }
}

impl RunAsyncWith<&cli::Cli> for SetupArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        setup::run(cli, &self).await
    }
}

impl RunAsyncWith<&cli::Cli> for CrossdevArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        crossdev::run(&self, cli).await
    }
}

impl RunAsyncWith<&cli::Cli> for ToolchainArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        crossdev::toolchain(&self, cli).await
    }
}

impl RunAsyncWith<&cli::Cli> for StagesArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        crossdev::stage1(&self, cli).await
    }
}

impl RunAsyncWith<&cli::Cli> for EtcArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        crate::etc::run(cli, self.command.as_ref(), &self.opts).await
    }
}

impl RunAsyncWith<&cli::Cli> for EmergeArgs {
    type Output = Result<()>;

    async fn run_async_with(self, cli: &cli::Cli) -> Self::Output {
        emerge::run_emerge(cli).await
    }
}

impl RunWith<&cli::Cli> for LogArgs {
    type Output = Result<()>;

    fn run_with(self, cli: &cli::Cli) -> Self::Output {
        run_log(&self.command, cli)
    }
}

impl RunWith<&cli::Cli> for AtomArgs {
    type Output = Result<()>;

    fn run_with(self, _cli: &cli::Cli) -> Self::Output {
        run_atom(&self.atoms);
        Ok(())
    }
}

impl RunWith<&cli::Cli> for ActiveArgs {
    type Output = Result<()>;

    fn run_with(self, cli: &cli::Cli) -> Self::Output {
        crate::active::run(self.command.as_ref(), cli)
    }
}

impl RunWith<&cli::Cli> for EnvArgs {
    type Output = Result<()>;

    fn run_with(self, cli: &cli::Cli) -> Self::Output {
        maint::env::env_update(cli.roots().merge_root())
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


impl RunAsyncWith<&cli::Cli> for CompletionArgs {
    type Output = Result<()>;

    async fn run_async_with(self, _cli: &cli::Cli) -> Self::Output {
        let Some(shell) = usage::complete::Shell::from_name(&self.shell) else {
            bail!(
                "unsupported shell {:?}; expected bash, zsh, fish, nu, powershell, or elvish",
                self.shell
            );
        };
        println!("{}", cli::Cli::completion_script(shell));
        Ok(())
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
