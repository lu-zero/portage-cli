//! Root-aware merge planning for cross-compilation (crossdev / `{target}-emerge`).
//!
//! Stage 3b: dual `(package, merge_root)` solver nodes live in
//! `portage-atom-pubgrub`; this module handles cross-context detection and
//! plan/output glue.

use camino::{Utf8Path, Utf8PathBuf};
use gentoo_core::Arch;
use portage_atom::Version;
use portage_atom_pubgrub::{MergeRoot, PortagePackage};

use crate::Roots;

/// Cross-compilation context derived from CLI roots.
///
/// The single owner of "is this a cross build, and how" for the resolver: the
/// derived predicates ([`is_cross_arch`](Self::is_cross_arch),
/// [`target_arch`](Self::target_arch)) are computed here so call sites don't
/// each re-derive them from `chost`. `--root-deps=rdeps` is deliberately NOT
/// derived here — it's a property of which *operation* is running (`em
/// crossdev --setup` vs `em stages --stage1`), not of the sysroot's
/// CHOST/CBUILD; see `DepgraphOpts::root_deps_rdeps`.
#[derive(Debug, Clone)]
pub struct CrossContext {
    /// Whether dual-root cross planning is active for this invocation (any
    /// of: config≠install root, foreign target arch, or BROOT genuinely
    /// differing from the target — true for `--root`/`--prefix`/cross,
    /// false for bare and `--local` since BROOT equals the target there).
    pub active: bool,
    /// `ESYSROOT` / `PORTAGE_CONFIGROOT`: where `DEPEND` is resolved.
    pub sysroot: Utf8PathBuf,
    /// `ROOT` / `EROOT`: where target packages install.
    pub target: Utf8PathBuf,
    /// Target `CHOST` from the profile `make.conf` (if readable).
    pub chost: Option<String>,
    /// Host `CBUILD` from the profile `make.conf` (if readable).
    pub cbuild: Option<String>,
    /// Gentoo keyword `ARCH` of the target `CHOST` (e.g. `riscv`), when `active`
    /// and the `CHOST` maps to a known arch. Derived once; drives keyword
    /// acceptance for the target.
    target_arch: Option<Arch>,
    /// Where a `MergeRoot::Host` entry actually lands (mirrors `Cli::host_roots()`):
    /// the prefix under `--prefix` (an unprivileged overlay can't write the
    /// real host `/`), else the real host `/`. Used by [`display_root`] so
    /// the `-p` merge list matches where the merge actually goes.
    host_target: Utf8PathBuf,
}

impl CrossContext {
    /// `true` when the target profile declares a different machine than the
    /// host. When CHOST/CBUILD can't be read (no sysroot config yet, or a
    /// same-arch offset that never declares them), default to same-arch —
    /// NOT `sysroot != "/"`, which used to treat *any* non-host sysroot
    /// (including a plain same-arch `--root <dir>`) as foreign-arch. Mirrors
    /// `detect()`'s own `cross_arch` local (which already used `_ => false`).
    /// Treating any non-host sysroot as foreign wrongly pins DEPEND to the
    /// empty target for same-arch `--root` (huge false plan vs real emerge).
    pub fn is_cross_arch(&self) -> bool {
        match (self.chost.as_deref(), self.cbuild.as_deref()) {
            (Some(c), Some(b)) => c != b,
            _ => false,
        }
    }

    /// The target keyword arch (from `CHOST`), if this is an active cross build
    /// to a recognised arch. Used to accept the target's keywords instead of the
    /// host `--arch`.
    pub fn target_arch(&self) -> Option<&Arch> {
        self.target_arch.as_ref()
    }
}

