//! Parallel and sequential merge scheduling.

use std::collections::VecDeque;

use anyhow::{Context, bail};
use futures_util::stream::{FuturesUnordered, StreamExt};

use crate::binpkg;
use crate::cli;
use crate::ebuild;
use crate::error::Result;
use crate::maint;
use crate::query;

/// One package's merge failure, for the end-of-run report.
struct MergeFailure {
    cpv: String,
    log: camino::Utf8PathBuf,
    cause: String,
}

/// Caller-facing inputs to [`run_merge_plan`] — keeps the public surface a
/// single struct instead of an ever-growing positional arg list.
pub(crate) struct MergePlanRequest<'a> {
    pub plan: &'a [query::depgraph::PlannedMerge],
    pub blockers: &'a [Vec<usize>],
    pub roots: &'a portage_resolve::Roots,
    pub work_base: &'a camino::Utf8Path,
    pub distdir: Option<&'a camino::Utf8Path>,
    pub merge_flags: &'a cli::MergeFlags,
    pub globals: &'a cli::Cli,
    /// When set, each successful package gets a completion marker under this
    /// job id (see `maint::resume`) so `-r` can skip finished work.
    pub resume_job: Option<ResumeJob<'a>>,
}

/// Where to record per-package completion for a live resume job.
#[derive(Clone, Copy)]
pub(crate) struct ResumeJob<'a> {
    pub root: &'a camino::Utf8Path,
    pub job_id: &'a str,
}

/// Plan-wide state shared by the sequential and parallel merge loops.
struct MergeRun<'a> {
    plan: &'a [query::depgraph::PlannedMerge],
    roots: &'a portage_resolve::Roots,
    host_roots: &'a portage_resolve::Roots,
    work_base: &'a camino::Utf8Path,
    distdir: Option<&'a camino::Utf8Path>,
    quiet: bool,
    merge_flags: &'a cli::MergeFlags,
    binpkg_index: Option<&'a portage_binpkg::BinpkgIndex>,
    host_binpkg_index: Option<&'a portage_binpkg::BinpkgIndex>,
    remote_indices: &'a [portage_binpkg::RemoteBinpkgIndex],
    enforce_no_source: bool,
    target_env: &'a binpkg::DesiredBuildEnv,
    target_dirs: &'a [std::path::PathBuf],
    host_env: &'a binpkg::DesiredBuildEnv,
    host_dirs: &'a [std::path::PathBuf],
    resume_job: Option<ResumeJob<'a>>,
}

/// Per-run mode bits derived once from [`MergeRun`] / [`cli::MergeFlags`].
struct ActionFlags {
    keep_going: bool,
    emptytree: bool,
    buildpkg: bool,
    buildpkgonly: bool,
    fetchonly: bool,
    fetch_all_uri: bool,
    enforce_no_source: bool,
    quiet: bool,
    self_contained_bootstrap: bool,
}

impl MergeRun<'_> {
    fn action_flags(&self) -> ActionFlags {
        ActionFlags {
            keep_going: self.merge_flags.keep_going,
            emptytree: self.merge_flags.emptytree,
            buildpkg: self.merge_flags.buildpkg,
            buildpkgonly: self.merge_flags.buildpkgonly,
            fetchonly: self.merge_flags.fetchonly,
            fetch_all_uri: self.merge_flags.fetch_all_uri,
            enforce_no_source: self.enforce_no_source,
            quiet: self.quiet,
            // See `ebuild::RootContext::self_contained_bootstrap` — native
            // toolchain bootstrap sets this on `roots`.
            self_contained_bootstrap: self.roots.installed_view_target_only(),
        }
    }
}

/// Everything needed to act on one plan entry (fetch / binpkg / source).
struct PackageAction<'a> {
    planned: &'a query::depgraph::PlannedMerge,
    merge_root: &'a camino::Utf8Path,
    host_roots: &'a portage_resolve::Roots,
    entry_roots: &'a portage_resolve::Roots,
    work_base: &'a camino::Utf8Path,
    distdir: Option<&'a camino::Utf8Path>,
    flags: &'a ActionFlags,
    merge_gate: Option<&'a tokio::sync::Mutex<()>>,
    binpkg_index: Option<&'a portage_binpkg::BinpkgIndex>,
    remote_indices: &'a [portage_binpkg::RemoteBinpkgIndex],
    desired_chost: &'a str,
    desired_build_env_key: &'a str,
}

/// Verify `pkgdir` can actually be written to (creating it if missing) — the
/// `--buildpkg` preflight in [`run_merge_plan`]. A probe file is written and
/// removed rather than just checking metadata, since permission bits alone
/// don't capture every reason a write can fail (e.g. a read-only mount).
fn check_pkgdir_writable(pkgdir: &camino::Utf8Path) -> Result<()> {
    std::fs::create_dir_all(pkgdir.as_std_path()).with_context(|| format!("creating {pkgdir}"))?;
    let probe = pkgdir.join(".em-write-probe");
    std::fs::write(probe.as_std_path(), b"").with_context(|| format!("writing to {pkgdir}"))?;
    let _ = std::fs::remove_file(probe.as_std_path());
    Ok(())
}

