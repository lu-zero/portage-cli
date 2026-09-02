use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::{Result, bail};
use portage_atom::{Cpn, Cpv};
use portage_metadata::RawCacheEntry;
use portage_repo::{CacheReadOpts, Repository, cache_entries_parallel};

use crate::style::{C_BOLD, C_LABEL, C_MASKED, C_PKG, C_STAR};

/// Open every configured repo path, skipping (with a warning) any that fail
/// Errors only when no path is configured, or none opened.
fn open_repos(repo_paths: &[std::path::PathBuf]) -> Result<Vec<Repository>> {
    if repo_paths.is_empty() {
        bail!("no repositories configured");
    }
    let mut repos: Vec<Repository> = Vec::with_capacity(repo_paths.len());
    for p in repo_paths {
        match crate::repo_open::open(p) {
            Ok(r) => repos.push(r),
            Err(e) => crate::style::warn_line!("skipping {}: {e}", p.display()),
        }
    }
    if repos.is_empty() {
        bail!("no usable repositories");
    }
    Ok(repos)
}

pub async fn run(
    repo_paths: &[std::path::PathBuf],
    pattern: Option<&str>,
    all: bool,
    search_desc: bool,
    name_only: bool,
    homepage: bool,
) -> Result<()> {
    let repos = open_repos(repo_paths)?;
    let pat = pattern.unwrap_or("");

    if search_desc {
        run_desc(&repos, pat, all, name_only, homepage).await
    } else {
        run_name(&repos, pat, all, name_only, homepage);
        Ok(())
    }
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    if nb.len() > hb.len() {
        return false;
    }
    hb.windows(nb.len()).any(|w| w.eq_ignore_ascii_case(nb))
}

fn run_name(repos: &[Repository], pat: &str, all: bool, name_only: bool, homepage: bool) {
    let pat_has_slash = pat.contains('/');
    let mut matched: BTreeMap<String, (Cpn, usize)> = BTreeMap::new();
    for (idx, repo) in repos.iter().enumerate() {
        for cat in repo.categories() {
            for pkg in cat.packages() {
                let hit = if all {
                    true
                } else if pat_has_slash {
                    let full = format!("{}/{}", cat.name(), pkg.name());
                    contains_ci(&full, pat)
                } else {
                    contains_ci(pkg.name(), pat)
                };
                if hit {
                    let key = format!("{}/{}", cat.name(), pkg.name());
                    matched.entry(key).or_insert_with(|| (*pkg.cpn(), idx));
                }
            }
        }
    }

    let mut out = anstream::stdout();

    if name_only {
        for key in matched.keys() {
            writeln!(out, "{C_PKG}{key}{C_PKG:#}").ok();
        }
        return;
    }

    for (key, (cpn, idx)) in &matched {
        let info = latest_entry_info(&repos[*idx], cpn, homepage);
        if info.is_empty() {
            writeln!(out, "{C_PKG}{key}{C_PKG:#}").ok();
        } else {
            writeln!(out, "{C_PKG}{key}{C_PKG:#}: {info}").ok();
        }
    }
}

fn latest_entry_info(repo: &Repository, cpn: &Cpn, homepage: bool) -> String {
    let Some(cat) = repo.category(cpn.category.as_str()) else {
        return String::new();
    };
    let Some(pkg) = cat.package(cpn.package.as_str()) else {
        return String::new();
    };
    let Ok(ebuilds) = pkg.ebuilds() else {
        return String::new();
    };
    let Some(latest) = ebuilds.last() else {
        return String::new();
    };
    match repo.cache_entry(latest.cpv()).ok().flatten() {
        Some(entry) if homepage => entry.metadata.homepage.join(" "),
        Some(entry) => entry.metadata.description,
        None => String::new(),
    }
}

