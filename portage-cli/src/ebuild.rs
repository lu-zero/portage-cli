use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use bzip2::Compression;
use bzip2::write::BzEncoder;
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Cpv;
use portage_distfiles::{DistfileResolver, FetchConfig, FetchStatus, Fetcher};
use portage_metadata::SrcUriEntry;
use portage_repo::{
    DEFAULT_MAKE_CONF, Ebuild, EbuildEnv, LEGACY_MAKE_CONF, MakeConf, Manifest, ReposConf,
    Repository,
};
use portage_vdb::{ContentsEntry, ContentsKind, InstalledPackage, MergeSpec, Vdb};

use crate::postprocess;
use crate::preserve_libs;

/// Which phases a [`run_inner`] call owns — the single source of truth for the
/// build-tree epilogues (clean, env-dump/restore, buildpkg, tree-drop).
///
/// The fakeroost/sudo scoping (Q6: the ptrace tax / real root must stay off
/// the compile) splits a source build into [`PhaseGroup::Compile`] (parent,
/// un-wrapped) + [`PhaseGroup::Install`] (wrapped `__worker` child). The other
/// backends (real root, hakoniwa umbrella) use [`PhaseGroup::Full`] — one
/// process. [`PhaseGroup::Debug`] backs `em ebuild`.
#[derive(Clone, Debug)]
enum PhaseGroup {
    /// Full source build + merge: clean → `pretend..qmerge` → buildpkg → tree-drop.
    Full,
    /// Pre-install phases only (the un-wrapped parent): clean →
    /// `pretend..compile` → dump env to `worker-env`. No buildpkg, no tree-drop
    /// — the compile artifacts must survive for the Install worker.
    Compile,
    /// Install + qmerge (the wrapped worker): restore env from `worker-env` →
    /// `install,qmerge` → buildpkg → tree-drop. Does NOT wipe `work/` (the
    /// compile artifacts live there); only `image/temp/homedir`.
    Install,
    /// Merge a pre-built GPKG (`-k`/`-g`): clean → extract image → `qmerge` →
    /// tree-drop. No src_install — the extracted image is the payload.
    BinpkgMerge,
    /// `-B`/`--buildpkgonly`: clean → `pretend..install` → buildpkg →
    /// tree-drop. No `qmerge` at all, unlike every other merge-shaped
    /// group — the image is packaged but never installed into the live
    /// ROOT/VDB. Real emerge's own caveat applies: this doesn't resolve or
    /// install anything, so the ebuild's own DEPEND/BDEPEND closure must
    /// already be satisfied on the build host.
    BuildOnly,
    /// Debug (`em ebuild`): run the given phases only; no clean/drop/buildpkg.
    Debug(Vec<String>),
}

