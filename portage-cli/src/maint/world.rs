use std::collections::HashSet;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Dep;
use portage_vdb::Vdb;

use super::sets::KnownSets;
use crate::query::which::dep_matches_cpv;
use crate::style::{C_PKG, C_WARN};
use crate::util::write_atomic;

const DEFAULT_WORLD: &str = "/var/lib/portage/world";

/// The repository plus every resolved policy input, owned, so each world atom
/// can be asked the two questions the VDB alone cannot answer: does any ebuild
/// still match it, and is any of them visible here?
///
/// `emaint --check world` reports both (`has no available ebuilds` /
/// `has no visible ebuilds`); checking only the VDB silently passes a world
/// full of entries that can never be rebuilt.
pub struct TreeView {
    data: portage_resolve::repo::RepoData,
    accept_keywords: portage_resolve::repo::AcceptKeywords,
    accept_licenses: portage_resolve::repo::AcceptLicenses,
    accept_properties: portage_resolve::repo::AcceptProperties,
    accept_restrict: portage_resolve::repo::AcceptRestrict,
    package_mask: Vec<Dep>,
    package_unmask: Vec<Dep>,
    pre_env: portage_atom_pubgrub::UseLayer,
    env_use: portage_atom_pubgrub::UseLayer,
    package_use: Vec<(Dep, Vec<portage_atom_pubgrub::UseOverride>)>,
    profile_package_use: Vec<(Dep, Vec<portage_atom_pubgrub::UseOverride>)>,
    force_mask: portage_resolve::force_mask::ForceMask,
    multi_repo: bool,
}

impl TreeView {
    /// Load the main repo (plus overlays when `multi_repo`) and the config that
    /// decides visibility.
    pub async fn load(
        repo_path: &Utf8Path,
        roots: &portage_resolve::Roots,
        arch: &gentoo_core::Arch,
        multi_repo: bool,
    ) -> Result<Self> {
        let repo = crate::repo_open::open(repo_path.as_std_path())
            .map_err(|e| anyhow::anyhow!("failed to open repo at {repo_path}: {e}"))?;
        let set = crate::repo_open::repo_set_from_conf(repo, roots, multi_repo);
        let (data, env) = tokio::join!(
            portage_resolve::repo::load_repos(&set),
            portage_resolve::use_env::build_use_env(set.main(), roots.config(), None, None),
        );
        let env = env?;
        Ok(Self {
            data,
            accept_keywords: portage_resolve::repo::AcceptKeywords::new(
                arch,
                &env.accept_keywords,
                env.package_accept_keywords,
            ),
            accept_licenses: portage_resolve::repo::AcceptLicenses::new(
                env.accept_license,
                env.package_license,
            ),
            accept_properties: portage_resolve::repo::AcceptProperties::new(
                env.accept_properties,
                env.package_properties,
            ),
            accept_restrict: portage_resolve::repo::AcceptRestrict::new(
                env.accept_restrict,
                env.package_restrict,
            ),
            package_mask: env.package_mask,
            package_unmask: env.package_unmask,
            pre_env: env.pre_env,
            env_use: env.env_use,
            package_use: env.package_use,
            profile_package_use: env.profile_package_use,
            force_mask: env.force_mask,
            multi_repo: set.is_multi(),
        })
    }

