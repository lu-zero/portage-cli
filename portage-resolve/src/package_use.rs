//! `package.use` entry synthesis: turning cross-package `[flag]` USE-dep
//! requirements into concrete `package.use` lines, and the co-solve fixpoint
//! that auto-applies them (emerge's autounmask-preview behaviour)

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use camino::Utf8Path;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    DepEdge, IUseDefault, PortagePackage, UseDemand, UseFlagRequirement, UseFlagState, UseLayer,
    UseOverride, resolve_effective_use,
};

/// Entries to write into `/etc/portage/package.use`
pub struct PackageUseEntry {
    /// Filename inside `package.use/`
    ///
    /// The bare package name (e.g `pygments`), or `category-package` (e.g
    /// `dev-python-pygments`) when the bare name is ambiguous within this
    /// batch — see `assign_filenames` (private, in this module).
    pub filename: String,
    /// Lines to add/update in that file
    pub lines: Vec<PackageUseLine>,
}

/// One `package.use` line: an atom, the flags it forces, and the explanatory
/// comments placed above it
#[derive(Clone, PartialEq, Eq)]
pub struct PackageUseLine {
    /// Comment lines explaining the requirement, e.g. `# required by firefox`
    pub comments: Vec<String>,
    /// The atom spec, e.g. `>=dev-python/pygments-2.19.2`
    pub atom: String,
    /// Flags to enable (no prefix) and disable (`-` prefix)
    pub flags: Vec<String>,
}

/// Build `package.use` entries for all non-trivial USE flag requirements
#[allow(clippy::too_many_arguments)]
pub fn build_entries(
    flag_reqs: &[UseFlagRequirement],
    root_atoms: &[String],
    root_labels: &[String],
    edges: &[DepEdge],
    defaults: &UseLayer,
    env_use: &UseLayer,
    package_use: &[(Dep, Vec<UseOverride>)],
    profile_package_use: &[portage_atom_pubgrub::ProfileUseNode],
    conf: &UseLayer,
    iuse_defaults_of: &dyn Fn(&Cpv) -> HashMap<Interned<DefaultInterner>, IUseDefault>,
    matched: &[MatchedFlag],
    conflicts: &[UseConflict],
) -> Vec<PackageUseEntry> {
    // Pre-compute once for all requirements.
    let adj = build_adjacency(edges);
    let root_cpns = parse_root_cpns(root_atoms);
    // The value each settled flag must keep; `None` for a conflict, which gets no line either way.
    let mut settled: HashMap<(Cpn, Interned<DefaultInterner>), Option<bool>> = HashMap::new();
    for m in matched {
        settled.insert((*m.package.cpn(), m.flag), Some(m.enable));
    }
    for c in conflicts {
        settled.insert((*c.package.cpn(), c.flag), None);
    }
    let keeps = |cpn: Cpn, flag: Interned<DefaultInterner>, enable: bool| {
        settled.get(&(cpn, flag)).is_none_or(|v| *v == Some(enable))
    };

    let mut by_cpn: HashMap<Cpn, Vec<PackageUseLine>> = HashMap::new();

    for req in flag_reqs {
        if req.required_enabled.is_empty() && req.required_disabled.is_empty() {
            continue;
        }
        if req.package.is_virtual() {
            continue;
        }

        let cpn = *req.package.cpn();

        let ver = req.upgrade_to.as_ref().unwrap_or(&req.version);
        let slot_suffix = req
            .package
            .slot()
            .map(|s| format!(":{}", s.as_str()))
            .unwrap_or_default();
        let atom = format!(">={}-{}{}", cpn, ver_str(ver), slot_suffix);

        // A flag already set by the pristine config, including an IUSE
        // `+flag` default, is not a package.use change.
        let cpv = Cpv::new(cpn, ver.clone());
        let iuse_defaults = iuse_defaults_of(&cpv);
        let eff = resolve_effective_use(
            &iuse_defaults,
            defaults,
            &cpv,
            req.package.slot(),
            package_use,
            env_use,
            profile_package_use,
            conf,
        );
        let mut flags: Vec<String> = Vec::new();
        for f in &req.required_enabled {
            if eff.get_opt(*f) != Some(UseFlagState::Enabled) && keeps(cpn, *f, true) {
                flags.push(f.as_str().to_string());
            }
        }
        for f in &req.required_disabled {
            if eff.get_opt(*f) != Some(UseFlagState::Disabled) && keeps(cpn, *f, false) {
                flags.push(format!("-{}", f.as_str()));
            }
        }
        if flags.is_empty() {
            continue;
        }

        let comments = build_comments(req, root_labels, &root_cpns, &adj);

        add_line(&mut by_cpn, cpn, atom, flags, comments);
    }

    // Parents flipped to agree with a settled flag, grouped per parent atom and target.
    struct Flip {
        parent: Cpn,
        flags: Vec<String>,
        wanted: Vec<String>,
        spelled: Vec<String>,
    }
    let mut flips: BTreeMap<(String, String), Flip> = BTreeMap::new();
    for m in matched {
        let target = m.package.cpn();
        for d in &m.flipped {
            let Some(agree) = d.parent_value_to_agree() else {
                continue;
            };
            let parent = *d.parent.cpn();
            let slot = d
                .parent
                .slot()
                .map(|s| format!(":{}", s.as_str()))
                .unwrap_or_default();
            let atom = format!(">={}-{}{}", parent, ver_str(&d.parent_version), slot);
            let flip = flips
                .entry((atom, target.to_string()))
                .or_insert_with(|| Flip {
                    parent,
                    flags: Vec::new(),
                    wanted: Vec::new(),
                    spelled: Vec::new(),
                });
            push_new(&mut flip.flags, signed(d.flag, agree));
            push_new(&mut flip.wanted, signed(m.flag, m.enable));
            push_new(&mut flip.spelled, use_dep_spelling(d));
        }
    }
    for ((atom, target), flip) in flips {
        let pn = target.rsplit('/').next().unwrap_or(&target);
        let comment = format!(
            "# to match {target}[{}]: {} needs {pn}[{}]",
            flip.wanted.join(","),
            flip.parent,
            flip.spelled.join(",")
        );
        add_line(&mut by_cpn, flip.parent, atom, flip.flags, vec![comment]);
    }

    assign_filenames(by_cpn)
}

