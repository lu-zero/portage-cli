use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use bzip2::Compression;
use bzip2::write::BzEncoder;
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Cpv;
use portage_distfiles::{DistfileResolver, FetchConfig, FetchStatus, Fetcher, RestrictGate};
use portage_metadata::{Eapi, RestrictExpr, SrcUriEntry};
use portage_repo::{Ebuild, EbuildEnv, MakeConf, Manifest, ReposConf, Repository};
use portage_vdb::{ContentsEntry, ContentsKind, InstalledPackage, MergeSpec, Vdb};
use tracing::Instrument;

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
    /// Full source build + merge: clean → `pretend..qmerge` → buildpkg → tree-drop
    Full,
    /// Pre-install phases only (the un-wrapped parent): clean → `pretend..compile` → dump env
    /// to `worker-env`
    ///
    /// No buildpkg, no tree-drop — the compile artifacts must survive for the Install worker.
    Compile,
    /// Install + qmerge (the wrapped worker): restore env from `worker-env` → `install,qmerge`
    /// → buildpkg → tree-drop
    ///
    /// Does NOT wipe `work/` (the compile artifacts live there); only `image/temp/homedir`.
    Install,
    /// Merge a pre-built GPKG (`-k`/`-g`): clean → extract image → `qmerge` → tree-drop
    ///
    /// No src_install — the extracted image is the payload.
    BinpkgMerge,
    /// `-B`/`--buildpkgonly`: clean → `pretend..install` → buildpkg → tree-drop
    ///
    /// No `qmerge` at all, unlike every other merge-shaped group — the image is packaged but
    /// never installed into the live ROOT/VDB. Real emerge's own caveat applies: this doesn't
    /// resolve or install anything, so the ebuild's own DEPEND/BDEPEND closure must already be
    /// satisfied on the build host.
    BuildOnly,
    /// `-f`/`--fetchonly`: resolve `SRC_URI` under the plan's USE and download distfiles into
    /// DISTDIR
    ///
    /// No unpack/build/install — mirrors emerge's `EbuildFetcher` short-circuit (no phase shell
    /// beyond what's needed for SRC_URI + RESTRICT=fetch / `pkg_nofetch`). `all_uri` is
    /// `-F`/`--fetch-all-uri`: every `SRC_URI` entry regardless of USE, instead of just what
    /// the plan's own USE selection asks for.
    FetchOnly { all_uri: bool },
    /// Debug (`em ebuild`): run the given phases only; no clean/drop/buildpkg
    Debug(Vec<RunPhase>),
}

/// A phase `em`'s own build pipeline runs — a real PMS-defined ebuild phase
/// function, or one of `em`'s own orchestration steps around it
///
/// `Fetch`/`Clean`/`Qmerge` are not PMS phase functions (`run_one_phase`
/// intercepts them before ever reaching [`portage_repo::EbuildShell::run_phase`]);
/// every other name is looked up via [`portage_metadata::Phase`], the real
/// PMS 9 vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RunPhase {
    Ebuild(portage_metadata::Phase),
    Fetch,
    Clean,
    Qmerge,
}

impl RunPhase {
    const PRETEND: Self = Self::Ebuild(portage_metadata::Phase::PkgPretend);
    const SETUP: Self = Self::Ebuild(portage_metadata::Phase::PkgSetup);
    const UNPACK: Self = Self::Ebuild(portage_metadata::Phase::SrcUnpack);
    const PREPARE: Self = Self::Ebuild(portage_metadata::Phase::SrcPrepare);
    const CONFIGURE: Self = Self::Ebuild(portage_metadata::Phase::SrcConfigure);
    const COMPILE: Self = Self::Ebuild(portage_metadata::Phase::SrcCompile);
    const TEST: Self = Self::Ebuild(portage_metadata::Phase::SrcTest);
    const INSTALL: Self = Self::Ebuild(portage_metadata::Phase::SrcInstall);
}

impl std::fmt::Display for RunPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ebuild(p) => write!(f, "{p}"),
            Self::Fetch => write!(f, "fetch"),
            Self::Clean => write!(f, "clean"),
            Self::Qmerge => write!(f, "qmerge"),
        }
    }
}

impl std::str::FromStr for RunPhase {
    type Err = portage_metadata::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "fetch" => Ok(Self::Fetch),
            "clean" => Ok(Self::Clean),
            "merge" | "qmerge" => Ok(Self::Qmerge),
            _ => s.parse().map(Self::Ebuild),
        }
    }
}

impl PhaseGroup {
    /// The phases this group runs, in order
    fn phases(&self) -> Vec<RunPhase> {
        use RunPhase as P;
        match self {
            Self::Full => vec![
                P::PRETEND,
                P::SETUP,
                P::Fetch,
                P::UNPACK,
                P::PREPARE,
                P::CONFIGURE,
                P::COMPILE,
                P::TEST,
                P::INSTALL,
                P::Qmerge,
            ],
            Self::Compile => vec![
                P::PRETEND,
                P::SETUP,
                P::Fetch,
                P::UNPACK,
                P::PREPARE,
                P::CONFIGURE,
                P::COMPILE,
                P::TEST,
            ],
            Self::Install => vec![P::INSTALL, P::Qmerge],
            Self::BinpkgMerge => vec![P::Qmerge],
            Self::BuildOnly => vec![
                P::PRETEND,
                P::SETUP,
                P::Fetch,
                P::UNPACK,
                P::PREPARE,
                P::CONFIGURE,
                P::COMPILE,
                P::TEST,
                P::INSTALL,
            ],
            // `run_fetch` sources the ebuild when needed and reads SRC_URI/USE
            // from the live shell — no pretend/setup required for download.
            Self::FetchOnly { .. } => vec![P::Fetch],
            Self::Debug(p) => p.clone(),
        }
    }

    /// A real build/merge (not `em ebuild`): gates `src_test` skip and the
    /// merge critical-section lock.
    fn is_merge(&self) -> bool {
        !matches!(self, Self::Debug(_))
    }

    /// Subdirs to wipe before the phase loop (stale-tree clean)
    ///
    /// Full/Compile/BinpkgMerge: everything (starting fresh). Install:
    /// `image` only — `work/` *and* `temp` (`${T}`) hold state the Compile
    /// parent produced that `src_install` may still need. Debug: none.
    ///
    /// Keep `temp` (`${T}`) across Compile→Install: PMS scratch that
    /// `src_prepare` may stage for `src_install` (e.g. gnupg systemd units).
    /// Install's phase list is only `install`/`qmerge`, so wiping temp here
    /// would drop that staged state.
    fn clean_subs(&self) -> Option<&'static [&'static str]> {
        match self {
            Self::Full | Self::Compile | Self::BinpkgMerge | Self::BuildOnly => {
                Some(&["work", "image", "temp", "homedir"])
            }
            Self::Install => Some(&["image", "homedir"]),
            // FetchOnly only needs DISTDIR; no build tree to scrub.
            Self::FetchOnly { .. } | Self::Debug(_) => None,
        }
    }

    /// Dump the live env to `worker-env` after the phase loop (Compile only —
    /// the Install worker sources it to recover BUILD_DIR etc. across the
    /// process boundary).
    fn should_dump_env(&self) -> bool {
        matches!(self, Self::Compile)
    }

    /// Source `worker-env` before the phase loop (Install only)
    fn should_restore_env(&self) -> bool {
        matches!(self, Self::Install)
    }

    /// Build a binpkg (Full + Install, when `-b` is set; BuildOnly
    /// unconditionally, since packaging the image is the entire point).
    fn should_buildpkg(&self) -> bool {
        matches!(self, Self::Full | Self::Install | Self::BuildOnly)
    }

    /// Drop the build tree afterward
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
    /// live for this invocation — `Cli::host_roots()`'s merge root. See
    /// `EbuildShell::build_broot`'s doc comment for the full rationale.
    pub broot: Option<&'a Utf8Path>,
    /// The native toolchain bootstrap (`em toolchain --setup`) is
    /// unconditionally self-contained regardless of `--root`/`--prefix`
    /// topology — it must not source the `--prefix` overlay's
    /// config-overlay `bashrc`.
    ///
    /// That recipe's `CPPFLAGS="-I<prefix>/usr/include ..."` is right for an
    /// ordinary package layered over an already-populated prefix, but for
    /// THIS bootstrap it can shadow a version-matched local header with an
    /// incompatible one from the freshly-installed target libc — the same
    /// class of bug already fixed for `--root` on 2026-07-03 (see
    /// `setup.rs`'s `BASHRC_PREFIX`/`self_contained`).
    pub self_contained_bootstrap: bool,
    /// Directories ahead of the sanitised phase `PATH` ([`portage_repo::phase_path_dirs`]),
    /// resolved by the caller
    ///
    /// Empty for every build but `em setup --local`'s own, which has to reach the host tools a
    /// still-empty prefix borrows.
    pub extra_path: &'a [Utf8PathBuf],
}

/// `LD_LIBRARY_PATH` for a build phase: the prefix's own `ld.so.conf`
/// (`eprefix`), plus the host's own when sharing it for DEPEND (`sysroot`
/// set — a `--prefix` overlay, not `--local`). Without this, build-time
/// tools like `llvm-min-tblgen` died loading shared libraries neither path
/// had been exported for.
pub(crate) fn build_ld_library_path(
    eprefix: Option<&Utf8Path>,
    sysroot: Option<&Utf8Path>,
) -> Option<String> {
    let mut dirs = Vec::new();
    if let Some(eprefix) = eprefix {
        let conf = eprefix.join("etc/ld.so.conf");
        if let Ok(paths) = ldconfig::SearchPaths::from_file(&conf, None) {
            dirs.extend(paths.iter().map(|p| p.to_string()));
        }
    }
    if sysroot.is_some()
        && let Ok(paths) = ldconfig::SearchPaths::from_file(Utf8Path::new("/etc/ld.so.conf"), None)
    {
        dirs.extend(paths.iter().map(|p| p.to_string()));
    }
    (!dirs.is_empty()).then(|| dirs.join(":"))
}

/// The base directory for build work trees: `<prefix>/var/tmp/portage` under
/// a prefix; otherwise the system `/var/tmp/portage` when writable, falling
/// back to the user cache.
///
/// Per-package trees live under [`package_work_dir`], which further keys by
/// merge root so dual-root plan entries (host + target) never share a WORKDIR.
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

/// Stable subdirectory key for a merge root under [`default_work_base`]
///
/// Portage keeps `$PORTAGE_TMPDIR/portage/$CATEGORY/$PF` without ROOT in the
/// path and serializes dual-ROOT same-CPV merges instead. em prefers
/// **per-root builddirs** so host and target copies of the same CPV can run
/// under `--jobs` without sharing a WORKDIR.
///
/// `/` → `host`; other paths → path with `/` replaced by `-` (trimmed).
pub fn work_root_key(merge_root: &Utf8Path) -> String {
    let s = merge_root.as_str().trim_end_matches('/');
    if s.is_empty() || s == "/" {
        return "host".to_string();
    }
    s.trim_start_matches('/').replace('/', "-")
}