impl PhaseGroup {
    /// The phases this group runs, in order.
    fn phases(&self) -> Vec<String> {
        match self {
            Self::Full => [
                "pretend",
                "setup",
                "fetch",
                "unpack",
                "prepare",
                "configure",
                "compile",
                "test",
                "install",
                "qmerge",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            Self::Compile => [
                "pretend",
                "setup",
                "fetch",
                "unpack",
                "prepare",
                "configure",
                "compile",
                "test",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            Self::Install => ["install", "qmerge"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            Self::BinpkgMerge => vec!["qmerge".to_string()],
            Self::BuildOnly => [
                "pretend",
                "setup",
                "fetch",
                "unpack",
                "prepare",
                "configure",
                "compile",
                "test",
                "install",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            Self::Debug(p) => p.clone(),
        }
    }

    /// A real build/merge (not `em ebuild`): gates `src_test` skip and the
    /// merge critical-section lock.
    fn is_merge(&self) -> bool {
        !matches!(self, Self::Debug(_))
    }

    /// Subdirs to wipe before the phase loop (stale-tree clean).
    /// Full/Compile/BinpkgMerge: everything (starting fresh). Install:
    /// `image` only — `work/` *and* `temp` (`${T}`) hold state the Compile
    /// parent produced that `src_install` may still need.
    /// Debug: none.
    ///
    /// `temp` used to be wiped here too, on the reasoning that only `work/`
    /// (the compile artifacts) needs to survive the Compile→Install process
    /// boundary. But `${T}` is cross-phase scratch space by PMS convention,
    /// not throwaway-per-phase: an ebuild's `src_prepare`/`src_configure` can
    /// stage a file there for `src_install` to pick up later (e.g.
    /// `app-crypt/gnupg`'s systemd unit templates, copied into `${T}` in
    /// `src_prepare` and `doins`'d from there in `src_install_all`). Since
    /// `Install`'s own phase list is just `["install", "qmerge"]` (no
    /// `prepare` to redo the copy), wiping `temp` here silently destroyed
    /// that staged file between the two processes and `doins` failed on a
    /// file that existed moments earlier in the same build. Caught live
    /// chasing a stage1 gnupg failure — see `todo/stage-build-shakeout.md`.
    fn clean_subs(&self) -> Option<&'static [&'static str]> {
        match self {
            Self::Full | Self::Compile | Self::BinpkgMerge | Self::BuildOnly => {
                Some(&["work", "image", "temp", "homedir"])
            }
            Self::Install => Some(&["image", "homedir"]),
            Self::Debug(_) => None,
        }
    }

    /// Dump the live env to `worker-env` after the phase loop (Compile only —
    /// the Install worker sources it to recover BUILD_DIR etc. across the
    /// process boundary).
    fn should_dump_env(&self) -> bool {
        matches!(self, Self::Compile)
    }

    /// Source `worker-env` before the phase loop (Install only).
    fn should_restore_env(&self) -> bool {
        matches!(self, Self::Install)
    }

    /// Build a binpkg (Full + Install, when `-b` is set; BuildOnly
    /// unconditionally, since packaging the image is the entire point).
    fn should_buildpkg(&self) -> bool {
        matches!(self, Self::Full | Self::Install | Self::BuildOnly)
    }

    /// Drop the build tree afterward.
    fn should_tree_drop(&self) -> bool {
        matches!(
            self,
            Self::Full | Self::Install | Self::BinpkgMerge | Self::BuildOnly
        )
    }
}

/// The root-model views an ebuild execution needs, bundled so the
/// build/merge call chain (`run`/`build_and_merge`/`merge_binpkg`/
/// `run_inner`) takes one parameter for this instead of one per field.
#[derive(Clone, Copy, Default)]
pub struct RootContext<'a> {
    pub config_root: Option<&'a Utf8Path>,
    pub sysroot: Option<&'a Utf8Path>,
    pub eprefix: Option<&'a Utf8Path>,
    /// Where BDEPEND-class build tools (a cross toolchain, its pkg-config, …)
    /// live for this invocation — `Cli::broot()`'s merge root. See
    /// `EbuildShell::build_broot`'s doc comment for the full rationale.
    pub broot: Option<&'a Utf8Path>,
    /// The native toolchain bootstrap (`em toolchain --setup`) is
    /// unconditionally self-contained regardless of `--root`/`--prefix`
    /// topology (mirrors `Roots::installed_view_target_only`, the same
    /// signal for the same reason) — it must not source the `--prefix`
    /// overlay's config-overlay `bashrc`. That recipe's `CPPFLAGS="-I<prefix>/
    /// usr/include ..."` is right for an ordinary package layered over an
    /// already-populated prefix (finds already-installed headers ahead of
    /// the host's), but for THIS bootstrap it lands ahead of a package's own
    /// project-local `-I` flags and can shadow a version-matched local header
    /// with an incompatible one from the freshly-installed target libc —
    /// found live 2026-07-16 running gcc's own `libiberty/obstack.c` self-
    /// build under `--prefix` (the exact same class of bug already fixed for
    /// `--root` on 2026-07-03, see `setup.rs`'s `BASHRC_PREFIX`/`self_contained`
    /// doc comment).
    pub self_contained_bootstrap: bool,
}

/// The base directory for build work trees: `<prefix>/var/tmp/portage` under
/// a prefix; otherwise the system `/var/tmp/portage` when writable, falling
/// back to the user cache.
pub fn default_work_base(prefix: Option<&Utf8Path>) -> Utf8PathBuf {
    if let Some(p) = prefix {
        return p.join("var/tmp/portage");
    }
    let system = Utf8Path::new("/var/tmp/portage");
    let probe = system.join(format!(".em-write-probe-{}", std::process::id()));
    if std::fs::create_dir_all(system).is_ok() && std::fs::write(&probe, b"").is_ok() {
        let _ = std::fs::remove_file(&probe);
        return system.to_owned();
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    Utf8PathBuf::from(home).join(".cache/em/build")
}

/// Source the host profile stack (`/etc/portage/make.profile` +
/// `/etc/portage/profile`) and make.conf into the shell, and set its
/// effective USE. Returns `false` when no profile is resolvable (the build
/// proceeds with bare defaults).
pub(crate) async fn apply_profile_env(
    shell: &mut portage_repo::EbuildShell,
    config_root: Option<&Utf8Path>,
    config_overlay: Option<&Utf8Path>,
) -> Result<bool> {
    // PORTAGE_CONFIGROOT: profile/make.conf come from here (host unless --root
    // / --config-root offsets it). See docs/root-model.md.
    let base = config_root.unwrap_or_else(|| Utf8Path::new("/"));
    let Ok(profile_path) =
        std::fs::canonicalize(base.join("etc/portage/make.profile").as_std_path())
    else {
        return Ok(false);
    };
    let stack = portage_repo::ProfileStack::build(profile_path)
        .context("building profile stack")?
        .with_user_profile(base.join("etc/portage/profile").into_std_path_buf())
        .context("loading the user profile")?;
    let conf_candidates = [
        base.join("etc/portage/make.conf"),
        base.join("etc/make.conf"),
    ];
    let confs: Vec<&std::path::Path> = conf_candidates
        .iter()
        .map(|p| p.as_std_path())
        .filter(|p| p.exists())
        .collect();
    stack
        .configure_shell(shell, &confs)
        .await
        .context("sourcing profile environment")?;

    // Every profile/make.conf-derived variable (CHOST, ELIBC, MULTILIB_ABIS,
    // DEFAULT_ABI, …) must reach real subprocesses an ebuild/eclass spawns
    // directly, not just em's own Rust builtins — see export_sourced_env.
    shell
        .export_sourced_env()
        .context("exporting profile environment")?;

    // Portage `bashrc` hooks (not PMS): each profile's `profile.bashrc` in stack
    // order, then the user's `${config_root}/etc/portage/bashrc`. run_phase
    // sources these per phase with the full env — the user hook is where overlay
    // search paths can be wired without build-system knowledge in our code.
    let mut bashrc: Vec<Utf8PathBuf> = Vec::new();
    for profile in stack.profiles() {
        let p = profile.path().join("profile.bashrc");
        if p.is_file()
            && let Ok(p) = Utf8PathBuf::from_path_buf(p)
        {
            bashrc.push(p);
        }
    }
    let user = base.join("etc/portage/bashrc");
    if user.is_file() {
        bashrc.push(user);
    }
    // User config overlay bashrc (e.g. `--local`'s ~/.gentoo/etc/portage/bashrc),
    // sourced last so it wins — the natural home for the overlay search-path
    // recipe, without writing the host /etc/portage.
    if let Some(overlay) = config_overlay {
        let ob = overlay.join("bashrc");
        if ob.is_file() {
            bashrc.push(ob);
        }
    }
    shell.set_bashrc_files(bashrc);

    Ok(true)
}

pub async fn run(
    ebuild_path: &str,
    phases: &[String],
    work_dir: Option<&Utf8Path>,
    repo_override: Option<&str>,
    root: &Utf8Path,
    roots: RootContext<'_>,
) -> Result<()> {
    // Standalone `em ebuild <path> <phase>`: no resolved plan entry exists, so
    // there's no authoritative Cpv to pass — `run_inner` falls back to
    // deriving one from `ebuild_path` (fine here: this debug entry point only
    // ever targets a real on-disk ebuild, never a cross-derived virtual one).
    run_inner(
        ebuild_path,
        None,
        &PhaseGroup::Debug(phases.to_vec()),
        work_dir,
        repo_override,
        root,
        None,
        None,
        None,
        roots,
        None,
        false,
        None,
    )
    .await
}

/// Build one resolved plan entry through the full phase chain and merge it
/// into `root`: the per-package effective USE replaces the make.conf USE, the
/// work tree lives under `work_base/<category>/<pf>`, and `distdir` (when
/// set, e.g. `<prefix>/var/cache/distfiles`) overrides the writable distfiles
/// location.
#[allow(clippy::too_many_arguments)]
pub async fn build_and_merge(
    ebuild_path: &Utf8Path,
    cpv: &portage_atom::Cpv,
    use_flags: &[portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>],
    work_base: &Utf8Path,
    root: &Utf8Path,
    distdir: Option<&Utf8Path>,
    quiet: bool,
    roots: RootContext<'_>,
    merge_gate: Option<&tokio::sync::Mutex<()>>,
    buildpkg: bool,
    // `-B`/`--buildpkgonly`: package the image, never qmerge it. Checked
    // first and unconditionally single-process -- there's no install into
    // the live ROOT/VDB to delegate to a privilege-wrapped worker, so the
    // Q6 compile/install split this function otherwise does has nothing to
    // scope around (an unprivileged run is instead wrapped whole by
    // `maybe_supervise`, see `needs_whole_process_wrap`). `buildpkg` is
    // forced `true` for the `run_inner` call below regardless of the
    // caller's own `-b`: producing the binpkg is the entire point of `-B`,
    // not a separate opt-in on top of it.
    buildpkgonly: bool,
) -> Result<()> {
    let ebuild = Ebuild::with_cpv(cpv.clone(), ebuild_path);
    let pf = format!("{}-{}", ebuild.name(), ebuild.version());
    let work_dir = work_base.join(ebuild.category()).join(pf);
    let log = work_dir.join("build.log");

    if buildpkgonly {
        return run_inner(
            ebuild_path.as_str(),
            Some(cpv),
            &PhaseGroup::BuildOnly,
            Some(&work_dir),
            None,
            root,
            Some(use_flags),
            distdir,
            Some((log.clone(), quiet)),
            roots,
            merge_gate,
            true,
            None,
        )
        .await
        .with_context(|| format!("build log: {log}"));
    }

    if let Some(backend) = crate::privilege::install_wrap_backend() {
        // Scoped privilege (Q6): compile runs un-wrapped in this process;
        // install+qmerge(+binpkg) delegates to a wrapped __worker child so the
        // ptrace tax / real root stays off the compile's make/gcc tree.
        run_inner(
            ebuild_path.as_str(),
            Some(cpv),
            &PhaseGroup::Compile,
            Some(&work_dir),
            None,
            root,
            Some(use_flags),
            distdir,
            Some((log.clone(), quiet)),
            roots,
            None,
            false,
            None,
        )
        .await
        .with_context(|| format!("build log: {log}"))?;

        let use_str = use_flags
            .iter()
            .map(|f| f.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let cpv_str = cpv.to_string();
        let code = crate::privilege::spawn_install_worker(
            backend,
            &crate::privilege::WorkerArgs {
                ebuild_path: ebuild_path.as_str(),
                cpv: &cpv_str,
                use_flags: &use_str,
                work_base: work_base.as_str(),
                root: root.as_str(),
                distdir: distdir.map(|d| d.as_str()),
                config_root: roots.config_root.map(|c| c.as_str()),
                sysroot: roots.sysroot.map(|s| s.as_str()),
                eprefix: roots.eprefix.map(|e| e.as_str()),
                broot: roots.broot.map(|b| b.as_str()),
                self_contained_bootstrap: roots.self_contained_bootstrap,
                buildpkg,
                quiet,
                binpkg: None,
            },
        )
        .await
        .context("starting the install worker")?;
        if code != 0 {
            anyhow::bail!("install worker exited with status {code} (build log: {log})");
        }
        Ok(())
    } else {
        run_inner(
            ebuild_path.as_str(),
            Some(cpv),
            &PhaseGroup::Full,
            Some(&work_dir),
            None,
            root,
            Some(use_flags),
            distdir,
            Some((log.clone(), quiet)),
            roots,
            merge_gate,
            buildpkg,
            None,
        )
        .await
        .with_context(|| format!("build log: {log}"))
    }
}

/// Merge a pre-built binary package (`-k`/`--usepkg`): extract the GPKG's image
/// into the work tree, then run only the `qmerge` phase (which sources the
/// ebuild for env/hooks and merges from `work_root/image`). Skips fetch →
/// compile entirely. The caller has already validated the binpkg is reusable
/// (version + USE + slot match) via [`portage_binpkg::BinpkgIndex`].
#[allow(clippy::too_many_arguments)]
pub async fn merge_binpkg(
    binpkg_path: &Utf8Path,
    ebuild_path: &Utf8Path,
    cpv: &portage_atom::Cpv,
    use_flags: &[portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>],
    work_base: &Utf8Path,
    root: &Utf8Path,
    quiet: bool,
    roots: RootContext<'_>,
    merge_gate: Option<&tokio::sync::Mutex<()>>,
) -> Result<()> {
    let ebuild = Ebuild::with_cpv(cpv.clone(), ebuild_path);
    let pf = format!("{}-{}", ebuild.name(), ebuild.version());
    let work_dir = work_base.join(ebuild.category()).join(pf);
    let log = work_dir.join("build.log");

    if let Some(backend) = crate::privilege::install_wrap_backend() {
        // The qmerge chowns must run under (fake) root. Delegate to the
        // wrapped __worker (BinpkgMerge group, binpkg set).
        let use_str = use_flags
            .iter()
            .map(|f| f.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let cpv_str = cpv.to_string();
        let code = crate::privilege::spawn_install_worker(
            backend,
            &crate::privilege::WorkerArgs {
                ebuild_path: ebuild_path.as_str(),
                cpv: &cpv_str,
                use_flags: &use_str,
                work_base: work_base.as_str(),
                root: root.as_str(),
                distdir: None,
                config_root: roots.config_root.map(|c| c.as_str()),
                sysroot: roots.sysroot.map(|s| s.as_str()),
                eprefix: roots.eprefix.map(|e| e.as_str()),
                broot: roots.broot.map(|b| b.as_str()),
                self_contained_bootstrap: roots.self_contained_bootstrap,
                buildpkg: false,
                quiet,
                binpkg: Some(binpkg_path.as_str()),
            },
        )
        .await
        .context("starting the install worker")?;
        if code != 0 {
            anyhow::bail!("install worker exited with status {code} (merge log: {log})");
        }
        Ok(())
    } else {
        // The image is extracted inside run_inner (after its clean step) from
        // the binpkg, then the qmerge phase merges from work_root/image.
        run_inner(
            ebuild_path.as_str(),
            Some(cpv),
            &PhaseGroup::BinpkgMerge,
            Some(&work_dir),
            None,
            root,
            Some(use_flags),
            None,
            Some((log.clone(), quiet)),
            roots,
            merge_gate,
            false,
            Some(binpkg_path),
        )
        .await
        .with_context(|| format!("merge log: {log}"))
    }
}

/// The `em __worker` body: run the install group (install+qmerge+binpkg) for one
/// package inside the privilege session the parent spawned us into. The ebuild
/// is re-sourced (portage spawns each phase in its own process); the parent's
/// captured env is restored so cross-phase state (BUILD_DIR, …) survives.
#[allow(clippy::too_many_arguments)]
pub async fn run_install_worker(
    ebuild_path: &str,
    cpv_str: &str,
    use_flags_str: &str,
    work_base: &str,
    root: &str,
    distdir: Option<&str>,
    roots: RootContext<'_>,
    binpkg: Option<&str>,
    buildpkg: bool,
    quiet: bool,
) -> Result<()> {
    use portage_atom::interner::{DefaultInterner, Interned};
    let use_flags: Vec<Interned<DefaultInterner>> = use_flags_str
        .split_whitespace()
        .map(Interned::<DefaultInterner>::intern)
        .collect();

    // The parent already resolved this identity (`WorkerArgs::cpv`) — parsed
    // here, not re-derived from `ebuild_path`'s on-disk directory name, which
    // is wrong for a cross-derived package (see `Ebuild::with_cpv`).
    let cpv = portage_atom::Cpv::parse(cpv_str)
        .with_context(|| format!("invalid --cpv {cpv_str:?} passed to __worker"))?;
    let ebuild_obj = Ebuild::with_cpv(cpv.clone(), Utf8Path::new(ebuild_path));
    let pf = format!("{}-{}", ebuild_obj.name(), ebuild_obj.version());
    let work_dir = Utf8Path::new(work_base)
        .join(ebuild_obj.category())
        .join(pf);
    let log = work_dir.join("build.log");

    let group = if binpkg.is_some() {
        PhaseGroup::BinpkgMerge
    } else {
        PhaseGroup::Install
    };
    run_inner(
        ebuild_path,
        Some(&cpv),
        &group,
        Some(&work_dir),
        None,
        Utf8Path::new(root),
        Some(&use_flags),
        distdir.map(Utf8Path::new),
        Some((log.clone(), quiet)),
        roots,
        None,
        buildpkg,
        binpkg.map(Utf8Path::new),
    )
    .await
    .with_context(|| format!("merge log: {log}"))
}

/// Resolve a repo's master repositories (depth-first), so eclasses inherited
/// from a master are found. Master locations come from `repos.conf` by name,
/// falling back to a sibling of `repo_root`. Masters that can't be opened are
/// skipped with a warning rather than aborting the build.
fn resolve_masters(
    repo: &Repository,
    repo_root: &Utf8Path,
    conf: Option<&ReposConf>,
) -> Vec<Repository> {
    fn recurse(
        repo: &Repository,
        repo_root: &Utf8Path,
        conf: Option<&ReposConf>,
        out: &mut Vec<Repository>,
        seen: &mut HashSet<String>,
    ) {
        for name in &repo.layout().masters {
            if !seen.insert(name.clone()) {
                continue;
            }
            let location = conf
                .and_then(|c| c.find(name))
                .and_then(|e| e.location.as_path().map(std::path::PathBuf::from))
                .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
                .unwrap_or_else(|| repo_root.parent().unwrap_or(repo_root).join(name));
            match Repository::open(location.as_std_path()) {
                Ok(master) => {
                    recurse(&master, &location, conf, out, seen);
                    out.push(master);
                }
                Err(e) => {
                    eprintln!("warning: master repo '{name}' for {repo_root} unavailable: {e}");
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(repo.name().to_string());
    recurse(repo, repo_root, conf, &mut out, &mut seen);
    out
}

#[allow(clippy::too_many_arguments)]
async fn run_inner(
    ebuild_path: &str,
    // The resolved plan entry's authoritative identity, when there is one —
    // `None` only for the standalone `em ebuild` debug path, which has no
    // plan to draw it from. `Some` always wins over deriving one from
    // `ebuild_path`'s on-disk directory name, which is wrong for a
    // cross-derived package (see `Ebuild::with_cpv`).
    cpv: Option<&portage_atom::Cpv>,
    group: &PhaseGroup,
    work_dir: Option<&Utf8Path>,
    repo_override: Option<&str>,
    root: &Utf8Path,
    use_flags: Option<&[portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>]>,
    distdir: Option<&Utf8Path>,
    phase_log: Option<(Utf8PathBuf, bool)>,
    roots: RootContext<'_>,
    merge_gate: Option<&tokio::sync::Mutex<()>>,
    buildpkg: bool,
    // A pre-built GPKG to merge (`-k`/`-g`): its image is extracted into
    // work_dir/image *after* the clean step. Only meaningful with
    // [`PhaseGroup::Install`]. None for a normal source build.
    binpkg: Option<&Utf8Path>,
) -> Result<()> {
    let RootContext {
        config_root,
        sysroot,
        eprefix,
        broot,
        self_contained_bootstrap,
    } = roots;
    let path = Utf8Path::new(ebuild_path);
    let ebuild = match cpv {
        Some(cpv) => Ebuild::with_cpv(cpv.clone(), path),
        None => Ebuild::from_path(path).with_context(|| format!("loading {ebuild_path}"))?,
    };

    let repo_root = match repo_override {
        Some(r) => Utf8PathBuf::from(r),
        None => ebuild
            .repo_root()
            .ok_or_else(|| anyhow::anyhow!("cannot determine repo root from ebuild path"))?
            .to_owned(),
    };

    let repo = Repository::open(repo_root.as_std_path())
        .with_context(|| format!("opening repo at {repo_root}"))?;

    // Cross-* packages sidestep masters (they symlink into gentoo, so
    // `repo_root` already is gentoo), but plain overlays inherit a master's
    // eclasses and need its tree resolved — see `resolve_masters`.
    let repos_conf = {
        let cr = config_root.unwrap_or_else(|| Utf8Path::new("/"));
        let overlay = eprefix.map(|e| e.join("etc/portage"));
        let extra: Vec<&Utf8Path> = overlay.as_deref().into_iter().collect();
        ReposConf::load_rooted(cr, &extra).ok()
    };
    let masters = resolve_masters(&repo, &repo_root, repos_conf.as_ref());

    let work_root = match work_dir {
        Some(p) => p.to_owned(),
        None => {
            let pf = format!("{}-{}", ebuild.name(), ebuild.version());
            Utf8PathBuf::from(format!("/var/tmp/portage/{}/{pf}", ebuild.category()))
        }
    };

    let master_refs: Vec<&Repository> = masters.iter().collect();
    let mut shell = repo
        .shell_with_masters(&master_refs)
        .await
        .context("creating shell")?;
    if let Some(dir) = distdir {
        shell.set_distdir(dir.to_owned());
    }
    shell.set_phase_log(phase_log);

    // Profile build environment: source the make.defaults chain and make.conf
    // into the shell so phases see CHOST, CFLAGS/LDFLAGS, MULTILIB_ABIS/ABI/
    // LIBDIR_*, and the USE_EXPAND variables (PYTHON_TARGETS, …) that eclasses
    // read directly. This also resolves the profile's effective USE.
    // The config overlay (`package.use`/`bashrc` over host config) is the
    // prefix's `etc/portage` in an in-place `--local` build (`EPREFIX/etc/portage`).
    let config_overlay = (!self_contained_bootstrap)
        .then_some(eprefix)
        .flatten()
        .map(|e| e.join("etc/portage"));
    if !apply_profile_env(&mut shell, config_root, config_overlay.as_deref()).await? {
        let cr = config_root.unwrap_or_else(|| Utf8Path::new("/"));
        eprintln!(
            "warning: no usable profile at {cr}/etc/portage/make.profile — building without profile defaults"
        );
    }

    // Per-package build environment: `/etc/portage/package.env` maps this package
    // to env files under `/etc/portage/env/`, sourced on top of `make.conf` so
    // FEATURES, *FLAGS, MAKEOPTS, … take effect per package. Sourced before the
    // resolved USE is applied (below) so the plan's USE wins — USE set by an env
    // file is intentionally not reflected here (a resolver-side follow-up; see
    // todo/package-env.md).
    {
        let base = config_root.unwrap_or_else(|| Utf8Path::new("/"));
        let mut portage_dirs = vec![base.join("etc/portage").into_std_path_buf()];
        if let Some(overlay) = config_overlay.as_deref() {
            portage_dirs.push(overlay.as_std_path().to_path_buf());
        }
        let slot = repo
            .cache_entry(ebuild.cpv())
            .ok()
            .flatten()
            .map(|c| c.metadata.slot.slot.as_str().to_string());
        for env_file in portage_repo::env_files_for(&portage_dirs, ebuild.cpv(), slot.as_deref()) {
            shell
                .source_env_file(&env_file)
                .await
                .with_context(|| format!("sourcing package.env file {}", env_file.display()))?;
        }
        // Package-env overrides (FEATURES/*FLAGS/…) need the same subprocess
        // visibility as the profile/make.conf sweep above.
        shell
            .export_sourced_env()
            .context("exporting package.env environment")?;
    }

    // Root model (docs/root-model.md): PORTAGE_CONFIGROOT = config_root, and
    // SYSROOT/ESYSROOT = the build-against base (only when it differs from the
    // install target, i.e. a --prefix overlay; otherwise SYSROOT = ROOT).
    //
    // NB: in overlay mode (target ≠ base) a package merged into the target is
    // not yet visible to later builds in the run — that needs a merged sysroot,
    // which is shelved (see docs/root-model.md "Overlay support — shelved").
    shell.set_build_roots(config_root, sysroot, eprefix, broot);

    if let Some(flags) = use_flags {
        // The resolved plan's effective USE for this package overrides the
        // profile-resolved set (the sourced environment stays).
        let refs: Vec<&str> = flags.iter().map(|f| f.as_str()).collect();
        shell.set_use_flags(&refs).context("setting USE flags")?;
    } else if let Ok(Some(entry)) = repo.cache_entry(ebuild.cpv()) {
        // Standalone `em ebuild` (no resolved plan): apply the ebuild's own IUSE
        // `+` defaults on top of the profile USE, so phases see the flags the
        // merge path would compute (e.g. llvm-r1's `+llvm_slot_NN`). The full
        // resolver isn't run here, so package.use / REQUIRED_USE nuances aren't
        // reflected — this just closes the common IUSE-default gap that
        // otherwise makes standalone phase runs diverge from a real merge.
        let mut use_set: Vec<String> = shell
            .get_var("USE")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let have: std::collections::HashSet<String> = use_set.iter().cloned().collect();
        let mut added = false;
        for iuse in &entry.metadata.iuse {
            if iuse.is_enabled_default() && !have.contains(iuse.name()) {
                use_set.push(iuse.name().to_string());
                added = true;
            }
        }
        if added {
            let refs: Vec<&str> = use_set.iter().map(String::as_str).collect();
            shell
                .set_use_flags(&refs)
                .context("applying IUSE defaults for em ebuild")?;
        }
    }

    // PMS 11.1: REPLACING_VERSIONS — the installed versions this merge
    // replaces (same slot), visible to pkg_pretend/setup/preinst/postinst.
    // Computed up front from the target root's VDB and the ebuild's SLOT.
    // Also for a debug `em ebuild … qmerge`, which merges without a plan.
    if group.is_merge()
        || matches!(group, PhaseGroup::Debug(p) if p.iter().any(|p| p == "merge" || p == "qmerge"))
    {
        let slot = repo
            .cache_entry(ebuild.cpv())
            .ok()
            .flatten()
            .map(|c| c.metadata.slot.slot.as_str().to_string())
            .unwrap_or_else(|| "0".to_string());
        let replacing = open_or_create_vdb(&vdb_root_for(root))
            .ok()
            .and_then(|vdb| vdb.find_slot_occupant(&ebuild.cpv().cpn, &slot).ok())
            .flatten()
            .map(|old| old.cpv().version.to_string())
            .unwrap_or_default();
        shell.preset_var("REPLACING_VERSIONS", &replacing);
    }

    // FEATURES from the configured environment (profile + make.conf). Only a
    // small set is acted on; the rest are accepted silently.
    let features: std::collections::HashSet<String> = shell
        .get_var("FEATURES")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let merge_mode = group.is_merge();

    // Clean the build tree before starting a merge, mirroring portage's `clean`
    // phase that precedes `setup`. `run_phase` creates work/image/temp/homedir
    // with `create_dir_all` (additive), so without this a re-emerge after a
    // failed build would carry the previous attempt's stale ${WORKDIR} and,
    // worse, a stale ${D} image whose leftover files would then be merged.
    // Standalone `em ebuild` (merge_mode=false) is left untouched — re-running
    // a single phase against the existing tree is a debug use case, and
    // portage's `ebuild` command doesn't auto-clean either. `keepwork` opts out
    // (FEATURES=keepwork keeps the tree for inspection), matching the post-merge
    // cleanup below. `build.log` and the `.em-helpers` shim dir are left: the
    // log is truncated by the phase-log tee, and the shims are idempotent.
    if !features.contains("keepwork")
        && let Some(wd) = work_dir
        && let Some(subs) = group.clean_subs()
    {
        for sub in subs {
            let _ = std::fs::remove_dir_all(wd.join(sub));
        }
    }

    // `-k`/`--usepkg`: extract the binpkg image *after* the clean above (which
    // wipes work_dir/image to defeat stale-${D} leakage on re-emerge). The image
    // is the authoritative payload here — there is no src_compile to repopulate
    // it — so it must land between the clean and the qmerge walk.
    if let (Some(wd), Some(bp)) = (work_dir, binpkg) {
        let image_dir = wd.join("image");
        std::fs::create_dir_all(image_dir.as_std_path())
            .with_context(|| format!("creating {image_dir}"))?;
        portage_binpkg::extract_image(bp.as_std_path(), image_dir.as_std_path())
            .with_context(|| format!("extracting image from {bp}"))?;
    }

    // Install worker: restore the compile parent's captured env so cross-phase
    // shell state (BUILD_DIR, a custom S, configure-time vars) survives the
    // process boundary. Source the ebuild first (defines the phase functions
    // and eclass state), overlay the captured env, then mark the shell
    // phase-sourced so the phase loop treats install as a later phase of the
    // same package — re-sourcing would re-assert the default S over the
    // restored one.
    if group.should_restore_env()
        && let Some(wd) = work_dir
    {
        let env_path = wd.join("worker-env");
        if env_path.exists() {
            shell
                .source_ebuild(&ebuild)
                .await
                .context("sourcing ebuild for env restore")?;
            shell
                .source_env_file(env_path.as_std_path())
                .await
                .with_context(|| format!("restoring environment {env_path}"))?;
            shell.mark_phase_sourced(&ebuild);
        }
    }

    let phases = group.phases();
    for phase in &phases {
        // In the merge chain, src_test only runs under FEATURES=test
        // (an explicit `em ebuild … test` always runs it).
        if merge_mode && phase == "test" && !features.contains("test") {
            continue;
        }

        // Serialise the merge critical section under `--jobs`: builds (compile
        // phases) run concurrently, but the qmerge — collision check, VDB
        // counter, world/profile updates — must not interleave across packages.
        // The guard is held only for this phase; non-merge phases stay parallel.
        // The in-process gate only covers tasks in this process; parallel
        // `__worker` children serialise on the flock (design Q2 — released by
        // the kernel if a worker dies).
        let _merge_guard = match (merge_gate, phase.as_str()) {
            (Some(gate), "merge" | "qmerge") => Some(gate.lock().await),
            _ => None,
        };
        let _merge_flock = match (merge_mode, work_dir, phase.as_str()) {
            (true, Some(wd), "merge" | "qmerge") => lock_merge_flock(wd).await,
            _ => None,
        };
        run_one_phase(&mut shell, &ebuild, &repo, phase, &work_root, root).await?;
        drop(_merge_flock);
        drop(_merge_guard);

        // Portage runs ecompress/estrip at the tail of __dyn_install: the
        // shell still holds the docompress/dostrip lists src_install built
        // up, and everything downstream (preinst, CONTENTS, qmerge) sees
        // the final image.
        if phase == "install" {
            post_process_after_install(&shell, &work_root, &features)?;
        }
    }

    // Compile parent: dump the live variables for the Install worker to
    // source. Lives at work_dir top-level — the Install clean doesn't touch it.
    if group.should_dump_env()
        && let Ok(env_data) = capture_variables(&mut shell, &work_root).await
    {
        let _ = std::fs::write(work_root.join("worker-env").as_std_path(), &env_data);
    }

    // Build a binary package from the freshly-merged image + VDB entry, if asked.
    // Runs after qmerge (VDB + CONTENTS written) and before the build tree is
    // dropped, inside the same privilege session so ${D} ownership/xattrs are
    // read correctly. `-B`/`BuildOnly` never ran qmerge at all, so it computes
    // its own scratch metadata instead (`build_binpkg_standalone`) -- and,
    // unlike `-b`'s packaging (a bonus on top of an already-successful
    // install), a packaging failure here is the *whole* operation failing,
    // so it propagates instead of just printing a warning.
    if buildpkg && group.should_buildpkg() {
        let is_buildonly = matches!(group, PhaseGroup::BuildOnly);
        let result = if is_buildonly {
            build_binpkg_standalone(&mut shell, &ebuild, &work_root, root).await
        } else {
            build_binpkg(&shell, &ebuild, &work_root, root)
        };
        match result {
            Ok(path) => println!(">>> Created binary package: {path}"),
            Err(e) if is_buildonly => {
                return Err(e.context("--buildpkgonly: creating binary package"));
            }
            Err(e) => eprintln!("warning: --buildpkg failed for {}: {e:#}", ebuild.cpv()),
        }
    }

    // Successful merge chain: drop the build tree, keeping build.log
    // (FEATURES=keepwork keeps everything).
    if group.should_tree_drop()
        && !features.contains("keepwork")
        && let Some(wd) = work_dir
    {
        for sub in ["work", "image", "temp", "homedir"] {
            let _ = std::fs::remove_dir_all(wd.join(sub));
        }
        let _ = std::fs::remove_file(wd.join("worker-env").as_std_path());
    }

    Ok(())
}

/// Exclusive flock on `<work_base>/.merge.lock` (the grandparent of the
/// per-package work dir), held around the merge critical section so parallel
/// `__worker` processes — and concurrent em instances sharing the tree —
/// cannot interleave qmerge. Blocking acquire runs off the async executor;
/// released on drop (or by the kernel on process exit).
async fn lock_merge_flock(work_dir: &Utf8Path) -> Option<std::fs::File> {
    let base = work_dir.parent()?.parent()?;
    let path = base.join(".merge.lock").into_std_path_buf();
    tokio::task::spawn_blocking(move || {
        // append: never truncate — other processes may hold the lock fd.
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive).ok()?;
        Some(f)
    })
    .await
    .ok()
    .flatten()
}

/// Build the ecompress/estrip configuration from the post-`src_install`
/// shell state (docompress/dostrip accumulators, FEATURES, RESTRICT,
/// PORTAGE_COMPRESS) and run the image post-processing pass.
/// The image subtree that gets post-processed and merged: the shell's `ED`
/// (`image/${EPREFIX}`, set by `init_build_env`), falling back to
/// `work_root/image` when `ED` is unset or empty. With `EPREFIX=""` this is the
/// plain image dir, so host / `--prefix` builds are unchanged.
fn ed_image_dir(shell: &portage_repo::EbuildShell, work_root: &Utf8Path) -> Utf8PathBuf {
    shell
        .get_var("ED")
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|| work_root.join("image"))
}

/// Pack the freshly-merged image (`${D}`) + VDB entry into a GPKG under `PKGDIR`
/// (default `/var/cache/binpkgs` for a host build, `<root>/var/cache/binpkgs`
/// otherwise), returning the written path.
fn build_binpkg(
    shell: &portage_repo::EbuildShell,
    ebuild: &Ebuild,
    work_root: &Utf8Path,
    root: &Utf8Path,
) -> Result<Utf8PathBuf> {
    let cat = ebuild.category();
    let pf = format!("{}-{}", ebuild.name(), ebuild.version());
    let vdb_dir = root.join("var/db/pkg").join(cat).join(&pf);
    anyhow::ensure!(
        vdb_dir.exists(),
        "VDB entry {vdb_dir} not found (qmerge did not write it?)"
    );
    write_binpkg(shell, ebuild, work_root, root, &vdb_dir)
}

/// `-B`/`--buildpkgonly`: package the image without ever touching the live
/// ROOT/VDB. Matches real portage's own model (`EbuildBuild.py`): it never
/// calls `merge()` for `-B` either, packaging straight from `${D}` instead.
/// Computes CONTENTS/metadata the exact same way a normal merge would --
/// `walk_image` + `Vdb::register` -- just pointed at scratch locations
/// under `work_root/temp` (already covered by the existing tree-drop
/// cleanup) rather than the real root and the real VDB, which are
/// genuinely never written to at any point.
async fn build_binpkg_standalone(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    work_root: &Utf8Path,
    root: &Utf8Path,
) -> Result<Utf8PathBuf> {
    let env = shell.collect_env();
    let env_dump = capture_environment(shell, work_root).await;
    let image_dir = ed_image_dir(shell, work_root);
    let cp = ConfigProtect::from_shell(shell);

    // A throwaway destination -- CONTENTS records absolute installed paths
    // (`/usr/bin/foo`) independent of where the corresponding real bytes
    // land, so pointing walk_image here instead of at `root` produces an
    // identical contents list without copying a single file into the real
    // system.
    let scratch_dest = work_root.join("temp/buildpkgonly-dest");
    let WalkResult { contents, size, .. } = walk_image(&image_dir, &scratch_dest, &cp)?;

    let scratch_vdb_root = work_root.join("temp/buildpkgonly-vdb");
    let vdb = open_or_create_vdb(&scratch_vdb_root)?;
    let counter = vdb.next_counter()?;
    let build_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let elf = crate::elfscan::scan_image(&image_dir);
    let spec = merge_spec_from_env(
        env,
        ebuild.cpv().clone(),
        contents,
        elf,
        size,
        build_time,
        counter,
    );
    let installed = vdb.register(&spec)?;

    let pf = format!("{}-{}", ebuild.name(), ebuild.version());
    let ebuild_dest = installed.path().join(format!("{pf}.ebuild"));
    if let Err(e) = std::fs::copy(ebuild.path(), ebuild_dest.as_std_path()) {
        eprintln!("warning: could not copy ebuild into package metadata: {e}");
    }
    if let Ok(ref data) = env_dump
        && let Err(e) = write_environment_bz2(&installed, data)
    {
        eprintln!("warning: could not write environment.bz2: {e}");
    }

    write_binpkg(shell, ebuild, work_root, root, installed.path())
}

/// Shared GPKG-writing core: pack `image_dir` (`${D}`) + `metadata_dir` (a
/// VDB-shaped directory -- the real VDB entry for a normal `-b` merge via
/// [`build_binpkg`], or a scratch one for `-B` via
/// [`build_binpkg_standalone`]) into a GPKG under `PKGDIR`.
fn write_binpkg(
    shell: &portage_repo::EbuildShell,
    ebuild: &Ebuild,
    work_root: &Utf8Path,
    root: &Utf8Path,
    metadata_dir: &Utf8Path,
) -> Result<Utf8PathBuf> {
    let cat = ebuild.category();
    let pf = format!("{}-{}", ebuild.name(), ebuild.version());
    let image_dir = ed_image_dir(shell, work_root);
    // PKGDIR precedence: $PKGDIR env (portage honours it) → the shell's resolved
    // value (make.conf/make.globals) → the default. Must agree with the
    // consumer's `binpkg::resolve_pkgdir` — including its root-awareness:
    // `root.join("var/cache/binpkgs")` needs no separate host-vs-root branch,
    // since it already reduces to the real system's `/var/cache/binpkgs` when
    // `root` is `/`. See `resolve_pkgdir`'s doc comment for why a non-host
    // root must never fall back to that real, root-owned system path.
    let pkgdir = std::env::var("PKGDIR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            shell
                .get_var("PKGDIR")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|| root.join("var/cache/binpkgs"));
    let build_id = next_build_id(&pkgdir, cat, &pf);
    let out = pkgdir.join(cat).join(format!("{pf}-{build_id}.gpkg.tar"));
    portage_binpkg::write_gpkg(
        &portage_binpkg::GpkgInput {
            image_dir: image_dir.as_std_path(),
            metadata_dir: metadata_dir.as_std_path(),
            basename: &pf,
        },
        out.as_std_path(),
    )
    .with_context(|| format!("writing binary package {out}"))?;
    Ok(out)
}

/// The next free GPKG build-id for `<cat>/<pf>` in `pkgdir` (portage numbers
/// rebuilds `<pf>-1`, `<pf>-2`, …); 1 when none exist.
fn next_build_id(pkgdir: &Utf8Path, cat: &str, pf: &str) -> u32 {
    let dir = pkgdir.join(cat);
    let prefix = format!("{pf}-");
    let mut max = 0u32;
    if let Ok(rd) = std::fs::read_dir(dir.as_std_path()) {
        for e in rd.flatten() {
            if let Some(rest) = e.file_name().to_string_lossy().strip_prefix(&prefix)
                && let Some(id) = rest.strip_suffix(".gpkg.tar")
                && let Ok(n) = id.parse::<u32>()
            {
                max = max.max(n);
            }
        }
    }
    max + 1
}

fn post_process_after_install(
    shell: &portage_repo::EbuildShell,
    work_root: &Utf8Path,
    features: &std::collections::HashSet<String>,
) -> Result<()> {
    // `ED` is the prefix subtree of the image (`image/${EPREFIX}`); == the image
    // dir when EPREFIX is empty. Post-process exactly what will be merged.
    let image_dir = ed_image_dir(shell, work_root);
    if !image_dir.exists() {
        return Ok(());
    }

    // docompress/dostrip path lists the install phase accumulated (PMS
    // 12.3.9/12.3.10), pushed into shared state by the Rust builtins.
    let paths = shell.install_paths();
    let to_paths =
        |v: Vec<String>| -> Vec<Utf8PathBuf> { v.into_iter().map(Utf8PathBuf::from).collect() };

    // PMS 12.3.9 defaults, then whatever the ebuild added via docompress.
    let mut compress_include = vec![
        Utf8PathBuf::from("/usr/share/doc"),
        Utf8PathBuf::from("/usr/share/info"),
        Utf8PathBuf::from("/usr/share/man"),
    ];
    compress_include.extend(to_paths(paths.compress));
    let mut compress_exclude = to_paths(paths.compress_exclude);
    if let Some(pf) = shell.get_var("PF") {
        compress_exclude.push(Utf8PathBuf::from(format!("/usr/share/doc/{pf}/html")));
    }

    let compress_cmd = shell
        .get_var("PORTAGE_COMPRESS")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "bzip2".to_string());
    let compress_flags: Vec<String> = shell
        .get_var("PORTAGE_COMPRESS_FLAGS")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "-9".to_string())
        .split_whitespace()
        .map(str::to_string)
        .collect();

    // Conservative RESTRICT check: a conditional `use? ( strip )` counts as
    // restricted; the cost is only an unstripped binary.
    let restrict_strip = shell
        .get_var("RESTRICT")
        .unwrap_or_default()
        .split_whitespace()
        .any(|t| t == "strip");
    let strip = if features.contains("nostrip") {
        postprocess::StripMode::Disabled
    } else if restrict_strip {
        // dostrip <path> opts paths back in under RESTRICT=strip.
        postprocess::StripMode::Only(to_paths(paths.strip))
    } else {
        postprocess::StripMode::All
    };

    let cfg = postprocess::PostProcess {
        compress_include,
        compress_exclude,
        compress_cmd,
        compress_flags,
        strip,
        strip_exclude: to_paths(paths.strip_exclude),
        strip_cmd: shell
            .get_var("STRIP")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "strip".to_string()),
    };

    let stats = postprocess::post_process_image(&image_dir, &cfg)?;
    if stats.compressed + stats.relinked + stats.stripped > 0 {
        println!(
            ">>> post-install: {} file(s) compressed, {} symlink(s) retargeted, {} object(s) stripped",
            stats.compressed, stats.relinked, stats.stripped
        );
    }
    Ok(())
}

async fn run_one_phase(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    repo: &Repository,
    phase: &str,
    work_root: &Utf8Path,
    root: &Utf8Path,
) -> Result<()> {
    match phase {
        "fetch" => run_fetch(shell, ebuild, repo, work_root).await,
        "clean" => run_clean(work_root),
        "merge" | "qmerge" => run_merge(shell, ebuild, work_root, root).await,
        _ => shell
            .run_phase(ebuild, phase, work_root.as_std_path(), root.as_std_path())
            .await
            .with_context(|| format!("phase {phase} failed")),
    }
}

async fn run_fetch(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    repo: &Repository,
    work_root: &Utf8Path,
) -> Result<()> {
    // Read SRC_URI from the live shell. In a merge run the ebuild is already
    // sourced (the `pretend` phase ran first), so avoid re-sourcing here: doing
    // so over an already-sourced shell no-ops the eclasses (their include guards
    // are set) and would drop their global-scope effects (e.g. gnome.org's
    // custom `S`). Only source when running `fetch` standalone (nothing sourced
    // yet), where there are no later phases to disturb.
    if !shell.is_phase_sourced(ebuild) {
        shell
            .source_ebuild(ebuild)
            .await
            .context("sourcing ebuild")?;
    }
    shell.set_a_from_src_uri();

    let src_uri_str = shell.get_var("SRC_URI").unwrap_or_default();
    let distdir = Utf8PathBuf::from(
        shell
            .get_var("DISTDIR")
            .unwrap_or_else(|| "/var/cache/distfiles".into()),
    );

    if src_uri_str.trim().is_empty() {
        println!("fetch: nothing to fetch (SRC_URI is empty)");
        return Ok(());
    }

    let entries = SrcUriEntry::parse(&src_uri_str).context("parsing SRC_URI")?;

    let use_flags: HashSet<String> = shell
        .get_var("USE")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();

    let gentoo_mirrors = gentoo_mirrors_list();
    let resolver = DistfileResolver::from_repo(repo, gentoo_mirrors).context("loading mirrors")?;
    let distfiles = resolver.resolve(&entries, &use_flags);

    if distfiles.is_empty() {
        println!("fetch: nothing to fetch");
        return Ok(());
    }

    let manifest_path = ebuild
        .path()
        .parent()
        .map(|p| p.join("Manifest"))
        .filter(|p| p.exists());
    let manifest = match manifest_path {
        Some(ref p) => {
            let raw = std::fs::read_to_string(p).context("reading Manifest")?;
            Manifest::parse(&raw).context("parsing Manifest")?
        }
        None => Manifest { entries: vec![] },
    };

    let (fetch_cmd, resume_cmd) = read_fetch_commands();
    let config = FetchConfig::from_make_conf(fetch_cmd, resume_cmd);
    let ro_distdirs: Vec<Utf8PathBuf> = shell
        .get_var("PORTAGE_RO_DISTDIRS")
        .unwrap_or_default()
        .split_whitespace()
        .map(Utf8PathBuf::from)
        .collect();
    let fetcher = Fetcher::new(distdir.clone(), config).with_ro_distdirs(ro_distdirs);

    std::fs::create_dir_all(distdir.as_std_path())
        .with_context(|| format!("creating distdir {distdir}"))?;

    let results = fetcher.fetch_all(&distfiles, &manifest).await;

    let mut any_failed = false;
    let mut any_restricted = false;
    for (df, result) in results {
        match result {
            Ok(FetchStatus::AlreadyPresent) => println!("fetch: {} (already present)", df.filename),
            Ok(FetchStatus::Downloaded) => println!("fetch: {} ok", df.filename),
            Ok(FetchStatus::FetchRestricted) => {
                eprintln!(
                    "fetch: {} is fetch-restricted (RESTRICT=fetch)",
                    df.filename
                );
                any_restricted = true;
            }
            Err(e) => {
                eprintln!("fetch: {} failed: {e}", df.filename);
                any_failed = true;
            }
        }
    }

    if any_restricted || any_failed {
        shell
            .run_phase(ebuild, "nofetch", work_root.as_std_path(), Path::new("/"))
            .await
            .context("pkg_nofetch failed")?;
    }

    if any_failed {
        bail!("one or more distfiles could not be fetched");
    }
    Ok(())
}

async fn run_merge(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    work_root: &Utf8Path,
    root: &Utf8Path,
) -> Result<()> {
    let temp_dir = work_root.join("temp");
    std::fs::create_dir_all(temp_dir.as_std_path()).context("creating temp dir")?;

    shell
        .source_ebuild(ebuild)
        .await
        .context("sourcing ebuild")?;
    let env = shell.collect_env();

    let env_dump = capture_environment(shell, work_root).await;

    let vdb_root = vdb_root_for(root);
    let vdb = open_or_create_vdb(&vdb_root)?;

    let slot_main = env.slot_main().to_owned();
    // The slot occupant (if any) is the package being replaced — its files are
    // exempt from collision detection and it is unmerged after the new content
    // lands. This includes a same-cpv reinstall (emerge's default for a
    // requested atom): a self-replace whose old/new CONTENTS match, so the
    // unmerge removes nothing but the own-file collision exemption still applies.
    let old_pkg = vdb
        .find_slot_occupant(&ebuild.cpv().cpn, &slot_main)
        .context("slot conflict query failed")?;

    shell
        .run_phase(
            ebuild,
            "preinst",
            work_root.as_std_path(),
            root.as_std_path(),
        )
        .await
        .context("pkg_preinst failed")?;

    // Merge the prefix subtree of the image (`ED = image/${EPREFIX}`) into the
    // merge root (`EROOT`); identity when EPREFIX is empty.
    let image_dir = ed_image_dir(shell, work_root);
    let cp = ConfigProtect::from_shell(shell);
    let WalkResult {
        contents,
        size,
        protected,
    } = walk_image(&image_dir, root, &cp)?;

    let exclude_cpv = old_pkg.as_ref().map(|p| p.cpv().clone());
    let collisions = vdb
        .find_collisions(&contents, exclude_cpv.as_ref())
        .context("collision check failed")?;
    if !collisions.is_empty() {
        for c in &collisions {
            eprintln!("collision: {} is already owned by {}", c.path, c.owner);
        }
        bail!(
            "{} file collision(s) detected — aborting merge",
            collisions.len()
        );
    }

    if let Some(ref old) = old_pkg {
        let exclude: HashSet<Cpv> = std::iter::once(old.cpv().clone()).collect();
        let mut registry = preserve_libs::PreservedLibsRegistry::load(root);
        let graph = preserve_libs::build_link_graph(&vdb, &exclude, &registry, root);
        unmerge_slot_occupant(
            shell,
            old,
            work_root,
            root,
            &vdb,
            &contents,
            &ebuild.cpv().version,
            &graph,
            &mut registry,
        )
        .await?;
        registry.store();
        shell.preset_var("REPLACED_BY_VERSION", "");
    }

    let build_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let counter = vdb.next_counter()?;
    let elf = crate::elfscan::scan_image(&image_dir);
    let spec = merge_spec_from_env(
        env,
        ebuild.cpv().clone(),
        contents,
        elf,
        size,
        build_time,
        counter,
    );
    let installed = vdb.register(&spec)?;

    // Copy the ebuild into the VDB entry as `<PF>.ebuild`, as portage does.
    let pf = format!("{}-{}", ebuild.name(), ebuild.version());
    let ebuild_dest = installed.path().join(format!("{pf}.ebuild"));
    if let Err(e) = std::fs::copy(ebuild.path(), ebuild_dest.as_std_path()) {
        eprintln!("warning: could not copy ebuild into VDB: {e}");
    }

    if let Ok(ref data) = env_dump
        && let Err(e) = write_environment_bz2(&installed, data)
    {
        eprintln!("warning: could not write environment.bz2: {e}");
    }

    println!(
        "merge: {}/{}-{} registered (counter={counter})",
        ebuild.category(),
        ebuild.name(),
        ebuild.version()
    );

    if !protected.is_empty() {
        println!(
            "\n * {} protected config file(s) were installed with a ._cfg name.\n \
             * Run `em dispatch` (dispatch-conf) or `em etc` to merge them:",
            protected.len()
        );
        for p in &protected {
            println!(" *   {p}");
        }
    }

    shell
        .run_phase(
            ebuild,
            "postinst",
            work_root.as_std_path(),
            root.as_std_path(),
        )
        .await
        .context("pkg_postinst failed")?;

    Ok(())
}

/// Run `pkg_prerm`, delete `old_pkg`'s CONTENTS files that aren't also owned
/// by `new_contents` (an empty slice for a full removal, as opposed to an
/// in-place replace), unregister from the VDB, then run `pkg_postrm` — all
/// within `old_work_root` (a scratch dir the caller owns and cleans up).
///
/// Shared by [`unmerge_slot_occupant`] (an in-place replace during a normal
/// merge, which also presets `REPLACED_BY_VERSION` before calling this) and
/// [`unmerge_standalone`] (the standalone `-C`/`--unmerge` command — no
/// replacement, so no `REPLACED_BY_VERSION`).
#[allow(clippy::too_many_arguments)]
async fn unmerge_package(
    shell: &mut portage_repo::EbuildShell,
    old_pkg: &InstalledPackage,
    old_work_root: &Utf8Path,
    root: &Utf8Path,
    vdb: &Vdb,
    new_contents: &[ContentsEntry],
    graph: &preserve_libs::LinkGraph,
    registry: &mut preserve_libs::PreservedLibsRegistry,
) -> Result<()> {
    let old_pn = old_pkg.cpv().cpn.package.as_ref();
    let old_pvr = old_pkg.cpv().version.to_string();
    let old_pf = format!("{old_pn}-{old_pvr}");

    std::fs::create_dir_all(old_work_root.join("temp").as_std_path())
        .context("creating old work root")?;

    // `run_merge` copies the ebuild into the VDB entry itself as `<PF>.ebuild`
    // (see the `std::fs::copy` next to `write_environment_bz2`), so this is
    // reliably present for any cleanly-merged package — no need to re-derive
    // a repo-relative path from `old_pkg.category()`, which for a
    // cross-derived package is the virtual alias identity
    // (`cross-<tuple>`), never a real on-disk category: that always missed,
    // forcing every cross-category replace through the `environment.bz2`
    // fallback below unconditionally.
    //
    // Copied (not sourced in place) into `old_work_root`, which outlives
    // `vdb.unregister()` below: pkg_postrm runs *after* unregister removes
    // the VDB directory, so sourcing straight from `old_pkg.path()` would
    // work for pkg_prerm but then fail for pkg_postrm with an I/O error once
    // its source file is gone.
    let old_ebuild_src = old_pkg.path().join(format!("{old_pf}.ebuild"));
    let old_ebuild_path = old_work_root.join(format!("{old_pf}.ebuild"));

    // The VDB already has this package's authoritative Cpv (`old_pkg.cpv()`),
    // which for a cross-derived package is the virtual identity
    // (`cross-<tuple>/gcc-...`) it was registered under — `Ebuild::from_path`
    // would instead re-derive CATEGORY from `old_ebuild_path`'s on-disk
    // directory name, recovering the wrong (real, not virtual) category.
    let old_ebuild =
        if std::fs::copy(old_ebuild_src.as_std_path(), old_ebuild_path.as_std_path()).is_ok() {
            Some(Ebuild::with_cpv(old_pkg.cpv().clone(), &old_ebuild_path))
        } else {
            eprintln!(
                "warning: old ebuild not found at {old_ebuild_src}, skipping pkg_prerm/pkg_postrm"
            );
            None
        };

    let old_sourced = match &old_ebuild {
        Some(e) => {
            shell
                .run_phase(e, "prerm", old_work_root.as_std_path(), root.as_std_path())
                .await
                .context("pkg_prerm failed")?;
            true
        }
        None => try_run_phase_from_env_bz2(shell, old_pkg, "prerm", old_work_root, root).await,
    };

    let old_contents = old_pkg.contents().context("reading old CONTENTS")?;

    // preserve-libs (portage's FEATURES=preserve-libs): never physically
    // delete a shared library some other still-installed object's
    // NEEDED.ELF.2 genuinely requires and that nothing else provides. See
    // `preserve_libs` module doc. `graph`/`registry` are built/loaded once
    // per batch by the caller, not per package here.
    let to_preserve = preserve_libs::find_libs_to_preserve(graph, old_pkg, &old_contents);
    let preserve_paths: HashSet<Utf8PathBuf> = to_preserve
        .iter()
        .flat_map(|e| std::iter::once(e.path.clone()).chain(e.soname_symlink.clone()))
        .collect();
    if !to_preserve.is_empty() {
        preserve_libs::report_preserved(old_pkg.cpv(), &to_preserve, vdb);
    }
    registry.register(
        old_pkg.cpv(),
        &old_pkg.slot().unwrap_or_default(),
        old_pkg.counter().ok().flatten().unwrap_or(0),
        preserve_paths.iter().cloned().collect(),
    );

    remove_old_unique_files(&old_contents, new_contents, &preserve_paths, root)?;

    vdb.unregister(old_pkg)
        .context("unregistering old package")?;

    if old_sourced {
        match &old_ebuild {
            Some(e) => {
                shell
                    .run_phase(e, "postrm", old_work_root.as_std_path(), root.as_std_path())
                    .await
                    .context("pkg_postrm failed")?;
            }
            None => {
                let _ =
                    try_run_phase_from_env_bz2(shell, old_pkg, "postrm", old_work_root, root).await;
            }
        }
    }

    Ok(())
}

/// `graph`/`registry`: built/loaded once by the caller (a single in-place
/// replace only ever removes this one old occupant, so "once per batch"
/// and "once per call" coincide here — no batching concern like `-C`'s
/// multi-atom case).
#[allow(clippy::too_many_arguments)]
async fn unmerge_slot_occupant(
    shell: &mut portage_repo::EbuildShell,
    old_pkg: &InstalledPackage,
    work_root: &Utf8Path,
    root: &Utf8Path,
    vdb: &Vdb,
    new_contents: &[ContentsEntry],
    new_version: &portage_atom::Version,
    graph: &preserve_libs::LinkGraph,
    registry: &mut preserve_libs::PreservedLibsRegistry,
) -> Result<()> {
    // PMS 11.1: the old package's pkg_prerm/pkg_postrm see the version
    // replacing it.
    shell.preset_var("REPLACED_BY_VERSION", &new_version.to_string());
    let old_pn = old_pkg.cpv().cpn.package.as_ref();
    let old_pvr = old_pkg.cpv().version.to_string();
    let old_pf = format!("{old_pn}-{old_pvr}");
    let old_work_root = work_root
        .parent()
        .unwrap_or(work_root)
        .join(format!("{old_pf}.old"));

    unmerge_package(
        shell,
        old_pkg,
        &old_work_root,
        root,
        vdb,
        new_contents,
        graph,
        registry,
    )
    .await?;
    let _ = std::fs::remove_dir_all(old_work_root.as_std_path());
    Ok(())
}

/// Standalone removal for `-C`/`--unmerge` (`emerge.rs::unmerge_atoms`): no
/// replacement and no active install to derive a sibling scratch dir from,
/// so the scratch tree is `<work_base>/<category>/<pf>.unmerge`. Reuses
/// [`unmerge_package`] with an empty `new_contents`, so every file the
/// package owns is removed (except any preserve-libs finds still in use).
///
/// `graph`/`registry`: built/loaded once by the caller for the *whole*
/// removal batch (all atoms matched on the command line, not just
/// `old_pkg`) — see `preserve_libs::build_link_graph`'s doc for why a
/// multi-atom batch needs one shared graph, not one per package.
pub async fn unmerge_standalone(
    shell: &mut portage_repo::EbuildShell,
    old_pkg: &InstalledPackage,
    work_base: &Utf8Path,
    root: &Utf8Path,
    vdb: &Vdb,
    graph: &preserve_libs::LinkGraph,
    registry: &mut preserve_libs::PreservedLibsRegistry,
) -> Result<()> {
    let old_work_root = work_base
        .join(old_pkg.category())
        .join(format!("{}.unmerge", old_pkg.pf()));

    unmerge_package(
        shell,
        old_pkg,
        &old_work_root,
        root,
        vdb,
        &[],
        graph,
        registry,
    )
    .await?;
    let _ = std::fs::remove_dir_all(old_work_root.as_std_path());
    Ok(())
}

async fn try_run_phase_from_env_bz2(
    shell: &mut portage_repo::EbuildShell,
    pkg: &InstalledPackage,
    phase: &str,
    work_root: &Utf8Path,
    root: &Utf8Path,
) -> bool {
    let env_bz2 = pkg.path().join("environment.bz2");
    if !env_bz2.exists() {
        return false;
    }

    let temp_env = work_root.join("temp/environment.old");
    let compressed = match std::fs::read(env_bz2.as_std_path()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("warning: could not read environment.bz2: {e}");
            return false;
        }
    };
    let decompressed = match decompress_bzip2(&compressed) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("warning: could not decompress environment.bz2: {e}");
            return false;
        }
    };
    if let Err(e) = std::fs::write(temp_env.as_std_path(), &decompressed) {
        eprintln!("warning: could not write temp environment: {e}");
        return false;
    }

    let source_cmd = format!(". '{}'", temp_env.as_str().replace('\'', "'\\''"));
    if shell.run_string(&source_cmd).await.is_err() {
        eprintln!("warning: could not source saved environment");
        return false;
    }

    let func = match phase {
        "prerm" => "pkg_prerm",
        "postrm" => "pkg_postrm",
        other => other,
    };

    let root_str = {
        let s = root.as_str();
        if s.ends_with('/') {
            s.to_owned()
        } else {
            format!("{s}/")
        }
    };
    // The phase function is entirely optional (PMS: an ebuild need not
    // define pkg_prerm/pkg_postrm at all) — guard the call so a package
    // that simply never defined it doesn't produce a spurious "command
    // not found" for a hook nothing was ever supposed to run.
    if let Err(e) = shell
        .run_string(&format!(
            "if declare -F '{func}' >/dev/null 2>&1; then \
             ROOT='{root_str}' EROOT='{root_str}' EBUILD_PHASE_FUNC='{func}' {func}; \
             fi"
        ))
        .await
    {
        eprintln!("warning: {func} from environment.bz2 failed: {e}");
    }

    true
}

/// Resolve a CONTENTS/image path under `root`, rejecting absolute segments and
/// `..` components that would escape the intended merge root.
fn safe_dest_under(root: &Utf8Path, path: &Utf8Path) -> Result<Utf8PathBuf> {
    let rel = path.strip_prefix("/").unwrap_or(path);
    for c in rel.components() {
        match c {
            camino::Utf8Component::Normal(_) | camino::Utf8Component::CurDir => {}
            _ => {
                bail!("unsafe path escapes root {root}: {path}");
            }
        }
    }
    Ok(root.join(rel))
}

fn remove_old_unique_files(
    old_contents: &[ContentsEntry],
    new_contents: &[ContentsEntry],
    preserve: &HashSet<Utf8PathBuf>,
    root: &Utf8Path,
) -> Result<()> {
    let new_paths: HashSet<&Utf8PathBuf> = new_contents.iter().map(|e| &e.path).collect();

    for entry in old_contents.iter().rev() {
        if new_paths.contains(&entry.path) || preserve.contains(&entry.path) {
            continue;
        }
        let dest = match safe_dest_under(root, &entry.path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("warning: skipping unsafe CONTENTS path: {e:#}");
                continue;
            }
        };

        match entry.kind {
            ContentsKind::Obj | ContentsKind::Sym => {
                if (dest.exists() || std::fs::symlink_metadata(dest.as_std_path()).is_ok())
                    && let Err(e) = std::fs::remove_file(dest.as_std_path())
                {
                    eprintln!("warning: could not remove {dest}: {e}");
                }
            }
            ContentsKind::Dir => {
                let _ = std::fs::remove_dir(dest.as_std_path());
            }
            _ => {}
        }
    }
    Ok(())
}

