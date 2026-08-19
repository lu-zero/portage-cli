use camino::{Utf8Path, Utf8PathBuf};

use portage_atom::{Cpn, Cpv};

use super::ebuild::Ebuild;
use super::manifest::Manifest;
use super::pkgmetadata::PkgMetadata;
use super::util;
use crate::error::Result;

/// A package directory within a category
///
/// For example, `dev-lang/rust/` contains ebuild files like `rust-1.75.0.ebuild`.
///
/// See [PMS 4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
#[derive(Debug, Clone)]
pub struct Package {
    cpn: Cpn,
    path: Utf8PathBuf,
}

impl Package {
    pub(crate) fn new(category: &str, name: &str, path: Utf8PathBuf) -> Self {
        Self {
            cpn: Cpn::new(category, name),
            path,
        }
    }

    /// The category/package name atom
    pub fn cpn(&self) -> &Cpn {
        &self.cpn
    }

    /// The category name
    pub fn category(&self) -> &str {
        &self.cpn.category
    }

    /// The package name
    pub fn name(&self) -> &str {
        &self.cpn.package
    }

    /// Absolute path to the package directory
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// List all ebuilds in this package directory, sorted by version
    ///
    /// Parses each `*.ebuild` filename into a [`Cpv`] by stripping the `.ebuild`
    /// extension and parsing `category/stem` as a versioned package atom.
    pub fn ebuilds(&self) -> Result<Vec<Ebuild>> {
        let entries = match std::fs::read_dir(&self.path) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(util::io_err(self.path.as_std_path(), e)),
        };