/// Prompt before acting on `count` packages (`--ask`) — `verb` is what the
/// run would do ("merge", "unmerge", "build"). Defaults to no on empty input
/// or EOF.
pub(crate) fn confirm_action(verb: &str, count: usize) -> Result<bool> {
    use std::io::Write;
    print!("\n>>> Would you like to {verb} these {count} package(s)? [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(false);
    }
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// Which [`portage_resolve::Roots`] a plan entry actually installs into: the outer EROOT
/// (`host_roots`) for a Host-rooted entry — an unsatisfied BDEPEND scheduled
/// onto the build host by a `--target` solve (see `cross_target_runtime_deps`
/// in portage-atom-pubgrub) — or the `--target`-substituted sysroot (`roots`,
/// the resolved install target) for everything else. `host_roots` equals
/// `roots` outside `--target`, so this is a no-op there.
///
/// Found live: the merge loop used a single, plan-wide root for every entry
/// regardless of `PlannedMerge.merge_root`, so a Host BDEPEND (e.g.
/// `dev-python/jinja2`, rebuilt for a python target the real host lacked)
/// silently built into the sysroot instead — the package "succeeded" but
/// never became available where the later build that needed it actually
/// looked. See `todo/stage-build-shakeout.md`.
fn entry_roots<'a>(
    planned: &query::depgraph::PlannedMerge,
    roots: &'a portage_resolve::Roots,
    host_roots: &'a portage_resolve::Roots,
) -> &'a portage_resolve::Roots {
    if planned.merge_root == query::depgraph::MergeRoot::Host {
        host_roots
    } else {
        roots
    }
}

/// Which local binpkg index a plan entry's reuse lookup consults: the host's
/// own (built with host CHOST/CFLAGS) for a `MergeRoot::Host` entry, the
/// target's otherwise. Mirrors [`entry_roots`]'s selection — see
/// `run_merge_plan`'s `dual_pkgdir` for why these can genuinely differ under
/// `--target` (S1/S4 in `todo/binpkg-subtargets.md`).
fn entry_binpkg_index<'a>(
    planned: &query::depgraph::PlannedMerge,
    target: Option<&'a portage_binpkg::BinpkgIndex>,
    host: Option<&'a portage_binpkg::BinpkgIndex>,
) -> Option<&'a portage_binpkg::BinpkgIndex> {
    if planned.merge_root == query::depgraph::MergeRoot::Host {
        host
    } else {
        target
    }
}

/// Which precomputed [`binpkg::DesiredBuildEnv`] (and its `portage_dirs`) a
/// plan entry's desired build_env_key/CHOST reads from — mirrors
/// [`entry_roots`]'s selection.
fn entry_desired_env<'a>(
    planned: &query::depgraph::PlannedMerge,
    target: (&'a binpkg::DesiredBuildEnv, &'a [std::path::PathBuf]),
    host: (&'a binpkg::DesiredBuildEnv, &'a [std::path::PathBuf]),
) -> (&'a binpkg::DesiredBuildEnv, &'a [std::path::PathBuf]) {
    if planned.merge_root == query::depgraph::MergeRoot::Host {
        host
    } else {
        target
    }
}