/// Detect cross context from CLI roots (no flag required). `host_merge_root`
/// is `Cli::host_roots()`'s `merge_root()` — passed in rather than derived from
/// `roots.is_overlay()` here, because `roots` can be `--target`-substituted
/// (its `eprefix`/overlay-ness cleared), which would wrongly report the real
/// host as the destination for a `MergeRoot::Host` entry even under an
/// unprivileged `--prefix` overlay (`Cli::host_roots()` stays overlay-aware
/// regardless of `--target`, since it's derived from `base_roots()`).
pub fn detect(roots: &Roots, host_merge_root: &Utf8Path) -> CrossContext {
    let sysroot = roots
        .sysroot()
        .map(|p| p.to_owned())
        .unwrap_or_else(|| Utf8PathBuf::from("/"));
    let target = roots.merge_root().to_owned();
    let dual_root = sysroot.as_str() != target.as_str();
    // Dual-root solver bookkeeping ((package, merge_root) nodes, BROOT-
    // satisfaction dropping, host_copies' post-solve walk) is needed only
    // when BROOT is genuinely a *different filesystem* from the target —
    // true for `--root`/`--prefix`/cross (BROOT stays the real, already-
    // populated host, or a real sysroot, while the target moves), false for
    // both the bare invocation (broot == target == `/`) and `--local`
    // (broot == target == the same prefix — structurally the same
    // single-root shape as bare, just at a different path). Replaced the
    // old `target != "/"` test (2026-07-16): that made `--local` spuriously
    // "active", engaging `host_copies`'s Tier-1 (already-populated-host)
    // machinery against a `--local` prefix's own, initially-empty BROOT —
    // it can't find *anything* satisfied there and ends up inserting a
    // parallel `@Host` copy of nearly the whole closure alongside the
    // regular Target one, duplicate entries preflight then rejects the
    // order of. `roots.broot() == None` means "trivially equals
    // merge_root" (see `Roots::broot`'s own doc), so that case reads as
    // "doesn't differ" correctly too.
    let broot_differs = roots.broot().is_some_and(|b| b.as_str() != target.as_str());
    let (chost, cbuild) = read_chost_cbuild(&sysroot);
    let cross_arch = match (chost.as_deref(), cbuild.as_deref()) {
        (Some(c), Some(b)) => c != b,
        _ => false,
    };
    let host_target = host_merge_root.to_owned();

    if !dual_root && !cross_arch && !broot_differs {
        // Populate sysroot/target truthfully even when inactive (unlike the
        // old hardcoded "/"): `--local`'s own `-p` display must still show
        // ` to <prefix>/`, not silently collapse to the bare host's `/`
        // (`display_root`'s Target arm no longer special-cases `active`
        // either, for the same reason).
        return CrossContext {
            active: false,
            sysroot,
            target,
            chost: None,
            cbuild: None,
            target_arch: None,
            host_target,
        };
    }

    let target_arch = chost.as_deref().and_then(Arch::from_chost);
    CrossContext {
        active: true,
        sysroot,
        target,
        chost,
        cbuild,
        target_arch,
        host_target,
    }
}

/// One line of the merge list with an explicit merge destination.
#[derive(Debug, Clone)]
pub struct PlanEntry {
    /// The solved package identity.
    pub pkg: PortagePackage,
    /// The version to merge.
    pub version: Version,
    /// Where it merges (host BROOT or the target).
    pub merge_root: MergeRoot,
}

/// Map solver install order to plan entries (merge root from solver identity).
pub fn build_plan(target_order: Vec<(PortagePackage, Version)>) -> Vec<PlanEntry> {
    target_order
        .into_iter()
        .map(|(pkg, ver)| PlanEntry {
            merge_root: pkg.merge_root(),
            pkg,
            version: ver,
        })
        .collect()
}

/// Display path for emerge-style ` to <path>/` annotations. `target` is
/// truthful (the real merge destination) regardless of `cross.active` now —
/// see `detect()`'s doc comment on why the inactive case (bare host,
/// `--local`) still needs a real path here, not a hardcoded `/`.
pub fn display_root<'a>(
    merge_root: MergeRoot,
    target: &'a Utf8Path,
    cross: &'a CrossContext,
) -> &'a Utf8Path {
    match merge_root {
        MergeRoot::Host => cross.host_target.as_path(),
        MergeRoot::Target => target,
    }
}

