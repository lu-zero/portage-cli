//! Second-root build closure
//!
//! The build-time providers a toolchain sysroot (`base`) or build host
//! (`host`) lacks, spliced into a finished Target plan with the `--jobs N`
//! blocker edges their ordering needs. Post-solve: the solver already aliases
//! Target to Host, and a third root would triple the pubgrub package universe.

use std::collections::HashMap;

use portage_atom::{Cpn, Cpv, Version};
use portage_atom_pubgrub::{DepClass, MergeRoot, PortagePackage};

use crate::Roots;
use crate::effective_use;
use crate::repo::Adapter;
use crate::root_aware::CrossContext;
use crate::{Avail, all_cpns, unsatisfied_cpns};

/// A finished Target plan with a second root's missing build closure spliced in
pub struct Plan {
    /// The whole plan, closure entries interleaved deps-first in front of the
    /// entries that need them — never just the new entries
    pub order: Vec<(PortagePackage, Version)>,
    /// `(consumer, dependency)` pairs for `--jobs N`, which builds its
    /// ready-set from solver-time edges predating these synthetic entries and
    /// so cannot see list position. Every pair points strictly backwards in
    /// `order`.
    pub blockers: Vec<(PortagePackage, PortagePackage)>,
}

/// What separates the two roots: how new entries are stamped, and which dep
/// classes are examined on a plan entry (`top`) versus on a closure node
/// discovered under one (`deep`)
struct Spec {
    stamp: MergeRoot,
    top: &'static [DepClass],
    deep: &'static [DepClass],
}

const BASE: Spec = Spec {
    stamp: MergeRoot::Base,
    top: &[DepClass::Depend],
    // A copy's shared library needs its own runtime deps in the same sysroot
    // for the linker to resolve them.
    deep: &[DepClass::Depend, DepClass::Rdepend],
};

const HOST: Spec = Spec {
    stamp: MergeRoot::Host,
    top: &[DepClass::Depend, DepClass::Bdepend, DepClass::Idepend],
    deep: &[DepClass::Depend, DepClass::Bdepend, DepClass::Idepend],
};

/// Splice the toolchain sysroot's missing `DEPEND` closure
/// (`MergeRoot::Base`) into `target_order` — the board-root topology, where
/// PMS table 8.2's `SYSROOT` is a separate filesystem from `ROOT` and a
/// provider merged only into `ROOT` is invisible to the compiler.
/// Passthrough when [`Roots::base_merge_root`] is `None`.
pub fn base(
    target_order: &[(PortagePackage, Version)],
    adapter: &Adapter<'_>,
    roots: &Roots,
) -> Plan {
    if roots.base_merge_root().is_none() {
        return passthrough(target_order);
    }
    run(
        target_order,
        adapter,
        Avail::initial_base_depend(roots),
        &BASE,
    )
}

/// Splice the build host's missing `DEPEND`/`BDEPEND`/`IDEPEND` closure
/// (`MergeRoot::Host`) into `target_order` — a native offset
/// (`--root`/`--prefix`, same arch), where a build-time provider the host
/// lacks must be built there first. Passthrough for everything else:
/// cross-arch goes through the solver's own dual-root path.
pub fn host(
    target_order: &[(PortagePackage, Version)],
    adapter: &Adapter<'_>,
    roots: &Roots,
    cross: &CrossContext,
) -> Plan {
    if !cross.active || cross.is_cross_arch() {
        return passthrough(target_order);
    }
    run(target_order, adapter, Avail::initial_bdepend(roots), &HOST)
}

fn passthrough(target_order: &[(PortagePackage, Version)]) -> Plan {
    Plan {
        order: target_order.to_vec(),
        blockers: Vec::new(),
    }
}

/// Static inputs shared across the walk
struct Ctx<'a> {
    adapter: &'a Adapter<'a>,
    target_ver: &'a HashMap<Cpn, (Version, PortagePackage)>,
    spec: &'a Spec,
}

/// The build plan as a graph: `target_order` as immovable anchors (ids
/// `0..fixed`), plus whatever closure nodes discovery appends
struct Graph {
    fixed: usize,
    node: Vec<(PortagePackage, Version)>,
    /// `needs[u]` — nodes that must build before `u`
    needs: Vec<Vec<usize>>,
    /// `None` records a CPN [`resolve`] already failed on, so it is not retried
    of_cpn: HashMap<Cpn, Option<usize>>,
}

