use std::collections::HashMap;

use portage_atom::DepEntry;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Version};
use portage_atom_pubgrub::PortagePackage;
use portage_vdb::Vdb;

/// One VDB-installed package, as the depgraph's post-solve passes need it.
pub struct VdbEntry {
    /// The package name.
    pub cpn: Cpn,
    /// Interned at load time — this entry is re-registered with the solver
    /// on every USE-dep co-solve fixpoint iteration (`mod.rs`'s
    /// `build_and_solve`), so this avoids re-interning the same slot string
    /// on every one of those calls.
    pub slot: Option<Interned<DefaultInterner>>,
    /// The installed version.
    pub version: Version,
    /// USE flags active at build time.
    pub active_use: Vec<Interned<DefaultInterner>>,
    /// The package's declared `IUSE`, prefix-stripped.
    pub iuse: Vec<Interned<DefaultInterner>>,
    /// RDEPEND + DEPEND as stored in the VDB (pre-USE evaluation).
    pub deps: Vec<DepEntry>,
}

/// Installed view for **ROOT** / RDEPEND / merge filtering / action tags.
///
/// See docs/user/root-model.md: host-config stage uses `VDB(target)` only; prefix
/// overlay uses `VDB(base) ∪ VDB(target)`; host uses `VDB(/)`.
///
/// `--emptytree` does **not** clear this view — emerge still reads the VDB for
/// action tags and post-solve checks; only package *selection* changes (see
/// `InstalledPolicy::Rebuild` in the solver).
///
/// `roots.installed_view_target_only()` drops the `base` side of the union
/// (see `Roots::with_target_only_installed_view`'s doc comment for why this
/// is a dedicated flag rather than reusing `base` itself).
pub fn load_target_installed(roots: &crate::Roots) -> Vec<VdbEntry> {
    let target = roots.target();
    if roots.installed_view_target_only() {
        return load_one(target);
    }
    let base = roots.base();
    if base != target {
        return load_installed(base, target);
    }
    load_one(target.or(base))
}

/// Union of two VDB roots with target shadowing base (prefix / general overlay).
/// `None` means the host `/var/db/pkg`.
///
/// Dedup key is `(Cpn, slot)`, not `(Cpn, version)`: target must shadow
/// base even when versions differ. Same package in different slots stays
/// both (slot is part of the key).
pub fn load_installed(
    base: Option<&camino::Utf8Path>,
    target: Option<&camino::Utf8Path>,
) -> Vec<VdbEntry> {
    let mut roots = vec![target];
    if target != base {
        roots.push(base);
    }
    let mut seen: std::collections::HashSet<(Cpn, Option<Interned<DefaultInterner>>)> =
        std::collections::HashSet::new();
    let mut out: Vec<VdbEntry> = Vec::new();
    for root in roots {
        for entry in load_one(root) {
            if seen.insert((entry.cpn, entry.slot)) {
                out.push(entry);
            }
        }
    }
    out
}

/// A package present on the build host (BROOT): the host instance's slot-resolved
/// package, version, and VDB-recorded active USE / IUSE. The USE/IUSE let the
/// solver check an edge's atom USE-deps against the host, so a `[flag]` the host
/// lacks triggers a rebuild rather than being pruned as host-satisfied.
pub struct HostInstalledEntry {
    /// The slot-resolved package identity.
    pub package: PortagePackage,
    /// The installed version.
    pub version: Version,
    /// USE flags active at build time.
    pub active_use: Vec<Interned<DefaultInterner>>,
    /// The package's declared `IUSE`, prefix-stripped.
    pub iuse: Vec<Interned<DefaultInterner>>,
}

