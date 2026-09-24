//! Post-solve trim: drop plan entries only pulled for `BDEPEND` already
//! satisfied on BROOT or by earlier within-run merges

use std::collections::{HashMap, HashSet};

use portage_atom::{Cpn, Cpv, DepEntry, Version};
use portage_atom_pubgrub::PortagePackage;

use crate::Avail;
use crate::installed::BrootSnapshot;

use crate::effective_use;
use crate::repo::{RepoData, ResolvePolicy};

/// Context for [`trim_within_run_bdepend`]
pub struct TrimCtx<'a> {
    /// Raw BROOT/prefix rows captured once for this resolver invocation.
    /// The snapshot carries the root selection and ordering, so this context
    /// has no second `Roots` authority.
    pub broot_snapshot: &'a BrootSnapshot,
    /// The loaded repository facts
    pub data: &'a RepoData,
    /// The run's keyword/mask/license/USE policy (the trim only reads its
    /// USE-fold fields, via [`effective_use::evaluated_deps`])
    pub policy: ResolvePolicy<'a>,
    /// CPNs explicitly requested on the command line — never trimmed
    pub root_cpns: &'a HashSet<Cpn>,
    /// CPNs the solver kept for a same-version USE rebuild — never trimmed
    pub reinstall_cpns: &'a HashSet<Cpn>,
}