/// Build and merge a resolved plan in install order.
///
/// **Resume progress:** when [`MergePlanRequest::resume_job`] is `Some`,
/// each successful package creates a marker file under that job id (see
/// `maint::resume`) so a later `-r` can drop completed work from the
/// re-resolved plan. That is required under `--emptytree` (VDB presence
/// alone is not a completion marker — the tree starts installed). Markers
/// are independent files, so `--jobs N` never contends on a shared JSON
/// rewrite. Non-emptytree installs/upgrades also still VDB-skip
/// already-present non-reinstall entries in the merge loop.
///
/// With `-f`/`--fetchonly`, only distfiles (or remote binpkgs under `-g`) are
/// downloaded — no build, no install, no env-update.
pub(crate) async fn run_merge_plan(req: MergePlanRequest<'_>) -> Result<()> {
    let MergePlanRequest {
        plan,
        blockers,
        roots,
        work_base,
        distdir,
        merge_flags,
        globals,
        resume_job,
    } = req;

    let quiet = globals.quiet;
    let jobs = merge_flags.jobs.map(|j| j as usize).unwrap_or(1).max(1);
    let buildpkg = merge_flags.buildpkg;
    let buildpkgonly = merge_flags.buildpkgonly;
    // `-F`/`--fetch-all-uri` is fetch-only too (just a different SRC_URI
    // resolution mode inside the fetch phase — see `merge_flags.fetch_all_uri`'s
    // own doc comment); every top-level "did anything actually get merged"
    // gate below treats the two identically.
    let fetchonly = merge_flags.fetchonly || merge_flags.fetch_all_uri;
    let usepkg = merge_flags.usepkg;
    let getbinpkg = merge_flags.getbinpkg;
    let getbinpkgonly = merge_flags.getbinpkgonly;

    let merge_root = roots.merge_root();
    let total = plan.len();

    // A `--target` plan can carry `MergeRoot::Host` entries (an unsatisfied
    // BDEPEND scheduled onto the build host — see `cross_target_runtime_deps`
    // in portage-atom-pubgrub). `roots` here is the `--target`-substituted
    // sysroot; `broot()` is where a Host entry actually belongs — the real
    // host `/` for plain `--root` (portage `ROOT=` parity: BDEPEND resolves
    // and installs on the host, full stop), matching `base_roots()` for
    // `--prefix`/`--local`. NOT `base_roots()` directly: that's "the outer
    // EROOT" (where crossdev's own `cross-*` toolchain *bootstrap* packages
    // land via the separate `bypass_cross_root` mechanism in `emerge.rs`) —
    // a different, unprivileged-writable-location concern from "where does
    // an ordinary package's BDEPEND resolve". Equal to `roots` when `--target`
    // isn't active, so this is a no-op outside cross builds.
    let host_roots = globals.broot();

    // Per-entry PKGDIR (S1/S4 in todo/binpkg-subtargets.md): a Host entry's
    // binpkgs live in the *host*'s PKGDIR (built with host CHOST/CFLAGS), a
    // Target entry's in the target's own — distinct whenever the two roots'
    // own PKGDIR resolution disagrees (config-root make.conf, or simply a
    // different merge_root falling to the root-relative default). Outside
    // `--target` (and for `--root`/`--prefix`/`--local` with no distinct host
    // config), `host_roots` resolves to the same PKGDIR as `roots`, so this
    // is a no-op there — deliberately compared by resolved path, not gated on
    // "is --target active", so a plain `--root` whose config-root make.conf
    // sets a different PKGDIR is still handled correctly.
    let target_pkgdir = binpkg::resolve_pkgdir_for_roots(roots).await;
    let host_pkgdir = binpkg::resolve_pkgdir_for_roots(&host_roots).await;
    let plan_has_host_entries = plan
        .iter()
        .any(|p| p.merge_root == query::depgraph::MergeRoot::Host);
    let dual_pkgdir = plan_has_host_entries && host_pkgdir != target_pkgdir;

    // Fail fast: verify PKGDIR is actually writable *before* starting a
    // potentially multi-hour build, rather than discovering it deep into a
    // `--keep-going` run once dozens of packages have already silently died.
    // Found live (todo/stage-build-shakeout.md): a stage3 --buildpkg attempt
    // hit a permission-denied PKGDIR (fixed separately — resolve_pkgdir is now
    // root-aware), and each failure surfaced as an unexplained, silent worker
    // death rather than the single clear error this check now gives instead.
    // Fetch-only never writes PKGDIR (remote binpkg cache is under work_base).
    // When `dual_pkgdir`, a Host entry's producer path (`write_binpkg`,
    // already per-entry) writes into `host_pkgdir` too — check it up front
    // for the same reason.
    if !fetchonly && (buildpkg || buildpkgonly) {
        let flag = if buildpkg {
            "--buildpkg"
        } else {
            "--buildpkgonly"
        };
        check_pkgdir_writable(&target_pkgdir)
            .with_context(|| format!("{flag}: PKGDIR {target_pkgdir} is not writable"))?;
        if dual_pkgdir {
            check_pkgdir_writable(&host_pkgdir)
                .with_context(|| format!("{flag}: host PKGDIR {host_pkgdir} is not writable"))?;
        }
    }

    let usepkgonly = merge_flags.usepkgonly;

    // Implication chain (portage actions.py): -g ⇒ --usepkg, -G ⇒ --getbinpkg +
    // binpkg-only (no source). -K is its own local-only binpkg-only flag. So
    // all of -k/-K/-g/-G enable local reuse; local overrides remote; -K/-G
    // both refuse to fall back to a source build.
    let want_local = usepkg || usepkgonly || getbinpkg || getbinpkgonly;
    let want_remote = getbinpkg || getbinpkgonly;
    let enforce_no_source = usepkgonly || getbinpkgonly;

    // Open the local binpkg index once if any binpkg reuse is in effect.
    let binpkg_index = if want_local {
        match portage_binpkg::BinpkgIndex::open(target_pkgdir.as_std_path()) {
            Ok(idx) => {
                if !idx.is_empty() {
                    println!(
                        ">>> --usepkg: {} local binary package(s) in {target_pkgdir}",
                        idx.len()
                    );
                }
                Some(idx)
            }
            Err(e) => {
                eprintln!("warning: --usepkg index unavailable ({target_pkgdir}): {e:#}");
                None
            }
        }
    } else {
        None
    };

    // A second, host-rooted index for Host plan entries when their PKGDIR
    // genuinely differs from the target's. No fallback to the target index
    // when this fails to open or `want_local` is false: falling back would
    // reintroduce exactly the cross-PKGDIR confusion this separates out (the
    // CHOST/build_env_key gates would usually save us, but Phase 1a was
    // about not relying on "usually") — a Host entry with no host index
    // simply misses and builds/uses the normal source or remote path.
    let host_binpkg_index_owned = if want_local && dual_pkgdir {
        match portage_binpkg::BinpkgIndex::open(host_pkgdir.as_std_path()) {
            Ok(idx) => {
                if !idx.is_empty() {
                    println!(
                        ">>> --usepkg: {} host binary package(s) in {host_pkgdir}",
                        idx.len()
                    );
                }
                Some(idx)
            }
            Err(e) => {
                eprintln!("warning: --usepkg host index unavailable ({host_pkgdir}): {e:#}");
                None
            }
        }
    } else {
        None
    };
    // A Host entry consults its own PKGDIR's index when distinct, else the
    // shared one already opened above (dual_pkgdir false: same PKGDIR, no
    // point opening it twice).
    let host_binpkg_index = if dual_pkgdir {
        host_binpkg_index_owned.as_ref()
    } else {
        binpkg_index.as_ref()
    };

    // Fetch each configured remote binhost's Packages index. `-g`/`-G` only.
    let remote_indices: Vec<portage_binpkg::RemoteBinpkgIndex> = if want_remote {
        let binhosts = binpkg::portage_binhosts(globals).await;
        if binhosts.is_empty() {
            eprintln!(
                "warning: --getbinpkg set but no binhost configured (PORTAGE_BINHOST unset, no binrepos.conf)"
            );
        }
        let mut fetched = Vec::new();
        for repo in &binhosts {
            let base = &repo.sync_uri;
            match portage_distfiles::fetch_index_cached(
                &repo.sync_uri,
                repo.frozen,
                roots.merge_root(),
            )
            .await
            {
                Ok((text, reason)) => {
                    let idx = portage_binpkg::RemoteBinpkgIndex::new(&text, base);
                    println!(
                        ">>> --getbinpkg: {} package(s) on {base} ({reason})",
                        idx.len()
                    );
                    fetched.push(idx);
                }
                Err(e) => {
                    eprintln!("warning: could not fetch binhost index {base}: {e:#}");
                }
            }
        }
        fetched
    } else {
        Vec::new()
    };

    // Per-plan, not per-entry: each Roots value's make.conf/package.env
    // portage_dirs only depend on `roots`/`host_roots`, both fixed for the
    // whole plan. Computing once here (instead of the old per-entry 5x
    // read_make_conf_var_for_roots calls) also fixes the perf cost of
    // re-parsing the same make.conf file once per plan entry.
    let target_env = binpkg::DesiredBuildEnv::for_roots(roots).await;
    let target_dirs = binpkg::DesiredBuildEnv::portage_dirs(roots);
    let host_env = binpkg::DesiredBuildEnv::for_roots(&host_roots).await;
    let host_dirs = binpkg::DesiredBuildEnv::portage_dirs(&host_roots);

    let run = MergeRun {
        plan,
        roots,
        host_roots: &host_roots,
        work_base,
        distdir,
        quiet,
        merge_flags,
        binpkg_index: binpkg_index.as_ref(),
        host_binpkg_index,
        remote_indices: &remote_indices,
        enforce_no_source,
        target_env: &target_env,
        target_dirs: &target_dirs,
        host_env: &host_env,
        host_dirs: &host_dirs,
        resume_job,
    };

    let (merged, skipped, failures) = if jobs <= 1 {
        merge_sequential(&run).await
    } else {
        merge_parallel(&run, blockers, jobs).await
    };

    // Refresh ${ROOT}/etc/profile.env and the linker cache, as emerge does
    // after merging — only worthwhile if something was actually installed.
    // `-B` / `-f` leave the live root untouched by contract.
    if merged > 0
        && !buildpkgonly
        && !fetchonly
        && let Err(e) = maint::env::env_update(merge_root)
    {
        eprintln!("warning: env-update failed: {e:#}");
    }

    if failures.is_empty() {
        let extra = if skipped > 0 {
            format!(" ({skipped} already installed)")
        } else {
            String::new()
        };
        let done = if fetchonly {
            format!("{merged} package(s) fetched")
        } else if buildpkgonly {
            format!("{merged} binary package(s) built")
        } else {
            format!("{merged} package(s) merged into {merge_root}")
        };
        println!("\n>>> Done — {done}{extra}");
        return Ok(());
    }

    let fail_verb = if fetchonly { "fetch" } else { "merge" };
    eprintln!("\n>>> {} package(s) failed to {fail_verb}:", failures.len());
    for f in &failures {
        eprintln!("  * {}", f.cpv);
        eprintln!("      {}", f.cause);
        if f.log.exists() {
            eprintln!("      log: {}", f.log);
        }
    }
    if merged > 0 || skipped > 0 {
        eprintln!(
            "    ({merged} ok, {skipped} already installed, {} failed of {total})",
            failures.len()
        );
    }
    bail!(
        "{} of {total} package(s) failed to {fail_verb}",
        failures.len()
    );
}

