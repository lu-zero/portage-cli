//! Post-solve trim: drop plan entries only pulled for `DEPEND` already
//! satisfied on the sysroot (`ESYSROOT`)
//!
//! Host-config stage (`--config-root / --root <empty>`) and prefix overlays
//! (`--prefix`) stamp `DEPEND` onto the target merge root in the solver, which
//! can over-pull bootstrap packages (e.g. `sys-devel/gcc-11.5.0`) that the
//! host sysroot already provides. This pass mirrors `crate::bdepend_trim` but
//! checks `DEPEND` against the sysroot VDB only — within-run target merges do
//! not satisfy build-time `DEPEND` on a foreign sysroot.

use std::collections::{HashMap, HashSet};

use portage_atom::{Cpn, Cpv, DepEntry, Version};
use portage_atom_pubgrub::PortagePackage;

use crate::Avail;
use crate::installed::VdbSnapshotCache;

use crate::bdepend_trim::TrimCtx;
use crate::effective_use;

/// Drop entries only needed for `DEPEND` edges already satisfied on the sysroot
///
/// No-op when `sysroot == target` (full offset / crossdev sysroot).
pub fn trim_sysroot_satisfied_depend(
    order: Vec<(PortagePackage, Version)>,
    sysroot: Option<&camino::Utf8Path>,
    target: &camino::Utf8Path,
    ctx: &TrimCtx<'_>,
) -> Vec<(PortagePackage, Version)> {
    trim_sysroot_satisfied_depend_with_cache(
        order,
        sysroot,
        target,
        ctx,
        &VdbSnapshotCache::default(),
    )
}

/// [`trim_sysroot_satisfied_depend`] using VDB snapshots shared with the resolve.
pub fn trim_sysroot_satisfied_depend_with_cache(
    order: Vec<(PortagePackage, Version)>,
    sysroot: Option<&camino::Utf8Path>,
    target: &camino::Utf8Path,
    ctx: &TrimCtx<'_>,
    snapshots: &VdbSnapshotCache,
) -> Vec<(PortagePackage, Version)> {
    if order.is_empty() || sysroot == Some(target) {
        return order;
    }

    let mut kept_cpvs: Vec<Cpv> = Vec::with_capacity(order.len());
    let mut kept_indices: Vec<usize> = Vec::with_capacity(order.len());
    let sysroot_avail = Avail::initial_sysroot_depend_with_cache(sysroot, snapshots);
    let mut evaluated = effective_use::EvaluatedDepsCache::new(ctx.data, ctx.policy, false);
    let evaluated_order = evaluated.snapshot(&order);
    let consumers = depend_consumers(&evaluated_order);

    for (i, (pkg, ver)) in order.iter().enumerate() {
        let cand = TrimCandidate {
            index: i,
            pkg,
            ver,
            order: &order,
            kept_cpvs: &kept_cpvs,
            kept_indices: &kept_indices,
            ctx,
            sysroot_avail: &sysroot_avail,
            evaluated: &evaluated_order,
            consumers: &consumers,
        };
        if should_keep(&cand) {
            kept_cpvs.push(Cpv::new(*pkg.cpn(), ver.clone()));
            kept_indices.push(i);
        }
    }

    kept_indices
        .iter()
        .map(|&index| order[index].clone())
        .collect()
}

fn depend_consumers<'a, 'b>(
    evaluated: &'a [Option<&'a effective_use::EvaluatedDeps<'b>>],
) -> HashMap<Cpn, Vec<usize>> {
    let mut out: HashMap<Cpn, Vec<usize>> = HashMap::new();
    for (index, deps) in evaluated.iter().enumerate() {
        let Some(deps) = deps else {
            continue;
        };
        // Keep every mentioned CPN; group satisfaction remains authoritative.
        let mut cpns = HashSet::new();
        for entries in [
            deps.depend(),
            deps.rdepend(),
            deps.pdepend(),
            deps.idepend(),
        ] {
            collect_cpns(entries, &mut cpns);
        }
        for cpn in cpns {
            out.entry(cpn).or_default().push(index);
        }
    }
    out
}

fn collect_cpns(entries: &[DepEntry], out: &mut HashSet<Cpn>) {
    DepEntry::walk_atoms(entries, &mut |dep| {
        if dep.blocker.is_none() {
            out.insert(dep.cpn);
        }
    });
}

struct TrimCandidate<'a, 'b> {
    index: usize,
    pkg: &'a PortagePackage,
    ver: &'a Version,
    order: &'a [(PortagePackage, Version)],
    kept_cpvs: &'a [Cpv],
    kept_indices: &'a [usize],
    ctx: &'a TrimCtx<'b>,
    sysroot_avail: &'a Avail,
    evaluated: &'a [Option<&'a effective_use::EvaluatedDeps<'b>>],
    consumers: &'a HashMap<Cpn, Vec<usize>>,
}