/// Per-package work tree: `$work_base/<root-key>/<category>/<pf>`
///
/// See [`work_root_key`]. Callers that only know the outer prefix work base
/// must also pass the entry's merge root so dual-root plans isolate.
pub fn package_work_dir(
    work_base: &Utf8Path,
    merge_root: &Utf8Path,
    category: &str,
    pf: &str,
) -> Utf8PathBuf {
    work_base
        .join(work_root_key(merge_root))
        .join(category)
        .join(pf)
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
    // / --config-root offsets it). See docs/user/root-model.md.
    let base = config_root.unwrap_or_else(|| Utf8Path::new("/"));
    let Ok(profile_path) =
        std::fs::canonicalize(base.join("etc/portage/make.profile").as_std_path())
    else {
        return Ok(false);
    };
    let mut stack = portage_repo::ProfileStack::build(profile_path)
        .context("building profile stack")?
        .with_user_profile(base.join("etc/portage/profile").into_std_path_buf())
        .context("loading the user profile")?;
    // `--prefix`'s own config overlay (`<prefix>/etc/portage/profile`) layers
    // on top of the host's, same as its `package.use`/`bashrc` already do —
    // this is where `em setup --prefix` writes `use.force: prefix-guest`
    // (real Gentoo Prefix's own `features/prefix/rpath/use.force`
    // convention), so an eclass's `!use prefix-guest` checks (toolchain.eclass
    // gcc configure flags, virtual/os-headers RDEPEND) see the host's libc
    // as authoritative instead of assuming a self-hosted Prefix.
    if let Some(overlay) = config_overlay {
        stack = stack
            .with_user_profile(overlay.join("profile").into_std_path_buf())
            .context("loading the prefix overlay profile")?;
    }
    // make.conf(5): file or Flat directory of fragments. Legacy first, then
    // `/etc/portage/make.conf` (later overrides — same order as Portage).
    let conf_owned = portage_repo::expand_make_conf_paths([
        base.join("etc/make.conf"),
        base.join("etc/portage/make.conf"),
    ])
    .context("listing make.conf")?;
    let confs: Vec<portage_repo::ConfSource> = conf_owned
        .iter()
        .map(|p| portage_repo::ConfSource::File(p.as_path()))
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
    let phases: Vec<RunPhase> = phases
        .iter()
        .map(|p| p.parse().map_err(|e| anyhow!("unknown phase {p:?}: {e}")))
        .collect::<Result<_>>()?;
    // Standalone `em ebuild <path> <phase>`: no resolved plan entry exists, so
    // there's no authoritative Cpv to pass — `run_inner` falls back to
    // deriving one from `ebuild_path` (fine here: this debug entry point only
    // ever targets a real on-disk ebuild, never a cross-derived virtual one).
    run_inner(RunInner {
        ebuild_path,
        cpv: None,
        group: &PhaseGroup::Debug(phases),
        work_dir,
        repo_override,
        root,
        use_flags: None,
        distdir: None,
        phase_log: None,
        roots,
        merge_gate: None,
        buildpkg: false,
        binpkg: None,
        force_verify_signature: false,
        activity: None,
    })
    .await
}

/// Inputs for [`build_and_merge`] — one resolved plan entry through the full
/// phase chain into `root`.
pub struct BuildAndMerge<'a> {
    pub ebuild_path: &'a Utf8Path,
    pub cpv: &'a portage_atom::Cpv,
    pub use_flags:
        &'a [portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>],
    pub work_base: &'a Utf8Path,
    pub root: &'a Utf8Path,
    pub distdir: Option<&'a Utf8Path>,
    pub quiet: bool,
    pub roots: RootContext<'a>,
    pub merge_gate: Option<&'a tokio::sync::Mutex<()>>,
    pub buildpkg: bool,
    /// `-B`/`--buildpkgonly`: package the image, never qmerge it
    ///
    /// Checked first and unconditionally single-process — there's no install into the live
    /// ROOT/VDB to delegate to a privilege-wrapped worker, so the compile/install split this
    /// function otherwise does has nothing to scope around (an unprivileged run is wrapped
    /// whole instead, see `needs_whole_process_wrap`).
    ///
    /// `buildpkg` is forced `true` for the `run_inner` call below regardless
    /// of the caller's own `-b`: producing the binpkg is the entire point of
    /// `-B`, not a separate opt-in on top of it.
    pub buildpkgonly: bool,
    /// `-f`/`--fetchonly`: download distfiles only (wins over `-b`/`-B`)
    pub fetchonly: bool,
    /// `-F`/`--fetch-all-uri`: like `fetchonly`, but ignores USE conditionals
    /// when resolving SRC_URI (every entry, not just what's USE-selected).
    pub fetch_all_uri: bool,
    /// When set, emit phase enter/leave on the activity bus
    pub activity: Option<crate::activity::ActivityPkgCtx>,
}

/// Build one resolved plan entry through the full phase chain and merge it
/// into `root`: the per-package effective USE replaces the make.conf USE, the
/// work tree lives under `work_base/<root-key>/<category>/<pf>` (see
/// [`package_work_dir`]), and `distdir` (when set, e.g.
/// `<prefix>/var/cache/distfiles`) overrides the writable distfiles location.
pub async fn build_and_merge(opts: BuildAndMerge<'_>) -> Result<()> {
    let BuildAndMerge {
        ebuild_path,
        cpv,
        use_flags,
        work_base,
        root,
        distdir,
        quiet,
        roots,
        merge_gate,
        buildpkg,
        buildpkgonly,
        fetchonly,
        fetch_all_uri,
        activity,
    } = opts;
    let ebuild = Ebuild::with_cpv(cpv.clone(), ebuild_path);
    let pf = format!("{}-{}", ebuild.name(), ebuild.version());
    let work_dir = package_work_dir(work_base, root, ebuild.category(), &pf);
    let log = work_dir.join("build.log");

    // Fetch-only short-circuit (emerge `EbuildBuild` when opts.fetchonly): no
    // privilege worker, no compile, no qmerge. Checked before `-B` so `-fB`
    // still only fetches. `-F` implies the same short-circuit, just with a
    // different SRC_URI resolution mode inside `run_fetch`.
    if fetchonly || fetch_all_uri {
        return run_inner(RunInner {
            ebuild_path: ebuild_path.as_str(),
            cpv: Some(cpv),
            group: &PhaseGroup::FetchOnly {
                all_uri: fetch_all_uri,
            },
            work_dir: Some(&work_dir),
            repo_override: None,
            root,
            use_flags: Some(use_flags),
            distdir,
            phase_log: Some((log.clone(), quiet)),
            roots,
            merge_gate,
            buildpkg: false,
            binpkg: None,
            force_verify_signature: false,
            activity,
        })
        .await
        .with_context(|| format!("fetch log: {log}"));
    }

    let result = if buildpkgonly {
        run_inner(RunInner {
            ebuild_path: ebuild_path.as_str(),
            cpv: Some(cpv),
            group: &PhaseGroup::BuildOnly,
            work_dir: Some(&work_dir),
            repo_override: None,
            root,
            use_flags: Some(use_flags),
            distdir,
            phase_log: Some((log.clone(), quiet)),
            roots,
            merge_gate,
            buildpkg: true,
            binpkg: None,
            force_verify_signature: false,
            activity,
        })
        .await
        .with_context(|| format!("build log: {log}"))
    } else if let Some(backend) = crate::privilege::install_wrap_backend() {
        // Scoped privilege (Q6): compile runs un-wrapped in this process;
        // install+qmerge(+binpkg) delegates to a wrapped __worker child so the
        // ptrace tax / real root stays off the compile's make/gcc tree.
        // Activity: compile phases emit on the parent bus; install phases
        // continue via LiveFs in the worker (same job_id / live_root).
        run_inner(RunInner {
            ebuild_path: ebuild_path.as_str(),
            cpv: Some(cpv),
            group: &PhaseGroup::Compile,
            work_dir: Some(&work_dir),
            repo_override: None,
            root,
            use_flags: Some(use_flags),
            distdir,
            phase_log: Some((log.clone(), quiet)),
            roots,
            merge_gate: None,
            buildpkg: false,
            binpkg: None,
            force_verify_signature: false,
            activity: activity.clone(),
        })
        .await
        .with_context(|| format!("build log: {log}"))?;

        spawn_install_worker_step(
            backend,
            WorkerStep {
                ebuild_path,
                cpv,
                use_flags,
                work_base,
                root,
                roots,
                quiet,
                distdir,
                buildpkg,
                binpkg: None,
                force_verify_signature: false,
                activity: activity.as_ref(),
                log_label: "build",
                log: &log,
            },
        )
        .await
    } else {
        run_inner(RunInner {
            ebuild_path: ebuild_path.as_str(),
            cpv: Some(cpv),
            group: &PhaseGroup::Full,
            work_dir: Some(&work_dir),
            repo_override: None,
            root,
            use_flags: Some(use_flags),
            distdir,
            phase_log: Some((log.clone(), quiet)),
            roots,
            merge_gate,
            buildpkg,
            binpkg: None,
            force_verify_signature: false,
            activity,
        })
        .await
        .with_context(|| format!("build log: {log}"))
    };

    // Whatever the merge chain left for the `echo` module — this process for a
    // whole-chain run, the `__worker` child for a split one — is held here and
    // replayed once the whole run finishes, as `mod_echo` does.
    crate::elog::take_pending(cpv, &work_dir, root);
    result
}

/// Inputs for [`merge_binpkg`] — reuse a pre-built GPKG without compiling
pub struct MergeBinpkg<'a> {
    pub binpkg_path: &'a Utf8Path,
    pub ebuild_path: &'a Utf8Path,
    pub cpv: &'a portage_atom::Cpv,
    pub use_flags:
        &'a [portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>],
    pub work_base: &'a Utf8Path,
    pub root: &'a Utf8Path,
    pub quiet: bool,
    pub roots: RootContext<'a>,
    pub merge_gate: Option<&'a tokio::sync::Mutex<()>>,
    /// This binpkg's origin (a `binrepos.conf` entry with
    /// `verify-signature = yes`) forces cryptographic signature
    /// verification, independent of `FEATURES=binpkg-request-signature`.
    /// `false` for a local `-k` reuse.
    pub force_verify_signature: bool,
    pub activity: Option<crate::activity::ActivityPkgCtx>,
}

/// Merge a pre-built binary package (`-k`/`--usepkg`): extract the GPKG's image
/// into the work tree, then run only the `qmerge` phase (which sources the
/// ebuild for env/hooks and merges from `work_root/image`). Skips fetch →
/// compile entirely. The caller has already validated the binpkg is reusable
/// (version + USE + slot match) via [`portage_binpkg::BinpkgIndex`].
pub async fn merge_binpkg(opts: MergeBinpkg<'_>) -> Result<()> {
    let MergeBinpkg {
        binpkg_path,
        ebuild_path,
        cpv,
        use_flags,
        work_base,
        root,
        quiet,
        roots,
        force_verify_signature,
        merge_gate,
        activity,
    } = opts;
    let ebuild = Ebuild::with_cpv(cpv.clone(), ebuild_path);
    let pf = format!("{}-{}", ebuild.name(), ebuild.version());
    let work_dir = package_work_dir(work_base, root, ebuild.category(), &pf);
    let log = work_dir.join("build.log");

    let result = if let Some(backend) = crate::privilege::install_wrap_backend() {
        // The qmerge chowns must run under (fake) root. Delegate to the
        // wrapped __worker (BinpkgMerge group, binpkg set).
        spawn_install_worker_step(
            backend,
            WorkerStep {
                ebuild_path,
                cpv,
                use_flags,
                work_base,
                root,
                roots,
                quiet,
                distdir: None,
                buildpkg: false,
                binpkg: Some(binpkg_path),
                force_verify_signature,
                activity: activity.as_ref(),
                log_label: "merge",
                log: &log,
            },
        )
        .await
    } else {
        // The image is extracted inside run_inner (after its clean step) from
        // the binpkg, then the qmerge phase merges from work_root/image.
        run_inner(RunInner {
            ebuild_path: ebuild_path.as_str(),
            cpv: Some(cpv),
            group: &PhaseGroup::BinpkgMerge,
            work_dir: Some(&work_dir),
            repo_override: None,
            root,
            use_flags: Some(use_flags),
            distdir: None,
            phase_log: Some((log.clone(), quiet)),
            roots,
            merge_gate,
            buildpkg: false,
            binpkg: Some(binpkg_path),
            force_verify_signature,
            activity,
        })
        .await
        .with_context(|| format!("merge log: {log}"))
    };

    // A binpkg still runs pkg_preinst/pkg_postinst, so it has elog messages of
    // its own to hand to the end-of-run replay.
    crate::elog::take_pending(cpv, &work_dir, root);
    result
}

/// Inputs for [`run_install_worker`] (the privilege-wrapped `__worker` child)
pub struct InstallWorker<'a> {
    pub ebuild_path: &'a str,
    pub cpv_str: &'a str,
    pub use_flags_str: &'a str,
    pub work_base: &'a str,
    pub root: &'a str,
    pub distdir: Option<&'a str>,
    pub roots: RootContext<'a>,
    pub binpkg: Option<&'a str>,
    /// See `RunInner`'s field of the same name; relayed across the
    /// `__worker` process boundary as a bare flag (`--force-verify-signature`)
    /// rather than the keyring/config itself — the worker re-sources
    /// `make.conf`/profile via the normal `EbuildShell` sweep and re-reads
    /// `FEATURES`/`BINPKG_GPG_VERIFY_GPG_HOME` itself, same as everything
    /// else it re-derives.
    pub force_verify_signature: bool,
    pub buildpkg: bool,
    pub quiet: bool,
    pub activity_job_id: Option<&'a str>,
    pub activity_parent_job_id: Option<&'a str>,
    pub activity_live_root: Option<&'a str>,
    pub activity_side: Option<&'a str>,
    /// Parent-bound Unix socket for phase JSONL re-emit onto the parent bus
    pub activity_reemit_path: Option<&'a str>,
}

