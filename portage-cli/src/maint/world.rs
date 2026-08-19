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
    defaults: portage_atom_pubgrub::UseLayer,
    conf: portage_atom_pubgrub::UseLayer,
    env_use: portage_atom_pubgrub::UseLayer,
    package_use: Vec<(Dep, Vec<portage_atom_pubgrub::UseOverride>)>,
    profile_package_use: Vec<portage_atom_pubgrub::ProfileUseNode>,
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
            defaults: env.defaults,
            conf: env.conf,
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
            defaults: &self.defaults,
            conf: &self.conf,
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
/// atoms (skipped under `--oneshot`/`--buildpkgonly`/`--fetchonly`/
/// `--onlydeps`/`--pretend` — see the caller).
///
/// An atom whose `Cpn` already has an entry is replaced in place rather
/// than duplicated; a genuinely new one is appended. Best-effort: a
/// failure here is reported but never unwinds a merge already on disk.
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

/// Add `@name` set references to `world_sets` — the other half of real
/// emerge's world-recording, sibling to [`add_atoms`]'s `world` file.
/// Writes the literal `@name` reference, never the set's expanded members
/// (those stay off `world` entirely — `emerge.rs::select_world_atoms`
/// already filters `Set`-origin atoms out for this reason).
///
/// A name already present is left untouched. Best-effort, like
/// `add_atoms`: reported but never unwinds a merge already on disk.
pub fn add_set_refs(root: Option<&Utf8Path>, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let path = world_sets_path(root);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(str::to_owned).collect();

    for name in names {
        let rendered = format!("@{name}");
        if !lines.iter().any(|l| l.trim() == rendered) {
            lines.push(rendered);
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

/// Remove `tokens` (plain atoms or `@set` names) from the world files —
/// `-W`/`--deselect`. Mirrors real emerge's `action_deselect`: a plain-atom
/// token is matched by `Cpn` against `world` (any version-qualified form);
/// a `@name` token is matched by exact name against `world_sets` — the two
/// files and token kinds are handled independently, never cross-matched.
///
/// `world` is also checked for a stray `@name` line for backward
/// compatibility (`check_world_file` has long tolerated `@` lines living
/// directly in `world` rather than `world_sets`). Tokens matching nothing
/// are silent no-ops (matches real emerge). Returns the total lines removed.
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

    let world_removed = remove_matching_lines(&world_path(root), |t| match t.strip_prefix('@') {
        Some(name) => set_names.contains(name),
        None => Dep::parse(t).is_ok_and(|d| cpns.contains(&d.cpn)),
    })?;
    let sets_removed = if set_names.is_empty() {
        0
    } else {
        remove_matching_lines(&world_sets_path(root), |t| {
            t.strip_prefix('@')
                .is_some_and(|name| set_names.contains(name))
        })?
    };
    Ok(world_removed + sets_removed)
}

/// Drop every non-blank, non-comment line of `path` for which `should_drop`
/// is true, rewriting the file only when something actually changed — a
/// missing `world_sets` (very common: most installs never register a
/// world-candidate set) must not be conjured into existence by a `-W` call
/// that only ever touched `world`.
fn remove_matching_lines(path: &Utf8Path, should_drop: impl Fn(&str) -> bool) -> Result<usize> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut removed = 0usize;
    let kept: Vec<&str> = existing
        .lines()
        .filter(|line| {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                return true;
            }
            let drop = should_drop(t);
            if drop {
                removed += 1;
            }
            !drop
        })
        .collect();

    if removed > 0 {
        let new_content = if kept.is_empty() {
            String::new()
        } else {
            kept.join("\n") + "\n"
        };
        write_atomic(path, new_content).with_context(|| format!("writing {path}"))?;
    }
    Ok(removed)
}

fn world_path(root: Option<&Utf8Path>) -> Utf8PathBuf {
    match root {
        Some(r) => r.join("var/lib/portage/world"),
        None => Utf8PathBuf::from(DEFAULT_WORLD),
    }
}