fn should_keep(cand: &TrimCandidate<'_, '_>) -> bool {
    let cpn = *cand.pkg.cpn();
    let same_cpn: Vec<&Version> = cand
        .order
        .iter()
        .filter(|(p, _)| p.cpn() == &cpn)
        .map(|(_, v)| v)
        .collect();
    if same_cpn.len() > 1 {
        // Parallel PYTHON_TARGETS installs (3.13 + 3.14) must all stay.
        if cpn == Cpn::parse("dev-lang/python").expect("dev-lang/python is a valid CPN") {
            return true;
        }
        // Bootstrap gcc (11.x) after the real toolchain (16.x) is DEPEND-only noise.
        // Run before the `@system` root_cpn guard: expanded sets list `sys-devel/gcc`
        // once but must not pin every resolved slot/version.
        if cpn == Cpn::parse("sys-devel/gcc").expect("sys-devel/gcc is a valid CPN") {
            return same_cpn.iter().max().is_some_and(|max| cand.ver == *max);
        }
    }

    if cand.ctx.root_cpns.contains(&cpn) || cand.ctx.reinstall_cpns.contains(&cpn) {
        return true;
    }

    // DEPEND providers can appear after their consumer in install order (e.g.
    // bootstrap `gcc-11` after `gcc-16`), so every other plan entry is checked.
    let Some(consumers) = cand.consumers.get(&cpn) else {
        return false;
    };
    for &j in consumers {
        if j == cand.index {
            continue;
        }
        let Some(deps) = cand.evaluated.get(j).and_then(Option::as_ref) else {
            continue;
        };
        if cand
            .sysroot_avail
            .has_unsatisfied_atom_for_cpn(deps.depend(), cpn)
        {
            return true;
        }

        // Kept indices are appended in plan order, so entries before `j` form a prefix.
        let prefix_len = cand.kept_indices.partition_point(|&index| index < j);
        let runtime_avail =
            crate::bdepend_avail::AvailPrefix::new(None, &cand.kept_cpvs[..prefix_len]);
        if runtime_avail.has_unsatisfied_atom_for_cpn(deps.rdepend(), cpn)
            || runtime_avail.has_unsatisfied_atom_for_cpn(deps.pdepend(), cpn)
            || runtime_avail.has_unsatisfied_atom_for_cpn(deps.idepend(), cpn)
        {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    fn empty_layer() -> &'static portage_atom_pubgrub::UseLayer {
        use std::sync::OnceLock;
        static E: OnceLock<portage_atom_pubgrub::UseLayer> = OnceLock::new();
        E.get_or_init(portage_atom_pubgrub::UseLayer::default)
    }

    use std::collections::{HashMap, HashSet};

    use portage_repo::{AcceptSet, LicenseGroupRegistry};

    use super::*;
    use crate::Roots;
    use crate::installed::BrootSnapshot;
    use crate::repo::{AcceptKeywords, AcceptOverlay, RepoData, ResolvePolicy};

    fn empty_roots() -> Roots {
        Roots::default()
    }

    #[test]
    fn gcc_bootstrap_version_orders_below_current() {
        let v11 = Version::parse("11.5.0").unwrap();
        let v16 = Version::parse("16.1.1_p20260606").unwrap();
        assert!(v16 > v11);
        assert_eq!([&v11, &v16].into_iter().max(), Some(&v16));
    }

    #[test]
    fn no_op_when_sysroot_equals_target() {
        let pkg = PortagePackage::unslotted(Cpn::parse("app-misc/a").unwrap());
        let ver = Version::parse("1.0").unwrap();
        let order = vec![(pkg, ver)];
        let data = RepoData::from_parts(
            Vec::new(),
            HashMap::new(),
            "gentoo".into(),
            HashMap::new(),
            HashMap::new(),
        );
        let root_cpns = HashSet::new();
        let reinstall = HashSet::new();
        let roots = empty_roots();
        let broot_snapshot = BrootSnapshot::load(&roots);
        let fm = crate::force_mask::ForceMask::default();
        let arch = gentoo_core::Arch::intern("amd64");
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let al = AcceptOverlay::new(
            AcceptSet::from_tokens(&["*".into()], &LicenseGroupRegistry::default()),
            Vec::new(),
        );
        let ctx = TrimCtx {
            broot_snapshot: &broot_snapshot,
            data: &data,
            policy: ResolvePolicy {
                accept_keywords: &ak,
                package_mask: &crate::repo::PolicyMaskList::EMPTY,
                package_unmask: &crate::repo::PolicyMaskList::EMPTY,
                accept_licenses: &al,
                accept_properties: &al,
                accept_restrict: &al,
                defaults: empty_layer(),
                conf: empty_layer(),
                env_use: empty_layer(),
                package_use: &[],
                profile_package_use: &[],
                force_mask: &fm,
                facts: None,
            },
            root_cpns: &root_cpns,
            reinstall_cpns: &reinstall,
        };
        let target = camino::Utf8Path::new("/tmp/stage");
        let out = trim_sysroot_satisfied_depend(order.clone(), Some(target), target, &ctx);
        assert_eq!(out.len(), order.len());
    }
}
