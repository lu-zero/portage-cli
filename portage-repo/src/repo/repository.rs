use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};

use gentoo_core::Arch;
use jwalk::WalkDir;
use portage_atom::{Cpn, Cpv, Dep};
use portage_metadata::{CacheEntry, Eapi};

use super::category::Categories;
use super::ebuild::Ebuild;
use crate::metadata_cache::{DirMetadataCache, MemoryMetadataCache, MetadataCache};

type EbuildFilter = dyn Fn(&Ebuild) -> bool + Send + Sync;

/// Lazy, composable ebuild discovery over a repository tree.
///
/// Wraps a [`jwalk::WalkDir`] builder and an optional filter closure.
/// Nothing is walked until [`IntoIterator::into_iter`] or a collecting
/// method is called. The filter is applied during iteration, not upfront.
///
/// ```
/// # use portage_repo::Repository;
/// # fn demo(repo: Repository) {
/// // iterate lazily
/// for ebuild in repo.ebuilds().unwrap() {
///     println!("{}", ebuild.cpv());
/// }
///
/// // filter + collect
/// let ebuilds = repo.ebuilds()
///     .unwrap()
///     .filter(|eb| eb.category() == "dev-util")
///     .collect_vec();
/// # }
/// ```
pub struct Ebuilds {
    walker: WalkDir,
    filter: Option<Arc<EbuildFilter>>,
}

/// Concrete iterator produced by [`Ebuilds::into_iter`].
///
/// Holds the jwalk `DirEntryIter` and converts each entry
/// to an [`Ebuild`] on the fly, applying the optional filter.
pub struct EbuildsIter {
    inner: jwalk::DirEntryIter<((), ())>,
    filter: Option<Arc<EbuildFilter>>,
}

impl Ebuilds {
    fn new(walker: WalkDir) -> Self {
        Self {
            walker,
            filter: None,
        }
    }

    /// Retain only ebuilds matching the predicate.
    ///
    /// Consuming: call `.filter(...)` repeatedly to chain predicates.
    pub fn filter<F>(mut self, f: F) -> Self
    where
        F: Fn(&Ebuild) -> bool + Send + Sync + 'static,
    {
        self.filter = Some(Arc::new(f));
        self
    }

    /// Collect all matching ebuilds into a sorted `Vec`.
    pub fn collect_vec(self) -> Vec<Ebuild> {
        let mut v: Vec<Ebuild> = self.into_iter().collect();
        v.sort_by(|a, b| a.cpv().cmp(b.cpv()));
        v
    }
}

fn dir_entry_to_ebuild(entry: jwalk::Result<jwalk::DirEntry<((), ())>>) -> Option<Ebuild> {
    let entry = entry.ok()?;
    let path: Utf8PathBuf = entry.path().try_into().ok()?;
    let stem = path.file_name()?.strip_suffix(".ebuild")?;
    let cat_name = path.parent()?.parent()?.file_name()?;

    let mut cpv_str = String::with_capacity(cat_name.len() + 1 + stem.len());
    cpv_str.push_str(cat_name);
    cpv_str.push('/');
    cpv_str.push_str(stem);
    let cpv = Cpv::parse(&cpv_str).ok()?;
    Some(Ebuild::new(cpv, path))
}

impl IntoIterator for Ebuilds {
    type Item = Ebuild;
    type IntoIter = EbuildsIter;

    fn into_iter(self) -> EbuildsIter {
        EbuildsIter {
            inner: self.walker.into_iter(),
            filter: self.filter,
        }
    }
}

impl Iterator for EbuildsIter {
    type Item = Ebuild;

    fn next(&mut self) -> Option<Ebuild> {
        loop {
            let ebuild = dir_entry_to_ebuild(self.inner.next()?)?;
            match &self.filter {
                Some(f) if !f(&ebuild) => continue,
                _ => return Some(ebuild),
            }
        }
    }
}

/// Lazy iterator over every `metadata/md5-cache/{cat}/{name-version}` file.
///
/// Produced by [`Repository::cache_entries`]. Each item is a `(Cpv, …)`
/// tuple; the second element is the parsed entry or the I/O / parse error
/// for that specific file.
pub struct CacheEntries {
    walker: WalkDir,
}

/// Concrete iterator produced by [`CacheEntries::into_iter`].
pub struct CacheEntriesIter {
    inner: jwalk::DirEntryIter<((), ())>,
}

fn dir_entry_to_cache(
    entry: jwalk::Result<jwalk::DirEntry<((), ())>>,
) -> Option<(Cpv, Result<CacheEntry>)> {
    let entry = entry.ok()?;
    if !entry.file_type().is_file() {
        return None;
    }
    let path: Utf8PathBuf = entry.path().try_into().ok()?;
    let stem = path.file_name()?;
    let cat_name = path.parent()?.file_name()?;

    let mut cpv_str = String::with_capacity(cat_name.len() + 1 + stem.len());
    cpv_str.push_str(cat_name);
    cpv_str.push('/');
    cpv_str.push_str(stem);
    let cpv = Cpv::parse(&cpv_str).ok()?;

    let result = match std::fs::read_to_string(&path) {
        Ok(contents) => CacheEntry::parse(&contents).map_err(Error::from),
        Err(e) => Err(util::io_err(&path, e)),
    };
    Some((cpv, result))
}

impl IntoIterator for CacheEntries {
    type Item = (Cpv, Result<CacheEntry>);
    type IntoIter = CacheEntriesIter;

    fn into_iter(self) -> CacheEntriesIter {
        CacheEntriesIter {
            inner: self.walker.into_iter(),
        }
    }
}

impl Iterator for CacheEntriesIter {
    type Item = (Cpv, Result<CacheEntry>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = dir_entry_to_cache(self.inner.next()?) {
                return Some(item);
            }
        }
    }
}