/// Act on one plan entry: fetch-only, binpkg merge, or source build.
async fn act_on_package(a: PackageAction<'_>) -> anyhow::Result<()> {
    let PackageAction {
        planned,
        merge_root,
        host_roots,
        entry_roots,
        work_base,
        distdir,
        flags,
        merge_gate,
        binpkg_index,
        remote_indices,
        desired_chost,
        desired_build_env_key,
    } = a;
    let ActionFlags {
        buildpkg,
        buildpkgonly,
        fetchonly,
        fetch_all_uri,
        enforce_no_source,
        quiet,
        self_contained_bootstrap,
        ..
    } = *flags;

    let desired_use: Vec<String> = planned
        .use_flags
        .iter()
        .map(|f| f.as_str().to_string())
        .collect();

    let reused = binpkg_index.and_then(|idx| {
        idx.find_reusable(
            &planned.cpv.to_string(),
            &desired_use,
            desired_chost,
            desired_build_env_key,
        )
    });
    let remote_url = reused
        .is_none()
        .then(|| {
            remote_indices.iter().find_map(|idx| {
                idx.find_reusable(
                    &planned.cpv.to_string(),
                    &desired_use,
                    desired_chost,
                    desired_build_env_key,
                )
            })
        })
        .flatten();

    let root_ctx = ebuild::RootContext {
        config_root: entry_roots.config(),
        sysroot: entry_roots.build_sysroot(),
        eprefix: entry_roots.eprefix(),
        broot: Some(host_roots.merge_root()),
        self_contained_bootstrap,
    };

    if fetchonly || fetch_all_uri {
        // Local binpkg already present → nothing to download.
        if let Some(binpkg_path) = reused {
            println!(
                ">>> binpkg already present (no fetch needed): {}",
                binpkg_path.display()
            );
            return Ok(());
        }
        // Remote binpkg: download into the run cache, do not merge.
        if let Some(url) = remote_url {
            let path = fetch_remote_binpkg(&url, work_base).await?;
            println!(">>> Fetched binary package: {url} -> {path}");
            return Ok(());
        }
        if enforce_no_source {
            bail!("no matching binpkg and source builds disabled (-K/--getbinpkgonly)");
        }
        // Source: distfile fetch only.
        return ebuild::build_and_merge(ebuild::BuildAndMerge {
            ebuild_path: &planned.ebuild_path,
            cpv: &planned.cpv,
            use_flags: &planned.use_flags,
            work_base,
            root: merge_root,
            distdir,
            quiet,
            roots: root_ctx,
            merge_gate,
            buildpkg: false,
            buildpkgonly: false,
            fetchonly,
            fetch_all_uri,
        })
        .await;
    }

    if let Some(binpkg_path) = reused {
        println!(">>> Using binary package: {}", binpkg_path.display());
        let path = camino::Utf8Path::from_path(binpkg_path.as_path())
            .unwrap_or_else(|| camino::Utf8Path::new("/invalid-binpkg-path"));
        return ebuild::merge_binpkg(ebuild::MergeBinpkg {
            binpkg_path: path,
            ebuild_path: &planned.ebuild_path,
            cpv: &planned.cpv,
            use_flags: &planned.use_flags,
            work_base,
            root: merge_root,
            quiet,
            roots: root_ctx,
            merge_gate,
        })
        .await;
    }

    if let Some(url) = remote_url {
        match fetch_remote_binpkg(&url, work_base).await {
            Ok(path) => {
                println!(">>> Fetched binary package: {url}");
                ebuild::merge_binpkg(ebuild::MergeBinpkg {
                    binpkg_path: &path,
                    ebuild_path: &planned.ebuild_path,
                    cpv: &planned.cpv,
                    use_flags: &planned.use_flags,
                    work_base,
                    root: merge_root,
                    quiet,
                    roots: root_ctx,
                    merge_gate,
                })
                .await
            }
            Err(e) if enforce_no_source => Err(e),
            Err(e) => {
                eprintln!(">>> Failed to fetch binpkg {url} — {e:#}; building from source");
                ebuild::build_and_merge(ebuild::BuildAndMerge {
                    ebuild_path: &planned.ebuild_path,
                    cpv: &planned.cpv,
                    use_flags: &planned.use_flags,
                    work_base,
                    root: merge_root,
                    distdir,
                    quiet,
                    roots: root_ctx,
                    merge_gate,
                    buildpkg,
                    buildpkgonly,
                    fetchonly: false,
                    fetch_all_uri: false,
                })
                .await
            }
        }
    } else if enforce_no_source {
        bail!("no matching binpkg and source builds disabled (-K/--getbinpkgonly)");
    } else {
        ebuild::build_and_merge(ebuild::BuildAndMerge {
            ebuild_path: &planned.ebuild_path,
            cpv: &planned.cpv,
            use_flags: &planned.use_flags,
            work_base,
            root: merge_root,
            distdir,
            quiet,
            roots: root_ctx,
            merge_gate,
            buildpkg,
            buildpkgonly,
            fetchonly: false,
            fetch_all_uri: false,
        })
        .await
    }
}

