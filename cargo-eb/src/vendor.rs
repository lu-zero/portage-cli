//! Vendor tarball: `cargo vendor` into a scratch `cargo_home/gentoo`, packed so it
//! unpacks where cargo.eclass's `ECARGO_VENDOR` points.

use std::ffi::OsStr;
use std::path::Path;

use anyhow::{Context, Result};

use crate::archive::TarXz;
use crate::cargo::{PreparedPackage, crate_license, run_cargo};

/// Archive path of the cargo config snippet redirecting non-crates.io sources.
pub const SOURCES_TOML: &str = "cargo_home/sources.toml";

/// What went into the vendor tarball.
pub struct Vendored {
    /// `(vendored directory, SPDX license)`; `None` for `license-file` only crates.
    pub licenses: Vec<(String, Option<String>)>,
    /// Whether [`SOURCES_TOML`] is in the tarball (git or alternate-registry deps).
    pub extra_sources: bool,
}

/// `cargo vendor` the package's dependencies and pack them into `tarball`.
pub fn vendor_to_tarball(prepared: &PreparedPackage, tarball: &Path) -> Result<Vendored> {
    let scratch = tempfile::TempDir::new()?;
    let cargo_home = scratch.path().join("cargo_home");
    let vendor_dir = cargo_home.join("gentoo");
    let args = [
        OsStr::new("vendor"),
        OsStr::new("--versioned-dirs"),
        OsStr::new("--manifest-path"),
        prepared.manifest.as_os_str(),
        vendor_dir.as_os_str(),
    ];
    let offline = [&[OsStr::new("--offline")], &args[..]].concat();
    let config = run_cargo(&prepared.cargo_cwd, &offline)
        .or_else(|_| run_cargo(&prepared.cargo_cwd, &args))
        .context("cargo vendor")?;
    let sources = extra_sources(&String::from_utf8(config)?)?;

    let mut tar = TarXz::create(tarball)?;
    tar.append_tree(&cargo_home, Path::new("cargo_home"), &|_, _| false)?;
    if let Some(sources) = &sources {
        tar.append_data(Path::new(SOURCES_TOML), sources.as_bytes())?;
    }
    tar.finish()?;

    let mut licenses = Vec::new();
    for entry in std::fs::read_dir(&vendor_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        licenses.push((name, crate_license(&entry.path().join("Cargo.toml"))));
    }
    licenses.sort();
    Ok(Vendored {
        licenses,
        extra_sources: sources.is_some(),
    })
}

/// The `[source.*]` entries `cargo vendor` printed for non-crates.io sources,
/// pointed at cargo.eclass's `gentoo` directory source instead.
fn extra_sources(vendor_config: &str) -> Result<Option<String>> {
    let config: toml::Table =
        toml::from_str(vendor_config).context("parsing cargo vendor output")?;
    let mut out = toml::Table::new();
    let sources = config.get("source").and_then(|s| s.as_table());
    for (name, source) in sources.into_iter().flatten() {
        if name == "crates-io" || name == "vendored-sources" {
            continue;
        }
        let Some(mut source) = source.as_table().cloned() else {
            continue;
        };
        source.insert("replace-with".into(), "gentoo".into());
        out.insert(name.clone(), source.into());
    }
    if out.is_empty() {
        return Ok(None);
    }
    let mut root = toml::Table::new();
    root.insert("source".into(), out.into());
    Ok(Some(toml::to_string(&root)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_sources_are_redirected_to_gentoo() {
        let printed = r#"
[source.crates-io]
replace-with = "vendored-sources"

[source."git+https://github.com/o/r?rev=abc"]
git = "https://github.com/o/r"
rev = "abc"
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "/tmp/x"
"#;
        let out = extra_sources(printed).unwrap().unwrap();
        let t: toml::Table = toml::from_str(&out).unwrap();
        let src = &t["source"]["git+https://github.com/o/r?rev=abc"];
        assert_eq!(src["replace-with"].as_str(), Some("gentoo"));
        assert_eq!(src["rev"].as_str(), Some("abc"));
        assert_eq!(t["source"].as_table().unwrap().len(), 1);
    }

    #[test]
    fn crates_io_only_needs_no_sources() {
        let printed = "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n\
                       [source.vendored-sources]\ndirectory = \"/tmp/x\"\n";
        assert_eq!(extra_sources(printed).unwrap(), None);
    }
}
