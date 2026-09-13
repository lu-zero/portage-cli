//! Ebuild generation via `minijinja`.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::Result;
use chrono::Datelike;
use minijinja::{Environment, context};

use crate::cargo::{Crate, PackageMetadata};

const TEMPLATE: &str = include_str!("../templates/ebuild.j2");

fn crates_var(crates: &[Crate]) -> String {
    let mut entries: Vec<String> = crates.iter().filter_map(|c| c.crate_entry()).collect();
    if entries.is_empty() {
        return "\n".to_string();
    }
    entries.sort();
    format!("\n\t{}\n", entries.join("\n\t"))
}

/// Build the `declare -A GIT_CRATES=(...)` block.
///
/// The subdir each entry needs is resolved by opening the crate's fetched
/// archive at `distdir` and finding where its `Cargo.toml` actually landed
/// (`cargo::package_directory_in_archive`), not a `{name}-%commit%`
/// placeholder. The entry value itself is built by `Crate::git_crate_entry`,
/// which already handles every `GitHost` — no need to duplicate that here.
fn git_crates_var(crates: &[Crate], distdir: &Path) -> String {
    let mut entries = Vec::new();
    for c in crates {
        let Crate::Git(g) = c else { continue };
        let subdir = crate::cargo::package_directory_in_archive(c, distdir).unwrap_or_else(|| {
            // Best-effort fallback if the archive wasn't found (fetch
            // failed to reach here, or the crate hasn't been fetched yet) —
            // the common case (no nested workspace member) still matches.
            let repo = g.repository.rsplit('/').next().unwrap_or(&g.name);
            format!("{repo}-{}", g.commit)
        });
        let Some(val) = c.git_crate_entry(&subdir) else {
            continue;
        };
        // shlex quote like `pycargoebuild/ebuild.py:get_GIT_CRATES`
        entries.push(format!("\t[{}]='{}'", g.name, val.replace('\'', "'\\''")));
    }
    if entries.is_empty() {
        return String::new();
    }
    entries.sort();
    format!("\n\ndeclare -A GIT_CRATES=(\n{}\n)", entries.join("\n"))
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn bash_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
}
fn url_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'/'
            | b':'
            | b'@'
            | b'#'
            | b'?'
            | b'='
            | b'&' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `Cargo.toml`'s `description` field is optional, but PMS 7.2 requires
/// `DESCRIPTION` "must not be empty" — fall back to a generic description
/// built from the crate name rather than emitting `DESCRIPTION=""`.
fn pkg_description(pkg: &PackageMetadata) -> String {
    match pkg.description.as_deref().map(str::trim) {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => format!("{} Rust crate", pkg.name),
    }
}

fn get_pkg_license(
    pkg: &PackageMetadata,
    mapping: &HashMap<String, String>,
) -> Result<String, anyhow::Error> {
    if let Some(lic) = &pkg.license {
        let spdx = lic.replace('/', " OR ");
        let ebuild = crate::license::spdx_to_ebuild(&spdx, mapping)?;
        Ok(crate::license::format_license_var(&ebuild, "LICENSE=\""))
    } else {
        Ok(String::new())
    }
}

fn crate_licenses_from_spdx(
    spdx_list: &[String],
    mapping: &HashMap<String, String>,
) -> Result<String, anyhow::Error> {
    let mut gentoo_set = BTreeSet::new();
    for spdx in spdx_list {
        gentoo_set.insert(crate::license::spdx_to_ebuild(spdx, mapping)?);
    }
    if gentoo_set.is_empty() {
        return Ok(String::new());
    }
    let combined_gentoo = gentoo_set.iter().cloned().collect::<Vec<_>>().join(" ");
    let parsed = portage_metadata::LicenseExpr::parse(&combined_gentoo)?;
    let deduped = parsed.dedup().to_string();
    let mut s = crate::license::format_license_var(&deduped, "LICENSE+=\" ");
    if !s.starts_with('\n') && !s.is_empty() {
        s = format!(" {s}");
    }
    Ok(s)
}

/// IUSE tokens for Cargo features (`+foo` if default). The `default` group is
/// already stripped in [`PackageMetadata`].
pub fn iuse_plus(pkg: &PackageMetadata) -> String {
    let mut v: Vec<String> = pkg
        .features
        .iter()
        .map(|(k, d)| if *d { format!("+{k}") } else { k.clone() })
        .collect();
    v.sort();
    if v.is_empty() {
        String::new()
    } else {
        format!(" {}", v.join(" "))
    }
}