/// CLI strings for the install worker's activity identity (owned so they outlive
/// the `WorkerArgs` borrow of local `String`s).
fn worker_activity_cli(
    activity: Option<&crate::activity::ActivityPkgCtx>,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let Some(act) = activity else {
        return (None, None, None, None);
    };
    let Some(live) = act.live_root.as_ref() else {
        return (None, None, None, None);
    };
    (
        Some(act.job_id.to_string()),
        act.parent_job_id.as_deref().map(str::to_string),
        Some(live.to_string()),
        Some(act.merge_root.as_str().to_string()),
    )
}

/// The fields that differ between the two [`spawn_install_worker_step`] call
/// sites (`build_and_merge`'s scoped-install path vs `merge_binpkg`); the rest
/// of the `WorkerArgs` is assembled from the shared context.
struct WorkerStep<'a> {
    ebuild_path: &'a Utf8Path,
    cpv: &'a portage_atom::Cpv,
    use_flags: &'a [portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>],
    work_base: &'a Utf8Path,
    root: &'a Utf8Path,
    roots: RootContext<'a>,
    quiet: bool,
    distdir: Option<&'a Utf8Path>,
    buildpkg: bool,
    binpkg: Option<&'a Utf8Path>,
    force_verify_signature: bool,
    activity: Option<&'a crate::activity::ActivityPkgCtx>,
    /// `build`/`merge` — names the log in the non-zero-exit error only
    log_label: &'a str,
    log: &'a Utf8Path,
}