fn run_clean(work_root: &Utf8Path) -> Result<()> {
    if work_root.exists() {
        std::fs::remove_dir_all(work_root).with_context(|| format!("cleaning {work_root}"))?;
        println!("clean: removed {work_root}");
    } else {
        println!("clean: {work_root} does not exist, nothing to do");
    }
    Ok(())
}

/// CONFIG_PROTECT / CONFIG_PROTECT_MASK resolution (portage's `ConfigProtect`).
///
/// A path is protected when the longest matching `CONFIG_PROTECT` prefix is
/// longer than the longest matching `CONFIG_PROTECT_MASK` prefix. Protected
/// files that already exist and differ are diverted to `._cfgNNNN_<name>`
/// for `dispatch-conf`/`etc-update` instead of being overwritten.
struct ConfigProtect {
    protect: Vec<String>,
    mask: Vec<String>,
}

impl ConfigProtect {
    /// Read the lists from the configured shell. `/etc` is always protected
    /// (portage's make.globals guarantees it).
    fn from_shell(shell: &portage_repo::EbuildShell) -> Self {
        let read = |name: &str| -> Vec<String> {
            shell
                .get_var(name)
                .unwrap_or_default()
                .split_whitespace()
                .map(|s| s.trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let mut protect = read("CONFIG_PROTECT");
        if !protect.iter().any(|p| p == "/etc") {
            protect.push("/etc".to_string());
        }
        Self {
            protect,
            mask: read("CONFIG_PROTECT_MASK"),
        }
    }

    /// Length of the longest entry in `list` that prefix-matches `obj` on
    /// whole components (`obj == p` or `obj` under `p/`); 0 if none.
    fn longest_match(list: &[String], obj: &str) -> usize {
        list.iter()
            .filter(|p| obj == p.as_str() || obj.starts_with(&format!("{p}/")))
            .map(String::len)
            .max()
            .unwrap_or(0)
    }

    fn is_protected(&self, obj: &Utf8Path) -> bool {
        let obj = obj.as_str();
        Self::longest_match(&self.protect, obj) > Self::longest_match(&self.mask, obj)
    }

    /// A config-protection set that protects nothing (for tests / contexts
    /// where protection does not apply).
    #[cfg(test)]
    fn none() -> Self {
        Self {
            protect: vec![],
            mask: vec![],
        }
    }
}

/// portage's `new_protect_filename`: the next `._cfgNNNN_<name>` beside
/// `dest` (highest existing index + 1), plus the most recent existing one
/// so the caller can reuse it when the content already matches.
fn scan_cfg(dest: &Utf8Path) -> (Utf8PathBuf, Option<Utf8PathBuf>) {
    let dir = dest.parent().unwrap_or_else(|| Utf8Path::new("/"));
    let name = dest.file_name().unwrap_or_default();
    let mut highest: i32 = -1;
    let mut latest: Option<Utf8PathBuf> = None;
    if let Ok(rd) = std::fs::read_dir(dir.as_std_path()) {
        for entry in rd.flatten() {
            let Ok(f) = entry.file_name().into_string() else {
                continue;
            };
            // ._cfg<4 digits>_<name>
            let Some(rest) = f.strip_prefix("._cfg") else {
                continue;
            };
            if rest.len() > 5
                && rest.as_bytes()[4] == b'_'
                && &rest[5..] == name
                && let Ok(n) = rest[..4].parse::<i32>()
                && n > highest
            {
                highest = n;
                latest = Some(dir.join(&f));
            }
        }
    }
    (dir.join(format!("._cfg{:04}_{name}", highest + 1)), latest)
}

/// Set a symlink's own atime/mtime. `std::fs` always follows symlinks, so we
/// go through `utimensat(AT_SYMLINK_NOFOLLOW)`. Best-effort: failures are
/// ignored, matching the regular-file mtime path.
fn set_symlink_times(path: &Utf8Path, meta: &std::fs::Metadata) {
    use rustix::fs::{AtFlags, CWD, Timespec, Timestamps, utimensat};
    let to_ts = |t: std::io::Result<std::time::SystemTime>| -> Timespec {
        let d = t
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .unwrap_or_default();
        Timespec {
            tv_sec: d.as_secs() as i64,
            tv_nsec: d.subsec_nanos() as i64,
        }
    };
    let times = Timestamps {
        last_access: to_ts(meta.accessed()),
        last_modification: to_ts(meta.modified()),
    };
    let _ = utimensat(CWD, path.as_str(), &times, AtFlags::SYMLINK_NOFOLLOW);
}

/// Result of merging the image into ROOT.
struct WalkResult {
    contents: Vec<ContentsEntry>,
    size: u64,
    /// Installed paths whose update was diverted to a `._cfg` file.
    protected: Vec<Utf8PathBuf>,
}

fn walk_image(
    image_dir: &Utf8Path,
    dest_root: &Utf8Path,
    cp: &ConfigProtect,
) -> Result<WalkResult> {
    if !image_dir.exists() {
        return Ok(WalkResult {
            contents: vec![],
            size: 0,
            protected: vec![],
        });
    }

    let mut contents: Vec<ContentsEntry> = Vec::new();
    let mut total_size: u64 = 0;
    let mut protected: Vec<Utf8PathBuf> = Vec::new();
    // Source (dev, ino) -> first merged dest, for re-creating intra-image
    // hardlinks as shared inodes in ROOT.
    let mut hardlinks: std::collections::HashMap<(u64, u64), Utf8PathBuf> =
        std::collections::HashMap::new();
    let mut queue: std::collections::VecDeque<Utf8PathBuf> = std::collections::VecDeque::new();
    queue.push_back(image_dir.to_path_buf());

    while let Some(dir) = queue.pop_front() {
        let read_dir = std::fs::read_dir(dir.as_std_path())
            .with_context(|| format!("reading image dir {dir}"))?;

        for entry in read_dir {
            let entry = entry.context("reading dir entry")?;
            let src_path: Utf8PathBuf = entry
                .path()
                .try_into()
                .map_err(|_| anyhow::anyhow!("non-UTF-8 path in image"))?;

            let rel = src_path
                .strip_prefix(image_dir)
                .map_err(|_| anyhow::anyhow!("path escape: {src_path}"))?;
            // Reject `..` / absolute components in the relative image path so a
            // hostile image cannot write outside dest_root.
            let dest_path = safe_dest_under(dest_root, &Utf8PathBuf::from("/").join(rel))?;
            let installed = Utf8PathBuf::from("/").join(rel);

            let meta = std::fs::symlink_metadata(src_path.as_std_path())
                .with_context(|| format!("stat {src_path}"))?;

            if meta.file_type().is_symlink() {
                let raw_target = std::fs::read_link(src_path.as_std_path())
                    .with_context(|| format!("readlink {src_path}"))?;
                let target: Utf8PathBuf = raw_target
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("non-UTF-8 symlink target"))?;
                // Symlinks are config-protectable too (portage bug #485598):
                // divert when an existing link points somewhere different.
                let write_path = if cp.is_protected(&installed) {
                    match std::fs::read_link(dest_path.as_std_path()) {
                        Ok(existing) if existing == target.as_std_path() => dest_path.clone(),
                        Ok(_) => {
                            let (next, latest) = scan_cfg(&dest_path);
                            let reuse = latest.filter(|p| {
                                std::fs::read_link(p.as_std_path())
                                    .is_ok_and(|t| t == target.as_std_path())
                            });
                            protected.push(installed.clone());
                            reuse.unwrap_or(next)
                        }
                        Err(_) => dest_path.clone(),
                    }
                } else {
                    dest_path.clone()
                };
                if std::fs::symlink_metadata(write_path.as_std_path()).is_ok() {
                    std::fs::remove_file(write_path.as_std_path())
                        .with_context(|| format!("removing {write_path}"))?;
                }
                std::os::unix::fs::symlink(target.as_std_path(), write_path.as_std_path())
                    .with_context(|| format!("symlink {write_path}"))?;
                // Preserve the link's own mtime (std follows symlinks; this
                // does not), so the on-disk time matches CONTENTS.
                set_symlink_times(&write_path, &meta);
                preserve_owner(&write_path, &meta);
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                contents.push(ContentsEntry {
                    kind: ContentsKind::Sym,
                    path: installed,
                    md5: None,
                    mtime,
                    target: Some(target),
                });
            } else if meta.is_dir() {
                std::fs::create_dir_all(dest_path.as_std_path())
                    .with_context(|| format!("mkdir {dest_path}"))?;
                preserve_owner(&dest_path, &meta);
                contents.push(ContentsEntry {
                    kind: ContentsKind::Dir,
                    path: installed,
                    md5: None,
                    mtime: None,
                    target: None,
                });
                queue.push_back(src_path);
            } else if meta.is_file() {
                if let Some(parent) = dest_path.parent() {
                    std::fs::create_dir_all(parent.as_std_path())
                        .with_context(|| format!("mkdir {parent}"))?;
                }
                let src_data = std::fs::read(src_path.as_std_path())
                    .with_context(|| format!("reading {src_path}"))?;
                let md5_str = format!("{:x}", md5::compute(&src_data));

                // Config protection: an existing, differing file in a
                // protected path is written to a `._cfg` sidecar (.keep
                // placeholders are never protected). CONTENTS still records
                // the real path with the new md5, matching portage.
                let is_keep = meta.len() == 0
                    && installed
                        .file_name()
                        .is_some_and(|n| n.starts_with(".keep"));
                let write_path = if !is_keep
                    && cp.is_protected(&installed)
                    && std::fs::symlink_metadata(dest_path.as_std_path()).is_ok()
                {
                    let same = std::fs::read(dest_path.as_std_path())
                        .is_ok_and(|d| format!("{:x}", md5::compute(&d)) == md5_str);
                    if same {
                        dest_path.clone()
                    } else {
                        let (next, latest) = scan_cfg(&dest_path);
                        let reuse = latest.filter(|p| {
                            std::fs::read(p.as_std_path())
                                .is_ok_and(|d| format!("{:x}", md5::compute(&d)) == md5_str)
                        });
                        protected.push(installed.clone());
                        reuse.unwrap_or(next)
                    }
                } else {
                    dest_path.clone()
                };

                // Hardlink preservation: a file already hardlinked inside the
                // image (nlink > 1) is recreated as a hardlink in ROOT,
                // sharing one inode, rather than copied independently (matches
                // portage's source-inode `_hardlink_merge_map`).
                use std::os::unix::fs::MetadataExt;
                let inode = (meta.dev(), meta.ino());
                let mut linked = false;
                if meta.nlink() > 1
                    && let Some(first) = hardlinks.get(&inode)
                {
                    let _ = std::fs::remove_file(write_path.as_std_path());
                    if std::fs::hard_link(first.as_std_path(), write_path.as_std_path()).is_ok() {
                        linked = true;
                    }
                }

                if !linked {
                    // Portage unlinks the destination before installing. A bare
                    // `std::fs::copy` opens the existing file O_WRONLY|O_TRUNC,
                    // which is EACCES when the destination is read-only (e.g.
                    // bash's mode-0555 `bashbug` on re-merge). Removing first
                    // lets the copy create a fresh file, which needs only write
                    // permission on the *directory* (not the file). Ignore
                    // NotFound (fresh install); any other unlink error falls
                    // through to `copy`, which surfaces the canonical message.
                    if let Err(e) = std::fs::remove_file(write_path.as_std_path())
                        && e.kind() != std::io::ErrorKind::NotFound
                    {
                        let _ = e;
                    }
                    std::fs::copy(src_path.as_std_path(), write_path.as_std_path())
                        .with_context(|| format!("copy {src_path} → {write_path}"))?;
                    std::fs::set_permissions(write_path.as_std_path(), meta.permissions())
                        .with_context(|| format!("chmod {write_path}"))?;
                    // Preserve the image file's mtime (portage does), so the
                    // on-disk time matches what CONTENTS records.
                    if let Ok(modified) = meta.modified()
                        && let Ok(f) = std::fs::File::options()
                            .write(true)
                            .open(write_path.as_std_path())
                    {
                        let _ = f.set_modified(modified);
                    }
                    if meta.nlink() > 1 {
                        hardlinks.insert(inode, write_path.clone());
                    }
                }
                preserve_owner(&write_path, &meta);

                total_size += meta.len();
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                contents.push(ContentsEntry {
                    kind: ContentsKind::Obj,
                    path: installed,
                    md5: Some(md5_str),
                    mtime,
                    target: None,
                });
            }
        }
    }

    Ok(WalkResult {
        contents,
        size: total_size,
        protected,
    })
}

/// Set the merged path's owner to the image entry's uid/gid (`lchown`, so a
/// symlink's own ownership is set, not its target). Succeeds as real root and
/// under a fake root (fakeroost records the intended owner); a genuinely
/// unprivileged merge can't set foreign ownership, so the error is ignored and
/// the file keeps the build user — portage's unprivileged behaviour. portage
/// preserves image ownership on merge; em did not, so even a *root* install left
/// non-root-owned files (`acct-user/*` dirs) owned by the invoking user.
fn preserve_owner(path: &Utf8Path, meta: &std::fs::Metadata) {
    use std::os::unix::fs::MetadataExt;
    let _ = std::os::unix::fs::lchown(path.as_std_path(), Some(meta.uid()), Some(meta.gid()));
}

async fn capture_environment(
    shell: &mut portage_repo::EbuildShell,
    work_root: &Utf8Path,
) -> std::result::Result<Vec<u8>, String> {
    // `filter_declare_dump` only touches lines that literally start with
    // `declare -` (no leading whitespace), so it strips the top-level
    // `declare -p` variable entries it targets (readonly/dynamic bash
    // specials like EUID, PIPESTATUS, ...) without touching any `declare -f`
    // function definition or its (always-indented) body. Without this,
    // `try_run_phase_from_env_bz2` re-sourcing this dump to run
    // pkg_prerm/pkg_postrm hits "declare: cannot mutate readonly variable"
    // for every bash-builtin-readonly var the dump captured — re-declaring
    // them is never meaningful since a fresh shell already has them set.
    let dump = capture_shell_dump(shell, work_root, "{ declare -p; declare -f; }").await?;
    let text = String::from_utf8_lossy(&dump);
    Ok(filter_declare_dump(&text).into_bytes())
}

/// Variables-only dump for the Compile→Install worker handoff. Deliberately no
/// `declare -f`: the worker re-sources the ebuild (which defines every
/// ebuild/eclass function), and brush's function printer does not round-trip
/// heredoc bodies (the indented `<<-EOF` delimiter never terminates), which
/// would make the whole dump unparseable. Readonly declares are dropped —
/// re-declaring them in the worker only produces "cannot mutate readonly
/// variable" noise (portage's env restore filters them the same way).
///
/// Also drops bash's own dynamic/special variables (`PIPESTATUS`,
/// `FUNCNAME`, …): `declare -p` prints them like any other variable, but
/// re-`declare`ing them in the worker pins a stale snapshot in place of the
/// shell's live tracking. Confirmed concretely for `PIPESTATUS`: real bash
/// unconditionally replaces the whole array on every new pipeline regardless
/// of a prior explicit `declare`, but brush does not — once user code (here,
/// our own restore) has declared it, brush never resizes it again, so a
/// later 2-stage pipe in the Install worker still reports the compile
/// parent's stale 1-element snapshot. That silently broke `distutils-r1`'s
/// `pipestatus || die` check (`dev-python/jinja2`, the `install//usr/bin`
/// listing failure — see `todo/brush-pipestatus-not-reset.md` for the
/// brush-side bug and repro). Filtering the dump is the correct fix
/// independent of that brush bug: these variables are bash-maintained state,
/// not build environment, and were never meant to cross a process boundary.
/// Bash's own dynamic/special variables: never worth restoring across the
/// Compile→Install process boundary (see `capture_variables`'s doc comment).
const DYNAMIC_VAR_DENYLIST: &[&str] = &[
    "PIPESTATUS",
    "FUNCNAME",
    "BASH_LINENO",
    "BASH_SOURCE",
    "BASH_ARGV",
    "BASH_ARGV0",
    "BASH_ARGC",
    "BASH_CMDS",
    "BASH_COMMAND",
    "BASH_SUBSHELL",
    "BASH_ALIASES",
];

/// The variable name declared by one `declare -p` output line, e.g. `"PATH"`
/// from `declare -x PATH="/usr/bin"`.
fn declared_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("declare -")?;
    let (_flags, name_and_value) = rest.split_once(char::is_whitespace)?;
    Some(name_and_value.trim_start().split('=').next().unwrap_or(""))
}