/// Drop entries only needed for already-satisfied `BDEPEND` edges
///
/// "Already satisfied" means by the host/prefix VDB or earlier kept plan
/// entries.
///
/// No-op when the solver did not include `BDEPEND` (`with_bdeps=false`).
///
/// `full_solution_order` is every real package the solver selected, *before*
/// the caller's "already installed, nothing to display" filter drops entries
/// like `virtual/libcrypt` from `order`. Runtime-requirement scanning must
/// use the full set: an already-installed package invisible in `order` can
/// still be the sole reason some other package is required. Scanning only
/// `order` made such a dependency look orphaned and wrongly trimmable.
pub fn trim_within_run_bdepend(
    order: Vec<(PortagePackage, Version)>,
    full_solution_order: &[(PortagePackage, Version)],
    with_bdeps: bool,
    ctx: &TrimCtx<'_>,
) -> Vec<(PortagePackage, Version)> {
    if !with_bdeps || order.is_empty() {
        return order;
    }

    let mut evaluated = effective_use::EvaluatedDepsCache::new(ctx.data, ctx.policy, false);
    let runtime_required = runtime_required_cpns(full_solution_order, &mut evaluated);
    let evaluated_order = evaluated.snapshot(&order);
    let consumers = bdepend_consumers(&evaluated_order);
    // The VDB is stable for this filtering pass; the prefix view below shares
    // this base and adds only plan entries earlier than the consumer.
    let base_avail = Avail::initial_bdepend_from_snapshot(ctx.broot_snapshot);
    let mut kept_cpvs: Vec<Cpv> = Vec::with_capacity(order.len());
    let mut kept_indices: Vec<usize> = Vec::with_capacity(order.len());

    for (i, (pkg, ver)) in order.iter().enumerate() {
        let cand = TrimCandidate {
            index: i,
            pkg,
            kept_cpvs: &kept_cpvs,
            kept_indices: &kept_indices,
            ctx,
            runtime_required: &runtime_required,
            base_avail: &base_avail,
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

fn runtime_required_cpns(
    order: &[(PortagePackage, Version)],
    evaluated: &mut effective_use::EvaluatedDepsCache<'_>,
) -> HashSet<Cpn> {
    let mut out = HashSet::new();
    for (pkg, ver) in order {
        let Some(deps) = evaluated.get(pkg, ver) else {
            continue;
        };
        for entries in [
            deps.depend(),
            deps.rdepend(),
            deps.pdepend(),
            deps.idepend(),
        ] {
            collect_cpns_from_entries(entries, &mut out);
        }
    }
    out
}

fn collect_cpns_from_entries(entries: &[DepEntry], out: &mut HashSet<Cpn>) {
    DepEntry::walk_atoms(entries, &mut |dep| {
        if dep.blocker.is_none() {
            out.insert(dep.cpn);
        }
    });
}

fn bdepend_consumers<'a, 'b>(
    evaluated: &'a [Option<&'a effective_use::EvaluatedDeps<'b>>],
) -> HashMap<Cpn, Vec<usize>> {
    let mut out: HashMap<Cpn, Vec<usize>> = HashMap::new();
    for (index, deps) in evaluated.iter().enumerate() {
        let Some(deps) = deps else {
            continue;
        };
        // Keep every mentioned CPN; group satisfaction remains authoritative.
        let mut cpns = HashSet::new();
        collect_cpns_from_entries(deps.bdepend(), &mut cpns);
        for cpn in cpns {
            out.entry(cpn).or_default().push(index);
        }
    }
    out
}

struct TrimCandidate<'a, 'b> {
    index: usize,
    pkg: &'a PortagePackage,
    kept_cpvs: &'a [Cpv],
    kept_indices: &'a [usize],
    ctx: &'a TrimCtx<'b>,
    runtime_required: &'a HashSet<Cpn>,
    base_avail: &'a Avail,
    evaluated: &'a [Option<&'a effective_use::EvaluatedDeps<'b>>],
    consumers: &'a HashMap<Cpn, Vec<usize>>,
}

fn should_keep(cand: &TrimCandidate<'_, '_>) -> bool {
    let cpn = *cand.pkg.cpn();
    if cand.ctx.root_cpns.contains(&cpn) || cand.ctx.reinstall_cpns.contains(&cpn) {
        return true;
    }
    if cand.runtime_required.contains(&cpn) {
        return true;
    }

    let Some(consumers) = cand.consumers.get(&cpn) else {
        return false;
    };
    for &j in consumers {
        if j <= cand.index {
            continue;
        }
        // Kept indices are appended in plan order, so entries before `j` form a prefix.
        let prefix_len = cand.kept_indices.partition_point(|&index| index < j);
        let avail = crate::bdepend_avail::AvailPrefix::new(
            Some(cand.base_avail),
            &cand.kept_cpvs[..prefix_len],
        );
        let Some(deps) = cand.evaluated.get(j).and_then(Option::as_ref) else {
            continue;
        };
        if avail.has_unsatisfied_atom_for_cpn(deps.bdepend(), cpn) {
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

    use portage_metadata::CacheEntry;
    use portage_repo::{AcceptSet, LicenseGroupRegistry};

    use super::*;
    use crate::Roots;
    use crate::force_mask::ForceMask;
    use crate::repo::{AcceptKeywords, AcceptOverlay};

    fn empty_roots() -> Roots {
        Roots::default()
    }

    // The permissive keyword/license policy the trim tests fold USE under
    fn test_policy<'a>(
        accept_keywords: &'a AcceptKeywords,
        accept_licenses: &'a AcceptOverlay,
        force_mask: &'a ForceMask,
    ) -> ResolvePolicy<'a> {
        ResolvePolicy {
            accept_keywords,
            package_mask: &[],
            package_unmask: &[],
            accept_licenses,
            accept_properties: accept_licenses,
            accept_restrict: accept_licenses,
            defaults: empty_layer(),
            conf: empty_layer(),
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask,
            facts: None,
        }
    }

    fn accept_all_licenses() -> AcceptOverlay {
        AcceptOverlay::new(
            AcceptSet::from_tokens(&["*".into()], &LicenseGroupRegistry::default()),
            Vec::new(),
        )
    }

    fn accept_amd64() -> AcceptKeywords {
        let arch = gentoo_core::Arch::intern("amd64");
        AcceptKeywords::from_global(&arch, &["amd64"])
    }

    // Build a `RepoData` from `(cpv, md5-cache-text)` pairs, one version per CPN
    fn repo_from(entries: &[(&str, &str)]) -> RepoData {
        let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
        let mut cpns = Vec::new();
        for (cpv_str, text) in entries {
            let cpv = Cpv::parse(cpv_str).unwrap();
            let entry = CacheEntry::parse(text).unwrap();
            cpns.push(cpv.cpn);
            versions.entry(cpv.cpn).or_default().push((cpv, entry));
        }
        RepoData {
            cpns,
            versions,
            repo_name: "test".into(),
            repo_of: HashMap::new(),
            real_cpn_of: HashMap::new(),
        }
    }

    // Regression test for the bug found chasing `sys-apps/shadow` missing
    // `sys-libs/libxcrypt`: an already-installed package (here
    // `virtual/lib`, standing in for `virtual/libcrypt`) is correctly
    // excluded from the *displayed* `order` — but it's still the sole
    // reason `sys-libs/reallib` (standing in for `sys-libs/libxcrypt`) is
    // required. `runtime_required_cpns` must see `virtual/lib`'s RDEPEND
    // edge via `full_solution_order` even though `virtual/lib` itself never
    // appears in `order`, or `reallib` gets wrongly trimmed as orphaned.
    #[test]
    fn already_installed_package_excluded_from_order_still_pins_its_rdepend() {
        let data = repo_from(&[
            (
                "sys-apps/consumer-1",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nRDEPEND=virtual/lib\n",
            ),
            (
                "virtual/lib-1",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nRDEPEND=sys-libs/reallib\n",
            ),
            (
                "sys-libs/reallib-1",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\n",
            ),
        ]);

        let consumer = (
            PortagePackage::unslotted(Cpn::parse("sys-apps/consumer").unwrap()),
            Version::parse("1").unwrap(),
        );
        let virtual_lib = (
            PortagePackage::unslotted(Cpn::parse("virtual/lib").unwrap()),
            Version::parse("1").unwrap(),
        );
        let reallib = (
            PortagePackage::unslotted(Cpn::parse("sys-libs/reallib").unwrap()),
            Version::parse("1").unwrap(),
        );

        // `order`: what's actually displayed/merged — `virtual/lib` is
        // already installed and excluded, matching the real bug scenario.
        let order = vec![consumer.clone(), reallib.clone()];
        let full_solution_order = vec![consumer.clone(), virtual_lib, reallib.clone()];

        let root_cpns: HashSet<Cpn> = [*consumer.0.cpn()].into_iter().collect();
        let reinstall = HashSet::new();
        let roots = empty_roots();
        let broot_snapshot = BrootSnapshot::load(&roots);
        let fm = ForceMask::default();
        let (ak, al) = (accept_amd64(), accept_all_licenses());
        let ctx = TrimCtx {
            broot_snapshot: &broot_snapshot,
            data: &data,
            policy: test_policy(&ak, &al, &fm),
            root_cpns: &root_cpns,
            reinstall_cpns: &reinstall,
        };

        let kept = trim_within_run_bdepend(order.clone(), &full_solution_order, true, &ctx);
        assert!(
            kept.iter().any(|(p, _)| p.cpn() == reallib.0.cpn()),
            "reallib must survive: it's required via virtual/lib's RDEPEND, \
             even though virtual/lib itself isn't in the displayed order"
        );

        // Negative control: with the pre-fix behaviour (scanning only `order`,
        // which excludes `virtual/lib`), `reallib` looks orphaned and is
        // wrongly dropped — demonstrating the bug this fix closes.
        let buggy = trim_within_run_bdepend(order.clone(), &order, true, &ctx);
        assert!(
            !buggy.iter().any(|(p, _)| p.cpn() == reallib.0.cpn()),
            "sanity check: scanning only `order` reproduces the original bug"
        );
    }

    #[test]
    fn no_op_when_with_bdeps_off() {
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
        let broot_snapshot = BrootSnapshot::load(&roots);
        let fm = ForceMask::default();
        let (ak, al) = (accept_amd64(), accept_all_licenses());
        let ctx = TrimCtx {
            broot_snapshot: &broot_snapshot,
            data: &data,
            policy: test_policy(&ak, &al, &fm),
            root_cpns: &root_cpns,
            reinstall_cpns: &reinstall,
        };
        let out = trim_within_run_bdepend(order.clone(), &order, false, &ctx);
        assert_eq!(out.len(), order.len());
    }
}