/// Sequential build+merge in install order (the `--jobs 1` / default path).
/// Returns `(merged, skipped, failures)`.
async fn merge_sequential(run: &MergeRun<'_>) -> (usize, usize, Vec<MergeFailure>) {
    let flags = run.action_flags();
    let total = run.plan.len();
    let mut merged = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<MergeFailure> = Vec::new();

    for (i, planned) in run.plan.iter().enumerate() {
        let entry_roots = entry_roots(planned, run.roots, run.host_roots);
        let merge_root = entry_roots.merge_root();
        let entry_index = entry_binpkg_index(planned, run.binpkg_index, run.host_binpkg_index);

        // Per-entry desired build_env_key (S6: package.env-aware) and CHOST
        // — proper binpkg reuse across cross-compilation and multi-arch
        // scenarios, host vs target selected the same way entry_roots is.
        let (desired_env, desired_dirs) = entry_desired_env(
            planned,
            (run.target_env, run.target_dirs),
            (run.host_env, run.host_dirs),
        );
        let desired_build_env_key = desired_env.key_for(desired_dirs, &planned.cpv).await;

        // VDB-presence skip for non-emptytree non-reinstall entries (an
        // interrupted ordinary install/upgrade). `--emptytree` must not use
        // this path — the tree starts installed — resume completion is
        // tracked via `maint::resume` and filtered out of the plan before
        // we get here. Intentional reinstalls (explicit target / USE rebuild)
        // always build.
        // Under `-f`, still skip fully-installed packages that are not being
        // reinstalled — their distfiles are not needed for this plan step.
        let pkg_vdb = merge_root.join("var/db/pkg").join(planned.cpv.to_string());
        if !flags.emptytree && !planned.reinstall && pkg_vdb.exists() {
            println!(
                ">>> [{}/{total}] {} is already installed — skipping",
                i + 1,
                planned.cpv
            );
            skipped += 1;
            continue;
        }

        let action = if flags.fetchonly || flags.fetch_all_uri {
            "Fetching"
        } else {
            "Emerging"
        };
        println!("\n>>> {action} ({} of {total}) {}", i + 1, planned.cpv);
        let result = act_on_package(PackageAction {
            planned,
            merge_root,
            host_roots: run.host_roots,
            entry_roots,
            work_base: run.work_base,
            distdir: run.distdir,
            flags: &flags,
            merge_gate: None,
            binpkg_index: entry_index,
            remote_indices: run.remote_indices,
            desired_chost: &desired_env.chost,
            desired_build_env_key: &desired_build_env_key,
        })
        .await;
        match result {
            Ok(()) => {
                merged += 1;
                if let Some(job) = run.resume_job
                    && let Err(e) = crate::maint::resume::mark_completed(
                        job.root,
                        job.job_id,
                        planned.merge_root,
                        &planned.cpv.to_string(),
                    )
                {
                    eprintln!("warning: could not update resume progress: {e:#}");
                }
                // Refresh this root's `ld.so.cache` immediately, not just once
                // at the very end of the whole batch (the caller's own
                // `env_update` after `merge_sequential` returns) — a later
                // package in this same batch may need to dynamically load a
                // library this one just installed (found live: `pkgconf`
                // merging mid-`stages --stage1` left a stale cache, so the
                // very next package's `configure` couldn't load the freshly
                // installed `libpkgconf.so`, even though both the package and
                // the cache entry were correct by the time the whole run
                // finished — the cache just wasn't refreshed yet at the
                // moment it was needed). `-B`/`-f` installed nothing to refresh.
                if !flags.buildpkgonly
                    && !flags.fetchonly
                    && !flags.fetch_all_uri
                    && let Err(e) = maint::env::env_update(merge_root)
                {
                    eprintln!("warning: env-update failed: {e:#}");
                }
            }
            Err(e) => {
                let fail_verb = if flags.fetchonly || flags.fetch_all_uri {
                    "fetch"
                } else {
                    "emerge"
                };
                eprintln!(">>> Failed to {fail_verb} {} — {e:#}", planned.cpv);
                failures.push(MergeFailure {
                    cpv: planned.cpv.to_string(),
                    log: run
                        .work_base
                        .join(planned.cpv.to_string())
                        .join("build.log"),
                    cause: format!("{e:#}"),
                });
                if !flags.keep_going {
                    eprintln!(">>> Stopping (pass --keep-going to continue past failures).");
                    break;
                }
            }
        }
    }
    (merged, skipped, failures)
}

