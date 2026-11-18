use std::collections::{BinaryHeap, HashMap};
use std::time::{Duration, Instant};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Version};

use crate::package::PortagePackage;
use crate::provider::{InstalledPolicy, PortageDependencyProvider};
use crate::version_set::PortageVersionSet;

// `DepClass` (the five PMS 8.2 dependency variables) is shared vocabulary,
// defined once in `portage-solver`.
pub use portage_solver::DepClass;

type OrderedNode = (String, (PortagePackage, Version));

/// A labeled edge in the dependency graph: (from_pkg, from_version) depends on
/// (to_pkg, to_version) via the given class
#[derive(Debug, Clone)]
pub struct DepEdge {
    /// The package that declares the dependency
    pub from: (PortagePackage, Version),
    /// The package that is depended upon
    pub to: (PortagePackage, Version),
    /// Which dependency class this edge belongs to
    pub class: DepClass,
    /// The USE flag in `from` that gates this dep, if it was inside `flag? ( dep )`
    pub via_use_flag:
        Option<portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>>,
}

/// Measurements collected while building and ordering a PMS 8.3 dependency graph.
/// See [PMS 8.3](https://projects.gentoo.org/pms/9/pms.html#package-dependency-specifications).
///
/// The counters describe work performed by the graph pass; they do not affect
/// the selected packages or ordering.
#[derive(Debug, Default, Clone, Copy)]
pub struct GraphStats {
    /// Number of dependency-graph builds performed for this result.
    pub graph_builds: usize,
    /// Number of selected solution nodes considered by the graph pass.
    pub graph_nodes: usize,
    /// Number of virtual nodes expanded while resolving dependency edges.
    pub virtual_expansions: usize,
    /// Number of labeled edges emitted by the graph pass.
    pub graph_edges: usize,
    /// Number of strongly connected components in the hard/soft graph.
    pub scc_count: usize,
    /// Size of the largest strongly connected component.
    pub largest_scc: usize,
    /// Number of reachability probes made while repairing soft-edge inversions.
    pub repair_reachability_calls: usize,
    /// Number of graph nodes visited by soft-repair reachability probes.
    pub repair_reachability_nodes: usize,
    /// Time spent constructing the dependency graph.
    pub graph_build_time: Duration,
}

/// The result of building and ordering the PMS 8.3 dependency graph.
/// See [PMS 8.3](https://projects.gentoo.org/pms/9/pms.html#package-dependency-specifications).
#[derive(Debug)]
pub struct InstallOrderResult {
    /// The dependency-respecting install order.
    pub order: Vec<(PortagePackage, Version)>,
    /// The labeled graph used to produce the order.
    pub edges: Vec<DepEdge>,
    /// Graph and soft-repair measurements for this pass.
    pub stats: GraphStats,
}

/// An edge the install order places backwards inside a cycle that has a build-time edge
///
/// No order satisfies such a cycle; building one package with a USE flag off first can open it.
#[derive(Debug, Clone)]
pub struct CycleSplit {
    /// The backwards edge: `broken.to` merges after `broken.from`
    pub broken: DepEdge,
    /// Flags whose removal cuts every path that closes the cycle, best first; empty when none does
    pub candidates: Vec<SplitCandidate>,
}

/// A package whose USE flag, turned off, opens a [`CycleSplit`]'s cycle
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitCandidate {
    /// The package that would be built twice, flag off and then flag on
    pub package: (PortagePackage, Version),
    /// The USE flag gating its dependencies on the cycle
    pub flag: Interned<DefaultInterner>,
    /// Whether a DEPEND or BDEPEND edge on the cycle goes away with the flag
    pub build_time: bool,
}

impl InstallOrderResult {
    /// The cycles the order breaks across a build-time edge, each with its split candidates
    ///
    /// A cycle of RDEPEND edges only is left out: dropping a runtime edge is how such a cycle
    /// is ordered. Candidates rank build-time edges first, then by package key, then by flag.
    pub fn cycle_splits(&self) -> Vec<CycleSplit> {
        let pos: HashMap<(&PortagePackage, &Version), usize> = self
            .order
            .iter()
            .enumerate()
            .map(|(i, (p, v))| ((p, v), i))
            .collect();
        let n = self.order.len();
        // `adj[u]` holds `(v, edge)`: u must merge before v.
        let mut adj: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n];
        let mut radj: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut ends: Vec<Option<(usize, usize)>> = vec![None; self.edges.len()];
        for (e, edge) in self.edges.iter().enumerate() {
            if !matches!(
                edge.class,
                DepClass::Depend | DepClass::Bdepend | DepClass::Rdepend
            ) {
                continue;
            }
            let (Some(&u), Some(&v)) = (
                pos.get(&(&edge.to.0, &edge.to.1)),
                pos.get(&(&edge.from.0, &edge.from.1)),
            ) else {
                continue;
            };
            if u == v {
                continue;
            }
            adj[u].push((v, e));
            radj[v].push(u);
            ends[e] = Some((u, v));
        }
        let is_hard =
            |e: usize| matches!(self.edges[e].class, DepClass::Depend | DepClass::Bdepend);

        let mut splits = Vec::new();
        for (broken, &end) in ends.iter().enumerate() {
            let Some((dep, consumer)) = end.filter(|(u, v)| u > v) else {
                continue;
            };
            let from_consumer = reachable(consumer, n, |u| adj[u].iter().map(|&(v, _)| v));
            let to_dep = reachable(dep, n, |v| radj[v].iter().copied());
            if !from_consumer[dep] {
                continue;
            }
            // Every edge on some path consumer ⇝ dep, plus the broken edge that closes it.
            let on_cycle: Vec<usize> = ends
                .iter()
                .enumerate()
                .filter_map(|(e, end)| {
                    let (u, v) = (*end)?;
                    (from_consumer[u] && to_dep[v]).then_some(e)
                })
                .collect();
            if !on_cycle.iter().any(|&e| is_hard(e)) {
                continue;
            }

            let mut groups: Vec<(usize, Interned<DefaultInterner>)> = on_cycle
                .iter()
                .filter_map(|&e| Some((ends[e]?.1, self.edges[e].via_use_flag?)))
                .collect();
            groups.sort_unstable_by(|a, b| (a.0, a.1.as_str()).cmp(&(b.0, b.1.as_str())));
            groups.dedup();
            let mut candidates: Vec<SplitCandidate> = groups
                .into_iter()
                .filter_map(|(node, flag)| {
                    let dropped = |e: usize| {
                        ends[e].is_some_and(|(_, v)| v == node)
                            && self.edges[e].via_use_flag == Some(flag)
                    };
                    let still_closed = !dropped(broken)
                        && reachable(consumer, n, |u| {
                            adj[u]
                                .iter()
                                .filter(|&&(_, e)| !dropped(e))
                                .map(|&(v, _)| v)
                        })[dep];
                    (!still_closed).then(|| SplitCandidate {
                        package: self.order[node].clone(),
                        flag,
                        build_time: on_cycle.iter().any(|&e| dropped(e) && is_hard(e)),
                    })
                })
                .collect();
            candidates.sort_by_cached_key(|c| {
                let key = format!("{}-{}", c.package.0, c.package.1);
                (!c.build_time, key, c.flag.as_str().to_owned())
            });
            splits.push(CycleSplit {
                broken: self.edges[broken].clone(),
                candidates,
            });
        }
        splits
    }

    /// This order again with each build's flag-gated edges dropped, and a flags-on rebuild inserted per build
    ///
    /// Each rebuild goes right after the last package its dropped edges need. `None` when a build has no
    /// edge carrying one of its flags.
    pub fn split(&self, builds: &[SplitBuild]) -> Option<SplitOrder> {
        let gated = |e: &DepEdge| builds.iter().any(|b| b.gates(e));
        let (dropped, kept): (Vec<DepEdge>, Vec<DepEdge>) =
            self.edges.iter().cloned().partition(|e| gated(e));
        let reordered = order_graph(
            self.order.iter().map(|(p, v)| (p, v)),
            kept,
            GraphStats::default(),
        );
        let position =
            |node: &(PortagePackage, Version)| reordered.order.iter().position(|n| n == node);
        let mut inserts: Vec<(usize, usize)> = Vec::with_capacity(builds.len());
        for (i, c) in builds.iter().enumerate() {
            let last_dep = dropped
                .iter()
                .filter(|e| c.gates(e))
                .filter(|e| {
                    matches!(
                        e.class,
                        DepClass::Depend | DepClass::Bdepend | DepClass::Rdepend
                    )
                })
                .filter_map(|e| position(&e.to))
                .max()?;
            inserts.push((last_dep.max(position(&c.package)?) + 1, i));
        }
        let remaining = reordered.cycle_splits();
        let mut order = reordered.order;
        // From the back, so earlier insert points stay valid; a later candidate goes after an earlier one at the same point.
        inserts.sort_unstable();
        for &(at, i) in inserts.iter().rev() {
            order.insert(at, builds[i].package.clone());
        }
        let mut rebuilds = vec![0; builds.len()];
        for (shift, &(at, i)) in inserts.iter().enumerate() {
            rebuilds[i] = at + shift;
        }
        Some(SplitOrder {
            order,
            rebuilds,
            remaining,
        })
    }
}

