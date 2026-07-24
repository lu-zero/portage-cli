use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use anstyle::Style;
use anyhow::Result;
use portage_metadata::Stability;
use portage_repo::Repository;
use portage_vdb::Vdb;

use super::ResolveMode;

use crate::style::{C_DISABLED, C_PKG, C_STABLE, C_TESTING};

pub fn run(repo_path: &Path, vdb: Option<&Vdb>, mode: ResolveMode, atoms: &[String]) -> Result<()> {
    let repo = Repository::open(repo_path)?;

    let ebuilds: Vec<_> = repo.ebuilds()?.into_iter().collect();

    for raw in atoms {
        let matches = super::matching_ebuilds(&repo, vdb, mode, &ebuilds, raw)?;

        if matches.is_empty() {
            eprintln!("em: no ebuilds found for '{raw}'");
            continue;
        }

        let mut all_arches: std::collections::BTreeSet<String> = Default::default();
        let mut version_keywords: Vec<(String, BTreeMap<String, Stability>)> = Vec::new();

        for ebuild in &matches {
            let cpv = ebuild.cpv();
            let mut kw_map: BTreeMap<String, Stability> = BTreeMap::new();
            if let Ok(Some(entry)) = repo.cache_entry(cpv) {
                for kw in &entry.metadata.keywords {
                    let arch = kw.arch.as_str().to_owned();
                    if arch != "*" {
                        all_arches.insert(arch.clone());
                        kw_map.insert(arch, kw.stability);
                    }
                }
            }
            version_keywords.push((cpv.to_string(), kw_map));
        }

        let arches: Vec<String> = all_arches.into_iter().collect();
        let col_w = arches.iter().map(|a| a.len()).max().unwrap_or(4).max(4);
        let ver_w = version_keywords
            .iter()
            .map(|(v, _)| v.len())
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

        for (version, kw_map) in &version_keywords {
            writeln!(out, "{C_PKG}{:<ver_w$}{C_PKG:#}", version).ok();
            for arch in &arches {
                let (sym, style) = match kw_map.get(arch) {
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
    Ok(())
}