/// Build a `SetResolver` over `config_root`'s profile (for `@system`/
/// `@profile`) and `eroot` (for `@world`/`@selected` and user sets), then
/// resolve `name` to its flat atom list. The one place that construction
/// lives for a *known* set name — `emerge.rs::expand_sets` does the same
/// lazily for arbitrary `@set` refs typed on the command line, where the
/// per-token failure handling has to differ.
///
/// `@preserved-rebuild` is special-cased the same way `expand_sets` special-
/// cases it: a VDB/preserve-libs-registry query `SetResolver` (profile/
/// config-only) has no access to, so it never needs (or can use) a profile
/// stack at all.
pub(crate) fn resolve_set(
    config_root: Option<&Utf8Path>,
    eroot: &Utf8Path,
    name: &str,
) -> Result<Vec<Dep>> {
    // VDB-aware built-ins (@preserved-rebuild, …) can't go through
    // `SetResolver` (no VDB access); route them through the shared resolver
    // first. `None` → not a VDB-aware name, fall through to the profile stack.
    if let Some(res) = super::sets::resolve_vdb_set(name, eroot) {
        return res.with_context(|| format!("failed to resolve @{name}"));
    }
    let config_root = config_root.unwrap_or(Utf8Path::new("/"));
    let profile_link = config_root.join("etc/portage/make.profile");
    let canon = std::fs::canonicalize(profile_link.as_std_path())
        .with_context(|| format!("cannot resolve {profile_link}"))?;
    let stack = portage_repo::ProfileStack::build(canon)
        .context("failed to build profile stack")?
        .with_user_profile(config_root.join("etc/portage/profile").into_std_path_buf())
        .context("failed to append site-local user profile")?;
    let resolver = portage_repo::SetResolver::new(&stack, eroot);
    resolver
        .resolve(name)
        .with_context(|| format!("failed to resolve @{name}"))
}

