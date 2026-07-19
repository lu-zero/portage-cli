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
pub(crate) fn resolve_pkgdir(globals: &Cli) -> Utf8PathBuf {
    resolve_pkgdir_for_roots(&globals.roots())
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
pub(crate) fn resolve_pkgdir_for_roots(roots: &Roots) -> Utf8PathBuf {
    if let Ok(v) = std::env::var("PKGDIR")
        && !v.trim().is_empty()
    {
        return Utf8PathBuf::from(v);
    }
    if let Some(v) = read_make_conf_var_for_roots(roots, "PKGDIR")
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
pub(crate) fn open_local_index_for_preview(
    globals: &Cli,
    merge_flags: &crate::cli::MergeFlags,
) -> Option<portage_binpkg::BinpkgIndex> {
    if !(merge_flags.usepkg || merge_flags.usepkgonly) {
        return None;
    }
    portage_binpkg::BinpkgIndex::open(resolve_pkgdir(globals).as_std_path()).ok()
}

/// Read a variable from `make.conf` under the resolved config root.
pub(crate) fn read_make_conf_var(globals: &Cli, var: &str) -> Option<String> {
    read_make_conf_var_for_roots(&globals.roots(), var)
}

/// Read a variable from make.conf under specific roots — used to get
/// per-entry CHOST/CFLAGS/etc. for cross-compilation scenarios, where the
/// desired config root isn't `globals`'s own (e.g. a `MergeRoot::Host` entry
/// under `--target`).
pub(crate) fn read_make_conf_var_for_roots(roots: &Roots, var: &str) -> Option<String> {
    let cfg_root = roots
        .config()
        .map(|c| c.to_path_buf())
        .unwrap_or_else(|| Utf8PathBuf::from("/"));
    for rel in ["etc/portage/make.conf", "etc/make.conf"] {
        let p = cfg_root.join(rel);
        if p.exists()
            && let Ok(mc) = MakeConf::load(&p)
            && let Some(v) = mc.get(var).filter(|s| !s.is_empty())
        {
            return Some(v.to_owned());
        }
    }
    None
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
pub(crate) fn portage_binhosts(globals: &Cli) -> Vec<BinRepoEntry> {
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
        .filter(|v| !v.trim().is_empty())
        .or_else(|| read_make_conf_var(globals, "PORTAGE_BINHOST").filter(|v| !v.is_empty()));

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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Regression test for the stage3 --buildpkg failure: a non-host root
    /// must never default PKGDIR to the real system's `/var/cache/binpkgs`
    /// (root-owned, not writable, and not even meaningful for a different
    /// root's package cache) — see `resolve_pkgdir`'s doc comment.
    #[test]
    fn non_host_root_gets_root_relative_pkgdir_default() {
        assert!(
            std::env::var("PKGDIR").is_err(),
            "test assumes no ambient PKGDIR override"
        );
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let cli = Cli::parse_from(["em", "--root", root]);
        let pkgdir = resolve_pkgdir(&cli);
        assert_eq!(
            pkgdir,
            camino::Utf8Path::new(root).join("var/cache/binpkgs")
        );
    }

    /// A plain host build (root `/`, no --root/--prefix/--local/--target) is
    /// unaffected by the root-aware branch — it still falls through to the
    /// pre-existing make.globals/hardcoded-default lookup, exactly as before
    /// this change.
    #[test]
    fn host_root_skips_the_root_relative_branch() {
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
        assert_eq!(resolve_pkgdir(&cli), expected);
    }

    /// A `--target` plan's own roots (`resolve_pkgdir`) resolve under the
    /// target sysroot, while `broot()` (host roots, what a `MergeRoot::Host`
    /// entry actually wants) resolve as a plain host build — the two must
    /// disagree here, or a Host BDEPEND entry would look up binpkgs in the
    /// wrong PKGDIR (S1/S4 in `todo/binpkg-subtargets.md`).
    #[test]
    fn resolve_pkgdir_for_roots_target_vs_host() {
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
            resolve_pkgdir_for_roots(&target_roots),
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
        assert_eq!(resolve_pkgdir_for_roots(&host_roots), expected_host);
    }

    /// Config-root make.conf `PKGDIR=` wins for whichever roots see that
    /// config root — proven independently for target and host roots.
    #[test]
    fn resolve_pkgdir_for_roots_honours_config_root_make_conf() {
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
            resolve_pkgdir_for_roots(&cli.roots()),
            camino::Utf8Path::new("/custom/pkgdir")
        );
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
    #[test]
    fn portage_binhosts_reads_a_real_binrepos_conf_file() {
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
        let result = portage_binhosts(&cli);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "myhost");
        assert_eq!(result[0].sync_uri, "https://example.invalid/binhost");
    }
}
