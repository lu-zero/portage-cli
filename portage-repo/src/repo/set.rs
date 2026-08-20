//! Every repository one invocation searches or merges across, as one type

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use futures_util::Stream;
use futures_util::stream;
use portage_atom::{Cpn, Cpv};
use portage_metadata::CacheEntry;

use super::ebuild::Ebuild;
use super::repos_conf::RepoEntry;
use super::repository::{EbuildsIter, Repository};
use crate::error::Result;

/// Every repository one invocation searches or merges across, in priority
/// order, plus the virtual (alias) repos configured alongside them.
///
/// This is the whole "which repos" parameter — there is no second list. A
/// caller that used to take `repo: &Repository` plus a separately-threaded
/// `overlays`/`sources` list takes one `&RepoSet` instead.
#[derive(Clone)]
pub struct RepoSet {
    /// **Descending** `(priority, name)`: index 0 wins a duplicate cpv
    /// over index 1
    ///
    /// [`Self::ebuilds`]/callers merge in exactly this order.
    repos: Vec<Arc<Repository>>,
    /// Index into `repos` of the repo that names `RepoData::repo_name`
    ///
    /// Alias entries resolve their `source` against this repo. Always in
    /// range.
    main: usize,
    /// [`super::repos_conf::Location::Alias`] entries — virtual, no
    /// on-disk tree
    ///
    /// Part of "the repo world this command sees", so they travel with
    /// the set instead of being a second return value nobody can
    /// mis-pair with it.
    aliases: Vec<RepoEntry>,
}

impl RepoSet {
    /// One tree, no repos.conf: `--repo`, and the single-repo query applets
    pub fn single(main: Repository) -> Self {
        Self {
            repos: vec![Arc::new(main)],
            main: 0,
            aliases: Vec::new(),
        }
    }

    /// Pre-opened repos in descending-priority order. `main` indexes into
    /// `repos` and must be `< repos.len()`.
    pub fn from_ordered(repos: Vec<Arc<Repository>>, main: usize, aliases: Vec<RepoEntry>) -> Self {
        debug_assert!(main < repos.len(), "main index must be in range");
        Self {
            repos,
            main,
            aliases,
        }
    }

    /// Replace the alias set
    pub fn set_aliases(&mut self, aliases: Vec<RepoEntry>) {
        self.aliases = aliases;
    }

    /// Prepend caller-supplied aliases ahead of any already present
    ///
    /// E.g. `crossdev --setup -p`'s in-memory target, injected before
    /// whatever repos.conf itself already lists.
    pub fn prepend_aliases(&mut self, extra: &[RepoEntry]) {
        if extra.is_empty() {
            return;
        }
        let mut merged = extra.to_vec();
        merged.append(&mut self.aliases);
        self.aliases = merged;
    }

    /// The main repo
    ///
    /// Every *deliberately* main-only use goes through this and is
    /// greppable: profiles, make.conf, `use_env::build_use_env`,
    /// arch.list, `RepoData::repo_name`, alias sources.
    pub fn main(&self) -> &Repository {
        &self.repos[self.main]
    }

    /// Index of [`Self::main`] within [`Self::iter`]'s order
    pub fn main_index(&self) -> usize {
        self.main
    }

    /// Number of repos in the set (always `>= 1`)
    pub fn len(&self) -> usize {
        self.repos.len()
    }

    /// Never empty — a `RepoSet` always has at least its main repo
    pub fn is_empty(&self) -> bool {
        false
    }

    /// True when the set searches more than the main repo — the exact
    /// meaning `multi_repo` has at its display call sites ("… or overlays").
    pub fn is_multi(&self) -> bool {
        self.repos.len() > 1
    }

