//! CLI-facing binpkg config resolution: `PKGDIR` lookup and the
//! `binrepos.conf`/`PORTAGE_BINHOST` remote-binhost list.
//!
//! The binpkg *format* itself (the `Packages` index, GPKG containers, the
//! USE-reuse check, maintenance operations) lives in the standalone
//! `portage-binpkg` crate — this module only holds the bits that genuinely
//! need `&Cli`/`make.conf`, which that crate deliberately doesn't depend on.

use std::collections::HashSet;

use camino::{Utf8Path, Utf8PathBuf};

use portage_repo::MakeConf;
use portage_resolve::Roots;

use crate::cli::Cli;

/// Real portage's own hardcoded system default — for the host-root test
/// only; `resolve_pkgdir` no longer references this directly since
/// `merge_root.join("var/cache/binpkgs")` reduces to the same string when
/// `merge_root` is `/`.
#[cfg(test)]
const DEFAULT_PKGDIR: &str = "/var/cache/binpkgs";
const MAKE_GLOBALS: &str = "/usr/share/portage/config/make.globals";

/// Resolve `PKGDIR` under `globals`'s own (`--target`-substituted) roots —
/// see [`resolve_pkgdir_for_roots`] for the algorithm. Shared by `em maint
/// binhost`/`em maint binpkg` and the `-k` consumer, all single-root
/// commands where "globals's own roots" is unambiguous.
pub(crate) async fn resolve_pkgdir(globals: &Cli) -> Utf8PathBuf {
    resolve_pkgdir_for_roots(&globals.roots()).await
}

/// Resolve `PKGDIR` under specific `roots`: `$PKGDIR` env → `make.conf`
/// (`roots.config()`) → `make.globals` (host builds only) →
/// `roots.merge_root().join("var/cache/binpkgs")`.
///
/// The per-roots twin of [`resolve_pkgdir`] — used by the merge path to open
/// the **host** PKGDIR for `MergeRoot::Host` plan entries under `--target`,
/// separately from the target's own PKGDIR that `resolve_pkgdir` resolves.
///
/// The `make.globals`/hardcoded-default steps are **host** defaults — real
/// portage's own system-wide install convention, unconditionally
/// `/var/cache/binpkgs` (confirmed: this repo's own `make.globals` hardcodes
/// exactly that). For a `--root`/`--target`/`--local`/`--prefix` merge root
/// (anything other than `/`), consulting that host default is wrong: it's a
/// real, root-owned system path the build has no business writing to, and
/// unprivileged builds can't anyway. Caught live: a stage3 `--buildpkg` run
/// tried to write there, got `EACCES`, and appears to have destabilized the
/// fakeroost ptrace session for several packages — see
/// `todo/stage-build-shakeout.md`. Skip straight to a root-relative default
/// in that case; `$PKGDIR`/config-root `make.conf` (explicit user choices)
/// still apply regardless of root.
pub(crate) async fn resolve_pkgdir_for_roots(roots: &Roots) -> Utf8PathBuf {
    if let Ok(v) = std::env::var("PKGDIR")
        && !v.trim().is_empty()
    {
        return Utf8PathBuf::from(v);
    }
    if let Some(v) = read_make_conf_var_for_roots(roots, "PKGDIR").await
        && !v.is_empty()
    {
        return Utf8PathBuf::from(v);
    }
    let merge_root = roots.merge_root().to_owned();
    // make.globals is a host-level default; only consult it for a real host
    // build. A non-host root falls through to the join below unconditionally
    // — no separate "is this the host?" branch needed there, since
    // `"/".join("var/cache/binpkgs")` already *is* the host default.
    if merge_root.as_str() == "/" {
        let mg = Utf8Path::new(MAKE_GLOBALS);
        if mg.exists()
            && let Ok(mc) = MakeConf::load(mg)
            && let Some(v) = mc.get("PKGDIR").filter(|s| !s.is_empty())
        {
            return Utf8PathBuf::from(v);
        }
    }
    merge_root.join("var/cache/binpkgs")
}

/// Resolve the GPG verify keyring directory (`BINPKG_GPG_VERIFY_GPG_HOME`):
/// `$BINPKG_GPG_VERIFY_GPG_HOME` env → make.conf → `<config root>/etc/portage/gnupg`.
/// Root-aware via `roots.config()` (never the real host's `/etc/portage/gnupg`
/// for a non-host `--root`/`--target`/`--prefix` — the same class of bug this
/// project already fixed once for `PKGDIR`, see `resolve_pkgdir_for_roots`'s
/// doc comment) — a flat directory of armored public-key files, not a real
/// gpg keybox (see `portage_binpkg::gpg`'s module doc for why).
pub(crate) async fn resolve_gpg_verify_home_for_roots(roots: &Roots) -> Utf8PathBuf {
    if let Ok(v) = std::env::var("BINPKG_GPG_VERIFY_GPG_HOME")
        && !v.trim().is_empty()
    {
        return Utf8PathBuf::from(v);
    }
    if let Some(v) = read_make_conf_var_for_roots(roots, "BINPKG_GPG_VERIFY_GPG_HOME").await
        && !v.is_empty()
    {
        return Utf8PathBuf::from(v);
    }
    roots
        .config()
        .unwrap_or_else(|| Utf8Path::new("/"))
        .join("etc/portage/gnupg")
}