/// Drop readonly declares and bash's dynamic/special variables from a
/// `declare -p` dump, keeping everything else verbatim (including blank/
/// non-`declare` lines, so callers can filter arbitrary text defensively).
fn filter_declare_dump(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let is_readonly = l
                .strip_prefix("declare -")
                .and_then(|rest| rest.split_whitespace().next())
                .is_some_and(|flags| flags.contains('r'));
            let is_dynamic =
                declared_name(l).is_some_and(|name| DYNAMIC_VAR_DENYLIST.contains(&name));
            !is_readonly && !is_dynamic
        })
        .fold(String::with_capacity(text.len()), |mut acc, l| {
            acc.push_str(l);
            acc.push('\n');
            acc
        })
}

async fn capture_variables(
    shell: &mut portage_repo::EbuildShell,
    work_root: &Utf8Path,
) -> std::result::Result<Vec<u8>, String> {
    let dump = capture_shell_dump(shell, work_root, "declare -p").await?;
    let text = String::from_utf8_lossy(&dump);
    Ok(filter_declare_dump(&text).into_bytes())
}

async fn capture_shell_dump(
    shell: &mut portage_repo::EbuildShell,
    work_root: &Utf8Path,
    dump_cmd: &str,
) -> std::result::Result<Vec<u8>, String> {
    let dump_path = work_root.join("temp/environment");
    let path_escaped = dump_path.as_str().replace('\'', "'\\''");
    shell
        .run_string(&format!(
            "{dump_cmd} > '{path_escaped}' 2>/dev/null || true"
        ))
        .await
        .map_err(|e| format!("environment capture failed: {e}"))?;
    std::fs::read(dump_path.as_std_path()).map_err(|e| format!("reading env dump: {e}"))
}

