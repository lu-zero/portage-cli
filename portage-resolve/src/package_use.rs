//! `package.use` entry synthesis: turning cross-package `[flag]` USE-dep
//! requirements into concrete `package.use` lines, and the co-solve fixpoint
//! that auto-applies them (emerge's autounmask-preview behaviour).

use std::collections::{HashMap, HashSet, VecDeque};

use camino::Utf8Path;
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    DepEdge, UseFlagRequirement, UseFlagState, UseLayer, UseOverride, resolve_effective_use,
};

/// Entries to write into `/etc/portage/package.use`.
pub struct PackageUseEntry {
    /// Filename inside `package.use/`: the bare package name (e.g.
    /// `pygments`), or `category-package` (e.g. `dev-python-pygments`) when
    /// the bare name is ambiguous within this batch — see
    /// `assign_filenames` (private, in this module).
    pub filename: String,
    /// Lines to add/update in that file.
    pub lines: Vec<PackageUseLine>,
}

/// One `package.use` line: an atom, the flags it forces, and the explanatory
/// comments placed above it.
#[derive(Clone, PartialEq, Eq)]
pub struct PackageUseLine {
    /// Comment lines explaining the requirement, e.g. `# required by firefox`.
    pub comments: Vec<String>,
    /// The atom spec, e.g. `>=dev-python/pygments-2.19.2`.
    pub atom: String,
    /// Flags to enable (no prefix) and disable (`-` prefix).
    pub flags: Vec<String>,
}

/// Build `package.use` entries for all non-trivial USE flag requirements.
pub fn build_entries(
    flag_reqs: &[UseFlagRequirement],
    root_atoms: &[String],
    root_labels: &[String],
    edges: &[DepEdge],
    pre_env: &UseLayer,
    env_use: &UseLayer,
    package_use: &[(Dep, Vec<UseOverride>)],
) -> Vec<PackageUseEntry> {
    // Pre-compute once for all requirements.
    let adj = build_adjacency(edges);
    let root_cpns = parse_root_cpns(root_atoms);

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

        // A flag belongs in package.use only if the global config does not
        // already set it the way the requirement needs.  A flag already enabled
        // (e.g. a PYTHON_TARGETS member in the profile) just triggers a rebuild,
        // shown via the `*` USE marker — it is not an autounmask change.
        let cpv = Cpv::new(cpn, ver.clone());
        let eff = resolve_effective_use(
            &HashMap::new(),
            pre_env,
            &cpv,
            req.package.slot(),
            package_use,
            env_use,
        );
        let mut flags: Vec<String> = Vec::new();
        for f in &req.required_enabled {
            if eff.get_opt(*f) != Some(UseFlagState::Enabled) {
                flags.push(f.as_str().to_string());
            }
        }
        for f in &req.required_disabled {
            if eff.get_opt(*f) != Some(UseFlagState::Disabled) {
                flags.push(format!("-{}", f.as_str()));
            }
        }
        if flags.is_empty() {
            continue;
        }

        let comments = build_comments(req, root_labels, &root_cpns, &adj);

        // `flag_reqs` merges the co-solve's applied requirements with the
        // solver's own, so the same requirement can arrive twice.
        let line = PackageUseLine {
            comments,
            atom,
            flags,
        };
        let lines = by_cpn.entry(cpn).or_default();
        if !lines.contains(&line) {
            lines.push(line);
        }
    }

    assign_filenames(by_cpn)
}

/// Turn per-package line groups into [`PackageUseEntry`]s, choosing each
/// one's filename: the bare package name (e.g. `mesa`) when unambiguous,
/// falling back to `category-package` only for names that collide across
/// categories in this batch (e.g. both `x11-libs/foo` and `dev-libs/foo`
/// need an entry) — real portage users keep bare-name package.use files day
/// to day, and cat-name for everything is only needed to disambiguate.
/// Sorted by filename so the report/written output is reproducible across
/// runs; lines within a file keep their caller-given order.
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

/// Result of [`cosolve_use_deps`]: the augmented `package.use`, the
/// requirements that drove at least one applied flag, and the converged solve
/// (if the fixpoint ended on a solve of the returned `package.use`).
pub type CosolveOutcome<T> = (
    Vec<(Dep, Vec<UseOverride>)>,
    Vec<UseFlagRequirement>,
    Option<T>,
);

