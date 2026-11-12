use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::{Result, bail};
use portage_atom::{Cpn, Cpv};
use portage_metadata::RawCacheEntry;
use portage_repo::{CacheReadOpts, Repository, cache_entries_parallel, repo_entries};

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
    let opts = CacheReadOpts {
        latest_per_cpn: true,
        ..Default::default()
    };
    // One read per repository, so an md5-cache hit names the repository it came from (an
    // overlay-only package must not be looked up in the first repository).
    let mut from_cache = Vec::new();
    for (repo, repository) in rest {
        let cached = cache_entries_parallel(std::slice::from_ref(repository), &opts, move |text| {
            let raw = RawCacheEntry::new(text);
            let description = raw.field("DESCRIPTION").unwrap_or("").to_string();
            let page = homepage.then(|| raw.field("HOMEPAGE").unwrap_or("").to_string());
            Ok::<_, portage_repo::Error>((description, page))
        })
        .await?;
        from_cache.extend(cached.into_iter().filter_map(|(cpv, r)| {
            let (description, homepage) = r.ok()?;
            Some(Described {
                cpv,
                repo: *repo,
                description,
                homepage,
            })
        }));
    }
    let from_index = indexed.iter().flat_map(|(repo, index)| {
        index.iter().map(|(cpv, description)| Described {
            cpv: cpv.clone(),
            repo: *repo,
            description: description.clone(),
            homepage: None,
        })
    });
    Ok(newest_per_package(from_index.chain(from_cache)))
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
    let mut found = described(indexed, repos, homepage).await?;
    // The md5-cache read above sees nothing of a repository without one (most overlays).
    for (position, repo) in repos.iter().filter(|(_, r)| !r.has_primary_cache()) {
        found.extend(sourced(repo, *position, homepage).await);
    }
    let mut entries: Vec<(Cpn, Option<String>)> = newest_per_package(found)
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
) -> Result<()> {
    let repos = open_repos(repo_paths)?;
    // A repository without md5-cache answers `cache_entry` only from the user-side cache, empty
    // until something sources its ebuilds; source them now, as emerge's `aux_get` would, so its
    // descriptions match and its blocks show a version.
    for repo in repos.iter().filter(|r| !r.has_primary_cache()) {
        repo_entries(repo).await;
    }
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
    let names = category_names(&repos);

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
            for cat in walk_categories(repo, &names) {
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

        // -S: also match the descriptions, read once above from the indexes and md5-caches.
        if search_desc {
            for d in descs.iter().filter(|d| contains_ci(&d.description, pat)) {
                let cpn = d.cpv.cpn;
                let key = format!("{}/{}", cpn.category, cpn.package);
                matched.entry(key).or_insert((cpn, d.repo));
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
        let Ok(target) = Cpv::parse(&format!("{category}/{}", rel.file_stem()?)) else {
            continue;
        };
        if let Ok(Some(entry)) = other.cache_entry(&target) {
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
    let Some(Ok(Some(manifest))) = repo.package(cpn).map(|pkg| pkg.manifest()) else {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // The md5-cache read finds nothing here; sourcing does.
        assert!(
            described(&[], &[(0, repo.clone())], false)
                .await
                .unwrap()
                .is_empty()
        );
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
