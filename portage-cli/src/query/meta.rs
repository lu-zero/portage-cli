//! `em query meta` — display package metadata from repo + VDB.

use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use humansize::{BINARY, format_size};
use portage_vdb::Vdb;

use super::ResolveMode;
use crate::style::{C_LABEL, C_PKG};
use crate::vdb::find_packages;

pub fn run(repo_path: &Path, vdb: Option<&Vdb>, mode: ResolveMode, atoms: &[String]) -> Result<()> {
    let repo = crate::repo_open::open(repo_path)?;
    let ebuilds: Vec<_> = repo.ebuilds()?.into_iter().collect();
    let set = portage_repo::RepoSet::single(repo);
    let repo = set.main();

    for raw in atoms {
        let matches = super::matching_ebuilds(&set, vdb, mode, &ebuilds, raw)?;

        let Some(best) = matches.last() else {
            eprintln!("em: no ebuild found for '{raw}'");
            continue;
        };
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

        let mut out = anstream::stdout();
        writeln!(out, " {C_PKG}*{C_PKG:#} {C_PKG}{cpv}{C_PKG:#}").ok();

        if let Some(ref pm) = pkg_meta {
            for maint in &pm.maintainers {
                writeln!(
                    out,
                    "   {C_LABEL}Maintainer:{C_LABEL:#}  {}",
                    maint.display()
                )
                .ok();
            }
        }

        if !m.homepage.is_empty() {
            writeln!(
                out,
                "   {C_LABEL}Homepage:{C_LABEL:#}    {}",
                m.homepage.join(" ")
            )
            .ok();
        }

        writeln!(out, "   {C_LABEL}Description:{C_LABEL:#} {}", m.description).ok();

        if let Some(ref pm) = pkg_meta
            && let Some(ref ld) = pm.longdescription
        {
            for line in wrap(ld, 72) {
                writeln!(out, "                {line}").ok();
            }
        }

        if let Some(ref lic) = m.license {
            writeln!(out, "   {C_LABEL}License:{C_LABEL:#}     {lic}").ok();
        }

        writeln!(out, "   {C_LABEL}Slot:{C_LABEL:#}        {}", m.slot).ok();

        if !m.keywords.is_empty() {
            let kws: Vec<String> = m.keywords.iter().map(|k| k.to_string()).collect();
            writeln!(out, "   {C_LABEL}Keywords:{C_LABEL:#}    {}", kws.join(" ")).ok();
        }

        if let Some(vdb) = vdb {
            let installed = find_packages(vdb, &cpv.cpn.to_string());
            if !installed.is_empty() {
                writeln!(out, "   {C_LABEL}Installed:{C_LABEL:#}").ok();
                for pkg in &installed {
                    writeln!(
                        out,
                        "     {C_LABEL}Version:{C_LABEL:#}   {}",
                        pkg.cpv().version
                    )
                    .ok();
                    if let Ok(slot) = pkg.slot() {
                        writeln!(out, "     {C_LABEL}Slot:{C_LABEL:#}      {slot}").ok();
                    }
                    if let Ok(repo_name) = pkg.repository()
                        && let Some(r) = repo_name
                    {
                        writeln!(out, "     {C_LABEL}Repo:{C_LABEL:#}      {r}").ok();
                    }
                    if let Ok(Some(ts)) = pkg.build_time() {
                        let t = UNIX_EPOCH + Duration::from_secs(ts);
                        writeln!(
                            out,
                            "     {C_LABEL}Built:{C_LABEL:#}     {}",
                            humantime::format_rfc3339_seconds(t)
                        )
                        .ok();
                    }
                    if let Ok(Some(bytes)) = pkg.size() {
                        writeln!(
                            out,
                            "     {C_LABEL}Size:{C_LABEL:#}      {}",
                            format_size(bytes, BINARY)
                        )
                        .ok();
                    }
                    if let Ok(flags) = pkg.use_flags()
                        && !flags.is_empty()
                    {
                        writeln!(
                            out,
                            "     {C_LABEL}USE:{C_LABEL:#}       {}",
                            flags.join(" ")
                        )
                        .ok();
                    }
                }
            }
        }

        writeln!(out).ok();
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
