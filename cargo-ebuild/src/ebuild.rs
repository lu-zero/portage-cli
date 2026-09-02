//! Ebuild generation via `minijinja` — keeps `pycargoebuild/ebuild.py:EBUILD_TEMPLATE` verbatim.

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

fn get_crate_licenses(
    crates: &[Crate],
    distdir: &Path,
    mapping: &HashMap<String, String>,
    overrides: Option<&HashMap<String, String>>,
) -> Result<String, anyhow::Error> {
    let mut gentoo_set = BTreeSet::new();
    for krate in crates {
        let name = krate.name().to_string();
        let spdx = if let Some(ov) = overrides.and_then(|m| m.get(&name)) {
            if ov.is_empty() {
                continue;
            }
            ov.clone()
        } else if let Some(lic) = crate::cargo::license_from_crate(krate, distdir) {
            lic
        } else {
            continue;
        };
        let gentoo = crate::license::spdx_to_ebuild(&spdx, mapping)?;
        gentoo_set.insert(gentoo);
    }
    if gentoo_set.is_empty() {
        return Ok(String::new());
    }
    // Gentoo crate licenses are AND of all distinct Gentoo groups — portage_metadata handles dedup via LicenseExpr
    // Keep each group's original formatting (e.g., "|| ( MIT Apache-2.0 )" stays grouped)
    let combined_gentoo = gentoo_set.iter().cloned().collect::<Vec<_>>().join(" ");
    // Validate and dedup via LicenseExpr
    let parsed = portage_metadata::LicenseExpr::parse(&combined_gentoo)?;
    let deduped = parsed.dedup().to_string();
    let mut s = crate::license::format_license_var(&deduped, "LICENSE+=\" ");
    if !s.starts_with('\n') && !s.is_empty() {
        s = format!(" {s}");
    }
    Ok(s)
}

#[allow(clippy::too_many_arguments)]
pub fn render_ebuild_with_distdir(
    pkg: &PackageMetadata,
    crates: &[Crate],
    crate_tarball: Option<&str>,
    prog_version: &str,
    distdir: &Path,
    mapping_path: &Path,
    license_overrides: Option<&HashMap<String, String>>,
    include_crate_license: bool,
    use_features: bool,
) -> Result<String> {
    let mut env = Environment::new();
    env.set_trim_blocks(true);
    env.set_keep_trailing_newline(true);
    env.add_template("ebuild", TEMPLATE)?;
    let tmpl = env.get_template("ebuild")?;

    let year = chrono::Utc::now().year();
    let crates_str = if crate_tarball.is_some() {
        "\n".to_string()
    } else {
        crates_var(crates)
    };
    let git_str = git_crates_var(crates, distdir);

    let mapping = crate::license::load_mapping(mapping_path).unwrap_or_default();
    let pkg_license = get_pkg_license(pkg, &mapping).unwrap_or_default();
    let crate_licenses = if include_crate_license {
        get_crate_licenses(crates, distdir, &mapping, license_overrides).unwrap_or_default()
    } else {
        String::new()
    };

    let pkg_features = if !use_features || pkg.features.is_empty() {
        None
    } else {
        let mut v: Vec<String> = pkg
            .features
            .iter()
            .map(|(k, d)| if *d { format!("+{k}") } else { k.clone() })
            .collect();
        v.sort();
        Some(v.join(" "))
    };
    let pkg_features_use = pkg_features.as_ref().map(|_| {
        let mut v: Vec<String> = pkg.features.keys().cloned().collect();
        v.sort();
        v.iter()
            .map(|f| format!("\t\t$(usev {f})"))
            .collect::<Vec<_>>()
            .join("\n")
    });

    let out = tmpl.render(context! {
        year => year,
        prog_version => prog_version,
        crates => crates_str,
        git_crates => git_str,
        description => bash_escape(&collapse_ws(&pkg_description(pkg))),
        homepage => url_escape(pkg.homepage.as_deref().unwrap_or("")),
        crate_tarball => crate_tarball.unwrap_or(""),
        pkg_license => pkg_license,
        crate_licenses => crate_licenses,
        pkg_features => pkg_features,
        pkg_features_use => pkg_features_use,
    })?;
    Ok(out)
}