fn write_environment_bz2(pkg: &InstalledPackage, env_data: &[u8]) -> Result<()> {
    use std::io::Write;

    let path = pkg.path().join("environment.bz2");
    let mut encoder = BzEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(env_data)
        .context("compressing environment")?;
    let compressed = encoder.finish().context("finalizing bzip2")?;
    std::fs::write(path.as_std_path(), compressed).context("writing environment.bz2")
}

fn decompress_bzip2(data: &[u8]) -> std::result::Result<Vec<u8>, String> {
    use bzip2::read::BzDecoder;
    use std::io::Read;

    let mut decoder = BzDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| format!("bzip2 decompress: {e}"))?;
    Ok(out)
}

fn merge_spec_from_env(
    env: EbuildEnv,
    cpv: portage_atom::Cpv,
    contents: Vec<ContentsEntry>,
    elf: crate::elfscan::ElfScan,
    size: u64,
    build_time: u64,
    counter: u64,
) -> MergeSpec {
    MergeSpec {
        cpv,
        eapi: env.eapi,
        slot: env.slot,
        use_flags: env.use_flags,
        iuse: env.iuse,
        depend: env.depend,
        rdepend: env.rdepend,
        bdepend: env.bdepend,
        pdepend: env.pdepend,
        idepend: env.idepend,
        keywords: env.keywords,
        license: env.license,
        description: env.description,
        homepage: env.homepage,
        restrict: env.restrict,
        properties: env.properties,
        defined_phases: env.defined_phases,
        repository: env.repository,
        inherited: env.inherited,
        features: env.features,
        chost: env.chost,
        cbuild: env.cbuild,
        cflags: env.cflags,
        cxxflags: env.cxxflags,
        ldflags: env.ldflags,
        needed: elf.needed,
        needed_elf2: elf.needed_elf2,
        requires: elf.requires,
        provides: elf.provides,
        contents,
        build_time,
        size,
        counter,
    }
}