/// Packages present on the **build host** (BROOT) for `host_installed` — a
/// BDEPEND already present there is satisfied without building it, unless a
/// USE-dep on that edge demands a flag the host lacks (in which case the
/// package is rebuilt). Duplicates across slots of the same package are kept
/// (each slot is a distinct `PortagePackage`).
///
/// The root selection (BROOT, plus the prefix's own VDB under `--prefix`) is
/// `crate::broot_vdb_packages` — shared with
/// `Avail::initial_bdepend`, which the same #28/#30 bug (reading the bare
/// host `/var/db/pkg` instead of the given BROOT) was once fixed in
/// separately; see that function's doc comment for the full rationale.
/// `add_host_installed` (`provider/mod.rs`) does a plain `HashMap::insert`
/// keyed by package, so whichever entry is appended last wins — matching
/// "what is in the prefix drives" for a package present in both (host
/// entries come first, prefix second).
pub fn load_host_installed(roots: &crate::Roots) -> Vec<HostInstalledEntry> {
    crate::broot_vdb_packages(roots)
        .into_iter()
        .map(|pkg| {
            let slot = pkg.slot_main().ok();
            let package = match slot.as_deref().filter(|s| !s.is_empty()) {
                Some(s) => PortagePackage::slotted(*pkg.cpn(), Interned::intern(s)),
                None => PortagePackage::unslotted(*pkg.cpn()),
            };
            let active_use = pkg
                .use_flags()
                .unwrap_or_default()
                .into_iter()
                .map(|f| Interned::intern(&f))
                .collect();
            let iuse = pkg
                .iuse()
                .unwrap_or_default()
                .into_iter()
                .map(|f| Interned::intern(f.trim_start_matches(['+', '-'])))
                .collect();
            HostInstalledEntry {
                package,
                version: pkg.cpv().version.clone(),
                active_use,
                iuse,
            }
        })
        .collect()
}

/// VDB entries from a cross sysroot (`ESYSROOT`) for `DEPEND` satisfaction.
pub fn load_sysroot_entries(sysroot: &camino::Utf8Path) -> Vec<VdbEntry> {
    load_one(Some(sysroot))
}

fn load_one(root: Option<&camino::Utf8Path>) -> Vec<VdbEntry> {
    let vdb = match root {
        Some(r) => Vdb::open(r.join("var/db/pkg")),
        None => Vdb::open_default(),
    };
    let Ok(vdb) = vdb else {
        return Vec::new();
    };
    vdb.packages()
        .into_iter()
        .map(|pkg| {
            let active_use = pkg
                .use_flags()
                .unwrap_or_default()
                .into_iter()
                .map(|f| Interned::intern(&f))
                .collect();
            let iuse = pkg
                .iuse()
                .unwrap_or_default()
                .into_iter()
                .map(|f| Interned::intern(f.trim_start_matches(['+', '-'])))
                .collect();
            let mut deps: Vec<DepEntry> = Vec::new();
            for field in [pkg.rdepend(), pkg.depend()] {
                if let Ok(Some(entries)) = field {
                    deps.extend(entries);
                }
            }
            VdbEntry {
                cpn: *pkg.cpn(),
                slot: pkg.slot_main().ok().map(|s| Interned::intern(&s)),
                version: pkg.cpv().version.clone(),
                active_use,
                iuse,
                deps,
            }
        })
        .collect()
}