/// The cpns the world file at `eroot` tracks, or is about to: `@selected`
/// (every atom in `var/lib/portage/world` plus whatever its `world_sets`
/// refs expand to) union `additions` — the atoms this invocation would
/// itself record ([`add_atoms`]' input, empty when it won't record any).
///
/// Both halves of real emerge's `check_system_world`, which gates its bold
/// `PKG_MERGE_WORLD`/`PKG_NOMERGE_WORLD`/`PKG_BINARY_MERGE_WORLD` rows:
/// already in `conf.selected_sets`, *or* a `favorites` target of a
/// non-`--oneshot` run that `create_world_atom` would add. Dropping the
/// second half would render `em -p newpkg` the same as `em -1p newpkg`,
/// which real emerge does not do.
///
/// `@selected`, deliberately not `@world`: `@world` is `@selected ∪
/// @system`, and real portage colours system packages with a separate
/// `PKG_*_SYSTEM` hue `em` doesn't implement — folding it in here would bold
/// the entire system set instead.
///
/// A failed resolve (no `make.profile` under a bare `--root`, an unreadable
/// or malformed world file) contributes nothing rather than erroring: this
/// only drives styling, and an otherwise-fine `-p` must not abort over it.
pub(crate) fn selected_cpns(
    config_root: Option<&Utf8Path>,
    eroot: &Utf8Path,
    additions: &[Dep],
) -> HashSet<portage_atom::Cpn> {
    let mut out: HashSet<portage_atom::Cpn> = resolve_set(config_root, eroot, "selected")
        .map(|atoms| atoms.into_iter().map(|d| d.cpn).collect())
        .unwrap_or_default();
    out.extend(additions.iter().map(|d| d.cpn));
    out
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

    fn read_world_sets(root: &Utf8Path) -> String {
        std::fs::read_to_string(world_sets_path(Some(root))).unwrap_or_default()
    }

    #[test]
    fn add_set_refs_appends_a_new_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        add_set_refs(Some(&root), &["myset".to_owned()]);

        assert_eq!(read_world_sets(&root), "@myset\n");
    }

    #[test]
    fn add_set_refs_is_a_no_op_when_already_present() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("var/lib/portage")).unwrap();
        std::fs::write(world_sets_path(Some(&root)), "@myset\n").unwrap();

        add_set_refs(Some(&root), &["myset".to_owned()]);

        assert_eq!(read_world_sets(&root), "@myset\n");
    }

    #[test]
    fn deselect_removes_a_name_from_world_sets_not_world() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("var/lib/portage")).unwrap();
        std::fs::write(world_path(Some(&root)), "app-shells/bash\n").unwrap();
        std::fs::write(world_sets_path(Some(&root)), "@myset\n@other-set\n").unwrap();

        let removed = remove_atoms(Some(&root), &["@myset".to_owned()]).unwrap();

        assert_eq!(removed, 1);
        assert_eq!(read_world(&root), "app-shells/bash\n");
        assert_eq!(read_world_sets(&root), "@other-set\n");
    }

    #[test]
    fn deselect_with_no_set_token_never_touches_a_missing_world_sets() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("var/lib/portage")).unwrap();
        std::fs::write(world_path(Some(&root)), "app-shells/bash\n").unwrap();

        remove_atoms(Some(&root), &["app-shells/bash".to_owned()]).unwrap();

        assert!(!world_sets_path(Some(&root)).exists());
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

    /// A root `SetResolver` can resolve `@selected` in: any existing dir is a
    /// valid (empty) profile, which is all `@selected` needs — it reads the
    /// world file, not the profile.
    fn world_root(content: &str) -> (tempfile::TempDir, Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("etc/portage/make.profile")).unwrap();
        std::fs::create_dir_all(root.join("var/lib/portage")).unwrap();
        std::fs::write(world_path(Some(&root)), content).unwrap();
        (tmp, root)
    }

    fn sorted(cpns: HashSet<portage_atom::Cpn>) -> Vec<String> {
        let mut v: Vec<String> = cpns.iter().map(|c| c.to_string()).collect();
        v.sort();
        v
    }

    #[test]
    fn selected_cpns_keeps_slotted_use_dep_and_ranged_entries() {
        // Every shape `select_world_atoms` can write (it takes whatever
        // `Dep::parse` accepts from the command line) must still be
        // recognised as world membership — a slot/USE-dep/`>=` entry is not
        // a bare `cat/pkg`.
        let (_tmp, root) = world_root(
            "# a comment\n\n\
             dev-lang/python:3.11\n\
             app-misc/foo[bar]\n\
             >=media-libs/libwebp-1.2\n\
             =app-editors/nano-8.0\n",
        );

        assert_eq!(
            sorted(selected_cpns(Some(&root), &root, &[])),
            [
                "app-editors/nano",
                "app-misc/foo",
                "dev-lang/python",
                "media-libs/libwebp"
            ]
        );
    }

    #[test]
    fn selected_cpns_adds_what_this_run_would_record() {
        // The `check_system_world` half that keeps a plain `em -p newpkg`
        // bold: not in world yet, but this run would add it.
        let (_tmp, root) = world_root("app-shells/bash\n");

        assert_eq!(
            sorted(selected_cpns(
                Some(&root),
                &root,
                &[Dep::parse("app-editors/nano").unwrap()]
            )),
            ["app-editors/nano", "app-shells/bash"]
        );
        // `--oneshot` (and every read-only caller) passes no additions, so
        // the same target stays unbold.
        assert_eq!(
            sorted(selected_cpns(Some(&root), &root, &[])),
            ["app-shells/bash"]
        );
    }

    #[test]
    fn selected_cpns_survives_a_root_with_no_profile() {
        // A bare `--root` sysroot has no `etc/portage/make.profile`: the
        // resolve fails, and the display must degrade to "nothing tracked"
        // rather than losing the run's own additions.
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        assert!(selected_cpns(Some(&root), &root, &[]).is_empty());
        assert_eq!(
            sorted(selected_cpns(
                Some(&root),
                &root,
                &[Dep::parse("app-editors/nano").unwrap()]
            )),
            ["app-editors/nano"]
        );
    }
}
