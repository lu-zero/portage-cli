//! `em query meta` — display package metadata from repo + VDB

use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use humansize::{BINARY, format_size};
use portage_vdb::Vdb;

use super::ResolveMode;
use crate::cli::OutputFormat;
use crate::style::{C_LABEL, C_PKG};
use crate::vdb::find_packages;

/// One installed copy's own details, distinct from the ebuild's own
/// (possibly newer) metadata above it.
struct InstalledMeta {
    version: String,
    slot: Option<String>,
    repo: Option<String>,
    built: Option<String>,
    size: Option<u64>,
    use_flags: Vec<String>,
}

/// Everything `em query meta` knows about one resolved atom: the best
/// ebuild's own metadata, plus every installed copy's own details.
struct Meta {
    cpv: String,
    maintainers: Vec<String>,
    homepage: Vec<String>,
    description: String,
    longdescription: Option<String>,
    license: Option<String>,
    slot: String,
    keywords: Vec<String>,
    installed: Vec<InstalledMeta>,
}

fn collect_meta(
    repo: &portage_repo::Repository,
    vdb: Option<&Vdb>,
    best: &portage_repo::Ebuild,
) -> Result<Meta> {
    let cpv = best.cpv();
    let entry = repo
        .cache_entry(cpv)?
        .ok_or_else(|| anyhow!("no cache entry for {cpv} — run `em regen`"))?;
    let m = &entry.metadata;

    let pkg_meta = repo
        .category(cpv.cpn.category.as_ref())
        .and_then(|c| {
            c.packages()
                .into_iter()
                .find(|p| p.name() == cpv.cpn.package.as_ref())
        })
        .and_then(|p| p.metadata_xml().ok().flatten());

    let maintainers = pkg_meta
        .as_ref()
        .map(|pm| pm.maintainers.iter().map(|m| m.display()).collect())
        .unwrap_or_default();
    let longdescription = pkg_meta.as_ref().and_then(|pm| pm.longdescription.clone());

    let installed = match vdb {
        Some(vdb) => find_packages(vdb, &cpv.cpn.to_string())
            .into_iter()
            .map(|pkg| InstalledMeta {
                version: pkg.cpv().version.to_string(),
                slot: pkg.slot().ok().map(|s| s.to_string()),
                repo: pkg.repository().ok().flatten().map(|r| r.to_string()),
                built: pkg.build_time().ok().flatten().map(|ts| {
                    humantime::format_rfc3339_seconds(UNIX_EPOCH + Duration::from_secs(ts))
                        .to_string()
                }),
                size: pkg.size().ok().flatten(),
                use_flags: pkg
                    .use_flags()
                    .ok()
                    .map(|flags| flags.iter().map(|f| f.to_string()).collect())
                    .unwrap_or_default(),
            })
            .collect(),
        None => Vec::new(),
    };

    Ok(Meta {
        cpv: cpv.to_string(),
        maintainers,
        homepage: m.homepage.iter().map(|h| h.to_string()).collect(),
        description: m.description.clone(),
        longdescription,
        license: m.license.as_ref().map(|l| l.to_string()),
        slot: m.slot.to_string(),
        keywords: m.keywords.iter().map(|k| k.to_string()).collect(),
        installed,
    })
}

fn print_pretty(meta: &Meta) {
    let mut out = anstream::stdout();
    writeln!(out, " {C_PKG}*{C_PKG:#} {C_PKG}{}{C_PKG:#}", meta.cpv).ok();

    for maint in &meta.maintainers {
        writeln!(out, "   {C_LABEL}Maintainer:{C_LABEL:#}  {maint}").ok();
    }
    if !meta.homepage.is_empty() {
        writeln!(
            out,
            "   {C_LABEL}Homepage:{C_LABEL:#}    {}",
            meta.homepage.join(" ")
        )
        .ok();
    }
    writeln!(
        out,
        "   {C_LABEL}Description:{C_LABEL:#} {}",
        meta.description
    )
    .ok();
    if let Some(ld) = &meta.longdescription {
        for line in wrap(ld, 72) {
            writeln!(out, "                {line}").ok();
        }
    }
    if let Some(lic) = &meta.license {
        writeln!(out, "   {C_LABEL}License:{C_LABEL:#}     {lic}").ok();
    }
    writeln!(out, "   {C_LABEL}Slot:{C_LABEL:#}        {}", meta.slot).ok();
    if !meta.keywords.is_empty() {
        writeln!(
            out,
            "   {C_LABEL}Keywords:{C_LABEL:#}    {}",
            meta.keywords.join(" ")
        )
        .ok();
    }

    if !meta.installed.is_empty() {
        writeln!(out, "   {C_LABEL}Installed:{C_LABEL:#}").ok();
        for pkg in &meta.installed {
            writeln!(out, "     {C_LABEL}Version:{C_LABEL:#}   {}", pkg.version).ok();
            if let Some(slot) = &pkg.slot {
                writeln!(out, "     {C_LABEL}Slot:{C_LABEL:#}      {slot}").ok();
            }
            if let Some(repo) = &pkg.repo {
                writeln!(out, "     {C_LABEL}Repo:{C_LABEL:#}      {repo}").ok();
            }
            if let Some(built) = &pkg.built {
                writeln!(out, "     {C_LABEL}Built:{C_LABEL:#}     {built}").ok();
            }
            if let Some(bytes) = pkg.size {
                writeln!(
                    out,
                    "     {C_LABEL}Size:{C_LABEL:#}      {}",
                    format_size(bytes, BINARY)
                )
                .ok();
            }
            if !pkg.use_flags.is_empty() {
                writeln!(
                    out,
                    "     {C_LABEL}USE:{C_LABEL:#}       {}",
                    pkg.use_flags.join(" ")
                )
                .ok();
            }
        }
    }
    writeln!(out).ok();
}

