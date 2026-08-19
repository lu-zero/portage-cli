//! Native-offset host build-copies (Tier 1 `--root` for a Gentoo host)
//!
//! When building into an offset ROOT with the host (`/`) as `BROOT`/`ESYSROOT`,
//! a target package's build-time edges (`DEPEND`, `BDEPEND`, `IDEPEND`) that
//! the host *lacks* must be built on the host first so the target can compile
//! and link against them. emerge lists these as a second merge `to /` alongside
//! the ROOT runtime copy. Example: `net-misc/curl`'s `DEPEND="${RDEPEND}"`
//! includes `net-libs/{nghttp2,nghttp3,ngtcp2}`; if the host lacks them, emerge
//! merges each twice — once `to /eroot/` (runtime) and once `to /` (build).
//!
//! This is computed as a **post-solve closure walk** over the finalized Target
//! plan, NOT inside the solver: the dual `(package, merge_root)` solver nodes
//! share `PackageData` via `host_aliases`, and introducing `pkg@Host` into a
//! single solve balloons the Target closure (the Tier 1 aliasing blocker).
//! Keeping the Target solve single-rooted preserves its parity (12 packages
//! for curl); the host copies are derived against the host VDB afterwards.
//!
//! `cross.active` (any offset/dual-root context — plain `--root`/`--prefix`
//! included, not just cross-arch) can, for some invocations (e.g. crossdev's
//! own host-arch tools), still have the solver emit its own `MergeRoot::Host`
//! nodes directly into `target_order` — the invariant above doesn't hold
//! universally. This walk must never re-derive or duplicate those: it seeds
//! its availability and seen-set from whatever `MergeRoot::Host` entries are
//! already present, and only fills genuine remaining gaps (the
//! `dev-perl/Digest-HMAC` duplicate finding).
//!
//! Host copies reuse the Target solve's version for a CPN (same arch, same
//! repo), falling back to the newest *accepted* (keyword/mask/license) repo
//! version for build-only deps absent from the Target plan. The walk is
//! bounded by the host's own availability (VDB, weaving in the `--prefix`
//! VDB the same way `bdepend_avail::Avail::initial_bdepend` does), which
//! already provides the toolchain (gcc, python, …) — so only the genuinely
//! missing libraries are scheduled, and each is emitted only after its own
//! unmet build edges (deps-first, not BFS discovery order) — matching the
//! no-forward-reference invariant `preflight::check` validates for the rest
//! of the plan.
//!
//! The `compute` function returns the **whole** reordered plan, not just the new
//! copies: each copy is interleaved directly in front of the first Target
//! entry that (transitively) needs it, by walking `target_order` in its
//! existing order and appending each entry's not-yet-emitted dependencies
//! right before it. This was originally a separate step (`mod.rs` spliced a
//! copies-only list returned here at position 0 of the whole plan) — wrong
//! whenever a copy depended on a seeded `MergeRoot::Host` entry that wasn't
//! already at position 0 (a forward reference, the same bug class this walk
//! exists to prevent for copies among themselves; caught in review before
//! landing). Interleaving during the walk itself removes the need for any
//! separate position-tracking: a copy discovered while visiting entry `E`
//! is always pushed immediately before `E`, so it's automatically after
//! everything `E` doesn't depend on and before `E` itself, with no
//! possibility of landing after a later consumer either.

use std::collections::{HashMap, HashSet};

use portage_atom::{Cpn, Cpv, Version};
use portage_atom_pubgrub::{MergeRoot, PortagePackage};

use crate::Roots;
use crate::{Avail, unsatisfied_cpns};

use crate::effective_use;
use crate::repo::Adapter;
use crate::root_aware::CrossContext;

/// Static inputs shared across the walk
struct Ctx<'a> {
    adapter: &'a Adapter<'a>,
    target_ver: &'a HashMap<Cpn, (Version, PortagePackage)>,
}

/// Mutable walk state: host availability (VDB + already-planned Host entries
/// + emitted copies) and the seen-set (also breaks dependency cycles)
struct Walk {
    avail: Avail,
    seen: HashSet<Cpn>,
}