    /// Priority order (index 0 is highest-priority)
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Repository> + '_ {
        self.repos.iter().map(Arc::as_ref)
    }

    /// The repo at priority-order `index`
    pub fn get(&self, index: usize) -> &Repository {
        &self.repos[index]
    }

    /// First (highest-priority) repo with this name; `None` if unknown
    pub fn by_name(&self, name: &str) -> Option<&Repository> {
        self.iter().find(|r| r.name() == name)
    }

    /// `Location::Alias` entries configured alongside this set
    pub fn aliases(&self) -> &[RepoEntry] {
        &self.aliases
    }

    /// Union of [`Repository::find_cpns`] across the set, deduplicated and
    /// sorted by `Cpn`.
    ///
    /// Deliberately order-*independent*: a bare name is unique or ambiguous,
    /// and which repo answered cannot change that. Returning a sorted set is
    /// what makes that a property of the type rather than of each caller —
    /// and keeps `--ask`'s numbered candidate list stable across runs, the
    /// contract `Repository::find_cpns` already holds per repo.
    ///
    /// For a **bare** name, this also synthesises virtual [`Location::Alias`]
    /// repos (e.g. crossdev's `cross-<tuple>`): each alias whose source is
    /// the main repo contributes a `Cpn::new(dest_cat, name)` for every
    /// destination category it lists, the same synthesis `load_repos`
    /// applies building `RepoData`. Without this, a crossdev alias for
    /// `gcc` was invisible to bare-name resolution.
    ///
    /// [`Location::Alias`]: super::repos_conf::Location::Alias
    pub fn find_cpns(&self, pattern: &str) -> Vec<Cpn> {
        let mut found: BTreeSet<Cpn> = BTreeSet::new();
        for repo in self.iter() {
            found.extend(repo.find_cpns(pattern));
        }
        // Bare names only: a `cat/pkg` pattern either parsed to a real repo
        // above or doesn't exist, and an alias never widens a full atom.
        if pattern.find('/').is_none() {
            found.extend(self.alias_cpns_for_bare_name(pattern));
        }
        found.into_iter().collect()
    }

    /// The virtual cpns an alias repo contributes for a bare `name`: for each
    /// alias whose source is the main repo, one `Cpn::new(dest_cat, name)` per
    /// destination category that aliases a source cpn whose package matches.
    fn alias_cpns_for_bare_name(&self, name: &str) -> BTreeSet<Cpn> {
        let main_name = self.main().name();
        let mut out: BTreeSet<Cpn> = BTreeSet::new();
        for entry in &self.aliases {
            let super::repos_conf::Location::Alias { source, aliases } = &entry.location else {
                continue;
            };
            if *source != main_name {
                continue;
            }
            for (dest_cat, source_cpns) in aliases {
                // Only when some aliased source package's *package* part
                // matches — matches `load_repos`'s `Cpn::new(dest_cat,
                // source_cpn.package)` synthesis exactly.
                if source_cpns.iter().any(|cpn| cpn.package.as_ref() == name) {
                    out.insert(Cpn::new(dest_cat, name));
                }
            }
        }
        out
    }

    /// Every ebuild in the set, in priority order, **shadowed by cpv**: at
    /// most one [`Ebuild`] per cpv, from the highest-priority repo that has
    /// it — the same shadowing a `RepoData` merge applies to cache entries,
    /// so `em which` and the merge plan cannot disagree about which file a
    /// cpv is.
    ///
    /// Lazy: chains each repo's `Ebuilds` walker, filtering against the
    /// cpvs already emitted. The **main** repo failing to enumerate is an
    /// error; any other repo failing is `tracing::warn!`-ed and skipped —
    /// the same leniency an unopenable repos.conf entry and an unreadable
    /// master categories file already get elsewhere.
    pub fn ebuilds(&self) -> Result<EbuildsAcross<'_>> {
        // Validate main eagerly so its failure surfaces as this call's
        // `Err`, per this method's documented contract — iteration itself
        // always starts at priority index 0, not at main's position.
        self.main().ebuilds()?;
        let (index, current) = EbuildsAcross::open_from(self, 0);
        Ok(EbuildsAcross {
            set: self,
            index,
            current,
            seen: HashSet::new(),
        })
    }

    /// Priority-ordered [`Repository::cache_entry`]: first hit wins, with
    /// the index of the repo that served it (needed to attribute the source
    /// repo and to find the ebuild path).
    pub fn cache_entry(&self, cpv: &Cpv) -> Option<(usize, CacheEntry)> {
        for (index, repo) in self.repos.iter().enumerate() {
            if let Ok(Some(entry)) = repo.cache_entry(cpv) {
                return Some((index, entry));
            }
        }
        None
    }

    /// Every metadata cache entry across the set, in priority order,
    /// shadowed by cpv — the memoized counterpart to [`Self::ebuilds`].
    ///
    /// Sourced from [`crate::entries::repo_entries`] per repo: bulk read,
    /// per-entry suspect narrowing, secondary-cache and live-source
    /// fallback, with the sync-stamp/gap-index memo where supported — the
    /// fast path [`Self::ebuilds`]'s raw directory walk doesn't get, since
    /// it answers a different question. Prefer this whenever what's needed
    /// is metadata (DEPEND, DESCRIPTION, …), not a bare file listing.
    ///
    /// A [`Stream`], not a collected `Vec`: a later repo's bulk read is
    /// only awaited once a consumer asks for more than an earlier repo
    /// alone serves, so a first-match/early-exit caller never pays for the
    /// repos behind it. A caller needing every entry more than once
    /// collects it once itself (`StreamExt::collect`).
    pub fn entries(&self) -> impl Stream<Item = EntryIn<'_>> + '_ {
        stream::unfold(
            EntriesState {
                index: 0,
                seen: HashSet::new(),
                current: None,
            },
            move |mut state| async move {
                loop {
                    match state.current.as_mut().and_then(Iterator::next) {
                        Some((cpv, entry)) => {
                            if !self.is_multi() || state.seen.insert(cpv.clone()) {
                                let repo = self.get(state.index);
                                let index = state.index;
                                return Some((
                                    EntryIn {
                                        repo,
                                        index,
                                        cpv,
                                        entry,
                                    },
                                    state,
                                ));
                            }
                            // Lower-priority repo's duplicate of an
                            // already-emitted cpv — shadowed, not returned.
                        }
                        None => {
                            // `current` was never started (index 0) or just
                            // ran out — either way, move to the next repo.
                            if state.current.is_some() {
                                state.index += 1;
                            }
                            if state.index >= self.len() {
                                return None;
                            }
                            let fetched = crate::entries::repo_entries(self.get(state.index)).await;
                            state.current = Some(fetched.into_iter());
                        }
                    }
                }
            },
        )
    }
}