/// Add `flags` to the line for `atom`, starting one if there is none
///
/// `flag_reqs` merges the co-solve's applied requirements with the solver's own, so the same flags and
/// comments can arrive twice.
fn add_line(
    by_cpn: &mut HashMap<Cpn, Vec<PackageUseLine>>,
    cpn: Cpn,
    atom: String,
    flags: Vec<String>,
    comments: Vec<String>,
) {
    let lines = by_cpn.entry(cpn).or_default();
    match lines.iter_mut().find(|l| l.atom == atom) {
        Some(line) => {
            for c in comments {
                push_new(&mut line.comments, c);
            }
            for f in flags {
                push_new(&mut line.flags, f);
            }
        }
        None => lines.push(PackageUseLine {
            comments,
            atom,
            flags,
        }),
    }
}

fn push_new(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

fn signed(flag: Interned<DefaultInterner>, enable: bool) -> String {
    if enable {
        flag.as_str().to_string()
    } else {
        format!("-{}", flag.as_str())
    }
}

/// A USE dependency as written in an atom: `flag`, `-flag`, `flag=`, `!flag=`, `flag?` or `!flag?`
pub fn use_dep_spelling(d: &UseDemand) -> String {
    use portage_atom::UseDepKind;
    let flag = d.flag.as_str();
    match d.kind {
        UseDepKind::Enabled => flag.to_string(),
        UseDepKind::Disabled => format!("-{flag}"),
        UseDepKind::Equal => format!("{flag}="),
        UseDepKind::EqualInverse => format!("!{flag}="),
        UseDepKind::Conditional => format!("{flag}?"),
        UseDepKind::ConditionalInverse => format!("!{flag}?"),
    }
}

/// Turn per-package line groups into [`PackageUseEntry`]s, choosing each one's filename
///
/// The bare package name (e.g `mesa`) when unambiguous, falling back to
/// `category-package` only for names that collide across categories in
/// this batch — real portage users keep bare-name package.use files day to
/// day. Sorted by filename for reproducible output; lines within a file
/// keep their caller-given order.
fn assign_filenames(by_cpn: HashMap<Cpn, Vec<PackageUseLine>>) -> Vec<PackageUseEntry> {
    let cpns: Vec<Cpn> = by_cpn.keys().copied().collect();
    let mut names_by_bare: HashMap<&str, HashSet<&str>> = HashMap::new();
    for cpn in &cpns {
        names_by_bare
            .entry(cpn.package.as_str())
            .or_default()
            .insert(cpn.category.as_str());
    }

    let mut entries: Vec<PackageUseEntry> = by_cpn
        .into_iter()
        .map(|(cpn, lines)| {
            let filename = if names_by_bare[cpn.package.as_str()].len() > 1 {
                format!(
                    "{}-{}",
                    cpn.category.as_str().replace('/', "-"),
                    cpn.package.as_str()
                )
            } else {
                cpn.package.as_str().to_string()
            };
            PackageUseEntry { filename, lines }
        })
        .collect();
    entries.sort_by(|a, b| a.filename.cmp(&b.filename));
    entries
}

fn ver_str(v: &Version) -> String {
    v.to_string()
}

/// Why a parent's flag cannot be flipped to settle a USE conflict
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagPin {
    /// Set by the user's `package.use`
    Config,
    /// `use.mask`ed by the profile
    Masked,
    /// `use.force`d by the profile
    Forced,
    /// Already changed the other way for another demand
    Decided,
}

/// A flag demanded both ways, settled by flipping the parents whose `[flag=]`-style demand lost
#[derive(PartialEq)]
pub struct MatchedFlag {
    /// The package whose flag was demanded
    pub package: PortagePackage,
    /// Its planned version
    pub version: Version,
    /// The flag
    pub flag: Interned<DefaultInterner>,
    /// The value the plain `[flag]`/`[-flag]` demands asked for
    pub enable: bool,
    /// The losing demands; each parent's own flag is set to [`UseDemand::parent_value_to_agree`]
    pub flipped: Vec<UseDemand>,
}

/// Demands on one flag that cannot all hold
#[derive(PartialEq)]
pub struct UseConflict {
    /// The package whose flag was demanded
    pub package: PortagePackage,
    /// Its planned version
    pub version: Version,
    /// The flag
    pub flag: Interned<DefaultInterner>,
    /// Every demand, with what keeps its parent's flag from changing, if anything
    pub demands: Vec<(UseDemand, Option<FlagPin>)>,
}

/// Result of [`cosolve_use_deps`]
pub struct CosolveOutcome<T> {
    /// The caller's `package.use` plus the forced flags
    pub package_use: Vec<(Dep, Vec<UseOverride>)>,
    /// The requirements that drove at least one forced flag
    pub applied: Vec<UseFlagRequirement>,
    /// Flags demanded both ways and settled by flipping parents
    pub matched: Vec<MatchedFlag>,
    /// Flags demanded both ways that no change settles; none of them is forced
    pub conflicts: Vec<UseConflict>,
    /// The solve of `package_use`, when the fixpoint ended on one
    pub solved: Option<T>,
}

/// Auto-apply cross-package `[flag]` USE-deps to a fixpoint (emerge's
/// autounmask-preview dependency calculation)
///
/// Starting from `package_use`, repeatedly: solve, read the in-plan
/// USE-flag requirements via `reqs_of`, force every demanded flag that is
/// real IUSE of its target via a synthetic `cpn flags` entry, and re-solve
/// — until no new flag is added. Flags the caller's *initial* `package_use`
/// sets either way are pins, never forced (emerge refuses to override
/// explicit configuration; the demand is left to the advisory).
///
/// A flag demanded both ways is settled when one side is plain `[flag]`/`[-flag]` and the other only
/// follows its parents (`[flag=]`, `[flag?]`, …): the plain side wins and those parents' flags are set
/// to agree, unless `fixed` or the caller's `package.use` holds one of them; then it is a conflict.
///
/// [`CosolveOutcome::solved`] is `None` when a solve failed or the iteration bound was hit (the
/// returned `package_use` then needs a fresh solve). A flag forced once is never re-forced, so the bound
/// is a backstop.
pub fn cosolve_use_deps<T, S, R, F>(
    mut package_use: Vec<(Dep, Vec<UseOverride>)>,
    data: &crate::repo::RepoData,
    solve: S,
    reqs_of: R,
    fixed: F,
) -> CosolveOutcome<T>
where
    S: Fn(&[(Dep, Vec<UseOverride>)]) -> Option<T>,
    R: Fn(&T) -> Vec<UseFlagRequirement>,
    F: Fn(&PortagePackage, &Version, Interned<DefaultInterner>) -> Option<FlagPin>,
{
    type Key = (Cpn, Interned<DefaultInterner>);

    // Flags pinned by the caller's configuration: never forced, matching emerge's refusal to override
    // explicit config.
    let pinned: HashSet<Key> = package_use
        .iter()
        .flat_map(|(dep, overrides)| overrides.iter().map(|o| (dep.cpn, o.flag)))
        .collect();
    let configured = package_use.len();

    let mut applied: HashMap<Key, bool> = HashMap::new();
    let mut applied_reqs = Vec::new();
    // Every demand seen on a flag, across iterations: the two sides of a conflict can surface in different
    // solves, as forcing one side is what violates the other.
    let mut seen: HashMap<Key, (PortagePackage, Version, Vec<UseDemand>)> = HashMap::new();
    let mut decisions: HashMap<Key, Settled> = HashMap::new();
    let mut solved = None;
    for _ in 0..8 {
        let Some(solution) = solve(&package_use) else {
            break;
        };
        let reqs = reqs_of(&solution);
        for req in &reqs {
            let targets = req_targets(req, data);
            let ver = req.upgrade_to.as_ref().unwrap_or(&req.version);
            for d in &req.demands {
                let key = (*req.package.cpn(), d.flag);
                if !targets.iter().any(|&(_, f, _)| f == d.flag) {
                    continue;
                }
                let (_, _, demands) = seen
                    .entry(key)
                    .or_insert_with(|| (req.package.clone(), ver.clone(), Vec::new()));
                if !demands.contains(d) {
                    demands.push(d.clone());
                }
            }
        }

        // Re-decided every time: a demand arriving later, once its parent enters the plan, can flip
        // another parent or turn the flag into a conflict.
        let mut changed = false;
        let mut contested: Vec<&Key> = seen
            .iter()
            .filter(|(key, (_, _, demands))| {
                !pinned.contains(*key)
                    && demands.iter().any(|d| d.enable)
                    && demands.iter().any(|d| !d.enable)
            })
            .map(|(key, _)| key)
            .collect();
        contested
            .sort_by(|a, b| (a.0.to_string(), a.1.as_str()).cmp(&(b.0.to_string(), b.1.as_str())));
        for key in contested {
            let (package, version, demands) = &seen[key];
            let decision = settle(package, version, key.1, demands, |d, agree| {
                let parent_key = (*d.parent.cpn(), d.flag);
                if pinned.contains(&parent_key) {
                    Some(FlagPin::Config)
                } else if applied.get(&parent_key) == Some(&!agree) {
                    Some(FlagPin::Decided)
                } else {
                    fixed(&d.parent, &d.parent_version, d.flag)
                }
            });
            if decisions.get(key) == Some(&decision) {
                continue;
            }
            changed = true;
            if let Some(Settled::Matched(old)) = decisions.get(key) {
                for d in &old.flipped {
                    clear_override(&mut package_use, configured, (*d.parent.cpn(), d.flag));
                    applied.remove(&(*d.parent.cpn(), d.flag));
                }
            }
            match &decision {
                Settled::Matched(m) => {
                    set_override(&mut package_use, configured, &mut applied, *key, m.enable);
                    for d in &m.flipped {
                        if let Some(agree) = d.parent_value_to_agree() {
                            let parent_key = (*d.parent.cpn(), d.flag);
                            set_override(
                                &mut package_use,
                                configured,
                                &mut applied,
                                parent_key,
                                agree,
                            );
                        }
                    }
                }
                Settled::Conflict(_) => {
                    clear_override(&mut package_use, configured, *key);
                    applied.remove(key);
                }
            }
            decisions.insert(*key, decision);
        }

        let mut new_by_cpn: HashMap<Cpn, Vec<UseOverride>> = HashMap::new();
        for req in reqs {
            let mut contributed = false;
            for (cpn, flag, enable) in req_targets(&req, data) {
                let key = (cpn, flag);
                if pinned.contains(&key)
                    || decisions.contains_key(&key)
                    || applied.contains_key(&key)
                {
                    continue; // pinned, demanded both ways, or already forced
                }
                applied.insert(key, enable);
                contributed = true;
                new_by_cpn
                    .entry(cpn)
                    .or_default()
                    .push(UseOverride { flag, enable });
            }
            if contributed {
                applied_reqs.push(req);
            }
        }
        if new_by_cpn.is_empty() && !changed {
            solved = Some(solution);
            break;
        }
        for (cpn, flags) in new_by_cpn {
            if let Ok(dep) = Dep::parse(&cpn.to_string()) {
                package_use.push((dep, flags));
            }
        }
    }

    let mut decisions: Vec<(Key, Settled)> = decisions.into_iter().collect();
    decisions.sort_by(|(a, _), (b, _)| {
        (a.0.to_string(), a.1.as_str()).cmp(&(b.0.to_string(), b.1.as_str()))
    });
    let mut out = CosolveOutcome {
        package_use,
        applied: applied_reqs,
        matched: Vec::new(),
        conflicts: Vec::new(),
        solved,
    };
    for (_, decision) in decisions {
        match decision {
            Settled::Matched(m) => out.matched.push(m),
            Settled::Conflict(c) => out.conflicts.push(c),
        }
    }
    out
}

/// How a flag demanded both ways was settled
#[derive(PartialEq)]
enum Settled {
    Matched(MatchedFlag),
    Conflict(UseConflict),
}

/// Settle a flag demanded both ways; `pin_of` says what holds a losing demand's parent flag
///
/// The plain `[flag]`/`[-flag]` side wins when all of them agree and every parent on the other side can
/// be set to agree.
fn settle(
    package: &PortagePackage,
    version: &Version,
    flag: Interned<DefaultInterner>,
    demands: &[UseDemand],
    pin_of: impl Fn(&UseDemand, bool) -> Option<FlagPin>,
) -> Settled {
    let plain: HashSet<bool> = demands
        .iter()
        .filter(|d| d.parent_value_to_agree().is_none())
        .map(|d| d.enable)
        .collect();
    let winner = (plain.len() == 1)
        .then(|| plain.into_iter().next())
        .flatten();
    let pins: Vec<Option<FlagPin>> = demands
        .iter()
        .map(|d| {
            let agree = d.parent_value_to_agree()?;
            (winner == Some(!d.enable))
                .then(|| pin_of(d, agree))
                .flatten()
        })
        .collect();
    match winner {
        Some(enable) if pins.iter().all(Option::is_none) => Settled::Matched(MatchedFlag {
            package: package.clone(),
            version: version.clone(),
            flag,
            enable,
            flipped: demands
                .iter()
                .filter(|d| d.enable != enable)
                .cloned()
                .collect(),
        }),
        _ => {
            let mut demands: Vec<_> = demands.iter().cloned().zip(pins).collect();
            demands.sort_by_cached_key(|(d, pin)| {
                (!d.enable, pin.is_none(), d.parent.cpn().to_string())
            });
            Settled::Conflict(UseConflict {
                package: package.clone(),
                version: version.clone(),
                flag,
                demands,
            })
        }
    }
}

/// Drop any flag the co-solve forced for `key`, leaving the caller's first `configured` entries alone
fn clear_override(
    package_use: &mut Vec<(Dep, Vec<UseOverride>)>,
    configured: usize,
    (cpn, flag): (Cpn, Interned<DefaultInterner>),
) {
    for (dep, overrides) in &mut package_use[configured..] {
        if dep.cpn == cpn {
            overrides.retain(|o| o.flag != flag);
        }
    }
    let mut i = 0;
    package_use.retain(|(_, overrides)| {
        i += 1;
        i <= configured || !overrides.is_empty()
    });
}

/// Force `key` to `enable`, replacing an earlier forced value
fn set_override(
    package_use: &mut Vec<(Dep, Vec<UseOverride>)>,
    configured: usize,
    applied: &mut HashMap<(Cpn, Interned<DefaultInterner>), bool>,
    key: (Cpn, Interned<DefaultInterner>),
    enable: bool,
) {
    clear_override(package_use, configured, key);
    applied.insert(key, enable);
    if let Ok(dep) = Dep::parse(&key.0.to_string()) {
        package_use.push((
            dep,
            vec![UseOverride {
                flag: key.1,
                enable,
            }],
        ));
    }
}

/// The `(target cpn, flag, enable)` triples one requirement demands
///
/// Filtered to flags that are **real IUSE** of the target's
/// selected/upgrade version (a `[bar]` on a target without `bar` cannot be
/// applied — CC7).
///
/// Used by the co-solve to auto-apply USE-deps via synthetic `package.use` and
/// re-solve, instead of only suggesting them.
fn req_targets(
    req: &UseFlagRequirement,
    data: &crate::repo::RepoData,
) -> Vec<(
    portage_atom::Cpn,
    portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>,
    bool,
)> {
    let mut out = Vec::new();
    {
        if req.package.is_virtual()
            || (req.required_enabled.is_empty() && req.required_disabled.is_empty())
        {
            return out;
        }
        let cpn = *req.package.cpn();
        let ver = req.upgrade_to.as_ref().unwrap_or(&req.version);
        let iuse: HashSet<String> = crate::repo::find_cache(data, &req.package, ver)
            .map(|c| {
                c.metadata
                    .iuse
                    .iter()
                    .map(|i| i.name().to_string())
                    .collect()
            })
            .unwrap_or_default();
        for f in &req.required_enabled {
            if iuse.contains(f.as_str()) {
                out.push((cpn, *f, true));
            }
        }
        for f in &req.required_disabled {
            if iuse.contains(f.as_str()) {
                out.push((cpn, *f, false));
            }
        }
    }
    out
}

/// Adjacency map: CPN → Vec<(to_CPN, annotation)>
/// annotation = "from-cpv\[flag\]" when gated, "from-cpv" otherwise
///
/// Ordered, not hashed: [`build_comments`] seeds its search by iterating this
/// and keeps the *first* chain that reaches the target, so a randomized key
/// order made the `# required by` justification flip between equally-valid
/// requirers from run to run. The map is plan-sized, so ordering costs nothing.
type Adjacency = BTreeMap<String, Vec<(String, String)>>;

fn build_adjacency(edges: &[DepEdge]) -> Adjacency {
    let mut adj: Adjacency = BTreeMap::new();
    for e in edges {
        if e.from.0.is_virtual() || e.to.0.is_virtual() {
            continue;
        }
        let from_cpn = e.from.0.cpn().to_string();
        let from_cpv = format!("{}-{}", e.from.0.cpn(), e.from.1);
        let annotation = match e.via_use_flag {
            Some(f) => format!("{}[{}]", from_cpv, f.as_str()),
            None => from_cpv,
        };
        let to_cpn = e.to.0.cpn().to_string();
        adj.entry(from_cpn).or_default().push((to_cpn, annotation));
    }
    adj
}

/// Strip operators and version suffix from a root atom to get "cat/pkg"
fn parse_root_cpns(root_atoms: &[String]) -> HashSet<String> {
    root_atoms
        .iter()
        .map(|r| {
            let base = r.trim_start_matches(['>', '<', '=', '~', '!']);
            if let Some(slash) = base.find('/') {
                let after_slash = &base[slash + 1..];
                if let Some(rel) = after_slash.rfind('-').and_then(|i| {
                    after_slash[i + 1..]
                        .chars()
                        .next()
                        .filter(char::is_ascii_digit)
                        .map(|_| i)
                }) {
                    return format!("{}/{}", &base[..slash], &after_slash[..rel]);
                }
            }
            base.to_string()
        })
        .collect()
}

/// `root_labels` names what the user actually asked for — an explicit atom by
/// its own text, a set by `@name` — so a `@world` run attributes the change to
/// the set rather than reprinting every atom the set expanded to
fn build_comments(
    req: &UseFlagRequirement,
    root_labels: &[String],
    root_cpns: &HashSet<String>,
    adj: &Adjacency,
) -> Vec<String> {
    let target_key = req.package.cpn().to_string();

    // BFS: (current_CPN, path_of_annotations_so_far)
    // path grows as we walk from a root toward the target.
    let mut queue: VecDeque<(String, Vec<String>)> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();

    // Seed with edges whose source is exactly a root CPN.
    for (from_cpn, neighbors) in adj {
        if root_cpns.contains(from_cpn) {
            for (to_cpn, annotation) in neighbors {
                queue.push_back((to_cpn.clone(), vec![annotation.clone()]));
            }
            visited.insert(from_cpn.clone());
        }
    }

    let mut found_path: Option<Vec<String>> = None;
    'bfs: while let Some((current, path)) = queue.pop_front() {
        if current == target_key {
            found_path = Some(path);
            break 'bfs;
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        if let Some(neighbors) = adj.get(&current) {
            for (to_cpn, annotation) in neighbors {
                if !visited.contains(to_cpn) {
                    let mut new_path = path.clone();
                    new_path.push(annotation.clone());
                    queue.push_back((to_cpn.clone(), new_path));
                }
            }
        }
    }

    let mut comments = Vec::new();
    // A requested atom that names the flag itself is the requirer, whatever else reaches the
    // package; it is credited as `<atom> (argument)`, which no package name can be.
    let arguments: Vec<&String> = req
        .required_by
        .iter()
        .filter(|r| r.ends_with(" (argument)"))
        .collect();
    if !arguments.is_empty() {
        for credit in arguments {
            comments.push(format!("# required by {credit}"));
        }
    } else if let Some(path) = found_path {
        // Show chain from deepest (closest to target) back to root.
        for hop in path.iter().rev() {
            comments.push(format!("# required by {hop}"));
        }
        let roots = root_labels.join(", ");
        comments.push(format!("# required by {roots} (argument)"));
    } else if !req.required_by.is_empty() {
        // Fallback: solver-level immediate requirers.
        for r in &req.required_by {
            comments.push(format!("# required by {r}"));
        }
    } else {
        let list = root_labels.join(", ");
        comments.push(format!("# required by {list} (argument)"));
    }
    comments
}

/// Write `entries` under `package_use_path` (`/etc/portage/package.use`),
/// creating/updating files and inserting a block comment pointing to the
/// requesting version
///
/// `package.use` is legitimately either a directory of per-package files
/// (portage(5)'s usual layout — one file per [`PackageUseEntry::filename`])
/// or a single flat file. When it already exists as a plain file, merge
/// every entry's lines into that one file instead of `create_dir_all`-ing
/// over it (which would just fail).
pub fn write(entries: &[PackageUseEntry], package_use_path: &Utf8Path) -> anyhow::Result<()> {
    use anyhow::Context as _;

    if package_use_path.is_file() {
        let existing = std::fs::read_to_string(package_use_path)
            .with_context(|| format!("failed to read {package_use_path}"))?;
        let all_lines: Vec<PackageUseLine> = entries
            .iter()
            .flat_map(|e| e.lines.iter().cloned())
            .collect();
        let new_content = merge_content(&existing, &all_lines);
        std::fs::write(package_use_path, &new_content)
            .with_context(|| format!("failed to write {package_use_path}"))?;
        tracing::info!("Written: {package_use_path}");
        return Ok(());
    }

    std::fs::create_dir_all(package_use_path)
        .with_context(|| format!("failed to create {package_use_path}"))?;

    for entry in entries {
        let path = package_use_path.join(&entry.filename);
        let existing = if path.exists() {
            std::fs::read_to_string(&path).with_context(|| format!("failed to read {path}"))?
        } else {
            String::new()
        };

        // Build the new content: keep existing lines, append new atoms that
        // aren't already present, update atoms whose flags have changed.
        let new_content = merge_content(&existing, &entry.lines);
        std::fs::write(&path, &new_content).with_context(|| format!("failed to write {path}"))?;
        tracing::info!("Written: {path}");
    }
    Ok(())
}

/// Merge new lines into existing file content
///
/// Atoms already present in the file are updated in-place (flags and comments
/// both replaced); new ones are appended.  Existing lines unrelated to the
/// new entries are preserved.
fn merge_content(existing: &str, lines: &[PackageUseLine]) -> String {
    let mut output: Vec<String> = existing.lines().map(|l| l.to_string()).collect();

    // Remove trailing blank lines so we append cleanly.
    while output
        .last()
        .map(|l: &String| l.trim().is_empty())
        .unwrap_or(false)
    {
        output.pop();
    }

    for line in lines {
        // Check if a line for this atom already exists.
        let existing_pos = output.iter().position(|l| {
            let tok: Vec<&str> = l.split_whitespace().collect();
            tok.first() == Some(&line.atom.as_str())
        });

        let new_line = format!("{} {}", line.atom, line.flags.join(" "));

        if let Some(pos) = existing_pos {
            // Scan backwards to find the start of the comment block above
            // this atom line, so we can replace it along with the atom.
            let mut comment_start = pos;
            while comment_start > 0 && output[comment_start - 1].trim_start().starts_with('#') {
                comment_start -= 1;
            }
            let new_block: Vec<String> = line
                .comments
                .iter()
                .cloned()
                .chain(std::iter::once(new_line))
                .collect();
            output.splice(comment_start..=pos, new_block);
        } else {
            // Append with comment header.
            if !output.is_empty() {
                output.push(String::new());
            }
            for comment in &line.comments {
                output.push(comment.clone());
            }
            output.push(new_line);
        }
    }

    let mut result = output.join("\n");
    result.push('\n');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpn(s: &str) -> Cpn {
        Cpn::parse(s).unwrap()
    }

    fn line(atom: &str) -> PackageUseLine {
        PackageUseLine {
            comments: Vec::new(),
            atom: atom.to_string(),
            flags: vec!["X".to_string()],
        }
    }

    fn requirement(package: &str, required_by: &[&str]) -> UseFlagRequirement {
        UseFlagRequirement {
            package: portage_atom_pubgrub::PortagePackage::unslotted(cpn(package)),
            version: Version::parse("1").unwrap(),
            upgrade_to: None,
            required_enabled: Vec::new(),
            required_disabled: Vec::new(),
            required_by: required_by.iter().map(|r| r.to_string()).collect(),
            demands: Vec::new(),
        }
    }

    /// `dev-lang/rust` requires `net-misc/curl[http2]`; curl's IUSE carries
    /// `+http2`, so no package.use change is needed (emerge prints none).
    #[test]
    fn a_flag_enabled_by_an_iuse_default_needs_no_package_use_line() {
        let mut req = requirement("net-misc/curl", &["dev-lang/rust"]);
        req.required_enabled = vec![Interned::intern("http2")];
        let empty = UseLayer::empty();
        let build = |defaults: HashMap<Interned<DefaultInterner>, IUseDefault>| {
            build_entries(
                std::slice::from_ref(&req),
                &[],
                &[],
                &[],
                &empty,
                &empty,
                &[],
                &[],
                &empty,
                &move |_| defaults.clone(),
                &[],
                &[],
            )
        };
        assert_eq!(build(HashMap::new()).len(), 1, "no default: change needed");
        let plus_http2 = HashMap::from([(Interned::intern("http2"), IUseDefault::Enabled)]);
        assert!(build(plus_http2).is_empty(), "+http2 default satisfies it");
    }

    #[test]
    fn a_requested_atom_is_credited_only_when_it_named_the_flag_itself() {
        let roots: HashSet<String> = ["spike-test/needs".to_string()].into();
        let labels = ["spike-test/needs".to_string()];
        let mut adj = Adjacency::new();
        adj.insert(
            "spike-test/needs".to_string(),
            vec![(
                "spike-test/flagged".to_string(),
                "spike-test/needs-1".to_string(),
            )],
        );
        // A dependency of `needs` requires the flag; the parent's name equals the atom typed.
        let by_dependency = requirement("spike-test/flagged", &["spike-test/needs"]);
        assert_eq!(
            build_comments(&by_dependency, &labels, &roots, &adj),
            [
                "# required by spike-test/needs-1",
                "# required by spike-test/needs (argument)"
            ]
        );
        let by_argument = requirement("sys-libs/zlib", &["sys-libs/zlib[minizip] (argument)"]);
        assert_eq!(
            build_comments(&by_argument, &labels, &roots, &adj),
            ["# required by sys-libs/zlib[minizip] (argument)"]
        );
    }

    #[test]
    fn assign_filenames_prefers_the_bare_name_when_unambiguous() {
        let mut by_cpn = HashMap::new();
        by_cpn.insert(cpn("media-libs/mesa"), vec![line(">=media-libs/mesa-26")]);
        by_cpn.insert(cpn("dev-python/mako"), vec![line(">=dev-python/mako-1")]);

        let mut entries = assign_filenames(by_cpn);
        entries.sort_by(|a, b| a.filename.cmp(&b.filename));
        let filenames: Vec<&str> = entries.iter().map(|e| e.filename.as_str()).collect();
        assert_eq!(filenames, ["mako", "mesa"]);
    }

    #[test]
    fn assign_filenames_falls_back_to_cat_name_only_for_a_colliding_bare_name() {
        let mut by_cpn = HashMap::new();
        by_cpn.insert(cpn("x11-libs/foo"), vec![line(">=x11-libs/foo-1")]);
        by_cpn.insert(cpn("dev-libs/foo"), vec![line(">=dev-libs/foo-1")]);
        by_cpn.insert(cpn("media-libs/mesa"), vec![line(">=media-libs/mesa-26")]);

        let mut entries = assign_filenames(by_cpn);
        entries.sort_by(|a, b| a.filename.cmp(&b.filename));
        let filenames: Vec<&str> = entries.iter().map(|e| e.filename.as_str()).collect();
        // The colliding `foo`s are disambiguated; the unique `mesa` stays bare.
        assert_eq!(filenames, ["dev-libs-foo", "mesa", "x11-libs-foo"]);
    }

    #[test]
    fn write_creates_a_directory_when_the_target_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let package_use = camino::Utf8Path::from_path(dir.path())
            .unwrap()
            .join("package.use");
        let entries = vec![PackageUseEntry {
            filename: "mesa".to_string(),
            lines: vec![line(">=media-libs/mesa-26")],
        }];
        write(&entries, &package_use).unwrap();
        assert!(package_use.is_dir());
        let content = std::fs::read_to_string(package_use.join("mesa")).unwrap();
        assert!(content.contains(">=media-libs/mesa-26 X"));
    }

    #[test]
    fn write_appends_into_an_existing_flat_file_instead_of_replacing_it_with_a_dir() {
        let dir = tempfile::tempdir().unwrap();
        let package_use = camino::Utf8Path::from_path(dir.path())
            .unwrap()
            .join("package.use");
        std::fs::write(&package_use, "app-misc/existing -doc\n").unwrap();

        let entries = vec![PackageUseEntry {
            filename: "mesa".to_string(),
            lines: vec![line(">=media-libs/mesa-26")],
        }];
        write(&entries, &package_use).unwrap();

        assert!(
            package_use.is_file(),
            "must stay a flat file, not become a dir"
        );
        let content = std::fs::read_to_string(&package_use).unwrap();
        assert!(content.contains("app-misc/existing -doc"));
        assert!(content.contains(">=media-libs/mesa-26 X"));
    }
}
