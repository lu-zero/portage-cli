//! `em revdep`: rebuild installed packages whose own ELF objects require a
//! shared-library soname nothing installed currently provides — the same
//! underlying question `@preserved-rebuild` asks (see `preserve_libs.rs`),
//! but broader: not limited to sonames `FEATURES=preserve-libs` is tracking.
//!
//! Deliberately **VDB-metadata-based**, not gentoolkit's live `scanelf`
//! rescan of `ld.so.conf`/`PATH` directories (`revdep-rebuild`'s Python
//! implementation, `gentoolkit/revdep_rebuild/*.py`): every installed
//! package's own `NEEDED.ELF.2` VDB field already records exactly what its
//! own ELF objects require. Unlike gentoolkit — whose global, path-keyed
//! scan needs a separate `CONTENTS`-intersection pass (`assign.py`) to map a
//! broken file back to its owning package — `em`'s `NEEDED.ELF.2` is stored
//! *per package*, so the owner is already known while walking; there is
//! never an ownership-assignment step to write.
//!
//! Documented simplification, not silent: this cannot catch breakage in
//! files that were never recorded as ELF-owning VDB entries at all (hand-
//! installed binaries, `.la` libtool-archive consumers, or a file modified
//! out-of-band without a matching VDB update) — that would need a real
//! filesystem rescan, out of scope for this pass. See `README.md`'s `em
//! revdep` section.

use camino::{Utf8Path, Utf8PathBuf};
use std::collections::HashSet;

use portage_atom::Cpv;
use portage_vdb::Vdb;

use crate::cli;
use crate::elfscan;
use crate::error::Result;
use crate::preserve_libs::{self, PreservedLibsRegistry};
use crate::vdb::open_cli_vdb;

/// One broken `DT_NEEDED` reference: the soname nothing currently provides,
/// and which of the owning package's own files required it (for the
/// report).
struct BrokenNeed {
    soname: String,
    consumer: Utf8PathBuf,
}

/// One installed package with at least one [`BrokenNeed`]
struct BrokenConsumer {
    cpv: Cpv,
    slot: portage_vdb::SlotName,
    broken: Vec<BrokenNeed>,
}

/// Every `(multilib category, soname)` provided by a still-existing file:
/// either a live package's own recorded soname, or a registry-preserved path
/// re-scanned directly. Mirrors [`preserve_libs::build_link_graph`]'s own
/// provider set, but keyed on file existence rather than "not in the current
/// removal batch" — there is no removal batch here, just "is this soname
/// resolvable right now."
fn providers(
    vdb: &Vdb,
    registry: &PreservedLibsRegistry,
    root: &Utf8Path,
) -> HashSet<(String, String)> {
    let mut providers = HashSet::new();
    for pkg in vdb.packages() {
        for rec in preserve_libs::package_needed(&pkg) {
            let Some(soname) = rec.soname else { continue };
            let rel = rec.path.as_str().trim_start_matches('/');
            if root.join(rel).as_std_path().exists() {
                providers.insert((rec.category, soname));
            }
        }
    }
    for path in registry.all_paths() {
        let rel = path.as_str().trim_start_matches('/');
        let abs = root.join(rel);
        if let Some(info) = elfscan::scan_file(abs.as_std_path())
            && let Some(soname) = info.soname
        {
            providers.insert((info.category, soname));
        }
    }
    providers
}

