use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Result, anyhow};
use portage_metadata::IUseDefault;
use portage_vdb::Vdb;

use super::ResolveMode;
use crate::vdb::find_packages;

pub fn run(repo_path: &Path, vdb: Option<&Vdb>, mode: ResolveMode, atoms: &[String]) -> Result<()> {
    let repo = crate::repo_open::open(repo_path)?;
    let use_db = repo
        .use_db()
        .unwrap_or_else(|_| portage_repo::UseDb::default());
    let ebuilds: Vec<_> = repo.ebuilds()?.into_iter().collect();
    let set = portage_repo::RepoSet::single(repo);
    let repo = set.main();

    for raw in atoms {
        let matches = super::matching_ebuilds(&set, vdb, mode, &ebuilds, raw)?;

        let Some(best) = matches.last() else {
            eprintln!("em: no ebuilds found for '{raw}'");
            continue;
        };
        let cpv = best.cpv();
        let entry = repo
            .cache_entry(cpv)?
            .ok_or_else(|| anyhow!("no cache entry for {cpv} — run `em regen`"))?;

        let xml_flags: BTreeMap<String, String> = best
            .path()
            .parent()
            .and_then(|dir| std::fs::read_to_string(dir.join("metadata.xml")).ok())
            .and_then(|xml| portage_repo::PkgMetadata::parse(&xml).ok())
            .map(|meta| meta.into_use_flags())
            .unwrap_or_default();

        let installed_flags: Option<(String, HashSet<String>)> = vdb.and_then(|v| {
            find_packages(v, &cpv.cpn.to_string())
                .into_iter()
                .max_by(|a, b| a.cpv().version.cmp(&b.cpv().version))
                .and_then(|pkg| {
                    let version = pkg.cpv().to_string();
                    pkg.use_flags()
                        .ok()
                        .map(|flags| (version, flags.iter().map(|f| f.to_string()).collect()))
                })
        });

        match &installed_flags {
            Some((installed_ver, _)) if installed_ver != &cpv.to_string() => {
                println!("[{cpv}]  (installed: {installed_ver})");
            }
            Some(_) => println!("[{cpv}]  (installed)"),
            None => println!("[{cpv}]"),
        }

        let mut flags: Vec<_> = entry.metadata.iuse.iter().collect();
        flags.sort_by_key(|f| f.name());

        let cpn = cpv.cpn.to_string();
        for flag in flags {
            let iuse_prefix = match flag.default {
                Some(IUseDefault::Enabled) => "+",
                Some(IUseDefault::Disabled) => "-",
                None => " ",
            };
            let name = flag.name();

            let installed_marker = match &installed_flags {
                Some((_, active)) if active.contains(name) => "[+]",
                Some(_) => "[-]",
                None => "   ",
            };

            let desc = use_db
                .describe(&cpn, name)
                .or_else(|| xml_flags.get(name).map(String::as_str))
                .unwrap_or("");
            println!("  {iuse_prefix}{name:<30} {installed_marker}  {desc}");
        }
    }
    Ok(())
}
