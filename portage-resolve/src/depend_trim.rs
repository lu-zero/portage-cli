//! Post-solve trim: drop plan entries only pulled for `DEPEND` already
//! satisfied on the sysroot (`ESYSROOT`).
//!
//! Host-config stage (`--config-root / --root <empty>`) and prefix overlays
//! (`--prefix`) stamp `DEPEND` onto the target merge root in the solver, which
//! can over-pull bootstrap packages (e.g. `sys-devel/gcc-11.5.0`) that the
//! host sysroot already provides. This pass mirrors `crate::bdepend_trim` but
//! checks `DEPEND` against the sysroot VDB only — within-run target merges do
//! not satisfy build-time `DEPEND` on a foreign sysroot.

use portage_atom::{Cpn, Cpv, Version};
use portage_atom_pubgrub::PortagePackage;

use crate::Avail;

use crate::bdepend_trim::TrimCtx;
use crate::effective_use;

/// Drop entries only needed for `DEPEND` edges already satisfied on the
/// sysroot. No-op when `sysroot == target` (full offset / crossdev sysroot).
pub fn trim_sysroot_satisfied_depend(
    order: Vec<(PortagePackage, Version)>,
    sysroot: Option<&camino::Utf8Path>,
    target: &camino::Utf8Path,
    ctx: &TrimCtx<'_>,
) -> Vec<(PortagePackage, Version)> {
    if order.is_empty() || sysroot == Some(target) {
        return order;
    }

    let mut kept: Vec<(PortagePackage, Version)> = Vec::with_capacity(order.len());
    let mut kept_indices: Vec<usize> = Vec::with_capacity(order.len());
    let sysroot_avail = Avail::initial_sysroot_depend(sysroot);

    for (i, (pkg, ver)) in order.iter().enumerate() {
        let cand = TrimCandidate {
            index: i,
            pkg,
            ver,
            order: &order,
            kept: &kept,
            kept_indices: &kept_indices,
            ctx,
            sysroot_avail: &sysroot_avail,
        };
        if should_keep(&cand) {
            kept.push((pkg.clone(), ver.clone()));
            kept_indices.push(i);
        }
    }

    kept
}

struct TrimCandidate<'a, 'b> {
    index: usize,
    pkg: &'a PortagePackage,
    ver: &'a Version,
    order: &'a [(PortagePackage, Version)],
    kept: &'a [(PortagePackage, Version)],
    kept_indices: &'a [usize],
    ctx: &'a TrimCtx<'b>,
    sysroot_avail: &'a Avail,
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
    for (j, (consumer, consumer_ver)) in cand.order.iter().enumerate() {
        if j == cand.index {
            continue;
        }
        let Some(deps) = effective_use::evaluated_deps(
            cand.ctx.data,
            &cand.ctx.policy,
            consumer,
            consumer_ver,
            false,
        ) else {
            continue;
        };
        if cand
            .sysroot_avail
            .has_unsatisfied_atom_for_cpn(&deps.depend(), cpn)
        {
            return true;
        }

        let runtime_avail = target_avail_for_consumer(j, cand.kept, cand.kept_indices);
        if runtime_avail.has_unsatisfied_atom_for_cpn(&deps.rdepend(), cpn)
            || runtime_avail.has_unsatisfied_atom_for_cpn(&deps.pdepend(), cpn)
            || runtime_avail.has_unsatisfied_atom_for_cpn(&deps.idepend(), cpn)
        {
            return true;
        }
    }

    false
}

fn target_avail_for_consumer(
    consumer_index: usize,
    kept: &[(PortagePackage, Version)],
    kept_indices: &[usize],
) -> Avail {
    let mut out = Vec::new();
    for (k, (pkg, ver)) in kept.iter().enumerate() {
        if kept_indices[k] < consumer_index {
            out.push((Cpv::new(*pkg.cpn(), ver.clone()), None));
        }
    }
    Avail::from_cpvs(out)
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
        let data = RepoData {
            cpns: Vec::new(),
            versions: HashMap::new(),
            repo_name: "gentoo".into(),
            repo_of: HashMap::new(),
            real_cpn_of: HashMap::new(),
        };
        let root_cpns = HashSet::new();
        let reinstall = HashSet::new();
        let roots = empty_roots();
        let fm = crate::force_mask::ForceMask::default();
        let arch = gentoo_core::Arch::intern("amd64");
        let ak = AcceptKeywords::from_global(&arch, &["amd64"]);
        let al = AcceptOverlay::new(
            AcceptSet::from_tokens(&["*".into()], &LicenseGroupRegistry::default()),
            Vec::new(),
        );
        let ctx = TrimCtx {
            roots: &roots,
            data: &data,
            policy: ResolvePolicy {
                accept_keywords: &ak,
                package_mask: &[],
                package_unmask: &[],
                accept_licenses: &al,
                accept_properties: &al,
                accept_restrict: &al,
                pre_env: empty_layer(),
                env_use: empty_layer(),
                package_use: &[],
                profile_package_use: &[],
                force_mask: &fm,
            },
            root_cpns: &root_cpns,
            reinstall_cpns: &reinstall,
        };
        let target = camino::Utf8Path::new("/tmp/stage");
        let out = trim_sysroot_satisfied_depend(order.clone(), Some(target), target, &ctx);
        assert_eq!(out.len(), order.len());
    }
}