/// Open the local `PKGDIR` binpkg index if `-k`/`--usepkg` or
/// `-K`/`--usepkgonly` is active, for the `-p` display to check binary
/// reuse against (see `query::depgraph::output::PrettyCtx::binpkg_index`).
/// `None` when neither flag is set — matching `run_merge_plan`'s own
/// `want_local` condition, minus the remote (`-g`/`-G`) half: checking a
/// binhost here would add a network fetch to a plain preview, so `-p`
/// under `-g`/`-G` alone still shows `[ebuild ...]` even though the real
/// merge may reuse a remote binpkg. Silent on error (unlike
/// `run_merge_plan`'s own open, which warns) — this is a best-effort
/// preview hint, not the path that actually performs the reuse.
///
/// Also single-index only, unlike the real merge path's per-entry host/target
/// `dual_pkgdir` split (`run_merge_plan`): under `--target -k`, a `MergeRoot::Host`
/// row may print `[ebuild ...]` here while the real merge reuses a host-PKGDIR
/// binpkg — same display-only divergence class as the `-g`/`-G` one above.
pub(crate) async fn open_local_index_for_preview(
    globals: &Cli,
    merge_flags: &crate::cli::MergeFlags,
) -> Option<portage_binpkg::BinpkgIndex> {
    if !(merge_flags.usepkg || merge_flags.usepkgonly) {
        return None;
    }
    portage_binpkg::BinpkgIndex::open(resolve_pkgdir(globals).await.as_std_path()).ok()
}

/// Read a variable from `make.conf` under the resolved config root.
pub(crate) async fn read_make_conf_var(globals: &Cli, var: &str) -> Option<String> {
    read_make_conf_var_for_roots(&globals.roots(), var).await
}

/// Read a variable from make.conf under specific roots — used to get
/// per-entry CHOST/CFLAGS/etc. for cross-compilation scenarios, where the
/// desired config root isn't `globals`'s own (e.g. a `MergeRoot::Host` entry
/// under `--target`).
///
/// Evaluates the file via [`MakeConf::apply_to`] (a real, minimal
/// `brush_core::Shell` sourcing the file) rather than a plain
/// [`MakeConf::get`], so `${VAR}` self-references expand — the stock Gentoo
/// stage3 pattern `COMMON_FLAGS="-O2 -march=…"` + `CFLAGS="${COMMON_FLAGS}"`
/// used to make every reader of this function see a literal, `-m*`-token-free
/// `${COMMON_FLAGS}` string instead of the real flags. That silently starved
/// the binpkg `build_env_key` gate (every desired key came out empty on such
/// a host, which the asymmetric gate then rejects against any binpkg that
/// *does* have a recorded key — a permanent rebuild loop on stock configs).
/// References to a name this file never assigns (e.g. an ambient `${EPREFIX}`
/// some other subsystem sets) now expand to empty rather than staying
/// literal — also more correct than before, since that literal string was
/// never a usable value either.
pub(crate) async fn read_make_conf_var_for_roots(roots: &Roots, var: &str) -> Option<String> {
    let cfg_root = roots
        .config()
        .map(|c| c.to_path_buf())
        .unwrap_or_else(|| Utf8PathBuf::from("/"));
    for rel in ["etc/portage/make.conf", "etc/make.conf"] {
        let p = cfg_root.join(rel);
        if p.exists()
            && let Ok(mc) = MakeConf::load(&p)
        {
            let mut env = std::collections::BTreeMap::new();
            mc.apply_to(&mut env).await.ok()?;
            if let Some(v) = env.get(var).filter(|s| !s.is_empty()) {
                return Some(v.clone());
            }
        }
    }
    None
}

/// Evaluate whichever of `roots`' two candidate make.conf paths exists first
/// (`etc/portage/make.conf`, then `etc/make.conf` — same fallback rule
/// [`read_make_conf_var_for_roots`] uses per-variable) into a flat
/// `NAME -> value` map via [`MakeConf::apply_to`]. Empty map if neither
/// exists or fails to parse. Unlike `read_make_conf_var_for_roots`, this
/// picks one file and returns its whole map rather than falling back to the
/// second path per-variable — a split make.conf across both paths is not a
/// real configuration this repo needs to support, and evaluating once
/// instead of once-per-variable is also the point (see [`DesiredBuildEnv`]).
async fn evaluated_make_conf_env(roots: &Roots) -> std::collections::BTreeMap<String, String> {
    let cfg_root = roots
        .config()
        .map(|c| c.to_path_buf())
        .unwrap_or_else(|| Utf8PathBuf::from("/"));
    for rel in ["etc/portage/make.conf", "etc/make.conf"] {
        let p = cfg_root.join(rel);
        if p.exists()
            && let Ok(mc) = MakeConf::load(&p)
        {
            let mut env = std::collections::BTreeMap::new();
            if mc.apply_to(&mut env).await.is_ok() {
                return env;
            }
        }
    }
    std::collections::BTreeMap::new()
}

