use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;

use anyhow::{Result, bail};
use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv};
use portage_atom_pubgrub::{PortagePackage, UseFlagState};
use portage_repo::{Repository, ebuild_entries, repo_entries};
use portage_resolve::effective_use::effective_use;
use portage_resolve::repo::{ResolvedPolicy, is_masked};

use crate::search_key::{KeyOptions, SearchKey};
use crate::style::{C_BOLD, C_LABEL, C_MASKED, C_PKG, C_STAR};

/// Open every configured repo path, skipping (with a warning) any that fail
/// Errors only when no path is configured, or none opened.
fn open_repos(repo_paths: &[std::path::PathBuf]) -> Result<Vec<Repository>> {
    if repo_paths.is_empty() {
        bail!("no repositories configured");
    }
    let mut repos: Vec<Repository> = Vec::with_capacity(repo_paths.len());
    // Masters as the resolver resolves them (named under the main repository's parent), so an
    // overlay without md5-cache can be sourced against their eclasses and categories.
    let repos_dir = repo_paths[0]
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    for p in repo_paths {
        let opened = crate::repo_open::open_with_masters(p, &repos_dir, None, None)
            .or_else(|_| crate::repo_open::open(p));
        match opened {
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
    search_index: bool,
) -> Result<()> {
    let repos = open_repos(repo_paths)?;
    let pat = pattern.unwrap_or("");

    if search_desc {
        // The index holds descriptions only.
        let (indexed, rest) = split_indexed(&repos, search_index && !homepage);
        run_desc(&indexed, &rest, pat, all, name_only, homepage).await
    } else {
        run_name(&repos, pat, all, name_only, homepage);
        Ok(())
    }
}

/// The categories of `repo` to walk: every category any configured repository lists, as
/// Portage searches the union, so an overlay may keep a package in a category only its master
/// lists in `profiles/categories`
fn walk_categories<'a>(
    repo: &'a Repository,
    names: &'a [String],
) -> impl Iterator<Item = portage_repo::Category> + 'a {
    names.iter().filter_map(|name| repo.category(name))
}