    fn policy(&self) -> portage_resolve::repo::ResolvePolicy<'_> {
        portage_resolve::repo::ResolvePolicy {
            accept_keywords: &self.accept_keywords,
            package_mask: &self.package_mask,
            package_unmask: &self.package_unmask,
            accept_licenses: &self.accept_licenses,
            accept_properties: &self.accept_properties,
            accept_restrict: &self.accept_restrict,
            pre_env: &self.pre_env,
            env_use: &self.env_use,
            package_use: &self.package_use,
            profile_package_use: &self.profile_package_use,
            force_mask: &self.force_mask,
        }
    }

    /// `None` when the atom has an acceptable candidate; otherwise the problem,
    /// worded as the depgraph words it for an unsatisfiable root target.
    fn problem(&self, dep: &Dep) -> Option<String> {
        let vs = match &dep.version {
            Some(v) => portage_atom_pubgrub::PortageVersionSet::from_operator(
                dep.op.unwrap_or(portage_atom::Operator::GreaterOrEqual),
                dep.glob,
                v.clone(),
            ),
            None => portage_atom_pubgrub::PortageVersionSet::any(),
        };
        let policy = self.policy();
        if portage_resolve::repo::target_package(&self.data, dep, &policy)
            .slot()
            .is_some()
        {
            return None;
        }
        let reasons = portage_resolve::repo::filter_reasons_for_atom(&self.data, dep, &vs, &policy);
        if reasons.is_empty() {
            return Some(format!(
                "no ebuilds in ::{}{}",
                self.data.repo_name,
                if self.multi_repo { " or overlays" } else { "" }
            ));
        }
        Some(format!(
            "all ebuilds masked ({})",
            reasons
                .iter()
                .map(|c| format!(
                    "{}-{} {}",
                    c.cpv.cpn,
                    c.cpv.version,
                    portage_resolve::repo::filter_reason_text(&c.reasons)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

pub fn run(vdb: &Vdb, fix: bool, root: Option<&Utf8Path>, tree: Option<&TreeView>) -> Result<()> {
    let known_sets = KnownSets::load(root);
    let installed: Vec<_> = vdb.packages().into_iter().collect();

    let mut counts = Counts::default();
    check_world_file(
        &world_path(root),
        &installed,
        &known_sets,
        tree,
        fix,
        &mut counts,
    )?;
    check_world_sets_file(
        &world_sets_path(root),
        &known_sets,
        fix,
        &mut counts.orphaned,
    )?;

    let removable = counts.orphaned + counts.invalid;
    if removable == 0 && counts.unbuildable == 0 {
        anstream::println!("World files are consistent.");
        return Ok(());
    }
    if counts.unbuildable > 0 {
        eprintln!(
            "\n{} installed entr{} kept: still usable, but nothing in the tree can rebuild {}.",
            counts.unbuildable,
            if counts.unbuildable == 1 { "y" } else { "ies" },
            if counts.unbuildable == 1 {
                "it"
            } else {
                "them"
            },
        );
    }
    if removable > 0 && !fix {
        eprintln!(
            "{removable} entr{} can be removed. Run with --fix.",
            if removable == 1 { "y" } else { "ies" },
        );
    }

    Ok(())
}

/// World-file problems, split by what `--fix` may do about each.
#[derive(Default)]
struct Counts {
    /// Not installed, or naming a set that no longer exists — removable.
    orphaned: usize,
    /// Unparseable as an atom — removable.
    invalid: usize,
    /// Installed and usable, but unbuildable from the current tree — kept.
    unbuildable: usize,
}

fn check_world_file(
    path: &Utf8Path,
    installed: &[portage_vdb::InstalledPackage],
    known_sets: &KnownSets,
    tree: Option<&TreeView>,
    fix: bool,
    counts: &mut Counts,
) -> Result<()> {
    let content = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;

    let mut orphaned: Vec<String> = Vec::new();
    let mut invalid: Vec<String> = Vec::new();
    // Entries that are installed and so still usable, but whose ebuilds are
    // gone or no longer visible: reported, never removed. Dropping one from
    // world would leave an installed package nothing selects, i.e. depclean
    // bait — `emaint --fix world` does remove them, this deliberately does not.
    let mut unbuildable: Vec<(String, String)> = Vec::new();
    let mut kept: Vec<&str> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            kept.push(line);
            continue;
        }
        if let Some(set_name) = trimmed.strip_prefix('@') {
            if known_sets.contains(set_name) {
                kept.push(line);
            } else {
                orphaned.push(trimmed.to_owned());
            }
            continue;
        }
        let dep = match Dep::parse(trimmed) {
            Ok(d) => d,
            Err(e) => {
                crate::style::warn_line!("invalid world entry '{trimmed}': {e}");
                invalid.push(trimmed.to_owned());
                continue;
            }
        };
        if installed.iter().any(|pkg| dep_matches_cpv(&dep, pkg.cpv())) {
            kept.push(line);
            if let Some(problem) = tree.and_then(|t| t.problem(&dep)) {
                unbuildable.push((trimmed.to_owned(), problem));
            }
        } else {
            orphaned.push(trimmed.to_owned());
        }
    }

    if !invalid.is_empty() || !orphaned.is_empty() || !unbuildable.is_empty() {
        anstream::println!("{path}:");
    }
    for atom in &invalid {
        anstream::println!("  {C_PKG}{atom}{C_PKG:#}: invalid atom");
    }
    for atom in &orphaned {
        anstream::println!("  {C_PKG}{atom}{C_PKG:#}: not installed / unknown set");
    }
    for (atom, problem) in &unbuildable {
        anstream::println!("  {C_PKG}{atom}{C_PKG:#}: {problem}");
    }

    counts.orphaned += orphaned.len();
    counts.invalid += invalid.len();
    counts.unbuildable += unbuildable.len();

    if fix && (!orphaned.is_empty() || !invalid.is_empty()) {
        let new_content = kept.join("\n") + "\n";
        write_atomic(path, new_content).with_context(|| format!("writing {path}"))?;
        println!("Fixed {path}.");
    }

    Ok(())
}

fn check_world_sets_file(
    path: &Utf8Path,
    known_sets: &KnownSets,
    fix: bool,
    orphaned_count: &mut usize,
) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;

    let mut orphaned: Vec<String> = Vec::new();
    let mut kept: Vec<&str> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            kept.push(line);
            continue;
        }
        let set_name = trimmed.strip_prefix('@').unwrap_or(trimmed);
        if known_sets.contains(set_name) {
            kept.push(line);
        } else {
            orphaned.push(trimmed.to_owned());
        }
    }

    if !orphaned.is_empty() {
        use std::io::Write;
        let mut out = anstream::stdout();
        for atom in &orphaned {
            let _ = writeln!(out, "{C_WARN}!!!{C_WARN:#} {path}: '{atom}': unknown set");
        }
        let _ = out.flush();
    }
    *orphaned_count += orphaned.len();

    if fix && !orphaned.is_empty() {
        let new_content = kept.join("\n") + "\n";
        write_atomic(path, new_content).with_context(|| format!("writing {path}"))?;
        println!("Fixed {path}.");
    }

    Ok(())
}