/// A package built twice: first with `flags` off, then again with them on
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitBuild {
    /// The package built twice
    pub package: (PortagePackage, Version),
    /// The flags off in the first build: the candidate's own, then any its `REQUIRED_USE` turns off with it
    pub flags: Vec<Interned<DefaultInterner>>,
}

impl SplitBuild {
    /// Whether `edge` is one of the package's own, gated by one of the flags
    pub fn gates(&self, edge: &DepEdge) -> bool {
        edge.from == self.package && edge.via_use_flag.is_some_and(|f| self.flags.contains(&f))
    }
}

/// An install order with packages built twice, flags off and then on; see [`InstallOrderResult::split`]
#[derive(Debug, Clone)]
pub struct SplitOrder {
    /// The order, listing each split package at its flag-off position and again at its rebuild
    pub order: Vec<(PortagePackage, Version)>,
    /// Where each build's flags-on rebuild sits in `order`, in the order of the builds
    pub rebuilds: Vec<usize>,
    /// The cycles the order still breaks with the flags off
    pub remaining: Vec<CycleSplit>,
}

/// Which of `n` nodes `start` reaches through `next`, itself included
fn reachable<I: Iterator<Item = usize>>(
    start: usize,
    n: usize,
    next: impl Fn(usize) -> I,
) -> Vec<bool> {
    let mut seen = vec![false; n];
    seen[start] = true;
    let mut stack = vec![start];
    while let Some(u) = stack.pop() {
        for v in next(u) {
            if !seen[v] {
                seen[v] = true;
                stack.push(v);
            }
        }
    }
    seen
}