/// Assemble the `WorkerArgs` and run one privilege-wrapped `__worker` install, bailing on a
/// non-zero exit
///
/// Shared by the two spawn sites so a new `WorkerArgs` field lands in one place, not three.
async fn spawn_install_worker_step(
    backend: crate::privilege::Backend,
    step: WorkerStep<'_>,
) -> Result<()> {
    let use_str = step
        .use_flags
        .iter()
        .map(|f| f.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let cpv_str = step.cpv.to_string();
    let extra_path = step
        .roots
        .extra_path
        .iter()
        .map(|d| d.as_str())
        .collect::<Vec<_>>()
        .join(":");
    let (act_job, act_parent, act_live, act_side) = worker_activity_cli(step.activity);
    let reemit = step.activity.map(|a| a.bus.clone());
    let code = crate::privilege::spawn_install_worker(
        backend,
        &crate::privilege::WorkerArgs {
            ebuild_path: step.ebuild_path.as_str(),
            cpv: &cpv_str,
            use_flags: &use_str,
            work_base: step.work_base.as_str(),
            root: step.root.as_str(),
            distdir: step.distdir.map(|d| d.as_str()),
            config_root: step.roots.config_root.map(|c| c.as_str()),
            sysroot: step.roots.sysroot.map(|s| s.as_str()),
            eprefix: step.roots.eprefix.map(|e| e.as_str()),
            broot: step.roots.broot.map(|b| b.as_str()),
            self_contained_bootstrap: step.roots.self_contained_bootstrap,
            extra_path: &extra_path,
            buildpkg: step.buildpkg,
            quiet: step.quiet,
            binpkg: step.binpkg.map(|b| b.as_str()),
            force_verify_signature: step.force_verify_signature,
            activity_job_id: act_job.as_deref(),
            activity_parent_job_id: act_parent.as_deref(),
            activity_live_root: act_live.as_deref(),
            activity_side: act_side.as_deref(),
            activity_reemit_path: None,
        },
        reemit,
    )
    .await
    .context("starting the install worker")?;
    if code != 0 {
        anyhow::bail!(
            "install worker exited with status {code} ({} log: {})",
            step.log_label,
            step.log
        );
    }
    Ok(())
}

/// Rebuild a package activity handle inside the install worker (LiveFs + optional re-emit)
fn worker_activity_ctx(
    job_id: Option<&str>,
    parent_job_id: Option<&str>,
    live_root: Option<&str>,
    side: Option<&str>,
    reemit_path: Option<&str>,
    cpv: &str,
) -> Option<crate::activity::ActivityPkgCtx> {
    let job_id = job_id.filter(|s| !s.is_empty())?;
    let live_root = live_root.filter(|s| !s.is_empty())?;
    let cpv: Cpv = cpv.parse().ok()?;
    let side = match side.unwrap_or("target") {
        "host" => crate::activity::ActivityMergeRoot::Host,
        _ => crate::activity::ActivityMergeRoot::Target,
    };
    let bus = crate::activity::worker_activity_bus(Utf8Path::new(live_root), reemit_path);
    Some(
        crate::activity::ActivityPkgCtx::new(
            bus,
            job_id.into(),
            parent_job_id.filter(|s| !s.is_empty()).map(Into::into),
            std::sync::Arc::new(cpv),
            side,
        )
        .with_live_root(Utf8PathBuf::from(live_root)),
    )
}

/// The `em __worker` body: run the install group (install+qmerge+binpkg) for one
/// package inside the privilege session the parent spawned us into. The ebuild
/// is re-sourced (portage spawns each phase in its own process); the parent's
/// captured env is restored so cross-phase state (BUILD_DIR, …) survives.
pub async fn run_install_worker(opts: InstallWorker<'_>) -> Result<()> {
    let InstallWorker {
        ebuild_path,
        cpv_str,
        use_flags_str,
        work_base,
        root,
        distdir,
        roots,
        binpkg,
        force_verify_signature,
        buildpkg,
        quiet,
        activity_job_id,
        activity_parent_job_id,
        activity_live_root,
        activity_side,
        activity_reemit_path,
    } = opts;
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
    // `root` is the merge root the parent used when choosing the work tree.
    let work_dir = package_work_dir(
        Utf8Path::new(work_base),
        Utf8Path::new(root),
        ebuild_obj.category(),
        &pf,
    );
    let log = work_dir.join("build.log");

    // LiveFs + optional JSONL re-emit to parent bus; parent owns Session/Pkg*/history.
    let activity = worker_activity_ctx(
        activity_job_id,
        activity_parent_job_id,
        activity_live_root,
        activity_side,
        activity_reemit_path,
        cpv_str,
    );

    let group = if binpkg.is_some() {
        PhaseGroup::BinpkgMerge
    } else {
        PhaseGroup::Install
    };
    let result = run_inner(RunInner {
        ebuild_path,
        cpv: Some(&cpv),
        group: &group,
        work_dir: Some(&work_dir),
        repo_override: None,
        root: Utf8Path::new(root),
        use_flags: Some(&use_flags),
        distdir: distdir.map(Utf8Path::new),
        phase_log: Some((log.clone(), quiet)),
        roots,
        merge_gate: None,
        buildpkg,
        binpkg: binpkg.map(Utf8Path::new),
        force_verify_signature,
        activity,
    })
    .await
    .with_context(|| format!("merge log: {log}"));

    // A slot replacement runs the old package's pkg_prerm/pkg_postrm here, and
    // those queue their `echo` share in this process — the parent only ever
    // collects the *new* package's handoff. Print them before the worker exits
    // or they die with it.
    crate::elog::finalize_echo();
    result
}

/// Resolve a repo's master repositories (depth-first), so eclasses inherited from a master
/// are found
///
/// Master locations come from `repos.conf` by name, falling back to a sibling of
/// `repo_root`. Masters that can't be opened are skipped with a warning rather than
/// aborting the build.
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
            match crate::repo_open::open(location.as_std_path()) {
                Ok(master) => {
                    recurse(&master, &location, conf, out, seen);
                    out.push(master);
                }
                Err(e) => {
                    crate::style::warn_line!(
                        "master repo '{name}' for {repo_root} unavailable: {e}"
                    );
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

/// Core phase-runner inputs (shared by build, binpkg merge, worker, debug)
struct RunInner<'a> {
    ebuild_path: &'a str,
    /// Authoritative Cpv from the plan when present; `None` for standalone
    /// `em ebuild` (derived from the on-disk path — fine for real ebuilds).
    cpv: Option<&'a portage_atom::Cpv>,
    group: &'a PhaseGroup,
    work_dir: Option<&'a Utf8Path>,
    repo_override: Option<&'a str>,
    root: &'a Utf8Path,
    use_flags:
        Option<&'a [portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>]>,
    distdir: Option<&'a Utf8Path>,
    phase_log: Option<(Utf8PathBuf, bool)>,
    roots: RootContext<'a>,
    merge_gate: Option<&'a tokio::sync::Mutex<()>>,
    buildpkg: bool,
    /// Pre-built GPKG to extract after clean (`-k`/`-g`); Install group only
    binpkg: Option<&'a Utf8Path>,
    /// This binpkg's origin (a `binrepos.conf` entry with
    /// `verify-signature = yes`) forces cryptographic signature
    /// verification for `binpkg`, independent of whether
    /// `FEATURES=binpkg-request-signature` is set. `false` for a local
    /// `-k` reuse or when the remote repo didn't request it.
    force_verify_signature: bool,
    activity: Option<crate::activity::ActivityPkgCtx>,
}

async fn run_inner(opts: RunInner<'_>) -> Result<()> {
    let RunInner {
        ebuild_path,
        cpv,
        group,
        work_dir,
        repo_override,
        root,
        use_flags,
        distdir,
        phase_log,
        roots,
        merge_gate,
        buildpkg,
        binpkg,
        force_verify_signature,
        activity,
    } = opts;
    let RootContext {
        config_root,
        sysroot,
        eprefix,
        broot,
        self_contained_bootstrap,
        extra_path,
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

    let repo = crate::repo_open::open(repo_root.as_std_path())
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
            package_work_dir(
                Utf8Path::new("/var/tmp/portage"),
                root,
                ebuild.category(),
                &pf,
            )
        }
    };

    // Portage `EbuildBuildDir`: exclusive use of this package tree for the
    // whole phase chain (also blocks a second concurrent `em` on the same path).
    let _builddir_lock = lock_builddir(&work_root).await;

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
        crate::style::warn_line!(
            "no usable profile at {cr}/etc/portage/make.profile — building without profile defaults"
        );
    }

    // Per-package build environment: `/etc/portage/package.env` maps this package
    // to env files under `/etc/portage/env/`, sourced on top of `make.conf` so
    // FEATURES, *FLAGS, MAKEOPTS, … take effect per package. Sourced before the
    // resolved USE is applied (below) so the plan's USE wins — USE set by an env
    // file is intentionally not reflected here (a resolver-side follow-up).
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
            .map(|c| c.metadata.slot);
        for env_file in portage_repo::env_files_for(&portage_dirs, ebuild.cpv(), slot.as_ref()) {
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

    // Root model (docs/user/root-model.md): PORTAGE_CONFIGROOT = config_root, and
    // SYSROOT/ESYSROOT = the build-against base — the real host `/` for a
    // --prefix overlay or a same-arch --root; SYSROOT = ROOT only for a
    // topology with its own build closure (--local, cross --target).
    //
    // NB: in overlay mode (target ≠ base) a package merged into the target is
    // not yet visible to later builds in the run — that needs a merged sysroot,
    // which is shelved (see docs/user/root-model.md "Overlay support — shelved").
    let ld_library_path = build_ld_library_path(eprefix, sysroot);
    shell.set_build_roots(
        config_root,
        sysroot,
        eprefix,
        broot,
        ld_library_path.as_deref(),
    );
    shell.set_extra_path(extra_path.to_vec());
    shell.set_terminal(crate::style::terminal_config());

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
    if group.is_merge() || matches!(group, PhaseGroup::Debug(p) if p.contains(&RunPhase::Qmerge)) {
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
    // portage's `ebuild` command doesn't auto-clean either.
    //
    // FEATURES (make.conf(5)):
    // - `keepwork` — skip pre-clean (and post-clean) so WORKDIR can be reused
    // - `keeptemp` — keep `${T}` (`temp/`) through cleans that would wipe it
    // - `noclean` — post-merge only (does not disable this pre-clean)
    // `build.log` and the `.em-helpers` shim dir are left: the log is truncated
    // by the phase-log tee, and the shims are idempotent.
    if let (Some(wd), Some(subs)) = (work_dir, group.clean_subs()) {
        for sub in filter_clean_subs(subs, &features, CleanWhen::Pre) {
            let _ = std::fs::remove_dir_all(wd.join(sub));
        }
        // The elog handoff sits at the work dir's top level, out of reach of
        // the clean above (like `build.log`). A run killed between the worker
        // writing it and the parent collecting it would otherwise have its
        // messages replayed against this merge.
        crate::elog::discard_stale_pending(wd);
    }

    // `-k`/`--usepkg`: extract the binpkg image *after* the clean above (which
    // wipes work_dir/image to defeat stale-${D} leakage on re-emerge). The image
    // is the authoritative payload here — there is no src_compile to repopulate
    // it — so it must land between the clean and the qmerge walk.
    if let (Some(wd), Some(bp)) = (work_dir, binpkg) {
        let image_dir = wd.join("image");
        std::fs::create_dir_all(image_dir.as_std_path())
            .with_context(|| format!("creating {image_dir}"))?;

        // GPG verify policy: FEATURES=binpkg-request-signature (or this
        // binpkg's own binrepos.conf verify-signature=yes, relayed as
        // `force_verify_signature`) requires a signature to be present;
        // BINPKG_GPG_VERIFY_GPG_HOME supplies the keyring when configured.
        // Root-aware default (config_root/etc/portage/gnupg) — never the
        // real host path for a non-host --root/--target/--prefix, same
        // class of bug this project already fixed once for PKGDIR.
        let require_signature =
            features.contains("binpkg-request-signature") || force_verify_signature;
        let verify_home = shell
            .get_var("BINPKG_GPG_VERIFY_GPG_HOME")
            .map(Utf8PathBuf::from)
            .unwrap_or_else(|| {
                config_root
                    .unwrap_or(Utf8Path::new("/"))
                    .join("etc/portage/gnupg")
            });
        let keyring = portage_binpkg::gpg::load_keyring_dir(verify_home.as_std_path())
            .with_context(|| format!("loading GPG verify keyring at {verify_home}"))?;
        if require_signature && keyring.is_none() {
            bail!(
                "FEATURES=binpkg-request-signature (or this binpkg's binrepos.conf verify-signature=yes) requires a GPG verify keyring at {verify_home} — run `em maint binpkg gpg-import <keyfile>` first"
            );
        }
        let policy = portage_binpkg::VerifyPolicy {
            require_signature,
            keyring: keyring.as_ref(),
        };
        portage_binpkg::extract_image(bp.as_std_path(), image_dir.as_std_path(), policy)
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

    let fetch_all_uri = matches!(group, PhaseGroup::FetchOnly { all_uri: true });
    let phases = group.phases();
    // The chain's outcome is held rather than propagated, so the elog dispatch
    // below runs whether it succeeded or failed — portage dispatches from a
    // `finally` (`Scheduler.py`'s `_locked_task_cleanup`) for the same reason:
    // the `ewarn`/`eerror` a phase raised on its way to dying is precisely the
    // message elog exists to preserve, and `${T}` is about to be cleaned.
    let chain_result: Result<()> = async {
        for phase in &phases {
            // In the merge chain, src_test only runs under FEATURES=test
            // (an explicit `em ebuild … test` always runs it).
            if merge_mode && *phase == RunPhase::TEST && !features.contains("test") {
                continue;
            }

            // Serialise the merge critical section under `--jobs`: builds (compile
            // phases) run concurrently, but the qmerge — collision check, VDB
            // counter, world/profile updates — must not interleave across packages.
            // The guard is held only for this phase; non-merge phases stay parallel.
            // The in-process gate only covers tasks in this process; parallel
            // `__worker` children serialise on the flock (design Q2 — released by
            // the kernel if a worker dies).
            let _merge_guard = match (merge_gate, *phase) {
                (Some(gate), RunPhase::Qmerge) => Some(gate.lock().await),
                _ => None,
            };
            let _merge_flock = match (merge_mode, work_dir, *phase) {
                (true, Some(wd), RunPhase::Qmerge) => lock_merge_flock(wd).await,
                _ => None,
            };
            let phase_name = phase.to_string();
            let phase_started = activity.as_ref().map(|a| a.phase_enter(&phase_name));
            let phase_result = async {
                run_one_phase(
                    &mut shell,
                    &ebuild,
                    &repo,
                    *phase,
                    &work_root,
                    root,
                    fetch_all_uri,
                )
                .await
            }
            .instrument(tracing::info_span!("phase", phase = %phase))
            .await;
            if let (Some(act), Some(started)) = (activity.as_ref(), phase_started) {
                // Emit leave even on failure so dashboards do not stick mid-phase.
                act.phase_leave(&phase_name, started);
            }
            phase_result?;
            drop(_merge_flock);
            drop(_merge_guard);

            // Portage runs ecompress/estrip at the tail of __dyn_install: the
            // shell still holds the docompress/dostrip lists src_install built
            // up, and everything downstream (preinst, CONTENTS, qmerge) sees
            // the final image.
            if *phase == RunPhase::INSTALL {
                post_process_after_install(&shell, &work_root, &features)?;
            }
        }

        // Compile parent: dump the live variables for the Install worker to
        // source. Lives at work_dir top-level — the Install clean doesn't touch it.
        if group.should_dump_env() {
            let env_data = capture_variables(&mut shell, &work_root)
                .await
                .map_err(|e| anyhow!("capturing environment for worker-env handoff: {e}"))?;
            let env_path = work_root.join("worker-env");
            std::fs::write(env_path.as_std_path(), &env_data)
                .with_context(|| format!("writing {env_path}"))?;
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
                Ok(path) => tracing::info!("Created binary package: {path}"),
                Err(e) if is_buildonly => {
                    return Err(e.context("--buildpkgonly: creating binary package"));
                }
                Err(e) => tracing::warn!("--buildpkg failed for {}: {e:#}", ebuild.cpv()),
            }
        }
        Ok(())
    }
    .await;

    // File the messages the phases left in `${T}/logging` before the build tree
    // — and with it `${T}` — goes away. This side of the split is the one that
    // still has the files, and (being the privilege-wrapped `__worker` for a
    // split build) the one that can write under `<broot>/var/log/portage`; the
    // parent picks up the `echo` module's share from the work dir afterwards.
    //
    // On success only the group that ends the chain dispatches, so a split
    // build files once (from the worker) rather than twice. A *failure* ends
    // the chain wherever it happens, so any group dispatches — otherwise a
    // compile failure in the un-wrapped parent, which never tree-drops, would
    // lose exactly the diagnostics that explain it.
    if let Some(wd) = work_dir
        && (group.should_tree_drop() || chain_result.is_err())
    {
        dispatch_elog(&shell, &ebuild, &work_root, wd, roots);
    }
    chain_result?;

    // Successful merge chain: drop the build tree, keeping build.log.
    // FEATURES: keepwork keeps everything; noclean keeps source+temp
    // (work/temp); keeptemp keeps only temp. image/homedir still go unless
    // keepwork (stale ${D} must not linger). worker-env is droppable once
    // install has finished unless keepwork.
    if group.should_tree_drop()
        && let Some(wd) = work_dir
    {
        let post_subs = ["work", "image", "temp", "homedir"];
        let keep = filter_clean_subs(&post_subs, &features, CleanWhen::Post);
        for sub in keep {
            let _ = std::fs::remove_dir_all(wd.join(sub));
        }
        if !features.contains("keepwork") {
            let _ = std::fs::remove_file(wd.join("worker-env").as_std_path());
        }
    }

    Ok(())
}

/// When a build-tree clean runs relative to the phase loop
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanWhen {
    /// Before phases (stale-tree scrub)
    Pre,
    /// After a successful merge chain
    Post,
}

/// Apply `FEATURES=keepwork|keeptemp|noclean` to a clean-subdir list
///
/// Portage make.conf(5) / `__dyn_clean` shape:
/// - **keepwork** — skip all cleaning (pre and post)
/// - **keeptemp** — never delete `temp/` (`${T}`)
/// - **noclean** — after merge only: keep source and temporary files
///   (`work/` + `temp/`); still drop `image/` / `homedir`
fn filter_clean_subs(
    subs: &[&'static str],
    features: &std::collections::HashSet<String>,
    when: CleanWhen,
) -> Vec<&'static str> {
    if features.contains("keepwork") {
        return Vec::new();
    }
    let mut out: Vec<&'static str> = subs.to_vec();
    if features.contains("keeptemp") {
        out.retain(|s| *s != "temp");
    }
    if when == CleanWhen::Post && features.contains("noclean") {
        // "Do not delete the source and temporary files after the merge"
        out.retain(|s| *s != "work" && *s != "temp");
    }
    out
}

/// Exclusive flock on `<work_base>/.merge.lock`, held around the merge
/// critical section so parallel `__worker` processes — and concurrent em
/// instances sharing the tree — cannot interleave qmerge.
///
/// `work_dir` is [`package_work_dir`] (`$work_base/<root-key>/<cat>/<pf>`), so
/// the work base is three parents up. Blocking acquire runs off the async
/// executor; released on drop (or by the kernel on process exit).
async fn lock_merge_flock(work_dir: &Utf8Path) -> Option<std::fs::File> {
    // work_base / root_key / category / pf
    let base = work_dir.parent()?.parent()?.parent()?;
    acquire_flock(base.join(".merge.lock").into_std_path_buf(), "merge lock").await
}

/// How long to wait on a contended lock before saying so.
///
/// Contention is normal and brief: under `--jobs N` every worker serialises on
/// `.merge.lock` for its own qmerge. Only a wait this long means something is
/// actually stuck — a suspended `em`, or one wedged mid-merge — which is
/// otherwise indistinguishable from a hang, since the acquire is silent.
const LOCK_NOTICE_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// Blocking exclusive flock on `path`, announcing the wait if it runs long
///
/// Released on drop, or by the kernel if the process dies — note that a
/// *suspended* process keeps it, which is the case the notice exists to name.
async fn acquire_flock(path: std::path::PathBuf, label: &str) -> Option<std::fs::File> {
    let notice_path = path.clone();
    let acquire = tokio::task::spawn_blocking(move || {
        // append: never truncate — other processes may hold the lock fd.
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive).ok()?;
        Some(f)
    });
    tokio::pin!(acquire);
    match tokio::time::timeout(LOCK_NOTICE_AFTER, &mut acquire).await {
        Ok(joined) => joined.ok().flatten(),
        Err(_) => {
            match flock_holder_pid(&notice_path) {
                Some(pid) => crate::style::warn_line!(
                    "waiting for the {label} held by pid {pid} ({})",
                    notice_path.display()
                ),
                None => {
                    crate::style::warn_line!("waiting for the {label} ({})", notice_path.display())
                }
            }
            (&mut acquire).await.ok().flatten()
        }
    }
}

/// Best-effort pid holding an flock on `path`, from `/proc/locks`
///
/// `flock` locks are invisible to `fcntl(F_GETLK)`, so the holder can only be
/// found by matching the file against the kernel's own table. `None` on any
/// platform without `/proc/locks`, or for a lock the table attributes to no
/// single process (an OFD lock) — the caller then names just the file.
#[cfg(target_os = "linux")]
fn flock_holder_pid(path: &std::path::Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;

    let meta = std::fs::metadata(path).ok()?;
    let (dev, ino) = (meta.dev(), meta.ino());
    let flocks: Vec<_> = procfs::locks()
        .ok()?
        .into_iter()
        .filter(|l| l.lock_type == procfs::LockType::FLock && l.inode == ino)
        .collect();
    // Inode first, device only to disambiguate. `/proc/locks` reports the
    // *superblock's* `s_dev`, which is not `stat`'s `st_dev` on a filesystem
    // that hands each subvolume its own anonymous device — btrfs does, so on a
    // btrfs root the two never agree and requiring equality finds nothing.
    // An inode number is only unique within a filesystem, so fall back to the
    // device to pick between collisions when there is more than one candidate.
    let hit = match flocks.len() {
        0 => return None,
        1 => flocks.into_iter().next(),
        _ => flocks
            .into_iter()
            .find(|l| l.devmaj == rustix::fs::major(dev) && l.devmin == rustix::fs::minor(dev)),
    };
    hit
        // `pid` is `None` for an OFD lock, which genuinely has no single owner.
        .and_then(|l| l.pid)
        .and_then(|pid| u32::try_from(pid).ok())
}

/// See the Linux implementation — no `/proc/locks` to consult here.
#[cfg(not(target_os = "linux"))]
fn flock_holder_pid(_path: &std::path::Path) -> Option<u32> {
    None
}

/// Exclusive flock on the package work directory itself (Portage
/// `EbuildBuildDir`), held for the whole phase chain so two concurrent
/// merges never share a WORKDIR even if scheduling fails to serialize them.
async fn lock_builddir(work_dir: &Utf8Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(work_dir.as_std_path()).ok()?;
    acquire_flock(
        work_dir.join(".builddir.lock").into_std_path_buf(),
        "build directory lock",
    )
    .await
}

/// Build the ecompress/estrip configuration from the post-`src_install`
/// shell state (docompress/dostrip accumulators, FEATURES, RESTRICT,
/// PORTAGE_COMPRESS) and run the image post-processing pass.
///
/// The image subtree that gets post-processed and merged: the shell's `ED`
/// (`image/${EPREFIX}`, set by `init_build_env`), falling back to
/// `work_root/image` when `ED` is unset or empty. With `EPREFIX=""` this is
/// the plain image dir, so host / `--prefix` builds are unchanged.
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

/// `-B`/`--buildpkgonly`: package the image without ever touching the live ROOT/VDB
///
/// Matches real portage's own model: it never calls `merge()` for `-B` either, packaging
/// straight from `${D}` instead.
///
/// Computes CONTENTS/metadata the exact same way a normal merge would —
/// `walk_image` + `Vdb::register` — just pointed at scratch locations under
/// `work_root/temp` rather than the real root and VDB, which are never
/// written to at any point.
async fn build_binpkg_standalone(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    work_root: &Utf8Path,
    root: &Utf8Path,
) -> Result<Utf8PathBuf> {
    shell.apply_iuse_effective();
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
    let WalkResult { contents, size, .. } = walk_image(
        &image_dir,
        &work_root.join("image"),
        &scratch_dest,
        &cp,
        rewrite_d_symlinks(&env),
    )?;

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
        crate::style::warn_line!("could not copy ebuild into package metadata: {e}");
    }
    if let Ok(ref data) = env_dump
        && let Err(e) = write_environment_bz2(&installed, data)
    {
        crate::style::warn_line!("could not write environment.bz2: {e}");
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
    // `ED` is `image/${EPREFIX}` under `--prefix`/`--local`. Packages that
    // install nothing (every `virtual/*`, many `*-toolchain-symlinks`) never
    // create that nested path — `walk_image` treats a missing dir as empty and
    // merges fine, but `tar -C ED` fails with `Cannot open: No such file or
    // directory`. Live 2026-08-07: systemic `--buildpkg` failure under
    // `--prefix --target -b`. Create an empty ED so the GPKG image member is
    // a valid empty tree (same as a no-op install).
    if !image_dir.exists() {
        std::fs::create_dir_all(image_dir.as_std_path())
            .with_context(|| format!("creating empty image dir {image_dir} for --buildpkg"))?;
    }
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
    let build_id = crate::binpkg::next_build_id(&pkgdir, cat, &pf);
    let out = pkgdir.join(cat).join(format!("{pf}-{build_id}.gpkg.tar"));

    // FEATURES=binpkg-signing: sign the Manifest (clearsign) + a detached
    // .sig per metadata/image member, matching real portage's own gpkg
    // signing scheme (see `portage_binpkg::gpg`'s module doc for the
    // deliberate secret-key-as-file-path simplification vs real gpg-agent).
    let features: std::collections::HashSet<String> = shell
        .get_var("FEATURES")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let signing_key = if features.contains("binpkg-signing") {
        Some(resolve_binpkg_signing_key(shell, root)?)
    } else {
        None
    };
    portage_binpkg::write_gpkg(
        &portage_binpkg::GpkgInput {
            image_dir: image_dir.as_std_path(),
            metadata_dir: metadata_dir.as_std_path(),
            basename: &pf,
            signing: signing_key.as_ref(),
        },
        out.as_std_path(),
    )
    .with_context(|| format!("writing binary package {out}"))?;
    // Keep `Packages` coherent for `-k`/`-g` without a separate
    // `em maint binhost` (quickpkg already reindexes; normal `-b`/`-B` did not).
    let chost = shell.get_var("CHOST").unwrap_or_default();
    if let Err(e) = portage_binpkg::index_pkgdir(&pkgdir, &chost) {
        crate::style::warn_line!("could not refresh Packages index after {out}: {e:#}");
    }
    Ok(out)
}

/// Resolve `BINPKG_GPG_SIGNING_KEY` (+ `_GPG_HOME`/`_DIGEST`/passphrase
/// vars) into a loaded [`portage_binpkg::gpg::SigningKey`] for
/// `FEATURES=binpkg-signing`. **Redefined** here vs real portage: a path to
/// an armored secret-key file, not a gpg keyring key-ID — this project has
/// no gpg-agent/pinentry to resolve a keyring ID against (see
/// `portage_binpkg::gpg`'s module doc).
///
/// A relative path resolves against `BINPKG_GPG_SIGNING_GPG_HOME`,
/// defaulting to `<root>/etc/portage/gnupg`.
fn resolve_binpkg_signing_key(
    shell: &portage_repo::EbuildShell,
    root: &Utf8Path,
) -> Result<portage_binpkg::gpg::SigningKey> {
    let digest = shell
        .get_var("BINPKG_GPG_SIGNING_DIGEST")
        .and_then(|s| s.parse().ok())
        .unwrap_or(portage_binpkg::gpg::HashAlgorithm::Sha512);
    let key_var = shell
        .get_var("BINPKG_GPG_SIGNING_KEY")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "FEATURES=binpkg-signing requires BINPKG_GPG_SIGNING_KEY (path to an armored OpenPGP secret key)"
            )
        })?;
    let mut key_path = Utf8PathBuf::from(key_var.trim());
    if key_path.is_relative() {
        let home = shell
            .get_var("BINPKG_GPG_SIGNING_GPG_HOME")
            .map(Utf8PathBuf::from)
            .unwrap_or_else(|| root.join("etc/portage/gnupg"));
        key_path = home.join(key_path);
    }
    // No pinentry/gpg-agent here — the passphrase (if any) comes from the
    // environment. `_PASSPHRASE_FILE` (an em-only addition, documented as
    // such) wins over the bare env var: it avoids leaving the passphrase
    // readable via `/proc/<pid>/environ`.
    let passphrase = std::env::var("BINPKG_GPG_SIGNING_KEY_PASSPHRASE_FILE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|f| {
            std::fs::read_to_string(&f)
                .with_context(|| format!("reading {f}"))
                .map(|s| s.trim_end().to_string())
        })
        .transpose()?
        .or_else(|| std::env::var("BINPKG_GPG_SIGNING_KEY_PASSPHRASE").ok())
        .unwrap_or_default();
    portage_binpkg::gpg::SigningKey::load(key_path.as_std_path(), &passphrase, digest)
        .with_context(|| format!("loading GPG signing key {key_path}"))
}