/// Add `atoms` to the world file, matching real emerge's `_world_atom`
/// world-selection behaviour after a successful merge of explicitly-named
/// atoms (skipped entirely under `--oneshot`/`--buildpkgonly`/
/// `--fetchonly`/`--onlydeps`/`--pretend` — see the caller). An atom whose
/// `Cpn` already has an entry (in any version-qualified form) is replaced
/// in place rather than duplicated; a genuinely new one is appended.
/// Best-effort: a failure here is reported but never unwinds a merge that
/// already completed on disk.
pub fn add_atoms(root: Option<&Utf8Path>, atoms: &[Dep]) {
    if atoms.is_empty() {
        return;
    }
    let path = world_path(root);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(str::to_owned).collect();

    for atom in atoms {
        let rendered = atom.to_string();
        let already = lines.iter().position(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with('#')
                && !t.starts_with('@')
                && Dep::parse(t).is_ok_and(|d| d.cpn == atom.cpn)
        });
        match already {
            Some(idx) if lines[idx].trim() == rendered => {}
            Some(idx) => lines[idx] = rendered,
            None => lines.push(rendered),
        }
    }

    let new_content = if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    };
    if let Err(e) = write_atomic(&path, new_content) {
        crate::style::warn_line!("could not update {path}: {e:#}");
    }
}

/// Remove `tokens` (plain atoms or `@set` names) from the world file —
/// `-W`/`--deselect`. An atom removes any existing world line whose `Cpn`
/// matches (any version-qualified form, same granularity `add_atoms` uses
/// for replacement); a `@name` token removes the matching `@name` set
/// entry. Tokens matching nothing are silently no-ops (matches real
/// emerge: deselecting something not in world isn't an error). Returns the
/// number of world-file lines actually removed.
pub fn remove_atoms(root: Option<&Utf8Path>, tokens: &[String]) -> Result<usize> {
    let mut set_names: HashSet<&str> = HashSet::new();
    let mut cpns: HashSet<portage_atom::Cpn> = HashSet::new();
    for tok in tokens {
        match tok.strip_prefix('@') {
            Some(name) => {
                set_names.insert(name);
            }
            None => {
                let dep = Dep::parse(tok).with_context(|| format!("invalid atom '{tok}'"))?;
                cpns.insert(dep.cpn);
            }
        }
    }

    let path = world_path(root);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut removed = 0usize;
    let kept: Vec<&str> = existing
        .lines()
        .filter(|line| {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                return true;
            }
            let drop = match t.strip_prefix('@') {
                Some(name) => set_names.contains(name),
                None => Dep::parse(t).is_ok_and(|d| cpns.contains(&d.cpn)),
            };
            if drop {
                removed += 1;
            }
            !drop
        })
        .collect();

    let new_content = if kept.is_empty() {
        String::new()
    } else {
        kept.join("\n") + "\n"
    };
    write_atomic(&path, new_content).with_context(|| format!("writing {path}"))?;
    Ok(removed)
}

