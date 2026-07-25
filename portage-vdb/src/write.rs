//! VDB write API: package registration (merge) and removal (unmerge).

use camino::Utf8PathBuf;
use portage_atom::{Cpn, Cpv, Pf};

use crate::contents::{ContentsEntry, format_contents};
use crate::error::Error;
use crate::package::InstalledPackage;
use crate::{Result, Vdb};

/// All metadata needed to register a package in the VDB after a merge.
///
/// All string fields mirror their VDB flat-file counterparts.
pub struct MergeSpec {
    /// Parsed category/package-version.
    pub cpv: Cpv,
    /// EAPI string (e.g. `"8"`).
    pub eapi: String,
    /// Full SLOT value (e.g. `"0"` or `"0/5.1"`).
    pub slot: String,
    /// Active USE flags at build time.
    pub use_flags: Vec<String>,
    /// IUSE as declared by the ebuild (may include `+`/`-` defaults).
    pub iuse: Vec<String>,
    /// `DEPEND` line from the ebuild (build-time dependencies).
    pub depend: Option<String>,
    /// `RDEPEND` line from the ebuild (run-time dependencies).
    pub rdepend: Option<String>,
    /// `BDEPEND` line from the ebuild (build-host dependencies).
    pub bdepend: Option<String>,
    /// `PDEPEND` line from the ebuild (post-install dependencies).
    pub pdepend: Option<String>,
    /// `IDEPEND` line from the ebuild (install-time dependencies).
    pub idepend: Option<String>,
    /// Keywords for this package (e.g., `"amd64"`, `"~arm64"`).
    pub keywords: Vec<String>,
    /// License expression for this package.
    pub license: Option<String>,
    /// Short description of this package.
    pub description: String,
    /// Homepage URL for this package.
    pub homepage: Option<String>,
    /// `RESTRICT` line from the ebuild.
    pub restrict: Option<String>,
    /// `PROPERTIES` line from the ebuild.
    pub properties: Option<String>,
    /// Phase functions defined by the ebuild (e.g. `["configure", "install"]`).
    pub defined_phases: Vec<String>,
    /// Repository name (e.g. `"gentoo"`).
    pub repository: Option<String>,
    /// Inherited eclasses (INHERITED).
    pub inherited: Vec<String>,
    /// FEATURES active for the build.
    pub features: Option<String>,
    /// Target host triple (CHOST).
    pub chost: Option<String>,
    /// Build triple (CBUILD).
    pub cbuild: Option<String>,
    /// C compiler flags (CFLAGS).
    pub cflags: Option<String>,
    /// C++ compiler flags (CXXFLAGS).
    pub cxxflags: Option<String>,
    /// Linker flags (LDFLAGS).
    pub ldflags: Option<String>,
    /// Rust compiler flags (RUSTFLAGS).
    pub rustflags: Option<String>,
    /// Legacy `NEEDED` lines (`<path> <needed,comma>`).
    pub needed: Vec<String>,
    /// `NEEDED.ELF.2` lines.
    pub needed_elf2: Vec<String>,
    /// `REQUIRES` lines (`<cat>: <sonames>`).
    pub requires: Vec<String>,
    /// `PROVIDES` lines (`<cat>: <sonames>`).
    pub provides: Vec<String>,
    /// Installed file list (built by walking `$D`).
    pub contents: Vec<ContentsEntry>,
    /// Unix timestamp of the build.
    pub build_time: u64,
    /// Total installed size in bytes (sum of regular-file sizes).
    pub size: u64,
    /// Monotonically increasing VDB counter for this entry.
    pub counter: u64,
}

impl Vdb {
    /// Read and atomically increment the global VDB COUNTER.
    ///
    /// Lives at `{vdb_root}/COUNTER`.  Returns the *new* value.
    /// If the file is absent the counter starts at 1.
    pub fn next_counter(&self) -> Result<u64> {
        let path = self.root().join("COUNTER");
        let current: u64 = match std::fs::read_to_string(&path) {
            Ok(s) => s.trim().parse().unwrap_or(0),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(source) => return Err(Error::Io { path, source }),
        };
        let next = current + 1;
        std::fs::write(&path, format!("{next}\n")).map_err(|source| Error::Io { path, source })?;
        Ok(next)
    }