/// The desired build environment resolved from one config root's make.conf:
/// the four build-env flag vars (expanded) plus `CHOST`. Shared by `em maint
/// binpkg fingerprint` and the merge path's per-entry desired build_env_key
/// computation.
#[derive(Default)]
pub(crate) struct DesiredBuildEnv {
    pub cflags: String,
    pub cxxflags: String,
    pub ldflags: String,
    pub rustflags: String,
    pub chost: String,
    /// The full evaluated make.conf map this environment was derived from —
    /// kept so [`Self::key_for`] can seed a package.env overlay with it: an
    /// env file referencing e.g. `${COMMON_FLAGS}` needs the same baseline
    /// `for_roots` itself evaluated, not just the four flat flag values.
    make_conf_env: std::collections::BTreeMap<String, String>,
}

impl DesiredBuildEnv {
    /// Read the desired build env for `roots` — one make.conf evaluation,
    /// not five. `chost` falls back to the process `CHOST` env var when
    /// make.conf doesn't set it, the same rule `merge_sequential`/
    /// `merge_parallel` already apply for their own per-entry desired CHOST.
    pub(crate) async fn for_roots(roots: &Roots) -> Self {
        let make_conf_env = evaluated_make_conf_env(roots).await;
        let get = |name: &str| make_conf_env.get(name).cloned().unwrap_or_default();
        let chost = make_conf_env
            .get("CHOST")
            .cloned()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("CHOST").ok().filter(|s| !s.is_empty()))
            .unwrap_or_default();
        Self {
            cflags: get("CFLAGS"),
            cxxflags: get("CXXFLAGS"),
            ldflags: get("LDFLAGS"),
            rustflags: get("RUSTFLAGS"),
            chost,
            make_conf_env,
        }
    }

    /// The build-env key derived from this environment's flags (see
    /// [`portage_binpkg::build_env_key`]) — the make.conf-only baseline, no
    /// per-package package.env overlay. See [`Self::key_for`] for that.
    pub(crate) fn key(&self) -> String {
        portage_binpkg::build_env_key(&self.cflags, &self.cxxflags, &self.ldflags, &self.rustflags)
    }

    /// A bare `chost`-only environment, no make.conf/filesystem involved —
    /// for tests that only care about host-vs-target selection, not flag
    /// evaluation (e.g. `entry_desired_env`'s own tests in `merge/mod.rs`).
    #[cfg(test)]
    pub(crate) fn for_test(chost: &str) -> Self {
        Self {
            chost: chost.to_string(),
            ..Default::default()
        }
    }

    /// Per-package build-env key (S6): overlay `cpv`'s `package.env` files
    /// (`portage_repo::env_files_for`) onto the make.conf baseline and
    /// re-derive. Falls back to [`Self::key`] when no env file matches this
    /// package — the common case, zero extra I/O beyond the package.env
    /// lookup itself.
    ///
    /// This overlays each env file onto the make.conf baseline via
    /// [`MakeConf::apply_to`] again — a real shell sourcing round-trip, so
    /// `CFLAGS="${CFLAGS} -foo"` appends, `CFLAGS="-bar"` overrides, and
    /// arbitrary shell logic (conditionals, command substitution) in an env
    /// file is evaluated for real rather than silently ignored. A wrong
    /// *desired* key here only risks an extra rebuild or a missed reuse
    /// (this repo's established safe direction), never wrong-arch reuse —
    /// the binpkg side's own recorded key is always shell-accurate.
    ///
    /// `slot: None` limitation: slot-qualified `package.env` atoms won't
    /// match here (a plan entry doesn't carry a slot at this point) — the
    /// real build still applies them; the worst case is a missed reuse for
    /// that package, not a wrong one.
    pub(crate) async fn key_for(
        &self,
        portage_dirs: &[std::path::PathBuf],
        cpv: &portage_atom::Cpv,
    ) -> String {
        let env_files = portage_repo::env_files_for(portage_dirs, cpv, None);
        if env_files.is_empty() {
            return self.key();
        }
        let mut env = self.make_conf_env.clone();
        for f in &env_files {
            if let Some(p) = Utf8Path::from_path(f)
                && let Ok(mc) = MakeConf::load(p)
            {
                let _ = mc.apply_to(&mut env).await;
            }
        }
        // build_env_key only needs &str, so borrow straight out of `env`
        // instead of allocating a fresh String per flag just to re-borrow it.
        let get = |name: &str| env.get(name).map(String::as_str).unwrap_or("");
        portage_binpkg::build_env_key(
            get("CFLAGS"),
            get("CXXFLAGS"),
            get("LDFLAGS"),
            get("RUSTFLAGS"),
        )
    }

    /// The `/etc/portage` directories [`portage_repo::env_files_for`] should
    /// search for `roots`' `package.env` — mirrors the real build path's own
    /// construction (`ebuild.rs`'s per-package build-environment sourcing,
    /// `config_root.join("etc/portage")` plus the eprefix overlay) so
    /// plan-time and build-time can't silently drift apart.
    pub(crate) fn portage_dirs(roots: &Roots) -> Vec<std::path::PathBuf> {
        let base = roots
            .config()
            .map(|c| c.to_path_buf())
            .unwrap_or_else(|| Utf8PathBuf::from("/"));
        let mut dirs = vec![base.join("etc/portage").into_std_path_buf()];
        if let Some(overlay) = roots.config_overlay() {
            dirs.push(overlay.as_std_path().to_path_buf());
        }
        dirs
    }
}