/// Compute the finalized plan order for a native offset (`--root`/`--prefix`,
/// same arch), inserting host (`MergeRoot::Host`) build-copies immediately
/// before whichever Target entry first needs each one
///
/// Returns `target_order` unchanged for non-native-offset builds (plain
/// native, cross-arch, host) — cross-arch uses the solver's own dual-root
/// path, plain native has BROOT == ROOT, and neither ever schedules host
/// build-copies here.
pub fn compute(
    target_order: &[(PortagePackage, Version)],
    adapter: &Adapter<'_>,
    roots: &Roots,
    cross: &CrossContext,
) -> Vec<(PortagePackage, Version)> {
    if !cross.active || cross.is_cross_arch() {
        return target_order.to_vec();
    }

    // CPN -> (version, target package) for version reuse: a host copy of a
    // package also built for Target uses the Target version (same arch/repo).
    let target_ver: HashMap<Cpn, (Version, PortagePackage)> = target_order
        .iter()
        .filter(|(p, _)| p.merge_root() == MergeRoot::Target)
        .map(|(p, v)| (*p.cpn(), (v.clone(), p.clone())))
        .collect();
    let ctx = Ctx {
        adapter,
        target_ver: &target_ver,
    };

    // Host availability starts as the host's own BDEPEND view — the host VDB,
    // plus the prefix's under `--prefix` (same weave `preflight` relies on) —
    // and grows with each host copy.
    let mut walk = Walk {
        seen: HashSet::new(),
        avail: Avail::initial_bdepend(roots),
    };

    // Seed with whatever `MergeRoot::Host` entries the solver already put in
    // `target_order` (e.g. crossdev's own host-arch tools): never re-derive
    // or duplicate those, just record them as already available. This scan
    // is a separate upfront pass (not folded into the interleave loop below)
    // because an *earlier* Target entry can depend on a *later* seeded Host
    // entry — availability must see every seed regardless of where it sits;
    // only the seed's own position in the final order is unaffected (it's
    // never moved, only new copies are inserted around it).
    for (pkg, ver) in target_order
        .iter()
        .filter(|(p, _)| p.merge_root() == MergeRoot::Host)
    {
        walk.seen.insert(*pkg.cpn());
        walk.avail
            .record_merge_bdepend(Cpv::new(*pkg.cpn(), ver.clone()));
    }

    // Interleave: walk target_order in its existing order, and for each
    // Target entry, insert its not-yet-emitted host dependencies (deps-first,
    // recursively) immediately before it, then emit the entry itself. Host
    // entries pass through unchanged (already seeded above, never revisited).
    let mut order: Vec<(PortagePackage, Version)> = Vec::with_capacity(target_order.len());
    for (pkg, ver) in target_order {
        if pkg.merge_root() == MergeRoot::Target {
            visit_unsatisfied(&ctx, &mut walk, pkg, ver, &mut order, true);
        }
        order.push((pkg.clone(), ver.clone()));
    }
    order
}

/// Recurse into `pkg`'s unsatisfied-on-host `DEPEND`/`BDEPEND`/`IDEPEND` edges
///
/// Appends each resolved host copy to `order` only *after* its own edges
/// have been visited — deps-first, so a copy never lands before something
/// it needs.
///
/// Called just before `pkg` itself is pushed to `order`, so every copy
/// discovered here also ends up immediately before `pkg` — its first (and
/// closure-wide) consumer.
///
/// `top_level` is `true` only for direct per-Target-package calls, not
/// edges found recursing into an already-found copy. Under `cross.active`
/// the solver's dual-root expansion should already schedule every
/// unsatisfied BDEPEND/IDEPEND edge, so a *top-level* gap here means this
/// walk's `Avail` view and the solver's disagreed, or a post-solve trim
/// dropped a needed `@host` entry: worth surfacing.
///
/// A copy's own recursed-into edges never trigger this — those packages
/// never went through the solver, nothing else schedules their build deps.
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
    for (class, entries) in [
        ("DEPEND", deps.depend()),
        ("BDEPEND", deps.bdepend()),
        ("IDEPEND", deps.idepend()),
    ] {
        for cpn in unsatisfied_cpns(&entries, &walk.avail) {
            if !walk.seen.insert(cpn) {
                continue;
            }
            // Developer signal, not user-facing: the solver should already
            // cover this under cross.active, so a hit here means that path
            // missed one.
            if top_level && class != "DEPEND" {
                tracing::debug!("host_copies: top-level {class} gap for {cpn} (from {pkg})");
            }
            let Some((cver, cpkg)) = resolve(cpn, ctx) else {
                continue;
            };
            let host_pkg = cpkg.at_merge_root(MergeRoot::Host);
            visit_unsatisfied(ctx, walk, &host_pkg, &cver, order, false);
            walk.avail.record_merge_bdepend(Cpv::new(cpn, cver.clone()));
            order.push((host_pkg, cver));
        }
    }
}