    /// Register a merged package in the VDB.
    ///
    /// Creates `{vdb_root}/{category}/{pf}/` and writes all metadata flat files.
    /// The caller must have already:
    /// 1. Copied files from the image directory into the real root.
    /// 2. Run `pkg_preinst`.
    ///
    /// Call [`Vdb::next_counter`] to obtain the counter value before building `spec`.
    pub fn register(&self, spec: &MergeSpec) -> Result<InstalledPackage> {
        let category = spec.cpv.cpn.category.as_ref();
        let pf = format!("{}-{}", spec.cpv.cpn.package.as_ref(), spec.cpv.version);
        let pkg_dir = self.root().join(category).join(&pf);

        std::fs::create_dir_all(&pkg_dir).map_err(|source| Error::Io {
            path: pkg_dir.clone(),
            source,
        })?;

        let write_field = |name: &str, content: String| -> Result<()> {
            let p = pkg_dir.join(name);
            std::fs::write(&p, content).map_err(|source| Error::Io { path: p, source })
        };

        // Required fields.
        write_field("EAPI", format!("{}\n", spec.eapi))?;
        write_field("CATEGORY", format!("{category}\n"))?;
        write_field("SLOT", format!("{}\n", spec.slot))?;
        write_field("USE", format!("{}\n", spec.use_flags.join(" ")))?;
        write_field("IUSE", format!("{}\n", spec.iuse.join(" ")))?;
        write_field("DESCRIPTION", format!("{}\n", spec.description))?;
        write_field("BUILD_TIME", format!("{}\n", spec.build_time))?;
        write_field("SIZE", format!("{}\n", spec.size))?;
        write_field("COUNTER", format!("{}\n", spec.counter))?;
        write_field("CONTENTS", format_contents(&spec.contents))?;

        // Optional fields — omit file entirely when empty/absent.
        if !spec.keywords.is_empty() {
            write_field("KEYWORDS", format!("{}\n", spec.keywords.join(" ")))?;
        }
        macro_rules! write_opt {
            ($field:expr, $val:expr) => {
                if let Some(ref v) = $val {
                    write_field($field, format!("{v}\n"))?;
                }
            };
        }
        write_opt!("LICENSE", spec.license);
        write_opt!("HOMEPAGE", spec.homepage);
        write_opt!("RESTRICT", spec.restrict);
        write_opt!("PROPERTIES", spec.properties);
        write_opt!("DEPEND", spec.depend);
        write_opt!("RDEPEND", spec.rdepend);
        write_opt!("BDEPEND", spec.bdepend);
        write_opt!("PDEPEND", spec.pdepend);
        write_opt!("IDEPEND", spec.idepend);
        write_opt!("repository", spec.repository);
        write_opt!("FEATURES", spec.features);
        write_opt!("CHOST", spec.chost);
        write_opt!("CBUILD", spec.cbuild);
        write_opt!("CFLAGS", spec.cflags);
        write_opt!("CXXFLAGS", spec.cxxflags);
        write_opt!("LDFLAGS", spec.ldflags);
        write_opt!("RUSTFLAGS", spec.rustflags);

        // PF is the package's full name-version (the VDB dir basename); portage
        // always records it.
        write_field("PF", format!("{pf}\n"))?;

        if !spec.defined_phases.is_empty() {
            write_field(
                "DEFINED_PHASES",
                format!("{}\n", spec.defined_phases.join(" ")),
            )?;
        }
        if !spec.inherited.is_empty() {
            write_field("INHERITED", format!("{}\n", spec.inherited.join(" ")))?;
        }

        // ELF dynamic-link metadata (one line per entry, trailing newline).
        let lines = |v: &[String]| -> String {
            if v.is_empty() {
                String::new()
            } else {
                format!("{}\n", v.join("\n"))
            }
        };
        if !spec.needed.is_empty() {
            write_field("NEEDED", lines(&spec.needed))?;
        }
        if !spec.needed_elf2.is_empty() {
            write_field("NEEDED.ELF.2", lines(&spec.needed_elf2))?;
        }
        if !spec.requires.is_empty() {
            write_field("REQUIRES", lines(&spec.requires))?;
        }
        if !spec.provides.is_empty() {
            write_field("PROVIDES", lines(&spec.provides))?;
        }

        // Drop any fields cached for this entry (e.g. a same-version rebuild
        // overwriting USE in place) so later reads in this process see what
        // was just written, not whatever an earlier scan cached.
        crate::field_cache::invalidate_entry(&pkg_dir);

        Ok(InstalledPackage::from_dir(&pkg_dir, spec.cpv.clone()))
    }

