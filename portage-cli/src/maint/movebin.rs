//! `em maint movebin` — binary packages affected by `profiles/updates/`
//!
//! The binpkg twin of [`crate::maint::moveinst`], sharing its
//! `profiles/updates/` reader, and report-only for the same reason: renaming a
//! GPKG container is not enough on its own — the archive's own metadata and
//! the `Packages` index both name the old cpv — so this says what is stale and
//! leaves rebuilding or re-indexing to the operator.

use anyhow::{Context, Result};
use camino::Utf8Path;
use portage_atom::Cpv;
use portage_repo::{ProfileUpdate, Repository};

use super::moveinst::has_updates_dir;

pub fn run(repo: &Repository, pkgdir: &Utf8Path) -> Result<()> {
    if !has_updates_dir(repo) {
        println!("No profiles/updates directory found.");
        return Ok(());
    }
    if !pkgdir.is_dir() {
        println!("No binary packages at {pkgdir}.");
        return Ok(());
    }

    let moves = repo.profile_updates()?;
    if moves.is_empty() {
        println!("No package moves found.");
        return Ok(());
    }

    let mut containers = Vec::new();
    portage_binpkg::find_gpkg_containers(
        pkgdir.as_std_path(),
        pkgdir.as_std_path(),
        &mut containers,
    )
    .with_context(|| format!("scanning {pkgdir}"))?;

    let mut any = false;
    for (rel, _full) in &containers {
        let Some(cpv) = cpv_from_container(rel) else {
            continue;
        };
        for entry in &moves {
            // Only `move` renames a package; a `slotmove` changes SLOT, which
            // lives inside the container's metadata and not in its filename,
            // so it cannot be detected from a directory scan.
            if let ProfileUpdate::Move { old, new } = entry
                && cpv.cpn == *old
            {
                any = true;
                let new_cpv = Cpv::new(*new, cpv.version.clone());
                println!("move:     {cpv}  →  {new_cpv}");
            }
        }
    }

    if !any {
        println!("All binary packages are up to date with package moves.");
    }
    Ok(())
}

/// The `Cpv` a PKGDIR-relative container path names
fn cpv_from_container(rel: &str) -> Option<Cpv> {
    let cpv = crate::clean::cpv_from_container(rel)?;
    Cpv::parse(&cpv).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpv_str(rel: &str) -> Option<String> {
        cpv_from_container(rel).map(|cpv| cpv.to_string())
    }

    #[test]
    fn container_path_splits_into_cpn_and_cpv() {
        assert_eq!(
            cpv_str("sys-libs/zlib-1.3.2-r1.gpkg.tar"),
            Some("sys-libs/zlib-1.3.2-r1".to_string())
        );
        // The multi-instance build-id suffix must not leak into the cpv.
        assert_eq!(
            cpv_str("sys-libs/zlib-1.3.2-r1-3.gpkg.tar"),
            Some("sys-libs/zlib-1.3.2-r1".to_string())
        );
        assert_eq!(cpv_str("nonsense"), None);
    }
}