/// Every installed package with at least one `DT_NEEDED` soname the
/// [`providers`] set doesn't satisfy, optionally narrowed to sonames
/// containing `library_filter` (`-L`/`--library`, mirrors gentoolkit's own
/// flag).
fn find_broken_consumers(
    vdb: &Vdb,
    registry: &PreservedLibsRegistry,
    root: &Utf8Path,
    library_filter: Option<&str>,
) -> Vec<BrokenConsumer> {
    let providers = providers(vdb, registry, root);
    let mut out = Vec::new();
    for pkg in vdb.packages() {
        let mut broken = Vec::new();
        for rec in preserve_libs::package_needed(&pkg) {
            for soname in &rec.needed {
                if let Some(filter) = library_filter
                    && !soname.contains(filter)
                {
                    continue;
                }
                if !providers.contains(&(rec.category.clone(), soname.clone())) {
                    broken.push(BrokenNeed {
                        soname: soname.clone(),
                        consumer: rec.path.clone(),
                    });
                }
            }
        }
        if broken.is_empty() {
            continue;
        }
        let Ok(slot) = pkg.slot_main() else { continue };
        out.push(BrokenConsumer {
            cpv: pkg.cpv().clone(),
            slot,
            broken,
        });
    }
    out
}

/// Real-portage-style report, matching [`preserve_libs::report_preserved`]'s
/// `>>> package: <cpv>` / ` * ...` shape.
fn report(consumer: &BrokenConsumer) {
    println!(">>> package: {}", consumer.cpv);
    for need in &consumer.broken {
        crate::style::einfo_line!("broken: {} (needed by {})", need.soname, need.consumer);
    }
}