fn vdb_root_for(root: &Utf8Path) -> Utf8PathBuf {
    if root.as_str() == "/" {
        Utf8PathBuf::from("/var/db/pkg")
    } else {
        root.join("var/db/pkg")
    }
}

fn open_or_create_vdb(path: &Utf8Path) -> Result<Vdb> {
    if !path.exists() {
        std::fs::create_dir_all(path.as_std_path())
            .with_context(|| format!("creating VDB at {path}"))?;
    }
    Vdb::open(path).with_context(|| format!("opening VDB at {path}"))
}

fn gentoo_mirrors_list() -> Vec<String> {
    if let Ok(val) = std::env::var("GENTOO_MIRRORS")
        && !val.trim().is_empty()
    {
        return val.split_whitespace().map(str::to_owned).collect();
    }
    // make.conf rarely sets GENTOO_MIRRORS — the default
    // (`http://distfiles.gentoo.org`) lives in make.globals, so include it last.
    // Without it the mirror list is empty and a distfile whose upstream URL fails
    // has no fallback (the popt/tar fetch failures in the @system stage build).
    for candidate in [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF, MAKE_GLOBALS] {
        let p = Utf8Path::new(candidate);
        if p.exists()
            && let Ok(mc) = MakeConf::load(p)
            && let Some(val) = mc.get("GENTOO_MIRRORS")
        {
            return val.split_whitespace().map(str::to_owned).collect();
        }
    }
    vec![]
}