/// The next free GPKG build-id for `<cat>/<pf>` in `pkgdir` (portage numbers
/// rebuilds `<pf>-1`, `<pf>-2`, …); 1 when none exist.
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
        compress_exclude.push(Utf8Path::new("/usr/share/doc").join(pf).join("html"));
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
        // debug, not info: real portage doesn't print an interactive
        // ecompress/estrip summary either — this is developer detail
        // (`-vv`/`RUST_LOG`), not something that belongs mixed into the
        // default `>>>`-style merge output.
        tracing::debug!(
            "post-install: {} file(s) compressed, {} symlink(s) retargeted, {} object(s) stripped",
            stats.compressed,
            stats.relinked,
            stats.stripped
        );
    }
    Ok(())
}

async fn run_one_phase(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    repo: &Repository,
    phase: RunPhase,
    work_root: &Utf8Path,
    root: &Utf8Path,
    fetch_all_uri: bool,
) -> Result<()> {
    match phase {
        RunPhase::Fetch => run_fetch(shell, ebuild, repo, work_root, fetch_all_uri).await,
        RunPhase::Clean => run_clean(work_root),
        RunPhase::Qmerge => run_merge(shell, ebuild, work_root, root).await,
        RunPhase::Ebuild(p) => shell
            .run_phase(
                ebuild,
                p.as_str(),
                work_root.as_std_path(),
                root.as_std_path(),
            )
            .await
            .with_context(|| format!("phase {phase} failed")),
    }
}

/// Append one plain-text line to a phase log, matching `run_phase`'s own
/// create-parent-dirs-then-append pattern (`portage-repo`'s
/// `EbuildShell::run_phase`) — for native (non-subshell) status lines like
/// `run_fetch`'s that never go through that pty tee.
fn append_log_line(path: &Utf8Path, line: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.as_std_path())
        .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()));
}