fn run(
    target_order: &[(PortagePackage, Version)],
    adapter: &Adapter<'_>,
    mut avail: Avail,
    spec: &Spec,
) -> Plan {
    // A closure entry for a CPN also built for Target reuses the Target
    // version: both satisfy the same atom for the same consumers in the same
    // run, and a version split would break the `:=` subslot invariant.
    let target_ver: HashMap<Cpn, (Version, PortagePackage)> = target_order
        .iter()
        .filter(|(p, _)| p.merge_root() == MergeRoot::Target)
        .map(|(p, v)| (*p.cpn(), (v.clone(), p.clone())))
        .collect();
    let ctx = Ctx {
        adapter,
        target_ver: &target_ver,
        spec,
    };
    let mut graph = Graph {
        fixed: target_order.len(),
        node: target_order.to_vec(),
        needs: vec![Vec::new(); target_order.len()],
        of_cpn: HashMap::new(),
    };

    // Seed from entries the solver already stamped for this root, wherever
    // they sit: an earlier Target entry can depend on a later seed, so
    // availability must see every one before the walk starts.
    for (i, (pkg, ver)) in target_order.iter().enumerate() {
        if pkg.merge_root() == spec.stamp {
            graph.of_cpn.insert(*pkg.cpn(), Some(i));
            avail.record_merge(Cpv::new(*pkg.cpn(), ver.clone()), spec.stamp);
        }
    }

    for i in 0..graph.fixed {
        if graph.node[i].0.merge_root() == MergeRoot::Target {
            visit(&ctx, &mut graph, &mut avail, i, spec.top, true);
        }
    }
    emit(&graph, spec.stamp)
}