/// One `binrepos.conf` section — real portage's `BinRepoConfig`, restricted
/// to the fields em's remote binpkg fetch path uses. `frozen`/
/// `verify_signature` are parsed and carried but not yet *enforced*: `frozen`
/// ("prefer a locally cached index over fetching fresh") needs the
/// not-yet-built local index cache to have any effect, and
/// `verify_signature` needs the not-yet-built GPG verify step — both already
/// tracked in `todo/PENDING.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinRepoEntry {
    /// Section name, or an md5 hex digest of the `sync-uri` for a
    /// `PORTAGE_BINHOST`-derived implicit entry (matches real portage's own
    /// `_digest_uri` naming — this is display/debugging only, never a sort
    /// tie-breaker in practice: implicit entries always get a distinct
    /// priority `>= 1`, so they never actually tie against an explicit
    /// section's default `priority` of `0`).
    pub name: String,
    /// The binhost base URI, trailing slash stripped.
    pub sync_uri: String,
    pub frozen: bool,
    pub verify_signature: bool,
}

/// Resolve the configured remote binhosts: `binrepos.conf` (global defaults,
/// then `${PORTAGE_CONFIGROOT}/etc/portage/binrepos.conf` — either may be a
/// directory of `*.conf` files, real portage's own two-path search order,
/// `dbapi/bintree.py`'s `getbinpkgs` `config_paths`) plus legacy
/// `PORTAGE_BINHOST`, combined in real portage's own priority order
/// (`BinRepoConfigLoader.__init__`): explicit sections use their own
/// `priority =` (default `0`, ties broken by name); `PORTAGE_BINHOST`'s
/// space-separated URLs are folded in as unnamed, auto-prioritized entries,
/// skipping any URL an explicit section already covers. The combined list is
/// sorted **ascending** by `(priority, name)` and then **reversed** for
/// final order — matching `bintree.py`'s own
/// `reversed(list(self._binrepos_conf.values()))`. For a plain
/// `PORTAGE_BINHOST` list with no `binrepos.conf` at all, the two reversals
/// cancel out, netting the original left-to-right order (verified against
/// real portage's source, not assumed — see the unit tests below). Used by
/// `-g`/`--getbinpkg`.
///
/// Simplification vs real portage's `ConfigParser`: no `%(VAR)s`
/// interpolation, and a `[DEFAULT]` section's keys are not inherited into
/// other sections (same simplification `ReposConf` already makes for
/// `repos.conf`'s own `[DEFAULT]`/`main-repo`) — no configured value
/// observed in practice needs either.
pub(crate) async fn portage_binhosts(globals: &Cli) -> Vec<BinRepoEntry> {
    let config_root = globals
        .roots()
        .config()
        .map(|c| c.to_path_buf())
        .unwrap_or_else(|| Utf8PathBuf::from("/"));

    let mut sections: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
        std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for rel in [
        "usr/share/portage/config/binrepos.conf",
        "etc/portage/binrepos.conf",
    ] {
        let path = config_root.join(rel);
        for file in portage_repo::ini::collect_conf_files(path.as_std_path()).unwrap_or_default() {
            if let Ok(contents) = std::fs::read_to_string(&file) {
                portage_repo::ini::merge_sections(&mut sections, &mut order, &contents);
            }
        }
    }

    let binhost_var = std::env::var("PORTAGE_BINHOST")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let binhost_var = match binhost_var {
        Some(v) => Some(v),
        None => read_make_conf_var(globals, "PORTAGE_BINHOST")
            .await
            .filter(|v| !v.is_empty()),
    };

    combine_binhosts(&sections, &order, binhost_var.as_deref())
}