fn meta_to_json(meta: &Meta) -> serde_json::Value {
    let installed: Vec<serde_json::Value> = meta
        .installed
        .iter()
        .map(|pkg| {
            serde_json::json!({
                "version": pkg.version,
                "slot": pkg.slot,
                "repo": pkg.repo,
                "built": pkg.built,
                "size": pkg.size,
                "use": pkg.use_flags,
            })
        })
        .collect();
    serde_json::json!({
        "cpv": meta.cpv,
        "maintainers": meta.maintainers,
        "homepage": meta.homepage,
        "description": meta.description,
        "longdescription": meta.longdescription,
        "license": meta.license,
        "slot": meta.slot,
        "keywords": meta.keywords,
        "installed": installed,
    })
}

pub fn run(
    repo_path: &Path,
    vdb: Option<&Vdb>,
    mode: ResolveMode,
    atoms: &[String],
    format: OutputFormat,
) -> Result<()> {
    let repo = crate::repo_open::open(repo_path)?;
    let ebuilds: Vec<_> = repo.ebuilds()?.into_iter().collect();
    let set = portage_repo::RepoSet::single(repo);
    let repo = set.main();

    let mut json_results: Vec<serde_json::Value> = Vec::new();

    for raw in atoms {
        let matches = super::matching_ebuilds(&set, vdb, mode, &ebuilds, raw)?;

        let Some(best) = matches.last() else {
            crate::style::warn_line!("no ebuild found for '{raw}'");
            continue;
        };

        let meta = collect_meta(repo, vdb, best)?;
        match format {
            OutputFormat::Pretty => print_pretty(&meta),
            OutputFormat::Json => json_results.push(meta_to_json(&meta)),
        }
    }

    if matches!(format, OutputFormat::Json) {
        let json = serde_json::to_string_pretty(&json_results)
            .map_err(|e| anyhow::anyhow!("failed to serialize JSON output: {e}"))?;
        println!("{json}");
    }
    Ok(())
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current.clone());
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_to_json_shape() {
        let meta = Meta {
            cpv: "dev-python/setuptools-83.0.0".to_string(),
            maintainers: vec!["python@gentoo.org".to_string()],
            homepage: vec!["https://example.org".to_string()],
            description: "A packaging tool".to_string(),
            longdescription: None,
            license: Some("MIT".to_string()),
            slot: "0".to_string(),
            keywords: vec!["amd64".to_string(), "~arm64".to_string()],
            installed: vec![InstalledMeta {
                version: "83.0.0".to_string(),
                slot: Some("0".to_string()),
                repo: Some("gentoo".to_string()),
                built: Some("2026-01-01T00:00:00Z".to_string()),
                size: Some(12345),
                use_flags: vec!["python".to_string()],
            }],
        };
        let json = meta_to_json(&meta);
        assert_eq!(json["cpv"], "dev-python/setuptools-83.0.0");
        assert_eq!(json["maintainers"][0], "python@gentoo.org");
        assert_eq!(json["license"], "MIT");
        assert_eq!(json["installed"][0]["version"], "83.0.0");
        assert_eq!(json["installed"][0]["use"][0], "python");
    }

    #[test]
    fn meta_to_json_absent_fields_are_null_not_missing() {
        let meta = Meta {
            cpv: "dev-python/setuptools-83.0.0".to_string(),
            maintainers: vec![],
            homepage: vec![],
            description: String::new(),
            longdescription: None,
            license: None,
            slot: "0".to_string(),
            keywords: vec![],
            installed: vec![],
        };
        let json = meta_to_json(&meta);
        assert!(json["longdescription"].is_null());
        assert!(json["license"].is_null());
        assert_eq!(json["installed"], serde_json::json!([]));
    }

    #[test]
    fn wrap_respects_width() {
        let lines = wrap("one two three four five", 8);
        for line in &lines {
            assert!(line.len() <= 8, "{line:?} exceeds width");
        }
        assert_eq!(lines.join(" "), "one two three four five");
    }
}