    /// Remove a package's VDB directory.
    ///
    /// Only removes the VDB entry — the caller is responsible for removing
    /// installed files (from `CONTENTS`) before calling this.
    pub fn unregister(&self, pkg: &InstalledPackage) -> Result<()> {
        let path: Utf8PathBuf = pkg.path().to_path_buf();
        std::fs::remove_dir_all(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        crate::field_cache::invalidate_entry(&path);
        Ok(())
    }

    /// Find the installed package in the same main slot as the given CPN, if any.
    ///
    /// Used before a merge to detect slot conflicts that require replacing the
    /// existing occupant.  Returns `None` if no package with this CPN is
    /// installed in the given slot.
    pub fn find_slot_occupant(
        &self,
        cpn: &Cpn,
        slot_main: &str,
    ) -> Result<Option<InstalledPackage>> {
        let category = cpn.category.as_ref();
        let package_name = cpn.package.as_ref();
        let cat_dir = self.root().join(category);

        if !cat_dir.is_dir() {
            return Ok(None);
        }

        let read_dir = std::fs::read_dir(cat_dir.as_std_path()).map_err(|source| Error::Io {
            path: cat_dir,
            source,
        })?;

        for entry in read_dir {
            let entry = entry.map_err(|source| Error::Io {
                path: self.root().to_path_buf(),
                source,
            })?;
            let pkg_path: Utf8PathBuf = match entry.path().try_into() {
                Ok(p) => p,
                Err(_) => continue,
            };
            if !pkg_path.is_dir() {
                continue;
            }
            let Some(pf_str) = pkg_path.file_name() else {
                continue;
            };
            let Ok(pf) = Pf::parse(pf_str) else {
                continue;
            };
            if pf.package.as_ref() != package_name {
                continue;
            }
            let cpv = Cpv::from_parts(category, package_name, pf.version);
            let pkg = InstalledPackage::from_dir(&pkg_path, cpv);
            let Ok(pkg_slot) = pkg.slot() else {
                continue;
            };
            let pkg_slot_main = pkg_slot
                .split_once('/')
                .map(|(s, _)| s)
                .unwrap_or(&pkg_slot)
                .to_owned();
            if pkg_slot_main == slot_main {
                return Ok(Some(pkg));
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_atom::Cpv;

    use crate::ContentsKind;

    fn make_spec(cpv: Cpv) -> MergeSpec {
        MergeSpec {
            cpv,
            eapi: "8".into(),
            slot: "0".into(),
            use_flags: vec!["readline".into(), "nls".into()],
            iuse: vec!["+readline".into(), "+nls".into()],
            depend: None,
            rdepend: Some("sys-libs/readline".into()),
            bdepend: None,
            pdepend: None,
            idepend: None,
            keywords: vec!["amd64".into()],
            license: Some("GPL-3+".into()),
            description: "A test shell".into(),
            homepage: None,
            restrict: None,
            properties: None,
            defined_phases: vec!["configure".into(), "install".into()],
            repository: Some("gentoo".into()),
            inherited: vec![],
            features: None,
            chost: None,
            cbuild: None,
            cflags: None,
            cxxflags: None,
            ldflags: None,
            rustflags: None,
            needed: vec![],
            needed_elf2: vec![],
            requires: vec![],
            provides: vec![],
            contents: vec![
                ContentsEntry {
                    kind: ContentsKind::Dir,
                    path: "/usr".into(),
                    md5: None,
                    mtime: None,
                    target: None,
                },
                ContentsEntry {
                    kind: ContentsKind::Obj,
                    path: "/usr/bin/testsh".into(),
                    md5: Some("deadbeef".into()),
                    mtime: Some(1_700_000_000),
                    target: None,
                },
            ],
            build_time: 1_700_000_000,
            size: 12345,
            counter: 1,
        }
    }

    #[test]
    fn register_and_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let root: camino::Utf8PathBuf = tmp.path().to_path_buf().try_into().unwrap();
        let vdb = Vdb::open(root).unwrap();

        let cpv = Cpv::parse("app-shells/testsh-1.0").unwrap();
        let mut spec = make_spec(cpv);
        spec.rustflags = Some("-C target-cpu=neoverse-n1".into());

        let pkg = vdb.register(&spec).unwrap();
        assert_eq!(pkg.category(), "app-shells");
        assert_eq!(pkg.pf(), "testsh-1.0");

        // No dedicated accessor (build-env fields are read as a flat map by
        // the binpkg producer, not queried individually) — read the flat
        // file directly.
        assert_eq!(
            std::fs::read_to_string(pkg.path().join("RUSTFLAGS")).unwrap(),
            "-C target-cpu=neoverse-n1\n"
        );

        // Read back metadata.
        assert_eq!(pkg.eapi().unwrap().to_string(), "8");
        assert_eq!(pkg.slot().unwrap(), "0");
        assert_eq!(pkg.use_flags().unwrap(), vec!["readline", "nls"]);
        assert_eq!(pkg.description().unwrap(), "A test shell");
        assert_eq!(pkg.build_time().unwrap(), Some(1_700_000_000));
        assert_eq!(pkg.size().unwrap(), Some(12345));
        assert_eq!(pkg.counter().unwrap(), Some(1));
        assert_eq!(pkg.repository().unwrap().as_deref(), Some("gentoo"));
        assert_eq!(pkg.license().unwrap().as_deref(), Some("GPL-3+"));
        assert_eq!(pkg.keywords().unwrap(), vec!["amd64"]);

        let contents = pkg.contents().unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].kind, ContentsKind::Dir);
        assert_eq!(contents[1].kind, ContentsKind::Obj);
    }

    #[test]
    fn counter_increments() {
        let tmp = tempfile::tempdir().unwrap();
        let root: camino::Utf8PathBuf = tmp.path().to_path_buf().try_into().unwrap();
        let vdb = Vdb::open(root).unwrap();

        assert_eq!(vdb.next_counter().unwrap(), 1);
        assert_eq!(vdb.next_counter().unwrap(), 2);
        assert_eq!(vdb.next_counter().unwrap(), 3);
    }

    /// A same-version rebuild (e.g. a host BDEPEND rebuilt for a USE-flag
    /// fix) overwrites its VDB entry's fields in place via a second
    /// `register()` call at the same path. The field cache must not keep
    /// serving the pre-rebuild value afterward.
    #[test]
    fn register_over_existing_entry_invalidates_cached_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let root: camino::Utf8PathBuf = tmp.path().to_path_buf().try_into().unwrap();
        let vdb = Vdb::open(root).unwrap();

        let cpv = Cpv::parse("app-shells/testsh-1.0").unwrap();
        let pkg = vdb.register(&make_spec(cpv.clone())).unwrap();
        assert_eq!(pkg.use_flags().unwrap(), vec!["readline", "nls"]);

        let mut spec = make_spec(cpv);
        spec.use_flags = vec!["nls".into()];
        let rebuilt = vdb.register(&spec).unwrap();

        assert_eq!(
            rebuilt.use_flags().unwrap(),
            vec!["nls"],
            "a fresh handle at the same path must see the rebuild's USE, not the original"
        );
        assert_eq!(
            pkg.use_flags().unwrap(),
            vec!["nls"],
            "the pre-rebuild handle must also see the new value: it's the same on-disk \
             entry and the cache is keyed by path, not by InstalledPackage instance"
        );
    }