/// A single package-move or slot-move entry from `profiles/updates/`.
///
/// See [PMS 4.4.4](https://projects.gentoo.org/pms/9/pms.html#profiles-updates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileUpdate {
    /// `move <old> <new>` — package renamed.
    Move {
        /// Old category/package name.
        old: Cpn,
        /// New category/package name.
        new: Cpn,
    },
    /// `slotmove <dep> <old_slot> <new_slot>` — slot renamed.
    SlotMove {
        /// Atom (possibly versioned) identifying affected packages.
        /// Boxed to keep the enum near [`ProfileUpdate::Move`]'s size.
        dep: Box<Dep>,
        /// Old slot value.
        old_slot: String,
        /// New slot value.
        new_slot: String,
    },
}

use super::category::Category;
use super::layout::LayoutConf;
use super::profile::{Profile, ProfileDesc, ProfileStack};
use super::use_expand::UseExpand;
use super::util;
use crate::error::{Error, Result};

/// A Gentoo ebuild repository.
///
/// This is the main entry point for the crate. It eagerly loads `layout.conf`
/// and the repository name, while category/package enumeration is lazy.
///
/// See [PMS 4 — Tree Layout](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
///
/// Construct with [`Repository::builder`]: secondary metadata cache is required
/// (in-memory for tests, directory under XDG for production).
#[derive(Clone)]
pub struct Repository {
    path: Utf8PathBuf,
    layout: LayoutConf,
    name: String,
    arch_cache: Vec<Arch>,
    /// In-tree `metadata/md5-cache` (usually a [`DirMetadataCache`]).
    primary: Arc<dyn MetadataCache>,
    /// Always-present writable store for lazy overlay metadata / unprivileged writes.
    secondary: Arc<dyn MetadataCache>,
    /// Directory backing `secondary`, when it is a durable on-disk store.
    /// `None` for in-memory/custom stores, which have nowhere to keep a
    /// sidecar index.
    secondary_dir: Option<Utf8PathBuf>,
}

impl std::fmt::Debug for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repository")
            .field("path", &self.path)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// How the builder materialises the writable secondary metadata cache.
#[derive(Clone)]
enum SecondarySpec {
    /// Ephemeral (tests).
    Memory,
    /// Durable disk: `root.join(repo.name())` after the tree name is known.
    UserRoot(Utf8PathBuf),
    /// Pre-built store.
    Custom(Arc<dyn MetadataCache>),
}

/// Builder for [`Repository`]: requires a secondary metadata cache before open.
///
/// ```no_run
/// use portage_repo::Repository;
/// let repo = Repository::builder()
///     .in_memory_cache()
///     .open("/var/db/repos/gentoo")
///     .unwrap();
/// ```
///
/// Production: `builder().user_cache_root(app_md5_cache_root).open(tree)`.
#[derive(Clone, Default)]
pub struct RepositoryBuilder {
    secondary: Option<SecondarySpec>,
}

impl RepositoryBuilder {
    /// In-memory secondary (tests / ephemeral). Always writable, not durable.
    pub fn in_memory_cache(mut self) -> Self {
        self.secondary = Some(SecondarySpec::Memory);
        self
    }

    /// Durable secondary at `root/<repo-name>/` (name from the tree).
    ///
    /// Pass only the app-level root (e.g. `$XDG_CACHE_HOME/em/md5-cache`).
    /// [`Repository::write_cache_entry`] prefers in-tree primary, then this
    /// secondary — the same policy `em regen` uses without re-deriving paths.
    pub fn user_cache_root(mut self, root: impl Into<Utf8PathBuf>) -> Self {
        self.secondary = Some(SecondarySpec::UserRoot(root.into()));
        self
    }

    /// Pre-built secondary (exact dir via [`DirMetadataCache`], custom, …).
    pub fn cache(mut self, cache: impl MetadataCache + 'static) -> Self {
        self.secondary = Some(SecondarySpec::Custom(Arc::new(cache)));
        self
    }

    /// Open an ebuild repository at `path`.
    pub fn open(self, path: impl Into<PathBuf>) -> Result<Repository> {
        let spec = self.secondary.ok_or(Error::BuilderMissingSecondary)?;
        Repository::open_with_secondary(path, spec)
    }

    /// Open a repository and resolve masters from `repos_dir`.
    ///
    /// Masters share `UserRoot` (same root, per-name dirs) or get a fresh
    /// in-memory secondary for `Memory` / `Custom`.
    pub fn open_with_masters(
        self,
        path: impl Into<PathBuf>,
        repos_dir: impl AsRef<Path>,
    ) -> Result<(Repository, Vec<Repository>)> {
        let spec = self.secondary.ok_or(Error::BuilderMissingSecondary)?;
        let repo = Repository::open_with_secondary(path, spec.clone())?;
        let mut masters: Vec<Repository> = Vec::new();
        let mut seen = HashSet::new();
        seen.insert(repo.name().to_string());
        Repository::resolve_masters_with_spec(
            &repo,
            repos_dir.as_ref(),
            &mut masters,
            &mut seen,
            &spec,
        )?;
        Ok((repo, masters))
    }
}

impl Repository {
    /// Start a builder (secondary cache is required before [`RepositoryBuilder::open`]).
    pub fn builder() -> RepositoryBuilder {
        RepositoryBuilder::default()
    }

    fn open_with_secondary(
        path: impl Into<PathBuf>,
        secondary_spec: SecondarySpec,
    ) -> Result<Self> {
        let std_path = path.into();
        let path = Utf8PathBuf::from_path_buf(std_path).map_err(Error::InvalidRepository)?;
        if !path.is_dir() {
            return Err(Error::InvalidRepository(path.into_std_path_buf()));
        }

        let layout = LayoutConf::from_repo(path.as_std_path())?;

        let name = util::read_single_line(path.join("profiles").join("repo_name"))?
            .unwrap_or_else(|| path.file_name().unwrap_or_default().to_string());

        let arch_cache: Vec<Arch> = util::read_lines(path.join("profiles").join("arch.list"))
            .unwrap_or_default()
            .into_iter()
            .map(|s| Arch::intern(&s))
            .collect();

        let primary: Arc<dyn MetadataCache> = Arc::new(DirMetadataCache::new(
            path.join("metadata").join("md5-cache"),
        ));
        let secondary_dir = match &secondary_spec {
            SecondarySpec::UserRoot(root) => Some(root.join(&name)),
            SecondarySpec::Memory | SecondarySpec::Custom(_) => None,
        };
        let secondary = materialise_secondary(&name, secondary_spec);

        Ok(Repository {
            path,
            layout,
            name,
            arch_cache,
            primary,
            secondary,
            secondary_dir,
        })
    }