fn read_chost_cbuild(root: &Utf8Path) -> (Option<String>, Option<String>) {
    let var =
        |mc: &portage_repo::MakeConf, k| mc.get(k).filter(|s| !s.is_empty()).map(str::to_owned);
    for rel in ["etc/portage/make.conf", "etc/make.conf"] {
        if let Ok(mc) = portage_repo::MakeConf::load(&root.join(rel)) {
            let (chost, cbuild) = (var(&mc, "CHOST"), var(&mc, "CBUILD"));
            if chost.is_some() || cbuild.is_some() {
                return (chost, cbuild);
            }
        }
    }
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--prefix`: a `MergeRoot::Host` entry must display as landing in the
    /// prefix, not the real host — matching `Cli::host_roots()`'s merge
    /// destination fix. Before that fix `display_root` hardcoded `/` here,
    /// which stayed silently correct only because `Cli::host_roots()` itself
    /// used to be host-anchored for every topology.
    #[test]
    fn host_entry_displays_as_landing_in_the_prefix_under_overlay() {
        let roots = crate::Roots::for_test_overlay("/", "/opt/p");
        let cross = detect(&roots, Utf8Path::new("/opt/p"));
        assert!(cross.active);
        assert_eq!(
            display_root(MergeRoot::Host, &cross.target, &cross).as_str(),
            "/opt/p"
        );
    }

    /// `--root`: a `MergeRoot::Host` entry still displays as landing on the
    /// real host `/` — unaffected by the overlay-only display fix. Uses
    /// `for_test_root_with_broot` (BROOT genuinely separate from the
    /// offset) — `for_test` alone is `--local`-shaped (BROOT == target),
    /// not `--root`-shaped; see the tests below for that distinction.
    #[test]
    fn host_entry_displays_as_landing_on_the_real_host_under_offset() {
        let roots = crate::Roots::for_test_root_with_broot("/srv/x", "/");
        let cross = detect(&roots, Utf8Path::new("/"));
        assert!(cross.active, "--root: BROOT differs from the target");
        assert_eq!(
            display_root(MergeRoot::Host, &cross.target, &cross).as_str(),
            "/"
        );
    }

    /// `--local`: BROOT == target (single root). Dual-root bookkeeping must
    /// not engage against an empty prefix BROOT. `sysroot`/`target` still
    /// report the real path so `-p` shows ` to <prefix>/`.
    #[test]
    fn local_shaped_roots_are_not_active_but_still_report_the_real_target() {
        let roots = crate::Roots::for_test("/root/local-test");
        let cross = detect(&roots, Utf8Path::new("/root/local-test"));
        assert!(!cross.active, "--local: BROOT equals the target");
        assert_eq!(cross.target.as_str(), "/root/local-test");
        assert_eq!(cross.sysroot.as_str(), "/root/local-test");
    }

    /// The bare invocation (broot == target == `/`) stays inactive, as
    /// before — the new `broot_differs` predicate must not regress it.
    #[test]
    fn bare_invocation_is_not_active() {
        let roots = crate::Roots::default();
        let cross = detect(&roots, Utf8Path::new("/"));
        assert!(!cross.active);
    }

    /// The combined `--prefix --target` case: `roots` here would be
    /// `--target`-substituted (eprefix cleared), but `host_merge_root` is
    /// passed independently (from `Cli::host_roots()`, unaffected by that
    /// substitution) — the whole point of not deriving `host_target` from
    /// `roots.is_overlay()` inside `detect`.
    #[test]
    fn host_entry_displays_as_landing_in_the_prefix_even_when_roots_is_target_substituted() {
        let sysroot_roots = crate::Roots::for_test("/opt/p/usr/riscv64-unknown-linux-gnu");
        let cross = detect(&sysroot_roots, Utf8Path::new("/opt/p"));
        assert_eq!(
            display_root(MergeRoot::Host, &cross.target, &cross).as_str(),
            "/opt/p"
        );
    }
}
