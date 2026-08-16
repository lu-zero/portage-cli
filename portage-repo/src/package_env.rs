//! `/etc/portage/package.env` — per-package build environment.
//!
//! Maps atoms to environment files under `/etc/portage/env/`; the matching
//! files are sourced on top of `make.conf` for the package being built, so
//! `FEATURES`, `CFLAGS`/`CXXFLAGS`/`LDFLAGS`, `MAKEOPTS`, and arbitrary build
//! variables take effect per package (portage `config._grab_pkg_env`).
//!
//! Scope: this is the **build-environment** application only. `USE` set by an
//! env file is *not* reflected in dependency resolution — the resolved plan's
//! USE wins at build time — because the resolver does not yet read
//! `package.env`. That is a separate, resolver-side follow-up.

use std::path::{Path, PathBuf};

use portage_atom::{Cpv, Dep};

/// Resolve the ordered list of env-file paths to source for `cpv` (in `slot`).
///
/// `portage_dirs` are the `etc/portage` directories in precedence order (e.g.
/// the host config, then a `--local` overlay's `etc/portage`); a later
/// directory's entries are applied after — and therefore override — earlier
/// ones. Within a directory, `package.env` file/line order is preserved so a
/// later matching entry wins. Only existing `env/<name>` files are returned.
pub fn env_files_for(
    portage_dirs: &[PathBuf],
    cpv: &Cpv,
    slot: Option<&portage_atom::Slot>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in portage_dirs {
        let env_dir = dir.join("env");
        for (dep, names) in load_package_env(&dir.join("package.env")) {
            if dep.matches_cpv(cpv, slot) {
                for name in names {
                    let p = env_dir.join(&name);
                    if p.is_file() {
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}

/// Parse a `package.env` file (or directory of files) into `(atom, [env file
/// names])` entries.
///
/// One entry per line: `atom file1 file2 …`; `#` comments and blank lines are
/// skipped, as are lines with no env-file names or an unparseable atom. A
/// directory is read as its regular files, sorted by name (PMS 5.2.4 form).
///
/// Public (unlike the rest of this module's helpers) because
/// `portage-resolve`'s `use_env` needs the raw atom-keyed entries directly —
/// unlike [`env_files_for`], it has no single CPV to filter against yet at
/// the point it runs (it builds one flat, atom-keyed override list up front,
/// matched per candidate later, the same shape `package.use` already uses).
pub fn load_package_env(path: &Path) -> Vec<(Dep, Vec<String>)> {
    use crate::repo::util::{ConfigFilesMode, list_config_files};

    let Ok(files) = list_config_files(path, ConfigFilesMode::Flat) else {
        return Vec::new();
    };

    let mut result = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(atom_str) = parts.next() else {
                continue;
            };
            let Ok(dep) = Dep::parse(atom_str) else {
                continue;
            };
            let names: Vec<String> = parts.map(String::from).collect();
            if !names.is_empty() {
                result.push((dep, names));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn cpv(s: &str) -> Cpv {
        Cpv::parse(s).unwrap()
    }

    /// Build an `etc/portage` dir with a `package.env` and named env files.
    fn portage_dir(package_env: &str, env_files: &[(&str, &str)]) -> TempDir {
        let td = TempDir::new().unwrap();
        let pd = td.path();
        fs::write(pd.join("package.env"), package_env).unwrap();
        fs::create_dir_all(pd.join("env")).unwrap();
        for (name, body) in env_files {
            fs::write(pd.join("env").join(name), body).unwrap();
        }
        td
    }

    #[test]
    fn parses_atom_and_names_skipping_comments_and_bare_atoms() {
        let td = portage_dir(
            "# a comment\n\
             dev-libs/foo  fast big\n\
             dev-libs/bare\n\
             \n\
             =dev-libs/baz-1.2  one\n",
            &[],
        );
        let entries = load_package_env(&td.path().join("package.env"));
        assert_eq!(entries.len(), 2, "bare atom (no env files) dropped");
        assert_eq!(entries[0].1, vec!["fast", "big"]);
        assert_eq!(entries[1].1, vec!["one"]);
    }

    #[test]
    fn matches_only_the_right_package_in_order() {
        let td = portage_dir(
            "dev-libs/foo  ccache\n\
             dev-libs/foo  o3\n\
             dev-libs/other  nope\n",
            &[
                ("ccache", "FEATURES=\"${FEATURES} ccache\"\n"),
                ("o3", "CFLAGS=\"-O3\"\n"),
            ],
        );
        let dirs = vec![td.path().to_path_buf()];

        let foo = env_files_for(&dirs, &cpv("dev-libs/foo-1"), None);
        assert_eq!(foo.len(), 2, "both foo entries, in order");
        assert!(foo[0].ends_with("env/ccache"));
        assert!(foo[1].ends_with("env/o3"));

        let bar = env_files_for(&dirs, &cpv("dev-libs/bar-1"), None);
        assert!(bar.is_empty(), "non-matching package gets nothing");
    }

    #[test]
    fn missing_env_file_is_skipped() {
        let td = portage_dir("dev-libs/foo  present absent\n", &[("present", "X=1\n")]);
        let dirs = vec![td.path().to_path_buf()];
        let files = env_files_for(&dirs, &cpv("dev-libs/foo-1"), None);
        assert_eq!(files.len(), 1, "only the existing env file");
        assert!(files[0].ends_with("env/present"));
    }

    #[test]
    fn overlay_dir_applies_after_host() {
        let host = portage_dir("dev-libs/foo  host\n", &[("host", "A=1\n")]);
        let overlay = portage_dir("dev-libs/foo  ov\n", &[("ov", "B=2\n")]);
        let dirs = vec![host.path().to_path_buf(), overlay.path().to_path_buf()];
        let files = env_files_for(&dirs, &cpv("dev-libs/foo-1"), None);
        assert_eq!(files.len(), 2);
        assert!(files[0].ends_with("env/host"), "host first");
        assert!(files[1].ends_with("env/ov"), "overlay last (wins)");
    }

    #[test]
    fn slot_qualified_atom_respects_slot() {
        let td = portage_dir("dev-libs/foo:2  slotted\n", &[("slotted", "X=1\n")]);
        let dirs = vec![td.path().to_path_buf()];
        assert_eq!(
            env_files_for(
                &dirs,
                &cpv("dev-libs/foo-1"),
                Some(&portage_atom::Slot::new("2"))
            )
            .len(),
            1
        );
        assert!(
            env_files_for(
                &dirs,
                &cpv("dev-libs/foo-1"),
                Some(&portage_atom::Slot::new("3"))
            )
            .is_empty()
        );
    }

    #[test]
    fn absent_package_env_is_empty() {
        let td = TempDir::new().unwrap();
        assert!(env_files_for(&[td.path().to_path_buf()], &cpv("dev-libs/foo-1"), None).is_empty());
    }
}