impl PortageDependencyProvider {
    /// Build the labeled dependency graph from a solution
    ///
    /// Returns edges labeled with the dependency class (DEPEND, RDEPEND, etc.).
    /// Only edges where both endpoints are in the solution are included.
    pub fn dependency_graph(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<DepEdge> {
        self.dependency_graph_with_stats(solution).0
    }

    fn dependency_graph_with_stats(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> (Vec<DepEdge>, GraphStats) {
        let graph_start = Instant::now();
        let mut stats = GraphStats {
            graph_builds: 1,
            ..GraphStats::default()
        };
        let mut edges = Vec::new();
        let classes = [
            DepClass::Depend,
            DepClass::Rdepend,
            DepClass::Bdepend,
            DepClass::Pdepend,
            DepClass::Idepend,
        ];

        // Index solution by CPN so dependency lookups are O(1) instead of O(n).
        // Skip virtual packages (USE-decision nodes, synthetic root) — they
        // have no CPN and must not appear in the output graph.
        let mut by_cpn: HashMap<&Cpn, Vec<(&PortagePackage, &Version)>> = HashMap::new();
        for (sol_pkg, sol_ver) in solution.iter() {
            if sol_pkg.is_virtual() {
                continue;
            }
            by_cpn
                .entry(sol_pkg.cpn())
                .or_default()
                .push((sol_pkg, sol_ver));
        }

        for (pkg, version) in solution.iter() {
            // A `Host`-flavored package's own dependency data lives under its
            // `Target`-flavored alias (`self.packages` is keyed by whatever
            // identity the construction-time BFS discovered, always `Target`
            // for a real package — see `ensure_host_instances`/`host_aliases`).
            // Host nodes are keyed under their Target alias in `self.packages`;
            // resolve via `package_data` so Host BDEPEND edges are not lost.
            let Some(data) = self.package_data(pkg) else {
                continue;
            };
            let Some(vd) = data.versions.get(version) else {
                continue;
            };

            // Mirror `compute_dependencies`'s own "kept, not being rebuilt"
            // skip: a package the solver never expanded shouldn't manufacture
            // ordering edges from its raw metadata either (found live
            // 2026-08-24 — a Provided zstd's own BDEPEND on meson-format-array
            // still showed up here and created a phantom ordering conflict
            // the solver itself never relied on). `Provided` skips every
            // class; a genuinely kept install skips only the build-time ones.
            let kept = self.installed.get(pkg).filter(|(inst, policy)| {
                inst == version && !matches!(policy, InstalledPolicy::Rebuild)
            });
            if kept.is_some_and(|(_, policy)| matches!(policy, InstalledPolicy::Provided)) {
                continue;
            }
            let skip_build_time = kept.is_some();

            for (class_idx, &class) in classes.iter().enumerate() {
                if skip_build_time && matches!(class, DepClass::Depend | DepClass::Bdepend) {
                    continue;
                }
                for (dep_pkg, dep_vs, gating_flag) in &vd.by_class[class_idx] {
                    // A dep may point at a virtual choice/slot/use-decision node.
                    // Those are stripped from the solution but remain in
                    // `self.packages`, so expand them transitively to the real
                    // packages they select (filtered to the solution by the
                    // inner version-sets). Without this, deps reachable only
                    // through a USE-conditional / `||` group / multi-slot choice
                    // produce no ordering edge — e.g. `vala? ( || ( dev-lang/vala:0.56 ) )`
                    // left librsvg unordered w.r.t. vala.
                    let mut seen: std::collections::HashSet<&PortagePackage> =
                        std::collections::HashSet::new();
                    let mut emitted: std::collections::HashSet<(&PortagePackage, &Version)> =
                        std::collections::HashSet::new();
                    let mut work: Vec<(&PortagePackage, &PortageVersionSet)> =
                        vec![(dep_pkg, dep_vs)];
                    while let Some((dp, dvs)) = work.pop() {
                        if dp.is_virtual() {
                            if !seen.insert(dp) {
                                continue;
                            }
                            stats.virtual_expansions += 1;
                            if let Some(vdata) = self.package_data(dp) {
                                let selected_version =
                                    solution.get(dp).or_else(|| self.selected_virtuals.get(dp));
                                if let Some(version) = selected_version {
                                    if let Some(vver) = vdata.versions.get(version) {
                                        for (idp, idvs, _) in vver.by_class.iter().flatten() {
                                            work.push((idp, idvs));
                                        }
                                    }
                                } else {
                                    for vver in vdata.versions.values() {
                                        for (idp, idvs, _) in vver.by_class.iter().flatten() {
                                            work.push((idp, idvs));
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                        let Some(candidates) = by_cpn.get(dp.cpn()) else {
                            continue;
                        };
                        for &(sol_pkg, sol_ver) in candidates {
                            if dvs.contains(sol_ver) && emitted.insert((sol_pkg, sol_ver)) {
                                edges.push(DepEdge {
                                    from: (pkg.clone(), version.clone()),
                                    to: (sol_pkg.clone(), sol_ver.clone()),
                                    class,
                                    via_use_flag: *gating_flag,
                                });
                            }
                        }
                    }
                }
            }
        }

        stats.graph_edges = edges.len();
        stats.graph_build_time = graph_start.elapsed();
        (edges, stats)
    }

    /// Compute an installation order from a solution
    ///
    /// Returns packages in topological order: a dependency is merged before
    /// the package that needs it. Both build-time (DEPEND/BDEPEND) and
    /// runtime (RDEPEND) edges constrain the order. PDEPEND (merged *after*
    /// the parent) and IDEPEND (install-time only) do not constrain it.
    ///
    /// RDEPEND introduces cycles far more often than build deps alone (e.g.
    /// `gtk+` ↔ its icon-theme/at-spi runtime deps). Portage treats runtime
    /// edges as *soft*: when the graph stalls in a cycle, soft edges are
    /// dropped to break it while hard build-time edges are preserved. We do
    /// the same — only a genuine hard cycle (`xz-utils` ↔ `elt-patches`)
    /// falls back to a deterministic lexicographic tie-break.
    ///
    /// Refinements on the Portage-shaped walk:
    /// - **B:** inside a soft SCC, prefer soft-ready among hard-ready nodes.
    /// - **C:** after the walk, re-linearise promoting soft edges still
    ///   acyclic w.r.t. hard (+ already-promoted soft) constraints (pass-1
    ///   orientation first, then inverted; pass-1 order as tie-break). Fixes
    ///   empty `virtual/*` providers emitted before their real provider.
    pub fn install_order(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<(PortagePackage, Version)> {
        self.install_order_with_stats(solution).order
    }

    /// Compute an installation order and return its graph measurements.
    pub fn install_order_with_stats(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> InstallOrderResult {
        let (graph, stats) = self.dependency_graph_with_stats(solution);
        order_graph(solution.iter(), graph, stats)
    }
}

/// Order `nodes` under `graph`'s DEPEND/BDEPEND/RDEPEND edges (see [`PortageDependencyProvider::install_order`])
fn order_graph<'a>(
    nodes: impl Iterator<Item = (&'a PortagePackage, &'a Version)>,
    graph: Vec<DepEdge>,
    mut stats: GraphStats,
) -> InstallOrderResult {
    // Index nodes deterministically (sorted by cached key) so SCC discovery and all
    // tie-breaks are reproducible.
    let mut node_pv: Vec<OrderedNode> = nodes
        .map(|(pkg, ver)| (format!("{}-{}", pkg, ver), (pkg.clone(), ver.clone())))
        .collect();
    node_pv.sort_by(|a, b| a.0.cmp(&b.0));
    let n = node_pv.len();
    stats.graph_nodes = n;
    let idx: HashMap<(&PortagePackage, &Version), usize> = node_pv
        .iter()
        .enumerate()
        .map(|(i, (_, (pkg, ver)))| ((pkg, ver), i))
        .collect();
    let node_id = |pkg: &PortagePackage, ver: &Version| idx.get(&(pkg, ver)).copied();

    // Adjacency: dependency → dependent ("dependency comes first").
    // `succ_all` = hard (DEPEND/BDEPEND) + soft (RDEPEND); `succ_hard` only
    // the build-time edges, used to order within a cycle.
    let mut succ_all: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut succ_hard: Vec<Vec<usize>> = vec![Vec::new(); n];
    for edge in &graph {
        let hard = match edge.class {
            DepClass::Depend | DepClass::Bdepend => true,
            DepClass::Rdepend => false,
            // PDEPEND (merged after parent) / IDEPEND: no ordering constraint.
            _ => continue,
        };
        let (Some(u), Some(v)) = (
            node_id(&edge.to.0, &edge.to.1),
            node_id(&edge.from.0, &edge.from.1),
        ) else {
            continue;
        };
        succ_all[u].push(v);
        if hard {
            succ_hard[u].push(v);
        }
    }
    for adj in succ_all.iter_mut() {
        adj.sort_unstable();
        adj.dedup();
    }
    for adj in succ_hard.iter_mut() {
        adj.sort_unstable();
        adj.dedup();
    }

    // Strongly-connected components via iterative Tarjan.  Nodes in different
    // SCCs are linearised by the condensation (a DAG), so every cross-SCC
    // edge — every edge that is not part of a genuine cycle — is respected.
    let comp_of = tarjan_scc(&succ_all);
    let num_comps = comp_of.iter().copied().max().map_or(0, |m| m + 1);
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); num_comps];
    for (node, &c) in comp_of.iter().enumerate() {
        members[c].push(node);
    }
    stats.scc_count = num_comps;
    stats.largest_scc = members.iter().map(Vec::len).max().unwrap_or(0);

    // Condensation edges + in-degrees (deduplicated).
    let mut comp_succ: Vec<std::collections::BTreeSet<usize>> =
        vec![std::collections::BTreeSet::new(); num_comps];
    let mut comp_indeg = vec![0usize; num_comps];
    for u in 0..n {
        for &v in &succ_all[u] {
            let (cu, cv) = (comp_of[u], comp_of[v]);
            if cu != cv && comp_succ[cu].insert(cv) {
                comp_indeg[cv] += 1;
            }
        }
    }

    // The component key (max member node index, with nodes already sorted by
    // key) drives a deterministic max-heap tie-break.
    let comp_key = |c: usize| -> usize { members[c].iter().copied().max().unwrap_or(0) };

    let mut comp_ready: BinaryHeap<(usize, usize)> = (0..num_comps)
        .filter(|&c| comp_indeg[c] == 0)
        .map(|c| (comp_key(c), c))
        .collect();

    let mut result = Vec::with_capacity(n);
    while let Some((_, c)) = comp_ready.pop() {
        // Emit this component's members.  A singleton is just itself; a real
        // cycle is ordered internally by breaking soft (RDEPEND) edges before
        // hard ones (see `order_cycle`).
        if members[c].len() == 1 {
            let node = members[c][0];
            result.push(node_pv[node].1.clone());
        } else {
            for node in order_cycle(&members[c], &succ_hard, &succ_all, &node_pv) {
                result.push(node_pv[node].1.clone());
            }
        }
        for &cv in &comp_succ[c] {
            comp_indeg[cv] -= 1;
            if comp_indeg[cv] == 0 {
                comp_ready.push((comp_key(cv), cv));
            }
        }
    }

    // Pass 2: restore inverted soft edges that remain acyclic (bug #3).
    let order = repair_soft_inversions(result, &graph, &mut stats);
    InstallOrderResult {
        order,
        edges: graph,
        stats,
    }
}

/// Iterative Tarjan SCC
///
/// Returns the component id of each node; ids are dense
/// `0..num_components`. `succ[u]` lists nodes that must come *after* `u`.
fn tarjan_scc(succ: &[Vec<usize>]) -> Vec<usize> {
    let n = succ.len();
    let mut index = vec![usize::MAX; n];
    let mut lowlink = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut comp_of = vec![usize::MAX; n];
    let mut next_index = 0usize;
    let mut next_comp = 0usize;

    for s in 0..n {
        if index[s] != usize::MAX {
            continue;
        }
        // DFS frame: (node, next child position).
        let mut call: Vec<(usize, usize)> = vec![(s, 0)];
        while let Some(&mut (v, ref mut ci)) = call.last_mut() {
            if *ci == 0 {
                index[v] = next_index;
                lowlink[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if *ci < succ[v].len() {
                let w = succ[v][*ci];
                *ci += 1;
                if index[w] == usize::MAX {
                    call.push((w, 0));
                } else if on_stack[w] {
                    lowlink[v] = lowlink[v].min(index[w]);
                }
            } else {
                if lowlink[v] == index[v] {
                    // v is a root node; pop the SCC off the stack.
                    // SAFETY: v was pushed onto the stack on line 283 when we first visited it,
                    // and we haven't popped it yet, so stack is non-empty and contains v.
                    loop {
                        let x = stack.pop().expect("Tarjan's SCC: stack should contain v");
                        on_stack[x] = false;
                        comp_of[x] = next_comp;
                        if x == v {
                            break;
                        }
                    }
                    next_comp += 1;
                }
                call.pop();
                if let Some(&(parent, _)) = call.last() {
                    lowlink[parent] = lowlink[parent].min(lowlink[v]);
                }
            }
        }
    }
    comp_of
}

/// Order the members of a single `succ_all` (hard+soft) component
///
/// A soft (RDEPEND) cycle folds many packages into one component even when
/// most have acyclic hard chains (e.g. tools → elt-patches ↔ xz-utils). Hard
/// dependents must still follow their hard prerequisites; only true hard-
/// cycle members need a heuristic.
///
/// 1. Isolate irreducible hard cycles (Tarjan over `succ_hard` on `members`).
/// 2. Never emit a member with an unmet hard predecessor outside its
///    hard-group while an eligible member remains — the hard-group DAG
///    guarantees progress.
///
/// 3. Inside a hard-group: emit closest-to-ready (fewest pending hard, then
///    prefer no pending soft (**B**), then fewest soft+hard, then key).
///    Singleton groups topo-sort when soft edges are acyclic; pass-2
///    (`repair_soft_inversions`) may restore inverted soft edges that
///    remain acyclic with hard constraints.
fn order_cycle(
    members: &[usize],
    succ_hard: &[Vec<usize>],
    succ_all: &[Vec<usize>],
    node_pv: &[OrderedNode],
) -> Vec<usize> {
    use std::collections::HashSet;
    let set: HashSet<usize> = members.iter().copied().collect();
    let mut indeg_hard: HashMap<usize, usize> = members.iter().map(|&m| (m, 0)).collect();
    let mut indeg_all: HashMap<usize, usize> = members.iter().map(|&m| (m, 0)).collect();
    for &u in members {
        for &v in &succ_all[u] {
            if set.contains(&v) {
                // SAFETY: v is in set which is the keys of indeg_all (initialized above),
                // so get_mut must succeed.
                *indeg_all
                    .get_mut(&v)
                    .expect("v in set implies v in indeg_all") += 1;
            }
        }
        for &v in &succ_hard[u] {
            if set.contains(&v) {
                // SAFETY: v is in set which is the keys of indeg_hard (initialized above),
                // so get_mut must succeed.
                *indeg_hard
                    .get_mut(&v)
                    .expect("v in set implies v in indeg_hard") += 1;
            }
        }
    }

    // Hard-only sub-SCCs within this component: the genuinely irreducible
    // hard cycles. `local` remaps member node-ids to a dense 0..members.len()
    // range for `tarjan_scc`.
    let local: HashMap<usize, usize> = members.iter().enumerate().map(|(i, &m)| (m, i)).collect();
    let mut sub_hard: Vec<Vec<usize>> = vec![Vec::new(); members.len()];
    for &u in members {
        for &v in &succ_hard[u] {
            if let Some(&lv) = local.get(&v) {
                sub_hard[local[&u]].push(lv);
            }
        }
    }
    let group_of_local = tarjan_scc(&sub_hard);
    let group_of = |m: usize| -> usize { group_of_local[local[&m]] };

    // A member with an unmet hard predecessor outside its own hard-group is
    // never eligible while any node without one remains — that predecessor
    // is not part of any real cycle, so waiting for it is always possible.
    let mut cross_pending: HashMap<usize, usize> = members.iter().map(|&m| (m, 0)).collect();
    for &u in members {
        for &v in &succ_hard[u] {
            if set.contains(&v) && group_of(u) != group_of(v) {
                // SAFETY: v is in set which is the keys of cross_pending (initialized above),
                // so get_mut must succeed.
                *cross_pending
                    .get_mut(&v)
                    .expect("v in set implies v in cross_pending") += 1;
            }
        }
    }

    let mut remaining: HashSet<usize> = set.clone();
    let mut out = Vec::with_capacity(members.len());
    while !remaining.is_empty() {
        let pick = *remaining
            .iter()
            .min_by(|&&a, &&b| {
                let pa = cross_pending[&a] > 0;
                let pb = cross_pending[&b] > 0;
                let ha = indeg_hard[&a];
                let hb = indeg_hard[&b];
                let aa = indeg_all[&a];
                let ab = indeg_all[&b];
                // Soft-pending when all-indegree exceeds hard-indegree (extra
                // soft-only edges still unsatisfied). Prefer soft-ready among
                // equal hard readiness (**B**).
                let sa = aa > ha;
                let sb = ab > hb;
                // A pending cross-group hard predecessor always loses: that
                // hard edge is never violated, unlike edges inside a genuine
                // hard cycle. Largest key wins remaining ties.
                pa.cmp(&pb)
                    .then(ha.cmp(&hb))
                    .then(sa.cmp(&sb))
                    .then(aa.cmp(&ab))
                    .then_with(|| node_pv[b].0.cmp(&node_pv[a].0))
            })
            .unwrap();
        remaining.remove(&pick);
        out.push(pick);
        for &v in &succ_all[pick] {
            if let Some(e) = indeg_all.get_mut(&v) {
                *e = e.saturating_sub(1);
            }
        }
        for &v in &succ_hard[pick] {
            if let Some(e) = indeg_hard.get_mut(&v) {
                *e = e.saturating_sub(1);
            }
            if set.contains(&v)
                && group_of(pick) != group_of(v)
                && let Some(e) = cross_pending.get_mut(&v)
            {
                *e = e.saturating_sub(1);
            }
        }
    }
    out
}

/// Pass-2 repair after the soft-cycle walk (bug #3)
///
/// Pass-1 may emit a consumer before its RDEPEND provider when the provider is
/// still hard-blocked inside a soft SCC (empty `virtual/libcrypt` before
/// `sys-libs/libxcrypt`).  Rebuild a total order from constraints, carefully:
///
/// 1. **Lock pass-1-forward hard + soft edges** (`pos(dep) < pos(consumer)`).
///    These form a subgraph of the pass-1 total order → always acyclic.
///    Never drop a hard edge pass-1 already got right (live regression:
///    pass-2 used to skip some on a hard-SCC false path, then soft
///    promotions reordered `gcc` before its BDEPEND `glibc`).
/// 2. **Try inverted hard edges** (pass-1 violated a DEPEND/BDEPEND) if acyclic.
///
/// 3. **Try inverted soft edges** (earliest pass-1 consumer first) if
///    acyclic — the empty-virtual-before-provider fix when no hard path
///    `virtual →* provider` blocks the promote.
/// 4. Kahn topo with pass-1 index as ready-queue tie-break.
///
/// Limitation: when a **hard** path `virtual → … → provider` exists on the same
/// MergeRoot (e.g. Target python DEPEND virtual, glibc BDEPEND python,
/// libxcrypt DEPEND glibc), step 3 cannot promote and the virtual stays early.
/// That is a real hard/soft conflict, not a repair bug — needs dual-root
/// routing or library-identity Favor so the hard path does not exist on Target.
fn repair_soft_inversions(
    order: Vec<(PortagePackage, Version)>,
    graph: &[DepEdge],
    stats: &mut GraphStats,
) -> Vec<(PortagePackage, Version)> {
    let n = order.len();
    if n <= 1 {
        return order;
    }

    let mut pos: HashMap<(&PortagePackage, &Version), usize> = HashMap::with_capacity(n);
    for (i, (p, v)) in order.iter().enumerate() {
        pos.insert((p, v), i);
    }
    let mut reach_marks = vec![0u32; n];
    let mut reach_epoch = 0u32;

    // `before_succ[u]` = nodes that must come *after* u (u before them).
    let mut before_succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];

    let force_add = |u: usize, v: usize, succ: &mut [Vec<usize>], indeg: &mut [usize]| {
        if u == v || succ[u].contains(&v) {
            return;
        }
        // Forward-in-pass-1 edges only — caller guarantees acyclicity.
        succ[u].push(v);
        indeg[v] += 1;
    };

    let mut try_add = |u: usize, v: usize, succ: &mut [Vec<usize>], indeg: &mut [usize]| -> bool {
        if u == v {
            return false;
        }
        if succ[u].contains(&v) {
            return true;
        }
        // Adding u→v (u before v) cycles if v can already reach u.
        if reaches(v, u, succ, &mut reach_marks, &mut reach_epoch, stats) {
            return false;
        }
        succ[u].push(v);
        indeg[v] += 1;
        true
    };

    // (dep_idx, consumer_idx) = dep must come before consumer.
    // `bdepend_targets` = nodes something else BDEPENDs on — a build tool a
    // sibling package actually *invokes*, not just a plain runtime dep. Its
    // own RDEPEND inversions get promotion priority below (Q1): a build
    // tool run before its own interpreter/runtime exists fails outright,
    // unlike an ordinary soft RDEPEND cycle (e.g. gtk+ ↔ icon-theme).
    let mut hard: Vec<(usize, usize)> = Vec::new();
    let mut soft: Vec<(usize, usize)> = Vec::new();
    let mut bdepend_targets: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for edge in graph {
        let Some(&from_i) = pos.get(&(&edge.from.0, &edge.from.1)) else {
            continue;
        };
        let Some(&to_i) = pos.get(&(&edge.to.0, &edge.to.1)) else {
            continue;
        };
        match edge.class {
            DepClass::Depend | DepClass::Bdepend => hard.push((to_i, from_i)),
            DepClass::Rdepend => soft.push((to_i, from_i)),
            _ => {}
        }
        if edge.class == DepClass::Bdepend {
            bdepend_targets.insert(to_i);
        }
    }
    drop(pos);
    hard.sort_unstable();
    hard.dedup();
    soft.sort_unstable();
    soft.dedup();

    let (hard_fwd, mut hard_inv): (Vec<_>, Vec<_>) = hard
        .into_iter()
        .filter(|(d, c)| d != c)
        .partition(|(d, c)| d < c);
    let (soft_fwd, mut soft_inv): (Vec<_>, Vec<_>) = soft
        .into_iter()
        .filter(|(d, c)| d != c)
        .partition(|(d, c)| d < c);

    // 1. Lock every edge pass-1 already oriented correctly (subgraph of total order).
    for (dep, consumer) in hard_fwd.into_iter().chain(soft_fwd) {
        force_add(dep, consumer, &mut before_succ, &mut indeg);
    }

    // 2. Fix hard inversions when possible.
    hard_inv.sort_by_key(|(dep, consumer)| (*consumer, *dep));
    for (dep, consumer) in hard_inv {
        let _ = try_add(dep, consumer, &mut before_succ, &mut indeg);
    }

    // 3. Fix soft inversions (empty virtual before provider, a BDEPEND
    // tool's own RDEPEND, etc.). A tool that's a `bdepend_targets` consumer
    // goes first (Q1) so an unrelated, equally-promotable inversion
    // processed earlier can't lock a path that starves it — the greedy
    // walk otherwise depends on arbitrary index order once several
    // inversions share one soft cycle.
    soft_inv.sort_by_key(|(dep, consumer)| (!bdepend_targets.contains(consumer), *consumer, *dep));
    for (dep, consumer) in soft_inv {
        let _ = try_add(dep, consumer, &mut before_succ, &mut indeg);
    }

    // Kahn with min-heap on pass-1 index so unconstrained pairs keep pass-1 order.
    let mut ready: BinaryHeap<std::cmp::Reverse<usize>> = (0..n)
        .filter(|&i| indeg[i] == 0)
        .map(std::cmp::Reverse)
        .collect();
    let mut out = Vec::with_capacity(n);
    let mut seen = 0usize;
    while let Some(std::cmp::Reverse(i)) = ready.pop() {
        out.push(order[i].clone());
        seen += 1;
        for &v in &before_succ[i] {
            indeg[v] -= 1;
            if indeg[v] == 0 {
                ready.push(std::cmp::Reverse(v));
            }
        }
    }
    // Constraint graph should always be a DAG (forward edges + refused cycles).
    // If something still stalls, fall back to pass-1 rather than drop packages.
    if seen != n {
        return order;
    }
    out
}

/// Whether `start` can reach `target` following `succ` edges
fn reaches(
    start: usize,
    target: usize,
    succ: &[Vec<usize>],
    marks: &mut [u32],
    epoch: &mut u32,
    stats: &mut GraphStats,
) -> bool {
    stats.repair_reachability_calls += 1;
    if start == target {
        return true;
    }
    *epoch = epoch.wrapping_add(1);
    if *epoch == 0 {
        marks.fill(0);
        *epoch = 1;
    }
    let generation = *epoch;
    let mut stack = vec![start];
    marks[start] = generation;
    while let Some(u) = stack.pop() {
        stats.repair_reachability_nodes += 1;
        for &v in &succ[u] {
            if v == target {
                return true;
            }
            if marks[v] != generation {
                marks[v] = generation;
                stack.push(v);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{InMemoryRepository, PackageDeps};
    use crate::version_set::PortageVersionSet;
    use portage_atom::interner::{DefaultInterner, Interned};
    use portage_atom::{Cpn, Cpv, Dep, DepEntry};

    #[test]
    fn install_order_and_dependency_graph_work() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("app-misc/top-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("dev-libs/bottom").unwrap())]).into(),
                rdepend: (vec![]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/bottom-1.0").unwrap(),
            None,
            None,
            empty(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let top = PortagePackage::unslotted(Cpn::parse("app-misc/top").unwrap());

        let solution = provider
            .resolve_targets(vec![(top, PortageVersionSet::any())])
            .unwrap();

        let edges = provider.dependency_graph(&solution);
        assert!(
            edges.iter().any(|e| e.class == DepClass::Depend),
            "should have a DEPEND edge"
        );

        let order = provider.install_order(&solution);
        let names: Vec<&str> = order
            .iter()
            .map(|(p, _)| p.cpn().package.as_str())
            .collect();
        let bottom_pos = names.iter().position(|&n| n == "bottom").unwrap();
        let top_pos = names.iter().position(|&n| n == "top").unwrap();
        assert!(
            bottom_pos < top_pos,
            "bottom must come before top in install order, got: {:?}",
            names
        );

        let measured = provider.install_order_with_stats(&solution);
        assert_eq!(measured.order, order);
        assert_eq!(measured.edges.len(), edges.len());
        assert_eq!(measured.stats.graph_builds, 1);
        assert_eq!(measured.stats.graph_nodes, solution.iter().count());
        assert_eq!(measured.stats.graph_edges, edges.len());
    }

    #[test]
    fn duplicate_adjacency_keeps_labels_without_changing_order() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };
        repo.add_version(
            Cpv::parse("dev-libs/bottom-1.0").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            Cpv::parse("app-misc/top-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (DepEntry::parse("dev-libs/bottom").unwrap()).into(),
                rdepend: (DepEntry::parse("dev-libs/bottom").unwrap()).into(),
                ..empty()
            },
        );
        let mut provider = PortageDependencyProvider::new(repo);
        let top = PortagePackage::unslotted(Cpn::parse("app-misc/top").unwrap());
        let solution = provider
            .resolve_targets(vec![(top, PortageVersionSet::any())])
            .unwrap();

        let edges = provider.dependency_graph(&solution);
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().any(|edge| edge.class == DepClass::Depend));
        assert!(edges.iter().any(|edge| edge.class == DepClass::Rdepend));
        let order = provider.install_order(&solution);
        let names: Vec<&str> = order
            .iter()
            .map(|(package, _)| package.cpn().package.as_str())
            .collect();
        assert_eq!(names, ["bottom", "top"]);
    }

    #[test]
    fn selected_or_branch_does_not_expand_an_unselected_real_target() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };
        let slot = Interned::intern("0");
        repo.add_version(
            Cpv::parse("dev-libs/a-1.0").unwrap(),
            Some(slot),
            None,
            empty(),
        );
        repo.add_version(
            Cpv::parse("dev-libs/b-1.0").unwrap(),
            Some(slot),
            None,
            empty(),
        );
        repo.add_version(
            Cpv::parse("app-misc/consumer-1.0").unwrap(),
            Some(slot),
            None,
            PackageDeps {
                depend: (DepEntry::parse("|| ( dev-libs/a dev-libs/b )").unwrap()).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("app-misc/other-1.0").unwrap(),
            Some(slot),
            None,
            PackageDeps {
                depend: (DepEntry::parse("dev-libs/b").unwrap()).into(),
                ..empty()
            },
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let consumer = PortagePackage::slotted(Cpn::parse("app-misc/consumer").unwrap(), slot);
        let other = PortagePackage::slotted(Cpn::parse("app-misc/other").unwrap(), slot);
        let solution = provider
            .resolve_targets(vec![
                (consumer, PortageVersionSet::any()),
                (other, PortageVersionSet::any()),
            ])
            .unwrap();
        assert!(solution.iter().all(|(package, _)| !package.is_virtual()));

        let edges = provider.dependency_graph(&solution);
        let has_edge = |from: &str, to: &str| {
            edges.iter().any(|edge| {
                edge.from.0.cpn().package.as_str() == from && edge.to.0.cpn().package.as_str() == to
            })
        };
        assert!(has_edge("consumer", "a"));
        assert!(!has_edge("consumer", "b"));
        assert!(has_edge("other", "b"));
    }

    #[test]
    fn selected_use_decision_does_not_expand_an_unselected_branch() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };
        let slot = Interned::intern("0");
        repo.add_version(
            Cpv::parse("dev-libs/on-1.0").unwrap(),
            Some(slot),
            None,
            empty(),
        );
        repo.add_version(
            Cpv::parse("dev-libs/off-1.0").unwrap(),
            Some(slot),
            None,
            empty(),
        );
        repo.add_version_with_iuse(
            Cpv::parse("app-misc/conditional-1.0").unwrap(),
            Some(slot),
            None,
            vec![Interned::intern("flag")],
            PackageDeps {
                depend: (DepEntry::parse("flag? ( || ( dev-libs/on dev-libs/off ) )").unwrap())
                    .into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("app-misc/other-1.0").unwrap(),
            Some(slot),
            None,
            PackageDeps {
                depend: (DepEntry::parse("dev-libs/off").unwrap()).into(),
                ..empty()
            },
        );
        let mut config = crate::use_config::UseConfig::new();
        config.solver_decide(Interned::intern("flag"), true);
        repo.set_use_config(config);

        let mut provider = PortageDependencyProvider::new(repo);
        let conditional =
            PortagePackage::slotted(Cpn::parse("app-misc/conditional").unwrap(), slot);
        let other = PortagePackage::slotted(Cpn::parse("app-misc/other").unwrap(), slot);
        let solution = provider
            .resolve_targets(vec![
                (conditional, PortageVersionSet::any()),
                (other, PortageVersionSet::any()),
            ])
            .unwrap();
        assert!(solution.iter().all(|(package, _)| !package.is_virtual()));

        let edges = provider.dependency_graph(&solution);
        let has_edge = |from: &str, to: &str| {
            edges.iter().any(|edge| {
                edge.from.0.cpn().package.as_str() == from && edge.to.0.cpn().package.as_str() == to
            })
        };
        assert!(has_edge("conditional", "on"));
        assert!(!has_edge("conditional", "off"));
        assert!(has_edge("other", "off"));
    }

    // Regression test for the riscv64 stage3 shakeout: `dependency_graph`
    // did a raw `self.packages.get(pkg)` lookup instead of the alias-resolving
    // `self.package_data(pkg)` — so a `Host`-flavored solved package (whose
    // data lives under its `Target`-flavored alias, see `ensure_host_instances`)
    // always missed, silently producing zero outgoing edges for it. A `Host`
    // package's own BDEPEND on *another* `Host` package (e.g. one Host-routed
    // perl module needing Host-routed perl itself) then got no ordering edge
    // at all, so `install_order` could place the dependency *after* its own
    // consumer instead of before it.
    #[test]
    fn host_package_bdepend_on_another_host_package_orders_correctly() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };
        // Names deliberately chosen so a broken (edge-less) tie-break gets the
        // order wrong: `dev-build/dep`'s sort key is *smaller* than
        // `dev-build/user`'s, so without a real ordering edge the "largest
        // ready first" tie-break would emit `user` before `dep` — the wrong
        // order. Only a genuine dependency edge (dep must precede user)
        // forces the correct order regardless of naming.
        repo.add_version(
            portage_atom::Cpv::parse("dev-build/dep-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-build/user-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                bdepend: (vec![DepEntry::Atom(Dep::parse("dev-build/dep").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                bdepend: (vec![DepEntry::Atom(Dep::parse("dev-build/user").unwrap())]).into(),
                ..empty()
            },
        );

        let mut provider = PortageDependencyProvider::new(repo);
        provider.set_cross_active(true);
        provider.set_with_bdeps(true);
        // No `add_host_installed` calls: the host genuinely lacks `user` and
        // `dep`, so both are scheduled as unsatisfied Host BDEPEND — `user`
        // (a's BDEPEND) and, transitively, `dep` (user's own BDEPEND once
        // `user` is Host-routed).

        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        let solution = provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap();

        let order = provider.install_order(&solution);
        let names: Vec<&str> = order
            .iter()
            .map(|(p, _)| p.cpn().package.as_str())
            .collect();
        let user_pos = names.iter().position(|&n| n == "user");
        let dep_pos = names.iter().position(|&n| n == "dep");
        assert!(
            user_pos.is_some() && dep_pos.is_some(),
            "both user and dep must be scheduled (Host BDEPEND), got: {names:?}"
        );
        assert!(
            dep_pos.unwrap() < user_pos.unwrap(),
            "dep (user's own BDEPEND) must come before user in install order, got: {names:?}"
        );
    }

    #[test]
    fn dependency_graph_returns_labeled_edges() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("app-misc/app-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("dev-libs/lib").unwrap())]).into(),
                rdepend: (vec![DepEntry::Atom(Dep::parse("dev-libs/runtime").unwrap())]).into(),
                bdepend: (vec![]).into(),
                pdepend: (vec![]).into(),
                idepend: (vec![]).into(),
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/lib-1.0").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/runtime-1.0").unwrap(),
            None,
            None,
            empty(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let app = PortagePackage::unslotted(Cpn::parse("app-misc/app").unwrap());

        let solution = provider
            .resolve_targets(vec![(app, PortageVersionSet::any())])
            .unwrap();
        let edges = provider.dependency_graph(&solution);

        let dep_classes: Vec<_> = edges.iter().map(|e| e.class).collect();
        assert!(
            dep_classes.contains(&DepClass::Depend),
            "should have DEPEND edge"
        );
        assert!(
            dep_classes.contains(&DepClass::Rdepend),
            "should have RDEPEND edge"
        );
    }

    // Regression test for the 2026-07-16 `order_cycle` bug: an ordinary,
    // acyclic hard (BDEPEND) dependent of a genuine hard-cycle member must
    // still be ordered *after* it, even when an unrelated soft (RDEPEND)
    // cycle elsewhere folds both into the same `succ_all` component.
    //
    // Shape: `dev-util/elt` <-> `dev-util/xz` is a genuine hard (BDEPEND)
    // cycle. `sys-apps/sweep` has an ordinary hard BDEPEND on `elt` — no
    // cyclic relationship with it. `dev-util/fn` RDEPENDs on `sweep`, and
    // `elt` RDEPENDs on `fn` — a soft back-path that pulls `sweep` into the
    // same `succ_all` component as the `elt`/`xz` hard cycle. `sweep`'s name
    // is picked with a *larger* sort key than `elt`'s, so the old, ungated
    // indeg tie-break picked it first: `fn` reaches `indeg_hard == 0` first
    // and is emitted, then `elt`/`sweep` tie at `indeg_hard == 1` /
    // `indeg_all == 1`, and the largest-key tie-break picked `sweep` —
    // before its own real hard dependency `elt`.
    #[test]
    fn ordinary_hard_dependent_of_a_cycle_member_still_orders_after_it() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("dev-util/elt-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                bdepend: (vec![DepEntry::Atom(Dep::parse("dev-util/xz").unwrap())]).into(),
                rdepend: (vec![DepEntry::Atom(Dep::parse("dev-util/fn").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-util/xz-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                bdepend: (vec![DepEntry::Atom(Dep::parse("dev-util/elt").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-util/fn-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                rdepend: (vec![DepEntry::Atom(Dep::parse("sys-apps/sweep").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("sys-apps/sweep-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                bdepend: (vec![DepEntry::Atom(Dep::parse("dev-util/elt").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/top-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![
                    DepEntry::Atom(Dep::parse("dev-util/fn").unwrap()),
                    DepEntry::Atom(Dep::parse("sys-apps/sweep").unwrap()),
                ])
                .into(),
                ..empty()
            },
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let top = PortagePackage::unslotted(Cpn::parse("app-misc/top").unwrap());
        let solution = provider
            .resolve_targets(vec![(top, PortageVersionSet::any())])
            .unwrap_or_else(|e| panic!("resolution failed: {e:?}"));

        let order = provider.install_order(&solution);
        let names: Vec<&str> = order
            .iter()
            .map(|(p, _)| p.cpn().package.as_str())
            .collect();
        let elt_pos = names.iter().position(|&n| n == "elt").unwrap();
        let sweep_pos = names.iter().position(|&n| n == "sweep").unwrap();
        assert!(
            elt_pos < sweep_pos,
            "elt (sweep's real hard BDEPEND) must order before sweep, got: {names:?}"
        );
    }

    // Soft+hard cycle: `virtual/libcrypt` → libxcrypt → glibc → … → virtual
    // again. Hard graph is acyclic; soft RDEPEND closes the loop, so pass-1
    // emits the empty virtual first (hard-indegree 0). Pass-2 soft repair
    // must put the provider before the virtual.
    #[test]
    fn empty_virtual_rdepend_orders_after_provider_through_soft_hard_cycle() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };
        let depend = |atoms: &[&str]| PackageDeps {
            depend: atoms
                .iter()
                .map(|a| DepEntry::Atom(Dep::parse(a).unwrap()))
                .collect::<Vec<_>>()
                .into(),
            ..empty()
        };
        let rdepend = |atoms: &[&str]| PackageDeps {
            rdepend: atoms
                .iter()
                .map(|a| DepEntry::Atom(Dep::parse(a).unwrap()))
                .collect::<Vec<_>>()
                .into(),
            ..empty()
        };
        let bdepend = |atoms: &[&str]| PackageDeps {
            bdepend: atoms
                .iter()
                .map(|a| DepEntry::Atom(Dep::parse(a).unwrap()))
                .collect::<Vec<_>>()
                .into(),
            ..empty()
        };

        // Cycle members (names mirror Gentoo for readability in assert messages).
        //
        // Critical live detail: glibc's *hard* BDEPEND on tools (python, bison,
        // …) keeps glibc (and thus libxcrypt) late inside the SCC, while empty
        // `virtual/libcrypt` has hard-indegree 0 — its only edge to the provider
        // is soft RDEPEND. Without the BDEPEND "weight", order_cycle can still
        // clear the soft edge as a secondary key once glibc is hard-ready; with
        // it, the empty virtual is eligible long before the provider.
        repo.add_version(
            Cpv::parse("virtual/libcrypt-2").unwrap(),
            None,
            None,
            rdepend(&["sys-libs/libxcrypt"]),
        );
        repo.add_version(
            Cpv::parse("sys-libs/libxcrypt-4.5.2").unwrap(),
            None,
            None,
            // Hard DEPEND + soft RDEPEND on glibc (USE=system), like real ebuild.
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("sys-libs/glibc").unwrap())]).into(),
                rdepend: (vec![DepEntry::Atom(Dep::parse("sys-libs/glibc").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("sys-libs/glibc-2.43").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("virtual/os-headers").unwrap())]).into(),
                // Stand-in for bison/pax-utils style BDEPEND that keeps glibc
                // late *without* a hard edge back onto virtual/libcrypt (real
                // cross builds often put python on Host; a Target python DEPEND
                // on virtual/libcrypt would hard-path virtual → … → libxcrypt
                // and make the soft promote impossible).
                bdepend: (vec![DepEntry::Atom(Dep::parse("sys-devel/bison").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("sys-devel/bison-3.8").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            Cpv::parse("virtual/os-headers-0").unwrap(),
            None,
            None,
            rdepend(&["sys-kernel/linux-headers"]),
        );
        repo.add_version(
            Cpv::parse("sys-kernel/linux-headers-7.1").unwrap(),
            None,
            None,
            bdepend(&["dev-lang/perl"]),
        );
        repo.add_version(
            Cpv::parse("dev-lang/perl-5.44").unwrap(),
            None,
            None,
            depend(&["virtual/libcrypt"]),
        );
        // Consumer like pam: hard DEPEND on the empty virtual only.
        repo.add_version(
            Cpv::parse("sys-libs/pam-1.7.2").unwrap(),
            None,
            None,
            depend(&["virtual/libcrypt"]),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let pam = PortagePackage::unslotted(Cpn::parse("sys-libs/pam").unwrap());
        let solution = provider
            .resolve_targets(vec![(pam, PortageVersionSet::any())])
            .unwrap_or_else(|e| panic!("resolution failed: {e:?}"));

        let order = provider.install_order(&solution);
        let names: Vec<String> = order
            .iter()
            .map(|(p, v)| format!("{}-{v}", p.cpn()))
            .collect();
        let pos = |needle: &str| {
            names
                .iter()
                .position(|n| n.starts_with(needle))
                .unwrap_or_else(|| panic!("{needle} missing from order: {names:?}"))
        };
        let i_xcrypt = pos("sys-libs/libxcrypt");
        let i_virt = pos("virtual/libcrypt");
        let i_pam = pos("sys-libs/pam");
        let i_headers = pos("sys-kernel/linux-headers");
        let i_os = pos("virtual/os-headers");
        let i_perl = pos("dev-lang/perl");
        let i_glibc = pos("sys-libs/glibc");

        assert!(
            i_xcrypt < i_virt,
            "empty virtual/libcrypt must install after its RDEPEND provider \
             libxcrypt; order={names:?}"
        );
        assert!(
            i_virt < i_pam,
            "virtual/libcrypt before pam; order={names:?}"
        );
        assert!(
            i_virt < i_perl,
            "virtual/libcrypt before perl (hard DEPEND); order={names:?}"
        );
        assert!(
            i_glibc < i_xcrypt,
            "glibc before libxcrypt (hard DEPEND); order={names:?}"
        );
        // Soft os-headers → linux-headers may lose to the libcrypt soft promote
        // when both cannot be kept in one SCC; libcrypt is the #3 priority.
        // If both fit, headers should precede the empty os-headers virtual.
        if i_headers < i_os {
            // best case: both soft edges restored
        } else {
            // at least the hard chain still has glibc after *some* os-headers
            assert!(
                i_os < i_glibc,
                "virtual/os-headers before glibc; order={names:?}"
            );
        }
    }

    // Soft repair must not invert a hard BDEPEND that pass-1 already ordered
    // correctly (gcc after glibc).
    #[test]
    fn repair_preserves_pass1_correct_hard_bdepend_with_soft_noise() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        // Soft cycle that folds everything into one SCC:
        // virt -R→ lib, lib -D→ glibc -D→ os, os -R→ hdr -B→ perl -D→ virt
        // plus gcc -B→ glibc (must stay after glibc).
        repo.add_version(
            Cpv::parse("virtual/libcrypt-2").unwrap(),
            None,
            None,
            PackageDeps {
                rdepend: (vec![DepEntry::Atom(Dep::parse("sys-libs/libxcrypt").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("sys-libs/libxcrypt-4.5.2").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("sys-libs/glibc").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("sys-libs/glibc-2.43").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("virtual/os-headers").unwrap())]).into(),
                bdepend: (vec![DepEntry::Atom(Dep::parse("sys-devel/bison").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("sys-devel/bison-3.8").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            Cpv::parse("virtual/os-headers-0").unwrap(),
            None,
            None,
            PackageDeps {
                rdepend: (vec![DepEntry::Atom(
                    Dep::parse("sys-kernel/linux-headers").unwrap(),
                )])
                .into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("sys-kernel/linux-headers-7.1").unwrap(),
            None,
            None,
            PackageDeps {
                bdepend: (vec![DepEntry::Atom(Dep::parse("dev-lang/perl").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("dev-lang/perl-5.44").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("virtual/libcrypt").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("sys-devel/gcc-16").unwrap(),
            None,
            None,
            PackageDeps {
                // Real ebuild shape: BDEPEND on glibc (pre-flight checked this).
                bdepend: (vec![DepEntry::Atom(Dep::parse("sys-libs/glibc").unwrap())]).into(),
                depend: (vec![DepEntry::Atom(Dep::parse("sys-libs/glibc").unwrap())]).into(),
                ..empty()
            },
        );
        // Root pulls gcc so the hard BDEPEND is in the solution.
        repo.add_version(
            Cpv::parse("app-misc/need-gcc-1").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("sys-devel/gcc").unwrap())]).into(),
                ..empty()
            },
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let root = PortagePackage::unslotted(Cpn::parse("app-misc/need-gcc").unwrap());
        let solution = provider
            .resolve_targets(vec![(root, PortageVersionSet::any())])
            .unwrap_or_else(|e| panic!("resolution failed: {e:?}"));

        let order = provider.install_order(&solution);
        let names: Vec<String> = order
            .iter()
            .map(|(p, v)| format!("{}-{v}", p.cpn()))
            .collect();
        let pos = |needle: &str| {
            names
                .iter()
                .position(|n| n.starts_with(needle))
                .unwrap_or_else(|| panic!("{needle} missing from order: {names:?}"))
        };
        let i_glibc = pos("sys-libs/glibc");
        let i_gcc = pos("sys-devel/gcc");
        assert!(
            i_glibc < i_gcc,
            "pass-2 must not put gcc before its hard BDEPEND glibc; order={names:?}"
        );
    }

    // Full-graph #3 limitation: hard path virtual → python → glibc → libxcrypt
    // makes soft provider-before-virtual promote impossible. Document that
    // repair does not invent a hard-illegal order (and may leave virtual early).
    #[test]
    fn hard_path_through_python_blocks_soft_libcrypt_promote() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };
        repo.add_version(
            Cpv::parse("virtual/libcrypt-2").unwrap(),
            None,
            None,
            PackageDeps {
                rdepend: (vec![DepEntry::Atom(Dep::parse("sys-libs/libxcrypt").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("sys-libs/libxcrypt-4.5.2").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("sys-libs/glibc").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("sys-libs/glibc-2.43").unwrap(),
            None,
            None,
            PackageDeps {
                bdepend: (vec![DepEntry::Atom(Dep::parse("dev-lang/python").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("dev-lang/python-3.14").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("virtual/libcrypt").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            Cpv::parse("sys-libs/pam-1.7.2").unwrap(),
            None,
            None,
            PackageDeps {
                depend: (vec![DepEntry::Atom(Dep::parse("virtual/libcrypt").unwrap())]).into(),
                ..empty()
            },
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let pam = PortagePackage::unslotted(Cpn::parse("sys-libs/pam").unwrap());
        let solution = provider
            .resolve_targets(vec![(pam, PortageVersionSet::any())])
            .unwrap_or_else(|e| panic!("resolution failed: {e:?}"));

        let order = provider.install_order(&solution);
        let names: Vec<String> = order
            .iter()
            .map(|(p, v)| format!("{}-{v}", p.cpn()))
            .collect();
        let pos = |needle: &str| {
            names
                .iter()
                .position(|n| n.starts_with(needle))
                .unwrap_or_else(|| panic!("{needle} missing: {names:?}"))
        };
        // Hard: virtual before python before glibc before libxcrypt.
        assert!(
            pos("virtual/libcrypt") < pos("dev-lang/python"),
            "hard: virtual before python; {names:?}"
        );
        assert!(
            pos("dev-lang/python") < pos("sys-libs/glibc"),
            "hard: python before glibc; {names:?}"
        );
        assert!(
            pos("sys-libs/glibc") < pos("sys-libs/libxcrypt"),
            "hard: glibc before libxcrypt; {names:?}"
        );
        // Soft promote is illegal here; virtual *must* stay before libxcrypt.
        assert!(
            pos("virtual/libcrypt") < pos("sys-libs/libxcrypt"),
            "hard path forbids provider-before-virtual; {names:?}"
        );
    }

    // Guard against regressing the case `order_cycle`'s original heuristic
    // exists for: a component with NO genuine hard cycle (only an ordinary
    // soft/RDEPEND cycle, e.g. `gtk+` <-> its icon-theme runtime deps) must
    // still resolve — the hard-group gate added above is a no-op when every
    // hard-group is a singleton, so this is unaffected by the 2026-07-16 fix.
    #[test]
    fn pure_soft_cycle_still_orders_a_hard_dependent_after_it() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        };

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/a-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                rdepend: (vec![DepEntry::Atom(Dep::parse("dev-libs/b").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                rdepend: (vec![DepEntry::Atom(Dep::parse("dev-libs/a").unwrap())]).into(),
                ..empty()
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/c-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                bdepend: (vec![DepEntry::Atom(Dep::parse("dev-libs/a").unwrap())]).into(),
                ..empty()
            },
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let c = PortagePackage::unslotted(Cpn::parse("app-misc/c").unwrap());
        let solution = provider
            .resolve_targets(vec![(c, PortageVersionSet::any())])
            .unwrap_or_else(|e| panic!("resolution failed: {e:?}"));

        let order = provider.install_order(&solution);
        let names: Vec<&str> = order
            .iter()
            .map(|(p, _)| p.cpn().package.as_str())
            .collect();
        let a_pos = names.iter().position(|&n| n == "a").unwrap();
        let b_pos = names.iter().position(|&n| n == "b");
        let c_pos = names.iter().position(|&n| n == "c").unwrap();
        assert!(b_pos.is_some(), "b must still be scheduled, got: {names:?}");
        assert!(
            a_pos < c_pos,
            "a (c's real hard BDEPEND) must order before c, got: {names:?}"
        );
    }

    // Integration tests that reproduce the texlive-core → kpathsea scenario:
    // slotted packages, `_p` patch versions, `:=` slot-equals deps, and combined
    // slot+use-dep atoms.  These are the exact forms causing missing transitive
    // deps in the real depgraph (all three parse-level hypotheses were falsified).

    fn slot(s: &str) -> Interned<DefaultInterner> {
        Interned::intern(s)
    }

    fn rdepend(atoms: &[&str]) -> PackageDeps {
        PackageDeps {
            depend: (vec![]).into(),
            rdepend: atoms
                .iter()
                .map(|a| DepEntry::Atom(Dep::parse(a).unwrap()))
                .collect::<Vec<_>>()
                .into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        }
    }

    #[test]
    fn slotted_dep_via_slot_equals_operator_is_included() {
        // Reproduces: texlive-core has `>=dev-libs/kpathsea-6.4.0:=` in RDEPEND.
        // kpathsea is slotted (SLOT=0/6.4.0) and available at 6.4.0_p20240311-r1.
        // The solver must include kpathsea when resolving texlive-core.
        let mut repo = InMemoryRepository::new();

        repo.add_version(
            Cpv::parse("app-text/texlive-core-2024").unwrap(),
            Some(slot("0")),
            None,
            rdepend(&[">=dev-libs/kpathsea-6.4.0:="]),
        );
        repo.add_version(
            Cpv::parse("dev-libs/kpathsea-6.4.0_p20240311-r1").unwrap(),
            Some(slot("0")),
            Some(Interned::intern("6.4.0")),
            rdepend(&[]),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let target = PortagePackage::slotted(
            Cpn::parse("app-text/texlive-core").unwrap(),
            Interned::intern("0"),
        );

        let solution = provider
            .resolve_targets(vec![(target, PortageVersionSet::any())])
            .unwrap_or_else(|e| panic!("resolution failed: {e:?}"));

        let names: Vec<String> = provider
            .install_order(&solution)
            .into_iter()
            .filter(|(p, _)| !p.is_virtual())
            .map(|(p, _)| p.cpn().to_string())
            .collect();

        assert!(
            names.contains(&"dev-libs/kpathsea".to_string()),
            "kpathsea must be in install_order; got: {names:?}"
        );
        assert!(
            names.contains(&"app-text/texlive-core".to_string()),
            "texlive-core must be in install_order; got: {names:?}"
        );
    }

    #[test]
    fn slot_equals_with_use_deps_included_in_solution() {
        // Reproduces: `>=media-libs/harfbuzz-1.4.5:=[icu,graphite]`
        // The use deps are constraints on the installed harfbuzz, not on the
        // parent package.  harfbuzz must still appear in install_order.
        let mut repo = InMemoryRepository::new();

        repo.add_version(
            Cpv::parse("app-text/texlive-core-2024").unwrap(),
            Some(slot("0")),
            None,
            rdepend(&[">=media-libs/harfbuzz-1.4.5:=[icu,graphite]"]),
        );
        repo.add_version(
            Cpv::parse("media-libs/harfbuzz-12.3.2").unwrap(),
            Some(slot("0")),
            Some(Interned::intern("6.0.0")),
            rdepend(&[]),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let target = PortagePackage::slotted(
            Cpn::parse("app-text/texlive-core").unwrap(),
            Interned::intern("0"),
        );

        let solution = provider
            .resolve_targets(vec![(target, PortageVersionSet::any())])
            .unwrap_or_else(|e| panic!("resolution failed: {e:?}"));

        let names: Vec<String> = provider
            .install_order(&solution)
            .into_iter()
            .filter(|(p, _)| !p.is_virtual())
            .map(|(p, _)| p.cpn().to_string())
            .collect();

        assert!(
            names.contains(&"media-libs/harfbuzz".to_string()),
            "harfbuzz must be in install_order; got: {names:?}"
        );
    }

    #[test]
    fn versioned_dep_on_p_suffix_version() {
        // `>=dev-libs/kpathsea-6.4.0` must match `6.4.0_p20240311-r1`.
        // VersionSet.contains() must agree with Version's Ord impl.
        use crate::version_set::PortageVersionSet;
        use portage_atom::{Operator, Version};

        let vs = PortageVersionSet::from_operator(
            Operator::GreaterOrEqual,
            false,
            Version::parse("6.4.0").unwrap(),
        );
        for v_str in ["6.4.0_p20240311", "6.4.0_p20240311-r1", "6.5.0"] {
            let v = Version::parse(v_str).unwrap();
            assert!(vs.contains(&v), "VersionSet >=6.4.0 must contain {v_str}");
        }
        for v_str in ["6.3.9", "6.4.0_alpha"] {
            let v = Version::parse(v_str).unwrap();
            assert!(
                !vs.contains(&v),
                "VersionSet >=6.4.0 must NOT contain {v_str}"
            );
        }
    }

    /// The docbook bootstrap cycle: dxsl -R-> bdc -R-> ul -B-> po4a -B-> dxsl, with `ul`'s deps given.
    fn docbook_cycle(ul: PackageDeps) -> InstallOrderResult {
        let mut repo = InMemoryRepository::new();
        repo.add_version(
            Cpv::parse("app-text/dxsl-1").unwrap(),
            None,
            None,
            deps("app-text/bdc", ""),
        );
        repo.add_version(
            Cpv::parse("app-text/bdc-1").unwrap(),
            None,
            None,
            deps("|| ( sys-apps/ul app-misc/getopt )", ""),
        );
        repo.add_version(
            Cpv::parse("app-text/po4a-1").unwrap(),
            None,
            None,
            deps("", "app-text/dxsl"),
        );
        repo.add_version_with_iuse(
            Cpv::parse("sys-apps/ul-1").unwrap(),
            None,
            None,
            vec![slot("nls")],
            ul,
        );
        let mut config = crate::use_config::UseConfig::new();
        config.enable(slot("nls"));
        repo.set_use_config(config);

        let mut provider = PortageDependencyProvider::new(repo);
        let target = PortagePackage::unslotted(Cpn::parse("app-text/dxsl").unwrap());
        let solution = provider
            .resolve_targets(vec![(target, PortageVersionSet::any())])
            .unwrap_or_else(|e| panic!("resolution failed: {e:?}"));
        provider.install_order_with_stats(&solution)
    }

    fn deps(rdepend: &str, bdepend: &str) -> PackageDeps {
        PackageDeps {
            depend: (vec![]).into(),
            rdepend: DepEntry::parse(rdepend).unwrap().into(),
            bdepend: DepEntry::parse(bdepend).unwrap().into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        }
    }

    #[test]
    fn a_flag_gated_build_dep_on_a_broken_cycle_is_a_split_candidate() {
        let result = docbook_cycle(deps("", "nls? ( app-text/po4a )"));
        let splits = result.cycle_splits();
        assert_eq!(splits.len(), 1, "{splits:?}");
        assert_eq!(splits[0].broken.class, DepClass::Rdepend);
        let candidates: Vec<(&str, &str, bool)> = splits[0]
            .candidates
            .iter()
            .map(|c| {
                (
                    c.package.0.cpn().package.as_str(),
                    c.flag.as_str(),
                    c.build_time,
                )
            })
            .collect();
        assert_eq!(candidates, [("ul", "nls", true)]);
    }

    #[test]
    fn splitting_orders_the_cycle_and_places_the_rebuild_after_the_gated_deps() {
        let result = docbook_cycle(deps("", "nls? ( app-text/po4a )"));
        let candidate = result.cycle_splits()[0].candidates[0].clone();
        let build = SplitBuild {
            package: candidate.package,
            flags: vec![candidate.flag],
        };
        let split = result.split(&[build]).expect("ul has nls-gated edges");
        assert!(split.remaining.is_empty(), "{:?}", split.remaining);
        let names: Vec<&str> = split
            .order
            .iter()
            .map(|(p, _)| p.cpn().package.as_str())
            .collect();
        assert_eq!(names, ["ul", "bdc", "dxsl", "po4a", "ul"]);
        assert_eq!(split.rebuilds, [4]);
    }

    #[test]
    fn two_cycles_split_together_each_get_their_own_rebuild() {
        let mut repo = InMemoryRepository::new();
        for p in ["x", "y"] {
            let cpv = |n: &str| Cpv::parse(&format!("app-misc/{p}{n}-1")).unwrap();
            let atom = |n: &str| format!("app-misc/{p}{n}");
            repo.add_version(cpv("top"), None, None, deps(&atom("mid"), ""));
            repo.add_version(cpv("mid"), None, None, deps(&atom("ul"), ""));
            repo.add_version(cpv("tool"), None, None, deps("", &atom("top")));
            let gated = format!("nls? ( {} )", atom("tool"));
            repo.add_version_with_iuse(cpv("ul"), None, None, vec![slot("nls")], deps("", &gated));
        }
        let mut config = crate::use_config::UseConfig::new();
        config.enable(slot("nls"));
        repo.set_use_config(config);
        let mut provider = PortageDependencyProvider::new(repo);
        let targets = ["app-misc/xtop", "app-misc/ytop"].map(|t| {
            (
                PortagePackage::unslotted(Cpn::parse(t).unwrap()),
                PortageVersionSet::any(),
            )
        });
        let solution = provider.resolve_targets(targets.to_vec()).unwrap();
        let result = provider.install_order_with_stats(&solution);

        let builds: Vec<SplitBuild> = result
            .cycle_splits()
            .into_iter()
            .map(|s| SplitBuild {
                package: s.candidates[0].package.clone(),
                flags: vec![s.candidates[0].flag],
            })
            .collect();
        assert_eq!(builds.len(), 2);
        let split = result.split(&builds).expect("both have nls-gated edges");
        assert!(split.remaining.is_empty(), "{:?}", split.remaining);
        let names: Vec<&str> = split
            .order
            .iter()
            .map(|(p, _)| p.cpn().package.as_str())
            .collect();
        for p in ["x", "y"] {
            let (ul, tool) = (format!("{p}ul"), format!("{p}tool"));
            let first = names.iter().position(|n| *n == ul).unwrap();
            let rebuild = names.iter().rposition(|n| *n == ul).unwrap();
            let tool = names.iter().position(|n| *n == tool).unwrap();
            assert!(first < tool && tool < rebuild, "{names:?}");
            assert!(
                split.rebuilds.contains(&rebuild),
                "{:?} in {names:?}",
                split.rebuilds
            );
        }
    }

    #[test]
    fn a_cycle_without_a_gated_edge_has_no_candidate() {
        let splits = docbook_cycle(deps("", "app-text/po4a")).cycle_splits();
        assert_eq!(splits.len(), 1, "{splits:?}");
        assert!(splits[0].candidates.is_empty());
    }

    #[test]
    fn a_flag_that_leaves_another_path_closed_is_not_a_candidate() {
        let splits = docbook_cycle(deps("app-text/po4a", "nls? ( app-text/po4a )")).cycle_splits();
        assert_eq!(splits.len(), 1, "{splits:?}");
        assert!(splits[0].candidates.is_empty(), "{splits:?}");
    }

    #[test]
    fn a_runtime_only_cycle_is_not_a_split() {
        let mut repo = InMemoryRepository::new();
        repo.add_version(
            Cpv::parse("dev-libs/a-1").unwrap(),
            None,
            None,
            rdepend(&["dev-libs/b"]),
        );
        repo.add_version(
            Cpv::parse("dev-libs/b-1").unwrap(),
            None,
            None,
            rdepend(&["dev-libs/a"]),
        );
        let mut provider = PortageDependencyProvider::new(repo);
        let a = PortagePackage::unslotted(Cpn::parse("dev-libs/a").unwrap());
        let solution = provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap();
        assert!(
            provider
                .install_order_with_stats(&solution)
                .cycle_splits()
                .is_empty()
        );
    }
}