fn world_path(root: Option<&Utf8Path>) -> Utf8PathBuf {
    match root {
        Some(r) => r.join("var/lib/portage/world"),
        None => Utf8PathBuf::from(DEFAULT_WORLD),
    }
}

/// `pub(crate)`: also reused by `em --info`'s `Installed sets:` line.
pub(crate) fn world_sets_path(root: Option<&Utf8Path>) -> Utf8PathBuf {
    match root {
        Some(r) => r.join("var/lib/portage/world_sets"),
        None => Utf8PathBuf::from("/var/lib/portage/world_sets"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_world(root: &Utf8Path) -> String {
        std::fs::read_to_string(world_path(Some(root))).unwrap_or_default()
    }

    #[test]
    fn appends_a_new_atom_to_an_empty_world() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        add_atoms(Some(&root), &[Dep::parse("app-editors/nano").unwrap()]);

        assert_eq!(read_world(&root), "app-editors/nano\n");
    }

    #[test]
    fn replaces_an_existing_entry_for_the_same_cpn_instead_of_duplicating() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("var/lib/portage")).unwrap();
        std::fs::write(world_path(Some(&root)), "=app-editors/nano-8.0\n").unwrap();

        add_atoms(Some(&root), &[Dep::parse("app-editors/nano").unwrap()]);

        assert_eq!(read_world(&root), "app-editors/nano\n");
    }

    #[test]
    fn leaves_comments_and_set_refs_and_other_atoms_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("var/lib/portage")).unwrap();
        std::fs::write(
            world_path(Some(&root)),
            "# a comment\n@some-set\napp-shells/bash\n",
        )
        .unwrap();

        add_atoms(Some(&root), &[Dep::parse("app-editors/nano").unwrap()]);

        assert_eq!(
            read_world(&root),
            "# a comment\n@some-set\napp-shells/bash\napp-editors/nano\n"
        );
    }

    #[test]
    fn is_a_no_op_when_the_exact_atom_is_already_present() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("var/lib/portage")).unwrap();
        std::fs::write(world_path(Some(&root)), "app-editors/nano\n").unwrap();

        add_atoms(Some(&root), &[Dep::parse("app-editors/nano").unwrap()]);

        assert_eq!(read_world(&root), "app-editors/nano\n");
    }

    #[test]
    fn deselect_removes_a_versioned_or_bare_atom_by_cpn() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("var/lib/portage")).unwrap();
        std::fs::write(
            world_path(Some(&root)),
            "=app-editors/nano-8.0\napp-shells/bash\n",
        )
        .unwrap();

        let removed = remove_atoms(Some(&root), &["app-editors/nano".to_owned()]).unwrap();

        assert_eq!(removed, 1);
        assert_eq!(read_world(&root), "app-shells/bash\n");
    }

    #[test]
    fn deselect_removes_a_set_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("var/lib/portage")).unwrap();
        std::fs::write(world_path(Some(&root)), "@some-set\napp-shells/bash\n").unwrap();

        let removed = remove_atoms(Some(&root), &["@some-set".to_owned()]).unwrap();

        assert_eq!(removed, 1);
        assert_eq!(read_world(&root), "app-shells/bash\n");
    }

    #[test]
    fn deselect_is_a_no_op_for_an_atom_not_in_world() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("var/lib/portage")).unwrap();
        std::fs::write(world_path(Some(&root)), "app-shells/bash\n").unwrap();

        let removed = remove_atoms(Some(&root), &["app-editors/nano".to_owned()]).unwrap();

        assert_eq!(removed, 0);
        assert_eq!(read_world(&root), "app-shells/bash\n");
    }

    #[test]
    fn deselect_leaves_comments_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("var/lib/portage")).unwrap();
        std::fs::write(world_path(Some(&root)), "# a comment\napp-editors/nano\n").unwrap();

        remove_atoms(Some(&root), &["app-editors/nano".to_owned()]).unwrap();

        assert_eq!(read_world(&root), "# a comment\n");
    }

    #[test]
    fn deselect_rejects_an_invalid_atom() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        assert!(remove_atoms(Some(&root), &["not a valid atom!!!".to_owned()]).is_err());
    }
}
