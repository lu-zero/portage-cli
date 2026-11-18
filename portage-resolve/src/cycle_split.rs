use portage_atom::{Cpv, Version};
use portage_atom_pubgrub::{CededFlag, PortagePackage, SplitCandidate};

use crate::effective_use::{effective_use, pinned_flags};
use crate::repo::{RepoData, ResolvePolicy, find_cache};
use crate::required_use::unmet_groups;

/// Why a [`SplitCandidate`] cannot be built with its flag off
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The profile's `use.force`/`use.mask` pins the flag
    Pinned,
    /// `REQUIRED_USE` fails with the flag off; the failing groups, rendered
    RequiredUse(Vec<String>),
    /// The plan has the package at another merge root too, and one `package.use` entry would reach both
    OtherMergeRoot,
}

/// Whether `candidate` can be built with its flag off, judged before any re-solve
///
/// What the flag-off build pulls in, and whether a `[flag]` USE dependency turns the flag back on,
/// is only known after solving with it, so the caller checks those on that solve.
pub fn check(
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    policy: &ResolvePolicy,
    ceded: &[CededFlag],
    candidate: &SplitCandidate,
) -> Result<(), Rejection> {
    let (pkg, ver) = &candidate.package;
    if order
        .iter()
        .any(|(p, v)| p.cpn() == pkg.cpn() && v == ver && p.merge_root() != pkg.merge_root())
    {
        return Err(Rejection::OtherMergeRoot);
    }
    let Some(cache) = find_cache(data, pkg, ver) else {
        return Ok(());
    };
    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
    let stable = policy
        .accept_keywords
        .is_stable(&cache.metadata.keywords, &cpv, pkg.slot());
    if pinned_flags(policy, pkg, ver, cache, stable).contains(&candidate.flag) {
        return Err(Rejection::Pinned);
    }
    let Some(required_use) = cache.metadata.required_use.as_ref() else {
        return Ok(());
    };
    let effective = effective_use(policy, pkg, ver, cache, stable, ceded);
    // A group the plan already fails is reported on its own; only what turning the flag off breaks counts.
    let before = unmet_groups(required_use, cache, &effective, None);
    let mut unmet = unmet_groups(required_use, cache, &effective, Some(candidate.flag));
    unmet.retain(|g| !before.contains(g));
    if unmet.is_empty() {
        Ok(())
    } else {
        Err(Rejection::RequiredUse(unmet))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use gentoo_core::Arch;
    use portage_atom::interner::Interned;
    use portage_atom::{Cpn, Cpv};
    use portage_atom_pubgrub::{MergeRoot, UseLayer};
    use portage_metadata::CacheEntry;
    use portage_repo::{AcceptSet, LicenseGroupRegistry};

    use super::*;
    use crate::force_mask::{ForceMask, ForceMaskLayer, fold_signed};
    use crate::repo::{AcceptKeywords, AcceptOverlay, PolicyMaskList};

    fn data(iuse: &str, required_use: &str) -> RepoData {
        let cpv = Cpv::parse("sys-apps/ul-1").unwrap();
        let text = format!(
            "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nIUSE={iuse}\nREQUIRED_USE={required_use}\n"
        );
        let entry = CacheEntry::parse(&text).unwrap();
        RepoData::from_parts(
            vec![cpv.cpn],
            HashMap::from([(cpv.cpn, vec![(cpv, entry)])]),
            "test".into(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn policy(force_mask: ForceMask) -> ResolvePolicy<'static> {
        let layer: &'static UseLayer = Box::leak(Box::default());
        let lic: &'static AcceptOverlay = Box::leak(Box::new(AcceptOverlay::new(
            AcceptSet::from_tokens(&["*".into()], &LicenseGroupRegistry::default()),
            Vec::new(),
        )));
        let arch = Arch::intern("amd64");
        ResolvePolicy {
            accept_keywords: Box::leak(Box::new(AcceptKeywords::from_global(&arch, &["amd64"]))),
            package_mask: &PolicyMaskList::EMPTY,
            package_unmask: &PolicyMaskList::EMPTY,
            accept_licenses: lic,
            accept_properties: lic,
            accept_restrict: lic,
            defaults: layer,
            conf: layer,
            env_use: layer,
            package_use: &[],
            profile_package_use: &[],
            force_mask: Box::leak(Box::new(force_mask)),
            facts: None,
        }
    }

    fn ul(root: MergeRoot) -> (PortagePackage, Version) {
        let cpn = Cpn::parse("sys-apps/ul").unwrap();
        let pkg = PortagePackage::slotted_at(cpn, Interned::intern("0"), root);
        (pkg, Version::parse("1").unwrap())
    }

    fn check_nls(
        data: &RepoData,
        policy: &ResolvePolicy,
        order: &[(PortagePackage, Version)],
    ) -> Result<(), Rejection> {
        let candidate = SplitCandidate {
            package: ul(MergeRoot::Target),
            flag: Interned::intern("nls"),
            build_time: true,
        };
        check(data, order, policy, &[], &candidate)
    }

    #[test]
    fn a_free_flag_can_be_turned_off() {
        let order = [ul(MergeRoot::Target)];
        assert_eq!(
            check_nls(&data("+nls", ""), &policy(ForceMask::default()), &order),
            Ok(())
        );
    }

    #[test]
    fn a_forced_flag_is_pinned() {
        let force_mask = ForceMask::one(ForceMaskLayer {
            use_force: fold_signed(vec!["nls".into()]),
            ..Default::default()
        });
        let order = [ul(MergeRoot::Target)];
        assert_eq!(
            check_nls(&data("+nls", ""), &policy(force_mask), &order),
            Err(Rejection::Pinned)
        );
    }

    #[test]
    fn required_use_broken_by_the_flag_rejects_it() {
        let order = [ul(MergeRoot::Target)];
        let result = check_nls(
            &data("+nls +doc", "doc? ( nls )"),
            &policy(ForceMask::default()),
            &order,
        );
        assert!(
            matches!(&result, Err(Rejection::RequiredUse(groups)) if groups.len() == 1),
            "{result:?}"
        );
    }

    #[test]
    fn required_use_the_plan_already_fails_is_not_blamed_on_the_flag() {
        let order = [ul(MergeRoot::Target)];
        assert_eq!(
            check_nls(
                &data("+nls doc", "doc"),
                &policy(ForceMask::default()),
                &order
            ),
            Ok(())
        );
    }

    #[test]
    fn a_package_at_two_merge_roots_is_rejected() {
        let order = [ul(MergeRoot::Host), ul(MergeRoot::Target)];
        assert_eq!(
            check_nls(&data("+nls", ""), &policy(ForceMask::default()), &order),
            Err(Rejection::OtherMergeRoot)
        );
    }
}