struct EntriesState {
    index: usize,
    seen: HashSet<Cpv>,
    current: Option<std::vec::IntoIter<(Cpv, CacheEntry)>>,
}

/// One [`RepoSet::entries`] item: metadata for one cpv, from the repo that
/// actually served it — carried alongside so a consumer can resolve the
/// ebuild's file path ([`Repository::ebuild_for_cpv`]) or read further
/// repo-scoped data without a second priority-ordered search.
pub struct EntryIn<'a> {
    /// The repo `entry` was found in
    pub repo: &'a Repository,
    /// `repo`'s index in the set's priority order
    pub index: usize,
    /// The cpv this entry describes
    pub cpv: Cpv,
    /// The metadata cache entry itself
    pub entry: CacheEntry,
}

/// One [`Ebuild`] from [`RepoSet::ebuilds`], carrying the repo it
/// actually came from
///
/// So a consumer reads metadata from the repo that owns the file
/// (`item.repo.cache_entry(item.ebuild.cpv())`) instead of doing a second
/// priority-ordered search that could answer from a different repo than
/// the one the path came from.
pub struct EbuildIn<'a> {
    /// The repo `ebuild` was found in
    pub repo: &'a Repository,
    /// `repo`'s index in the set's priority order
    pub index: usize,
    /// The ebuild itself
    pub ebuild: Ebuild,
}

/// Iterator produced by [`RepoSet::ebuilds`]
pub struct EbuildsAcross<'a> {
    set: &'a RepoSet,
    index: usize,
    current: Option<EbuildsIter>,
    /// Stays empty (and unconsulted — see `next`) for a single-repo set: no
    /// cpv can possibly repeat there, so there's nothing to dedupe against.
    seen: HashSet<Cpv>,
}

impl<'a> EbuildsAcross<'a> {
    /// The first enumerable repo at or after `from`, skipping (and warning
    /// about) any that fails — `(set.len(), None)` once every remaining repo
    /// has been tried.
    fn open_from(set: &'a RepoSet, mut from: usize) -> (usize, Option<EbuildsIter>) {
        while from < set.len() {
            let repo = set.get(from);
            match repo.ebuilds() {
                Ok(ebuilds) => return (from, Some(ebuilds.into_iter())),
                Err(e) => {
                    tracing::warn!(
                        "repo '{}': skipping (cannot enumerate ebuilds): {e}",
                        repo.name()
                    );
                    from += 1;
                }
            }
        }
        (from, None)
    }
}

