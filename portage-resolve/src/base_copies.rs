//! Toolchain-sysroot build copies (the board-root topology)
//!
//! `em --target T --root R` deliberately decouples the shared, expensive
//! crossdev toolchain sysroot (`/usr/T`, `Roots::base()`) from a disposable
//! board root `R` (`Roots::merge_root()`) — reusing one toolchain build
//! across many board destinations instead of rebuilding it per board like
//! real crossdev effectively does. PMS table 8.2 then bites: a target
//! package's `RDEPEND` is satisfied against `EROOT` (= `R`), but its
//! `DEPEND` — the headers, libs and `.pc` files it *compiles and links
//! against* — is satisfied against `SYSROOT` (= `/usr/T`). Those are
//! different filesystems here for the first time, so a `DEPEND` provider
//! merged only into `R` is invisible to the compiler.
//!
//! Real Portage + real crossdev, measured against a freshly built (verified
//! empty-VDB) toolchain sysroot:
//! `ROOT=/realtarget i586-pc-linux-gnu-emerge -b -k sys-libs/ncurses sys-libs/readline`
//! produces **three** entries — `ncurses to /realtarget/`, `ncurses to
//! /usr/i586-pc-linux-gnu/`, `readline to /realtarget/` — and `readline`
//! then builds. This module reproduces the middle one.
//!
//! Computed as a post-solve closure walk for the same reason
//! [`crate::host_copies`] is (see its doc comment on the Tier 1 aliasing
//! blocker): the solver already aliases every Target package to a Host
//! twin, and a third root would triple the pubgrub package universe. The
//! Target plan is additively extended, never rewritten.
//!
//! Deliberately a sibling module, not a mode flag on `host_copies`: the two
//! walks differ on every axis that matters — the gate
//! ([`crate::Roots::base_merge_root`] vs. native-offset `cross.active`),
//! the availability seed (the base VDB alone, via
//! [`crate::Avail::initial_base_depend`], vs. the host/prefix weave), the
//! seed dependency class (`DEPEND` only, vs. `DEPEND`+`BDEPEND`+`IDEPEND`),
//! the recursion classes (`DEPEND`+`RDEPEND`, vs. the same three as the
//! seed), the stamp (`Base` vs. `Host`), and the arch of the copy (target
//! arch, same as the Target entry, vs. host arch). `RDEPEND` is followed on
//! recursion — not just `DEPEND` — because a copy's own shared libraries
//! need *their* runtime deps present in the same sysroot for the linker to
//! resolve them, one level down from the direct compile-time need.

use std::collections::{HashMap, HashSet};

use portage_atom::{Cpn, Cpv, Version};
use portage_atom_pubgrub::{MergeRoot, PortagePackage};

use crate::Roots;
use crate::{Avail, unsatisfied_cpns};

use crate::effective_use;
use crate::repo::Adapter;

/// Static inputs shared across the walk
struct Ctx<'a> {
    adapter: &'a Adapter<'a>,
    target_ver: &'a HashMap<Cpn, (Version, PortagePackage)>,
}

/// Mutable walk state: sysroot availability (VDB + already-planned `Base`
/// entries + emitted copies) and the seen-set (also breaks dependency cycles)
struct Walk {
    avail: Avail,
    seen: HashSet<Cpn>,
}