/// Auto-apply cross-package `[flag]` USE-deps to a fixpoint (emerge's
/// autounmask-preview dependency calculation).
///
/// Starting from `package_use`, repeatedly: solve (`solve` returns an opaque
/// solve outcome `T`, or `None` if the solve failed), read the in-plan USE-flag
/// requirements from it via `reqs_of`, force every demanded flag that is real
/// IUSE of its target via a synthetic `cpn flags` entry, and re-solve — until no
/// new flag is added. Flags the caller's *initial* `package_use` sets either
/// way are pins and are never forced (emerge refuses to override explicit
/// configuration; the demand is left to the advisory).
///
/// Returns the augmented `package_use`, the requirements that drove at least
/// one applied flag (for the mandatory "USE changes are necessary" report —
/// the final solve no longer demands them, so they must be carried out), and,
/// **when the fixpoint converged on a solve of that exact `package_use`**,
/// that final outcome — so the caller can reuse it instead of solving once
/// more. The outcome is `None` if a solve failed or the iteration bound was
/// hit (the returned `package_use` then has additions that were not re-solved,
/// so the caller must solve again).
///
/// `applied` (a flag forced once) is never re-forced, so a `[bar]` vs `[-bar]`
/// contradiction resolves to first-wins + advisory for the loser rather than
/// oscillating; the bound is a backstop.
pub fn cosolve_use_deps<T, S, R>(
    mut package_use: Vec<(Dep, Vec<UseOverride>)>,
    data: &crate::repo::RepoData,
    solve: S,
    reqs_of: R,
) -> CosolveOutcome<T>
where
    S: Fn(&[(Dep, Vec<UseOverride>)]) -> Option<T>,
    R: Fn(&T) -> Vec<UseFlagRequirement>,
{
    use portage_atom::Cpn;
    use portage_atom::interner::{DefaultInterner, Interned};

    // Flags pinned by the caller's configuration (profile + user package.use):
    // never forced, matching emerge's refusal to override explicit config.
    let pinned: HashSet<(Cpn, Interned<DefaultInterner>)> = package_use
        .iter()
        .flat_map(|(dep, overrides)| overrides.iter().map(|o| (dep.cpn, o.flag)))
        .collect();

    let mut applied: HashMap<(Cpn, Interned<DefaultInterner>), bool> = HashMap::new();
    let mut applied_reqs: Vec<UseFlagRequirement> = Vec::new();
    for _ in 0..8 {
        let Some(solved) = solve(&package_use) else {
            return (package_use, applied_reqs, None); // solve failed — caller must re-solve / fall back
        };
        let mut new_by_cpn: HashMap<Cpn, Vec<UseOverride>> = HashMap::new();
        for req in reqs_of(&solved) {
            let mut contributed = false;
            for (cpn, flag, enable) in req_targets(&req, data) {
                if pinned.contains(&(cpn, flag)) || applied.contains_key(&(cpn, flag)) {
                    continue; // pinned, or already forced (same or opposite)
                }
                applied.insert((cpn, flag), enable);
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
        if new_by_cpn.is_empty() {
            // Fixpoint: `solved` is a solve of the current `package_use`.
            return (package_use, applied_reqs, Some(solved));
        }
        for (cpn, flags) in new_by_cpn {
            if let Ok(dep) = Dep::parse(&cpn.to_string()) {
                package_use.push((dep, flags));
            }
        }
    }
    (package_use, applied_reqs, None) // bound hit — additions not yet solved
}

/// The `(target cpn, flag, enable)` triples one requirement demands, filtered
/// to flags that are **real IUSE** of the target's selected/upgrade version
/// (a `[bar]` on a target without `bar` cannot be applied — CC7). Used by the
/// co-solve to auto-apply USE-deps via synthetic `package.use` and re-solve,
/// instead of only suggesting them.
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

/// Adjacency map: CPN → Vec<(to_CPN, annotation)>.
/// annotation = "from-cpv\[flag\]" when gated, "from-cpv" otherwise.
type Adjacency = HashMap<String, Vec<(String, String)>>;

fn build_adjacency(edges: &[DepEdge]) -> Adjacency {
    let mut adj: Adjacency = HashMap::new();
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

/// Strip operators and version suffix from a root atom to get "cat/pkg".
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
/// the set rather than reprinting every atom the set expanded to.
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
    if let Some(path) = found_path {
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
/// requesting version.
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

/// Merge new lines into existing file content.
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