        let mut ebuilds = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| util::io_err(self.path.as_std_path(), e))?;
            let path: Utf8PathBuf = match entry.path().try_into() {
                Ok(p) => p,
                Err(_) => continue,
            };
            if let Some(name) = path.file_name()
                && let Some(stem) = name.strip_suffix(".ebuild")
            {
                let mut cpv_str = String::with_capacity(self.cpn.category.len() + 1 + stem.len());
                cpv_str.push_str(&self.cpn.category);
                cpv_str.push('/');
                cpv_str.push_str(stem);
                if let Ok(cpv) = Cpv::parse(&cpv_str) {
                    ebuilds.push(Ebuild::new(cpv, path));
                }
            }
        }
        ebuilds.sort_by(|a, b| a.cpv().cmp(b.cpv()));
        Ok(ebuilds)
    }

    /// Look up a specific ebuild by version string
    ///
    /// The `version` parameter is the version portion only (e.g. `"1.75.0"`),
    /// not the full filename.
    pub fn ebuild(&self, version: &str) -> Result<Option<Ebuild>> {
        let cpv_str = format!("{}/{}-{version}", self.cpn.category, self.cpn.package);
        let cpv = Cpv::parse(&cpv_str)?;
        let filename = format!("{}-{version}.ebuild", self.cpn.package);
        let path = self.path.join(&filename);
        if path.is_file() {
            Ok(Some(Ebuild::new(cpv, path)))
        } else {
            Ok(None)
        }
    }

    /// Whether a `Manifest` file exists (cheap existence check)
    pub fn has_manifest(&self) -> bool {
        self.path.join("Manifest").is_file()
    }

    /// Parse the `Manifest` file for this package
    ///
    /// Returns `Ok(None)` if no `Manifest` file exists.
    pub fn manifest(&self) -> Result<Option<Manifest>> {
        let path = self.path.join("Manifest");
        match std::fs::read_to_string(&path) {
            Ok(contents) => Manifest::parse(&contents).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(util::io_err(path.as_std_path(), e)),
        }
    }

    /// Whether a `metadata.xml` file exists (cheap existence check)
    pub fn has_metadata_xml(&self) -> bool {
        self.path.join("metadata.xml").is_file()
    }

    /// Parse the `metadata.xml` file for this package
    ///
    /// Returns `Ok(None)` if no `metadata.xml` file exists.
    ///
    /// See [PMS Appendix A](https://projects.gentoo.org/pms/9/pms.html#metadata-xml).
    pub fn metadata_xml(&self) -> Result<Option<PkgMetadata>> {
        let path = self.path.join("metadata.xml");
        match std::fs::read_to_string(&path) {
            Ok(contents) => PkgMetadata::parse(&contents).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(util::io_err(path.as_std_path(), e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_pkg(category: &str, name: &str) -> (tempfile::TempDir, Package) {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_dir = tmp.path().join(category).join(name);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let pkg = Package::new(category, name, pkg_dir.try_into().unwrap());
        (tmp, pkg)
    }

    #[test]
    fn ebuilds_parses_filenames() {
        let (_tmp, pkg) = setup_pkg("dev-util", "foo");
        let dir = pkg.path();
        std::fs::write(dir.join("foo-1.0.ebuild"), "EAPI=8\n").unwrap();
        std::fs::write(dir.join("foo-2.0-r1.ebuild"), "EAPI=8\n").unwrap();
        std::fs::write(dir.join("not-an-ebuild.txt"), "skip me").unwrap();

        let ebuilds = pkg.ebuilds().unwrap();
        assert_eq!(ebuilds.len(), 2);
        assert_eq!(ebuilds[0].cpv().version.to_string(), "1.0");
        assert_eq!(ebuilds[1].cpv().version.to_string(), "2.0-r1");
    }

    #[test]
    fn ebuilds_skips_malformed_filenames() {
        let (_tmp, pkg) = setup_pkg("dev-util", "foo");
        std::fs::write(pkg.path().join(".foo-1.0.ebuild"), "EAPI=8\n").unwrap();
        std::fs::write(pkg.path().join("foo-.ebuild"), "EAPI=8\n").unwrap();

        let ebuilds = pkg.ebuilds().unwrap();
        assert!(ebuilds.is_empty());
    }

    #[test]
    fn ebuild_lookup_by_version() {
        let (_tmp, pkg) = setup_pkg("dev-util", "foo");
        std::fs::write(pkg.path().join("foo-1.0.ebuild"), "EAPI=8\n").unwrap();

        let eb = pkg.ebuild("1.0").unwrap();
        assert!(eb.is_some());
        assert_eq!(eb.unwrap().cpv().version.to_string(), "1.0");

        assert!(pkg.ebuild("99.0").unwrap().is_none());
    }

    #[test]
    fn has_manifest() {
        let (_tmp, pkg) = setup_pkg("dev-util", "foo");
        assert!(!pkg.has_manifest());
        std::fs::write(pkg.path().join("Manifest"), "").unwrap();
        assert!(pkg.has_manifest());
    }

    #[test]
    fn manifest_parses() {
        let (_tmp, pkg) = setup_pkg("dev-util", "foo");
        std::fs::write(
            pkg.path().join("Manifest"),
            "DIST foo-1.0.tar.gz 123 BLAKE2S abc SHA512 def\n",
        )
        .unwrap();

        let m = pkg.manifest().unwrap().unwrap();
        assert_eq!(m.entries.len(), 1);
    }

    #[test]
    fn manifest_missing_returns_none() {
        let (_tmp, pkg) = setup_pkg("dev-util", "foo");
        assert!(pkg.manifest().unwrap().is_none());
    }

    #[test]
    fn metadata_xml_parses() {
        let (_tmp, pkg) = setup_pkg("dev-util", "foo");
        std::fs::write(
            pkg.path().join("metadata.xml"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE pkgmetadata SYSTEM \"https://www.gentoo.org/dtd/metadata.dtd\">\n\
             <pkgmetadata><use><flag name=\"ssl\">Enable SSL support</flag></use></pkgmetadata>\n",
        )
        .unwrap();

        let meta = pkg.metadata_xml().unwrap().unwrap();
        assert_eq!(meta.use_flags().len(), 1);
        assert_eq!(
            meta.use_flags().get("ssl").map(String::as_str),
            Some("Enable SSL support")
        );
    }

    #[test]
    fn metadata_xml_missing_returns_none() {
        let (_tmp, pkg) = setup_pkg("dev-util", "foo");
        assert!(pkg.metadata_xml().unwrap().is_none());
    }
}
