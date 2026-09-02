//! `em maint movebin` — binary packages affected by `profiles/updates/`
//!
//! The binpkg twin of [`crate::maint::moveinst`], sharing its
//! `profiles/updates/` reader, and report-only for the same reason: renaming a
//! GPKG container is not enough on its own — the archive's own metadata and
//! the `Packages` index both name the old cpv — so this says what is stale and
//! leaves rebuilding or re-indexing to the operator.

use anyhow::{Context, Result};
use camino::Utf8Path;

use super::moveinst::{UpdateEntry, load_moves};

pub fn run(repo_path: &Utf8Path, pkgdir: &Utf8Path) -> Result<()> {
    let updates_dir = repo_path.join("profiles/updates");
    if !updates_dir.exists() {
        println!("No profiles/updates directory found.");
        return Ok(());
    }
    if !pkgdir.is_dir() {
        println!("No binary packages at {pkgdir}.");
        return Ok(());
    }

    let moves = load_moves(&updates_dir)?;
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
        let Some((cpn, cpv)) = cpn_and_cpv(rel) else {
            continue;
        };
        for entry in &moves {
            // Only `move` renames a package; a `slotmove` changes SLOT, which
            // lives inside the container's metadata and not in its filename,
            // so it cannot be detected from a directory scan.
            if let UpdateEntry::Move { from, to } = entry
                && cpn == *from
            {
                any = true;
                println!("move:     {cpv}  →  {}", cpv.replacen(from, to, 1));
            }
        }
    }

    if !any {
        println!("All binary packages are up to date with package moves.");
    }
    Ok(())
}

/// `(category/package, category/PF)` from a PKGDIR-relative container path
fn cpn_and_cpv(rel: &str) -> Option<(String, String)> {
    let cpv = crate::clean::cpv_from_container(rel)?;
    let (cat, pf) = cpv.rsplit_once('/')?;
    let cpv_parsed = portage_atom::Cpv::parse(&cpv).ok()?;
    let _ = pf;
    Some((format!("{cat}/{}", cpv_parsed.cpn.package), cpv))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_path_splits_into_cpn_and_cpv() {
        assert_eq!(
            cpn_and_cpv("sys-libs/zlib-1.3.2-r1.gpkg.tar"),
            Some((
                "sys-libs/zlib".to_string(),
                "sys-libs/zlib-1.3.2-r1".to_string()
            ))
        );
        // The multi-instance build-id suffix must not leak into either half.
        assert_eq!(
            cpn_and_cpv("sys-libs/zlib-1.3.2-r1-3.gpkg.tar"),
            Some((
                "sys-libs/zlib".to_string(),
                "sys-libs/zlib-1.3.2-r1".to_string()
            ))
        );
        assert_eq!(cpn_and_cpv("nonsense"), None);
    }
}
