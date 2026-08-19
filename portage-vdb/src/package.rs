//! Installed package representation.

use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpv, DepEntry, Pf, Slot};
use portage_metadata::{Eapi, IUse};

use crate::Result;
use crate::contents::ContentsEntry;
use crate::error::Error;
use crate::field_cache;

/// An interned SLOT name — the part before any `/`, as stored per package.
///
/// Distinct from [`portage_atom::Slot`], which pairs a slot with its subslot;
/// this is just the name. Interned because a whole VDB draws its slots from a
/// handful of distinct strings. Derefs to `&str`.
pub type SlotName = Interned<DefaultInterner>;

/// A package installed in the VDB.
///
/// Each instance corresponds to a directory under `/var/db/pkg/$CATEGORY/$PF/`.
/// Fields are read lazily from the filesystem on first access.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    path: Utf8PathBuf,
    cpv: Cpv,
}

impl InstalledPackage {
    pub(crate) fn from_dir(path: &Utf8Path, cpv: Cpv) -> Self {
        Self {
            path: path.to_path_buf(),
            cpv,
        }
    }

    /// The directory path in the VDB (`/var/db/pkg/$CATEGORY/$PF`).
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// The category name (e.g. `app-shells`).
    pub fn category(&self) -> &str {
        self.cpv.cpn.category.as_ref()
    }

    /// The package name-version without category (e.g. `bash-5.3_p9-r2`).
    ///
    /// This is the `PF` format used for VDB directory names (PMS §11.1).
    pub fn pf(&self) -> Pf {
        Pf {
            package: self.cpv.cpn.package,
            version: self.cpv.version.clone(),
        }
    }

    /// The parsed Cpn (category + package name).
    pub fn cpn(&self) -> &portage_atom::Cpn {
        &self.cpv.cpn
    }

    /// The parsed Cpv (category + package name + version).
    pub fn cpv(&self) -> &Cpv {
        &self.cpv
    }

    // -- Metadata fields (read from individual files) --

    /// Read an arbitrary VDB field by name, returning `None` if the file is absent.
    ///
    /// The value is returned as a raw (trimmed) string, exactly as stored on disk.
    /// Use this for generic `em query has`-style lookups; prefer the typed accessors
    /// (e.g. [`use_flags`](Self::use_flags), [`slot`](Self::slot)) for normal use.
    pub fn field(&self, name: &str) -> Result<Option<String>> {
        self.read_field_opt(name)
    }

