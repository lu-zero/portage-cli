use anyhow::Result;
use portage_atom::Cpv;
use portage_repo::{ProfileUpdate, Repository};
use portage_vdb::{InstalledPackage, Vdb};

/// Detect installed packages affected by package moves recorded in
/// `profiles/updates/`
///
/// Reports what would need to be renamed; does not modify the VDB.
pub fn run(repo: &Repository, vdb: &Vdb) -> Result<()> {
    if !has_updates_dir(repo) {
        println!("No profiles/updates directory found.");
        return Ok(());
    }

    let moves = repo.profile_updates()?;

    if moves.is_empty() {
        println!("No package moves found.");
        return Ok(());
    }

    let installed: Vec<_> = vdb.packages().into_iter().collect();

    let mut any = false;
    for entry in &moves {
        for pkg in affected(&installed, entry) {
            any = true;
            match entry {
                ProfileUpdate::Move { new, .. } => {
                    let new_cpv = Cpv::new(*new, pkg.cpv().version.clone());
                    println!("move:     {}  →  {new_cpv}", pkg.cpv());
                }
                ProfileUpdate::SlotMove {
                    old_slot, new_slot, ..
                } => {
                    println!("slotmove: {}  slot {old_slot} → {new_slot}", pkg.cpv());
                }
            }
        }
    }

    if !any {
        println!("All installed packages are up to date with package moves.");
    }

    Ok(())
}

/// Installed packages one `profiles/updates` entry applies to
///
/// A `slotmove` names a full atom, so its version, glob, and slot constraints
/// decide the match — not just the CPN string.
fn affected<'a>(
    installed: &'a [InstalledPackage],
    entry: &ProfileUpdate,
) -> Vec<&'a InstalledPackage> {
    match entry {
        ProfileUpdate::Move { old, .. } => {
            installed.iter().filter(|pkg| pkg.cpn() == old).collect()
        }
        ProfileUpdate::SlotMove { dep, old_slot, .. } => installed
            .iter()
            .filter(|pkg| match pkg.slot() {
                Ok(slot) => {
                    dep.matches_cpv(pkg.cpv(), Some(&slot)) && slot.slot.as_str() == old_slot
                }
                Err(_) => false,
            })
            .collect(),
    }
}

/// Whether the repo has a `profiles/updates` directory at all
///
/// The typed parser answers "no moves" for both a missing directory and an
/// empty one, and the two cases report differently.
pub(crate) fn has_updates_dir(repo: &Repository) -> bool {
    repo.path().join("profiles").join("updates").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_atom::Dep;

    fn installed_pkg(vdb_root: &std::path::Path, cat: &str, pf: &str, slot: &str) {
        let dir = vdb_root.join(cat).join(pf);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SLOT"), slot).unwrap();
    }

    /// The VDB half of the fixture: two slots of one package, plus a bystander
    fn installed() -> (tempfile::TempDir, Vec<InstalledPackage>) {
        let tmp = tempfile::tempdir().unwrap();
        let vdb_root = tmp.path().join("var/db/pkg");
        installed_pkg(&vdb_root, "dev-libs", "libfoo-1.0", "2");
        installed_pkg(&vdb_root, "dev-libs", "libfoo-2.0", "3");
        installed_pkg(&vdb_root, "app-misc", "other-1.0", "0");
        let vdb = Vdb::open(camino::Utf8PathBuf::try_from(tmp.path().join("var/db/pkg")).unwrap())
            .unwrap();
        let pkgs: Vec<_> = vdb.packages().into_iter().collect();
        (tmp, pkgs)
    }

    fn slotmove(atom: &str, old: &str, new: &str) -> ProfileUpdate {
        ProfileUpdate::SlotMove {
            dep: Box::new(Dep::parse(atom).unwrap()),
            old_slot: old.to_string(),
            new_slot: new.to_string(),
        }
    }

    fn cpvs(matched: &[&InstalledPackage]) -> Vec<String> {
        matched.iter().map(|p| p.cpv().to_string()).collect()
    }

    // A `slotmove` atom is a full dependency atom: only the package whose own
    // slot is the recorded old one is affected, whichever version carries it.
    #[test]
    fn slotmove_atom_matches_only_its_own_slot() {
        let (_tmp, pkgs) = installed();
        let entry = slotmove("dev-libs/libfoo", "2", "3");

        assert_eq!(cpvs(&affected(&pkgs, &entry)), vec!["dev-libs/libfoo-1.0"]);
    }

    // A versioned slotmove atom is legal per PMS 4.4.4 and used to match
    // nothing: the reader compared the whole atom text against a bare CPN.
    #[test]
    fn versioned_slotmove_atom_matches_through_the_version_constraint() {
        let (_tmp, pkgs) = installed();
        let entry = slotmove(">=dev-libs/libfoo-2.0", "3", "4");

        assert_eq!(cpvs(&affected(&pkgs, &entry)), vec!["dev-libs/libfoo-2.0"]);
    }

    // A `move` renames by CPN, so every installed version of it is affected.
    #[test]
    fn move_entry_matches_every_version_of_the_cpn() {
        let (_tmp, pkgs) = installed();
        let entry = ProfileUpdate::Move {
            old: portage_atom::Cpn::parse("dev-libs/libfoo").unwrap(),
            new: portage_atom::Cpn::parse("dev-libs/libfoo2").unwrap(),
        };

        assert_eq!(
            cpvs(&affected(&pkgs, &entry)),
            vec!["dev-libs/libfoo-1.0", "dev-libs/libfoo-2.0"]
        );
    }
}