impl<'a> Iterator for EbuildsAcross<'a> {
    type Item = EbuildIn<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let current = self.current.as_mut()?;
            match current.next() {
                Some(ebuild) => {
                    if !self.set.is_multi() || self.seen.insert(ebuild.cpv().clone()) {
                        let repo = self.set.get(self.index);
                        let index = self.index;
                        return Some(EbuildIn {
                            repo,
                            index,
                            ebuild,
                        });
                    }
                    // Lower-priority repo's duplicate of an already-emitted
                    // cpv — shadowed, not returned.
                }
                None => {
                    let (index, current) = Self::open_from(self.set, self.index + 1);
                    self.index = index;
                    self.current = current;
                    self.current.as_ref()?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_atom::interner::Interned;

    fn make_repo(dir: &tempfile::TempDir, categories: &[&str]) -> Repository {
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(
            dir.path().join("profiles").join("categories"),
            categories.join("\n"),
        )
        .unwrap();
        Repository::builder()
            .in_memory_cache()
            .open(dir.path())
            .unwrap()
    }

    fn add_ebuild(dir: &tempfile::TempDir, cat: &str, pkg: &str, version: &str) {
        let pkg_dir = dir.path().join(cat).join(pkg);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join(format!("{pkg}-{version}.ebuild")),
            "EAPI=8\nSLOT=0\n",
        )
        .unwrap();
    }

    #[test]
    fn single_has_one_repo_and_is_not_multi() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir, &[]);
        let set = RepoSet::single(repo);
        assert_eq!(set.len(), 1);
        assert!(!set.is_multi());
        assert_eq!(set.main_index(), 0);
    }

    #[test]
    fn from_ordered_exposes_priority_order_and_by_name() {
        let main_dir = tempfile::tempdir().unwrap();
        let overlay_dir = tempfile::tempdir().unwrap();
        let main = make_repo(&main_dir, &[]);
        let overlay = make_repo(&overlay_dir, &[]);
        let main_name = main.name();
        let overlay_name = overlay.name();

        // Overlay ranked above main, as a real higher-priority overlay would be.
        let set = RepoSet::from_ordered(vec![Arc::new(overlay), Arc::new(main)], 1, Vec::new());

        assert!(set.is_multi());
        assert_eq!(set.main().name(), main_name);
        assert_eq!(set.get(0).name(), overlay_name);
        assert_eq!(
            set.by_name(overlay_name.as_str()).unwrap().name(),
            overlay_name
        );
        assert!(set.by_name("nonexistent").is_none());
    }

    #[test]
    fn find_cpns_unions_and_dedupes_across_repos() {
        let main_dir = tempfile::tempdir().unwrap();
        let overlay_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(main_dir.path().join("sys-apps").join("foo")).unwrap();
        std::fs::create_dir_all(overlay_dir.path().join("app-misc").join("bar")).unwrap();
        let main = make_repo(&main_dir, &["sys-apps"]);
        let overlay = make_repo(&overlay_dir, &["app-misc"]);
        let set = RepoSet::from_ordered(vec![Arc::new(main), Arc::new(overlay)], 0, Vec::new());

        let mut names: Vec<String> = set
            .find_cpns("foo")
            .into_iter()
            .chain(set.find_cpns("bar"))
            .map(|c| c.to_string())
            .collect();
        names.sort();
        assert_eq!(names, vec!["app-misc/bar", "sys-apps/foo"]);
    }

    // A bare name that matches a [`Location::Alias`] repo's source package
    // resolves to the synthesised cross cpn (`cross-<tuple>/<pkg>`), matching
    // what `load_repos` injects into `RepoData` — previously `find_cpns`
    // ignored aliases entirely, so a crossdev alias for e.g. `gcc` was
    // invisible to bare-name resolution even though `cross-.../gcc` resolved
    // fine as a full atom. Only aliases whose source is the main repo are
    // considered (the same gate `load_repos` applies).
    #[test]
    fn find_cpns_synthesises_alias_cross_cpns_for_a_bare_name() {
        use std::collections::{HashMap, HashSet};

        let main_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(main_dir.path().join("sys-devel").join("gcc")).unwrap();
        let main = make_repo(&main_dir, &["sys-devel"]);

        let real_cpn = Cpn::parse("sys-devel/gcc").unwrap();
        let cross_cat = "cross-riscv64-unknown-linux-gnu";
        let mut aliases: HashMap<String, HashSet<Cpn>> = HashMap::new();
        aliases.insert(cross_cat.to_string(), [real_cpn].into_iter().collect());
        let alias_entry = super::super::repos_conf::RepoEntry {
            name: "crossdev".into(),
            location: super::super::repos_conf::Location::Alias {
                source: main.name(),
                aliases,
            },
            masters: None,
            sync_type: None,
            sync_uri: None,
            auto_sync: false,
            volatile: None,
            priority: None,
        };

        let mut set = RepoSet::single(main);
        set.set_aliases(vec![alias_entry]);

        let found: Vec<String> = set
            .find_cpns("gcc")
            .into_iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(
            found,
            vec![
                "cross-riscv64-unknown-linux-gnu/gcc".to_string(),
                "sys-devel/gcc".to_string(),
            ],
            "bare name must resolve to both the real cpn and the alias's cross cpn"
        );
    }

    // An alias whose source is *not* the main repo is not synthesised — the
    // same gate `load_repos` applies (it can't disambiguate a same-named cpn
    // coming from a non-main source). The bare name still resolves against
    // real repos only.
    #[test]
    fn find_cpns_ignores_an_alias_whose_source_is_not_main() {
        use std::collections::{HashMap, HashSet};

        let main_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(main_dir.path().join("sys-devel").join("gcc")).unwrap();
        let main = make_repo(&main_dir, &["sys-devel"]);

        let real_cpn = Cpn::parse("sys-devel/gcc").unwrap();
        let mut aliases: HashMap<String, HashSet<Cpn>> = HashMap::new();
        aliases.insert(
            "cross-riscv64-unknown-linux-gnu".to_string(),
            [real_cpn].into_iter().collect(),
        );
        let alias_entry = super::super::repos_conf::RepoEntry {
            name: "crossdev".into(),
            location: super::super::repos_conf::Location::Alias {
                source: Interned::intern("some-other-repo"),
                aliases,
            },
            masters: None,
            sync_type: None,
            sync_uri: None,
            auto_sync: false,
            volatile: None,
            priority: None,
        };

        let mut set = RepoSet::single(main);
        set.set_aliases(vec![alias_entry]);

        let found: Vec<String> = set
            .find_cpns("gcc")
            .into_iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(
            found,
            vec!["sys-devel/gcc".to_string()],
            "a non-main-sourced alias must not synthesise a cross cpn"
        );
    }

    #[test]
    fn ebuilds_shadows_a_duplicate_cpv_by_priority() {
        let main_dir = tempfile::tempdir().unwrap();
        let overlay_dir = tempfile::tempdir().unwrap();
        add_ebuild(&main_dir, "sys-apps", "foo", "1.0");
        add_ebuild(&overlay_dir, "sys-apps", "foo", "1.0");
        add_ebuild(&overlay_dir, "app-misc", "bar", "2.0");
        let main = make_repo(&main_dir, &["sys-apps"]);
        let overlay = make_repo(&overlay_dir, &["sys-apps", "app-misc"]);
        let overlay_name = overlay.name();

        // Overlay ranked above main: its copy of sys-apps/foo-1.0 should win.
        let set = RepoSet::from_ordered(vec![Arc::new(overlay), Arc::new(main)], 1, Vec::new());

        let items: Vec<_> = set.ebuilds().unwrap().collect();
        assert_eq!(
            items.len(),
            2,
            "the duplicate cpv must be shadowed, not duplicated"
        );
        let foo = items
            .iter()
            .find(|i| i.ebuild.cpv().cpn.package.as_ref() == "foo")
            .unwrap();
        assert_eq!(foo.repo.name(), overlay_name, "higher-priority repo wins");
        assert_eq!(foo.index, 0);
    }

    #[test]
    fn cache_entry_returns_the_highest_priority_hit_with_its_index() {
        let main_dir = tempfile::tempdir().unwrap();
        let overlay_dir = tempfile::tempdir().unwrap();
        let main = make_repo(&main_dir, &[]);
        let overlay = make_repo(&overlay_dir, &[]);

        let cache_dir = overlay_dir
            .path()
            .join("metadata")
            .join("md5-cache")
            .join("sys-apps");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(cache_dir.join("foo-1.0"), "EAPI=8\nDESCRIPTION=t\nSLOT=0\n").unwrap();

        let set = RepoSet::from_ordered(vec![Arc::new(main), Arc::new(overlay)], 0, Vec::new());
        let cpv = portage_atom::Cpv::parse("sys-apps/foo-1.0").unwrap();

        let (index, entry) = set.cache_entry(&cpv).expect("overlay's entry found");
        assert_eq!(index, 1);
        assert_eq!(entry.metadata.description, "t");
    }
}