/// Portage's shipped defaults; the source of `GENTOO_MIRRORS` when neither the
/// environment nor make.conf overrides it.
const MAKE_GLOBALS: &str = "/usr/share/portage/config/make.globals";

fn read_fetch_commands() -> (Option<String>, Option<String>) {
    for candidate in [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF] {
        let p = Utf8Path::new(candidate);
        if p.exists()
            && let Ok(mc) = MakeConf::load(p)
        {
            let fetch = mc.get("FETCHCOMMAND").map(str::to_owned);
            let resume = mc.get("RESUMECOMMAND").map(str::to_owned);
            if fetch.is_some() || resume.is_some() {
                return (fetch, resume);
            }
        }
    }
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_vdb::ContentsKind;
    use std::fs;
    use std::os::unix::fs::symlink;

    /// Regression test for the gnupg stage1 failure: the Install worker must
    /// never wipe `temp` (`${T}`) — it's cross-phase scratch space the
    /// Compile parent may have staged a file into for `src_install` to
    /// consume, not throwaway-per-process state like `image` (`${D}`).
    #[test]
    fn install_worker_clean_subs_never_includes_temp() {
        let install_subs = PhaseGroup::Install.clean_subs().unwrap();
        assert!(
            !install_subs.contains(&"temp"),
            "Install must not wipe temp (${{T}}): {install_subs:?}"
        );
        // image (${D}) still must be wiped: a stale install destination from
        // an earlier attempt must never leak into the current merge.
        assert!(install_subs.contains(&"image"));
    }

    #[test]
    fn safe_dest_under_rejects_parent_and_absolute_escapes() {
        let root = Utf8Path::new("/var/tmp/root");
        assert_eq!(
            safe_dest_under(root, Utf8Path::new("/usr/bin/foo")).unwrap(),
            Utf8PathBuf::from("/var/tmp/root/usr/bin/foo")
        );
        assert!(safe_dest_under(root, Utf8Path::new("/../../etc/passwd")).is_err());
        assert!(safe_dest_under(root, Utf8Path::new("../etc/passwd")).is_err());
        assert!(safe_dest_under(root, Utf8Path::new("/usr/../etc/passwd")).is_err());
    }

    /// Full/Compile/BinpkgMerge start genuinely fresh, so they wipe
    /// everything including `work` — unlike Install, which relies on
    /// `work`'s compile artifacts surviving the process boundary.
    #[test]
    fn full_and_compile_clean_subs_wipe_work_too() {
        for group in [
            PhaseGroup::Full,
            PhaseGroup::Compile,
            PhaseGroup::BinpkgMerge,
        ] {
            let subs = group.clean_subs().unwrap();
            assert!(subs.contains(&"work"), "{group:?}: {subs:?}");
            assert!(subs.contains(&"temp"), "{group:?}: {subs:?}");
        }
    }

    /// Regression test for the jinja2 stage3 failure: restoring `PIPESTATUS`
    /// (or the other bash dynamic vars) into the Install worker pins a stale
    /// snapshot that brush never resizes on later pipelines — see
    /// `todo/brush-pipestatus-not-reset.md`. The fix is simply never
    /// dumping them in the first place.
    #[test]
    fn filter_declare_dump_drops_readonly_and_dynamic_vars() {
        let dump = concat!(
            "declare -x PATH=\"/usr/bin\"\n",
            "declare -ar SOME_READONLY=([0]=\"x\")\n",
            "declare -a PIPESTATUS=([0]=\"1\")\n",
            "declare -a FUNCNAME=\n",
            "declare -- BASH_ARGV0=\"\"\n",
            "declare -x BUILD_DIR=\"/work/foo\"\n",
        );
        let filtered = filter_declare_dump(dump);
        assert!(filtered.contains("PATH="));
        assert!(filtered.contains("BUILD_DIR="));
        assert!(!filtered.contains("SOME_READONLY"));
        assert!(!filtered.contains("PIPESTATUS"));
        assert!(!filtered.contains("FUNCNAME"));
        assert!(!filtered.contains("BASH_ARGV0"));
    }

    #[test]
    fn walk_image_copies_files_and_builds_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();

        fs::create_dir_all(image.join("usr/bin").as_std_path()).unwrap();
        fs::write(image.join("usr/bin/testprog").as_std_path(), b"#!/bin/sh\n").unwrap();
        symlink("testprog", image.join("usr/bin/tp").as_std_path()).unwrap();
        fs::create_dir_all(root.as_std_path()).unwrap();

        let WalkResult { contents, size, .. } =
            walk_image(&image, &root, &ConfigProtect::none()).unwrap();

        assert!(root.join("usr/bin/testprog").exists());
        assert!(
            root.join("usr/bin/tp")
                .as_std_path()
                .symlink_metadata()
                .is_ok()
        );

        let dirs: Vec<_> = contents
            .iter()
            .filter(|e| e.kind == ContentsKind::Dir)
            .collect();
        let objs: Vec<_> = contents
            .iter()
            .filter(|e| e.kind == ContentsKind::Obj)
            .collect();
        let syms: Vec<_> = contents
            .iter()
            .filter(|e| e.kind == ContentsKind::Sym)
            .collect();
        assert!(!dirs.is_empty());
        assert_eq!(objs.len(), 1);
        assert_eq!(syms.len(), 1);
        assert_eq!(objs[0].path, Utf8PathBuf::from("/usr/bin/testprog"));
        assert!(objs[0].md5.is_some());
        assert_eq!(syms[0].path, Utf8PathBuf::from("/usr/bin/tp"));
        assert_eq!(syms[0].target.as_deref(), Some(Utf8Path::new("testprog")));
        assert!(size > 0);
    }

    #[test]
    fn walk_image_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();
        fs::create_dir_all(image.as_std_path()).unwrap();
        fs::create_dir_all(root.as_std_path()).unwrap();

        let WalkResult { contents, size, .. } =
            walk_image(&image, &root, &ConfigProtect::none()).unwrap();
        assert!(contents.is_empty());
        assert_eq!(size, 0);
    }

    #[test]
    fn walk_image_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("no-such-image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();
        let WalkResult { contents, size, .. } =
            walk_image(&image, &root, &ConfigProtect::none()).unwrap();
        assert!(contents.is_empty());
        assert_eq!(size, 0);
    }

    #[test]
    fn config_protect_diverts_existing_differing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();

        // An existing, differing config file under a protected path.
        fs::create_dir_all(root.join("etc").as_std_path()).unwrap();
        fs::write(root.join("etc/foo.conf").as_std_path(), b"old\n").unwrap();
        // A masked subpath that auto-updates, and a brand-new protected file.
        fs::create_dir_all(root.join("etc/env.d").as_std_path()).unwrap();
        fs::write(root.join("etc/env.d/99x").as_std_path(), b"old\n").unwrap();

        fs::create_dir_all(image.join("etc/env.d").as_std_path()).unwrap();
        fs::write(image.join("etc/foo.conf").as_std_path(), b"new\n").unwrap();
        fs::write(image.join("etc/env.d/99x").as_std_path(), b"new\n").unwrap();
        fs::write(image.join("etc/new.conf").as_std_path(), b"fresh\n").unwrap();

        let cp = ConfigProtect {
            protect: vec!["/etc".into()],
            mask: vec!["/etc/env.d".into()],
        };
        let WalkResult {
            contents,
            protected,
            ..
        } = walk_image(&image, &root, &cp).unwrap();

        // Differing protected file diverted; original untouched.
        assert_eq!(
            fs::read(root.join("etc/foo.conf").as_std_path()).unwrap(),
            b"old\n"
        );
        assert_eq!(
            fs::read(root.join("etc/._cfg0000_foo.conf").as_std_path()).unwrap(),
            b"new\n"
        );
        // Masked path overwritten in place (no divert).
        assert_eq!(
            fs::read(root.join("etc/env.d/99x").as_std_path()).unwrap(),
            b"new\n"
        );
        assert!(!root.join("etc/._cfg0000_99x").exists());
        // New protected file merged directly.
        assert_eq!(
            fs::read(root.join("etc/new.conf").as_std_path()).unwrap(),
            b"fresh\n"
        );

        assert_eq!(protected, [Utf8PathBuf::from("/etc/foo.conf")]);
        // CONTENTS records the real path with the new md5, never the ._cfg.
        let foo = contents
            .iter()
            .find(|e| e.path == Utf8Path::new("/etc/foo.conf"))
            .unwrap();
        assert_eq!(
            foo.md5.as_deref(),
            Some(&*format!("{:x}", md5::compute(b"new\n")))
        );
        assert!(!contents.iter().any(|e| e.path.as_str().contains("._cfg")));
    }

    #[test]
    fn walk_image_overwrites_readonly_file() {
        // Re-merging over an existing read-only file (e.g. bash's mode-0555
        // bashbug) must not EACCES: the destination is unlinked before copy, so
        // the copy creates a fresh file (directory write perm is enough).
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();

        fs::create_dir_all(root.join("usr/bin").as_std_path()).unwrap();
        fs::write(root.join("usr/bin/bug").as_std_path(), b"old\n").unwrap();
        // Make the existing destination read-only (mode 0555).
        std::fs::set_permissions(
            root.join("usr/bin/bug").as_std_path(),
            std::fs::Permissions::from_mode(0o555),
        )
        .unwrap();

        fs::create_dir_all(image.join("usr/bin").as_std_path()).unwrap();
        fs::write(image.join("usr/bin/bug").as_std_path(), b"new\n").unwrap();

        let WalkResult { contents, .. } = walk_image(&image, &root, &ConfigProtect::none())
            .expect("re-merge over a read-only file must succeed (unlink before copy)");

        assert_eq!(
            fs::read(root.join("usr/bin/bug").as_std_path()).unwrap(),
            b"new\n"
        );
        // The image mode is applied after copy; the read-only 0555 dest was
        // replaced, so the new content is readable.
        assert!(contents.iter().any(|e| e.path == "/usr/bin/bug"));
    }

    #[test]
    fn walk_image_preserves_symlink_mtime() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();
        fs::create_dir_all(image.join("usr/bin").as_std_path()).unwrap();
        fs::write(image.join("usr/bin/tool").as_std_path(), b"x").unwrap();
        symlink("tool", image.join("usr/bin/tp").as_std_path()).unwrap();
        fs::create_dir_all(root.as_std_path()).unwrap();

        // Backdate the image symlink's own mtime.
        use rustix::fs::{AtFlags, CWD, Timespec, Timestamps, utimensat};
        let want = Timespec {
            tv_sec: 1_000_000_000,
            tv_nsec: 0,
        };
        let _ = utimensat(
            CWD,
            image.join("usr/bin/tp").as_str(),
            &Timestamps {
                last_access: want,
                last_modification: want,
            },
            AtFlags::SYMLINK_NOFOLLOW,
        );

        walk_image(&image, &root, &ConfigProtect::none()).unwrap();

        let merged = fs::symlink_metadata(root.join("usr/bin/tp").as_std_path()).unwrap();
        assert_eq!(merged.mtime(), 1_000_000_000);
    }

    #[test]
    fn walk_image_preserves_intra_image_hardlinks() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();

        fs::create_dir_all(image.join("usr/bin").as_std_path()).unwrap();
        fs::write(
            image.join("usr/bin/tool").as_std_path(),
            b"#!/bin/sh\necho hi\n",
        )
        .unwrap();
        // Two hardlinks to the same inode in the image.
        fs::hard_link(
            image.join("usr/bin/tool").as_std_path(),
            image.join("usr/bin/tool-alias").as_std_path(),
        )
        .unwrap();
        // A separate, identical-content file that is NOT a hardlink.
        fs::write(
            image.join("usr/bin/copy").as_std_path(),
            b"#!/bin/sh\necho hi\n",
        )
        .unwrap();
        fs::create_dir_all(root.as_std_path()).unwrap();

        walk_image(&image, &root, &ConfigProtect::none()).unwrap();

        let a = fs::metadata(root.join("usr/bin/tool").as_std_path()).unwrap();
        let b = fs::metadata(root.join("usr/bin/tool-alias").as_std_path()).unwrap();
        let c = fs::metadata(root.join("usr/bin/copy").as_std_path()).unwrap();
        // The two image-hardlinks share one inode in ROOT.
        assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));
        // The non-hardlinked file stays independent.
        assert_ne!((a.dev(), a.ino()), (c.dev(), c.ino()));
    }

    #[test]
    fn config_protect_reuses_matching_cfg_and_increments_otherwise() {
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();
        fs::create_dir_all(root.join("etc").as_std_path()).unwrap();
        fs::write(root.join("etc/foo.conf").as_std_path(), b"old\n").unwrap();
        // A pending ._cfg already holding the exact content we're about to install.
        fs::write(root.join("etc/._cfg0000_foo.conf").as_std_path(), b"new\n").unwrap();
        fs::create_dir_all(image.join("etc").as_std_path()).unwrap();
        fs::write(image.join("etc/foo.conf").as_std_path(), b"new\n").unwrap();

        let cp = ConfigProtect {
            protect: vec!["/etc".into()],
            mask: vec![],
        };
        walk_image(&image, &root, &cp).unwrap();
        // Reused the existing ._cfg0000 rather than creating ._cfg0001.
        assert!(!root.join("etc/._cfg0001_foo.conf").exists());
        assert_eq!(
            fs::read(root.join("etc/._cfg0000_foo.conf").as_std_path()).unwrap(),
            b"new\n"
        );
    }

    #[test]
    fn remove_old_unique_files_removes_only_unique() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        fs::create_dir_all(root.join("usr/bin").as_std_path()).unwrap();
        fs::write(root.join("usr/bin/old-only").as_std_path(), b"old").unwrap();
        fs::write(root.join("usr/bin/shared").as_std_path(), b"shared").unwrap();

        let old_contents = vec![
            ContentsEntry {
                kind: ContentsKind::Dir,
                path: "/usr/bin".into(),
                md5: None,
                mtime: None,
                target: None,
            },
            ContentsEntry {
                kind: ContentsKind::Obj,
                path: "/usr/bin/old-only".into(),
                md5: Some("aa".into()),
                mtime: Some(0),
                target: None,
            },
            ContentsEntry {
                kind: ContentsKind::Obj,
                path: "/usr/bin/shared".into(),
                md5: Some("bb".into()),
                mtime: Some(0),
                target: None,
            },
        ];
        let new_contents = vec![ContentsEntry {
            kind: ContentsKind::Obj,
            path: "/usr/bin/shared".into(),
            md5: Some("cc".into()),
            mtime: Some(1),
            target: None,
        }];

        remove_old_unique_files(&old_contents, &new_contents, &HashSet::new(), &root).unwrap();

        assert!(!root.join("usr/bin/old-only").exists());
        assert!(root.join("usr/bin/shared").exists());
        assert!(root.join("usr/bin").exists());
    }
}
