use std::sync::LazyLock;

use camino::{Utf8Path, Utf8PathBuf};

use portage_atom::Cpv;
use portage_metadata::Eapi;
use regex::Regex;

use super::util;
use crate::error::{Error, Result};

/// PMS 7.3.1 regex for detecting EAPI before sourcing
///
/// Matches lines of the form `EAPI=value`, `EAPI='value'`, or `EAPI="value"`,
/// with optional leading whitespace and optional trailing comment.
/// Uses alternation instead of a backreference (unsupported by the `regex` crate).
static EAPI_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^[ \t]*EAPI=(?:'([A-Za-z0-9+_.-]*)'|"([A-Za-z0-9+_.-]*)"|([A-Za-z0-9+_.-]*))[ \t]*([ \t]#.*)?$"#,
    )
    .unwrap()
});

/// A single ebuild file within a package directory
///
/// This is intentionally thin — it represents the `.ebuild` file on disk.
/// Metadata extraction goes through [`EbuildShell`](crate::EbuildShell).
///
/// See [PMS 4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
#[derive(Debug, Clone)]
pub struct Ebuild {
    cpv: Cpv,
    path: Utf8PathBuf,
}

impl Ebuild {
    pub(crate) fn new(cpv: Cpv, path: Utf8PathBuf) -> Self {
        Self { cpv, path }
    }

    /// The full category/package-version atom
    pub fn cpv(&self) -> &Cpv {
        &self.cpv
    }

    /// The category name
    pub fn category(&self) -> &str {
        &self.cpv.cpn.category
    }

    /// The package name (without version)
    pub fn name(&self) -> &str {
        &self.cpv.cpn.package
    }

    /// The version
    pub fn version(&self) -> &portage_atom::Version {
        &self.cpv.version
    }

    /// Absolute path to the `.ebuild` file
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// Read the raw ebuild file content
    pub fn read_raw(&self) -> Result<String> {
        util::read_to_string(&self.path)
    }

    /// Construct an [`Ebuild`] from an already-known [`Cpv`] and its on-disk
    /// `.ebuild` file path.
    ///
    /// Unlike [`from_path`](Self::from_path), the CPV is never parsed back
    /// out of the path text — it's exactly the one given. Use this when the
    /// caller already has the authoritative identity and the file's on-disk
    /// location may not match it: a derived/aliased category (e.g.
    /// `cross-riscv64-unknown-linux-gnu/gcc`, mapped to `sys-devel/gcc`'s
    /// real ebuild) has no on-disk directory of its own.
    ///
    /// The path is still canonicalized, for [`repo_root`](Self::repo_root)/
    /// eclass resolution.
    pub fn with_cpv(cpv: Cpv, path: &Utf8Path) -> Self {
        let real_path = path.canonicalize_utf8().unwrap_or_else(|_| path.to_owned());
        Self::new(cpv, real_path)
    }

    /// Construct an [`Ebuild`] directly from a `.ebuild` file path
    ///
    /// The CPV is derived from the directory structure:
    /// `<repo>/<category>/<pkg>/<pkg>-<ver>.ebuild`
    pub fn from_path(path: &Utf8Path) -> Result<Self> {
        // Derive the CPV from the *given* path, not a canonicalized one: the
        // `cross-*` overlay symlinks `cross-<CTARGET>/pkg` to the real category,
        // and that overlay category is what the build must use as `CATEGORY`, so
        // `toolchain.eclass` sets `CTARGET=${CATEGORY#cross-}` and cross-builds.
        let filename = path
            .file_name()
            .ok_or_else(|| Error::Shell("ebuild path has no filename".into()))?;
        let stem = filename
            .strip_suffix(".ebuild")
            .ok_or_else(|| Error::Shell(format!("{filename:?} is not an .ebuild file")))?;

        let cat_dir = path
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| Error::Shell("ebuild path too short to contain category".into()))?;
        let category = cat_dir
            .file_name()
            .ok_or_else(|| Error::Shell("cannot determine category from path".into()))?;

        let cpv_str = format!("{category}/{stem}");
        let cpv = Cpv::parse(&cpv_str)
            .map_err(|e| Error::Shell(format!("invalid CPV {cpv_str:?}: {e}")))?;

        // Store the canonical path so `repo_root` (eclasses) and the ebuild dir
        // (FILESDIR) resolve to the real package, while the CPV keeps the given
        // overlay category — a `cross-*` symlink reads as the real ebuild but
        // builds under `cross-<CTARGET>`.
        let real_path = path.canonicalize_utf8().unwrap_or_else(|_| path.to_owned());
        Ok(Self::new(cpv, real_path))
    }

    /// Repository root inferred from the ebuild path
    ///
    /// The structure is `<repo>/<cat>/<pkg>/<pkg>-<ver>.ebuild`, so the root
    /// is three levels above the ebuild file.
    pub fn repo_root(&self) -> Option<&Utf8Path> {
        self.path.parent()?.parent()?.parent()
    }

    /// Detect the EAPI by regex-matching the ebuild text before sourcing
    ///
    /// Skips blank and comment lines, then checks whether the first
    /// non-blank non-comment line is an `EAPI=` assignment. If matched,
    /// returns the parsed [`Eapi`]; otherwise defaults to [`Eapi::Zero`].
    ///
    /// See [PMS 7.3.1](https://projects.gentoo.org/pms/9/pms.html#x1-690007.3.1).
    pub fn detect_eapi(&self) -> Result<Eapi> {
        let content = self.read_raw()?;
        Ok(detect_eapi_from_str(&content))
    }
}

