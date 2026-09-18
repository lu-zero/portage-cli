use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use anstyle::Style;
use anyhow::Result;
use portage_metadata::Stability;
use portage_vdb::Vdb;

use super::ResolveMode;

use crate::cli::OutputFormat;
use crate::style::{C_DISABLED, C_PKG, C_STABLE, C_TESTING};

/// One version's keyword matrix row: every arch it has a real (non-`*`)
/// keyword for, mapped to that keyword's stability.
struct VersionKeywords {
    version: String,
    by_arch: BTreeMap<String, Stability>,
}

/// Every version's keyword row for one resolved atom, and the full arch set
/// seen across all of them (a version missing an arch just has no entry —
/// "unkeyworded there", not `Disabled`).
fn collect_keywords(
    repo: &portage_repo::Repository,
    matches: &[&portage_repo::Ebuild],
) -> Vec<VersionKeywords> {
    matches
        .iter()
        .map(|ebuild| {
            let cpv = ebuild.cpv();
            let mut by_arch: BTreeMap<String, Stability> = BTreeMap::new();
            if let Ok(Some(entry)) = repo.cache_entry(cpv) {
                for kw in &entry.metadata.keywords {
                    let arch = kw.arch.as_str().to_owned();
                    if arch != "*" {
                        by_arch.insert(arch, kw.stability);
                    }
                }
            }
            VersionKeywords {
                version: cpv.to_string(),
                by_arch,
            }
        })
        .collect()
}

fn stability_str(s: Stability) -> &'static str {
    match s {
        Stability::Stable => "stable",
        Stability::Testing => "testing",
        Stability::Disabled => "disabled",
        Stability::DisabledAll => "disabled_all",
    }
}

fn print_pretty(rows: &[VersionKeywords]) {
    let mut all_arches: std::collections::BTreeSet<&str> = Default::default();
    for row in rows {
        all_arches.extend(row.by_arch.keys().map(String::as_str));
    }
    let arches: Vec<&str> = all_arches.into_iter().collect();
    let col_w = arches.iter().map(|a| a.len()).max().unwrap_or(4).max(4);
    let ver_w = rows
        .iter()
        .map(|r| r.version.len())
        .max()
        .unwrap_or(7)
        .max(7);

    let mut out = anstream::stdout();
    writeln!(out, "{C_PKG}{:<ver_w$}{C_PKG:#}", "version").ok();
    for arch in &arches {
        write!(out, "  {:>col_w$}", arch).ok();
    }
    writeln!(out).ok();

    write!(out, "{}", "-".repeat(ver_w)).ok();
    for _ in &arches {
        write!(out, "  {}", "-".repeat(col_w)).ok();
    }
    writeln!(out).ok();

    for row in rows {
        writeln!(out, "{C_PKG}{:<ver_w$}{C_PKG:#}", row.version).ok();
        for arch in &arches {
            let (sym, style) = match row.by_arch.get(*arch) {
                Some(Stability::Stable) => ("+", C_STABLE),
                Some(Stability::Testing) => ("~", C_TESTING),
                Some(Stability::Disabled) => ("-", C_DISABLED),
                Some(Stability::DisabledAll) => ("*", C_DISABLED),
                None => (" ", Style::new()),
            };
            write!(out, "  {style}{:>col_w$}{style:#}", sym).ok();
        }
        writeln!(out).ok();
    }
}

fn rows_to_json(atom: &str, rows: &[VersionKeywords]) -> serde_json::Value {
    let versions: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let keywords: serde_json::Map<String, serde_json::Value> = row
                .by_arch
                .iter()
                .map(|(arch, stability)| (arch.clone(), stability_str(*stability).into()))
                .collect();
            serde_json::json!({
                "version": row.version,
                "keywords": keywords,
            })
        })
        .collect();
    serde_json::json!({ "atom": atom, "versions": versions })
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

        if matches.is_empty() {
            crate::style::warn_line!("no ebuilds found for '{raw}'");
            continue;
        }

        let rows = collect_keywords(repo, &matches);
        match format {
            OutputFormat::Pretty => print_pretty(&rows),
            OutputFormat::Json => json_results.push(rows_to_json(raw, &rows)),
        }
    }

    if matches!(format, OutputFormat::Json) {
        let json = serde_json::to_string_pretty(&json_results)
            .map_err(|e| anyhow::anyhow!("failed to serialize JSON output: {e}"))?;
        println!("{json}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_to_json_shape() {
        let rows = vec![
            VersionKeywords {
                version: "dev-python/setuptools-79.0.1".to_string(),
                by_arch: BTreeMap::from([
                    ("amd64".to_string(), Stability::Stable),
                    ("arm64".to_string(), Stability::Testing),
                ]),
            },
            VersionKeywords {
                version: "dev-python/setuptools-83.0.0".to_string(),
                by_arch: BTreeMap::new(),
            },
        ];
        let json = rows_to_json("dev-python/setuptools", &rows);
        assert_eq!(json["atom"], "dev-python/setuptools");
        assert_eq!(
            json["versions"][0]["version"],
            "dev-python/setuptools-79.0.1"
        );
        assert_eq!(json["versions"][0]["keywords"]["amd64"], "stable");
        assert_eq!(json["versions"][0]["keywords"]["arm64"], "testing");
        // A version with no keywords at all gets an empty object, not an
        // absent field — scripts can rely on `keywords` always being there.
        assert_eq!(json["versions"][1]["keywords"], serde_json::json!({}));
    }

    #[test]
    fn stability_str_covers_every_variant() {
        assert_eq!(stability_str(Stability::Stable), "stable");
        assert_eq!(stability_str(Stability::Testing), "testing");
        assert_eq!(stability_str(Stability::Disabled), "disabled");
        assert_eq!(stability_str(Stability::DisabledAll), "disabled_all");
    }
}