async fn run_fetch(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    repo: &Repository,
    work_root: &Utf8Path,
    // `-F`/`--fetch-all-uri`: resolve every SRC_URI entry regardless of USE.
    fetch_all_uri: bool,
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
        tracing::info!("fetch: nothing to fetch (SRC_URI is empty)");
        return Ok(());
    }

    let entries = SrcUriEntry::parse(&src_uri_str).context("parsing SRC_URI")?;

    let use_flags: HashSet<String> = shell
        .get_var("USE")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();

    let gentoo_mirrors = gentoo_mirrors_list(shell);
    let resolver = DistfileResolver::from_repo(repo, gentoo_mirrors).context("loading mirrors")?;
    let restrict_gate = RestrictGate::from_restrict(
        &RestrictExpr::parse(&shell.get_var("RESTRICT").unwrap_or_default()).unwrap_or_default(),
    );
    let distfiles = if fetch_all_uri {
        resolver.resolve_all_with(&entries, restrict_gate)
    } else {
        resolver.resolve_with(&entries, &use_flags, restrict_gate)
    };

    if distfiles.is_empty() {
        tracing::info!("fetch: nothing to fetch");
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

    let (fetch_cmd, resume_cmd) = read_fetch_commands(shell);
    let config = FetchConfig::from_make_conf(fetch_cmd, resume_cmd);
    let ro_distdirs: Vec<Utf8PathBuf> = shell
        .get_var("PORTAGE_RO_DISTDIRS")
        .unwrap_or_default()
        .split_whitespace()
        .map(Utf8PathBuf::from)
        .collect();
    let fetcher = Fetcher::new(distdir.clone(), config)
        .with_ro_distdirs(ro_distdirs)
        .with_restrict(restrict_gate);

    std::fs::create_dir_all(distdir.as_std_path())
        .with_context(|| format!("creating distdir {distdir}"))?;

    let results = fetcher.fetch_all(&distfiles, &manifest).await;

    // A SRC_URI naming the same file more than once currently produces one
    // `Distfile`/result per URI, not per filename (a separate, pre-existing
    // bug in `DistfileResolver::resolve`/`resolve_all`, not fixed here — see
    // `resolve_uri_map`'s own doc comment). Render each filename's outcome
    // once rather than once per underlying URI.
    //
    // Real portage's own fetch check reports each distfile via `ebegin`/
    // `eend` — a colored `" * "` line ending in a right-aligned `"[ ok ]"`/
    // `"[ !! ]"` bracket, not a log line. `crate::style::estatus_line`
    // renders that same shape in one shot instead of real portage's
    // separate begin-now/finish-later calls (see its own doc comment).
    //
    // Fetch runs outside `run_phase`'s pty tee, so it must honour
    // `phase_output_quiet` itself: under `--jobs N` (`N > 1`) these lines
    // would otherwise print straight to the console with no coordination
    // against the persistent `Jobs: …` status line, garbling both.
    let width = crate::style::term_width();
    let ansi = crate::diag::stderr_wants_color();
    let quiet = shell.phase_output_quiet();
    let log_path = shell.phase_log_path().map(Utf8Path::to_owned);
    let report_fetch_line = |msg: &str, ok: bool| {
        if quiet {
            if let Some(log) = &log_path {
                append_log_line(log, &crate::style::estatus_line(msg, ok, width, false));
            }
        } else {
            eprintln!("{}", crate::style::estatus_line(msg, ok, width, ansi));
        }
    };
    let mut seen = std::collections::HashSet::new();
    let mut any_failed = false;
    let mut any_restricted = false;
    for (df, result) in results {
        if !seen.insert(df.filename.clone()) {
            continue;
        }
        let msg = format!("fetch: {}", df.filename);
        match result {
            Ok(FetchStatus::AlreadyPresent | FetchStatus::Downloaded) => {
                report_fetch_line(&msg, true);
            }
            Ok(FetchStatus::FetchRestricted) => {
                report_fetch_line(&msg, false);
                crate::style::error_line!("{} is fetch-restricted (RESTRICT=fetch)", df.filename);
                any_restricted = true;
            }
            Err(e) => {
                report_fetch_line(&msg, false);
                crate::style::error_line!("{} failed: {e}", df.filename);
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

    // Already sourced in an earlier phase of this shell — do not re-source
    // (can drop eclass IUSE contributions; matches `run_fetch`).
    if !shell.is_phase_sourced(ebuild) {
        shell
            .source_ebuild(ebuild)
            .await
            .context("sourcing ebuild")?;
    }
    shell.apply_iuse_effective();
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
    } = walk_image(
        &image_dir,
        &work_root.join("image"),
        root,
        &cp,
        rewrite_d_symlinks(&env),
    )?;

    let exclude_cpv = old_pkg.as_ref().map(|p| p.cpv().clone());
    let collisions = vdb
        .find_collisions(&contents, exclude_cpv.as_ref())
        .context("collision check failed")?;
    if !collisions.is_empty() {
        for c in &collisions {
            crate::style::warn_line!("collision: {} is already owned by {}", c.path, c.owner);
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
        // The old occupant's pkg_prerm/pkg_postrm run on this same shell, from
        // a different ebuild path — if it shares an eclass with `ebuild` (the
        // package actually being installed), `inherit`'s dedup list *and* the
        // eclass's own include guard would otherwise treat it as
        // already-sourced on the way back to pkg_postinst, silently dropping
        // that eclass's IUSE/RDEPEND/etc. contribution (found live: awk-4's
        // reinstall losing app-alternatives.eclass's `mawk` IUSE this way).
        let session = shell.save_session();
        unmerge_slot_occupant(UnmergeSlotOccupant {
            shell,
            old_pkg: old,
            work_root,
            root,
            vdb: &vdb,
            new_contents: &contents,
            new_version: &ebuild.cpv().version,
            graph: &graph,
            registry: &mut registry,
        })
        .await?;
        shell.restore_session(session);
        registry.store();
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

    // A newly registered package may own paths still listed in the
    // preserved-libs registry from a prior unmerge — reclaim those keys.
    {
        let mut registry = preserve_libs::PreservedLibsRegistry::load(root);
        registry.reclaim(&vdb, root);
        registry.store();
    }

    // Copy the ebuild into the VDB entry as `<PF>.ebuild`, as portage does.
    let pf = format!("{}-{}", ebuild.name(), ebuild.version());
    let ebuild_dest = installed.path().join(format!("{pf}.ebuild"));
    if let Err(e) = std::fs::copy(ebuild.path(), ebuild_dest.as_std_path()) {
        crate::style::warn_line!("could not copy ebuild into VDB: {e}");
    }

    if let Ok(ref data) = env_dump
        && let Err(e) = write_environment_bz2(&installed, data)
    {
        crate::style::warn_line!("could not write environment.bz2: {e}");
    }

    // debug, not info: this is internal VDB bookkeeping (the counter), and
    // at info level it was rendered with the enclosing `phase{phase="qmerge"}:`
    // span label leaking straight into default output — real portage has no
    // equivalent line at all in normal mode.
    tracing::debug!(
        "merge: {}/{}-{} registered (counter={counter})",
        ebuild.category(),
        ebuild.name(),
        ebuild.version()
    );

    if !protected.is_empty() {
        println!();
        crate::style::einfo_line!(
            "{} protected config file(s) were installed with a ._cfg name.",
            protected.len()
        );
        crate::style::einfo_line!("Run `em dispatch` (dispatch-conf) or `em etc` to merge them:");
        for p in &protected {
            crate::style::einfo_line!("  {p}");
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
/// Shared inputs for removing an installed package (in-place replace or `-C`).
struct UnmergePackage<'a> {
    shell: &'a mut portage_repo::EbuildShell,
    old_pkg: &'a InstalledPackage,
    old_work_root: &'a Utf8Path,
    root: &'a Utf8Path,
    vdb: &'a Vdb,
    new_contents: &'a [ContentsEntry],
    graph: &'a preserve_libs::LinkGraph,
    registry: &'a mut preserve_libs::PreservedLibsRegistry,
}

/// [`unmerge_standalone`] (the standalone `-C`/`--unmerge` command — no
/// replacement, so no `REPLACED_BY_VERSION`).
async fn unmerge_package(u: UnmergePackage<'_>) -> Result<()> {
    let UnmergePackage {
        shell,
        old_pkg,
        old_work_root,
        root,
        vdb,
        new_contents,
        graph,
        registry,
    } = u;
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
            None
        };

    // Stage environment.bz2 *before* unregister deletes the VDB directory, so
    // pkg_postrm can still run when the ebuild copy is missing.
    let staged_env = if old_ebuild.is_none() {
        stage_env_bz2(old_pkg, old_work_root)
    } else {
        None
    };
    if old_ebuild.is_none() && staged_env.is_none() {
        crate::style::warn_line!(
            "old ebuild not found at {old_ebuild_src} and no environment.bz2 — skipping pkg_prerm/pkg_postrm"
        );
    }

    let old_sourced = match &old_ebuild {
        Some(e) => {
            shell
                .run_phase(e, "prerm", old_work_root.as_std_path(), root.as_std_path())
                .await
                .context("pkg_prerm failed")?;
            true
        }
        None => match &staged_env {
            Some(env_file) => {
                try_run_phase_from_env_file(shell, "prerm", old_work_root, root, env_file).await
            }
            None => false,
        },
    };

    let old_contents = old_pkg.contents().context("reading old CONTENTS")?;

    // preserve-libs (portage's FEATURES=preserve-libs): never physically
    // delete a shared library some other still-installed object's
    // NEEDED.ELF.2 genuinely requires and that nothing else provides. See
    // `preserve_libs` module doc. `graph`/`registry` are built/loaded once
    // per batch by the caller, not per package here.
    let to_preserve = preserve_libs::find_libs_to_preserve(graph, old_pkg, &old_contents);
    // Slot replace: files the incoming package already owns (or will leave
    // on disk via remove_old_unique_files) must not enter the preserved-libs
    // registry — they are not orphaned.
    let new_paths: HashSet<&Utf8PathBuf> = new_contents.iter().map(|e| &e.path).collect();
    let to_preserve: Vec<_> = to_preserve
        .into_iter()
        .filter_map(|mut e| {
            if new_paths.contains(&e.path) {
                return None;
            }
            if e.soname_symlink
                .as_ref()
                .is_some_and(|s| new_paths.contains(s))
            {
                e.soname_symlink = None;
            }
            Some(e)
        })
        .collect();
    let preserve_paths: HashSet<Utf8PathBuf> = to_preserve
        .iter()
        .flat_map(|e| std::iter::once(e.path.clone()).chain(e.soname_symlink.clone()))
        .collect();
    if !to_preserve.is_empty() {
        preserve_libs::report_preserved(old_pkg.cpv(), &to_preserve, vdb);
    }
    registry.register(
        old_pkg.cpv(),
        &old_pkg.slot_raw().unwrap_or_default(),
        old_pkg.counter().ok().flatten().unwrap_or(0),
        preserve_paths.iter().cloned().collect(),
    );

    remove_old_unique_files(&old_contents, new_contents, &preserve_paths, root);

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
                if let Some(env_file) = &staged_env {
                    let _ =
                        try_run_phase_from_env_file(shell, "postrm", old_work_root, root, env_file)
                            .await;
                }
            }
        }
    }

    // Both removal phases have run; file what they said before the caller
    // deletes the scratch tree they said it in.
    dispatch_unmerge_elog(shell, old_pkg.cpv(), old_work_root, root);

    Ok(())
}

/// `graph`/`registry`: built/loaded once by the caller (a single in-place
/// replace only ever removes this one old occupant, so "once per batch"
/// and "once per call" coincide here — no batching concern like `-C`'s
/// multi-atom case).
struct UnmergeSlotOccupant<'a> {
    shell: &'a mut portage_repo::EbuildShell,
    old_pkg: &'a InstalledPackage,
    work_root: &'a Utf8Path,
    root: &'a Utf8Path,
    vdb: &'a Vdb,
    new_contents: &'a [ContentsEntry],
    new_version: &'a portage_atom::Version,
    graph: &'a preserve_libs::LinkGraph,
    registry: &'a mut preserve_libs::PreservedLibsRegistry,
}

async fn unmerge_slot_occupant(u: UnmergeSlotOccupant<'_>) -> Result<()> {
    let UnmergeSlotOccupant {
        shell,
        old_pkg,
        work_root,
        root,
        vdb,
        new_contents,
        new_version,
        graph,
        registry,
    } = u;
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

    unmerge_package(UnmergePackage {
        shell,
        old_pkg,
        old_work_root: &old_work_root,
        root,
        vdb,
        new_contents,
        graph,
        registry,
    })
    .await?;
    let _ = std::fs::remove_dir_all(old_work_root.as_std_path());
    Ok(())
}

/// Standalone removal for `-C`/`--unmerge` (`emerge.rs::unmerge_atoms`): no
/// replacement and no active install to derive a sibling scratch dir from,
/// so the scratch tree is `<work_base>/<root-key>/<category>/<pf>.unmerge`.
/// Reuses [`unmerge_package`] with an empty `new_contents`, so every file the
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
    let old_work_root = package_work_dir(
        work_base,
        root,
        old_pkg.category(),
        &format!("{}.unmerge", old_pkg.pf()),
    );

    unmerge_package(UnmergePackage {
        shell,
        old_pkg,
        old_work_root: &old_work_root,
        root,
        vdb,
        new_contents: &[],
        graph,
        registry,
    })
    .await?;
    let _ = std::fs::remove_dir_all(old_work_root.as_std_path());
    Ok(())
}

/// Decompress `environment.bz2` from the VDB into `work_root/temp/` so
/// prerm/postrm can both use it after the VDB directory is removed.
fn stage_env_bz2(pkg: &InstalledPackage, work_root: &Utf8Path) -> Option<Utf8PathBuf> {
    let env_bz2 = pkg.path().join("environment.bz2");
    if !env_bz2.exists() {
        return None;
    }
    let temp_dir = work_root.join("temp");
    if let Err(e) = std::fs::create_dir_all(temp_dir.as_std_path()) {
        crate::style::warn_line!("could not create {temp_dir}: {e}");
        return None;
    }
    let temp_env = temp_dir.join("environment.old");
    let compressed = match std::fs::read(env_bz2.as_std_path()) {
        Ok(d) => d,
        Err(e) => {
            crate::style::warn_line!("could not read environment.bz2: {e}");
            return None;
        }
    };
    let decompressed = match decompress_bzip2(&compressed) {
        Ok(d) => d,
        Err(e) => {
            crate::style::warn_line!("could not decompress environment.bz2: {e}");
            return None;
        }
    };
    if let Err(e) = std::fs::write(temp_env.as_std_path(), &decompressed) {
        crate::style::warn_line!("could not write temp environment: {e}");
        return None;
    }
    Some(temp_env)
}