/// Determine the emerge-style action tag and the currently-installed version
/// for a given (package, candidate version) pair.
///
/// - `("N",  None)`     — not installed at all
/// - `("NS", None)`     — not in this slot, but other slots are installed
/// - `("U",  Some(v))`  — upgrade within this slot
/// - `("D",  Some(v))`  — downgrade within this slot
/// - `("R",  Some(v))`  — same version, rebuild needed (changed USE flags)
pub fn action_tag<'a>(
    pkg: &PortagePackage,
    ver: &Version,
    installed: &'a HashMap<Cpn, HashMap<Interned<DefaultInterner>, Version>>,
) -> (&'static str, Option<&'a Version>) {
    let Some(by_slot) = installed.get(pkg.cpn()) else {
        return ("N", None);
    };
    let slot_key = pkg.slot().unwrap_or_else(|| Interned::intern(""));
    match by_slot.get(&slot_key) {
        None => ("NS", None),
        Some(inst) => {
            let tag = if ver > inst {
                "U"
            } else if ver < inst {
                "D"
            } else {
                "R"
            };
            (tag, Some(inst))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the riscv64 stage3 shakeout (#28/#30): a Host
    /// BDEPEND rebuilt into `base_roots()` must be recognized as satisfied
    /// by reading *that* root's VDB, not the bare host `/var/db/pkg`.
    #[test]
    fn load_host_installed_reads_the_given_root_not_the_bare_host() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_dir = tmp.path().join("var/db/pkg/dev-python/jinja2-3.1.6");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("EAPI"), "8").unwrap();
        std::fs::write(pkg_dir.join("SLOT"), "0").unwrap();
        std::fs::write(pkg_dir.join("CONTENTS"), "").unwrap();
        std::fs::write(
            pkg_dir.join("USE"),
            "python_targets_python3_14 python_targets_python3_13",
        )
        .unwrap();

        let root_str = tmp.path().to_str().unwrap();
        let host_roots = crate::Roots::for_test(root_str);
        let entries = load_host_installed(&host_roots);

        assert_eq!(
            entries.len(),
            1,
            "should find the package in the given root's VDB, not the bare host's"
        );
        assert!(
            entries[0]
                .active_use
                .iter()
                .any(|f| f.as_str() == "python_targets_python3_14"),
            "USE flags must come from the given root's VDB entry"
        );
    }

    fn write_fake_vdb_entry(root: &std::path::Path, cpv: &str, use_flags: &str) {
        let pkg_dir = root.join("var/db/pkg").join(cpv);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("EAPI"), "8").unwrap();
        std::fs::write(pkg_dir.join("SLOT"), "0").unwrap();
        std::fs::write(pkg_dir.join("CONTENTS"), "").unwrap();
        std::fs::write(pkg_dir.join("USE"), use_flags).unwrap();
    }

    /// `--prefix`: `load_host_installed` must weave in the prefix's own VDB
    /// (not just the host's), and the prefix's entry must win when both
    /// have the package — matching "what is in the prefix drives", since an
    /// unsatisfied BDEPEND now merges into the prefix, never the real host.
    #[test]
    fn load_host_installed_weaves_prefix_over_host_under_overlay() {
        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(
            host.path(),
            "dev-python/jinja2-3.1.6",
            "python_targets_python3_13",
        );
        write_fake_vdb_entry(
            prefix.path(),
            "dev-python/jinja2-3.1.6",
            "python_targets_python3_14",
        );

        let roots = crate::Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let entries = load_host_installed(&roots);

        // Host is read first, prefix second: not deduplicated here (the
        // caller's `HashMap::insert` per entry, in order, is what makes the
        // last one — the prefix's — win; see `add_host_installed`).
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .last()
                .unwrap()
                .active_use
                .iter()
                .any(|f| f.as_str() == "python_targets_python3_14"),
            "the prefix's entry must be read last, so it wins once inserted by package key"
        );
    }

    /// A package present only on the host (never built into the prefix)
    /// must still be found — the overlay weave adds the prefix, it doesn't
    /// replace the host.
    #[test]
    fn load_host_installed_still_finds_host_only_entry_under_overlay() {
        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(host.path(), "dev-python/jinja2-3.1.6", "");

        let roots = crate::Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let entries = load_host_installed(&roots);

        assert_eq!(entries.len(), 1, "must still find the host-only entry");
    }

    /// Regression test: `load_installed`'s target-shadows-base union must
    /// dedup by `(Cpn, slot)`, not `(Cpn, version)` — a base entry at a
    /// *different* version than the target's own must not survive the
    /// union.
    /// `sys-devel/binutils-2.46.1` in the target's own VDB, but the host's
    /// VDB still had the older `binutils-2.46.0` — a subsequent `-p` kept
    /// showing `[2.46.0]` (the host's version) as the installed base to
    /// "upgrade" from, because the old version-keyed dedup treated the two
    /// versions as distinct entries and let both through.
    #[test]
    fn load_installed_target_shadows_base_even_at_a_different_version() {
        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(host.path(), "sys-devel/binutils-2.46.0", "");
        write_fake_vdb_entry(prefix.path(), "sys-devel/binutils-2.46.1", "");

        let entries = load_installed(
            Some(host.path().try_into().unwrap()),
            Some(prefix.path().try_into().unwrap()),
        );

        assert_eq!(
            entries.len(),
            1,
            "the base's older version must be shadowed, not unioned alongside the target's"
        );
        assert_eq!(entries[0].version.to_string(), "2.46.1");
    }

    /// Same package, genuinely different slots (e.g. two active `gcc` slots)
    /// must both survive — the fix must not over-collapse by `Cpn` alone.
    #[test]
    fn load_installed_keeps_distinct_slots_of_the_same_package() {
        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        let pkg_dir = host.path().join("var/db/pkg/sys-devel/gcc-15.2.0");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("EAPI"), "8").unwrap();
        std::fs::write(pkg_dir.join("SLOT"), "15").unwrap();
        std::fs::write(pkg_dir.join("CONTENTS"), "").unwrap();
        std::fs::write(pkg_dir.join("USE"), "").unwrap();
        write_fake_vdb_entry(prefix.path(), "sys-devel/gcc-16.1.1", "");
        let pkg_dir = prefix.path().join("var/db/pkg/sys-devel/gcc-16.1.1");
        std::fs::write(pkg_dir.join("SLOT"), "16").unwrap();

        let entries = load_installed(
            Some(host.path().try_into().unwrap()),
            Some(prefix.path().try_into().unwrap()),
        );

        assert_eq!(entries.len(), 2, "distinct slots must both be kept");
    }
}