/// Download a remote binpkg from `url` into a per-run cache under `work_base`,
/// returning the local path. Cached per filename so a retry doesn't re-download.
async fn fetch_remote_binpkg(
    url: &str,
    work_base: &camino::Utf8Path,
) -> Result<camino::Utf8PathBuf> {
    let cache_dir = work_base.join("binpkg-cache");
    tokio::fs::create_dir_all(cache_dir.as_std_path())
        .await
        .with_context(|| format!("creating {cache_dir}"))?;
    // Filename: the last path segment of the URL (e.g. foo-1.0-1.gpkg.tar).
    let name = url.rsplit('/').next().unwrap_or("binpkg.gpkg.tar");
    let dest = cache_dir.join(name);
    if !dest.exists() {
        portage_distfiles::fetch_binpkg(url, dest.as_std_path())
            .await
            .with_context(|| format!("downloading {url}"))?;
    }
    Ok(dest)
}

/// Tracks which plan entries are ready to build given the build-dep `blockers`
/// (each entry's in-plan predecessors). A node is ready once all its blockers
/// have `complete`d; this is the topological bookkeeping behind `--jobs`,
/// independent of how many run at once or in what real-time order they finish.
struct Scheduler {
    /// Remaining un-completed blockers per node.
    outstanding: Vec<usize>,
    /// Reverse adjacency: `dependents[j]` are nodes blocked on `j`.
    dependents: Vec<Vec<usize>>,
    /// Nodes with no outstanding blockers, awaiting a build slot.
    ready: VecDeque<usize>,
}

impl Scheduler {
    fn new(blockers: &[Vec<usize>]) -> Self {
        let outstanding: Vec<usize> = blockers.iter().map(Vec::len).collect();
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); blockers.len()];
        for (i, bs) in blockers.iter().enumerate() {
            for &j in bs {
                dependents[j].push(i);
            }
        }
        let ready = (0..blockers.len())
            .filter(|&i| outstanding[i] == 0)
            .collect();
        Scheduler {
            outstanding,
            dependents,
            ready,
        }
    }

    /// Pop the next node whose blockers are all satisfied, if any is waiting.
    fn next_ready(&mut self) -> Option<usize> {
        self.ready.pop_front()
    }

    /// Mark node `i` finished (built or skipped), unblocking its dependents.
    fn complete(&mut self, i: usize) {
        for d in std::mem::take(&mut self.dependents[i]) {
            self.outstanding[d] -= 1;
            if self.outstanding[d] == 0 {
                self.ready.push_back(d);
            }
        }
    }
}

