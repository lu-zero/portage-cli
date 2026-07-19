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

/// Build and merge a resolved plan in install order.
///
/// Resume comes for free from the target VDB: a package already recorded
/// there at the planned version is skipped (a previous run merged it), so
/// re-running after an interruption continues from the first unmerged entry
/// without a separate state file. `--emptytree` forces every entry to rebuild.
///
/// With `-f`/`--fetchonly`, only distfiles (or remote binpkgs under `-g`) are
/// downloaded — no build, no install, no env-update.
pub(crate) async fn run_merge_plan(
    plan: &[query::depgraph::PlannedMerge],
    blockers: &[Vec<usize>],
    roots: &portage_resolve::Roots,
    work_base: &camino::Utf8Path,
    distdir: Option<&camino::Utf8Path>,
    merge_flags: &cli::MergeFlags,
    globals: &cli::Cli,
) -> Result<()> {
    let quiet = globals.quiet;
    let jobs = merge_flags.jobs.map(|j| j as usize).unwrap_or(1).max(1);
    let buildpkg = merge_flags.buildpkg;
    let buildpkgonly = merge_flags.buildpkgonly;
    let fetchonly = merge_flags.fetchonly;
    let usepkg = merge_flags.usepkg;
    let getbinpkg = merge_flags.getbinpkg;
    let getbinpkgonly = merge_flags.getbinpkgonly;

    let merge_root = roots.merge_root();
    let total = plan.len();

    // Fail fast: verify PKGDIR is actually writable *before* starting a
    // potentially multi-hour build, rather than discovering it deep into a
    // `--keep-going` run once dozens of packages have already silently died.
    // Found live (todo/stage-build-shakeout.md): a stage3 --buildpkg attempt
    // hit a permission-denied PKGDIR (fixed separately — resolve_pkgdir is now
    // root-aware), and each failure surfaced as an unexplained, silent worker
    // death rather than the single clear error this check now gives instead.
    // Fetch-only never writes PKGDIR (remote binpkg cache is under work_base).
    if !fetchonly && (buildpkg || buildpkgonly) {
        let flag = if buildpkg {
            "--buildpkg"
        } else {
            "--buildpkgonly"
        };
        let pkgdir = binpkg::resolve_pkgdir(globals);
        check_pkgdir_writable(&pkgdir)
            .with_context(|| format!("{flag}: PKGDIR {pkgdir} is not writable"))?;
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
        let pkgdir = binpkg::resolve_pkgdir(globals);
        match portage_binpkg::BinpkgIndex::open(pkgdir.as_std_path()) {
            Ok(idx) => {
                if !idx.is_empty() {
                    println!(
                        ">>> --usepkg: {} local binary package(s) in {pkgdir}",
                        idx.len()
                    );
                }
                Some(idx)
            }
            Err(e) => {
                eprintln!("warning: --usepkg index unavailable ({pkgdir}): {e:#}");
                None
            }
        }
    } else {
        None
    };

    // Fetch each configured remote binhost's Packages index. `-g`/`-G` only.
    let remote_indices: Vec<portage_binpkg::RemoteBinpkgIndex> = if want_remote {
        let binhosts = binpkg::portage_binhosts(globals);
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
    let (merged, skipped, failures) = if jobs <= 1 {
        merge_sequential(
            plan,
            roots,
            &host_roots,
            work_base,
            distdir,
            quiet,
            merge_flags,
            binpkg_index.as_ref(),
            &remote_indices,
            enforce_no_source,
        )
        .await
    } else {
        merge_parallel(
            plan,
            blockers,
            roots,
            &host_roots,
            work_base,
            distdir,
            quiet,
            jobs,
            merge_flags,
            binpkg_index.as_ref(),
            &remote_indices,
            enforce_no_source,
        )
        .await
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
#[allow(clippy::too_many_arguments)]
async fn act_on_package(
    planned: &query::depgraph::PlannedMerge,
    merge_root: &camino::Utf8Path,
    host_roots: &portage_resolve::Roots,
    entry_roots: &portage_resolve::Roots,
    work_base: &camino::Utf8Path,
    distdir: Option<&camino::Utf8Path>,
    quiet: bool,
    merge_gate: Option<&tokio::sync::Mutex<()>>,
    self_contained_bootstrap: bool,
    buildpkg: bool,
    buildpkgonly: bool,
    fetchonly: bool,
    binpkg_index: Option<&portage_binpkg::BinpkgIndex>,
    remote_indices: &[portage_binpkg::RemoteBinpkgIndex],
    enforce_no_source: bool,
    // Desired CHOST for binpkg reuse (empty skips the CHOST gate).
    desired_chost: &str,
    // Desired build_env_key for binpkg reuse (empty skips the build_env_key gate).
    desired_build_env_key: &str,
) -> anyhow::Result<()> {
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

    if fetchonly {
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
        return ebuild::build_and_merge(
            &planned.ebuild_path,
            &planned.cpv,
            &planned.use_flags,
            work_base,
            merge_root,
            distdir,
            quiet,
            root_ctx,
            merge_gate,
            false,
            false,
            true,
        )
        .await;
    }

    if let Some(binpkg_path) = reused {
        println!(">>> Using binary package: {}", binpkg_path.display());
        let path = camino::Utf8Path::from_path(binpkg_path.as_path())
            .unwrap_or_else(|| camino::Utf8Path::new("/invalid-binpkg-path"));
        return ebuild::merge_binpkg(
            path,
            &planned.ebuild_path,
            &planned.cpv,
            &planned.use_flags,
            work_base,
            merge_root,
            quiet,
            root_ctx,
            merge_gate,
        )
        .await;
    }

    if let Some(url) = remote_url {
        match fetch_remote_binpkg(&url, work_base).await {
            Ok(path) => {
                println!(">>> Fetched binary package: {url}");
                ebuild::merge_binpkg(
                    &path,
                    &planned.ebuild_path,
                    &planned.cpv,
                    &planned.use_flags,
                    work_base,
                    merge_root,
                    quiet,
                    root_ctx,
                    merge_gate,
                )
                .await
            }
            Err(e) if enforce_no_source => Err(e),
            Err(e) => {
                eprintln!(">>> Failed to fetch binpkg {url} — {e:#}; building from source");
                ebuild::build_and_merge(
                    &planned.ebuild_path,
                    &planned.cpv,
                    &planned.use_flags,
                    work_base,
                    merge_root,
                    distdir,
                    quiet,
                    root_ctx,
                    merge_gate,
                    buildpkg,
                    buildpkgonly,
                    false,
                )
                .await
            }
        }
    } else if enforce_no_source {
        bail!("no matching binpkg and source builds disabled (-K/--getbinpkgonly)");
    } else {
        ebuild::build_and_merge(
            &planned.ebuild_path,
            &planned.cpv,
            &planned.use_flags,
            work_base,
            merge_root,
            distdir,
            quiet,
            root_ctx,
            merge_gate,
            buildpkg,
            buildpkgonly,
            false,
        )
        .await
    }
}

/// Sequential build+merge in install order (the `--jobs 1` / default path).
/// Returns `(merged, skipped, failures)`.
#[allow(clippy::too_many_arguments)]
async fn merge_sequential(
    plan: &[query::depgraph::PlannedMerge],
    roots: &portage_resolve::Roots,
    host_roots: &portage_resolve::Roots,
    work_base: &camino::Utf8Path,
    distdir: Option<&camino::Utf8Path>,
    quiet: bool,
    merge_flags: &cli::MergeFlags,
    binpkg_index: Option<&portage_binpkg::BinpkgIndex>,
    remote_indices: &[portage_binpkg::RemoteBinpkgIndex],
    enforce_no_source: bool,
) -> (usize, usize, Vec<MergeFailure>) {
    let keep_going = merge_flags.keep_going;
    let emptytree = merge_flags.emptytree;
    let buildpkg = merge_flags.buildpkg;
    let buildpkgonly = merge_flags.buildpkgonly;
    let fetchonly = merge_flags.fetchonly;

    let total = plan.len();
    let mut merged = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<MergeFailure> = Vec::new();
    // See `ebuild::RootContext::self_contained_bootstrap` — the native
    // toolchain bootstrap sets this on `roots` (`with_target_only_installed_view`)
    // and every plan entry built under this invocation must see it, not just
    // the ones whose `entry_roots` happens to preserve it.
    let self_contained_bootstrap = roots.installed_view_target_only();

    for (i, planned) in plan.iter().enumerate() {
        let entry_roots = entry_roots(planned, roots, host_roots);
        let merge_root = entry_roots.merge_root();

        // Compute per-entry desired build_env_key from CFLAGS, CXXFLAGS, LDFLAGS, RUSTFLAGS
        // This allows proper binpkg reuse across cross-compilation and multi-arch scenarios
        let desired_cflags =
            binpkg::read_make_conf_var_for_roots(entry_roots, "CFLAGS").unwrap_or_default();
        let desired_cxxflags =
            binpkg::read_make_conf_var_for_roots(entry_roots, "CXXFLAGS").unwrap_or_default();
        let desired_ldflags =
            binpkg::read_make_conf_var_for_roots(entry_roots, "LDFLAGS").unwrap_or_default();
        let desired_rustflags =
            binpkg::read_make_conf_var_for_roots(entry_roots, "RUSTFLAGS").unwrap_or_default();
        let desired_build_env_key = portage_binpkg::build_env_key(
            &desired_cflags,
            &desired_cxxflags,
            &desired_ldflags,
            &desired_rustflags,
        );

        // Also compute per-entry desired CHOST (may differ from global for cross builds)
        let desired_chost_entry = binpkg::read_make_conf_var_for_roots(entry_roots, "CHOST")
            .or_else(|| std::env::var("CHOST").ok().filter(|s| !s.is_empty()))
            .unwrap_or_default();

        // The VDB is the resume state: `var/db/pkg/<cat>/<pf>` exists iff this
        // exact version is already installed in the target root. An intentional
        // reinstall (explicit target / USE rebuild) is built anyway — emerge
        // reinstalls a requested atom by default.
        // Under `-f`, still skip fully-installed packages that are not being
        // reinstalled — their distfiles are not needed for this plan step.
        let pkg_vdb = merge_root.join("var/db/pkg").join(planned.cpv.to_string());
        if !emptytree && !planned.reinstall && pkg_vdb.exists() {
            println!(
                ">>> [{}/{total}] {} is already installed — skipping",
                i + 1,
                planned.cpv
            );
            skipped += 1;
            continue;
        }

        let action = if fetchonly { "Fetching" } else { "Emerging" };
        println!("\n>>> {action} ({} of {total}) {}", i + 1, planned.cpv);
        let result = act_on_package(
            planned,
            merge_root,
            host_roots,
            entry_roots,
            work_base,
            distdir,
            quiet,
            None,
            self_contained_bootstrap,
            buildpkg,
            buildpkgonly,
            fetchonly,
            binpkg_index,
            remote_indices,
            enforce_no_source,
            &desired_chost_entry,
            &desired_build_env_key,
        )
        .await;
        match result {
            Ok(()) => {
                merged += 1;
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
                if !buildpkgonly
                    && !fetchonly
                    && let Err(e) = maint::env::env_update(merge_root)
                {
                    eprintln!("warning: env-update failed: {e:#}");
                }
            }
            Err(e) => {
                let fail_verb = if fetchonly { "fetch" } else { "emerge" };
                eprintln!(">>> Failed to {fail_verb} {} — {e:#}", planned.cpv);
                failures.push(MergeFailure {
                    cpv: planned.cpv.to_string(),
                    log: work_base.join(planned.cpv.to_string()).join("build.log"),
                    cause: format!("{e:#}"),
                });
                if !keep_going {
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
#[allow(clippy::too_many_arguments)]
async fn merge_parallel(
    plan: &[query::depgraph::PlannedMerge],
    blockers: &[Vec<usize>],
    roots: &portage_resolve::Roots,
    host_roots: &portage_resolve::Roots,
    work_base: &camino::Utf8Path,
    distdir: Option<&camino::Utf8Path>,
    quiet: bool,
    jobs: usize,
    merge_flags: &cli::MergeFlags,
    binpkg_index: Option<&portage_binpkg::BinpkgIndex>,
    remote_indices: &[portage_binpkg::RemoteBinpkgIndex],
    enforce_no_source: bool,
) -> (usize, usize, Vec<MergeFailure>) {
    let keep_going = merge_flags.keep_going;
    let emptytree = merge_flags.emptytree;
    let buildpkg = merge_flags.buildpkg;
    let buildpkgonly = merge_flags.buildpkgonly;
    let fetchonly = merge_flags.fetchonly;

    let total = plan.len();
    let merge_gate = tokio::sync::Mutex::new(());
    // See `merge_sequential`'s matching comment.
    let self_contained_bootstrap = roots.installed_view_target_only();

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
            let planned = &plan[i];
            let entry_roots = entry_roots(planned, roots, host_roots);
            let merge_root = entry_roots.merge_root();

            // Compute per-entry desired build_env_key from CFLAGS, CXXFLAGS, LDFLAGS, RUSTFLAGS
            let desired_cflags =
                binpkg::read_make_conf_var_for_roots(entry_roots, "CFLAGS").unwrap_or_default();
            let desired_cxxflags =
                binpkg::read_make_conf_var_for_roots(entry_roots, "CXXFLAGS").unwrap_or_default();
            let desired_ldflags =
                binpkg::read_make_conf_var_for_roots(entry_roots, "LDFLAGS").unwrap_or_default();
            let desired_rustflags =
                binpkg::read_make_conf_var_for_roots(entry_roots, "RUSTFLAGS").unwrap_or_default();
            let desired_build_env_key = portage_binpkg::build_env_key(
                &desired_cflags,
                &desired_cxxflags,
                &desired_ldflags,
                &desired_rustflags,
            );

            // Also compute per-entry desired CHOST
            let desired_chost_entry = binpkg::read_make_conf_var_for_roots(entry_roots, "CHOST")
                .or_else(|| std::env::var("CHOST").ok().filter(|s| !s.is_empty()))
                .unwrap_or_default();

            if !emptytree
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
            let action = if fetchonly { "Fetching" } else { "Emerging" };
            println!(
                ">>> {action} ({started} of {total}) {} [+{} in flight]",
                planned.cpv,
                inflight.len()
            );
            let gate = &merge_gate;
            let entry_roots_clone = entry_roots.clone();
            let desired_chost_entry_clone = desired_chost_entry.clone();
            let desired_build_env_key_clone = desired_build_env_key.clone();
            inflight.push(async move {
                let res = act_on_package(
                    planned,
                    merge_root,
                    host_roots,
                    &entry_roots_clone,
                    work_base,
                    distdir,
                    quiet,
                    Some(gate),
                    self_contained_bootstrap,
                    buildpkg,
                    buildpkgonly,
                    fetchonly,
                    binpkg_index,
                    remote_indices,
                    enforce_no_source,
                    &desired_chost_entry_clone,
                    &desired_build_env_key_clone,
                )
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
                // See the matching comment in `merge_sequential`: refresh this
                // entry's root `ld.so.cache` right away, not just once for the
                // whole batch — a still-running or not-yet-started sibling may
                // need to dynamically load what this merge just installed.
                let merge_root = entry_roots(&plan[i], roots, host_roots).merge_root();
                if !buildpkgonly
                    && !fetchonly
                    && let Err(e) = maint::env::env_update(merge_root)
                {
                    eprintln!("warning: env-update failed: {e:#}");
                }
            }
            Err(e) => {
                let fail_verb = if fetchonly { "fetch" } else { "emerge" };
                eprintln!(">>> Failed to {fail_verb} {} — {e:#}", plan[i].cpv);
                failures.push(MergeFailure {
                    cpv: plan[i].cpv.to_string(),
                    log: work_base.join(plan[i].cpv.to_string()).join("build.log"),
                    cause: format!("{e:#}"),
                });
                // Dependents stay blocked (their count never reaches 0), so a
                // package whose build dep failed is never started.
                if !keep_going {
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
}