/// Parse EAPI from ebuild file content per PMS 7.3.1
///
/// Extracted as a free function for testability without filesystem access.
fn detect_eapi_from_str(content: &str) -> Eapi {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // First non-blank non-comment line: check for EAPI assignment.
        // Groups: 1 = single-quoted, 2 = double-quoted, 3 = unquoted.
        if let Some(caps) = EAPI_RE.captures(line) {
            let eapi_str = caps
                .get(1)
                .or_else(|| caps.get(2))
                .or_else(|| caps.get(3))
                .map(|m| m.as_str())
                .unwrap_or("");
            return eapi_str.parse().unwrap_or(Eapi::Zero);
        }
        // Not an EAPI assignment — default to 0
        return Eapi::Zero;
    }
    // Empty file or only blanks/comments — default to 0
    Eapi::Zero
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io_err(path: &std::path::Path) -> impl Fn(std::io::Error) -> Error + '_ {
        move |source| Error::Io {
            path: path.to_owned(),
            source,
        }
    }

    /// The CPV/CATEGORY-preservation invariant ("the merge-path decoupling")
    ///
    /// The whole point of [`Ebuild::with_cpv`]: a cross-derived package's
    /// virtual identity (category `cross-riscv64-unknown-linux-gnu`) must
    /// win over whatever category the real on-disk ebuild's path implies
    /// (`sys-devel`, from the real `gcc` ebuild it's aliased to).
    #[test]
    fn with_cpv_keeps_the_given_identity_not_the_paths() -> Result<()> {
        let dir = tempfile::tempdir().map_err(io_err(std::path::Path::new("tempdir")))?;
        let real_dir = dir.path().join("sys-devel").join("gcc");
        std::fs::create_dir_all(&real_dir).map_err(io_err(&real_dir))?;
        let real_path = real_dir.join("gcc-16.1.1.ebuild");
        std::fs::write(&real_path, "EAPI=8\n").map_err(io_err(&real_path))?;
        let real_path = Utf8PathBuf::from_path_buf(real_path)
            .map_err(|p| Error::Shell(format!("non-utf8 test path: {p:?}")))?;
        let canon_root =
            Utf8PathBuf::from_path_buf(dir.path().canonicalize().map_err(io_err(dir.path()))?)
                .map_err(|p| Error::Shell(format!("non-utf8 canonical path: {p:?}")))?;

        let cpv = Cpv::parse("cross-riscv64-unknown-linux-gnu/gcc-16.1.1")?;
        let ebuild = Ebuild::with_cpv(cpv.clone(), &real_path);

        assert_eq!(ebuild.cpv(), &cpv);
        assert_eq!(ebuild.category(), "cross-riscv64-unknown-linux-gnu");
        assert_eq!(ebuild.name(), "gcc");
        // repo_root still resolves through the *real* path — eclasses and
        // FILESDIR live there, not under a nonexistent virtual category dir.
        assert_eq!(ebuild.repo_root(), Some(canon_root.as_path()));
        Ok(())
    }

    #[test]
    fn detect_eapi_simple() {
        assert_eq!(detect_eapi_from_str("EAPI=8\n"), Eapi::Eight);
    }

    #[test]
    fn detect_eapi_single_quoted() {
        assert_eq!(detect_eapi_from_str("EAPI='7'\n"), Eapi::Seven);
    }

    #[test]
    fn detect_eapi_double_quoted() {
        assert_eq!(detect_eapi_from_str("EAPI=\"6\"\n"), Eapi::Six);
    }

    #[test]
    fn detect_eapi_with_comment_and_blanks() {
        let content = "\n# Copyright\n# License\n\nEAPI=8\n";
        assert_eq!(detect_eapi_from_str(content), Eapi::Eight);
    }

    #[test]
    fn detect_eapi_trailing_comment() {
        assert_eq!(detect_eapi_from_str("EAPI=7 # foo\n"), Eapi::Seven);
    }

    #[test]
    fn detect_eapi_leading_whitespace() {
        assert_eq!(detect_eapi_from_str("  EAPI=5\n"), Eapi::Five);
        assert_eq!(detect_eapi_from_str("\tEAPI=5\n"), Eapi::Five);
    }

    #[test]
    fn detect_eapi_missing_defaults_to_zero() {
        let content = "inherit eutils\nSRC_URI=foo\n";
        assert_eq!(detect_eapi_from_str(content), Eapi::Zero);
    }

    #[test]
    fn detect_eapi_empty_value_defaults_to_zero() {
        assert_eq!(detect_eapi_from_str("EAPI=\n"), Eapi::Zero);
    }

    #[test]
    fn detect_eapi_empty_file() {
        assert_eq!(detect_eapi_from_str(""), Eapi::Zero);
    }

    #[test]
    fn detect_eapi_only_comments() {
        assert_eq!(detect_eapi_from_str("# comment\n# another\n"), Eapi::Zero);
    }

    #[test]
    fn detect_eapi_mismatched_quotes_defaults_to_zero() {
        // Mismatched quotes: single open, double close — regex won't match
        assert_eq!(detect_eapi_from_str("EAPI='7\"\n"), Eapi::Zero);
    }
}