pub fn myfeatures_body(pkg: &PackageMetadata) -> String {
    let mut v: Vec<String> = pkg.features.keys().cloned().collect();
    v.sort();
    v.iter()
        .map(|f| format!("\t\t$(usev {f})"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct RenderInput<'a> {
    pub pkg: &'a PackageMetadata,
    pub crates: &'a [Crate],
    pub crate_tarball: Option<&'a str>,
    pub source_tarball: Option<&'a str>,
    pub prog_version: &'a str,
    pub distdir: &'a Path,
    pub mapping_path: &'a Path,
    pub crate_license_spdx: &'a [String],
}

pub fn render_ebuild(input: RenderInput<'_>) -> Result<String> {
    let mut env = Environment::new();
    env.set_trim_blocks(true);
    env.set_keep_trailing_newline(true);
    env.add_template("ebuild", TEMPLATE)?;
    let tmpl = env.get_template("ebuild")?;

    let year = chrono::Utc::now().year();
    let tarball = input.crate_tarball.is_some();
    let crates_str = if tarball {
        "\n".to_string()
    } else {
        crates_var(input.crates)
    };
    let git_str = if tarball {
        String::new()
    } else {
        git_crates_var(input.crates, input.distdir)
    };

    let mapping = crate::license::load_mapping(input.mapping_path).unwrap_or_default();
    let pkg_license = get_pkg_license(input.pkg, &mapping).unwrap_or_default();
    let crate_licenses = if input.crate_license_spdx.is_empty() {
        String::new()
    } else {
        crate_licenses_from_spdx(input.crate_license_spdx, &mapping).unwrap_or_default()
    };

    let iuse = iuse_plus(input.pkg);
    let myfeatures = if iuse.is_empty() {
        String::new()
    } else {
        myfeatures_body(input.pkg)
    };

    let out = tmpl.render(context! {
        year => year,
        prog_version => input.prog_version,
        crates => crates_str,
        git_crates => git_str,
        description => bash_escape(&collapse_ws(&pkg_description(input.pkg))),
        homepage => input
            .pkg
            .homepage
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(url_escape),
        crate_tarball => input.crate_tarball.unwrap_or(""),
        source_tarball => input.source_tarball.unwrap_or(""),
        pkg_license => pkg_license,
        crate_licenses => crate_licenses,
        iuse_plus => iuse,
        myfeatures => myfeatures,
    })?;
    Ok(out)
}

const CARGO_FEATURES_MARKER: &str = "# Cargo features\nIUSE+=\"";
const CRATE_LICENSES_MARKER: &str = "# Dependent crate licenses\nLICENSE+=\"";
const CRATE_TARBALL_MARKER: &str = "# Crate tarball\nSRC_URI+=\"";
const SOURCE_SNAPSHOT_MARKER: &str = "# Source snapshot\nSRC_URI=\"";

fn replace_quoted(hay: &mut String, marker: &str, replacement: &str) -> Result<()> {
    let start = hay
        .find(marker)
        .ok_or_else(|| anyhow::anyhow!("missing generated marker {marker:?}"))?;
    let after = start + marker.len();
    let rel = hay[after..]
        .find('"')
        .ok_or_else(|| anyhow::anyhow!("unclosed quote after {marker:?}"))?;
    hay.replace_range(start..after + rel + 1, replacement);
    Ok(())
}

fn replace_myfeatures(hay: &mut String, body: &str) -> Result<()> {
    const START: &str = "local myfeatures=(";
    let start = hay
        .find(START)
        .ok_or_else(|| anyhow::anyhow!("missing local myfeatures=(... )"))?;
    let open = start + START.len();
    let close = hay[open..]
        .find(')')
        .ok_or_else(|| anyhow::anyhow!("unclosed myfeatures=("))?;
    hay.replace_range(open..open + close, &format!("\n{body}\n\t"));
    Ok(())
}

pub struct UpdateInput<'a> {
    pub existing: &'a str,
    pub pkg: &'a PackageMetadata,
    pub crates: &'a [Crate],
    pub crate_tarball: Option<&'a str>,
    pub source_tarball: Option<&'a str>,
    pub distdir: &'a Path,
    pub mapping_path: &'a Path,
    pub crate_license_spdx: &'a [String],
}

/// Rewrite generated bits. Maintainer `IUSE=` is left alone.
pub fn update_ebuild(input: UpdateInput<'_>) -> Result<String> {
    let mapping = crate::license::load_mapping(input.mapping_path).unwrap_or_default();
    let crate_licenses = if input.crate_license_spdx.is_empty() {
        String::new()
    } else {
        crate_licenses_from_spdx(input.crate_license_spdx, &mapping).unwrap_or_default()
    };
    let iuse = iuse_plus(input.pkg);
    let mut out = input.existing.to_string();

    if let Some(src) = input.source_tarball {
        replace_quoted(
            &mut out,
            SOURCE_SNAPSHOT_MARKER,
            &format!("{SOURCE_SNAPSHOT_MARKER}{src}\""),
        )?;
    }

    if let Some(tarball) = input.crate_tarball {
        replace_quoted(
            &mut out,
            CRATE_TARBALL_MARKER,
            &format!("{CRATE_TARBALL_MARKER} {tarball}\""),
        )?;
    } else {
        if out.contains("CRATES=\"") {
            replace_quoted(
                &mut out,
                "CRATES=\"",
                &format!("CRATES=\"{}\"", crates_var(input.crates)),
            )?;
        } else {
            anyhow::bail!("CRATES= not found");
        }
        let git_str = git_crates_var(input.crates, input.distdir);
        if out.contains("declare -A GIT_CRATES") {
            if git_str.is_empty() {
                if let Some(s) = out.find("\n\ndeclare -A GIT_CRATES")
                    && let Some(e) = out[s + 2..].find(')').map(|i| s + 2 + i + 1)
                {
                    out.replace_range(s..e, "");
                }
            } else if let Some(s) = out.find("declare -A GIT_CRATES")
                && let Some(e) = out[s..].find(')').map(|i| s + i + 1)
            {
                out.replace_range(s..e, git_str.trim());
            }
        } else if !git_str.is_empty()
            && let Some(pos) = out.find("CRATES=\"")
            && let Some(end) = out[pos..].find('"').map(|i| pos + i + 1)
        {
            out.insert_str(end, &git_str);
        }
    }

    if out.contains("# Dependent crate licenses") {
        replace_quoted(
            &mut out,
            CRATE_LICENSES_MARKER,
            &format!("{CRATE_LICENSES_MARKER}{crate_licenses}\""),
        )?;
    }

    if !iuse.is_empty() {
        replace_quoted(
            &mut out,
            CARGO_FEATURES_MARKER,
            &format!("{CARGO_FEATURES_MARKER}{iuse}\""),
        )?;
        replace_myfeatures(&mut out, &myfeatures_body(input.pkg))?;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn pkg(description: Option<&str>) -> PackageMetadata {
        PackageMetadata {
            name: "foo".to_string(),
            version: "1.0.0".to_string(),
            license: None,
            license_file: None,
            description: description.map(str::to_string),
            homepage: None,
            features: Default::default(),
            publish: true,
        }
    }

    fn pkg_with_features() -> PackageMetadata {
        let mut p = pkg(Some("does a thing"));
        p.features = BTreeMap::from([("serde".into(), true), ("json".into(), false)]);
        p
    }

    /// PMS 7.2: `DESCRIPTION` must not be empty.
    #[test]
    fn pkg_description_falls_back_when_missing() {
        assert_eq!(pkg_description(&pkg(None)), "foo Rust crate");
        assert_eq!(pkg_description(&pkg(Some("  "))), "foo Rust crate");
    }

    #[test]
    fn pkg_description_keeps_real_value() {
        assert_eq!(pkg_description(&pkg(Some("does a thing"))), "does a thing");
    }

    #[test]
    fn iuse_plus_marks_defaults() {
        assert_eq!(iuse_plus(&pkg_with_features()), " +serde json");
    }

    #[test]
    fn update_leaves_maintainer_iuse() {
        let existing = r#"CRATES="
"
IUSE="debug"
# Cargo features
IUSE+=" old"
src_configure() {
	local myfeatures=(
		$(usev old)
	)
	cargo_src_configure
}
"#;
        let out = update_ebuild(UpdateInput {
            existing,
            pkg: &pkg_with_features(),
            crates: &[],
            crate_tarball: None,
            source_tarball: None,
            distdir: Path::new("/nonexistent"),
            mapping_path: Path::new("/nonexistent"),
            crate_license_spdx: &[],
        })
        .unwrap();
        assert!(out.contains("IUSE=\"debug\""));
        assert!(out.contains("IUSE+=\" +serde json\""));
        assert!(!out.contains("IUSE+=\" old\""));
    }

    #[test]
    fn update_without_feature_marker_errors() {
        let existing = "CRATES=\"\n\"\nIUSE=\"debug\"\n";
        let err = update_ebuild(UpdateInput {
            existing,
            pkg: &pkg_with_features(),
            crates: &[],
            crate_tarball: None,
            source_tarball: None,
            distdir: Path::new("/nonexistent"),
            mapping_path: Path::new("/nonexistent"),
            crate_license_spdx: &[],
        })
        .unwrap_err();
        assert!(err.to_string().contains("Cargo features"), "{err}");
    }
}