/// `em revdep [-L NAME]`: detect and rebuild packages with broken shared library
/// dependencies
///
/// Always `--oneshot`+`--complete-graph` — a revdep-triggered rebuild is never a world
/// selection and must not leave a half-fixed chain unresolved. `-p`/`-a`/`-j`/`-k`/etc. are
/// read from `cli` by [`crate::emerge_atoms`] exactly as for every other caller.
pub async fn run(cli: &cli::Cli, library: Option<&str>) -> Result<()> {
    let vdb = open_cli_vdb(cli)?;
    let roots = cli.roots();
    let root = roots.merge_root().to_owned();
    let registry = PreservedLibsRegistry::load(&root);

    let broken = find_broken_consumers(&vdb, &registry, &root, library);
    if broken.is_empty() {
        println!(">>> Nothing to revdep-rebuild.");
        return Ok(());
    }

    let mut atoms: Vec<String> = Vec::with_capacity(broken.len());
    for consumer in &broken {
        report(consumer);
        atoms.push(format!("{}:{}", consumer.cpv.cpn, consumer.slot));
    }
    atoms.sort();
    atoms.dedup();

    println!("\n>>> {} package(s) would be rebuilt.", atoms.len());

    let mut merge_flags = cli.merge_flags.clone();
    merge_flags.oneshot = true;
    merge_flags.complete_graph = true;

    crate::emerge_atoms(
        cli,
        &atoms,
        crate::EmergeOpts {
            use_override: &[],
            nodeps: false,
            depgraph_flags: None,
            merge_flags: Some(merge_flags),
            use_outer_eroot: false,
            target_only_installed_view: false,
            update_world: false,
            is_resume: false,
            activity: None,
            activity_session: Default::default(),
            extra_aliases: &[],
            extra_path: &[],
            autounmask_widen: false,
            sysroot_override: None,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fake_package(
        vdb_root: &std::path::Path,
        cat: &str,
        pf: &str,
        slot: &str,
        contents_lines: &str,
        needed_lines: &str,
    ) {
        let dir = vdb_root.join(cat).join(pf);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SLOT"), slot).unwrap();
        std::fs::write(dir.join("CONTENTS"), contents_lines).unwrap();
        std::fs::write(dir.join("NEEDED.ELF.2"), needed_lines).unwrap();
    }

    fn open_vdb(root: &Utf8Path) -> Vdb {
        Vdb::open(root.join("var/db/pkg")).unwrap()
    }

    #[test]
    fn flags_a_consumer_with_no_provider_anywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let vdb_root = root.join("var/db/pkg");

        // Consumer needs libfoo.so.1, but nothing installed provides it and
        // no registry entry covers it either.
        write_fake_package(
            vdb_root.as_std_path(),
            "app-misc",
            "consumer-1.0",
            "0",
            "obj /usr/bin/consumer bbbb 0\n",
            "X86_64;/usr/bin/consumer;;;libfoo.so.1;x86_64\n",
        );

        let vdb = open_vdb(&root);
        let registry = PreservedLibsRegistry::load(&root);
        let broken = find_broken_consumers(&vdb, &registry, &root, None);

        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].cpv.to_string(), "app-misc/consumer-1.0");
        assert_eq!(broken[0].broken[0].soname, "libfoo.so.1");
    }

    #[test]
    fn fully_satisfied_graph_is_not_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let vdb_root = root.join("var/db/pkg");

        // Provider file must genuinely exist on disk for the existence check.
        let lib_path = root.join("usr/lib64/libfoo.so.1");
        std::fs::create_dir_all(lib_path.parent().unwrap()).unwrap();
        std::fs::write(
            &lib_path,
            b"not really an elf, existence is all that matters here",
        )
        .unwrap();

        write_fake_package(
            vdb_root.as_std_path(),
            "sys-libs",
            "libfoo-1.0",
            "0",
            "obj /usr/lib64/libfoo.so.1 aaaa 0\n",
            "X86_64;/usr/lib64/libfoo.so.1;libfoo.so.1;;;x86_64\n",
        );
        write_fake_package(
            vdb_root.as_std_path(),
            "app-misc",
            "consumer-1.0",
            "0",
            "obj /usr/bin/consumer bbbb 0\n",
            "X86_64;/usr/bin/consumer;;;libfoo.so.1;x86_64\n",
        );

        let vdb = open_vdb(&root);
        let registry = PreservedLibsRegistry::load(&root);
        let broken = find_broken_consumers(&vdb, &registry, &root, None);

        assert!(broken.is_empty());
    }

    #[test]
    fn library_filter_narrows_to_matching_sonames_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let vdb_root = root.join("var/db/pkg");

        write_fake_package(
            vdb_root.as_std_path(),
            "app-misc",
            "consumer-1.0",
            "0",
            "obj /usr/bin/consumer bbbb 0\n",
            "X86_64;/usr/bin/consumer;;;libfoo.so.1,libbar.so.2;x86_64\n",
        );

        let vdb = open_vdb(&root);
        let registry = PreservedLibsRegistry::load(&root);

        let all = find_broken_consumers(&vdb, &registry, &root, None);
        assert_eq!(all[0].broken.len(), 2);

        let filtered = find_broken_consumers(&vdb, &registry, &root, Some("bar"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].broken.len(), 1);
        assert_eq!(filtered[0].broken[0].soname, "libbar.so.2");

        let unmatched = find_broken_consumers(&vdb, &registry, &root, Some("qux"));
        assert!(unmatched.is_empty());
    }

    #[test]
    fn a_missing_provider_file_is_still_broken_despite_stale_vdb_bookkeeping() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let vdb_root = root.join("var/db/pkg");

        // libfoo's own VDB record still lists it as a provider, but the file
        // itself was removed from disk out-of-band.
        write_fake_package(
            vdb_root.as_std_path(),
            "sys-libs",
            "libfoo-1.0",
            "0",
            "obj /usr/lib64/libfoo.so.1 aaaa 0\n",
            "X86_64;/usr/lib64/libfoo.so.1;libfoo.so.1;;;x86_64\n",
        );
        write_fake_package(
            vdb_root.as_std_path(),
            "app-misc",
            "consumer-1.0",
            "0",
            "obj /usr/bin/consumer bbbb 0\n",
            "X86_64;/usr/bin/consumer;;;libfoo.so.1;x86_64\n",
        );

        let vdb = open_vdb(&root);
        let registry = PreservedLibsRegistry::load(&root);
        let broken = find_broken_consumers(&vdb, &registry, &root, None);

        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].cpv.to_string(), "app-misc/consumer-1.0");
    }
}