/// Create a node for every dep of `u` this root lacks, recursing into each
/// before moving on, then record every dep of `u` that resolves to a node
///
/// Recording the node (and its availability) *before* recursing is what lets
/// a cycle among closure nodes still produce its back edge as a fact.
fn visit(
    ctx: &Ctx<'_>,
    graph: &mut Graph,
    avail: &mut Avail,
    u: usize,
    classes: &[DepClass],
    top_level: bool,
) {
    let (pkg, ver) = graph.node[u].clone();
    let Some(deps) =
        effective_use::evaluated_deps(ctx.adapter.data, &ctx.adapter.policy(), &pkg, &ver, false)
    else {
        return;
    };
    let mut entries = Vec::new();
    for &class in classes {
        let listed = match class {
            DepClass::Depend => deps.depend(),
            DepClass::Rdepend => deps.rdepend(),
            DepClass::Bdepend => deps.bdepend(),
            DepClass::Pdepend => deps.pdepend(),
            DepClass::Idepend => deps.idepend(),
        };
        for cpn in unsatisfied_cpns(&listed, avail) {
            if graph.of_cpn.contains_key(&cpn) {
                continue;
            }
            // Developer signal, not user-facing: the solver should already
            // cover a plan entry's own host tools, so a hit here means that
            // path missed one.
            if top_level && class != DepClass::Depend {
                let stamp = ctx.spec.stamp;
                tracing::debug!(
                    "root_closure {stamp:?}: top-level {class} gap for {cpn} (from {pkg})"
                );
            }
            let Some((cver, cpkg)) = resolve(cpn, ctx) else {
                graph.of_cpn.insert(cpn, None);
                continue;
            };
            let v = graph.node.len();
            graph
                .node
                .push((cpkg.at_merge_root(ctx.spec.stamp), cver.clone()));
            graph.needs.push(Vec::new());
            graph.of_cpn.insert(cpn, Some(v));
            avail.record_merge(Cpv::new(cpn, cver), ctx.spec.stamp);
            visit(ctx, graph, avail, v, ctx.spec.deep, false);
        }
        entries.extend(listed);
    }
    // Source dependency-string order, never sorted or routed through a set:
    // the emission DFS follows this list, so the final plan order depends on
    // it. A dep already satisfied by an earlier consumer's node still belongs
    // here — `u` cannot build until that node is done either.
    for cpn in all_cpns(&entries) {
        if let Some(Some(v)) = graph.of_cpn.get(&cpn) {
            let v = *v;
            if !graph.needs[u].contains(&v) {
                graph.needs[u].push(v);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum State {
    New,
    Open,
    Done,
}

/// Linearize the graph: one DFS post-order per anchor, in anchor order, so a
/// new node lands immediately before the first anchor whose closure reached it
fn emit(graph: &Graph, stamp: MergeRoot) -> Plan {
    let mut state = vec![State::New; graph.node.len()];
    let mut pos = vec![usize::MAX; graph.node.len()];
    let mut order = Vec::with_capacity(graph.node.len());
    for root in 0..graph.fixed {
        emit_node(graph, root, stamp, &mut state, &mut pos, &mut order);
    }
    let blockers = graph
        .needs
        .iter()
        .enumerate()
        .flat_map(|(u, vs)| vs.iter().map(move |&v| (u, v)))
        .filter(|&(u, v)| pos[v] < pos[u])
        .map(|(u, v)| (graph.node[u].0.clone(), graph.node[v].0.clone()))
        .collect();
    Plan { order, blockers }
}

fn emit_node(
    graph: &Graph,
    u: usize,
    stamp: MergeRoot,
    state: &mut [State],
    pos: &mut [usize],
    order: &mut Vec<(PortagePackage, Version)>,
) {
    state[u] = State::Open;
    for &v in &graph.needs[u] {
        // An anchor already sits at its own position and is never moved.
        if v < graph.fixed {
            continue;
        }
        match state[v] {
            State::New => emit_node(graph, v, stamp, state, pos, order),
            State::Open => {
                let (from, to) = (&graph.node[u].0, &graph.node[v].0);
                tracing::debug!(
                    "root_closure {stamp:?}: cycle, {from} needs {to} — edge unordered"
                );
            }
            State::Done => {}
        }
    }
    state[u] = State::Done;
    pos[u] = order.len();
    order.push(graph.node[u].clone());
}

/// Resolve `(version, package)` for a closure entry on `cpn`
///
/// The Target plan's version when the CPN is also built for Target, else the
/// newest keyword/mask/license-accepted repo version (`ctx.adapter`'s
/// `accept_keywords` is already scoped to the arch the entry builds at).
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
    use std::collections::HashSet;

    use portage_metadata::CacheEntry;
    use portage_repo::{AcceptSet, LicenseGroupRegistry};

    use super::*;
    use crate::force_mask::ForceMask;
    use crate::repo::{self, AcceptKeywords, AcceptOverlay, AcceptProperties, AcceptRestrict};
    use crate::root_aware;

    fn empty_layer() -> &'static portage_atom_pubgrub::UseLayer {
        use std::sync::OnceLock;
        static E: OnceLock<portage_atom_pubgrub::UseLayer> = OnceLock::new();
        E.get_or_init(portage_atom_pubgrub::UseLayer::default)
    }

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
        ($name:ident, $data:expr, $arch:expr) => {
            let arch = gentoo_core::Arch::intern($arch);
            let accept_keywords = AcceptKeywords::from_global(&arch, &[$arch]);
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

    fn host_pkg(cpn: &str) -> PortagePackage {
        pkg(cpn).at_merge_root(MergeRoot::Host)
    }

    fn ver(v: &str) -> Version {
        Version::parse(v).unwrap()
    }

    // The board-root topology: a separate toolchain sysroot and board root.
    // The `TempDir`s come back with it — dropping them removes the roots.
    fn board_roots() -> (tempfile::TempDir, tempfile::TempDir, Roots) {
        let sysroot = tempfile::tempdir().unwrap();
        let board = tempfile::tempdir().unwrap();
        let roots = Roots::for_test_board_root(
            sysroot.path().to_str().unwrap(),
            board.path().to_str().unwrap(),
        );
        (sysroot, board, roots)
    }

    // A `--prefix`-shaped native offset: `cross.active` (target != host) but
    // not cross-arch (no make.conf to read a foreign `CHOST` from)
    fn offset_roots() -> (tempfile::TempDir, tempfile::TempDir, Roots, CrossContext) {
        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        let roots = Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let cross = root_aware::detect(&roots, roots.merge_root());
        (host, prefix, roots, cross)
    }

    fn names(plan: &Plan) -> Vec<String> {
        plan.order
            .iter()
            .map(|(p, _)| p.cpn().to_string())
            .collect()
    }

    fn edge_names(plan: &Plan) -> Vec<(String, String)> {
        plan.blockers
            .iter()
            .map(|(from, to)| (from.cpn().to_string(), to.cpn().to_string()))
            .collect()
    }

    // The incident this walk exists for: readline's DEPEND on ncurses must
    // get a sysroot entry, landing immediately before readline — matching
    // real Portage's own three-entry output for the same invocation.
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

        let (_sysroot, _board, roots) = board_roots();
        test_adapter!(a, &data, "x86");
        let plan = base(&target_order, &a, &roots);
        let got: Vec<(String, String, MergeRoot)> = plan
            .order
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

    // A provider already present in the sysroot's own VDB needs no entry.
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

        let (sysroot, _board, roots) = board_roots();
        write_fake_vdb_entry(sysroot.path(), "sys-libs/ncurses-6.5");

        test_adapter!(a, &data, "x86");
        assert_eq!(base(&target_order, &a, &roots).order, target_order);
    }

    // BDEPEND is a build-host tool concern (`host`'s job), never routed to
    // the sysroot.
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

        let (_sysroot, _board, roots) = board_roots();
        test_adapter!(a, &data, "x86");
        assert_eq!(base(&target_order, &a, &roots).order, target_order);
    }

    // A Target entry's own RDEPEND is never examined (only DEPEND is) — but
    // once a sysroot entry is scheduled, *its* RDEPEND is followed, at every
    // depth, since its shared libraries need their own runtime deps in the
    // same sysroot for the linker to resolve.
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

        let (_sysroot, _board, roots) = board_roots();
        test_adapter!(a, &data, "x86");
        let plan = base(&target_order, &a, &roots);
        let names = names(&plan);
        assert!(
            names.contains(&"dev-libs/libx".to_string()),
            "t's own DEPEND must get a sysroot entry: {names:?}"
        );
        assert!(
            names.contains(&"dev-libs/liby".to_string()),
            "libx's RDEPEND must be followed: {names:?}"
        );
        assert!(
            !names.contains(&"dev-libs/libz".to_string()),
            "t's own RDEPEND must NOT trigger a sysroot entry: {names:?}"
        );
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(pos("dev-libs/liby") < pos("dev-libs/libx"));
        assert!(pos("dev-libs/libx") < pos("sys-apps/t"));

        // The `--jobs N` scheduler needs these as real blocker edges, not
        // just list position.
        let edges = edge_names(&plan);
        assert!(
            edges.contains(&("sys-apps/t".to_string(), "dev-libs/libx".to_string())),
            "t must block on its libx entry: {edges:?}"
        );
        assert!(
            edges.contains(&("dev-libs/libx".to_string(), "dev-libs/liby".to_string())),
            "libx must block on its own liby entry: {edges:?}"
        );
    }

    // A dep shared by two consumers (one direct, one via another entry's own
    // closure) is scheduled exactly once and lands before its first consumer.
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

        let (_sysroot, _board, roots) = board_roots();
        test_adapter!(a, &data, "x86");
        let plan = base(&target_order, &a, &roots);
        let names = names(&plan);
        assert_eq!(
            names.iter().filter(|n| *n == "dev-libs/liba").count(),
            1,
            "liba must not be duplicated: {names:?}"
        );
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(pos("dev-libs/liba") < pos("sys-apps/t1"));
        assert!(pos("dev-libs/liba") < pos("dev-libs/libb"));
        assert!(pos("dev-libs/libb") < pos("sys-apps/t2"));

        // libb's own DEPEND on liba is already satisfied by the time t2's
        // walk reaches it, so no *new* entry is scheduled for it — but libb
        // still cannot build until liba's is done.
        let edges = edge_names(&plan);
        assert!(
            edges.contains(&("sys-apps/t1".to_string(), "dev-libs/liba".to_string())),
            "t1 must block on its liba entry: {edges:?}"
        );
        assert!(
            edges.contains(&("dev-libs/libb".to_string(), "dev-libs/liba".to_string())),
            "libb must still block on liba, scheduled earlier for a different \
             consumer: {edges:?}"
        );
        assert!(
            edges.contains(&("sys-apps/t2".to_string(), "dev-libs/libb".to_string())),
            "t2 must block on its own libb entry: {edges:?}"
        );
    }

    // Every other topology must pass `target_order` through unchanged.
    #[test]
    fn no_op_for_a_native_offset_and_for_a_plain_target() {
        let data = repo_from(&[(
            "sys-libs/ncurses-6.5",
            "EAPI=8\nSLOT=0\nKEYWORDS=x86\nDESCRIPTION=t\n",
        )]);
        let target_order = vec![(pkg("sys-libs/ncurses"), ver("6.5"))];

        test_adapter!(a, &data, "x86");
        let dir = tempfile::tempdir().unwrap();
        let plain = Roots::for_test(dir.path().to_str().unwrap());
        assert_eq!(base(&target_order, &a, &plain).order, target_order);

        let broot = tempfile::tempdir().unwrap();
        let native_offset = Roots::for_test_root_with_broot(
            dir.path().to_str().unwrap(),
            broot.path().to_str().unwrap(),
        );
        assert_eq!(base(&target_order, &a, &native_offset).order, target_order);
    }

    // Regression test for the `dev-perl/Digest-HMAC` duplicate-plan-entry
    // incident: when the solver's own dual-root expansion already scheduled a
    // `MergeRoot::Host` node for a CPN (crossdev's host-arch tools), `host`
    // must not re-derive or duplicate it. Before the fix this produced a
    // second, independently-versioned, anti-topologically ordered entry.
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
        // The second entry stands in for the solver's own
        // `append_unsatisfied_broot` output: already scheduled `@host`.
        let target_order = vec![
            (pkg("sys-apps/consumer"), ver("1.0")),
            (host_pkg("dev-libs/tool"), ver("2.0")),
        ];

        let (_host, _prefix, roots, cross) = offset_roots();
        assert!(
            cross.active && !cross.is_cross_arch(),
            "test setup must land in the native-offset case `host` exists for"
        );

        test_adapter!(a, &data, "amd64");
        assert_eq!(
            host(&target_order, &a, &roots, &cross).order,
            target_order,
            "must not re-derive a CPN the solver already scheduled @host"
        );
    }

    // A genuinely unsolvable input: `consumer` needs `tool` needs `base`, but
    // `base` is a seeded `MergeRoot::Host` entry the solver placed *after*
    // `consumer`, and a seeded entry is never repositioned. No linear order
    // satisfies that; `preflight::check` is what catches it. Pinned so a
    // future change that starts silently reordering seeded entries is caught.
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
        // `base` is seeded @host but deliberately not at the front.
        let target_order = vec![
            (pkg("sys-apps/consumer"), ver("1.0")),
            (host_pkg("dev-libs/base"), ver("1.0")),
        ];

        let (_host, _prefix, roots, cross) = offset_roots();
        test_adapter!(a, &data, "amd64");
        let plan = host(&target_order, &a, &roots, &cross);
        let names = names(&plan);
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(
            pos("dev-libs/tool") < pos("sys-apps/consumer"),
            "tool must still land before consumer, its one solver-known consumer: {names:?}"
        );
    }

    // The invariant `blockers` carries: every pair points strictly backwards
    // in `order`. The fixture above is the interesting case — `tool` needs a
    // seeded entry positioned after it, and no placement of `tool` alone can
    // fix that, so the edge is dropped rather than left to starve `--jobs N`.
    #[test]
    fn every_blocker_points_strictly_backwards_in_the_final_order() {
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
        let target_order = vec![
            (pkg("sys-apps/consumer"), ver("1.0")),
            (host_pkg("dev-libs/base"), ver("1.0")),
        ];

        let (_host, _prefix, roots, cross) = offset_roots();
        test_adapter!(a, &data, "amd64");
        let plan = host(&target_order, &a, &roots, &cross);
        let names = names(&plan);
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(
            !edge_names(&plan)
                .contains(&("dev-libs/tool".to_string(), "dev-libs/base".to_string())),
            "tool's edge onto a later-positioned seed must be dropped: {names:?}"
        );
        for (from, to) in &plan.blockers {
            assert!(
                pos(&to.cpn().to_string()) < pos(&from.cpn().to_string()),
                "every blocker must point strictly backwards: {from} -> {to} in {names:?}"
            );
        }
    }

    // Two *different* Target entries share a host dependency chain
    // (`t2 -> libb -> liba`, `t1 -> liba` directly). `liba` is resolved once
    // (under t1) and must not be re-derived for t2 — but `libb` (discovered
    // later) still depends on it and must land after it, despite `liba` no
    // longer being unsatisfied by the time `libb` is visited.
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
        let target_order = vec![
            (pkg("sys-apps/t1"), ver("1.0")),
            (pkg("sys-apps/t2"), ver("1.0")),
        ];

        let (_host, _prefix, roots, cross) = offset_roots();
        test_adapter!(a, &data, "amd64");
        let plan = host(&target_order, &a, &roots, &cross);
        let names = names(&plan);
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

        let edges = edge_names(&plan);
        assert!(
            edges.contains(&("dev-libs/libb".to_string(), "dev-libs/liba".to_string())),
            "libb must still block on liba, scheduled earlier for a different \
             consumer: {edges:?}"
        );
    }

    // A fan-out with a shared sync point: two independent consumers whose own
    // dependencies both need `l`. `l` is one node, built once, before both.
    #[test]
    fn two_consumers_fan_out_to_one_shared_sync_point() {
        let data = repo_from(&[
            (
                "sys-apps/t1-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/m\n",
            ),
            (
                "sys-apps/t2-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/n\n",
            ),
            (
                "dev-libs/m-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/l\n",
            ),
            (
                "dev-libs/n-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/l\n",
            ),
            (
                "dev-libs/l-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\n",
            ),
        ]);
        let target_order = vec![
            (pkg("sys-apps/t1"), ver("1.0")),
            (pkg("sys-apps/t2"), ver("1.0")),
        ];

        let (_host, _prefix, roots, cross) = offset_roots();
        test_adapter!(a, &data, "amd64");
        let plan = host(&target_order, &a, &roots, &cross);
        let names = names(&plan);
        assert_eq!(
            names.iter().filter(|n| *n == "dev-libs/l").count(),
            1,
            "the shared sync point must be one node: {names:?}"
        );
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(pos("dev-libs/l") < pos("dev-libs/m"), "{names:?}");
        assert!(pos("dev-libs/l") < pos("dev-libs/n"), "{names:?}");
        assert!(pos("dev-libs/m") < pos("sys-apps/t1"), "{names:?}");
        assert!(pos("dev-libs/n") < pos("sys-apps/t2"), "{names:?}");

        let edges = edge_names(&plan);
        for pair in [
            ("dev-libs/m", "dev-libs/l"),
            ("dev-libs/n", "dev-libs/l"),
            ("sys-apps/t1", "dev-libs/m"),
            ("sys-apps/t2", "dev-libs/n"),
        ] {
            let pair = (pair.0.to_string(), pair.1.to_string());
            assert!(edges.contains(&pair), "missing {pair:?} in {edges:?}");
        }
    }

    // A cycle among closure nodes: both are scheduled once, deps-first as far
    // as the cycle allows, and only the back edge is dropped from `blockers`.
    #[test]
    fn a_cycle_among_closure_nodes_drops_only_the_back_edge() {
        let data = repo_from(&[
            (
                "sys-apps/t-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/a\n",
            ),
            (
                "dev-libs/a-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/b\n",
            ),
            (
                "dev-libs/b-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/a\n",
            ),
        ]);
        let target_order = vec![(pkg("sys-apps/t"), ver("1.0"))];

        let (_host, _prefix, roots, cross) = offset_roots();
        test_adapter!(a, &data, "amd64");
        let plan = host(&target_order, &a, &roots, &cross);
        let names = names(&plan);
        for n in ["dev-libs/a", "dev-libs/b"] {
            assert_eq!(
                names.iter().filter(|x| *x == n).count(),
                1,
                "{n} must be scheduled exactly once: {names:?}"
            );
        }
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(pos("dev-libs/b") < pos("dev-libs/a"), "{names:?}");
        assert!(pos("dev-libs/a") < pos("sys-apps/t"), "{names:?}");

        let edges = edge_names(&plan);
        assert!(
            edges.contains(&("dev-libs/a".to_string(), "dev-libs/b".to_string())),
            "a must block on b: {edges:?}"
        );
        assert!(
            edges.contains(&("sys-apps/t".to_string(), "dev-libs/a".to_string())),
            "t must block on a: {edges:?}"
        );
        assert!(
            !edges.contains(&("dev-libs/b".to_string(), "dev-libs/a".to_string())),
            "the cycle's back edge must be dropped: {edges:?}"
        );
    }
}