/// Parallel build+merge for `--jobs N > 1`. Up to `jobs` packages *build*
/// concurrently; each only starts once its in-plan build dependencies
/// (`blockers`) have completed, so build order is respected. The compile phases
/// run in parallel (the heavy work is in child processes we await), while the
/// merge critical section is serialised by a shared async lock — so the live
/// root, VDB counter, and world/profile files are only mutated by one package
/// at a time. Returns `(merged, skipped, failures)`.
///
/// Concurrency is single-threaded (`FuturesUnordered`, not spawned tasks): the
/// `EbuildShell` need not be `Send`, and parallelism still comes from the
/// concurrently-running build subprocesses.
async fn merge_parallel(
    run: &MergeRun<'_>,
    blockers: &[Vec<usize>],
    jobs: usize,
) -> (usize, usize, Vec<MergeFailure>) {
    let flags = run.action_flags();
    let total = run.plan.len();
    let merge_gate = tokio::sync::Mutex::new(());

    let mut sched = Scheduler::new(blockers);
    let mut merged = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<MergeFailure> = Vec::new();
    let mut started = 0usize;
    let mut stop_new = false;
    let mut inflight = FuturesUnordered::new();

    loop {
        while !stop_new && inflight.len() < jobs {
            let Some(i) = sched.next_ready() else { break };
            let planned = &run.plan[i];
            let entry_roots = entry_roots(planned, run.roots, run.host_roots);
            let merge_root = entry_roots.merge_root();
            let entry_index = entry_binpkg_index(planned, run.binpkg_index, run.host_binpkg_index);

            // Per-entry desired build_env_key (S6: package.env-aware) and
            // CHOST — see the matching comment in `merge_sequential`.
            let (desired_env, desired_dirs) = entry_desired_env(
                planned,
                (run.target_env, run.target_dirs),
                (run.host_env, run.host_dirs),
            );
            let desired_build_env_key = desired_env.key_for(desired_dirs, &planned.cpv).await;

            if !flags.emptytree
                && !planned.reinstall
                && merge_root
                    .join("var/db/pkg")
                    .join(planned.cpv.to_string())
                    .exists()
            {
                println!(">>> {} is already installed — skipping", planned.cpv);
                skipped += 1;
                sched.complete(i);
                continue;
            }
            started += 1;
            let action = if flags.fetchonly || flags.fetch_all_uri {
                "Fetching"
            } else {
                "Emerging"
            };
            println!(
                ">>> {action} ({started} of {total}) {} [+{} in flight]",
                planned.cpv,
                inflight.len()
            );
            let gate = &merge_gate;
            let entry_roots_clone = entry_roots.clone();
            let desired_chost_entry_clone = desired_env.chost.clone();
            let desired_build_env_key_clone = desired_build_env_key.clone();
            let flags_ref = &flags;
            inflight.push(async move {
                let res = act_on_package(PackageAction {
                    planned,
                    merge_root,
                    host_roots: run.host_roots,
                    entry_roots: &entry_roots_clone,
                    work_base: run.work_base,
                    distdir: run.distdir,
                    flags: flags_ref,
                    merge_gate: Some(gate),
                    binpkg_index: entry_index,
                    remote_indices: run.remote_indices,
                    desired_chost: &desired_chost_entry_clone,
                    desired_build_env_key: &desired_build_env_key_clone,
                })
                .await;
                (i, res)
            });
        }

        let Some((i, res)) = inflight.next().await else {
            break;
        };
        match res {
            Ok(()) => {
                merged += 1;
                sched.complete(i);
                if let Some(job) = run.resume_job
                    && let Err(e) = crate::maint::resume::mark_completed(
                        job.root,
                        job.job_id,
                        run.plan[i].merge_root,
                        &run.plan[i].cpv.to_string(),
                    )
                {
                    eprintln!("warning: could not update resume progress: {e:#}");
                }
                // See the matching comment in `merge_sequential`: refresh this
                // entry's root `ld.so.cache` right away, not just once for the
                // whole batch — a still-running or not-yet-started sibling may
                // need to dynamically load what this merge just installed.
                let merge_root = entry_roots(&run.plan[i], run.roots, run.host_roots).merge_root();
                if !flags.buildpkgonly
                    && !flags.fetchonly
                    && !flags.fetch_all_uri
                    && let Err(e) = maint::env::env_update(merge_root)
                {
                    eprintln!("warning: env-update failed: {e:#}");
                }
            }
            Err(e) => {
                let fail_verb = if flags.fetchonly || flags.fetch_all_uri {
                    "fetch"
                } else {
                    "emerge"
                };
                eprintln!(">>> Failed to {fail_verb} {} — {e:#}", run.plan[i].cpv);
                failures.push(MergeFailure {
                    cpv: run.plan[i].cpv.to_string(),
                    log: run
                        .work_base
                        .join(run.plan[i].cpv.to_string())
                        .join("build.log"),
                    cause: format!("{e:#}"),
                });
                // Dependents stay blocked (their count never reaches 0), so a
                // package whose build dep failed is never started.
                if !flags.keep_going {
                    stop_new = true;
                    eprintln!(
                        ">>> Stopping new builds (pass --keep-going to continue past failures)."
                    );
                }
            }
        }
    }
    (merged, skipped, failures)
}

#[cfg(test)]
mod entry_roots_tests {
    use super::*;
    use query::depgraph::{MergeRoot, PlannedMerge};

    fn planned(merge_root: MergeRoot) -> Result<PlannedMerge> {
        Ok(PlannedMerge {
            merge_root,
            cpv: portage_atom::Cpv::parse("dev-python/jinja2-3.1.6")?,
            ebuild_path: camino::Utf8PathBuf::new(),
            use_flags: Vec::new(),
            depend: Vec::new(),
            bdepend: Vec::new(),
            reinstall: false,
        })
    }