async fn run_desc(
    repos: &[Repository],
    pat: &str,
    all: bool,
    name_only: bool,
    homepage: bool,
) -> Result<()> {
    let opts = CacheReadOpts {
        latest_per_cpn: true,
        ..Default::default()
    };

    let pat_owned = pat.to_string();
    let mut entries: Vec<(Cpv, Option<String>)> =
        cache_entries_parallel(repos, &opts, move |text| {
            let raw = RawCacheEntry::new(text);
            let desc = raw.field("DESCRIPTION").unwrap_or("");
            if !all && !contains_ci(desc, &pat_owned) {
                return Ok(None);
            }
            let info = if name_only {
                None
            } else if homepage {
                Some(raw.field("HOMEPAGE").unwrap_or("").to_string())
            } else {
                Some(desc.to_string())
            };
            Ok::<_, portage_repo::Error>(Some(info))
        })
        .await
        .into_iter()
        .filter_map(|(cpv, r)| r.ok().flatten().map(|info| (cpv, info)))
        .collect();

    entries.sort_by(|(a, _), (b, _)| {
        a.cpn
            .category
            .cmp(&b.cpn.category)
            .then_with(|| a.cpn.package.cmp(&b.cpn.package))
    });

    let mut out = anstream::stdout();
    for (cpv, info) in &entries {
        let key = format!("{}/{}", cpv.cpn.category, cpv.cpn.package);
        match info {
            None => writeln!(out, "{C_PKG}{key}{C_PKG:#}").ok(),
            Some(s) => writeln!(out, "{C_PKG}{key}{C_PKG:#}: {s}").ok(),
        };
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// emerge -s / -S style search
// ---------------------------------------------------------------------------

/// Search with emerge's output format: one block per package with latest
/// available/installed versions, distfile size, homepage, description and
/// license, bracketed by the search-key header and the result count.
pub async fn run_emerge_style(
    repo_paths: &[std::path::PathBuf],
    patterns: &[String],
    search_desc: bool,
) -> Result<()> {
    let repos = open_repos(repo_paths)?;

    // Installed versions (best per cpn) from the VDB.
    let mut installed: BTreeMap<Cpn, portage_atom::Version> = BTreeMap::new();
    if let Ok(vdb) = portage_vdb::Vdb::open_default() {
        for pkg in vdb.packages() {
            let cpv = pkg.cpv();
            installed
                .entry(cpv.cpn)
                .and_modify(|v| {
                    if cpv.version > *v {
                        *v = cpv.version.clone();
                    }
                })
                .or_insert_with(|| cpv.version.clone());
        }
    }

    let arch = gentoo_core::Arch::current();
    let arch_str = arch.as_str().to_string();

    for pat in patterns {
        let mut out = anstream::stdout();
        writeln!(
            out,
            "\n[ Results for search key : {C_BOLD}{pat}{C_BOLD:#} ]"
        )
        .ok();
        writeln!(out, "Searching...\n").ok();

        // Name matches from a category walk over every repo. For repos
        // without an md5-cache (overlays), the description is matched here
        // too — the parallel cache pass below cannot see them.
        let mut matched: BTreeMap<String, (Cpn, usize)> = BTreeMap::new();
        for (idx, repo) in repos.iter().enumerate() {
            let has_cache = repo.has_primary_cache();
            for cat in repo.categories() {
                for pkg in cat.packages() {
                    let mut hit = if pat.contains('/') {
                        contains_ci(&format!("{}/{}", cat.name(), pkg.name()), pat)
                    } else {
                        contains_ci(pkg.name(), pat)
                    };
                    if !hit && search_desc && !has_cache {
                        hit = latest_visible(&repos, idx, pkg.cpn(), &arch_str)
                            .is_some_and(|(_, e, _)| contains_ci(&e.metadata.description, pat));
                    }
                    if hit {
                        matched
                            .entry(format!("{}/{}", cat.name(), pkg.name()))
                            .or_insert_with(|| (*pkg.cpn(), idx));
                    }
                }
            }
        }

        // -S: also match descriptions (md5-cache pass over all repos).
        if search_desc {
            let opts = CacheReadOpts {
                latest_per_cpn: true,
                ..Default::default()
            };
            let pat_owned = pat.to_string();
            let desc_hits = cache_entries_parallel(&repos, &opts, move |text| {
                let raw = RawCacheEntry::new(text);
                Ok::<_, portage_repo::Error>(contains_ci(
                    raw.field("DESCRIPTION").unwrap_or(""),
                    &pat_owned,
                ))
            })
            .await;
            for (cpv, hit) in desc_hits {
                if hit.unwrap_or(false) {
                    let key = format!("{}/{}", cpv.cpn.category, cpv.cpn.package);
                    matched.entry(key).or_insert((cpv.cpn, 0));
                }
            }
        }

        for (key, (cpn, idx)) in &matched {
            print_search_block(&repos, *idx, cpn, key, &installed, &arch_str);
        }
        println!("[ Applications found : {} ]\n", matched.len());
    }
    Ok(())
}

/// The latest version whose KEYWORDS accept this arch (stable or testing),
/// with its metadata and a visibility flag.
///
/// The bool is `true` iff a keyword-matching version (stable ~ or *) was
/// found for the arch; `false` means the returned entry is a fallback to the
/// newest version that had metadata (the package appears as "\[ Masked \]" in
/// emerge-style search output).
fn latest_visible(
    repos: &[Repository],
    idx: usize,
    cpn: &Cpn,
    arch: &str,
) -> Option<(portage_atom::Version, portage_metadata::CacheEntry, bool)> {
    let repo = &repos[idx];
    let cat = repo.category(cpn.category.as_str())?;
    let pkg = cat.package(cpn.package.as_str())?;
    let ebuilds = pkg.ebuilds().ok()?;
    let mut newest: Option<(portage_atom::Version, portage_metadata::CacheEntry, bool)> = None;
    for eb in ebuilds.iter().rev() {
        let Some(entry) = entry_for(repos, idx, eb) else {
            continue;
        };
        if newest.is_none() {
            newest = Some((eb.cpv().version.clone(), entry.clone(), false));
        }
        let visible = entry.metadata.keywords.iter().any(|k| {
            let k = k.to_string();
            k == arch || k == format!("~{arch}") || k == "*" || k == "~*"
        });
        if visible {
            return Some((eb.cpv().version.clone(), entry, true));
        }
    }
    newest
}

/// Metadata for one ebuild: the repo's own md5-cache, the user-side sourced
/// cache, or — for a symlinked ebuild (crossdev) — the md5-cache of the repo
/// the link resolves into.
fn entry_for(
    repos: &[Repository],
    idx: usize,
    ebuild: &portage_repo::Ebuild,
) -> Option<portage_metadata::CacheEntry> {
    let repo = &repos[idx];
    let cpv = ebuild.cpv();
    // Primary then secondary (configured at open via builder).
    if let Ok(Some(entry)) = repo.cache_entry(cpv) {
        return Some(entry);
    }
    // Symlinked ebuild: the target repo's cache entry is byte-exact.
    let real = ebuild.path().canonicalize_utf8().ok()?;
    for other in repos {
        let Ok(rel) = real.strip_prefix(other.path()) else {
            continue;
        };
        let category = rel.components().next()?.as_str();
        let cache_file = other
            .path()
            .join("metadata/md5-cache")
            .join(category)
            .join(rel.file_stem()?);
        if let Ok(text) = std::fs::read_to_string(&cache_file)
            && let Ok(entry) = portage_metadata::CacheEntry::parse(&text)
        {
            return Some(entry);
        }
    }
    None
}

fn print_search_block(
    repos: &[Repository],
    idx: usize,
    cpn: &Cpn,
    key: &str,
    installed: &BTreeMap<Cpn, portage_atom::Version>,
    arch: &str,
) {
    let latest = latest_visible(repos, idx, cpn, arch);
    let is_masked = latest.as_ref().is_some_and(|(_, _, vis)| !vis);

    let mut out = anstream::stdout();
    write!(out, "{C_STAR}*{C_STAR:#}  {C_BOLD}{key}{C_BOLD:#}").ok();
    if is_masked {
        write!(out, " {C_MASKED}[ Masked ]{C_MASKED:#}").ok();
    }
    writeln!(out).ok();

    match &latest {
        Some((ver, _, _)) => writeln!(
            out,
            "      {C_LABEL}Latest version available:{C_LABEL:#} {ver}"
        )
        .ok(),
        None => writeln!(
            out,
            "      {C_LABEL}Latest version available:{C_LABEL:#} [ Unknown ]"
        )
        .ok(),
    };
    match installed.get(cpn) {
        Some(v) => writeln!(
            out,
            "      {C_LABEL}Latest version installed:{C_LABEL:#} {v}"
        )
        .ok(),
        None => writeln!(
            out,
            "      {C_LABEL}Latest version installed:{C_LABEL:#} [ Not Installed ]"
        )
        .ok(),
    };
    if let Some((_, entry, _)) = &latest {
        let size = distfiles_size(&repos[idx], cpn, &entry.metadata);
        writeln!(
            out,
            "      {C_LABEL}Size of files:{C_LABEL:#} {} KiB",
            size.div_ceil(1024)
        )
        .ok();
        writeln!(
            out,
            "      {C_LABEL}Homepage:{C_LABEL:#}      {}",
            entry.metadata.homepage.join(" ")
        )
        .ok();
        writeln!(
            out,
            "      {C_LABEL}Description:{C_LABEL:#}   {}",
            entry.metadata.description
        )
        .ok();
        let license = entry
            .metadata
            .license
            .as_ref()
            .map(|l| l.to_string())
            .unwrap_or_default();
        writeln!(out, "      {C_LABEL}License:{C_LABEL:#}       {license}").ok();
    }
    writeln!(out).ok();
}

/// Sum of the package's `Manifest` DIST sizes for every file the latest
/// version's `SRC_URI` can reference (all USE conditionals taken, as emerge's
/// search does).
fn distfiles_size(repo: &Repository, cpn: &Cpn, meta: &portage_metadata::EbuildMetadata) -> u64 {
    let manifest_path = repo
        .path()
        .join(cpn.category.as_str())
        .join(cpn.package.as_str())
        .join("Manifest");
    let Ok(content) = std::fs::read_to_string(&manifest_path) else {
        return 0;
    };
    let Ok(manifest) = portage_repo::Manifest::parse(&content) else {
        return 0;
    };
    let sizes: BTreeMap<String, u64> = manifest
        .dist_entries()
        .filter_map(|e| match e {
            portage_repo::ManifestEntry::Dist { filename, size, .. } => {
                Some((filename.clone(), *size))
            }
            _ => None,
        })
        .collect();
    let mut filenames = Vec::new();
    for entry in meta.src_uri.list() {
        entry.collect_filenames(&|_| true, &mut filenames);
    }
    filenames.sort();
    filenames.dedup();
    filenames
        .into_iter()
        .filter_map(|f| sizes.get(&f).copied())
        .sum()
}