    fn read_field(&self, name: &str) -> Result<String> {
        let p = self.path.join(name);
        self.read_field_opt(name)?.ok_or_else(|| Error::Io {
            path: p.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "field file missing"),
        })
    }

    fn read_field_opt(&self, name: &str) -> Result<Option<String>> {
        let p = self.path.join(name);
        field_cache::get_or_fetch(&p, || match std::fs::read_to_string(&p) {
            Ok(s) => Ok(Some(s.trim().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        })
        .map_err(|source| Error::Io { path: p, source })
    }

    /// The package description.
    pub fn description(&self) -> Result<String> {
        self.read_field("DESCRIPTION")
    }

    /// The EAPI this package was built with.
    pub fn eapi(&self) -> Result<Eapi> {
        let raw = self.read_field("EAPI")?;
        raw.parse().map_err(|_| Error::MalformedPackage {
            path: self.path.clone(),
            reason: format!("invalid EAPI: {raw}"),
        })
    }

    /// The package's slot, sub-slot included (e.g. `0/5.1`).
    ///
    /// Parsed rather than raw text so callers cannot hand the unsplit
    /// `"0/5.1"` to something expecting the slot name — see
    /// [`portage_atom::Dep::matches_cpv`]. Use [`Self::slot_main`] for just
    /// the name.
    pub fn slot(&self) -> Result<Slot> {
        let raw = self.read_field("SLOT")?;
        // An empty but present SLOT is the legitimate old-EAPI implicit-slot
        // case, and portage reads it as slot "0" (`_pkg_str`'s
        // `slot_invalid` fallback). An unreadable one stays an error, so
        // callers can still tell "no slot recorded" from "cannot read it".
        if raw.is_empty() {
            return Ok(Slot::new("0"));
        }
        Slot::parse(&raw).map_err(|_| Error::MalformedPackage {
            path: self.path.clone(),
            reason: format!("invalid SLOT: {raw}"),
        })
    }

    /// The raw `SLOT` file contents, for the rare caller that needs the text
    /// exactly as recorded rather than its parts.
    pub fn slot_raw(&self) -> Result<String> {
        self.read_field("SLOT")
    }

    /// The main slot only (the part before `/`, e.g. `0` from `0/5.1`).
    ///
    /// Interned rather than owned: a whole VDB draws its slots from a handful
    /// of distinct strings (`0`, `0/5.1`, `3.12`), so handing back a `String`
    /// allocates per package for a value every caller that keeps it was
    /// interning anyway. Derefs to `&str`.
    pub fn slot_main(&self) -> Result<SlotName> {
        Ok(self.slot()?.slot)
    }

    /// The subslot if present (the part after `/`, e.g. `5.1` from `0/5.1`).
    ///
    /// Interned, for the same reason as [`Self::slot_main`].
    pub fn subslot(&self) -> Result<Option<SlotName>> {
        Ok(self.slot()?.subslot)
    }

    /// The repository name this package was installed from.
    ///
    /// Interned: a system draws these from the two or three repos it has configured.
    pub fn repository(&self) -> Result<Option<Interned<DefaultInterner>>> {
        Ok(self
            .read_field_opt("repository")?
            .map(|r| Interned::intern(&r)))
    }

    /// USE flags active at build time.
    ///
    /// Interned: flag names are a small vocabulary repeated across the whole
    /// database (3257 tokens over 275 distinct names on one measured host),
    /// and every caller that keeps them was interning them anyway.
    pub fn use_flags(&self) -> Result<Vec<Interned<DefaultInterner>>> {
        let raw = self.read_field("USE")?;
        Ok(raw.split_whitespace().map(Interned::intern).collect())
    }

    /// IUSE flags declared by the package, with their `+`/`-` defaults.
    ///
    /// [`IUse`] keeps the name interned and the default separate, so callers
    /// stop stripping the prefix and re-interning what it already holds —
    /// use `Interned::from(&iu)` for the bare name.
    pub fn iuse(&self) -> Result<Vec<IUse>> {
        let raw = self.read_field("IUSE")?;
        IUse::parse_line(&raw).map_err(|e| Error::MalformedPackage {
            path: self.path.clone(),
            reason: format!("invalid IUSE: {e}"),
        })
    }

    /// Build timestamp (Unix epoch).
    pub fn build_time(&self) -> Result<Option<u64>> {
        self.read_field_opt("BUILD_TIME")?
            .map(|s| {
                s.parse().map_err(|_| Error::MalformedPackage {
                    path: self.path.clone(),
                    reason: format!("invalid BUILD_TIME: {s}"),
                })
            })
            .transpose()
    }

    /// Installed size in bytes.
    pub fn size(&self) -> Result<Option<u64>> {
        self.read_field_opt("SIZE")?
            .map(|s| {
                s.parse().map_err(|_| Error::MalformedPackage {
                    path: self.path.clone(),
                    reason: format!("invalid SIZE: {s}"),
                })
            })
            .transpose()
    }

    /// Installation counter (monotonically increasing).
    pub fn counter(&self) -> Result<Option<u64>> {
        self.read_field_opt("COUNTER")?
            .map(|s| {
                s.parse().map_err(|_| Error::MalformedPackage {
                    path: self.path.clone(),
                    reason: format!("invalid COUNTER: {s}"),
                })
            })
            .transpose()
    }

    /// Keywords. Empty if the KEYWORDS file is absent.
    ///
    /// Interned: the vocabulary is one entry per arch plus its `~` form, so a
    /// whole database's keywords resolve to a few dozen distinct strings.
    pub fn keywords(&self) -> Result<Vec<Interned<DefaultInterner>>> {
        let raw = self.read_field_opt("KEYWORDS")?.unwrap_or_default();
        Ok(raw.split_whitespace().map(Interned::intern).collect())
    }

    /// License string.
    pub fn license(&self) -> Result<Option<String>> {
        self.read_field_opt("LICENSE")
    }

    /// Homepage URL(s).
    pub fn homepage(&self) -> Result<Option<String>> {
        self.read_field_opt("HOMEPAGE")
    }

    // -- Dependency fields --

    /// DEPEND (build dependencies) parsed as a dep tree.
    pub fn depend(&self) -> Result<Option<Vec<DepEntry>>> {
        self.read_dep_field("DEPEND")
    }

    /// RDEPEND (runtime dependencies) parsed as a dep tree.
    pub fn rdepend(&self) -> Result<Option<Vec<DepEntry>>> {
        self.read_dep_field("RDEPEND")
    }

    /// BDEPEND (build-tool dependencies) parsed as a dep tree.
    pub fn bdepend(&self) -> Result<Option<Vec<DepEntry>>> {
        self.read_dep_field("BDEPEND")
    }

    /// PDEPEND (post-merge dependencies) parsed as a dep tree.
    pub fn pdepend(&self) -> Result<Option<Vec<DepEntry>>> {
        self.read_dep_field("PDEPEND")
    }

    /// IDEPEND (install-time dependencies) parsed as a dep tree.
    pub fn idepend(&self) -> Result<Option<Vec<DepEntry>>> {
        self.read_dep_field("IDEPEND")
    }

    fn read_dep_field(&self, name: &str) -> Result<Option<Vec<DepEntry>>> {
        let raw = match self.read_field_opt(name)? {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(None),
        };
        DepEntry::parse(&raw)
            .map(Some)
            .map_err(|source| Error::MalformedPackage {
                path: self.path.clone(),
                reason: format!("failed to parse {name}: {source}"),
            })
    }

    // -- CONTENTS --
    //
    // Deliberately outside `field_cache`. That cache exists for the small
    // fields a depgraph build re-reads 3-4 times (USE, IUSE, SLOT); CONTENTS
    // averages ~216 KB and is the whole installed-file list, so memoizing it
    // retains the entire VDB's file list for the process lifetime and hands
    // back a full copy per hit. Reading it fresh also keeps it correct
    // against a VDB another process is writing, which the cache — invalidated
    // only by this process's own `register`/`unregister` — is not.

    /// Parse the CONTENTS file — the list of files installed by this package.
    pub fn contents(&self) -> Result<Vec<ContentsEntry>> {
        Ok(ContentsEntry::parse(&self.contents_required()?))
    }

    /// The unparsed CONTENTS text, to scan with [`crate::ContentsRef::parse`]
    /// instead of materializing every entry.
    ///
    /// `None` when the package has no CONTENTS file at all.
    ///
    /// Returned as read, without the trim the other field accessors apply:
    /// trimming a string this size is a second full copy, and
    /// [`crate::ContentsRef::parse`] already skips blank lines and trims each
    /// line it yields.
    pub fn contents_raw(&self) -> Result<Option<String>> {
        let mut buf = String::new();
        Ok(self.contents_into(&mut buf)?.then_some(buf))
    }

    /// [`Self::contents_raw`] into a caller-owned buffer, returning whether a
    /// CONTENTS file was there at all.
    ///
    /// A scan reads one of these per package and they average a few hundred
    /// KB, so allocating a fresh `String` each time churns the whole VDB's
    /// worth of bytes through the allocator — and across threads that shows
    /// up as retained per-thread arenas, not just as work. Reusing one buffer
    /// per scanner costs the largest CONTENTS seen instead.
    pub fn contents_into(&self, buf: &mut String) -> Result<bool> {
        use std::io::Read as _;

        buf.clear();
        let path = self.path.join("CONTENTS");
        match std::fs::File::open(&path) {
            Ok(mut file) => match file.read_to_string(buf) {
                Ok(_) => Ok(true),
                Err(source) => Err(Error::Io { path, source }),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(Error::Io { path, source }),
        }
    }

    /// [`Self::contents_raw`] for the callers that treat an absent CONTENTS
    /// as an error rather than an empty package.
    fn contents_required(&self) -> Result<String> {
        self.contents_raw()?.ok_or_else(|| Error::Io {
            path: self.path.join("CONTENTS"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "field file missing"),
        })
    }

    /// Returns `true` if this package owns the given path.
    pub fn owns(&self, file_path: &Utf8Path) -> Result<bool> {
        let raw = self.contents_required()?;
        Ok(crate::ContentsRef::parse(&raw).any(|e| {
            matches!(e.kind, crate::ContentsKind::Obj | crate::ContentsKind::Sym)
                && e.path == file_path
        }))
    }
}

impl std::fmt::Display for InstalledPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.cpv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_fake_pkg(
        dir: &std::path::Path,
        category: &str,
        pf: &str,
        fields: &[(&str, &str)],
    ) -> Utf8PathBuf {
        let pkg_dir = dir.join(category).join(pf);
        fs::create_dir_all(&pkg_dir).unwrap();
        for (name, content) in fields {
            fs::write(pkg_dir.join(name), content).unwrap();
        }
        pkg_dir.try_into().unwrap()
    }

    #[test]
    fn read_basic_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let cpv = Cpv::parse("app-shells/bash-5.3_p9-r2").unwrap();
        let fields = [
            ("DESCRIPTION", "The standard GNU Bourne again shell"),
            ("EAPI", "8"),
            ("SLOT", "0"),
            ("USE", "net nls readline"),
            ("IUSE", "+net +nls +readline"),
            ("BUILD_TIME", "1778566176"),
            ("SIZE", "10401340"),
            ("COUNTER", "992555"),
            ("CATEGORY", "app-shells"),
            ("repository", "gentoo"),
        ];
        let pkg_dir = make_fake_pkg(tmp.path(), "app-shells", "bash-5.3_p9-r2", &fields);
        let pkg = InstalledPackage::from_dir(&pkg_dir, cpv);

        assert_eq!(pkg.category(), "app-shells");
        assert_eq!(pkg.pf(), "bash-5.3_p9-r2");
        assert_eq!(
            pkg.description().unwrap(),
            "The standard GNU Bourne again shell"
        );
        assert_eq!(pkg.slot().unwrap().to_string(), "0");
        assert_eq!(pkg.use_flags().unwrap(), vec!["net", "nls", "readline"]);
        let iuse: Vec<String> = pkg.iuse().unwrap().iter().map(|i| i.to_string()).collect();
        assert_eq!(iuse, vec!["+net", "+nls", "+readline"]);
        assert_eq!(pkg.build_time().unwrap(), Some(1778566176));
        assert_eq!(pkg.size().unwrap(), Some(10401340));
        assert_eq!(pkg.counter().unwrap(), Some(992555));
        assert_eq!(pkg.repository().unwrap().as_deref(), Some("gentoo"));
    }

    #[test]
    fn read_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let cpv = Cpv::parse("app-shells/bash-5.3").unwrap();
        let contents = "dir /etc\nobj /etc/foo abc123 100\nsym /etc/bar -> baz 200\n";
        let fields = [("CONTENTS", contents)];
        let pkg_dir = make_fake_pkg(tmp.path(), "app-shells", "bash-5.3", &fields);
        let pkg = InstalledPackage::from_dir(&pkg_dir, cpv);

        let entries = pkg.contents().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[1].path, Utf8PathBuf::from("/etc/foo"));
    }

    #[test]
    fn owns_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cpv = Cpv::parse("app-shells/bash-5.3").unwrap();
        let contents = "dir /etc\nobj /etc/foo abc123 100\n";
        let fields = [("CONTENTS", contents)];
        let pkg_dir = make_fake_pkg(tmp.path(), "app-shells", "bash-5.3", &fields);
        let pkg = InstalledPackage::from_dir(&pkg_dir, cpv);

        assert!(pkg.owns(Utf8Path::new("/etc/foo")).unwrap());
        assert!(!pkg.owns(Utf8Path::new("/etc/bar")).unwrap());
        assert!(!pkg.owns(Utf8Path::new("/etc")).unwrap());
    }
}