/// Compute the finalized plan order for the board-root topology (`--target T
/// --root R`), inserting toolchain-sysroot (`MergeRoot::Base`) `DEPEND`
/// copies immediately before whichever Target entry first needs each one
///
/// Returns `target_order` unchanged whenever [`Roots::base_merge_root`] is
/// `None` — every topology except the board-root one.
pub fn compute(
    target_order: &[(PortagePackage, Version)],
    adapter: &Adapter<'_>,
    roots: &Roots,
) -> Vec<(PortagePackage, Version)> {
    if roots.base_merge_root().is_none() {
        return target_order.to_vec();
    }

    // CPN -> (version, target package) for version reuse: a sysroot copy of
    // a package also built for Target uses the Target version. Both copies
    // satisfy the same atom for the same consumers in the same run — a
    // version split would silently break the `:=` subslot invariant PMS
    // relies on, and real Portage's control run picked one version and
    // used it at both roots.
    let target_ver: HashMap<Cpn, (Version, PortagePackage)> = target_order
        .iter()
        .filter(|(p, _)| p.merge_root() == MergeRoot::Target)
        .map(|(p, v)| (*p.cpn(), (v.clone(), p.clone())))
        .collect();
    let ctx = Ctx {
        adapter,
        target_ver: &target_ver,
    };

    // Sysroot availability starts as its own DEPEND view (the base VDB
    // alone — never the target's, see `initial_base_depend`'s own doc
    // comment) and grows with each copy merged here.
    let mut walk = Walk {
        seen: HashSet::new(),
        avail: Avail::initial_base_depend(roots),
    };

    // Seed with whatever `MergeRoot::Base` entries might already be in
    // `target_order` — the solver never emits any today (see
    // `portage_solver::MergeRoot::Base`'s doc comment), but seeding
    // defensively means a future solver change can't cause this walk to
    // silently double-schedule, the same lesson `host_copies` learned from
    // the `dev-perl/Digest-HMAC` duplicate incident.
    for (pkg, ver) in target_order
        .iter()
        .filter(|(p, _)| p.merge_root() == MergeRoot::Base)
    {
        walk.seen.insert(*pkg.cpn());
        walk.avail
            .record_merge(Cpv::new(*pkg.cpn(), ver.clone()), MergeRoot::Base);
    }

    // Interleave: walk target_order in its existing order, and for each
    // Target entry, insert its not-yet-emitted sysroot DEPEND copies
    // (deps-first, recursively) immediately before it, then emit the entry
    // itself. Base entries pass through unchanged (already seeded above).
    let mut order: Vec<(PortagePackage, Version)> = Vec::with_capacity(target_order.len());
    for (pkg, ver) in target_order {
        if pkg.merge_root() == MergeRoot::Target {
            visit_unsatisfied(&ctx, &mut walk, pkg, ver, &mut order, true);
        }
        order.push((pkg.clone(), ver.clone()));
    }
    order
}

/// Recurse into `pkg`'s unsatisfied-in-the-sysroot `DEPEND` edges (top
/// level) or `DEPEND`+`RDEPEND` edges (recursing into an already-found
/// copy's own closure)
///
/// Appends each resolved sysroot copy to `order` only *after* its own edges
/// have been visited — deps-first, so a copy never lands before something
/// it needs.
///
/// Called just before `pkg` itself is pushed to `order`, so every copy
/// discovered here also ends up immediately before `pkg` — its first (and
/// closure-wide) consumer.
fn visit_unsatisfied(
    ctx: &Ctx<'_>,
    walk: &mut Walk,
    pkg: &PortagePackage,
    ver: &Version,
    order: &mut Vec<(PortagePackage, Version)>,
    top_level: bool,
) {
    let Some(deps) =
        effective_use::evaluated_deps(ctx.adapter.data, &ctx.adapter.policy(), pkg, ver, false)
    else {
        return;
    };
    // Top level: DEPEND only (what a Target entry directly compiles/links
    // against). Recursing into a copy's own closure: DEPEND + RDEPEND too —
    // a copy's shared library needs its own runtime deps present in the
    // same sysroot for the linker to resolve them.
    let mut entries = deps.depend();
    if !top_level {
        entries.extend(deps.rdepend());
    }
    for cpn in unsatisfied_cpns(&entries, &walk.avail) {
        if !walk.seen.insert(cpn) {
            continue;
        }
        let Some((cver, cpkg)) = resolve(cpn, ctx) else {
            continue;
        };
        let base_pkg = cpkg.at_merge_root(MergeRoot::Base);
        visit_unsatisfied(ctx, walk, &base_pkg, &cver, order, false);
        walk.avail
            .record_merge(Cpv::new(cpn, cver.clone()), MergeRoot::Base);
        order.push((base_pkg, cver));
    }
}

