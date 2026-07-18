//! CLI dispatch: applet routing and shared helpers.

use std::str::FromStr;

use anyhow::bail;

use crate::cli::{
    self, Applet, CleanTarget, GlsaCommand, LogCommand, MaintCommand, NewsCommand, QueryCommand,
};
use crate::crossdev;
use crate::ebuild;
use crate::emerge::{self, parse_atoms};
use crate::error::Result;
use crate::vdb::open_cli_vdb;
use crate::{binpkg, maint, pkg, query, regen, search, select, setup, use_flags, vdb};

/// Dispatch one parsed invocation to its applet or the default emerge path.
pub(crate) async fn run(cli: &cli::Cli) -> Result<()> {
    match &cli.applet {
        Some(applet) => run_applet(applet, cli).await,
        None => {
            // `-c`/`--depclean` with no atoms is its primary mode ("clean
            // everything unreachable from @world"), unlike every other bare
            // invocation, which needs at least one atom to act on.
            if cli.atoms.is_empty() && !cli.depclean {
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
            buildpkg,
            quiet,
        } => {
            ebuild::run_install_worker(
                ebuild,
                cpv,
                use_flags,
                work_base,
                root,
                distdir.as_deref(),
                ebuild::RootContext {
                    config_root: config_root.as_deref().map(camino::Utf8Path::new),
                    sysroot: sysroot.as_deref().map(camino::Utf8Path::new),
                    eprefix: eprefix.as_deref().map(camino::Utf8Path::new),
                    broot: broot.as_deref().map(camino::Utf8Path::new),
                    self_contained_bootstrap: *self_contained_bootstrap,
                },
                binpkg.as_deref(),
                *buildpkg,
                *quiet,
            )
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
        Applet::Maint { command } => run_maint(command, globals),
        Applet::Portageq { command, args } => {
            eprintln!("portageq: command={} args={:?}", command, args);
            bail!("not implemented: portageq")
        }
        Applet::Sync { repos } => {
            eprintln!("sync: repos={:?}", repos);
            bail!("not implemented: sync")
        }
        Applet::Depclean { atoms } => crate::depclean::run_with_targets(globals, atoms).await,
        Applet::Regen {
            repos,
            output,
            repos_dir,
            jobs,
            dedup,
        } => {
            regen::run(
                repos,
                &globals.repo_path(),
                repos_dir.as_deref(),
                output.clone(),
                *jobs,
                *dedup,
            )
            .await
        }
        Applet::Quickpkg { atoms } => {
            let parsed = parse_atoms(atoms);
            eprintln!("quickpkg: atoms={:?}", parsed);
            bail!("not implemented: quickpkg")
        }
        Applet::Mirror { args } => {
            eprintln!("mirror: args={:?}", args);
            bail!("not implemented: mirror")
        }
        Applet::Pkg { command } => pkg::run(command),
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
        Applet::Log { command } => run_log(command),
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
        Applet::Atom { atoms } => run_atom(atoms),
        Applet::Select { command } => select::run(command, globals).await,
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

fn run_maint(command: &Option<MaintCommand>, globals: &cli::Cli) -> Result<()> {
    match command {
        None => bail!("not implemented: emaint (no subcommand)"),
        Some(MaintCommand::All) => bail!("not implemented: emaint all"),
        Some(MaintCommand::Binhost) => maint::binhost::run(globals),
        Some(MaintCommand::Binpkg { action }) => maint::binpkg::run(action, globals),
        Some(MaintCommand::Cleanconfmem) => bail!("not implemented: emaint cleanconfmem"),
        Some(MaintCommand::Cleanresume) => bail!("not implemented: emaint cleanresume"),
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
        Some(MaintCommand::Sync { repos }) => {
            eprintln!("emaint: sync repos={:?}", repos);
            bail!("not implemented: emaint sync")
        }
        Some(MaintCommand::World { fix }) => {
            let vdb = open_cli_vdb(globals)?;
            let roots = globals.roots();
            maint::world::run(&vdb, *fix, roots.target())
        }
    }
}

async fn run_query(command: &QueryCommand, globals: &cli::Cli) -> Result<()> {
    match command {
        QueryCommand::Belongs { file } => {
            let vdb = open_cli_vdb(globals)?;
            vdb::query_belongs(&vdb, file)
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
            let repo = portage_repo::Repository::open(repo_path.as_std_path())?;
            let vdb = open_cli_vdb(globals).ok();
            let parsed = query::resolve_atoms(atom, &repo, vdb.as_ref(), query::ResolveMode::Error);
            let atoms: Vec<String> = parsed.iter().map(|d| d.to_string()).collect();
            if atoms.is_empty() {
                bail!("equery depgraph: no valid atoms");
            }
            let roots = globals.roots();
            // See `DepgraphOpts::host_merge_root`: `Cli::broot()` stays
            // overlay-aware under `--target` substitution, unlike `roots`.
            let host_roots = globals.broot();
            let binpkg_index = binpkg::open_local_index_for_preview(globals, &globals.merge_flags);
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
                nodeps: globals.nodeps,
                extra_use_override: None,
                binpkg_index: binpkg_index.as_ref(),
            })
            .await?;
            if outcome.exit_code != 0 {
                std::process::exit(outcome.exit_code);
            }
            Ok(())
        }
        QueryCommand::Files { atom } => {
            let vdb = open_cli_vdb(globals)?;
            vdb::query_files(&vdb, atom)
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
                query::list::run_installed(&vdb, pattern)
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

fn run_log(command: &Option<LogCommand>) -> Result<()> {
    match command {
        None => bail!("not implemented: log (no subcommand)"),
        Some(LogCommand::Current) => bail!("not implemented: log current"),
        Some(LogCommand::List { limit }) => {
            eprintln!("log: list limit={:?}", limit);
            bail!("not implemented: log list")
        }
        Some(LogCommand::Time { atom }) => {
            eprintln!("log: time atom={:?}", atom);
            bail!("not implemented: log time")
        }
    }
}
fn run_atom(atoms: &[String]) -> Result<()> {
    for raw in atoms {
        match portage_atom::Dep::from_str(raw) {
            Ok(dep) => println!("{dep}"),
            Err(e) => eprintln!("error: '{}': {}", raw, e),
        }
    }
    Ok(())
}