/// Every category name any of `repos` lists in `profiles/categories`, sorted
fn category_names(repos: &[Repository]) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    for repo in repos {
        names.extend(repo.categories().into_iter().map(|c| c.name().to_string()));
    }
    names.into_iter().collect()
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
    let names = category_names(repos);
    let mut matched: BTreeMap<String, (Cpn, usize)> = BTreeMap::new();
    for (idx, repo) in repos.iter().enumerate() {
        for cat in walk_categories(repo, &names) {
            for name in cat.package_names() {
                let hit = if all {
                    true
                } else if pat_has_slash {
                    contains_ci(&format!("{}/{name}", cat.name()), pat)
                } else {
                    contains_ci(&name, pat)
                };
                if hit && let Some(pkg) = cat.package(&name) {
                    let key = format!("{}/{name}", cat.name());
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

/// The newest version of each package in a repository's `metadata/pkg_desc_index`, with its
/// description
type DescIndex = Vec<(Cpv, String)>;

/// The repository's [`DescIndex`]; `None` when it has none
fn desc_index(repo: &Repository) -> Option<DescIndex> {
    let (_, text) = repo
        .file("metadata/pkg_desc_index")
        .texts()
        .into_iter()
        .next()?;
    Some(
        text.lines()
            .filter_map(|line| {
                // `cat/pkg ver ver...: description`, versions in ascending order
                let (head, desc) = line.split_once(':')?;
                let mut words = head.split_whitespace();
                let name = words.next()?;
                let cpv = Cpv::parse(&format!("{name}-{}", words.next_back()?)).ok()?;
                Some((cpv, desc.trim_start().to_string()))
            })
            .collect(),
    )
}

/// One package's newest version across the repositories, with what a description search shows
struct Described {
    cpv: Cpv,
    /// Its repository's position, for an index; the first repository for an md5-cache entry
    repo: usize,
    description: String,
    homepage: Option<String>,
}

/// The newest version of every package, from the indexes and the md5-caches of `rest`; on equal
/// versions the earlier repository wins
async fn described(
    indexed: &[(usize, DescIndex)],
    rest: &[(usize, Repository)],
    homepage: bool,
) -> Result<Vec<Described>> {
    // `repo_entries` checks each cache entry against its ebuild, so a stale or missing one is sourced afresh.
    let mut from_repos = Vec::new();
    for (position, repository) in rest {
        from_repos.extend(sourced(repository, *position, homepage).await);
    }
    let from_index = indexed.iter().flat_map(|(repo, index)| {
        index.iter().map(|(cpv, description)| Described {
            cpv: cpv.clone(),
            repo: *repo,
            description: description.clone(),
            homepage: None,
        })
    });
    Ok(newest_per_package(from_index.chain(from_repos)))
}

/// The newest version of each package among `found`; on equal versions the earlier repository wins
fn newest_per_package(found: impl IntoIterator<Item = Described>) -> Vec<Described> {
    let mut best: std::collections::HashMap<Cpn, Described> = std::collections::HashMap::new();
    for d in found {
        match best.get(&d.cpv.cpn) {
            Some(prev)
                if prev.cpv.version > d.cpv.version
                    || (prev.cpv.version == d.cpv.version && prev.repo <= d.repo) => {}
            _ => {
                best.insert(d.cpv.cpn, d);
            }
        }
    }
    best.into_values().collect()
}

/// The descriptions of a repository with no md5-cache, from its user-side cache or by sourcing
/// its ebuilds (what emerge's `aux_get` does for such an overlay)
async fn sourced(repo: &Repository, position: usize, homepage: bool) -> Vec<Described> {
    let entries = repo_entries(repo).await;
    newest_per_package(entries.into_iter().map(|(cpv, entry)| Described {
        cpv,
        repo: position,
        homepage: homepage.then(|| entry.metadata.homepage.join(" ")),
        description: entry.metadata.description,
    }))
}

/// Repositories by position, with their description index
type Indexed = Vec<(usize, DescIndex)>;
/// Repositories by position, whose md5-cache has to be read
type Scanned = Vec<(usize, Repository)>;

/// The descriptions of the repositories with an index, and the repositories whose md5-cache has
/// to be read instead, each by its position in `repos`
fn split_indexed(repos: &[Repository], use_index: bool) -> (Indexed, Scanned) {
    let mut indexed = Vec::new();
    let mut rest = Vec::new();
    for (idx, repo) in repos.iter().enumerate() {
        match use_index.then(|| desc_index(repo)).flatten() {
            Some(index) => indexed.push((idx, index)),
            None => rest.push((idx, repo.clone())),
        }
    }
    (indexed, rest)
}

async fn run_desc(
    indexed: &[(usize, DescIndex)],
    repos: &[(usize, Repository)],
    pat: &str,
    all: bool,
    name_only: bool,
    homepage: bool,
) -> Result<()> {
    let mut entries: Vec<(Cpn, Option<String>)> = described(indexed, repos, homepage)
        .await?
        .into_iter()
        .filter(|d| all || contains_ci(&d.description, pat))
        .map(|d| {
            let info = if name_only {
                None
            } else {
                Some(d.homepage.unwrap_or(d.description))
            };
            (d.cpv.cpn, info)
        })
        .collect();

    // By name: interned values order by interning sequence, which the parallel read makes vary.
    entries.sort_by(|(a, _), (b, _)| {
        (a.category.as_str(), a.package.as_str()).cmp(&(b.category.as_str(), b.package.as_str()))
    });

    let mut out = anstream::stdout();
    for (cpn, info) in &entries {
        let key = format!("{}/{}", cpn.category, cpn.package);
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
    search_index: bool,
    options: KeyOptions,
    policy: Option<&ResolvedPolicy>,
    verbose: bool,
) -> Result<()> {
    let repos = open_repos(repo_paths)?;
    let mut entries = uncached_entry_maps(&repos).await;
    let descs = if search_desc {
        let (indexed, rest) = split_indexed(&repos, search_index);
        described(&indexed, &rest, false).await?
    } else {
        Vec::new()
    };

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
    let vis = Visibility {
        arch: &arch_str,
        policy,
    };
    let names = category_names(&repos);

    for pat in patterns {
        let key = SearchKey::new(pat, options)
            .map_err(|e| anyhow::anyhow!("invalid search key '{pat}': {e}"))?;
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
            for cat in walk_categories(repo, &names) {
                for pkg in cat.packages() {
                    let mut hit = key.matches_name(cat.name(), pkg.name());
                    if !hit && search_desc && !has_cache {
                        hit = latest_visible(&repos, &entries, idx, pkg.cpn(), &vis)
                            .is_some_and(|l| key.matches_text(&l.entry.metadata.description));
                    }
                    if hit {
                        matched
                            .entry(format!("{}/{}", cat.name(), pkg.name()))
                            .or_insert_with(|| (*pkg.cpn(), idx));
                    }
                }
            }
        }

        // -S: also match the descriptions, read once above from the indexes and md5-caches.
        if search_desc {
            for d in descs.iter().filter(|d| key.matches_text(&d.description)) {
                let cpn = d.cpv.cpn;
                let key = format!("{}/{}", cpn.category, cpn.package);
                matched.entry(key).or_insert((cpn, d.repo));
            }
        }

        check_matched(&repos, &mut entries, matched.values()).await;
        for (key, (cpn, idx)) in &matched {
            print_search_block(&repos, &entries, *idx, cpn, key, &installed, &vis, verbose);
        }
        println!("[ Applications found : {} ]\n", matched.len());
    }
    Ok(())
}

/// A repository's cache entries by version, each checked against its ebuild
type Entries = HashMap<Cpv, portage_metadata::CacheEntry>;

/// Every entry of each repository without an md5-cache, whose descriptions the name walk
/// matches; a repository with one starts empty and is filled by [`check_matched`]
///
/// Checking a whole md5-cache-backed tree (Gentoo's) costs a full tree read per search, so
/// only the packages that are shown are checked, as emerge's `aux_get` does.
async fn uncached_entry_maps(repos: &[Repository]) -> Vec<Entries> {
    let mut maps = Vec::with_capacity(repos.len());
    for repo in repos {
        maps.push(if repo.has_primary_cache() {
            Entries::new()
        } else {
            repo_entries(repo).await.into_iter().collect()
        });
    }
    maps
}

/// Check the entries of the matched packages' ebuilds, and of the ebuilds a symlinked one
/// resolves to, in the repositories whose maps [`uncached_entry_maps`] left empty
async fn check_matched<'a>(
    repos: &[Repository],
    entries: &mut [Entries],
    matched: impl Iterator<Item = &'a (Cpn, usize)>,
) {
    let mut wanted: Vec<Vec<portage_repo::Ebuild>> = vec![Vec::new(); repos.len()];
    let mut want = |idx: usize, ebuild: portage_repo::Ebuild| {
        if repos[idx].has_primary_cache()
            && !entries[idx].contains_key(ebuild.cpv())
            && !wanted[idx].iter().any(|e| e.cpv() == ebuild.cpv())
        {
            wanted[idx].push(ebuild);
        }
    };
    for (cpn, idx) in matched {
        let Some(pkg) = repos[*idx]
            .category(cpn.category.as_str())
            .and_then(|cat| cat.package(cpn.package.as_str()))
        else {
            continue;
        };
        for ebuild in pkg.ebuilds().unwrap_or_default() {
            if let Some((other, target)) = symlink_target(repos, &ebuild)
                && let Some(Ok(Some(real))) = repos[other]
                    .category(target.cpn.category.as_str())
                    .and_then(|cat| cat.package(target.cpn.package.as_str()))
                    .map(|p| p.ebuild(&target.version.to_string()))
            {
                want(other, real);
            }
            want(*idx, ebuild);
        }
    }
    for (idx, ebuilds) in wanted.into_iter().enumerate() {
        if !ebuilds.is_empty() {
            entries[idx].extend(ebuild_entries(&repos[idx], ebuilds).await);
        }
    }
}

/// The repository and version a symlinked ebuild resolves into, if it is another one
fn symlink_target(repos: &[Repository], ebuild: &portage_repo::Ebuild) -> Option<(usize, Cpv)> {
    let real = ebuild.path().canonicalize_utf8().ok()?;
    repos.iter().enumerate().find_map(|(idx, other)| {
        let rel = real.strip_prefix(other.path()).ok()?;
        let category = rel.components().next()?.as_str();
        let target = Cpv::parse(&format!("{category}/{}", rel.file_stem()?)).ok()?;
        (target != *ebuild.cpv()).then_some((idx, target))
    })
}

/// What makes a version visible: the configured keywords and `package.mask` when the profile
/// resolves, else the KEYWORDS of the arch alone
#[derive(Clone, Copy)]
struct Visibility<'a> {
    arch: &'a str,
    policy: Option<&'a ResolvedPolicy>,
}

/// Why a version is not visible
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hidden {
    Mask,
    Keywords,
}

impl Hidden {
    fn label(self) -> &'static str {
        match self {
            Self::Mask => "package.mask",
            Self::Keywords => "keywords",
        }
    }
}

impl Visibility<'_> {
    fn hidden(
        &self,
        repo: &Repository,
        cpv: &Cpv,
        entry: &portage_metadata::CacheEntry,
    ) -> Option<Hidden> {
        let meta = &entry.metadata;
        let Some(resolved) = self.policy else {
            let arch = self.arch;
            let accepted = meta.keywords.iter().any(|k| {
                let k = k.to_string();
                k == arch || k == format!("~{arch}") || k == "*" || k == "~*"
            });
            return (!accepted).then_some(Hidden::Keywords);
        };
        if is_masked(
            &resolved.package_mask,
            &resolved.package_unmask,
            cpv,
            &meta.slot,
            repo.name(),
        ) {
            return Some(Hidden::Mask);
        }
        let accepted = resolved
            .accept_keywords
            .accepts(&meta.keywords, cpv, Some(meta.slot.slot));
        (!accepted).then_some(Hidden::Keywords)
    }
}