/// Resolve `(version, package)` for a host copy of `cpn`
///
/// The Target plan's version when the CPN is also built for Target, else
/// the newest keyword/mask/license-accepted repo version.
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
    use crate::root_aware;

    fn accept_all_licenses() -> AcceptSet {
        AcceptSet::from_tokens(&["*".into()], &LicenseGroupRegistry::default())
    }

    // Build a `RepoData` from `(cpv, md5-cache-text)` pairs, one version per CPN
    // (mirrors the same-shaped helper in `bdepend_trim`'s and `depend_trim`'s
    // own test modules)
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

    // A `--prefix`-shaped `CrossContext`: `active` (target != host), not
    // cross-arch (no make.conf to read a foreign `CHOST` from) — the native
    // offset case `host_copies::compute` exists for
    fn native_offset_cross(roots: &Roots) -> CrossContext {
        root_aware::detect(roots, roots.merge_root())
    }

    // Regression test for the `5989eb1` fix (the `dev-perl/Digest-HMAC`
    // duplicate-plan-entry incident): when
    // the solver's own dual-root expansion has already scheduled a
    // `MergeRoot::Host` node for a CPN (simulating crossdev's host-arch
    // tools), `compute` must not re-derive or duplicate it. Before that fix
    // this produced a second, independently-versioned, anti-topologically
    // ordered copy; this pins the seeding behavior that stops it, which had
    // no test of its own — the fix was verified live only.
    #[test]
    fn does_not_duplicate_a_solver_seeded_host_entry() {
        let data = repo_from(&[
            (
                "sys-apps/consumer-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/tool\n",
            ),
            (
                "dev-libs/tool-2.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\n",
            ),
        ]);

        let consumer = (
            PortagePackage::unslotted(Cpn::parse("sys-apps/consumer").unwrap()),
            Version::parse("1.0").unwrap(),
        );
        // Stands in for the solver's own `append_unsatisfied_broot` output:
        // this CPN's BDEPEND is already scheduled `@host`.
        let tool_host = (
            PortagePackage::unslotted(Cpn::parse("dev-libs/tool").unwrap())
                .at_merge_root(MergeRoot::Host),
            Version::parse("2.0").unwrap(),
        );
        let target_order = vec![consumer, tool_host];

        let arch = gentoo_core::Arch::intern("amd64");
        let accept_keywords = AcceptKeywords::from_global(&arch, &["amd64"]);
        let force_mask = ForceMask::default();
        let installed_cpvs = std::collections::HashSet::new();
        let adapter = Adapter {
            data: &data,
            accept_keywords: &accept_keywords,
            package_mask: &[],
            package_unmask: &[],
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            defaults: empty_layer(),
            conf: empty_layer(),
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &force_mask,
            installed_cpvs: &installed_cpvs,
            rebuilding_cpvs: &std::collections::HashSet::new(),
            autosolve_use: false,
        };

        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        let roots = Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let cross = native_offset_cross(&roots);
        assert!(
            cross.active && !cross.is_cross_arch(),
            "test setup must land in the native-offset case compute() exists for"
        );

        let result = compute(&target_order, &adapter, &roots, &cross);
        assert_eq!(
            result, target_order,
            "must not re-derive a CPN the solver already scheduled @host"
        );
    }

    // Documents a genuinely unsolvable input: `consumer` needs `tool` needs
    // `base`, but `base` is a seeded `MergeRoot::Host` entry the solver
    // placed *after* `consumer` (host_copies never repositions an existing
    // `target_order` entry, only inserts new copies around it). No linear
    // order can satisfy that — not a bug `compute` can fix, exactly the
    // class of conflict `preflight::check` exists to catch instead. Pinned
    // so a future `compute` change that starts silently "resolving" this by
    // reordering seeded entries gets caught: that's a different, larger
    // design than the deps-first-copy-insertion this module does.
    #[test]
    fn seeded_host_entry_after_its_dependents_consumer_is_unsolvable_by_design() {
        let data = repo_from(&[
            (
                "sys-apps/consumer-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/tool\n",
            ),
            (
                "dev-libs/tool-2.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/base\n",
            ),
            (
                "dev-libs/base-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\n",
            ),
        ]);

        let consumer = (
            PortagePackage::unslotted(Cpn::parse("sys-apps/consumer").unwrap()),
            Version::parse("1.0").unwrap(),
        );
        // Seeded @host, but deliberately *not* at the front — simulates the
        // solver placing its own dual-root entry wherever the closure walk
        // found it, not necessarily before every Target entry.
        let base_host = (
            PortagePackage::unslotted(Cpn::parse("dev-libs/base").unwrap())
                .at_merge_root(MergeRoot::Host),
            Version::parse("1.0").unwrap(),
        );
        let target_order = vec![consumer, base_host];

        let arch = gentoo_core::Arch::intern("amd64");
        let accept_keywords = AcceptKeywords::from_global(&arch, &["amd64"]);
        let force_mask = ForceMask::default();
        let installed_cpvs = std::collections::HashSet::new();
        let adapter = Adapter {
            data: &data,
            accept_keywords: &accept_keywords,
            package_mask: &[],
            package_unmask: &[],
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            defaults: empty_layer(),
            conf: empty_layer(),
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &force_mask,
            installed_cpvs: &installed_cpvs,
            rebuilding_cpvs: &std::collections::HashSet::new(),
            autosolve_use: false,
        };

        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        let roots = Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let cross = native_offset_cross(&roots);

        let result = compute(&target_order, &adapter, &roots, &cross);
        let names: Vec<String> = result.iter().map(|(p, _)| p.cpn().to_string()).collect();
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        // `compute` never repositions `base` (an existing `target_order`
        // entry) — it only inserts `tool` as early as its one known
        // constraint (before `consumer`, the only relationship the solver
        // itself is aware of) requires. `tool` ends up before `base` here,
        // which does *not* actually satisfy tool's own BDEPEND on base — but
        // no placement of `tool` alone can fix that; `base` would have to
        // move too. `preflight::check` (run unconditionally before any real
        // build, see `emerge.rs`) is what actually catches this, not this
        // module.
        assert!(
            pos("dev-libs/tool") < pos("sys-apps/consumer"),
            "tool's copy must still land before consumer, its one solver-known consumer: {names:?}"
        );
    }

    // Regression test for a second forward-reference variant found in
    // review: two *different* Target entries share a host build-copy
    // dependency chain (`t2 -> libb -> liba`, `t1 -> liba` directly). `liba`
    // is resolved once (while visiting `t1`) and must not be re-derived for
    // `t2` — but `libb` (discovered later, under `t2`) still depends on it,
    // and must land after it despite `liba` no longer being "unsatisfied"
    // (it's already recorded as available) by the time `libb` is visited.
    #[test]
    fn a_later_consumers_copy_still_lands_after_an_earlier_consumers_shared_dep() {
        let data = repo_from(&[
            (
                "sys-apps/t1-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/liba\n",
            ),
            (
                "sys-apps/t2-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/libb\n",
            ),
            (
                "dev-libs/liba-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\n",
            ),
            (
                "dev-libs/libb-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/liba\n",
            ),
        ]);

        let t1 = (
            PortagePackage::unslotted(Cpn::parse("sys-apps/t1").unwrap()),
            Version::parse("1.0").unwrap(),
        );
        let t2 = (
            PortagePackage::unslotted(Cpn::parse("sys-apps/t2").unwrap()),
            Version::parse("1.0").unwrap(),
        );
        let target_order = vec![t1, t2];

        let arch = gentoo_core::Arch::intern("amd64");
        let accept_keywords = AcceptKeywords::from_global(&arch, &["amd64"]);
        let force_mask = ForceMask::default();
        let installed_cpvs = std::collections::HashSet::new();
        let adapter = Adapter {
            data: &data,
            accept_keywords: &accept_keywords,
            package_mask: &[],
            package_unmask: &[],
            accept_licenses: &AcceptOverlay::new(accept_all_licenses(), Vec::new()),
            accept_properties: &AcceptProperties::new(accept_all_licenses(), Vec::new()),
            accept_restrict: &AcceptRestrict::new(accept_all_licenses(), Vec::new()),
            defaults: empty_layer(),
            conf: empty_layer(),
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask: &force_mask,
            installed_cpvs: &installed_cpvs,
            rebuilding_cpvs: &std::collections::HashSet::new(),
            autosolve_use: false,
        };

        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        let roots = Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let cross = native_offset_cross(&roots);

        let result = compute(&target_order, &adapter, &roots, &cross);
        let names: Vec<String> = result.iter().map(|(p, _)| p.cpn().to_string()).collect();
        // liba must appear exactly once (never re-derived for t2).
        assert_eq!(
            names.iter().filter(|n| *n == "dev-libs/liba").count(),
            1,
            "liba must not be duplicated: {names:?}"
        );
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(
            pos("dev-libs/liba") < pos("sys-apps/t1"),
            "liba before its first consumer t1: {names:?}"
        );
        assert!(
            pos("dev-libs/liba") < pos("dev-libs/libb"),
            "liba before libb, which depends on it, even though libb is \
             discovered under a later consumer (t2): {names:?}"
        );
        assert!(
            pos("dev-libs/libb") < pos("sys-apps/t2"),
            "libb before its consumer t2: {names:?}"
        );
    }
}