/// The pure core of [`portage_binhosts`]: combine parsed `binrepos.conf`
/// sections with a legacy `PORTAGE_BINHOST` value into the final,
/// priority-ordered list. Split out from the I/O (file reads, env var,
/// `make.conf`) so the priority/reversal algorithm — the part most worth
/// getting exactly right — is unit-testable without mutating the real
/// process environment (`PORTAGE_BINHOST` is process-global; tests run
/// threaded within one process, so setting it in a test would race any
/// other test touching the same var).
fn combine_binhosts(
    sections: &std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    order: &[String],
    binhost_var: Option<&str>,
) -> Vec<BinRepoEntry> {
    let mut seen_uris: HashSet<String> = HashSet::new();
    // (priority, name) carried alongside each entry purely for the final
    // sort — not part of the public `BinRepoEntry`, since callers only ever
    // need the already-resolved order.
    let mut repos: Vec<(Option<i64>, String, BinRepoEntry)> = Vec::new();
    for name in order {
        let Some(s) = sections.get(name) else {
            continue;
        };
        let Some(sync_uri) = s.get("sync-uri").map(|v| normalize_binhost_uri(v)) else {
            eprintln!("warning: missing sync-uri setting for binrepo {name}");
            continue;
        };
        seen_uris.insert(sync_uri.clone());
        let priority = s.get("priority").and_then(|v| v.parse::<i64>().ok());
        repos.push((
            priority,
            name.clone(),
            BinRepoEntry {
                name: name.clone(),
                sync_uri,
                frozen: parse_binrepo_bool(s.get("frozen")),
                verify_signature: parse_binrepo_bool(s.get("verify-signature")),
            },
        ));
    }

    if let Some(val) = binhost_var {
        let mut current_priority: i64 = 0;
        for url in val.split_whitespace().rev() {
            let sync_uri = normalize_binhost_uri(url);
            if seen_uris.insert(sync_uri.clone()) {
                current_priority += 1;
                let name = format!("{:x}", md5::compute(sync_uri.as_bytes()));
                repos.push((
                    Some(current_priority),
                    name.clone(),
                    BinRepoEntry {
                        name,
                        sync_uri,
                        frozen: false,
                        verify_signature: false,
                    },
                ));
            }
        }
    }

    repos.sort_by(|a, b| (a.0.unwrap_or(0), &a.1).cmp(&(b.0.unwrap_or(0), &b.1)));
    repos.into_iter().rev().map(|(_, _, e)| e).collect()
}

fn normalize_binhost_uri(uri: &str) -> String {
    uri.trim().trim_end_matches('/').to_string()
}

fn parse_binrepo_bool(v: Option<&String>) -> bool {
    matches!(v.map(|s| s.to_lowercase()), Some(s) if s == "true" || s == "yes")
}