    #[test]
    fn unregister_removes_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root: camino::Utf8PathBuf = tmp.path().to_path_buf().try_into().unwrap();
        let vdb = Vdb::open(root).unwrap();

        let cpv = Cpv::parse("app-shells/testsh-1.0").unwrap();
        let pkg = vdb.register(&make_spec(cpv)).unwrap();
        assert!(pkg.path().is_dir());

        vdb.unregister(&pkg).unwrap();
        assert!(!pkg.path().exists());
    }

    #[test]
    fn find_slot_occupant_finds_match() {
        let tmp = tempfile::tempdir().unwrap();
        let root: camino::Utf8PathBuf = tmp.path().to_path_buf().try_into().unwrap();
        let vdb = Vdb::open(root).unwrap();

        let cpv = Cpv::parse("dev-lang/python-3.11.9").unwrap();
        let mut spec = make_spec(cpv.clone());
        spec.slot = "3.11".into();
        vdb.register(&spec).unwrap();

        let cpn = cpv.cpn;
        let found = vdb.find_slot_occupant(&cpn, "3.11").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().cpv().to_string(), "dev-lang/python-3.11.9");
    }

    #[test]
    fn find_slot_occupant_different_slot_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root: camino::Utf8PathBuf = tmp.path().to_path_buf().try_into().unwrap();
        let vdb = Vdb::open(root).unwrap();

        let cpv = Cpv::parse("dev-lang/python-3.11.9").unwrap();
        let mut spec = make_spec(cpv.clone());
        spec.slot = "3.11".into();
        vdb.register(&spec).unwrap();

        let cpn = cpv.cpn;
        let found = vdb.find_slot_occupant(&cpn, "3.12").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn find_slot_occupant_no_category_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root: camino::Utf8PathBuf = tmp.path().to_path_buf().try_into().unwrap();
        let vdb = Vdb::open(root).unwrap();

        let cpv = Cpv::parse("dev-lang/python-3.11.9").unwrap();
        let cpn = cpv.cpn;
        assert!(vdb.find_slot_occupant(&cpn, "3.11").unwrap().is_none());
    }

    #[test]
    fn counter_persists_across_vdb_instances() {
        let tmp = tempfile::tempdir().unwrap();
        let root: camino::Utf8PathBuf = tmp.path().to_path_buf().try_into().unwrap();

        {
            let vdb = Vdb::open(root.clone()).unwrap();
            assert_eq!(vdb.next_counter().unwrap(), 1);
            assert_eq!(vdb.next_counter().unwrap(), 2);
        }
        {
            let vdb = Vdb::open(root).unwrap();
            assert_eq!(vdb.next_counter().unwrap(), 3);
        }
    }
}
