use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Dep;
use portage_vdb::Vdb;

use super::sets::KnownSets;
use crate::query::which::dep_matches_cpv;

const DEFAULT_WORLD: &str = "/var/lib/portage/world";

pub fn run(vdb: &Vdb, fix: bool, root: Option<&Utf8Path>) -> Result<()> {
    let known_sets = KnownSets::load(root);
    let installed: Vec<_> = vdb.packages().into_iter().collect();

    let mut total_orphaned = 0usize;
    let mut total_invalid = 0usize;

    check_world_file(
        &world_path(root),
        &installed,
        &known_sets,
        fix,
        &mut total_orphaned,
        &mut total_invalid,
    )?;
    check_world_sets_file(
        &world_sets_path(root),
        &known_sets,
        fix,
        &mut total_orphaned,
    )?;

    if total_orphaned == 0 && total_invalid == 0 {
        println!("World files are consistent.");
    } else if !fix {
        eprintln!(
            "\n{} issue{} found. Run with --fix to remove them.",
            total_orphaned + total_invalid,
            if total_orphaned + total_invalid == 1 {
                ""
            } else {
                "s"
            }
        );
    }

    Ok(())
}

fn check_world_file(
    path: &Utf8Path,
    installed: &[portage_vdb::InstalledPackage],
    known_sets: &KnownSets,
    fix: bool,
    orphaned_count: &mut usize,
    invalid_count: &mut usize,
) -> Result<()> {
    let content = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;

    let mut orphaned: Vec<String> = Vec::new();
    let mut invalid: Vec<String> = Vec::new();
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
                eprintln!("warning: invalid world entry '{trimmed}': {e}");
                invalid.push(trimmed.to_owned());
                continue;
            }
        };
        if installed.iter().any(|pkg| dep_matches_cpv(&dep, pkg.cpv())) {
            kept.push(line);
        } else {
            orphaned.push(trimmed.to_owned());
        }
    }

    for atom in &invalid {
        println!("!!! {path}: '{atom}': invalid atom");
    }
    for atom in &orphaned {
        println!("!!! {path}: '{atom}': not installed / unknown set");
    }

    *orphaned_count += orphaned.len();
    *invalid_count += invalid.len();

    if fix && (!orphaned.is_empty() || !invalid.is_empty()) {
        let new_content = kept.join("\n") + "\n";
        std::fs::write(path, new_content).with_context(|| format!("writing {path}"))?;
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

    for atom in &orphaned {
        println!("!!! {path}: '{atom}': unknown set");
    }
    *orphaned_count += orphaned.len();

    if fix && !orphaned.is_empty() {
        let new_content = kept.join("\n") + "\n";
        std::fs::write(path, new_content).with_context(|| format!("writing {path}"))?;
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

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let new_content = if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    };
    // Atomic replace: write to a sibling tempfile then rename so a crash
    // mid-write cannot truncate world.
    let tmp = path.with_file_name(format!("{}.tmp", path.file_name().unwrap_or("world")));
    if let Err(e) = std::fs::write(&tmp, &new_content).and_then(|_| std::fs::rename(&tmp, &path)) {
        let _ = std::fs::remove_file(tmp.as_std_path());
        eprintln!("warning: could not update {path}: {e}");
    }
}

fn world_path(root: Option<&Utf8Path>) -> Utf8PathBuf {
    match root {
        Some(r) => r.join("var/lib/portage/world"),
        None => Utf8PathBuf::from(DEFAULT_WORLD),
    }
}

fn world_sets_path(root: Option<&Utf8Path>) -> Utf8PathBuf {
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
}