/// Update existing ebuild — `CRATES=` + `GIT_CRATES` + `# Dependent crate licenses` like `pycargoebuild/ebuild.py:update_ebuild`
#[allow(clippy::too_many_arguments)]
pub fn update_ebuild_with_distdir(
    existing: &str,
    _pkg: &PackageMetadata,
    crates: &[Crate],
    distdir: &Path,
    mapping_path: &Path,
    license_overrides: Option<&HashMap<String, String>>,
    include_crate_license: bool,
    crate_tarball: Option<&str>,
) -> Result<String> {
    let crates_str = if crate_tarball.is_some() {
        "\n".to_string()
    } else {
        crates_var(crates)
    };
    let git_str = git_crates_var(crates, distdir);
    let mapping = crate::license::load_mapping(mapping_path).unwrap_or_default();
    let crate_licenses = if include_crate_license {
        get_crate_licenses(crates, distdir, &mapping, license_overrides).unwrap_or_default()
    } else {
        String::new()
    };

    let mut out = existing.to_string();
    // CRATES — simple, handles both " and '
    if let Some(start) = out.find("CRATES=\"") {
        if let Some(end) = out[start + 8..].find('"').map(|i| start + 8 + i + 1) {
            out.replace_range(start..end, &format!("CRATES=\"{}\"", crates_str));
        }
    } else if let Some(start) = out.find("CRATES='") {
        if let Some(end) = out[start + 8..].find('\'').map(|i| start + 8 + i + 1) {
            out.replace_range(start..end, &format!("CRATES='{}'", crates_str));
        }
    } else {
        anyhow::bail!("CRATES= not found");
    }

    // GIT_CRATES — replace or append/remove
    if out.contains("declare -A GIT_CRATES") {
        if git_str.is_empty() {
            if let Some(s) = out.find("\n\ndeclare -A GIT_CRATES")
                && let Some(e) = out[s + 2..].find(')').map(|i| s + 2 + i + 1)
            {
                out.replace_range(s..e, "");
            } else if let Some(s) = out.find("declare -A GIT_CRATES")
                && let Some(e) = out[s..].find(')').map(|i| s + i + 1)
            {
                out.replace_range(s..e, "");
            }
        } else if let Some(s) = out.find("declare -A GIT_CRATES")
            && let Some(e) = out[s..].find(')').map(|i| s + i + 1)
        {
            out.replace_range(s..e, git_str.trim());
        }
    } else if !git_str.is_empty() {
        if let Some(pos) = out.find("CRATES=\"")
            && let Some(end) = out[pos..].find('"').map(|i| pos + i + 1)
        {
            out.insert_str(end, &git_str);
        } else if let Some(pos) = out.find("CRATES='")
            && let Some(end) = out[pos..].find('\'').map(|i| pos + i + 1)
        {
            out.insert_str(end, &git_str);
        }
    }

    // LICENSE — only if present in original
    if out.contains("# Dependent crate licenses") {
        let marker = "# Dependent crate licenses\nLICENSE+=\"";
        if let Some(start) = out.find("# Dependent crate licenses\nLICENSE+=\"") {
            if let Some(end) = out[start + marker.len()..]
                .find('"')
                .map(|i| start + marker.len() + i + 1)
            {
                if include_crate_license {
                    out.replace_range(
                        start..end,
                        &format!(
                            "# Dependent crate licenses\nLICENSE+=\"{}\"",
                            crate_licenses
                        ),
                    );
                } else {
                    // keep as is or remove? pycargoebuild expects 0 matches when --no-license, but template still has it
                    out.replace_range(
                        start..end,
                        &format!(
                            "# Dependent crate licenses\nLICENSE+=\"{}\"",
                            crate_licenses
                        ),
                    );
                }
            }
        } else if let Some(start) = out.find("# Dependent crate licenses\nLICENSE+='") {
            let marker2 = "# Dependent crate licenses\nLICENSE+='";
            if let Some(end) = out[start + marker2.len()..]
                .find('\'')
                .map(|i| start + marker2.len() + i + 1)
            {
                out.replace_range(
                    start..end,
                    &format!("# Dependent crate licenses\nLICENSE+='{}'", crate_licenses),
                );
            }
        }
    } else if include_crate_license && !crate_licenses.is_empty() {
        // If no marker but we have licenses, we don't add — pycargoebuild expects marker to exist for update
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(description: Option<&str>) -> PackageMetadata {
        PackageMetadata {
            name: "foo".to_string(),
            version: "1.0.0".to_string(),
            license: None,
            license_file: None,
            description: description.map(str::to_string),
            homepage: None,
            features: Default::default(),
        }
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
}