/// The newest version, with its metadata and whether it is visible
struct Latest {
    cpv: Cpv,
    entry: portage_metadata::CacheEntry,
    visible: bool,
    /// The newest version of all and why it is hidden, when it is not the one shown
    newer: Option<(portage_atom::Version, Hidden)>,
}

/// The newest visible version, or the newest of all when none is
fn latest_visible(
    repos: &[Repository],
    entries: &[Entries],
    idx: usize,
    cpn: &Cpn,
    vis: &Visibility,
) -> Option<Latest> {
    let repo = &repos[idx];
    let cat = repo.category(cpn.category.as_str())?;
    let pkg = cat.package(cpn.package.as_str())?;
    let ebuilds = pkg.ebuilds().ok()?;
    let mut newest: Option<(Cpv, portage_metadata::CacheEntry, Option<Hidden>)> = None;
    for eb in ebuilds.iter().rev() {
        let Some(entry) = entry_for(repos, entries, idx, eb) else {
            continue;
        };
        let hidden = vis.hidden(repo, eb.cpv(), &entry);
        if hidden.is_none() {
            let newer = newest.and_then(|(cpv, _, why)| Some((cpv.version, why?)));
            return Some(Latest {
                cpv: eb.cpv().clone(),
                entry,
                visible: true,
                newer,
            });
        }
        newest.get_or_insert((eb.cpv().clone(), entry, hidden));
    }
    newest.map(|(cpv, entry, _)| Latest {
        cpv,
        entry,
        visible: false,
        newer: None,
    })
}