/// Run `pkg_prerm` / `pkg_postrm` from a previously staged environment dump
async fn try_run_phase_from_env_file(
    shell: &mut portage_repo::EbuildShell,
    phase: &str,
    _work_root: &Utf8Path,
    root: &Utf8Path,
    env_file: &Utf8Path,
) -> bool {
    let source_cmd = format!(". '{}'", env_file.as_str().replace('\'', "'\\''"));
    if shell.run_string(&source_cmd).await.is_err() {
        crate::style::warn_line!("could not source saved environment");
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
        crate::style::warn_line!("{func} from environment.bz2 failed: {e}");
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
) {
    let new_paths: HashSet<&Utf8PathBuf> = new_contents.iter().map(|e| &e.path).collect();

    for entry in old_contents.iter().rev() {
        if new_paths.contains(&entry.path) || preserve.contains(&entry.path) {
            continue;
        }
        let dest = match safe_dest_under(root, &entry.path) {
            Ok(d) => d,
            Err(e) => {
                crate::style::warn_line!("skipping unsafe CONTENTS path: {e:#}");
                continue;
            }
        };

        match entry.kind {
            ContentsKind::Obj | ContentsKind::Sym => {
                if (dest.exists() || std::fs::symlink_metadata(dest.as_std_path()).is_ok())
                    && let Err(e) = std::fs::remove_file(dest.as_std_path())
                {
                    crate::style::warn_line!("could not remove {dest}: {e}");
                }
            }
            ContentsKind::Dir => {
                let _ = std::fs::remove_dir(dest.as_std_path());
            }
            _ => {}
        }
    }
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

/// CONFIG_PROTECT / CONFIG_PROTECT_MASK resolution (portage's `ConfigProtect`)
///
/// A path is protected when the longest matching `CONFIG_PROTECT` prefix is
/// longer than the longest matching `CONFIG_PROTECT_MASK` prefix. Protected
/// files that already exist and differ are diverted to `._cfgNNNN_<name>`
/// for `dispatch-conf`/`etc-update` instead of being overwritten.
///
/// Deliberately real-Portage semantics, not the PMS 13.3.3 letter (which
/// gives an ancestor `CONFIG_PROTECT_MASK` unconditional priority regardless
/// of how deep the matching `CONFIG_PROTECT` entry is). Matching what real
/// `emerge` actually does on a system is the intended behavior here.
pub(crate) struct ConfigProtect {
    protect: Vec<String>,
    mask: Vec<String>,
}

impl ConfigProtect {
    /// Read the lists from the configured shell
    ///
    /// `/etc` is always protected (portage's make.globals guarantees it).
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

    /// Read the lists from the root-aware `make.conf` rather than a build
    /// shell, for callers that only inspect the filesystem (`em etc`) and
    /// have no reason to source an ebuild environment.
    ///
    /// `/etc` is always protected, the same guarantee `from_shell` relies on
    /// portage's `make.globals` for. Paths are stored root-relative-ready
    /// (leading `/`, no trailing one), as `is_protected` compares them
    /// against the same shape.
    pub(crate) async fn from_roots(roots: &portage_resolve::Roots) -> Self {
        let read = |v: Option<String>| -> Vec<String> {
            v.unwrap_or_default()
                .split_whitespace()
                .map(|s| s.trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let mut protect =
            read(crate::binpkg::read_make_conf_var_for_roots(roots, "CONFIG_PROTECT").await);
        if !protect.iter().any(|p| p == "/etc") {
            protect.push("/etc".to_string());
        }
        Self {
            protect,
            mask: read(
                crate::binpkg::read_make_conf_var_for_roots(roots, "CONFIG_PROTECT_MASK").await,
            ),
        }
    }

    /// The protected directories themselves, for a caller that has to walk
    /// them rather than test a single path.
    pub(crate) fn protected_dirs(&self) -> &[String] {
        &self.protect
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

    pub(crate) fn is_protected(&self, obj: &Utf8Path) -> bool {
        let obj = obj.as_str();
        Self::longest_match(&self.protect, obj) > Self::longest_match(&self.mask, obj)
    }

    /// Explicit lists, for a test that needs a specific protect/mask pair
    /// without a shell or a `make.conf`.
    #[cfg(test)]
    pub(crate) fn for_test(protect: &[&str], mask: &[&str]) -> Self {
        Self {
            protect: protect.iter().map(|s| (*s).to_string()).collect(),
            mask: mask.iter().map(|s| (*s).to_string()).collect(),
        }
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

/// Set a symlink's own atime/mtime
///
/// `std::fs` always follows symlinks, so we go through `utimensat(AT_SYMLINK_NOFOLLOW)`.
/// Best-effort: failures are ignored, matching the regular-file mtime path.
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

/// Result of merging the image into ROOT
struct WalkResult {
    contents: Vec<ContentsEntry>,
    size: u64,
    /// Installed paths whose update was diverted to a `._cfg` file
    protected: Vec<Utf8PathBuf>,
}

fn rewrite_d_symlinks(env: &EbuildEnv) -> bool {
    env.eapi
        .parse::<Eapi>()
        .is_ok_and(|e| e.rewrites_d_symlinks())
}

/// Walk `image_dir` (`$ED`, where the built files physically live) into `dest_root`
///
/// `d_dir` is the bare `$D` (`work_root/image`, without the `$EPREFIX`
/// offset `$ED` adds) — PMS 13.4.1 rewrites an absolute symlink whose
/// target starts with `$D`, not `$ED`; stripping the wrong one either
/// leaves a leaked build-time `$D` path dangling or, under a real
/// `EPREFIX`, drops the offset entirely and escapes the prefix.
fn walk_image(
    image_dir: &Utf8Path,
    d_dir: &Utf8Path,
    dest_root: &Utf8Path,
    cp: &ConfigProtect,
    rewrite_d: bool,
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
                let mut target: Utf8PathBuf = raw_target
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("non-UTF-8 symlink target"))?;
                if rewrite_d
                    && target.is_absolute()
                    && let Ok(rest) = target.strip_prefix(d_dir)
                {
                    let rewritten = Utf8PathBuf::from("/").join(rest);
                    tracing::info!(
                        "rewriting absolute symlink {installed} -> {target} to {rewritten}"
                    );
                    target = rewritten;
                }
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
/// symlink's own ownership is set, not its target). Succeeds as real root
/// and under a fake root (fakeroost records the intended owner); an
/// unprivileged merge can't set foreign ownership, so the error is ignored
/// and the file keeps the build user (portage's own behaviour).
///
/// em previously left even a *root* install with `acct-user/*` dirs owned
/// by the invoking user.
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

/// Variables-only dump for the Compile→Install worker handoff
///
/// Deliberately no `declare -f`: the worker re-sources the ebuild (defining every
/// ebuild/eclass function), and brush's function printer doesn't round-trip heredoc bodies
/// (the indented `<<-EOF` delimiter never terminates), which would make the whole dump
/// unparseable.
///
/// Readonly declares are dropped too (re-declaring them in the worker only
/// produces "cannot mutate readonly variable" noise).
///
/// Also drops bash's own dynamic/special variables (`PIPESTATUS`,
/// `FUNCNAME`, …): `declare -p` prints them like any other variable, but
/// re-`declare`ing them in the worker pins a stale snapshot in place of the
/// shell's live tracking. Confirmed concretely for `PIPESTATUS`: real bash
/// unconditionally replaces the whole array on every new pipeline regardless
// of a prior explicit `declare`, but brush does not — once user code (here,
// our own restore) has declared it, brush never resizes it again, so a
// later 2-stage pipe in the Install worker still reports the compile
// parent's stale 1-element snapshot. That silently broke `distutils-r1`'s
// `pipestatus || die` check (`dev-python/jinja2`, the `install//usr/bin`
// listing failure).
// Filtering the dump is the correct fix independent of that brush bug: these
// variables are bash-maintained state, not build environment, and were never
// meant to cross a process boundary.
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
        iuse_effective: env.iuse_effective,
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
        rustflags: env.rustflags,
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

/// `GENTOO_MIRRORS`: process env → the live ebuild shell (root-aware —
/// `shell` already sourced whichever make.conf is correct for the active
/// `--root`/`--config-root`/`--prefix`, unlike a hardcoded host path) →
/// `make.globals` (never sourced into the shell itself, so still consulted
/// directly here, same convention as `binpkg.rs::resolve_pkgdir_for_roots`).
///
/// Without the `make.globals` fallback the mirror list is empty and a
/// distfile whose upstream URL fails has no fallback (the popt/tar fetch
/// failures in the @system stage build) — `make.globals` is where the real
/// `http://distfiles.gentoo.org` default lives.
fn gentoo_mirrors_list(shell: &portage_repo::EbuildShell) -> Vec<String> {
    if let Ok(val) = std::env::var("GENTOO_MIRRORS")
        && !val.trim().is_empty()
    {
        return val.split_whitespace().map(str::to_owned).collect();
    }
    if let Some(val) = shell.get_var("GENTOO_MIRRORS")
        && !val.trim().is_empty()
    {
        return val.split_whitespace().map(str::to_owned).collect();
    }
    let mg = Utf8Path::new(MAKE_GLOBALS);
    if mg.exists()
        && let Ok(mc) = MakeConf::load(mg)
        && let Some(val) = mc.get("GENTOO_MIRRORS")
    {
        return val.split_whitespace().map(str::to_owned).collect();
    }
    vec![]
}

/// Portage's shipped defaults; the source of `GENTOO_MIRRORS` when neither the
/// environment nor make.conf overrides it.
const MAKE_GLOBALS: &str = "/usr/share/portage/config/make.globals";

/// One `PORTAGE_ELOG_*` setting, along [`gentoo_mirrors_list`]'s
/// environment → shell → `make.globals` chain — but stopping at the first layer
/// that *sets* the variable, empty or not. `PORTAGE_ELOG_SYSTEM=""` in a
/// make.conf is how elog gets turned off, so an empty value must not fall
/// through to `make.globals`' non-empty default.
///
/// `pub(crate)`, not elog-specific despite the name: also reused by `em
/// --info` for DISTDIR/PKGDIR/PORTAGE_TMPDIR/GENTOO_MIRRORS, which are real
/// `make.globals`-only defaults (`ProfileStack` never sources that file —
/// see [`gentoo_mirrors_list`]'s doc) that a plain `shell.get_var` alone
/// would wrongly report as unset.
pub(crate) fn elog_setting(shell: &portage_repo::EbuildShell, name: &str) -> String {
    if let Ok(val) = std::env::var(name) {
        return val;
    }
    if let Some(val) = shell.get_var(name) {
        return val;
    }
    let mg = Utf8Path::new(MAKE_GLOBALS);
    if mg.exists()
        && let Ok(mc) = MakeConf::load(mg)
        && let Some(val) = mc.get(name)
    {
        return val.to_string();
    }
    String::new()
}

/// Run the configured elog modules over whatever this package's phases left in
/// `${T}/logging`.
///
/// The log directory follows portage's `mod_save`: `PORTAGE_LOGDIR` when set,
/// otherwise `<broot>/var/log/portage` — `broot` rather than the merge root
/// because the logs describe what *this* `em` did, and belong where `em` runs.
fn dispatch_elog(
    shell: &portage_repo::EbuildShell,
    ebuild: &Ebuild,
    work_root: &Utf8Path,
    work_dir: &Utf8Path,
    roots: RootContext<'_>,
) {
    let log = crate::elog::PackageLog::collect(&work_root.join("temp/logging"));
    if log.is_empty() {
        return;
    }
    let config = elog_config(shell, roots.broot);
    crate::elog::dispatch(
        &config,
        ebuild.cpv(),
        &log,
        crate::elog::Echo::Handoff(work_dir),
    );
}

/// The same, for the `pkg_prerm`/`pkg_postrm` of a package being removed
///
/// Real portage files these too (`vartree.py`'s
/// `_elog_process(phasefilter=("prerm", "postrm"))`) — the "after removing
/// this, you should …" `ewarn` is one of the more useful things elog carries.
/// They are echoed [in process](crate::elog::Echo::InProcess): the removal's
/// scratch work root is deleted the moment it finishes, so there is nowhere to
/// leave a handoff, and every caller is a process that finalizes its own queue.
fn dispatch_unmerge_elog(
    shell: &portage_repo::EbuildShell,
    cpv: &portage_atom::Cpv,
    old_work_root: &Utf8Path,
    root: &Utf8Path,
) {
    let log = crate::elog::PackageLog::collect(&old_work_root.join("temp/logging"));
    if log.is_empty() {
        return;
    }
    let config = elog_config(shell, None);
    crate::elog::dispatch(
        &config,
        cpv,
        &log,
        crate::elog::Echo::InProcess { root: Some(root) },
    );
}

/// Resolve the elog settings from the live shell
///
/// The log directory follows portage's `mod_save`: `PORTAGE_LOGDIR` when set,
/// otherwise `<broot>/var/log/portage` — `broot` rather than the merge root
/// because the logs describe what *this* `em` did, and belong where `em` runs.
/// `broot_hint` is the caller's root model when it has one; otherwise the
/// shell's own `BROOT`, which the build environment already set for the active
/// topology, stands in.
fn elog_config(
    shell: &portage_repo::EbuildShell,
    broot_hint: Option<&Utf8Path>,
) -> crate::elog::Config {
    let configured = elog_setting(shell, "PORTAGE_LOGDIR");
    let logdir = if configured.trim().is_empty() {
        let broot = broot_hint
            .map(Utf8Path::to_owned)
            .or_else(|| shell.get_var("BROOT").map(Utf8PathBuf::from))
            .filter(|b| !b.as_str().is_empty())
            .unwrap_or_else(|| Utf8PathBuf::from("/"));
        broot.join("var/log/portage")
    } else {
        Utf8PathBuf::from(configured.trim())
    };
    crate::elog::Config::new(
        &elog_setting(shell, "PORTAGE_ELOG_CLASSES"),
        &elog_setting(shell, "PORTAGE_ELOG_SYSTEM"),
        logdir,
    )
}

/// `FETCHCOMMAND`/`RESUMECOMMAND` from the live ebuild shell — root-aware for
/// the same reason as [`gentoo_mirrors_list`]. Previously read via a
/// hardcoded `[DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF]` scan of the *host's*
/// make.conf regardless of the active root, and via a plain `MakeConf::get`
/// that wouldn't expand a `${VAR}` self-reference the way `shell`'s real
/// sourcing does.
fn read_fetch_commands(shell: &portage_repo::EbuildShell) -> (Option<String>, Option<String>) {
    let fetch = shell.get_var("FETCHCOMMAND").filter(|s| !s.is_empty());
    let resume = shell.get_var("RESUMECOMMAND").filter(|s| !s.is_empty());
    (fetch, resume)
}

#[cfg(test)]
mod tests {

    // Pinned against a real kernel entry rather than a fixture: this is the
    // one place `em` depends on `/proc/locks` matching the file it is given,
    // and an inode-only match (the bug the device comparison fixes) would
    // still pass a fixture.
    #[test]
    #[cfg_attr(not(target_os = "linux"), ignore = "reads /proc/locks")]
    fn flock_holder_pid_finds_the_process_holding_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("held.lock");
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive).unwrap();

        assert_eq!(
            super::flock_holder_pid(&path),
            Some(std::process::id()),
            "should have found our own pid in /proc/locks"
        );

        drop(f);
        assert_eq!(
            super::flock_holder_pid(&path),
            None,
            "no holder once the lock is released"
        );
    }
    use super::*;
    use portage_vdb::ContentsKind;
    use std::fs;
    use std::os::unix::fs::symlink;

    // Dual-root same-CPV plan entries must not share a WORKDIR (Sonnet
    #[test]
    fn package_work_dir_isolates_merge_roots() {
        let base = Utf8Path::new("/tmp/em-work");
        let host = package_work_dir(base, Utf8Path::new("/"), "llvm-core", "clang-22.1.8");
        let target = package_work_dir(
            base,
            Utf8Path::new("/opt/xp/usr/riscv64-unknown-linux-gnu"),
            "llvm-core",
            "clang-22.1.8",
        );
        assert_ne!(host, target);
        assert!(host.as_str().contains("/host/llvm-core/clang-22.1.8"));
        assert!(
            target
                .as_str()
                .contains("opt-xp-usr-riscv64-unknown-linux-gnu")
        );
        assert_eq!(work_root_key(Utf8Path::new("/")), "host");
        assert_eq!(
            work_root_key(Utf8Path::new("/")),
            work_root_key(Utf8Path::new("//"))
        );
    }

    // Regression test for the gnupg stage1 failure: the Install worker must
    // never wipe `temp` (`${T}`) — it's cross-phase scratch space the
    // Compile parent may have staged a file into for `src_install` to
    // consume, not throwaway-per-process state like `image` (`${D}`).
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

    #[test]
    fn run_phase_round_trips_short_and_full_pms_names() {
        use portage_metadata::Phase;
        assert_eq!("compile".parse::<RunPhase>().unwrap(), RunPhase::COMPILE);
        assert_eq!(
            "src_compile".parse::<RunPhase>().unwrap(),
            RunPhase::Ebuild(Phase::SrcCompile)
        );
        assert_eq!(
            "preinst".parse::<RunPhase>().unwrap().to_string(),
            "preinst"
        );
    }

    #[test]
    fn run_phase_accepts_merge_and_qmerge_as_the_same_phase() {
        assert_eq!("merge".parse::<RunPhase>().unwrap(), RunPhase::Qmerge);
        assert_eq!("qmerge".parse::<RunPhase>().unwrap(), RunPhase::Qmerge);
        assert_eq!(RunPhase::Qmerge.to_string(), "qmerge");
    }

    #[test]
    fn run_phase_rejects_an_unknown_name() {
        assert!("not-a-real-phase".parse::<RunPhase>().is_err());
    }

    #[test]
    fn full_phase_group_runs_fetch_before_unpack_and_ends_on_qmerge() {
        let phases = PhaseGroup::Full.phases();
        assert_eq!(phases.first(), Some(&RunPhase::PRETEND));
        assert_eq!(phases.last(), Some(&RunPhase::Qmerge));
        let fetch_pos = phases.iter().position(|p| *p == RunPhase::Fetch).unwrap();
        let unpack_pos = phases.iter().position(|p| *p == RunPhase::UNPACK).unwrap();
        assert!(fetch_pos < unpack_pos);
    }

    // Full/Compile/BinpkgMerge start genuinely fresh, so they wipe
    // everything including `work` — unlike Install, which relies on
    // `work`'s compile artifacts surviving the process boundary.
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

    fn feats(tokens: &[&str]) -> std::collections::HashSet<String> {
        tokens.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn filter_clean_keepwork_skips_everything() {
        let base = ["work", "image", "temp", "homedir"];
        assert!(filter_clean_subs(&base, &feats(&["keepwork"]), CleanWhen::Pre).is_empty());
        assert!(filter_clean_subs(&base, &feats(&["keepwork"]), CleanWhen::Post).is_empty());
    }

    #[test]
    fn filter_clean_keeptemp_preserves_temp_pre_and_post() {
        let base = ["work", "image", "temp", "homedir"];
        let pre = filter_clean_subs(&base, &feats(&["keeptemp"]), CleanWhen::Pre);
        assert_eq!(pre, vec!["work", "image", "homedir"]);
        let post = filter_clean_subs(&base, &feats(&["keeptemp"]), CleanWhen::Post);
        assert_eq!(post, vec!["work", "image", "homedir"]);
    }

    #[test]
    fn filter_clean_noclean_only_affects_post() {
        let base = ["work", "image", "temp", "homedir"];
        let pre = filter_clean_subs(&base, &feats(&["noclean"]), CleanWhen::Pre);
        assert_eq!(pre, base.to_vec(), "noclean must not disable pre-clean");
        let post = filter_clean_subs(&base, &feats(&["noclean"]), CleanWhen::Post);
        assert_eq!(
            post,
            vec!["image", "homedir"],
            "noclean keeps source (work) and temp"
        );
    }

    // Regression test for the jinja2 stage3 failure: restoring `PIPESTATUS`
    // (or the other bash dynamic vars) into the Install worker pins a stale
    // snapshot that brush never resizes on later pipelines.
    // The fix is simply never dumping them in the first place.
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
            walk_image(&image, &image, &root, &ConfigProtect::none(), false).unwrap();

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
    fn walk_image_rewrites_absolute_symlink_into_d() {
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let root_on = Utf8PathBuf::try_from(tmp.path().join("root-on")).unwrap();
        let root_off = Utf8PathBuf::try_from(tmp.path().join("root-off")).unwrap();
        fs::create_dir_all(image.join("usr/bin").as_std_path()).unwrap();
        fs::write(image.join("usr/bin/tool").as_std_path(), b"x").unwrap();
        let abs = image.join("usr/bin/tool");
        symlink(abs.as_std_path(), image.join("usr/bin/tp").as_std_path()).unwrap();
        fs::create_dir_all(root_on.as_std_path()).unwrap();
        fs::create_dir_all(root_off.as_std_path()).unwrap();

        let WalkResult { contents, .. } =
            walk_image(&image, &image, &root_on, &ConfigProtect::none(), true).unwrap();
        let tp = contents
            .iter()
            .find(|e| e.path.as_str() == "/usr/bin/tp")
            .unwrap();
        assert_eq!(tp.target.as_deref(), Some(Utf8Path::new("/usr/bin/tool")));

        let WalkResult { contents, .. } =
            walk_image(&image, &image, &root_off, &ConfigProtect::none(), false).unwrap();
        let tp = contents
            .iter()
            .find(|e| e.path.as_str() == "/usr/bin/tp")
            .unwrap();
        assert_eq!(tp.target.as_deref(), Some(abs.as_path()));
    }

    // PMS 13.4.1 strips a leading $D, not $ED. Under a real EPREFIX, $ED is
    // a subdir of $D (`$D$EPREFIX`) — the two prior bugs: a target using
    // bare $D didn't match a strip-$ED prefix at all and stayed dangling;
    // a target using $ED got the whole prefix offset stripped, escaping it.
    #[test]
    fn walk_image_strips_bare_d_not_ed_under_a_real_eprefix() {
        let tmp = tempfile::tempdir().unwrap();
        let d = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let ed = d.join("prefixoff");
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();
        fs::create_dir_all(ed.join("usr/bin").as_std_path()).unwrap();
        fs::write(ed.join("usr/bin/tool").as_std_path(), b"x").unwrap();
        // A symlink target using the bare $D path (a common ebuild mistake
        // under offset-prefix EAPIs).
        symlink(
            d.join("usr/bin/tool").as_std_path(),
            ed.join("usr/bin/tp_bare_d").as_std_path(),
        )
        .unwrap();
        // A symlink target using $ED (the correct convention).
        symlink(
            ed.join("usr/bin/tool").as_std_path(),
            ed.join("usr/bin/tp_ed").as_std_path(),
        )
        .unwrap();
        fs::create_dir_all(root.as_std_path()).unwrap();

        let WalkResult { contents, .. } =
            walk_image(&ed, &d, &root, &ConfigProtect::none(), true).unwrap();

        let bare_d = contents
            .iter()
            .find(|e| e.path.as_str() == "/usr/bin/tp_bare_d")
            .unwrap();
        assert_eq!(
            bare_d.target.as_deref(),
            Some(Utf8Path::new("/usr/bin/tool")),
            "a bare-$D target must be rewritten relative to $D, not left unmodified"
        );

        let via_ed = contents
            .iter()
            .find(|e| e.path.as_str() == "/usr/bin/tp_ed")
            .unwrap();
        assert_eq!(
            via_ed.target.as_deref(),
            Some(Utf8Path::new("/prefixoff/usr/bin/tool")),
            "an $ED-based target must keep the $EPREFIX offset, not have it stripped away"
        );
    }

    #[test]
    fn walk_image_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();
        fs::create_dir_all(image.as_std_path()).unwrap();
        fs::create_dir_all(root.as_std_path()).unwrap();

        let WalkResult { contents, size, .. } =
            walk_image(&image, &image, &root, &ConfigProtect::none(), false).unwrap();
        assert!(contents.is_empty());
        assert_eq!(size, 0);
    }

    #[test]
    fn walk_image_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("no-such-image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();
        let WalkResult { contents, size, .. } =
            walk_image(&image, &image, &root, &ConfigProtect::none(), false).unwrap();
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
        } = walk_image(&image, &image, &root, &cp, false).unwrap();

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

        let WalkResult { contents, .. } =
            walk_image(&image, &image, &root, &ConfigProtect::none(), false)
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

        walk_image(&image, &image, &root, &ConfigProtect::none(), false).unwrap();

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

        walk_image(&image, &image, &root, &ConfigProtect::none(), false).unwrap();

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
        walk_image(&image, &image, &root, &cp, false).unwrap();
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

        remove_old_unique_files(&old_contents, &new_contents, &HashSet::new(), &root);

        assert!(!root.join("usr/bin/old-only").exists());
        assert!(root.join("usr/bin/shared").exists());
        assert!(root.join("usr/bin").exists());
    }
}