    fn resolve_masters_with_spec(
        repo: &Repository,
        repos_dir: &Path,
        out: &mut Vec<Repository>,
        seen: &mut HashSet<String>,
        spec: &SecondarySpec,
    ) -> Result<()> {
        for master_name in &repo.layout().masters {
            if !seen.insert(master_name.clone()) {
                continue;
            }
            let master_path = repos_dir.join(master_name);
            let master_spec = match spec {
                SecondarySpec::UserRoot(root) => SecondarySpec::UserRoot(root.clone()),
                SecondarySpec::Memory | SecondarySpec::Custom(_) => SecondarySpec::Memory,
            };
            let master = Self::open_with_secondary(master_path, master_spec)?;
            Self::resolve_masters_with_spec(&master, repos_dir, out, seen, spec)?;
            out.push(master);
        }
        Ok(())
    }

    /// Absolute path to the repository root.
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// Directory backing the in-tree primary md5-cache (`metadata/md5-cache`
    /// under [`Self::path`]), regardless of whether it's currently writable
    /// — matches `open_with_secondary`'s own construction of `primary`
    /// (private, in this module).
    pub fn primary_cache_dir(&self) -> Utf8PathBuf {
        self.path.join("metadata").join("md5-cache")
    }

    /// Directory backing the secondary (user) md5-cache, when it's a durable
    /// on-disk store — `None` for in-memory/custom stores, same condition
    /// [`Self::sidecar_path`] uses.
    pub fn secondary_cache_dir(&self) -> Option<&Utf8Path> {
        self.secondary_dir.as_deref()
    }

    /// Where a sidecar index for this repo lives, when the secondary store is
    /// durable. `None` for in-memory stores (tests), which must recompute.
    pub fn sidecar_path(&self, name: &str) -> Option<Utf8PathBuf> {
        self.secondary_dir.as_ref().map(|d| d.join(name))
    }

    /// A cheap stamp that changes when the tree is synced.
    ///
    /// `metadata/timestamp.chk` is what rsync rewrites on every sync, and a git
    /// checkout moves the repo directory's own mtime. Both are consulted, so a
    /// tree maintained either way invalidates. Content is deliberately not
    /// hashed: this decides whether a cached *derived* index may be reused, and
    /// the fallback on a miss is to recompute, not to be wrong.
    ///
    /// `None` when neither can be read — treat that as "always recompute".
    pub fn sync_stamp(&self) -> Option<String> {
        let stamp_of = |p: Utf8PathBuf| -> Option<String> {
            let m = std::fs::metadata(p.as_std_path()).ok()?;
            let secs = m
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            Some(format!("{secs}:{}", m.len()))
        };
        let chk = stamp_of(self.path.join("metadata").join("timestamp.chk"));
        let root = stamp_of(self.path.clone());
        match (chk, root) {
            (None, None) => None,
            (a, b) => Some(format!(
                "{}|{}",
                a.unwrap_or_default(),
                b.unwrap_or_default()
            )),
        }
    }

    /// When the tree was last synced, for deciding which ebuilds are newer than
    /// their cache entries. `None` when no marker can be read.
    pub fn sync_time(&self) -> Option<std::time::SystemTime> {
        let chk = self.path.join("metadata").join("timestamp.chk");
        std::fs::metadata(chk.as_std_path())
            .or_else(|_| std::fs::metadata(self.path.as_std_path()))
            .and_then(|m| m.modified())
            .ok()
    }

    /// Repository name (from `profiles/repo_name`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The parsed `metadata/layout.conf`.
    pub fn layout(&self) -> &LayoutConf {
        &self.layout
    }