pub(crate) fn next_build_id(pkgdir: &Utf8Path, cat: &str, pf: &str) -> u32 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Regression test for the stage3 --buildpkg failure: a non-host root
    /// must never default PKGDIR to the real system's `/var/cache/binpkgs`
    /// (root-owned, not writable, and not even meaningful for a different
    /// root's package cache) — see `resolve_pkgdir`'s doc comment.
    #[tokio::test]
    async fn non_host_root_gets_root_relative_pkgdir_default() {
        assert!(
            std::env::var("PKGDIR").is_err(),
            "test assumes no ambient PKGDIR override"
        );
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let cli = Cli::parse_from(["em", "--root", root]);
        let pkgdir = resolve_pkgdir(&cli).await;
        assert_eq!(
            pkgdir,
            camino::Utf8Path::new(root).join("var/cache/binpkgs")
        );
    }

    /// A plain host build (root `/`, no --root/--prefix/--local/--target) is
    /// unaffected by the root-aware branch — it still falls through to the
    /// pre-existing make.globals/hardcoded-default lookup, exactly as before
    /// this change.
    #[tokio::test]
    async fn host_root_skips_the_root_relative_branch() {
        assert!(
            std::env::var("PKGDIR").is_err(),
            "test assumes no ambient PKGDIR override"
        );
        // `["em"]` alone (zero args) trips clap's `arg_required_else_help`
        // (prints help and exits the process) — pass --root explicitly.
        let cli = Cli::parse_from(["em", "--root", "/"]);
        assert_eq!(cli.roots().merge_root().as_str(), "/");
        let expected = {
            let mg = Utf8Path::new(MAKE_GLOBALS);
            if mg.exists()
                && let Ok(mc) = MakeConf::load(mg)
                && let Some(v) = mc.get("PKGDIR").filter(|s| !s.is_empty())
            {
                Utf8PathBuf::from(v)
            } else {
                Utf8PathBuf::from(DEFAULT_PKGDIR)
            }
        };
        assert_eq!(resolve_pkgdir(&cli).await, expected);
    }

    /// A `--target` plan's own roots (`resolve_pkgdir`) resolve under the
    /// target sysroot, while `broot()` (host roots, what a `MergeRoot::Host`
    /// entry actually wants) resolve as a plain host build — the two must
    /// disagree here, or a Host BDEPEND entry would look up binpkgs in the
    /// wrong PKGDIR (S1/S4 in `todo/binpkg-subtargets.md`).
    #[tokio::test]
    async fn resolve_pkgdir_for_roots_target_vs_host() {
        assert!(
            std::env::var("PKGDIR").is_err(),
            "test assumes no ambient PKGDIR override"
        );
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let cli = Cli::parse_from([
            "em",
            "--root",
            root,
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);

        let target_roots = cli.roots();
        assert_eq!(
            target_roots.merge_root().as_str(),
            format!("{root}/usr/riscv64-unknown-linux-gnu")
        );
        assert_eq!(
            resolve_pkgdir_for_roots(&target_roots).await,
            camino::Utf8Path::new(root)
                .join("usr/riscv64-unknown-linux-gnu")
                .join("var/cache/binpkgs")
        );

        let host_roots = cli.broot();
        assert_eq!(
            host_roots.merge_root().as_str(),
            "/",
            "--root's BROOT is the real host, not the sysroot (task #17 fix)"
        );
        let expected_host = {
            let mg = Utf8Path::new(MAKE_GLOBALS);
            if mg.exists()
                && let Ok(mc) = MakeConf::load(mg)
                && let Some(v) = mc.get("PKGDIR").filter(|s| !s.is_empty())
            {
                Utf8PathBuf::from(v)
            } else {
                Utf8PathBuf::from(DEFAULT_PKGDIR)
            }
        };
        assert_eq!(resolve_pkgdir_for_roots(&host_roots).await, expected_host);
    }

    /// Config-root make.conf `PKGDIR=` wins for whichever roots see that
    /// config root — proven independently for target and host roots.
    #[tokio::test]
    async fn resolve_pkgdir_for_roots_honours_config_root_make_conf() {
        assert!(
            std::env::var("PKGDIR").is_err(),
            "test assumes no ambient PKGDIR override"
        );
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let conf_dir = dir.path().join("etc/portage");
        std::fs::create_dir_all(&conf_dir).unwrap();
        std::fs::write(conf_dir.join("make.conf"), "PKGDIR=\"/custom/pkgdir\"\n").unwrap();

        // `--config-root` required (see `portage_binhosts`'s own tests below):
        // a bare `--root` leaves `config()` at the real host `/`.
        let cli = Cli::parse_from(["em", "--root", root, "--config-root", root]);
        assert_eq!(
            resolve_pkgdir_for_roots(&cli.roots()).await,
            camino::Utf8Path::new("/custom/pkgdir")
        );
    }

    /// The stock Gentoo stage3 `COMMON_FLAGS=… CFLAGS="${COMMON_FLAGS}"`
    /// pattern must resolve to the real flags, not the literal `${…}` text —
    /// see `read_make_conf_var_for_roots`'s doc comment for why this matters
    /// (an unexpanded read silently starves the binpkg reuse key).
    #[tokio::test]
    async fn read_make_conf_var_expands_common_flags_indirection() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let conf_dir = dir.path().join("etc/portage");
        std::fs::create_dir_all(&conf_dir).unwrap();
        std::fs::write(
            conf_dir.join("make.conf"),
            "COMMON_FLAGS=\"-O2 -march=x86-64-v3\"\nCFLAGS=\"${COMMON_FLAGS}\"\n",
        )
        .unwrap();

        let cli = Cli::parse_from(["em", "--root", root, "--config-root", root]);
        assert_eq!(
            read_make_conf_var_for_roots(&cli.roots(), "CFLAGS")
                .await
                .as_deref(),
            Some("-O2 -march=x86-64-v3")
        );
    }

    #[tokio::test]
    async fn desired_build_env_for_roots_reads_expanded_flags() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let conf_dir = dir.path().join("etc/portage");
        std::fs::create_dir_all(&conf_dir).unwrap();
        std::fs::write(
            conf_dir.join("make.conf"),
            "COMMON_FLAGS=\"-O2 -march=x86-64-v3\"\nCFLAGS=\"${COMMON_FLAGS}\"\n",
        )
        .unwrap();

        let cli = Cli::parse_from(["em", "--root", root, "--config-root", root]);
        let env = DesiredBuildEnv::for_roots(&cli.roots()).await;
        assert_eq!(env.cflags, "-O2 -march=x86-64-v3");
        let key = env.key();
        assert!(!key.is_empty());
        assert_eq!(
            key,
            portage_binpkg::build_env_key("-O2 -march=x86-64-v3", "", "", "")
        );
    }

    /// Under `--target`, the sysroot's own config (`roots()`) and the host's
    /// (`broot()`) can have genuinely different make.conf CFLAGS — the
    /// fingerprint command's `--host` flag exists exactly for this split.
    #[tokio::test]
    async fn desired_build_env_host_vs_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("etc/portage")).unwrap();
        std::fs::write(
            dir.path().join("etc/portage/make.conf"),
            "CFLAGS=\"-O2 -march=armv8-a\"\n",
        )
        .unwrap();
        let sysroot_conf = dir.path().join("usr/riscv64-unknown-linux-gnu/etc/portage");
        std::fs::create_dir_all(&sysroot_conf).unwrap();
        std::fs::write(
            sysroot_conf.join("make.conf"),
            "CFLAGS=\"-O2 -march=rv64gcv\"\n",
        )
        .unwrap();

        let cli = Cli::parse_from([
            "em",
            "--root",
            root,
            "--config-root",
            root,
            "--target",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);

        let target_env = DesiredBuildEnv::for_roots(&cli.roots()).await;
        let host_env = DesiredBuildEnv::for_roots(&cli.broot()).await;
        assert_eq!(target_env.cflags, "-O2 -march=rv64gcv");
        assert_eq!(host_env.cflags, "-O2 -march=armv8-a");
        assert_ne!(target_env.key(), host_env.key());
    }

    /// Build a `--root R --config-root R` `Cli` with `R/etc/portage/make.conf`
    /// (given CFLAGS) plus, optionally, `package.env` and its named env files
    /// under that same `etc/portage` dir — the layout `env_files_for` (and
    /// the real build path) actually reads.
    fn cli_with_make_conf_and_package_env(
        dir: &std::path::Path,
        make_conf_cflags: &str,
        package_env: &str,
        env_files: &[(&str, &str)],
    ) -> Cli {
        let conf_dir = dir.join("etc/portage");
        std::fs::create_dir_all(&conf_dir).unwrap();
        std::fs::write(
            conf_dir.join("make.conf"),
            format!("CFLAGS=\"{make_conf_cflags}\"\n"),
        )
        .unwrap();
        if !package_env.is_empty() {
            std::fs::write(conf_dir.join("package.env"), package_env).unwrap();
            std::fs::create_dir_all(conf_dir.join("env")).unwrap();
            for (name, body) in env_files {
                std::fs::write(conf_dir.join("env").join(name), body).unwrap();
            }
        }
        let root = dir.to_str().unwrap();
        Cli::parse_from(["em", "--root", root, "--config-root", root])
    }

    #[tokio::test]
    async fn key_for_override_replaces_march() {
        let dir = tempfile::tempdir().unwrap();
        let cli = cli_with_make_conf_and_package_env(
            dir.path(),
            "-O2 -march=x86-64-v2",
            "dev-libs/foo  march_b\n",
            &[("march_b", "CFLAGS=\"-O2 -march=x86-64-v3\"\n")],
        );

        let roots = cli.roots();
        let env = DesiredBuildEnv::for_roots(&roots).await;
        let dirs = DesiredBuildEnv::portage_dirs(&roots);

        let foo = portage_atom::Cpv::parse("dev-libs/foo-1.0").unwrap();
        let bar = portage_atom::Cpv::parse("dev-libs/bar-1.0").unwrap();

        assert_eq!(
            env.key_for(&dirs, &foo).await,
            portage_binpkg::build_env_key("-O2 -march=x86-64-v3", "", "", "")
        );
        assert_ne!(env.key_for(&dirs, &foo).await, env.key());
        assert_eq!(
            env.key_for(&dirs, &bar).await,
            env.key(),
            "an unmatched package falls back to the make.conf-only key"
        );
    }

    #[tokio::test]
    async fn key_for_append_keeps_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let cli = cli_with_make_conf_and_package_env(
            dir.path(),
            "-O2 -march=armv8-a",
            "dev-libs/foo  outline_atomics\n",
            &[(
                "outline_atomics",
                "CFLAGS=\"${CFLAGS} -mno-outline-atomics\"\n",
            )],
        );

        let roots = cli.roots();
        let env = DesiredBuildEnv::for_roots(&roots).await;
        let dirs = DesiredBuildEnv::portage_dirs(&roots);
        let foo = portage_atom::Cpv::parse("dev-libs/foo-1.0").unwrap();

        assert_eq!(
            env.key_for(&dirs, &foo).await,
            portage_binpkg::build_env_key("-O2 -march=armv8-a -mno-outline-atomics", "", "", "")
        );
        assert_ne!(env.key_for(&dirs, &foo).await, env.key());
    }

    #[tokio::test]
    async fn key_for_no_matching_env_file_is_baseline_key() {
        let dir = tempfile::tempdir().unwrap();
        let cli = cli_with_make_conf_and_package_env(dir.path(), "-O2 -march=x86-64-v3", "", &[]);
        let roots = cli.roots();
        let env = DesiredBuildEnv::for_roots(&roots).await;
        let dirs = DesiredBuildEnv::portage_dirs(&roots);
        let foo = portage_atom::Cpv::parse("dev-libs/foo-1.0").unwrap();
        assert_eq!(env.key_for(&dirs, &foo).await, env.key());
    }

    fn parse_sections(
        contents: &str,
    ) -> (
        std::collections::HashMap<String, std::collections::HashMap<String, String>>,
        Vec<String>,
    ) {
        let mut sections = std::collections::HashMap::new();
        let mut order = Vec::new();
        portage_repo::ini::merge_sections(&mut sections, &mut order, contents);
        (sections, order)
    }

    fn uris(entries: &[BinRepoEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.sync_uri.as_str()).collect()
    }

    /// The two reversals in real portage's own algorithm (`BinRepoConfigLoader`
    /// assigns increasing priority walking `PORTAGE_BINHOST` *backwards*;
    /// `bintree.py` then consumes the whole sorted list *reversed*) cancel out
    /// for a plain `PORTAGE_BINHOST` with no `binrepos.conf` at all — verified
    /// against the real source, not assumed (see `binrepo/config.py` +
    /// `dbapi/bintree.py`).
    #[test]
    fn plain_portage_binhost_preserves_original_order() {
        let (sections, order) = parse_sections("");
        let result = combine_binhosts(&sections, &order, Some("A B C"));
        assert_eq!(uris(&result), vec!["A", "B", "C"]);
    }

    /// A higher `priority =` in `binrepos.conf` is tried *first* (ascending
    /// sort, then reversed for consumption — a higher number sorts later
    /// ascending, so ends up first after the reversal).
    #[test]
    fn binrepos_conf_priority_higher_number_tried_first() {
        let (sections, order) = parse_sections(
            "[low]\nsync-uri = http://low\npriority = 1\n\n\
             [high]\nsync-uri = http://high\npriority = 10\n",
        );
        let result = combine_binhosts(&sections, &order, None);
        assert_eq!(uris(&result), vec!["http://high", "http://low"]);
    }

    /// Explicit `binrepos.conf` sections (priority defaults to 0) and legacy
    /// `PORTAGE_BINHOST` entries (always priority >= 1) combine correctly:
    /// the `PORTAGE_BINHOST` entries outrank the unprioritized section.
    #[test]
    fn binrepos_conf_and_portage_binhost_combine() {
        let (sections, order) = parse_sections("[mine]\nsync-uri = http://mine\n");
        let result = combine_binhosts(&sections, &order, Some("http://a http://b"));
        // http://a and http://b (priority 2 and 1 respectively, per the
        // reversed-walk rule) outrank the unprioritized (priority 0) `mine`.
        assert_eq!(uris(&result), vec!["http://a", "http://b", "http://mine"]);
    }

    /// A `PORTAGE_BINHOST` URL already covered by an explicit `binrepos.conf`
    /// section is not duplicated.
    #[test]
    fn duplicate_sync_uri_is_not_added_twice() {
        let (sections, order) = parse_sections("[mine]\nsync-uri = http://dup\npriority = 5\n");
        let result = combine_binhosts(&sections, &order, Some("http://dup http://new"));
        assert_eq!(result.len(), 2);
        assert_eq!(uris(&result), vec!["http://dup", "http://new"]);
    }

    /// A section with no `sync-uri` is skipped entirely (matching real
    /// portage's own warn-and-skip behaviour), not merged with a blank URI.
    #[test]
    fn missing_sync_uri_is_skipped() {
        let (sections, order) = parse_sections("[broken]\npriority = 1\n");
        let result = combine_binhosts(&sections, &order, None);
        assert!(result.is_empty());
    }

    #[test]
    fn frozen_and_verify_signature_parsed_case_insensitively() {
        let (sections, order) =
            parse_sections("[a]\nsync-uri = http://a\nfrozen = True\nverify-signature = yes\n");
        let result = combine_binhosts(&sections, &order, None);
        assert_eq!(result.len(), 1);
        assert!(result[0].frozen);
        assert!(result[0].verify_signature);
    }

    #[test]
    fn frozen_and_verify_signature_default_false() {
        let (sections, order) = parse_sections("[a]\nsync-uri = http://a\n");
        let result = combine_binhosts(&sections, &order, None);
        assert_eq!(result.len(), 1);
        assert!(!result[0].frozen);
        assert!(!result[0].verify_signature);
    }

    /// Exercises the real `portage_binhosts` entry point end-to-end against a
    /// real file on disk (not just `combine_binhosts`'s pure core): a real
    /// `--root`, a real `etc/portage/binrepos.conf` file, real
    /// `collect_conf_files`/`merge_sections` I/O.
    #[tokio::test]
    async fn portage_binhosts_reads_a_real_binrepos_conf_file() {
        assert!(
            std::env::var("PORTAGE_BINHOST").is_err(),
            "test assumes no ambient PORTAGE_BINHOST override"
        );
        let dir = tempfile::tempdir().unwrap();
        let portage_dir = dir.path().join("etc/portage");
        std::fs::create_dir_all(&portage_dir).unwrap();
        std::fs::write(
            portage_dir.join("binrepos.conf"),
            "[myhost]\nsync-uri = https://example.invalid/binhost\npriority = 3\n",
        )
        .unwrap();

        // `config()` defaults to the real host `/` for a bare `--root`
        // (portage `ROOT=`/`PORTAGE_CONFIGROOT` parity — see
        // `base_roots()`'s doc comment); `--config-root` is required here so
        // this test reads only the tempdir's own file, never the real host's
        // `/etc/portage/binrepos.conf`.
        let root = dir.path().to_str().unwrap();
        let cli = Cli::parse_from(["em", "--root", root, "--config-root", root]);
        let result = portage_binhosts(&cli).await;
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "myhost");
        assert_eq!(result[0].sync_uri, "https://example.invalid/binhost");
    }
}