/// Metadata for one ebuild: the repo's own md5-cache, the user-side sourced
/// cache, or — for a symlinked ebuild (crossdev) — the md5-cache of the repo
/// the link resolves into.
fn entry_for(
    repos: &[Repository],
    entries: &[Entries],
    idx: usize,
    ebuild: &portage_repo::Ebuild,
) -> Option<portage_metadata::CacheEntry> {
    let cpv = ebuild.cpv();
    if let Some(entry) = entries[idx].get(cpv) {
        return Some(entry.clone());
    }
    // Symlinked ebuild: the target repo's cache entry is byte-exact.
    let real = ebuild.path().canonicalize_utf8().ok()?;
    for (other_idx, other) in repos.iter().enumerate() {
        let Ok(rel) = real.strip_prefix(other.path()) else {
            continue;
        };
        let category = rel.components().next()?.as_str();
        let Ok(target) = Cpv::parse(&format!("{category}/{}", rel.file_stem()?)) else {
            continue;
        };
        if let Some(entry) = entries[other_idx].get(&target) {
            return Some(entry.clone());
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn print_search_block(
    repos: &[Repository],
    entries: &[Entries],
    idx: usize,
    cpn: &Cpn,
    key: &str,
    installed: &BTreeMap<Cpn, portage_atom::Version>,
    vis: &Visibility,
    verbose: bool,
) {
    let latest = latest_visible(repos, entries, idx, cpn, vis);
    let is_masked = latest.as_ref().is_some_and(|l| !l.visible);

    let mut out = anstream::stdout();
    write!(out, "{C_STAR}*{C_STAR:#}  {C_BOLD}{key}{C_BOLD:#}").ok();
    if is_masked {
        write!(out, " {C_MASKED}[ Masked ]{C_MASKED:#}").ok();
    }
    writeln!(out).ok();

    match &latest {
        Some(l) => writeln!(
            out,
            "      {C_LABEL}Latest version available:{C_LABEL:#} {}",
            l.cpv.version
        )
        .ok(),
        None => writeln!(
            out,
            "      {C_LABEL}Latest version available:{C_LABEL:#} [ Unknown ]"
        )
        .ok(),
    };
    if verbose && let Some((version, why)) = latest.as_ref().and_then(|l| l.newer.as_ref()) {
        writeln!(
            out,
            "      {C_LABEL}Newer version masked:{C_LABEL:#}    {version} {C_MASKED}[ {} ]{C_MASKED:#}",
            why.label()
        )
        .ok();
    }
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
    if let Some(l) = &latest {
        let entry = &l.entry;
        let sizes = distfiles_sizes(
            &repos[idx],
            &l.cpv,
            &entry.metadata,
            vis.policy.map(|p| (p, entry)),
        );
        write!(
            out,
            "      {C_LABEL}Size of files:{C_LABEL:#} {} KiB",
            sizes.enabled.div_ceil(1024)
        )
        .ok();
        if verbose && sizes.all != sizes.enabled {
            write!(
                out,
                " ({} KiB with every USE flag)",
                sizes.all.div_ceil(1024)
            )
            .ok();
        }
        writeln!(out).ok();
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

/// The `Manifest` size of a version's distfiles
struct DistfilesSize {
    /// Those of the enabled USE, as emerge counts them; every one when no policy resolves
    enabled: u64,
    /// Those every USE conditional could reference
    all: u64,
}

fn distfiles_sizes(
    repo: &Repository,
    cpv: &Cpv,
    meta: &portage_metadata::EbuildMetadata,
    policy: Option<(&ResolvedPolicy, &portage_metadata::CacheEntry)>,
) -> DistfilesSize {
    let Some(Ok(Some(manifest))) = repo.package(&cpv.cpn).map(|pkg| pkg.manifest()) else {
        return DistfilesSize { enabled: 0, all: 0 };
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
    let total = |enabled: &dyn Fn(&str) -> bool| {
        let mut filenames = Vec::new();
        for entry in meta.src_uri.list() {
            entry.collect_filenames(enabled, &mut filenames);
        }
        filenames.sort();
        filenames.dedup();
        filenames
            .into_iter()
            .filter_map(|f| sizes.get(&f).copied())
            .sum()
    };
    let all = total(&|_| true);
    let enabled = match policy {
        Some((resolved, entry)) => {
            let policy = resolved.as_policy();
            let slot = entry.metadata.slot.slot;
            let stable =
                resolved
                    .accept_keywords
                    .is_stable(&entry.metadata.keywords, cpv, Some(slot));
            let pkg = PortagePackage::slotted(cpv.cpn, slot);
            let cfg = effective_use(&policy, &pkg, &cpv.version, entry, stable, &[]);
            total(&|flag| cfg.get(Interned::intern(flag)) == UseFlagState::Enabled)
        }
        None => all,
    };
    DistfilesSize { enabled, all }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KW: Visibility = Visibility {
        arch: "amd64",
        policy: None,
    };

    /// A repository with `metadata/md5-cache` entries `(cpv, description)` and no index
    fn repo_with_cache(dir: &std::path::Path, name: &str, entries: &[(&str, &str)]) -> Repository {
        let root = dir.join(name);
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        std::fs::write(root.join("profiles/repo_name"), format!("{name}\n")).unwrap();
        std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
        for (cpv, description) in entries {
            let (cat, pf) = cpv.split_once('/').unwrap();
            let cache = root.join("metadata/md5-cache").join(cat);
            std::fs::create_dir_all(&cache).unwrap();
            std::fs::write(
                cache.join(pf),
                format!("DESCRIPTION={description}\nEAPI=8\nSLOT=0\n_md5_=00000000000000000000000000000000\n"),
            )
            .unwrap();
        }
        Repository::builder().in_memory_cache().open(root).unwrap()
    }

    #[tokio::test]
    async fn an_md5_cache_hit_names_its_own_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repos = [
            repo_with_index(dir.path(), "gentoo", "app-misc/foo 1: Foo\n"),
            repo_with_cache(
                dir.path(),
                "overlay",
                &[
                    ("app-misc/only-1", "Overlay only"),
                    ("app-misc/foo-1", "Foo again"),
                ],
            ),
            repo_with_cache(
                dir.path(),
                "third",
                &[("app-misc/only-1", "Same version later")],
            ),
        ];
        let (indexed, rest) = split_indexed(&repos, true);
        assert_eq!(rest.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [1, 2]);
        let mut found: Vec<(String, usize, String)> = described(&indexed, &rest, false)
            .await
            .unwrap()
            .into_iter()
            .map(|d| (d.cpv.to_string(), d.repo, d.description))
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                ("app-misc/foo-1".to_string(), 0, "Foo".to_string()),
                ("app-misc/only-1".to_string(), 1, "Overlay only".to_string()),
            ],
            "the overlay-only package is the overlay's, and on equal versions the earlier repository wins"
        );
    }

    fn repo_with_index(dir: &std::path::Path, name: &str, index: &str) -> Repository {
        let root = dir.join(name);
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        std::fs::write(root.join("profiles/repo_name"), format!("{name}\n")).unwrap();
        std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
        std::fs::write(root.join("metadata/pkg_desc_index"), index).unwrap();
        Repository::builder().in_memory_cache().open(root).unwrap()
    }

    #[test]
    fn the_index_gives_the_newest_version_and_its_description() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_index(
            dir.path(),
            "a",
            "app-misc/foo 1 2: Foo: the tool\nnot an entry\napp-misc/bar 1-r1:\n",
        );
        let index: Vec<(String, String)> = desc_index(&repo)
            .unwrap()
            .into_iter()
            .map(|(cpv, d)| (cpv.to_string(), d))
            .collect();
        assert_eq!(
            index,
            [
                ("app-misc/foo-2".to_string(), "Foo: the tool".to_string()),
                ("app-misc/bar-1-r1".to_string(), String::new()),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_overlay_without_md5_cache_is_sourced_against_its_master() {
        let dir = tempfile::tempdir().unwrap();
        let master = dir.path().join("gentoo");
        let ov = dir.path().join("ov");
        for (root, name) in [(&master, "gentoo"), (&ov, "ov")] {
            std::fs::create_dir_all(root.join("profiles")).unwrap();
            std::fs::create_dir_all(root.join("metadata")).unwrap();
            std::fs::write(root.join("profiles/repo_name"), format!("{name}\n")).unwrap();
        }
        std::fs::write(master.join("metadata/layout.conf"), "masters =\n").unwrap();
        std::fs::write(master.join("profiles/categories"), "app-misc\n").unwrap();
        std::fs::create_dir_all(master.join("eclass")).unwrap();
        std::fs::write(
            master.join("eclass/desc.eclass"),
            "DESCRIPTION=\"from the master eclass\"\n",
        )
        .unwrap();
        // The overlay lists no categories and inherits a master eclass.
        std::fs::write(ov.join("metadata/layout.conf"), "masters = gentoo\n").unwrap();
        std::fs::create_dir_all(ov.join("app-misc/ovpkg")).unwrap();
        std::fs::write(
            ov.join("app-misc/ovpkg/ovpkg-1.ebuild"),
            "EAPI=8\ninherit desc\nSLOT=\"0\"\n",
        )
        .unwrap();
        let cache = tempfile::tempdir().unwrap();
        let repos: Vec<Repository> = [&master, &ov]
            .iter()
            .map(|p| {
                Repository::builder()
                    .user_cache_root(
                        camino::Utf8PathBuf::try_from(cache.path().to_path_buf()).unwrap(),
                    )
                    .open_with_masters(p.as_path(), dir.path(), None, None)
                    .unwrap()
            })
            .collect();
        let found: Vec<(String, String)> = sourced(&repos[1], 1, false)
            .await
            .into_iter()
            .map(|d| (d.cpv.to_string(), d.description))
            .collect();
        assert_eq!(
            found,
            [(
                "app-misc/ovpkg-1".to_string(),
                "from the master eclass".to_string()
            )]
        );
        // And the emerge-style block reads the same entries.
        let mut entries = uncached_entry_maps(&repos).await;
        let cpn = Cpn::parse("app-misc/ovpkg").unwrap();
        check_matched(&repos, &mut entries, [(cpn, 1)].iter()).await;
        assert!(latest_visible(&repos, &entries, 1, &cpn, &KW).is_some());
    }

    #[test]
    fn an_overlay_package_in_a_category_only_its_master_lists_is_walked() {
        let dir = tempfile::tempdir().unwrap();
        let master = repo_with_index(dir.path(), "a", "");
        std::fs::write(
            master.path().join("profiles/categories"),
            "app-misc\nsys-apps\n",
        )
        .unwrap();
        std::fs::create_dir_all(master.path().join("sys-apps/which")).unwrap();
        let ov = repo_with_index(dir.path(), "b", "");
        std::fs::create_dir_all(ov.path().join("app-misc/ovpkg")).unwrap();
        // The overlay lists no categories of its own.
        let repos = [master, ov];
        let names = category_names(&repos);
        assert_eq!(names, ["app-misc", "sys-apps"]);
        let walked: Vec<String> = walk_categories(&repos[1], &names)
            .map(|c| c.name().to_string())
            .collect();
        assert_eq!(walked, ["app-misc"]);
        assert_eq!(repos[1].categories().into_iter().count(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_repository_without_md5_cache_is_described_by_sourcing_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ov");
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        std::fs::create_dir_all(root.join("app-misc/foo")).unwrap();
        std::fs::write(root.join("profiles/repo_name"), "ov\n").unwrap();
        std::fs::write(root.join("profiles/categories"), "app-misc\n").unwrap();
        std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
        for (v, d) in [("1", "Old overlay tool"), ("2", "New overlay tool")] {
            std::fs::write(
                root.join(format!("app-misc/foo/foo-{v}.ebuild")),
                format!("EAPI=8\nDESCRIPTION=\"{d}\"\nHOMEPAGE=\"https://example.invalid/{v}\"\nSLOT=\"0\"\n"),
            )
            .unwrap();
        }
        let repo = Repository::builder().in_memory_cache().open(&root).unwrap();
        assert!(!repo.has_primary_cache());
        let found: Vec<(String, String, Option<String>)> = sourced(&repo, 0, true)
            .await
            .into_iter()
            .map(|d| (d.cpv.to_string(), d.description, d.homepage))
            .collect();
        assert_eq!(
            found,
            [(
                "app-misc/foo-2".to_string(),
                "New overlay tool".to_string(),
                Some("https://example.invalid/2".to_string())
            )]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_md5_cache_is_refreshed_from_the_ebuild() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ov");
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::create_dir_all(root.join("metadata/md5-cache/app-misc")).unwrap();
        std::fs::create_dir_all(root.join("app-misc/foo")).unwrap();
        std::fs::create_dir_all(root.join("app-misc/bar")).unwrap();
        std::fs::write(root.join("profiles/repo_name"), "ov\n").unwrap();
        std::fs::write(root.join("profiles/categories"), "app-misc\n").unwrap();
        std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
        let cache = root.join("metadata/md5-cache/app-misc/foo-1");
        std::fs::write(
            &cache,
            "DESCRIPTION=Stale\nEAPI=8\nSLOT=0\n_md5_=00000000000000000000000000000000\n",
        )
        .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&cache)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
        for pn in ["foo", "bar"] {
            std::fs::write(
                root.join(format!("app-misc/{pn}/{pn}-1.ebuild")),
                format!("EAPI=8\nDESCRIPTION=\"Fresh {pn}\"\nSLOT=\"0\"\n"),
            )
            .unwrap();
        }
        let repo = Repository::builder().in_memory_cache().open(&root).unwrap();
        let mut found: Vec<(String, String)> = described(&[], &[(0, repo)], false)
            .await
            .unwrap()
            .into_iter()
            .map(|d| (d.cpv.to_string(), d.description))
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                ("app-misc/bar-1".to_string(), "Fresh bar".to_string()),
                ("app-misc/foo-1".to_string(), "Fresh foo".to_string()),
            ]
        );
    }

    #[test]
    fn the_emerge_style_block_reads_a_stale_cache_entry_from_the_ebuild() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ov");
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::create_dir_all(root.join("metadata/md5-cache/app-misc")).unwrap();
        std::fs::create_dir_all(root.join("app-misc/foo")).unwrap();
        std::fs::write(root.join("profiles/repo_name"), "ov\n").unwrap();
        std::fs::write(root.join("profiles/categories"), "app-misc\n").unwrap();
        std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
        let cache = root.join("metadata/md5-cache/app-misc/foo-1");
        std::fs::write(
            &cache,
            "DESCRIPTION=Stale\nEAPI=8\nSLOT=0\nKEYWORDS=amd64\n_md5_=00000000000000000000000000000000\n",
        )
        .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&cache)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
        std::fs::write(
            root.join("app-misc/foo/foo-1.ebuild"),
            "EAPI=8\nDESCRIPTION=\"Fresh\"\nSLOT=\"0\"\nKEYWORDS=\"amd64\"\n",
        )
        .unwrap();
        let repos = [Repository::builder().in_memory_cache().open(&root).unwrap()];
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let cpn = Cpn::parse("app-misc/foo").unwrap();
        let entries = rt.block_on(async {
            let mut entries = uncached_entry_maps(&repos).await;
            assert!(
                entries[0].is_empty(),
                "a repository with an md5-cache is checked on demand"
            );
            check_matched(&repos, &mut entries, [(cpn, 0)].iter()).await;
            entries
        });
        let entry = latest_visible(&repos, &entries, 0, &cpn, &KW)
            .unwrap()
            .entry;
        assert_eq!(entry.metadata.description, "Fresh");
    }

    #[test]
    fn a_newer_version_without_the_arch_keyword_is_reported_as_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ov");
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::create_dir_all(root.join("metadata/md5-cache/app-misc")).unwrap();
        std::fs::create_dir_all(root.join("app-misc/foo")).unwrap();
        std::fs::write(root.join("profiles/repo_name"), "ov\n").unwrap();
        std::fs::write(root.join("profiles/categories"), "app-misc\n").unwrap();
        std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
        for (version, keywords) in [("1", "amd64"), ("2", "~arm64")] {
            std::fs::write(
                root.join(format!("app-misc/foo/foo-{version}.ebuild")),
                format!("EAPI=8\nDESCRIPTION=\"d\"\nSLOT=\"0\"\nKEYWORDS=\"{keywords}\"\n"),
            )
            .unwrap();
        }
        let repos = [Repository::builder().in_memory_cache().open(&root).unwrap()];
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let cpn = Cpn::parse("app-misc/foo").unwrap();
        let entries = rt.block_on(async {
            let mut entries = uncached_entry_maps(&repos).await;
            check_matched(&repos, &mut entries, [(cpn, 0)].iter()).await;
            entries
        });
        let latest = latest_visible(&repos, &entries, 0, &cpn, &KW).unwrap();
        assert_eq!(latest.cpv.version.to_string(), "1");
        let (newer, why) = latest.newer.unwrap();
        assert_eq!((newer.to_string(), why.label()), ("2".into(), "keywords"));
    }

    #[test]
    fn only_the_matched_packages_of_a_cached_repository_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ov");
        for d in [
            "profiles",
            "metadata/md5-cache/app-misc",
            "app-misc/foo",
            "app-misc/bar",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("profiles/repo_name"), "ov\n").unwrap();
        std::fs::write(root.join("profiles/categories"), "app-misc\n").unwrap();
        std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
        for pn in ["foo", "bar"] {
            let ebuild =
                format!("EAPI=8\nDESCRIPTION=\"Fresh {pn}\"\nSLOT=\"0\"\nKEYWORDS=\"amd64\"\n");
            std::fs::write(root.join(format!("app-misc/{pn}/{pn}-1.ebuild")), &ebuild).unwrap();
            // A fresh entry for foo, a stale one for bar.
            let md5 = if pn == "foo" {
                format!("{:x}", md5::compute(ebuild.as_bytes()))
            } else {
                "0".repeat(32)
            };
            std::fs::write(
                root.join(format!("metadata/md5-cache/app-misc/{pn}-1")),
                format!("DESCRIPTION=Cached {pn}\nEAPI=8\nSLOT=0\nKEYWORDS=amd64\n_md5_={md5}\n"),
            )
            .unwrap();
        }
        let repos = [Repository::builder().in_memory_cache().open(&root).unwrap()];
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let foo = Cpn::parse("app-misc/foo").unwrap();
        let bar = Cpn::parse("app-misc/bar").unwrap();
        let mut entries = rt.block_on(uncached_entry_maps(&repos));
        rt.block_on(check_matched(&repos, &mut entries, [(foo, 0)].iter()));
        let checked: Vec<String> = entries[0].keys().map(ToString::to_string).collect();
        assert_eq!(
            checked,
            ["app-misc/foo-1"],
            "bar was not matched, so not read"
        );
        // The fresh cached entry is kept as is.
        assert_eq!(
            entries[0].values().next().unwrap().metadata.description,
            "Cached foo"
        );
        rt.block_on(check_matched(&repos, &mut entries, [(bar, 0)].iter()));
        let entry = latest_visible(&repos, &entries, 0, &bar, &KW)
            .unwrap()
            .entry;
        assert_eq!(
            entry.metadata.description, "Fresh bar",
            "a stale entry is sourced"
        );
    }

    #[tokio::test]
    async fn the_newest_version_across_repositories_decides_the_description() {
        let dir = tempfile::tempdir().unwrap();
        let repos = [
            repo_with_index(
                dir.path(),
                "a",
                "app-misc/foo 1 2: Old\napp-misc/bar 1: Bar in a\n",
            ),
            repo_with_index(
                dir.path(),
                "b",
                "app-misc/foo 3: New\napp-misc/bar 1: Bar in b\n",
            ),
        ];
        assert!(split_indexed(&repos, false).0.is_empty());
        let (indexed, rest) = split_indexed(&repos, true);
        assert!(rest.is_empty());
        let mut found: Vec<(String, usize, String)> = described(&indexed, &rest, false)
            .await
            .unwrap()
            .into_iter()
            .map(|d| (d.cpv.to_string(), d.repo, d.description))
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                ("app-misc/bar-1".to_string(), 0, "Bar in a".to_string()),
                ("app-misc/foo-3".to_string(), 1, "New".to_string()),
            ]
        );
    }
}