    /// Lazy iterator over all categories declared in `profiles/categories`.
    ///
    /// Returns a [`Categories`] builder; nothing is read until the iterator
    /// is driven. Use `.filter()` to restrict and `.collect_vec()` to materialise.
    ///
    /// See [PMS 4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn categories(&self) -> Categories {
        Categories::new(
            self.path.join("profiles").join("categories"),
            self.path.clone(),
        )
    }

    /// List all ebuilds in the repository using parallel directory walking.
    ///
    /// Uses [`jwalk`] to walk category directories concurrently, collecting
    /// all `.ebuild` files. Only categories listed in `profiles/categories`
    /// are visited. Results are sorted by CPV.
    ///
    /// See [PMS 4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn ebuilds(&self) -> Result<Ebuilds> {
        let categories: HashSet<String> =
            util::read_lines(self.path.join("profiles").join("categories"))?
                .into_iter()
                .collect();
        self.ebuilds_in_categories(categories)
    }

    /// Like [`ebuilds`](Self::ebuilds), but with the valid categories taken
    /// as the union across this repo and its masters (portage semantics): an
    /// overlay may ship packages in a master's category without listing it in
    /// its own `profiles/categories`.
    pub fn ebuilds_with_masters(&self, masters: &[Repository]) -> Result<Ebuilds> {
        let mut categories: HashSet<String> = HashSet::new();
        for repo in std::iter::once(self).chain(masters.iter()) {
            if let Ok(lines) = util::read_lines(repo.path().join("profiles").join("categories")) {
                categories.extend(lines.into_iter().filter(|c| !c.is_empty()));
            }
        }
        self.ebuilds_in_categories(categories)
    }

    /// Like [`ebuilds`](Self::ebuilds), but with an explicit category set.
    ///
    /// Portage treats the valid categories as the *union* across a repo and
    /// its masters, so an overlay may ship packages in a master's category
    /// without listing it in its own `profiles/categories`.
    pub fn ebuilds_in_categories(&self, categories: HashSet<String>) -> Result<Ebuilds> {
        // Follow symlinks: overlays like crossdev's symlink whole package
        // directories to the host repo's ebuilds (the category comes from the
        // logical path, so cross-*/gcc stays cross-*/gcc).
        let walker = WalkDir::new(&self.path)
            .follow_links(true)
            .min_depth(3)
            .max_depth(3)
            .process_read_dir(move |depth, _path, _state, children| {
                children.retain(|entry| {
                    entry.as_ref().is_ok_and(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        match depth {
                            None => true,
                            Some(0) => categories.contains(name.as_ref()),
                            Some(1) => !name.starts_with('.'),
                            _ => name.ends_with(".ebuild"),
                        }
                    })
                });
            });

        Ok(Ebuilds::new(walker))
    }

    /// Look up a single category by name.
    pub fn category(&self, name: &str) -> Option<Category> {
        let cat_path: Utf8PathBuf = self.path.join(name);
        if cat_path.is_dir() {
            Some(Category::new(name.to_string(), cat_path))
        } else {
            None
        }
    }

    /// Resolve a package pattern to one or more [`Cpn`] values.
    ///
    /// * `cat/pkg` — exact lookup within the named category.
    /// * bare `name` — scans all categories for packages matching the name.
    ///
    /// Returns an empty `Vec` when no match is found.
    pub fn find_cpns(&self, pattern: &str) -> Vec<Cpn> {
        if let Some(slash) = pattern.find('/') {
            let cat_name = &pattern[..slash];
            let pkg_name = &pattern[slash + 1..];
            let Some(cat) = self.category(cat_name) else {
                return vec![];
            };
            if let Some(pkg) = cat.package(pkg_name) {
                return vec![*pkg.cpn()];
            }
            // Package name might be a glob/version pattern — just return empty
            vec![]
        } else {
            let name = pattern;
            self.categories()
                .into_iter()
                .filter_map(|cat| cat.package(name).map(|p| *p.cpn()))
                .collect()
        }
    }

    /// Read a metadata cache entry for the given `Cpv`.
    ///
    /// Looks up the **primary** (in-tree) store first, then the **secondary**
    /// (always-present writable store). Returns `Ok(None)` when neither has
    /// the entry. Freshness checks are the caller's responsibility.
    ///
    /// See [PMS 14 — Metadata Cache](https://projects.gentoo.org/pms/9/pms.html#metadata-cache).
    pub fn cache_entry(&self, cpv: &Cpv) -> Result<Option<CacheEntry>> {
        if let Some(entry) = self.primary.get(cpv)? {
            return Ok(Some(entry));
        }
        // Skip secondary when empty (common: in-memory test secondary, or
        // unused user-cache dir). Avoids a HashMap lock / dir probe per miss
        // when loading tens of thousands of ebuilds.
        if !self.secondary.is_populated() {
            return Ok(None);
        }
        self.secondary.get(cpv)
    }

    /// Persist `entry` to the secondary (always-writable) store.
    pub fn put_secondary(&self, cpv: &Cpv, entry: &CacheEntry) -> Result<()> {
        self.secondary.put(cpv, entry)
    }

    /// Prefer primary if it accepts a write; otherwise write secondary.
    ///
    /// Used by `em regen` when the in-tree cache may be unwritable.
    pub fn write_cache_entry(&self, cpv: &Cpv, entry: &CacheEntry) -> Result<()> {
        match self.primary.put(cpv, entry) {
            Ok(()) => Ok(()),
            Err(_) => self.secondary.put(cpv, entry),
        }
    }

    /// Whether the primary (in-tree) cache directory exists.
    pub fn has_primary_cache(&self) -> bool {
        self.primary.is_populated()
    }

    /// `{repo}/metadata/md5-cache/` — the directory PMS 14 places the cache in.
    ///
    /// Prefer [`Self::cache_entry`] / [`Self::write_cache_entry`] for entry I/O.
    pub(crate) fn cache_dir(&self) -> Utf8PathBuf {
        self.path.join("metadata").join("md5-cache")
    }

    /// Walk `metadata/md5-cache/` yielding every entry as `(Cpv, Result<CacheEntry>)`.
    ///
    /// The walk is parallel (via [`jwalk`]); parsing happens on demand as
    /// the iterator is consumed. Files whose name does not parse as a Cpv
    /// are skipped silently. I/O failures and parse errors on individual
    /// valid-named files come through as `Err` items so the consumer can
    /// decide whether to abort or continue.
    ///
    /// See [PMS 14 — Metadata Cache](https://projects.gentoo.org/pms/9/pms.html#metadata-cache).
    pub fn cache_entries(&self) -> CacheEntries {
        let walker = WalkDir::new(self.cache_dir())
            .skip_hidden(true)
            .min_depth(2)
            .max_depth(2);
        CacheEntries { walker }
    }

    /// Verify that `entry`'s recorded eclass checksums still match the live tree.
    ///
    /// For every `(name, md5)` in `entry.eclasses`, the eclass is located by
    /// searching this repository's `eclass/` directory and each master's (in
    /// order), then its current md5 is compared against the recorded one. An
    /// entry with no `_eclasses_` is trivially fresh.
    ///
    /// This does **not** verify `entry.md5` against the ebuild on disk —
    /// callers that want that check should compare it themselves (they
    /// already know which ebuild they're holding metadata for; this method
    /// has no Cpv to resolve a path from).
    ///
    /// Returns `false` if any eclass cannot be located, cannot be read, or
    /// hashes to a different value than the cache entry records.
    pub fn is_fresh(&self, entry: &CacheEntry, masters: &[Repository]) -> bool {
        self.is_fresh_cached(entry, masters, &mut std::collections::HashMap::new())
    }

    /// [`is_fresh`](Self::is_fresh) with a caller-held digest memo, for
    /// validating many entries against the same (small) set of eclasses —
    /// each eclass file is hashed once per memo instead of once per entry.
    pub fn is_fresh_cached(
        &self,
        entry: &CacheEntry,
        masters: &[Repository],
        digests: &mut std::collections::HashMap<String, Option<String>>,
    ) -> bool {
        if entry.eclasses.is_empty() {
            return true;
        }
        let eclass_dirs: Vec<Utf8PathBuf> = std::iter::once(self.path.join("eclass"))
            .chain(masters.iter().map(|m| m.path.join("eclass")))
            .collect();
        for (name, recorded) in &entry.eclasses {
            let actual = digests.entry(name.clone()).or_insert_with(|| {
                let path = find_eclass_in(&eclass_dirs, name)?;
                let bytes = std::fs::read(&path).ok()?;
                Some(format!("{:x}", md5::compute(&bytes)))
            });
            match actual {
                Some(d) if d.eq_ignore_ascii_case(recorded) => {}
                _ => return false,
            }
        }
        true
    }

    /// Parse `profiles/profiles.desc` to get available profile descriptions.
    ///
    /// See [PMS 5](https://projects.gentoo.org/pms/9/pms.html#profiles).
    pub fn profiles_desc(&self) -> Result<Vec<ProfileDesc>> {
        let lines = util::read_lines(self.path.join("profiles").join("profiles.desc"))?;
        let mut descs = Vec::new();
        for line in lines {
            descs.push(ProfileDesc::parse(&line)?);
        }
        Ok(descs)
    }

    /// Read the default EAPI for profiles in this repository.
    ///
    /// Returns `None` if `profiles/eapi` is absent (EAPI 0 is implied).
    ///
    /// See [PMS 4.4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn profiles_eapi(&self) -> Result<Option<Eapi>> {
        match util::read_single_line(self.path.join("profiles").join("eapi"))? {
            Some(s) => {
                let eapi = s.parse::<Eapi>().map_err(|e| {
                    Error::InvalidProfile(format!("bad EAPI in profiles/eapi: {e}"))
                })?;
                Ok(Some(eapi))
            }
            None => Ok(None),
        }
    }

    /// Parse the repository-level `profiles/package.mask`.
    ///
    /// These masks apply across all profiles in the repository and should
    /// be merged before any profile-stack masks.  Returns an empty `Vec`
    /// if the file is absent.
    ///
    /// See [PMS 4.4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn repo_package_mask(&self) -> Result<Vec<Dep>> {
        let lines = util::read_lines(self.path.join("profiles").join("package.mask"))?;
        lines
            .into_iter()
            .map(|l| Dep::parse(&l).map_err(Into::into))
            .collect()
    }

    /// Build a [`UseExpand`] grouper from this repository's `profiles/desc/` names.
    ///
    /// This is a convenience wrapper around [`Repository::use_expand_names`] that
    /// constructs the grouper ready for [`UseExpand::group`] calls.
    pub fn use_expand(&self) -> Result<UseExpand> {
        Ok(UseExpand::new(self.use_expand_names()?))
    }

    /// List available USE_EXPAND variable names from `profiles/desc/`.
    ///
    /// Returns the stem of each `.desc` file (e.g. `"cpu_flags_x86"`),
    /// sorted alphabetically.
    ///
    /// See [PMS 4.4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn use_expand_names(&self) -> Result<Vec<String>> {
        let dir: Utf8PathBuf = self.path.join("profiles").join("desc");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(util::io_err(&dir, e)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| util::io_err(&dir, e))?;
            let path: Utf8PathBuf = match entry.path().try_into() {
                Ok(p) => p,
                Err(_) => continue,
            };
            if let Some(fname) = path.file_name()
                && let Some(stem) = fname.strip_suffix(".desc")
                && !stem.starts_with('.')
            {
                names.push(stem.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Parse USE_EXPAND flag descriptions from `profiles/desc/{name}.desc`.
    ///
    /// Returns `(flag_name, description)` pairs.  Returns an empty `Vec`
    /// if the file does not exist.
    ///
    /// See [PMS 4.4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn use_expand_desc(&self, name: &str) -> Result<Vec<(String, String)>> {
        parse_desc_file(
            self.path
                .join("profiles")
                .join("desc")
                .join(format!("{name}.desc")),
        )
    }

    /// Parse all package-move and slot-move entries from `profiles/updates/`.
    ///
    /// Files are read in sorted order (oldest first by filename convention).
    /// Lines with unrecognised tags or parse errors are silently skipped.
    ///
    /// See [PMS 4.4.4](https://projects.gentoo.org/pms/9/pms.html#profiles-updates).
    pub fn profile_updates(&self) -> Result<Vec<ProfileUpdate>> {
        let dir: Utf8PathBuf = self.path.join("profiles").join("updates");
        let mut files: Vec<Utf8PathBuf> = match std::fs::read_dir(&dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let path: Utf8PathBuf = e.path().try_into().ok()?;
                    let name = path.file_name()?;
                    if name.starts_with('.') {
                        None
                    } else {
                        Some(path)
                    }
                })
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(util::io_err(&dir, e)),
        };
        files.sort();

        let mut updates = Vec::new();
        for file in files {
            for line in util::read_lines(&file)? {
                let mut parts = line.split_whitespace();
                match parts.next() {
                    Some("move") => {
                        let (Some(old_s), Some(new_s)) = (parts.next(), parts.next()) else {
                            continue;
                        };
                        let (Ok(old), Ok(new)) = (Cpn::parse(old_s), Cpn::parse(new_s)) else {
                            continue;
                        };
                        updates.push(ProfileUpdate::Move { old, new });
                    }
                    Some("slotmove") => {
                        let (Some(dep_s), Some(old_s), Some(new_s)) =
                            (parts.next(), parts.next(), parts.next())
                        else {
                            continue;
                        };
                        let Ok(dep) = Dep::parse(dep_s) else { continue };
                        updates.push(ProfileUpdate::SlotMove {
                            dep: Box::new(dep),
                            old_slot: old_s.to_string(),
                            new_slot: new_s.to_string(),
                        });
                    }
                    _ => continue, // unknown tag — skip
                }
            }
        }
        Ok(updates)
    }

    /// Open a profile directory relative to `profiles/`.
    pub fn profile(&self, relative_path: &str) -> Result<Profile> {
        let profile_path = self.path.join("profiles").join(relative_path);
        Profile::open(profile_path.into())
    }

    /// Build the full profile stack for a profile relative to `profiles/`.
    ///
    /// Follows `parent` files recursively and returns a [`ProfileStack`] with
    /// all ancestor profiles in resolution order.
    ///
    /// See [PMS 5.1](https://projects.gentoo.org/pms/9/pms.html#profiles).
    pub fn profile_stack(&self, relative_path: &str) -> Result<ProfileStack> {
        let profile_path = self.path.join("profiles").join(relative_path);
        ProfileStack::build(profile_path.into())
    }

    /// List available eclass names (without the `.eclass` extension).
    pub fn eclasses(&self) -> Result<Vec<String>> {
        let eclass_dir: Utf8PathBuf = self.path.join("eclass");
        let entries = match std::fs::read_dir(&eclass_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(util::io_err(&eclass_dir, e)),
        };

        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| util::io_err(&eclass_dir, e))?;
            let path: Utf8PathBuf = match entry.path().try_into() {
                Ok(p) => p,
                Err(_) => continue,
            };
            if let Some(stem) = path.file_name().and_then(|n| n.strip_suffix(".eclass")) {
                names.push(stem.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    /// List available license names from `licenses/`.
    pub fn licenses(&self) -> Result<Vec<String>> {
        list_dir_names(self.path.join("licenses"))
    }

    /// Architectures declared in `profiles/arch.list` (typed).
    ///
    /// Populated eagerly at `open()`. See
    /// [PMS 4.4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn arch_list(&self) -> &[Arch] {
        &self.arch_cache
    }

    /// Resolve an [`Arch`] to its Gentoo keyword string.
    pub fn arch_keyword<'a>(&self, arch: &'a Arch) -> &'a str {
        arch.as_str()
    }

    /// Extract the CPU architecture from a GNU CHOST triple.
    ///
    /// Returns `None` only when `chost` is empty.
    pub fn arch_from_chost(&self, chost: &str) -> Option<Arch> {
        Arch::from_chost(chost)
    }

    /// Parse global USE flag descriptions from `profiles/use.desc`.
    ///
    /// Returns `(flag_name, description)` pairs.
    pub fn use_desc(&self) -> Result<Vec<(String, String)>> {
        parse_desc_file(self.path.join("profiles").join("use.desc"))
    }

    /// Build a [`crate::UseDb`] for this repository.
    ///
    /// Combines global (`use.desc`) and package-local (`use.local.desc`)
    /// USE flag descriptions into an indexed structure with O(log n) lookup.
    pub fn use_db(&self) -> Result<crate::UseDb> {
        crate::UseDb::load(&self.path)
    }

    /// Parse per-package USE flag descriptions from `profiles/use.local.desc`.
    ///
    /// Returns `(Cpn, flag_name, description)` tuples.
    pub fn use_local_desc(&self) -> Result<Vec<(Cpn, String, String)>> {
        let lines = util::read_lines(self.path.join("profiles").join("use.local.desc"))?;
        let mut result = Vec::new();
        for line in lines {
            // Format: category/package:flag - description
            let Some((cpn_str, rest)) = line.split_once(':') else {
                continue;
            };
            let cpn = Cpn::parse(cpn_str)?;
            let (flag, desc) = if let Some((f, d)) = rest.split_once(" - ") {
                (f.to_string(), d.to_string())
            } else {
                (rest.to_string(), String::new())
            };
            result.push((cpn, flag, desc));
        }
        Ok(result)
    }

    /// Parse `profiles/thirdpartymirrors`.
    ///
    /// Returns `(mirror_name, [urls...])` pairs.
    pub fn thirdpartymirrors(&self) -> Result<Vec<(String, Vec<String>)>> {
        let lines = util::read_lines(self.path.join("profiles").join("thirdpartymirrors"))?;
        let mut result = Vec::new();
        for line in lines {
            let mut parts = line.split_whitespace();
            if let Some(name) = parts.next() {
                let urls: Vec<String> = parts.map(String::from).collect();
                result.push((name.to_string(), urls));
            }
        }
        Ok(result)
    }
}

