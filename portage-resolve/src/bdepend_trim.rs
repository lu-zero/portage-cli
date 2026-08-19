//! Post-solve trim: drop plan entries only pulled for `BDEPEND` already
//! satisfied on BROOT or by earlier within-run merges.

use std::collections::HashSet;

use portage_atom::{Cpn, Cpv, DepEntry, Version};
use portage_atom_pubgrub::PortagePackage;

use crate::Avail;
use crate::Roots;

use crate::effective_use;
use crate::repo::{RepoData, ResolvePolicy};

/// Context for [`trim_within_run_bdepend`].
pub struct TrimCtx<'a> {
    /// See [`crate::Avail::initial_bdepend`] — carries BROOT
    /// via `satisfaction_root(DepClass::Bdepend)` even under an active
    /// `--target` sysroot substitution, so this is the same `Roots` the
    /// caller already has for `DEPEND`, not a separately-picked one.
    pub roots: &'a Roots,
    /// The loaded repository facts.
    pub data: &'a RepoData,
    /// The run's keyword/mask/license/USE policy (the trim only reads its
    /// USE-fold fields, via [`effective_use::evaluated_deps`]).
    pub policy: ResolvePolicy<'a>,
    /// CPNs explicitly requested on the command line — never trimmed.
    pub root_cpns: &'a HashSet<Cpn>,
    /// CPNs the solver kept for a same-version USE rebuild — never trimmed.
    pub reinstall_cpns: &'a HashSet<Cpn>,
}

/// Drop entries that are only needed for `BDEPEND` edges already satisfied by
/// the host/prefix VDB or earlier kept plan entries. No-op when the solver did
/// not include `BDEPEND` (`with_bdeps=false`).
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

    let runtime_required = runtime_required_cpns(full_solution_order, ctx);
    // Computed once: the BROOT/prefix VDB scan this pass checks BDEPEND
    // satisfaction against never changes across the whole trim (no merges
    // happen here, just filtering decisions) — `avail_for_consumer` used to
    // rebuild this from scratch (a fresh VDB directory scan) for every
    // (candidate, consumer) pair it examined, up to O(n²) scans for an
    // n-package plan. Measured as the single largest allocation source in
    // this codebase (dhat: 110MB across 322 calls on a real firefox
    // resolve). Cloning this cheap, lazy (`AvailEntry::installed` isn't
    // eagerly read) base is far cheaper than re-scanning the VDB directory
    // each time.
    let base_avail = Avail::initial_bdepend(ctx.roots);
    let mut kept: Vec<(PortagePackage, Version)> = Vec::with_capacity(order.len());
    let mut kept_indices: Vec<usize> = Vec::with_capacity(order.len());

    for (i, (pkg, ver)) in order.iter().enumerate() {
        let cand = TrimCandidate {
            index: i,
            pkg,
            order: &order,
            kept: &kept,
            kept_indices: &kept_indices,
            ctx,
            runtime_required: &runtime_required,
            base_avail: &base_avail,
        };
        if should_keep(&cand) {
            kept.push((pkg.clone(), ver.clone()));
            kept_indices.push(i);
        }
    }

    kept
}

fn runtime_required_cpns(order: &[(PortagePackage, Version)], ctx: &TrimCtx<'_>) -> HashSet<Cpn> {
    let mut out = HashSet::new();
    for (pkg, ver) in order {
        let Some(deps) = effective_use::evaluated_deps(ctx.data, &ctx.policy, pkg, ver, false)
        else {
            continue;
        };
        for entries in [
            deps.depend(),
            deps.rdepend(),
            deps.pdepend(),
            deps.idepend(),
        ] {
            collect_cpns_from_entries(&entries, &mut out);
        }
    }
    out
}

fn collect_cpns_from_entries(entries: &[DepEntry], out: &mut HashSet<Cpn>) {
    for e in entries {
        match e {
            DepEntry::Atom(dep) if dep.blocker.is_none() => {
                out.insert(dep.cpn);
            }
            DepEntry::AllOf(c) | DepEntry::AnyOf(c) => collect_cpns_from_entries(c, out),
            DepEntry::ExactlyOneOf(c) | DepEntry::AtMostOneOf(c) => {
                collect_cpns_from_entries(c, out);
            }
            _ => {}
        }
    }
}

struct TrimCandidate<'a, 'b> {
    index: usize,
    pkg: &'a PortagePackage,
    order: &'a [(PortagePackage, Version)],
    kept: &'a [(PortagePackage, Version)],
    kept_indices: &'a [usize],
    ctx: &'a TrimCtx<'b>,
    runtime_required: &'a HashSet<Cpn>,
    base_avail: &'a Avail,
}

fn should_keep(cand: &TrimCandidate<'_, '_>) -> bool {
    let cpn = *cand.pkg.cpn();
    if cand.ctx.root_cpns.contains(&cpn) || cand.ctx.reinstall_cpns.contains(&cpn) {
        return true;
    }
    if cand.runtime_required.contains(&cpn) {
        return true;
    }

    for (j, (consumer, consumer_ver)) in cand.order.iter().enumerate().skip(cand.index + 1) {
        let avail = avail_for_consumer(j, cand.kept, cand.kept_indices, cand.base_avail);
        let Some(deps) = effective_use::evaluated_deps(
            cand.ctx.data,
            &cand.ctx.policy,
            consumer,
            consumer_ver,
            false,
        ) else {
            continue;
        };
        if avail.has_unsatisfied_atom_for_cpn(&deps.bdepend(), cpn) {
            return true;
        }
    }

    false
}

fn avail_for_consumer(
    consumer_index: usize,
    kept: &[(PortagePackage, Version)],
    kept_indices: &[usize],
    base_avail: &Avail,
) -> Avail {
    let mut avail = base_avail.clone();
    for (k, (pkg, ver)) in kept.iter().enumerate() {
        if kept_indices[k] < consumer_index {
            let cpv = Cpv::new(*pkg.cpn(), ver.clone());
            avail.record_merge(cpv, pkg.merge_root());
        }
    }
    avail
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

    /// The permissive keyword/license policy the trim tests fold USE under.
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
            pre_env: empty_layer(),
            env_use: empty_layer(),
            package_use: &[],
            profile_package_use: &[],
            force_mask,
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

    /// Build a `RepoData` from `(cpv, md5-cache-text)` pairs, one version per CPN.
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
        let fm = ForceMask::default();
        let (ak, al) = (accept_amd64(), accept_all_licenses());
        let ctx = TrimCtx {
            roots: &roots,
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
        let fm = ForceMask::default();
        let (ak, al) = (accept_amd64(), accept_all_licenses());
        let ctx = TrimCtx {
            roots: &roots,
            data: &data,
            policy: test_policy(&ak, &al, &fm),
            root_cpns: &root_cpns,
            reinstall_cpns: &reinstall,
        };
        let out = trim_within_run_bdepend(order.clone(), &order, false, &ctx);
        assert_eq!(out.len(), order.len());
    }
}
