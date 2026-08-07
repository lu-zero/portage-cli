use std::collections::{BinaryHeap, HashMap};

use portage_atom::{Cpn, Version};

use crate::package::PortagePackage;
use crate::provider::PortageDependencyProvider;
use crate::version_set::PortageVersionSet;

// `DepClass` (the five PMS 8.2 dependency variables) is shared vocabulary,
// defined once in `portage-solver`.
pub use portage_solver::DepClass;

/// A labeled edge in the dependency graph: (from_pkg, from_version) depends on
/// (to_pkg, to_version) via the given class.
#[derive(Debug, Clone)]
pub struct DepEdge {
    /// The package that declares the dependency.
    pub from: (PortagePackage, Version),
    /// The package that is depended upon.
    pub to: (PortagePackage, Version),
    /// Which dependency class this edge belongs to.
    pub class: DepClass,
    /// The USE flag in `from` that gates this dep, if it was inside `flag? ( dep )`.
    pub via_use_flag:
        Option<portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>>,
}

impl PortageDependencyProvider {
    /// Build the labeled dependency graph from a solution.
    ///
    /// Returns edges labeled with the dependency class (DEPEND, RDEPEND, etc.).
    /// Only edges where both endpoints are in the solution are included.
    pub fn dependency_graph(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<DepEdge> {
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
            // A direct `self.packages.get(pkg)` here always misses for a
            // `Host` node, silently producing zero outgoing edges for it —
            // so a Host package's own BDEPEND (e.g. one Host-routed perl
            // module needing another Host-routed perl) never gets an
            // ordering edge, and `install_order` falls back to an arbitrary
            // tie-break instead of real dependency order. Found live: a
            // riscv64 stage3 `--cross` build routed a whole chain of Host
            // BDEPEND packages (`dev-lang/perl` and its `dev-perl/*`
            // consumers) with no ordering edges between them, so `perl`
            // landed *after* consumers that need it.
            let Some(data) = self.package_data(pkg) else {
                continue;
            };
            let Some(vd) = data.versions.get(version) else {
                continue;
            };

            for (class_idx, &class) in classes.iter().enumerate() {
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
                            if let Some(vdata) = self.package_data(dp) {
                                for vver in vdata.versions.values() {
                                    for (idp, idvs, _) in vver.by_class.iter().flatten() {
                                        work.push((idp, idvs));
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

        edges
    }

    /// Compute an installation order from a solution.
    ///
    /// Returns packages in topological order: a dependency is merged before the
    /// package that needs it.  Both build-time (DEPEND/BDEPEND) and runtime
    /// (RDEPEND) edges constrain the order, so e.g. the requested target lands
    /// after the libraries it links and runs against.  PDEPEND (merged *after*
    /// the parent) and IDEPEND (install-time only) do not constrain it.
    ///
    /// RDEPEND introduces cycles far more often than build deps alone (e.g.
    /// `gtk+` ↔ its icon-theme/at-spi runtime deps).  Portage resolves these by
    /// treating runtime edges as *soft*: when the graph stalls in a cycle, soft
    /// edges are dropped to break it while hard build-time edges are preserved.
    /// We do the same — only if a genuine hard (build-time) cycle remains, as
    /// with bootstrap cycles (`xz-utils` ↔ `elt-patches`), do we fall back to a
    /// deterministic lexicographic tie-break.
    ///
    /// Two refinements on top of that Portage-shaped walk (bug #3, 2026-08-07):
    /// - **B:** inside a soft SCC, prefer fully soft-ready nodes among hard-ready
    ///   ones (`order_cycle` pick order).
    /// - **C:** after the walk, re-linearise promoting soft edges that remain
    ///   acyclic w.r.t. hard (+ already promoted soft) constraints — pass-1
    ///   orientation first, then inverted — with the pass-1 order as tie-break.
    ///   Fixes empty `virtual/*` RDEPEND providers that pass-1 emitted early while
    ///   the real provider was still hard-blocked (e.g. `virtual/libcrypt` before
    ///   `sys-libs/libxcrypt` through the glibc bootstrap cycle).
    pub fn install_order(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<(PortagePackage, Version)> {
        let graph = self.dependency_graph(solution);

        // Index nodes deterministically (sorted by key) so SCC discovery and all
        // tie-breaks are reproducible.
        let mut node_pv: Vec<(String, (PortagePackage, Version))> = solution
            .iter()
            .map(|(pkg, ver)| (format!("{}-{}", pkg, ver), (pkg.clone(), ver.clone())))
            .collect();
        node_pv.sort_by(|a, b| a.0.cmp(&b.0));
        let n = node_pv.len();
        let idx: HashMap<&str, usize> = node_pv
            .iter()
            .enumerate()
            .map(|(i, (k, _))| (k.as_str(), i))
            .collect();

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
            let to = format!("{}-{}", edge.to.0, edge.to.1);
            let from = format!("{}-{}", edge.from.0, edge.from.1);
            let (Some(&u), Some(&v)) = (idx.get(to.as_str()), idx.get(from.as_str())) else {
                continue;
            };
            succ_all[u].push(v);
            if hard {
                succ_hard[u].push(v);
            }
        }
        for adj in succ_all.iter_mut() {
            adj.sort_unstable();
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

        // The component key (max member node key) drives a deterministic
        // max-heap tie-break, preserving the "largest ready first" ordering and
        // keeping the requested target — which has no dependents and so becomes
        // ready last — near the end.
        let comp_key = |c: usize| -> &str {
            members[c]
                .iter()
                .map(|&i| node_pv[i].0.as_str())
                .max()
                .unwrap_or("")
        };

        let mut comp_ready: BinaryHeap<(String, usize)> = (0..num_comps)
            .filter(|&c| comp_indeg[c] == 0)
            .map(|c| (comp_key(c).to_string(), c))
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
                    comp_ready.push((comp_key(cv).to_string(), cv));
                }
            }
        }

        // Pass 2: restore inverted soft edges that remain acyclic (bug #3).
        repair_soft_inversions(result, &graph)
    }
}

/// Iterative Tarjan SCC.  Returns the component id of each node; ids are dense
/// `0..num_components`.  `succ[u]` lists nodes that must come *after* `u`.
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

/// Order the members of a single `succ_all` (hard+soft) component.
///
/// A multi-member component here does NOT mean every member is part of a
/// genuine hard cycle — an ordinary soft (RDEPEND) cycle anywhere among these
/// packages folds everything reachable through it into one component, even
/// packages with a perfectly ordinary, acyclic hard (DEPEND/BDEPEND) chain
/// onto something inside it (e.g. dozens of bootstrap tools all needing
/// `app-portage/elt-patches`, which itself has a genuine 2-node hard cycle
/// with `app-arch/xz-utils` — found live 2026-07-16, a real `--local`
/// from-scratch bootstrap folded 114 of 229 packages into one component this
/// way). Those non-cyclic hard dependents must still be ordered after their
/// real hard prerequisite, unconditionally — only the *actual* hard-cycle
/// members have no valid total order and need a heuristic tie-break.
///
/// So: first isolate the genuinely irreducible hard cycles within this
/// component (Tarjan over `succ_hard` restricted to `members` — cheap, this
/// component is usually tiny once soft edges are set aside). A member with
/// an unmet hard predecessor *outside its own hard-group* is never emitted
/// while an eligible member remains — every hard edge that isn't part of a
/// real hard cycle is respected exactly, regardless of what unrelated soft
/// cycle pulled it into this bigger component. This can't stall: the
/// hard-group condensation is itself a DAG, so an eligible member (no
/// pending cross-group hard predecessor) always exists.
///
/// Within one hard-group (a real cycle, no valid order exists), fall back to
/// the original heuristic: repeatedly emit the member closest to ready —
/// fewest pending in-component hard deps, then prefer no pending soft deps
/// (**B**), then fewest pending soft+hard, then largest key for determinism.
/// Groups that are all singletons (no real hard cycle present) behave
/// identically to a plain topological sort when soft edges are acyclic —
/// soft cycles still force a break; pass-2 (`repair_soft_inversions`) may
/// restore inverted soft edges that remain acyclic with hard constraints.
fn order_cycle(
    members: &[usize],
    succ_hard: &[Vec<usize>],
    succ_all: &[Vec<usize>],
    node_pv: &[(String, (PortagePackage, Version))],
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

/// Pass-2 repair after the soft-cycle walk (bug #3).
///
/// Pass-1 may emit a consumer before its RDEPEND provider when the provider is
/// still hard-blocked inside a soft SCC (empty `virtual/libcrypt` before
/// `sys-libs/libxcrypt`).  Rebuild a total order from constraints, carefully:
///
/// 1. **Lock pass-1-forward hard + soft edges** (`pos(dep) < pos(consumer)`).
///    These form a subgraph of the pass-1 total order → always acyclic. Never
///    drop a hard edge pass-1 already got right (live regression: pass-2 used
///    to add hard edges in arbitrary order, skip some on a hard-SCC false
///    path, then soft promotions reordered `gcc` before its BDEPEND `glibc`).
/// 2. **Try inverted hard edges** (pass-1 violated a DEPEND/BDEPEND) if acyclic.
/// 3. **Try inverted soft edges** (earliest pass-1 consumer first) if acyclic —
///    this is the empty-virtual-before-provider fix when no hard path
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
) -> Vec<(PortagePackage, Version)> {
    let n = order.len();
    if n <= 1 {
        return order;
    }

    let node_key = |p: &PortagePackage, v: &Version| format!("{p}-{v}");
    let mut pos: HashMap<String, usize> = HashMap::with_capacity(n);
    for (i, (p, v)) in order.iter().enumerate() {
        pos.insert(node_key(p, v), i);
    }

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

    let try_add = |u: usize, v: usize, succ: &mut [Vec<usize>], indeg: &mut [usize]| -> bool {
        if u == v {
            return false;
        }
        if succ[u].contains(&v) {
            return true;
        }
        // Adding u→v (u before v) cycles if v can already reach u.
        if reaches(v, u, succ) {
            return false;
        }
        succ[u].push(v);
        indeg[v] += 1;
        true
    };

    // (dep_idx, consumer_idx) = dep must come before consumer.
    let mut hard: Vec<(usize, usize)> = Vec::new();
    let mut soft: Vec<(usize, usize)> = Vec::new();
    for edge in graph {
        let Some(&from_i) = pos.get(&node_key(&edge.from.0, &edge.from.1)) else {
            continue;
        };
        let Some(&to_i) = pos.get(&node_key(&edge.to.0, &edge.to.1)) else {
            continue;
        };
        match edge.class {
            DepClass::Depend | DepClass::Bdepend => hard.push((to_i, from_i)),
            DepClass::Rdepend => soft.push((to_i, from_i)),
            _ => {}
        }
    }
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

    // 3. Fix soft inversions (empty virtual before provider, etc.).
    soft_inv.sort_by_key(|(dep, consumer)| (*consumer, *dep));
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

/// Whether `start` can reach `target` following `succ` edges.
fn reaches(start: usize, target: usize, succ: &[Vec<usize>]) -> bool {
    if start == target {
        return true;
    }
    let mut stack = vec![start];
    let mut visited = vec![false; succ.len()];
    visited[start] = true;
    while let Some(u) = stack.pop() {
        for &v in &succ[u] {
            if v == target {
                return true;
            }
            if !visited[v] {
                visited[v] = true;
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
                depend: (vec![DepEntry::Atom(Dep::parse("dev-libs/bottom-1.0").unwrap())]).into(),
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
    }

    /// Regression test for the riscv64 stage3 shakeout: `dependency_graph`
    /// did a raw `self.packages.get(pkg)` lookup instead of the alias-resolving
    /// `self.package_data(pkg)` — so a `Host`-flavored solved package (whose
    //
    // See issue #33.
    /// data lives under its `Target`-flavored alias, see `ensure_host_instances`)
    /// always missed, silently producing zero outgoing edges for it. A `Host`
    /// package's own BDEPEND on *another* `Host` package (e.g. one Host-routed
    /// perl module needing Host-routed perl itself) then got no ordering edge
    /// at all, so `install_order` could place the dependency *after* its own
    /// consumer instead of before it.
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
                depend: (vec![DepEntry::Atom(Dep::parse("dev-libs/lib-1.0").unwrap())]).into(),
                rdepend: (vec![DepEntry::Atom(Dep::parse("dev-libs/runtime-1.0").unwrap())]).into(),
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

    /// Regression test for the 2026-07-16 `order_cycle` bug: an ordinary,
    /// acyclic hard (BDEPEND) dependent of a genuine hard-cycle member must
    /// still be ordered *after* it, even when an unrelated soft (RDEPEND)
    /// cycle elsewhere folds both into the same `succ_all` component.
    ///
    /// Shape: `dev-util/elt` <-> `dev-util/xz` is a genuine hard (BDEPEND)
    /// cycle. `sys-apps/sweep` has an ordinary hard BDEPEND on `elt` — no
    /// cyclic relationship with it at all. `dev-util/fn` RDEPENDs on
    /// `sweep`, and `elt` RDEPENDs on `fn` — a soft back-path that pulls
    /// `sweep` into the same `succ_all` component as the `elt`/`xz` hard
    /// cycle. `sweep`'s name is deliberately picked with a *larger* sort key
    /// than `elt`'s, so the old, ungated indeg tie-break picked it first
    /// (traced by hand against the pre-fix code): `fn` reaches `indeg_hard
    /// == 0` first and is emitted, then `elt` and `sweep` tie at
    /// `indeg_hard == 1` / `indeg_all == 1`, and the largest-key tie-break
    /// picked `sweep` — before its own real hard dependency `elt`.
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

    /// Live clang-world #3 (2026-08-07): soft+hard cycle through the libcrypt
    /// bootstrap chain lets `order_cycle` emit empty `virtual/libcrypt` *before*
    /// its RDEPEND provider `sys-libs/libxcrypt`.
    ///
    /// Real tree cycle (all in one `succ_all` SCC once soft edges are kept):
    /// ```text
    /// virtual/libcrypt -RDEPEND→ libxcrypt -DEPEND→ glibc -DEPEND→ virtual/os-headers
    ///   -RDEPEND→ linux-headers -BDEPEND→ perl -DEPEND→ virtual/libcrypt
    /// ```
    /// Hard graph alone is acyclic; only soft RDEPEND closes the loop. The
    /// walker then prefers hard-ready nodes: empty virtuals have hard-indegree 0
    /// (their provider edge is soft), so they emit first. Downstream
    /// `build_blockers` only keeps edges with `to < from`, so the soft edge is
    /// dropped and pam can start with no provider merged.
    ///
    /// Minimal dual-root fixture without this cycle *does* order provider first
    /// (`cross_target_virtual_rdepend_provider_is_target_not_host`). This test
    /// is the cycle shape that breaks live.
    ///
    /// Pass-1 alone (wrong): `virtual/os-headers`, **`virtual/libcrypt`**,
    /// python, glibc, **libxcrypt**, … — fixed by pass-2 soft repair.
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

    /// Live regression (Sonnet 2026-08-07, full clang plan after first B+C):
    /// pass-2 reordered `sys-devel/gcc` *before* its hard BDEPEND
    /// `sys-libs/glibc`, tripping pre-flight. Soft repair must never drop a
    /// hard edge pass-1 already oriented correctly.
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
        repo.add_version(Cpv::parse("sys-devel/bison-3.8").unwrap(), None, None, empty());
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

    /// Full-graph #3 limitation: hard path virtual → python → glibc → libxcrypt
    /// makes soft provider-before-virtual promote impossible. Document that
    /// repair does not invent a hard-illegal order (and may leave virtual early).
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

    /// Guard against regressing the case `order_cycle`'s original heuristic
    /// exists for: a component with NO genuine hard cycle (only an ordinary
    /// soft/RDEPEND cycle, e.g. `gtk+` <-> its icon-theme runtime deps) must
    /// still resolve — the hard-group gate added above is a no-op when every
    /// hard-group is a singleton, so this is unaffected by the 2026-07-16 fix.
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
}