fn materialise_secondary(repo_name: &str, spec: SecondarySpec) -> Arc<dyn MetadataCache> {
    match spec {
        SecondarySpec::Memory => Arc::new(MemoryMetadataCache::new()),
        SecondarySpec::UserRoot(root) => Arc::new(DirMetadataCache::new(root.join(repo_name))),
        SecondarySpec::Custom(c) => c,
    }
}

/// Locate `{name}.eclass` by searching `dirs` in order (first hit wins).
fn find_eclass_in(dirs: &[Utf8PathBuf], name: &str) -> Option<Utf8PathBuf> {
    let filename = format!("{name}.eclass");
    for dir in dirs {
        let path = dir.join(&filename);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// List file/directory names in a directory (sorted, skipping dotfiles).
fn list_dir_names(dir: impl AsRef<Path>) -> Result<Vec<String>> {
    let dir = dir.as_ref();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(util::io_err(dir, e)),
    };

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| util::io_err(dir, e))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !name.starts_with('.') && name != "CVS" {
            names.push(name.into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Parse a `flag - description` file format used by `use.desc` etc.
fn parse_desc_file(path: impl AsRef<Path>) -> Result<Vec<(String, String)>> {
    let lines = util::read_lines(path)?;
    let mut result = Vec::new();
    for line in lines {
        if let Some((flag, desc)) = line.split_once(" - ") {
            result.push((flag.to_string(), desc.to_string()));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create the minimal directory structure required by `Repository::builder().open`.
    fn make_test_repo(dir: &tempfile::TempDir) -> Repository {
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        Repository::builder()
            .in_memory_cache()
            .open(dir.path())
            .unwrap()
    }

    #[test]
    fn profiles_eapi_absent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        assert!(repo.profiles_eapi().unwrap().is_none());
    }

    #[test]
    fn profiles_eapi_returns_parsed_eapi() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::write(dir.path().join("profiles").join("eapi"), "5\n").unwrap();
        assert_eq!(repo.profiles_eapi().unwrap(), Some(Eapi::Five));
    }

    #[test]
    fn repo_package_mask_absent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        assert!(repo.repo_package_mask().unwrap().is_empty());
    }

    #[test]
    fn repo_package_mask_parses_atoms() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::write(
            dir.path().join("profiles").join("package.mask"),
            "# comment\ndev-libs/foo\ndev-libs/bar\n",
        )
        .unwrap();
        let masks = repo.repo_package_mask().unwrap();
        assert_eq!(masks.len(), 2);
    }

    #[test]
    fn use_expand_names_absent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        assert!(repo.use_expand_names().unwrap().is_empty());
    }

    #[test]
    fn use_expand_names_and_desc() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let desc_dir = dir.path().join("profiles").join("desc");
        std::fs::create_dir_all(&desc_dir).unwrap();
        std::fs::write(
            desc_dir.join("cpu_flags_x86.desc"),
            "mmx - MMX instruction support\nsse2 - SSE2 support\n",
        )
        .unwrap();

        let names = repo.use_expand_names().unwrap();
        assert_eq!(names, vec!["cpu_flags_x86"]);

        let descs = repo.use_expand_desc("cpu_flags_x86").unwrap();
        assert_eq!(descs.len(), 2);
        assert_eq!(
            descs[0],
            ("mmx".to_string(), "MMX instruction support".to_string())
        );
        assert_eq!(descs[1], ("sse2".to_string(), "SSE2 support".to_string()));
    }

    #[test]
    fn use_expand_desc_absent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        assert!(repo.use_expand_desc("nonexistent").unwrap().is_empty());
    }

    #[test]
    fn profile_updates_absent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        assert!(repo.profile_updates().unwrap().is_empty());
    }

    #[test]
    fn profile_updates_parses_move_and_slotmove() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let updates_dir = dir.path().join("profiles").join("updates");
        std::fs::create_dir_all(&updates_dir).unwrap();
        std::fs::write(
            updates_dir.join("1Q-2024"),
            "# comment\nmove dev-libs/foo dev-libs/bar\nslotmove >=dev-libs/baz-1.0 0 1\n",
        )
        .unwrap();

        let updates = repo.profile_updates().unwrap();
        assert_eq!(updates.len(), 2);
        assert!(matches!(&updates[0], ProfileUpdate::Move { old, new }
            if old.to_string() == "dev-libs/foo" && new.to_string() == "dev-libs/bar"));
        assert!(
            matches!(&updates[1], ProfileUpdate::SlotMove { old_slot, new_slot, .. }
            if old_slot == "0" && new_slot == "1")
        );
    }

    #[test]
    fn profile_updates_skips_unknown_tags() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let updates_dir = dir.path().join("profiles").join("updates");
        std::fs::create_dir_all(&updates_dir).unwrap();
        std::fs::write(
            updates_dir.join("1Q-2024"),
            "unknown_tag foo bar\nmove dev-libs/a dev-libs/b\n",
        )
        .unwrap();

        let updates = repo.profile_updates().unwrap();
        assert_eq!(updates.len(), 1);
        assert!(matches!(&updates[0], ProfileUpdate::Move { .. }));
    }

    #[test]
    fn category_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::create_dir_all(dir.path().join("dev-util")).unwrap();

        assert!(repo.category("dev-util").is_some());
        assert!(repo.category("nonexistent").is_none());
    }

    #[test]
    fn cache_entry_reads_md5_cache() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);

        let cpv = Cpv::parse("dev-util/foo-1.0").unwrap();
        let cache_dir = dir
            .path()
            .join("metadata")
            .join("md5-cache")
            .join("dev-util");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(
            cache_dir.join("foo-1.0"),
            "EAPI=8\nDESCRIPTION=test\nSLOT=0\n",
        )
        .unwrap();

        let entry = repo.cache_entry(&cpv).unwrap().expect("cache file present");
        assert_eq!(entry.metadata.eapi, Eapi::Eight);
        assert_eq!(entry.metadata.description, "test");
    }

    #[test]
    fn cache_entries_walks_md5_cache() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);

        let cache_root = dir.path().join("metadata").join("md5-cache");
        std::fs::create_dir_all(cache_root.join("dev-util")).unwrap();
        std::fs::create_dir_all(cache_root.join("sys-apps")).unwrap();
        std::fs::write(
            cache_root.join("dev-util").join("foo-1.0"),
            "EAPI=8\nDESCRIPTION=foo\nSLOT=0\n",
        )
        .unwrap();
        std::fs::write(
            cache_root.join("sys-apps").join("bar-2.1"),
            "EAPI=8\nDESCRIPTION=bar\nSLOT=0\n",
        )
        .unwrap();
        // Malformed filename — should be silently skipped.
        std::fs::write(cache_root.join("dev-util").join("not-a-cpv"), "EAPI=8\n").unwrap();

        let mut entries: Vec<(String, Result<CacheEntry>)> = repo
            .cache_entries()
            .into_iter()
            .map(|(cpv, r)| (cpv.to_string(), r))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "dev-util/foo-1.0");
        assert_eq!(entries[0].1.as_ref().unwrap().metadata.description, "foo");
        assert_eq!(entries[1].0, "sys-apps/bar-2.1");
        assert_eq!(entries[1].1.as_ref().unwrap().metadata.description, "bar");
    }

    #[test]
    fn cache_entries_surfaces_parse_errors() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let cache_root = dir.path().join("metadata").join("md5-cache");
        std::fs::create_dir_all(cache_root.join("dev-util")).unwrap();
        // Missing mandatory DESCRIPTION etc — parse should error.
        std::fs::write(cache_root.join("dev-util").join("foo-1.0"), "EAPI=8\n").unwrap();

        let entries: Vec<_> = repo.cache_entries().into_iter().collect();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].1.is_err());
    }

    #[test]
    fn cache_entry_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let cpv = Cpv::parse("dev-util/foo-1.0").unwrap();
        assert!(repo.cache_entry(&cpv).unwrap().is_none());
    }

    #[test]
    fn is_fresh_validates_eclass_md5_across_local_and_masters() {
        let local_dir = tempfile::tempdir().unwrap();
        let master_dir = tempfile::tempdir().unwrap();
        let local = make_test_repo(&local_dir);
        let master = make_test_repo(&master_dir);

        // Two eclasses: one in local, one only in master.
        std::fs::create_dir_all(local_dir.path().join("eclass")).unwrap();
        std::fs::create_dir_all(master_dir.path().join("eclass")).unwrap();
        std::fs::write(
            local_dir.path().join("eclass").join("local-only.eclass"),
            b"local body\n",
        )
        .unwrap();
        std::fs::write(
            master_dir.path().join("eclass").join("master-only.eclass"),
            b"master body\n",
        )
        .unwrap();

        let local_md5 = format!("{:x}", md5::compute(b"local body\n"));
        let master_md5 = format!("{:x}", md5::compute(b"master body\n"));

        // Construct a CacheEntry via parse() to avoid hand-building EbuildMetadata.
        let make_entry = |eclasses: &[(&str, &str)]| {
            let eclass_field = eclasses
                .iter()
                .map(|(n, m)| format!("{n}\t{m}"))
                .collect::<Vec<_>>()
                .join("\t");
            let raw = format!("EAPI=8\nDESCRIPTION=test\nSLOT=0\n_eclasses_={eclass_field}\n");
            CacheEntry::parse(&raw).unwrap()
        };

        // Both eclasses present and matching — fresh.
        let entry = make_entry(&[("local-only", &local_md5), ("master-only", &master_md5)]);
        assert!(local.is_fresh(&entry, std::slice::from_ref(&master)));

        // Wrong md5 for the master eclass — stale.
        let entry = make_entry(&[("master-only", "00000000000000000000000000000000")]);
        assert!(!local.is_fresh(&entry, std::slice::from_ref(&master)));

        // Eclass not findable anywhere — stale.
        let entry = make_entry(&[("ghost", &local_md5)]);
        assert!(!local.is_fresh(&entry, std::slice::from_ref(&master)));

        // Empty eclass list — trivially fresh.
        let entry = make_entry(&[]);
        assert!(local.is_fresh(&entry, &[]));
    }

    #[test]
    fn profiles_desc_parses() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::create_dir_all(dir.path().join("profiles").join("default").join("linux")).unwrap();
        std::fs::write(
            dir.path().join("profiles").join("profiles.desc"),
            "amd64 default/linux/amd64/23.0 stable\n",
        )
        .unwrap();

        let descs = repo.profiles_desc().unwrap();
        assert_eq!(descs.len(), 1);
        assert_eq!(descs[0].path(), "default/linux/amd64/23.0");
    }

    #[test]
    fn ebuilds_lists_ebuilds() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::write(dir.path().join("profiles").join("categories"), "dev-util\n").unwrap();
        let pkg_dir = dir.path().join("dev-util").join("foo");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("foo-1.0.ebuild"), "EAPI=8\n").unwrap();
        std::fs::write(pkg_dir.join("foo-2.0.ebuild"), "EAPI=8\n").unwrap();

        let ebuilds: Vec<_> = repo.ebuilds().unwrap().into_iter().collect();
        assert_eq!(ebuilds.len(), 2);
    }

    #[test]
    fn thirdpartymirrors_parses() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::write(
            dir.path().join("profiles").join("thirdpartymirrors"),
            "foo https://foo.com/mirror1 https://foo.com/mirror2\n",
        )
        .unwrap();

        let mirrors = repo.thirdpartymirrors().unwrap();
        assert_eq!(mirrors.len(), 1);
        assert_eq!(mirrors[0].0, "foo");
        assert_eq!(mirrors[0].1.len(), 2);
    }

    #[test]
    fn use_desc_parses() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::write(
            dir.path().join("profiles").join("use.desc"),
            "ssl - Enable SSL support\nzlib - Use zlib compression\n",
        )
        .unwrap();

        let descs = repo.use_desc().unwrap();
        assert_eq!(descs.len(), 2);
        assert_eq!(descs[0].0, "ssl");
        assert_eq!(descs[0].1, "Enable SSL support");
    }

    #[test]
    fn find_cpns_exact_cat_pkg() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::create_dir_all(dir.path().join("sys-apps").join("foo")).unwrap();
        std::fs::write(dir.path().join("profiles").join("categories"), "sys-apps\n").unwrap();

        let cpns = repo.find_cpns("sys-apps/foo");
        assert_eq!(cpns.len(), 1);
        assert_eq!(cpns[0].category.as_ref(), "sys-apps");
        assert_eq!(cpns[0].package.as_ref(), "foo");
    }

    #[test]
    fn find_cpns_bare_name_single_match() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::create_dir_all(dir.path().join("sys-apps").join("foo")).unwrap();
        std::fs::write(dir.path().join("profiles").join("categories"), "sys-apps\n").unwrap();

        let cpns = repo.find_cpns("foo");
        assert_eq!(cpns.len(), 1);
        assert_eq!(cpns[0].category.as_ref(), "sys-apps");
        assert_eq!(cpns[0].package.as_ref(), "foo");
    }

    #[test]
    fn find_cpns_bare_name_multiple_categories() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::create_dir_all(dir.path().join("sys-apps").join("bar")).unwrap();
        std::fs::create_dir_all(dir.path().join("app-misc").join("bar")).unwrap();
        std::fs::write(
            dir.path().join("profiles").join("categories"),
            "sys-apps\napp-misc\n",
        )
        .unwrap();

        let cpns = repo.find_cpns("bar");
        assert_eq!(cpns.len(), 2);
    }

    #[test]
    fn find_cpns_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::write(dir.path().join("profiles").join("categories"), "sys-apps\n").unwrap();

        let cpns = repo.find_cpns("nonexistent");
        assert!(cpns.is_empty());
    }

    #[test]
    fn find_cpns_cat_pkg_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::create_dir_all(dir.path().join("sys-apps").join("foo")).unwrap();
        std::fs::write(dir.path().join("profiles").join("categories"), "sys-apps\n").unwrap();

        let cpns = repo.find_cpns("sys-apps/nonexistent");
        assert!(cpns.is_empty());
    }
}