/// Resolve `(version, package)` for a sysroot copy of `cpn`
///
/// The Target plan's version when the CPN is also built for Target, else
/// the newest keyword/mask/license-accepted repo version (target-arch
/// accepted — `ctx.adapter`'s `accept_keywords` is already scoped to the
/// target, matching the CPN it's resolving for).
///
/// `None` when the CPN is absent from the repo or has no accepted version.
fn resolve(cpn: Cpn, ctx: &Ctx<'_>) -> Option<(Version, PortagePackage)> {
    if let Some((v, p)) = ctx.target_ver.get(&cpn) {
        return Some((v.clone(), p.clone()));
    }
    let (cpv, cache) = ctx.adapter.newest_accepted(cpn)?;
    let slot = &cache.metadata.slot.slot;
    let pkg = if slot.as_str() == "0" {
        PortagePackage::unslotted(cpn)
    } else {
        PortagePackage::slotted(cpn, *slot)
    };
    Some((cpv.version.clone(), pkg))
}

#[cfg(test)]
mod tests {
    fn empty_layer() -> &'static portage_atom_pubgrub::UseLayer {
        use std::sync::OnceLock;
        static E: OnceLock<portage_atom_pubgrub::UseLayer> = OnceLock::new();
        E.get_or_init(portage_atom_pubgrub::UseLayer::default)
    }

    use portage_metadata::CacheEntry;
    use portage_repo::{AcceptSet, LicenseGroupRegistry};

    use super::*;
    use crate::force_mask::ForceMask;
    use crate::repo::{self, AcceptKeywords, AcceptOverlay, AcceptProperties, AcceptRestrict};

    fn accept_all_licenses() -> AcceptSet {
        AcceptSet::from_tokens(&["*".into()], &LicenseGroupRegistry::default())
    }

    fn repo_from(entries: &[(&str, &str)]) -> repo::RepoData {
        let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
        let mut cpns = Vec::new();
        for (cpv_str, text) in entries {
            let cpv = Cpv::parse(cpv_str).unwrap();
            let entry = CacheEntry::parse(text).unwrap();
            cpns.push(cpv.cpn);
            versions.entry(cpv.cpn).or_default().push((cpv, entry));
        }
        repo::RepoData {
            cpns,
            versions,
            repo_name: "test".into(),
            repo_of: HashMap::new(),
            real_cpn_of: HashMap::new(),
        }
    }

    fn write_fake_vdb_entry(root: &std::path::Path, cpv: &str) {
        let pkg_dir = root.join("var/db/pkg").join(cpv);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("EAPI"), "8").unwrap();
        std::fs::write(pkg_dir.join("SLOT"), "0").unwrap();
        std::fs::write(pkg_dir.join("CONTENTS"), "").unwrap();
        std::fs::write(pkg_dir.join("USE"), "").unwrap();
    }

    /// Bind `$name: Adapter` for the rest of the caller's scope.
    /// Statement-position (not a block-expression macro or function): every
    /// `Adapter` field is a borrow, so its backing `let`s must share the
    /// caller's own lifetime, not a nested block's or a helper function's.
    macro_rules! test_adapter {
        ($name:ident, $data:expr) => {
            let arch = gentoo_core::Arch::intern("x86");
            let accept_keywords = AcceptKeywords::from_global(&arch, &["x86"]);
            let accept_licenses = AcceptOverlay::new(accept_all_licenses(), Vec::new());
            let accept_properties = AcceptProperties::new(accept_all_licenses(), Vec::new());
            let accept_restrict = AcceptRestrict::new(accept_all_licenses(), Vec::new());
            let force_mask = ForceMask::default();
            let installed_cpvs = HashSet::new();
            let rebuilding_cpvs = HashSet::new();
            let $name = Adapter {
                data: $data,
                accept_keywords: &accept_keywords,
                package_mask: &[],
                package_unmask: &[],
                accept_licenses: &accept_licenses,
                accept_properties: &accept_properties,
                accept_restrict: &accept_restrict,
                defaults: empty_layer(),
                conf: empty_layer(),
                env_use: empty_layer(),
                package_use: &[],
                profile_package_use: &[],
                force_mask: &force_mask,
                installed_cpvs: &installed_cpvs,
                rebuilding_cpvs: &rebuilding_cpvs,
                autosolve_use: false,
                autounmask_widen: false,
            };
        };
    }

    fn pkg(cpn: &str) -> PortagePackage {
        PortagePackage::unslotted(Cpn::parse(cpn).unwrap())
    }

    fn ver(v: &str) -> Version {
        Version::parse(v).unwrap()
    }

    // The incident itself: readline's DEPEND on ncurses must get a sysroot
    // copy, landing immediately before readline — matching real Portage's
    // own three-entry output for this exact scenario (see the module doc).
    #[test]
    fn readline_gets_an_ncurses_sysroot_copy_before_it() {
        let data = repo_from(&[
            (
                "sys-libs/ncurses-6.5",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\n",
            ),
            (
                "sys-libs/readline-8.2",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\nDEPEND=sys-libs/ncurses\nRDEPEND=sys-libs/ncurses\n",
            ),
        ]);
        let target_order = vec![
            (pkg("sys-libs/ncurses"), ver("6.5")),
            (pkg("sys-libs/readline"), ver("8.2")),
        ];

        let sysroot = tempfile::tempdir().unwrap();
        let board = tempfile::tempdir().unwrap();
        let roots = Roots::for_test_board_root(
            sysroot.path().to_str().unwrap(),
            board.path().to_str().unwrap(),
        );

        test_adapter!(a, &data);
        let result = compute(&target_order, &a, &roots);
        let got: Vec<(String, String, MergeRoot)> = result
            .iter()
            .map(|(p, v)| (p.cpn().to_string(), v.to_string(), p.merge_root()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "sys-libs/ncurses".to_string(),
                    "6.5".to_string(),
                    MergeRoot::Target
                ),
                (
                    "sys-libs/ncurses".to_string(),
                    "6.5".to_string(),
                    MergeRoot::Base
                ),
                (
                    "sys-libs/readline".to_string(),
                    "8.2".to_string(),
                    MergeRoot::Target
                ),
            ],
            "must match real Portage's own 3-entry plan: {got:?}"
        );
    }

    // A provider already present in the sysroot's own VDB needs no copy.
    #[test]
    fn no_copy_when_the_sysroot_already_provides_it() {
        let data = repo_from(&[
            (
                "sys-libs/ncurses-6.5",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\n",
            ),
            (
                "sys-libs/readline-8.2",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\nDEPEND=sys-libs/ncurses\nRDEPEND=sys-libs/ncurses\n",
            ),
        ]);
        let target_order = vec![
            (pkg("sys-libs/ncurses"), ver("6.5")),
            (pkg("sys-libs/readline"), ver("8.2")),
        ];

        let sysroot = tempfile::tempdir().unwrap();
        let board = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(sysroot.path(), "sys-libs/ncurses-6.5");
        let roots = Roots::for_test_board_root(
            sysroot.path().to_str().unwrap(),
            board.path().to_str().unwrap(),
        );

        test_adapter!(a, &data);
        let result = compute(&target_order, &a, &roots);
        assert_eq!(result, target_order);
    }

    // BDEPEND is a build-host tool concern (host_copies' job), never copied
    // into the sysroot.
    #[test]
    fn bdepend_is_never_copied_to_the_sysroot() {
        let data = repo_from(&[
            (
                "dev-build/tool-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\n",
            ),
            (
                "sys-apps/consumer-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\nBDEPEND=dev-build/tool\n",
            ),
        ]);
        let target_order = vec![(pkg("sys-apps/consumer"), ver("1.0"))];

        let sysroot = tempfile::tempdir().unwrap();
        let board = tempfile::tempdir().unwrap();
        let roots = Roots::for_test_board_root(
            sysroot.path().to_str().unwrap(),
            board.path().to_str().unwrap(),
        );

        test_adapter!(a, &data);
        let result = compute(&target_order, &a, &roots);
        assert_eq!(result, target_order);
    }

    // A Target entry's own RDEPEND is never examined at top level (only
    // DEPEND is) — but once a copy is scheduled, *its* RDEPEND is followed,
    // one level down, since a copy's shared library needs its own runtime
    // deps present in the sysroot for the linker to resolve.
    #[test]
    fn rdepend_of_a_copy_is_followed_but_rdepend_of_a_target_entry_is_not() {
        let data = repo_from(&[
            (
                "sys-apps/t-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\nDEPEND=dev-libs/libx\nRDEPEND=dev-libs/libz\n",
            ),
            (
                "dev-libs/libx-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\nRDEPEND=dev-libs/liby\n",
            ),
            (
                "dev-libs/liby-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\n",
            ),
            (
                "dev-libs/libz-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\n",
            ),
        ]);
        let target_order = vec![(pkg("sys-apps/t"), ver("1.0"))];

        let sysroot = tempfile::tempdir().unwrap();
        let board = tempfile::tempdir().unwrap();
        let roots = Roots::for_test_board_root(
            sysroot.path().to_str().unwrap(),
            board.path().to_str().unwrap(),
        );

        test_adapter!(a, &data);
        let result = compute(&target_order, &a, &roots);
        let names: Vec<String> = result.iter().map(|(p, _)| p.cpn().to_string()).collect();
        assert!(
            names.contains(&"dev-libs/libx".to_string()),
            "t's own DEPEND must get a copy: {names:?}"
        );
        assert!(
            names.contains(&"dev-libs/liby".to_string()),
            "libx's RDEPEND must be followed one level down: {names:?}"
        );
        assert!(
            !names.contains(&"dev-libs/libz".to_string()),
            "t's own RDEPEND must NOT trigger a copy (that's the whole bug \
             this module doesn't reproduce): {names:?}"
        );
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(pos("dev-libs/liby") < pos("dev-libs/libx"));
        assert!(pos("dev-libs/libx") < pos("sys-apps/t"));
    }

    // A dep shared by two consumers (one direct, one via a copy's own
    // closure) is copied exactly once and lands before its first consumer —
    // mirrors host_copies' equivalent regression test.
    #[test]
    fn a_shared_dep_of_two_consumers_is_copied_once_and_lands_first() {
        let data = repo_from(&[
            (
                "sys-apps/t1-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\nDEPEND=dev-libs/liba\n",
            ),
            (
                "sys-apps/t2-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\nDEPEND=dev-libs/libb\n",
            ),
            (
                "dev-libs/liba-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\n",
            ),
            (
                "dev-libs/libb-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\nDEPEND=dev-libs/liba\n",
            ),
        ]);
        let target_order = vec![
            (pkg("sys-apps/t1"), ver("1.0")),
            (pkg("sys-apps/t2"), ver("1.0")),
        ];

        let sysroot = tempfile::tempdir().unwrap();
        let board = tempfile::tempdir().unwrap();
        let roots = Roots::for_test_board_root(
            sysroot.path().to_str().unwrap(),
            board.path().to_str().unwrap(),
        );

        test_adapter!(a, &data);
        let result = compute(&target_order, &a, &roots);
        let names: Vec<String> = result.iter().map(|(p, _)| p.cpn().to_string()).collect();
        assert_eq!(
            names.iter().filter(|n| *n == "dev-libs/liba").count(),
            1,
            "liba must not be duplicated: {names:?}"
        );
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(pos("dev-libs/liba") < pos("sys-apps/t1"));
        assert!(pos("dev-libs/liba") < pos("dev-libs/libb"));
        assert!(pos("dev-libs/libb") < pos("sys-apps/t2"));
    }

    // Every other topology must pass `target_order` through unchanged.
    #[test]
    fn no_op_for_a_native_offset_and_for_a_plain_target() {
        let data = repo_from(&[(
            "sys-libs/ncurses-6.5",
            "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\n",
        )]);
        let target_order = vec![(pkg("sys-libs/ncurses"), ver("6.5"))];

        test_adapter!(a, &data);
        let dir = tempfile::tempdir().unwrap();
        let plain = Roots::for_test(dir.path().to_str().unwrap());
        assert_eq!(compute(&target_order, &a, &plain), target_order);

        let broot = tempfile::tempdir().unwrap();
        let native_offset = Roots::for_test_root_with_broot(
            dir.path().to_str().unwrap(),
            broot.path().to_str().unwrap(),
        );
        assert_eq!(compute(&target_order, &a, &native_offset), target_order);
    }
}
