use std::collections::HashSet;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, DepEntry, UseDepKind, Version};
use portage_atom_pubgrub::{CededFlag, PortagePackage, SplitCandidate, SplitOrder};
use portage_metadata::CacheEntry;

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
    /// The flag-off build needs a `!flag? ( … )` dependency on a package the solve did not pick
    Unsolved(Cpn),
    /// A package merged between the two builds depends on the flag being on
    Demanded(Cpv),
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

/// Whether the re-ordered `split` can build `candidate` with its flag off until its rebuild
pub fn check_order(
    data: &RepoData,
    split: &SplitOrder,
    candidate: &SplitCandidate,
) -> Result<(), Rejection> {
    let (pkg, ver) = &candidate.package;
    let flag = candidate.flag;
    let planned: HashSet<Cpn> = split.order.iter().map(|(p, _)| *p.cpn()).collect();
    if let Some(cache) = find_cache(data, pkg, ver) {
        let mut flag_off_deps = Vec::new();
        for entry in dep_lists(cache) {
            negated_atoms(entry, flag, false, &mut flag_off_deps);
        }
        if let Some(dep) = flag_off_deps.iter().find(|d| !planned.contains(&d.cpn)) {
            return Err(Rejection::Unsolved(dep.cpn));
        }
    }
    let Some(first) = split.order.iter().position(|n| *n == candidate.package) else {
        return Ok(());
    };
    for (other, other_ver) in &split.order[first + 1..split.rebuild] {
        let Some(cache) = find_cache(data, other, other_ver) else {
            continue;
        };
        let mut atoms = Vec::new();
        for entry in dep_lists(cache) {
            all_atoms(entry, &mut atoms);
        }
        if atoms
            .iter()
            .any(|d| d.cpn == *pkg.cpn() && wants_flag(d, flag))
        {
            return Err(Rejection::Demanded(Cpv::new(
                *other.cpn(),
                other_ver.clone(),
            )));
        }
    }
    Ok(())
}

fn dep_lists(cache: &CacheEntry) -> impl Iterator<Item = &DepEntry> {
    let m = &cache.metadata;
    [&m.depend, &m.rdepend, &m.bdepend, &m.pdepend, &m.idepend]
        .into_iter()
        .flat_map(|list| list.list().iter())
}

/// Atoms under a `!flag? ( … )` group, which apply only once `flag` is off
fn negated_atoms<'a>(
    entry: &'a DepEntry,
    flag: Interned<DefaultInterner>,
    inside: bool,
    out: &mut Vec<&'a Dep>,
) {
    match entry {
        DepEntry::Atom(dep) if inside && dep.blocker.is_none() => out.push(dep),
        DepEntry::Atom(_) => {}
        DepEntry::UseConditional {
            flag: f,
            negate,
            children,
        } => {
            let inside = inside || (*negate && *f == flag);
            for child in children {
                negated_atoms(child, flag, inside, out);
            }
        }
        DepEntry::AllOf(children)
        | DepEntry::AnyOf(children)
        | DepEntry::ExactlyOneOf(children)
        | DepEntry::AtMostOneOf(children) => {
            for child in children {
                negated_atoms(child, flag, inside, out);
            }
        }
    }
}

fn all_atoms<'a>(entry: &'a DepEntry, out: &mut Vec<&'a Dep>) {
    match entry {
        DepEntry::Atom(dep) => out.push(dep),
        DepEntry::UseConditional { children, .. }
        | DepEntry::AllOf(children)
        | DepEntry::AnyOf(children)
        | DepEntry::ExactlyOneOf(children)
        | DepEntry::AtMostOneOf(children) => {
            for child in children {
                all_atoms(child, out);
            }
        }
    }
}

/// Whether `dep` can require `flag` on: any USE dependency on it but `[-flag]`
fn wants_flag(dep: &Dep, flag: Interned<DefaultInterner>) -> bool {
    dep.blocker.is_none()
        && dep
            .use_deps
            .iter()
            .flatten()
            .any(|u| u.flag == flag && u.kind != UseDepKind::Disabled)
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
        repo(&[(
            "sys-apps/ul-1",
            &format!("IUSE={iuse}\nREQUIRED_USE={required_use}\n"),
        )])
    }

    // `(cpv, extra md5-cache lines)`, one version per package
    fn repo(entries: &[(&str, &str)]) -> RepoData {
        let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
        let mut cpns = Vec::new();
        for (cpv, extra) in entries {
            let cpv = Cpv::parse(cpv).unwrap();
            let text = format!("EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\n{extra}");
            cpns.push(cpv.cpn);
            versions
                .entry(cpv.cpn)
                .or_default()
                .push((cpv, CacheEntry::parse(&text).unwrap()));
        }
        RepoData::from_parts(
            cpns,
            versions,
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

    fn node(cpv: &str) -> (PortagePackage, Version) {
        let cpv = Cpv::parse(cpv).unwrap();
        (
            PortagePackage::slotted(cpv.cpn, Interned::intern("0")),
            cpv.version,
        )
    }

    /// `order` ends with the rebuild of `sys-apps/ul-1`
    fn split_of(order: &[&str]) -> SplitOrder {
        let mut order: Vec<_> = order.iter().map(|c| node(c)).collect();
        order.push(node("sys-apps/ul-1"));
        SplitOrder {
            rebuild: order.len() - 1,
            order,
            remaining: Vec::new(),
        }
    }

    fn check_ul(data: &RepoData, split: &SplitOrder) -> Result<(), Rejection> {
        let candidate = SplitCandidate {
            package: node("sys-apps/ul-1"),
            flag: Interned::intern("nls"),
            build_time: true,
        };
        check_order(data, split, &candidate)
    }

    #[test]
    fn a_flag_off_dependency_the_solve_did_not_pick_rejects_the_split() {
        let data = repo(&[
            ("sys-apps/ul-1", "IUSE=+nls\nRDEPEND=!nls? ( dev-libs/x )\n"),
            ("app-text/po4a-1", ""),
            ("dev-libs/x-1", ""),
        ]);
        let without = split_of(&["sys-apps/ul-1", "app-text/po4a-1"]);
        assert_eq!(
            check_ul(&data, &without),
            Err(Rejection::Unsolved(Cpn::parse("dev-libs/x").unwrap()))
        );
        let with = split_of(&["dev-libs/x-1", "sys-apps/ul-1", "app-text/po4a-1"]);
        assert_eq!(check_ul(&data, &with), Ok(()));
    }

    #[test]
    fn a_flag_demand_between_the_two_builds_rejects_the_split() {
        let data = repo(&[
            ("sys-apps/ul-1", "IUSE=+nls\n"),
            ("app-text/bdc-1", "RDEPEND=sys-apps/ul[nls]\n"),
            ("app-text/po4a-1", ""),
            ("app-text/late-1", "RDEPEND=sys-apps/ul[nls]\n"),
        ]);
        let demanded = split_of(&["sys-apps/ul-1", "app-text/bdc-1", "app-text/po4a-1"]);
        assert_eq!(
            check_ul(&data, &demanded),
            Err(Rejection::Demanded(Cpv::parse("app-text/bdc-1").unwrap()))
        );
        let mut after_rebuild = split_of(&["sys-apps/ul-1", "app-text/po4a-1"]);
        after_rebuild.order.push(node("app-text/late-1"));
        assert_eq!(check_ul(&data, &after_rebuild), Ok(()));
    }
}