    #[test]
    fn host_entry_installs_into_outer_eroot_not_the_cross_sysroot() -> Result<()> {
        let roots =
            portage_resolve::Roots::for_test("/var/tmp/cross-stage1/usr/riscv64-unknown-linux-gnu");
        let host_roots = portage_resolve::Roots::for_test("/var/tmp/cross-stage1");
        let p = planned(MergeRoot::Host)?;
        assert_eq!(
            entry_roots(&p, &roots, &host_roots).merge_root().as_str(),
            "/var/tmp/cross-stage1"
        );
        Ok(())
    }

    #[test]
    fn target_entry_uses_the_plans_own_root() -> Result<()> {
        let roots =
            portage_resolve::Roots::for_test("/var/tmp/cross-stage1/usr/riscv64-unknown-linux-gnu");
        let host_roots = portage_resolve::Roots::for_test("/var/tmp/cross-stage1");
        let p = planned(MergeRoot::Target)?;
        assert_eq!(
            entry_roots(&p, &roots, &host_roots).merge_root().as_str(),
            "/var/tmp/cross-stage1/usr/riscv64-unknown-linux-gnu"
        );
        Ok(())
    }

    /// `--prefix`: an unsatisfied `MergeRoot::Host` entry must merge into
    /// the prefix, not the real host — an unprivileged overlay can't write
    /// `/`. `host_roots` here is `Cli::broot()`'s output for `--prefix`,
    /// which now resolves to the prefix (`outer_roots()`), not the host.
    #[test]
    fn host_entry_installs_into_the_prefix_under_overlay_not_the_host() -> Result<()> {
        let roots = portage_resolve::Roots::for_test("/opt/p");
        let host_roots = portage_resolve::Roots::for_test_overlay("/", "/opt/p");
        let p = planned(MergeRoot::Host)?;
        assert_eq!(
            entry_roots(&p, &roots, &host_roots).merge_root().as_str(),
            "/opt/p",
            "an unsatisfied Host-routed entry must merge into the prefix, not the real host"
        );
        Ok(())
    }

    /// Seed `dir/Packages` with a single-entry index so `BinpkgIndex::open`
    /// (the only public constructor) can load it in tests.
    fn seed_index(dir: &std::path::Path) {
        std::fs::write(
            dir.join("Packages"),
            "CPV: dev-python/jinja2-3.1.6\nIUSE:\nPATH: dev-python/jinja2-3.1.6-1.gpkg.tar\nUSE:\n",
        )
        .unwrap();
    }

    /// A Host entry's reuse lookup must hit the *host* PKGDIR's index, a
    /// Target entry's the target's — even when both indices contain the same
    /// cpv (S1/S4: distinct boards/roots, not distinct package sets).
    #[test]
    fn host_entry_uses_host_index_target_entry_uses_target_index() -> Result<()> {
        let host_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        seed_index(host_dir.path());
        seed_index(target_dir.path());
        let host_idx = portage_binpkg::BinpkgIndex::open(host_dir.path()).unwrap();
        let target_idx = portage_binpkg::BinpkgIndex::open(target_dir.path()).unwrap();

        let host_entry = planned(MergeRoot::Host)?;
        let target_entry = planned(MergeRoot::Target)?;

        let picked = entry_binpkg_index(&host_entry, Some(&target_idx), Some(&host_idx)).unwrap();
        assert_eq!(
            picked
                .find_reusable("dev-python/jinja2-3.1.6", &[], "", "")
                .unwrap(),
            host_dir.path().join("dev-python/jinja2-3.1.6-1.gpkg.tar")
        );

        let picked = entry_binpkg_index(&target_entry, Some(&target_idx), Some(&host_idx)).unwrap();
        assert_eq!(
            picked
                .find_reusable("dev-python/jinja2-3.1.6", &[], "", "")
                .unwrap(),
            target_dir.path().join("dev-python/jinja2-3.1.6-1.gpkg.tar")
        );
        Ok(())
    }

    /// When the host index is unavailable (`dual_pkgdir` but it failed to
    /// open, or `want_local` off), a Host entry must get `None` — never
    /// silently fall back to the target index. Falling back would
    /// reintroduce exactly the cross-PKGDIR confusion Phase 1b removes.
    #[test]
    fn host_entry_gets_none_not_target_fallback_when_host_index_missing() -> Result<()> {
        let target_dir = tempfile::tempdir().unwrap();
        seed_index(target_dir.path());
        let target_idx = portage_binpkg::BinpkgIndex::open(target_dir.path()).unwrap();
        let host_entry = planned(MergeRoot::Host)?;
        assert!(entry_binpkg_index(&host_entry, Some(&target_idx), None).is_none());
        Ok(())
    }

    /// `entry_desired_env` picks the same side `entry_roots`/
    /// `entry_binpkg_index` would for the same entry.
    #[test]
    fn entry_desired_env_picks_host_or_target() -> Result<()> {
        let target_env = binpkg::DesiredBuildEnv::for_test("riscv64-unknown-linux-gnu");
        let host_env = binpkg::DesiredBuildEnv::for_test("aarch64-unknown-linux-gnu");
        let dirs: Vec<std::path::PathBuf> = Vec::new();

        let host_entry = planned(MergeRoot::Host)?;
        let (picked, _) = entry_desired_env(&host_entry, (&target_env, &dirs), (&host_env, &dirs));
        assert_eq!(picked.chost, "aarch64-unknown-linux-gnu");

        let target_entry = planned(MergeRoot::Target)?;
        let (picked, _) =
            entry_desired_env(&target_entry, (&target_env, &dirs), (&host_env, &dirs));
        assert_eq!(picked.chost, "riscv64-unknown-linux-gnu");
        Ok(())
    }
}
