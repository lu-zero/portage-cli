# Repo/overlay abstraction redesign

**Current pin:** `master` @ `684ad7a`

**Post-landing benchmark finding (2026-08-11):** `em depends`/`which`
(step 4) are measurably slower than before this redesign — on this host,
`query which`/`query depends` +16-21% (real: ~+33ms / ~+155ms). Root
cause, isolated via `--repo`-scoped hyperfine runs against each configured
repo individually: `which.rs`/`depends.rs` call `RepoSet::ebuilds()`, a
**raw `jwalk` of every repo's ebuild files** — it shares none of
`repo_entries()`'s cache-freshness memoization (that machinery is only on
the metadata-*cache*-loading side, used by `resolve`/`load_repos`). Adding
overlays means walking more files (~12% more on this host: `guru`
3717 + `crossdev` 281 + `exp-llvm-libc` 3, alongside `gentoo`'s 32636),
and `depends` additionally does a per-file `Repository::cache_entry()`
read for all of them, not just main's. This is the direct, proportional,
*expected* cost of "scan every repo instead of just main" — not a bug,
just not quantified until benchmarked post-landing. Two real (but
tangential) perf bugs were found and fixed along the way and do **not**
resolve this: `ed05e1b`'s follow-up `1a01a0f` (EbuildsAcross's dedup
`HashSet` was paying a clone+hash+insert per ebuild even for a
single-repo set) and `684ad7a` (repo_entries' bulk read never consulted
the secondary/durable cache, only the in-tree one — real gap, but
`which`/`depends` don't call `repo_entries` at all, so it doesn't touch
this specific regression). **Open, undecided:** accept this as the cost
of step 4's feature, or give `which`/`depends` the same cache-based fast
path `repo_entries()` already has instead of a raw ebuild-file walk.

**Status: implemented and landed (2026-08-11).** Steps 1-4 of the Opus
plan below are done, each its own commit, tests passing, live-verified
against the real repo set on this host (including `guru`):

- `5487796` refactor(repo): give Repository ownership of its own masters
  (foundation step, redone cleanly after the prior session's commits were
  reset — the plan assumes this exists)
- `225ba3e` feat(repo): add RepoSet, the merged main+overlays view (step 1)
- `ed05e1b` perf(repo): make primary_entries' suspect rule per-entry (step 2a)
- `47cf02e` fix(repo): gate the warm-path memo on has_sync_marker (step 2b)
- `2de9ffb` refactor(repo): collapse primary_entries/overlay_entries into
  repo_entries (step 2c — the guru trust/perf change)
- `e12cf3e` refactor(query): migrate the repo+overlays call chain to
  RepoSet, delete RepoSource (step 3)
- `169d445` feat(query): em depends/which now see overlay packages (step 4)

**Not done — explicit follow-ups, out of scope for this pass** (plan §5,
steps 5-6): `DepgraphOpts { repos: &RepoSet }` replacing `repo_path` +
`multi_repo` (removes the double repo-set build per merge —
`emerge.rs`/`dispatch.rs` build one set to resolve atoms, `depgraph()`
builds a second one internally from `repo_path`); masters dedup/arena
(`Repository::masters` is still deep-owned, `gentoo` gets opened once per
repos.conf entry that has it as a master); `search.rs` still untouched;
`Location::Alias` repos still can't appear in `RepoSet::find_cpns`
(bare-name cross-atoms); single-repo query applets
(`keywords`/`meta`/`uses`) still can't see overlays (unaffected by this
pass, `RepoSet::single` makes that explicit rather than hidden behind a
`&[]` sentinel). See plan §6 for the full list of what doesn't fit
cleanly.

See "## Opus design plan (2026-08-11)" below for the full plan this
implementation followed — read it before touching any of the follow-ups.
It supersedes the open questions in the sections above where they overlap
(in particular: the sync-stamp generalization question is answered, and
the `masters().is_empty()` routing idea from the "Direction" section below
is explicitly rejected with evidence — see plan §3).

---

## The problem

Every caller that needs to search or merge across "main repo + configured
overlays" today takes a `repo: &Repository` *plus* a separately-threaded
`overlays`/`sources: &[RepoSource]` list, and loops over both:
`query::resolve_atom`, `query::resolve_atoms`, `query::depends::run`,
`query::which::run`, `emerge_atoms_inner` (`emerge.rs`), `dispatch.rs`'s
Depends/Depgraph/Which arms, `portage_resolve::repo::load_repos`.

This is the exact same shape as the `Repository` + separately-threaded
`masters: &[Repository]` bug fixed earlier this session (commit `0a0f3e5`,
"give Repository ownership of its own masters") — a primary thing plus a
side list of related things, threaded together through every consumer
instead of being folded into one type. Fixing it one call site at a time
(renaming `overlays_from_conf`→`merge_sources_from_conf`, deleting a lazy
variant, fixing `depends`/`which` to actually scan overlays) kept treating
symptoms, not the shape. Luca, twice in one session: "the whole layering of
overlays as a bolt is just pure retardation."

## Direction (Luca's, 2026-08-11)

1. **Overlays should be invisible above the repo layer.** Push "search main
   + overlays" down into the repository abstraction itself — something
   `RepoSet`-shaped that presents the same interface `Repository` already
   does (`find_cpns`, `ebuilds`, `cache_entry`, …) so upper layers call one
   method on one object and never know overlays exist as a distinct
   concept. `resolve_atom`/`depends::run`/`which::run`/`load_repos` should
   each take exactly one parameter for "which repos to search," not two.
2. **Stop cloning `Repository` to build "the main source."** Cloning is
   cheap today (`#[derive(Clone)]`, path+layout+name+arch_cache+2 `Arc`s)
   but the target is a reference (`Arc<Repository>` or similar) — and,
   longer-term, an arena where `Repository` values live once and
   masters/overlay-set membership are indices into it, rather than each
   `Repository` owning full (recursively cloneable) copies of its masters.
3. **The main-vs-overlay "trust" framing is wrong, not just the plumbing
   shape.** An abandoned WIP this session (stashed, see below) introduced
   `CacheTrust::Primary | Verified` to route between
   `portage_repo::primary_entries` (main) and `overlay_entries` (overlays).
   Luca: "demented" — nothing prevents hand-editing an ebuild in the main
   tree. On re-reading `portage-repo/src/overlay.rs`: `primary_entries`
   isn't actually blind trust — it uses `sync_stamp`/a gap-index sidecar to
   skip re-verifying an *unchanged* tree, and still digests any ebuild
   newer than the last recorded sync ("suspects"). That's a legitimate
   freshness optimization, unrelated to main-vs-overlay identity.
   `overlay_entries`, by contrast, unconditionally re-reads and md5's
   *every* ebuild on *every* call, with **no** sync-stamp/gap-index
   shortcut at all. Confirmed costly, not just theoretical: `guru` on this
   host has 3717 ebuilds and ships its own md5-cache (3716 entries,
   ~1:1) — every resolve that touches `guru` currently pays a full,
   unconditioned re-verify of thousands of files, something main never
   pays past the first run. The real distinguishing property Luca named:
   a main repo is self-contained (its own eclasses/profiles are local —
   `Repository::masters()` is empty); an overlay depends on a master for
   eclasses (`Repository::masters()` non-empty — already drives the
   `master_cache_entry` symlink-shortcut in `overlay.rs` and the eclass-dir
   search in `is_fresh_cached`). That's real, structural, and already
   sitting on `Repository` since the masters-ownership fix — no separate
   enum needed to express it.

## Open question to resolve during the redesign

Should the `sync_stamp`/gap-index freshness shortcut (currently
`primary_entries`-only) generalize to *any* repo, masters-having or not,
rather than being main-only? It looks like an unclaimed, real perf win for
large cache-shipping overlays like `guru` — but verify it's actually safe
to generalize (does the gap-index sidecar handle an overlay's cache being
regenerated out-of-band the same way it handles main's?) before assuming
so.

## Abandoned WIP

`stash@{0}` in the main `portage-cli` worktree: "WIP: RepoSource/CacheTrust
unification (repo+overlays bolt-on removal) - does not compile" — a
`CacheTrust`-tagged `{ repo, trust }` struct replacing the old
`RepoSource::Main | Overlay` enum, still threaded as a side list. Does
**not** satisfy point 1 above (still a side list, just a less-broken
element type), and its core `CacheTrust` concept is exactly what point 3
says is wrong. Discard and redesign from scratch rather than resume it —
don't `git stash pop` this expecting it to be a starting point, it isn't.

## Where to start reading

- `portage-resolve/src/repo.rs` — `RepoSource` enum (as of `b4d5af2`, still
  `Main | Overlay(Repository)`), `load_repos`.
- `portage-cli/src/repo_open.rs` — `merge_sources_from_conf` (builds the
  `Vec<RepoSource>`, splices main in at its repos.conf priority rank).
- `portage-cli/src/query/mod.rs` — `resolve_atom`/`resolve_atoms`, the
  current repo-then-overlays search loop.
- `portage-cli/src/query/depends.rs`, `which.rs` — the two commands fixed
  this session to actually scan overlay ebuilds (not just resolve names
  against them); their `scan_repo_for_reverse_deps`/ebuild-collection loops
  are exactly the kind of per-call-site iteration a `RepoSet` abstraction
  should absorb.
- `portage-repo/src/overlay.rs` — `primary_entries`/`overlay_entries`,
  `sync_stamp`/gap-index mechanics, `master_cache_entry`.
- `portage-repo/src/repo/repository.rs` — `Repository::masters()`, the
  precedent for "fold the side list into the primary type" (commit
  `0a0f3e5`, same session).

## Out of scope for this pass

- Anything not touching the repo/overlay search-and-merge path — this is
  scoped to the shape described above, not a general refactor pass.

## Opus design plan (2026-08-11)

Produced by a Plan-mode Opus agent given this whole file plus the file list
above as its starting context; it also read `git stash show -p stash@{0}`
to see what was already tried and rejected. Reproduced verbatim below —
this is the authoritative implementation plan for this task; treat the
"Direction"/"Open question" sections above as superseded wherever they
disagree with this (they don't disagree on the goal, only on some of the
mechanism — e.g. `masters().is_empty()` as the routing signal, proposed
above, is explicitly rejected in §3 below with live evidence).

---

### 0. Two premise corrections found while reading

Both change the answer to the open question, so they come first.

**(a) `primary_entries`' warm path does not re-verify anything — not even by mtime.**
`portage-repo/src/overlay.rs:90-120`: when `sync_stamp` matches the gap-index sidecar's first line, the function does a bulk cache read plus a `cache_entry` lookup for the memoised gap CPVs, and returns. It never stats an ebuild. The mtime "suspect" rule (`overlay.rs:152-165`) only runs on the *cold* path.

`Repository::sync_stamp` (`repository.rs:442-463`) = `mtime:len` of `metadata/timestamp.chk` ⊕ `mtime:len` of the **repo root directory**. Editing `<repo>/dev-libs/foo/foo-1.ebuild` changes neither. So a hand-edited main-repo ebuild between syncs is served from the stale cache entry indefinitely. This session's own claim ("it already correctly re-verifies a hand-edited main-repo ebuild via mtime") is true of the cold path only. That hole is tolerable for main (a sync-managed tree) and is *not* tolerable for the repos overlays exist for.

**(b) `depends`/`which` are currently main-only, not "scan main then overlays".**
`b4d5af2` removed the overlay parameter from both (`depends.rs:15-20`, `which.rs:17-22`). So they are a *gap to close*, not a loop to absorb — and closing it is a user-visible behavior change that needs its own commit.

Measured corroboration for the rest (this host): `guru` 3717 ebuilds / 3716 cache entries, and **every** ebuild mtime, `metadata/timestamp.chk` mtime and the repo root mtime are all exactly `1785517141` (the sync). `gentoo`'s newest ebuild mtime (`1786119796`) is well past its `timestamp.chk` (`1785517137`), so its suspect set is genuinely non-empty. `crossdev` and `pentoo` have **no** `metadata/md5-cache` and **no** `timestamp.chk`. `~/.cache/em/md5-cache/gentoo/gap-index` exists; no other repo has one.

### 1. The type

Lives in `portage-repo` (`portage-repo/src/repo/set.rs`, re-exported as `portage_repo::RepoSet`) — both `portage-resolve` and `portage-cli` need it and neither owns `Repository`.

```rust
/// Every repository one invocation searches or merges across, in priority
/// order, plus the virtual (alias) repos configured alongside them.
///
/// This is the whole "which repos" parameter. There is no second list.
#[derive(Clone)]
pub struct RepoSet {
    /// **Descending** repos.conf `(priority, name)`: index 0 wins a duplicate
    /// cpv over index 1. `load_repos` merges in exactly this order.
    repos: Vec<Arc<Repository>>,
    /// Index of the repo that names `RepoData::repo_name` and that alias
    /// entries resolve their `source` against. Always in range.
    main: usize,
    /// `Location::Alias` entries — virtual, no tree. Part of "the repo world
    /// this command sees", so they travel with the set instead of being a
    /// second return value nobody can mis-pair.
    aliases: Vec<RepoEntry>,
}
```

`Arc<Repository>`, not `&'a Repository` and not `Repository`:
- satisfies "a reference, not a clone" (this file's own earlier wording: "`Arc<Repository>` or similar"); building the set costs one refcount bump for main, not a `Repository` clone;
- `RepoSet: Send + Sync + 'static` (`MetadataCache: Send + Sync`, `metadata_cache.rs:23`), so `&RepoSet` held across `.await` in `load_repos` and inside `tokio::join!` is trivially fine, and the set can be *moved* into `spawn`/`spawn_blocking` later (see §6);
- a borrowed `RepoSet<'a>` would force every caller to hold a `Vec<Repository>` alongside the view — the same two-things-threaded-together shape we are deleting, one level down;
- it is the arena's stepping stone: swapping `Vec<Arc<Repository>>` for `(Arc<RepoArena>, Vec<u32>)` changes no method signature below.

#### Constructors

```rust
impl RepoSet {
    /// One tree, no repos.conf: `--repo`, and the single-repo query applets.
    pub fn single(main: Repository) -> Self;

    /// Pre-opened repos in descending-priority order. `main` indexes into
    /// `repos` and must be < repos.len(). Used by the conf loader in
    /// portage-cli (which owns the XDG cache root and the warn UI).
    pub fn from_ordered(repos: Vec<Arc<Repository>>, main: usize, aliases: Vec<RepoEntry>) -> Self;

    pub fn set_aliases(&mut self, aliases: Vec<RepoEntry>);
    /// Prepend caller-supplied aliases (depgraph's `extra_aliases`).
    pub fn prepend_aliases(&mut self, extra: &[RepoEntry]);
}
```

#### Identity accessors

```rust
    /// The main repo. Every *deliberately* main-only use goes through this and
    /// is greppable: profiles, make.conf, `use_env::build_use_env`, arch.list,
    /// `RepoData::repo_name`, alias sources.
    pub fn main(&self) -> &Repository;
    pub fn main_index(&self) -> usize;
    pub fn len(&self) -> usize;
    /// True when the set searches more than the main repo — the exact meaning
    /// `multi_repo` has at its display call sites ("… or overlays").
    pub fn is_multi(&self) -> bool { self.repos.len() > 1 }
    /// Priority order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Repository> + '_;
    pub fn get(&self, index: usize) -> &Repository;
    /// First (highest-priority) repo with this name; `None` if unknown.
    pub fn by_name(&self, name: &str) -> Option<&Repository>;
    pub fn aliases(&self) -> &[RepoEntry];
```

**No `Deref<Target = Repository>`.** It would read nicely (`set.name()`), but every *future* `Repository` method would silently become main-only through the set — precisely the failure class being removed. `main()` makes each such use a visible decision.

#### The `Repository`-shaped methods

```rust
    /// Union of `Repository::find_cpns` across the set, deduplicated and
    /// sorted by `Cpn`.
    ///
    /// Deliberately order-*independent*: a bare name is unique or ambiguous,
    /// and which repo answered cannot change that. Returning a sorted set is
    /// what makes that a property of the type rather than of each caller —
    /// and keeps `--ask`'s numbered candidate list stable across runs, the
    /// contract `Repository::find_cpns` already holds per repo
    /// (`find_cpns_bare_name_order_is_deterministic`).
    pub fn find_cpns(&self, pattern: &str) -> Vec<Cpn>;

    /// Every ebuild in the set, in priority order, **shadowed by cpv**: at
    /// most one `Ebuild` per cpv, from the highest-priority repo that has it
    /// — the same shadowing `load_repos` applies to cache entries, so
    /// `em which` and the merge plan cannot disagree about which file a cpv is.
    ///
    /// Lazy (chains each repo's `Ebuilds` walker, filtering against the cpvs
    /// already emitted). The **main** repo failing to enumerate is an error;
    /// any other repo failing is `tracing::warn!`-ed and skipped — the same
    /// leniency `merge_sources_from_conf` already gives an unopenable repo
    /// and `Repository::ebuilds` gives an unreadable master categories file.
    pub fn ebuilds(&self) -> Result<EbuildsAcross<'_>>;

    /// Priority-ordered `Repository::cache_entry`: first hit wins, with the
    /// index of the repo that served it (needed to attribute `::repo` and to
    /// find the ebuild path).
    pub fn cache_entry(&self, cpv: &Cpv) -> Option<(usize, CacheEntry)>;
```

with

```rust
pub struct EbuildIn<'a> { pub repo: &'a Repository, pub index: usize, pub ebuild: Ebuild }
pub struct EbuildsAcross<'a> { /* … */ }
impl<'a> Iterator for EbuildsAcross<'a> { type Item = EbuildIn<'a>; }
```

The item carries its repo so a walking consumer reads metadata from the repo that actually owns the file (`item.repo.cache_entry(item.ebuild.cpv())`) instead of doing a second priority-ordered search that could answer from a different repo than the one the path came from.

#### How both orderings coexist in one type

One ordered vector, three accessors with three explicitly documented contracts:

| accessor | order contract | consumer |
|---|---|---|
| `iter()` | priority order is load-bearing; first writer per cpv wins | `load_repos` merge |
| `ebuilds()` | priority order, first-wins shadowing per cpv | `which`, `depends` |
| `find_cpns()` | order-independent: deduped + sorted set | `resolve_atom` ambiguity/`--ask` |

Nothing outside the type can pick the wrong one, because the wrong one does not exist at the call site any more: the search functions never see the vector.

### 2. Building the set — where the repos.conf logic goes

The *ordering policy* is already in `ReposConf::load_from` (`repos_conf.rs:201-222`: main's `-1000` default, ascending `(priority, name)` sort). `merge_sources_from_conf` only reverses it and splices main in. That reversal + open loop moves into `portage-cli/src/repo_open.rs` (it needs `crate::xdg::md5_cache_root()` and `style::warn_line!`, both CLI concerns), returning the set:

```rust
/// The full priority-ordered repo set for this invocation: `main` plus every
/// repos.conf overlay, descending by (priority, name). Takes `main` by value —
/// the CLI opens it once and hands it over; nothing clones a `Repository`.
pub fn repo_set_from_conf(main: Repository, roots: &Roots, multi_repo: bool) -> RepoSet
```

Body, straight from today's `merge_sources_from_conf`:

```rust
if !multi_repo { return RepoSet::single(main); }
let Ok(conf) = roots.repos_conf() else { return RepoSet::single(main) };
let main = Arc::new(main);
let repos_dir = main.path().parent()…;
let mut repos = Vec::new();
let mut main_index = None;
for e in conf.repos().iter().rev() {                 // descending priority
    if e.location.as_path() == Some(main.path().as_std_path()) {
        main_index = Some(repos.len());
        repos.push(Arc::clone(&main));               // refcount bump, no clone
        continue;
    }
    let Some(path) = e.location.as_path() else { continue };  // alias: below
    match open_with_masters(path, &repos_dir) {
        Ok(r)  => repos.push(Arc::new(r)),
        Err(e) => { warn_line!("skipping repo '{}' at {}: {e}", …); }
    }
}
// Hardening (see below): main is in the set unconditionally.
let main_index = main_index.unwrap_or_else(|| { repos.push(Arc::clone(&main)); repos.len() - 1 });
let aliases = conf.repos().iter().filter(|e| matches!(e.location, Location::Alias{..})).cloned().collect();
RepoSet::from_ordered(repos, main_index, aliases)
```

Note the `unwrap_or_else`: **today, if no repos.conf entry's `location` string-equals `main.path()`, `RepoSource::Main` is never produced and the main repo is silently dropped from the merge entirely.** `Cli::repo_path()` (`cli.rs:578-594`) normally derives main's path from the same `ReposConf` entry, so the strings match — but its `/var/db/repos/gentoo` fallback, or any trailing-slash/symlink divergence, breaks it. The set makes main structurally unconditional (`main: usize` cannot be absent). Flag as a fix, not a refactor.

#### Lifetimes across `.await`

There is no lifetime shape to manage. `RepoSet` is owned and `'static`; each consumer takes `&RepoSet`. `load_repos(&RepoSet)` holds it across `.await` inside `tokio::join!` in `world.rs:50` and `depgraph/mod.rs:276` exactly as `&Repository` is held today. `Repository: Send + Sync` already (it is cloned into a `move` closure in `regen_cache`, `cache.rs:168`), so `&RepoSet: Send` and the futures stay `Send`.

### 3. `primary_entries` / `overlay_entries` — collapse, but not on `masters()`

**Collapse into one function. Do not key it on `masters().is_empty()`.**

Reading `overlay.rs`: `overlay_entries` (49-65) and `primary_entries` (84-197) run the *same* chain (`resolve_ebuilds`, 252-349). They differ in exactly two things:

1. **which** ebuilds enter the chain — all of them, versus only *suspects*;
2. whether the `sync_stamp`/gap-index memo is consulted at all.

Neither dimension is predicted by `masters()`. On this host: `guru` has `masters = gentoo` **and** a full shipped md5-cache (3716/3717); `pentoo` and `crossdev` have masters and **no** cache at all; a hand-made local overlay has neither. Keying on `masters().is_empty()` would route `guru` and `crossdev` identically while they need opposite treatment. `masters()` is real and structural — for eclass search (`is_fresh_cached`, `repository.rs:715`), the symlink shortcut (`master_cache_entry`, `overlay.rs:353`) and category union (`ebuilds`, `find_cpns`) — and all of those already live inside `Repository`. Using it here would just be `CacheTrust` again with a truer-looking label: still an identity tag standing in for evidence.

The evidence is coverage and mtime. So:

```rust
// portage-repo/src/entries.rs   (overlay.rs renamed — the module is no longer
// about overlays)
/// Every metadata entry of `repo`: the in-tree bulk read, plus the full chain
/// over the ebuilds that read cannot serve. One function for every repo —
/// `overlay_entries`' old behaviour is this function's degenerate case (a repo
/// with no in-tree cache: nothing is covered, so every ebuild is suspect and
/// takes the digest/source chain).
pub async fn repo_entries(repo: &Repository) -> Vec<(Cpv, CacheEntry)>;
pub async fn gap_entries(…) -> …;   // unchanged, still public
```

Two changes make that safe, and they are the actual content of this step:

**3a. Per-entry suspect rule (replaces the repo-wide `sync_time` comparison).**

```
suspect(e) =  !covered(e.cpv)                                   // no entry at all
           ||  mtime(e.path) > mtime(cache file serving e.cpv)  // ebuild newer than its entry
```

`cache_cpvs` (`cache.rs:398`) already walks every cache file and returns its path; it returns `(Cpv, PathBuf)` and can return the mtime with one `entry.metadata()` inside the existing parallel jwalk (measure it: the same walk is immediately followed by `fs::read` + parse of all 32k files, so the marginal stat should be lost in the noise — if it is not, keep `sync_time` for repos with a sync marker and use the per-entry rule only for the rest). Fall back to today's `> sync_time` when no cache-file mtime is available (in-memory secondaries, i.e. tests).

Why this and not `> sync_time`: `sync_time` (`repository.rs:467`) falls back to the repo root directory's mtime when there is no `timestamp.chk` — which is what `crossdev`/`pentoo`/any local overlay have. Root-dir mtime does not move when an ebuild three levels down is edited, and it *can* move ahead of an edited ebuild for unrelated reasons (a new category directory), which would un-suspect a genuinely stale entry. The per-entry rule compares the two files that actually matter and is repo-agnostic. It also closes the equivalent cold-path fragility for main.

**3b. Gate the stamp/gap-index memo on evidence that the stamp tracks content.**

This is the answer to the open question: **the suspect narrowing generalizes safely; the memo does not, as-is.** The memo skips the ebuild walk *entirely*, so its only protection is `sync_stamp`, and `sync_stamp` is only meaningful for a tree that is replaced wholesale by a sync that rewrites `metadata/timestamp.chk`. Without that file it degrades to the root-dir mtime, which is invariant under exactly the edits an overlay gets. Turning the memo on for `crossdev`/`pentoo`/local overlays would pin them on a stale answer forever — a regression in the workflow overlays exist for.

Add to `Repository`:

```rust
/// Whether this tree carries a sync marker that is rewritten whenever the
/// tree's content is replaced (`metadata/timestamp.chk`). Only then is
/// `sync_stamp` strong enough to memoise a derived index against.
pub fn has_sync_marker(&self) -> bool;
```

and require it (plus `sync_stamp().is_some()` and `sidecar_path().is_some()`, as today) for the warm path. On this host that enables the memo for `gentoo` **and** `guru` (both ship and rewrite `timestamp.chk`) and disables it for `crossdev`/`pentoo`. The code already agrees with that split by accident: a `crossdev` symlink ebuild resolved via `master_cache_entry` is never `put_secondary`'d (`overlay.rs:296-300`), so its gap-index CPV can never be recovered on a warm path and `recovered == cpvs.len()` (line 111) fails every time — `crossdev` would rescan on every run anyway. The gate just stops it writing a sidecar it can never use.

**Behavior change to flag loudly (this is the guru win, and it is a real trust change):** a cache-shipping overlay's entries are now accepted on coverage+mtime evidence instead of being `_md5_`- and eclass-verified on every call. On this host that takes `guru` from 3717 file reads + md5 digests + `is_fresh_cached` per resolve to **1** (the single uncached ebuild) — nothing else is suspect, because every guru ebuild's mtime equals its cache file's. This is the same evidence standard main already gets; with 3a it is strictly *stronger* than what main gets today (per-entry, not repo-wide). But it is a change, and if guru's shipped cache is ever stale relative to an ebuild whose mtime did not move, we now believe it.

Also fix while there: the doc comment on `disk_repo_with_ebuild` (`portage-resolve/src/repo.rs:2082-2087`) documents the old md5-per-ebuild overlay contract and stops being true.

### 4. Call sites — one parameter each

```rust
// portage-resolve/src/repo.rs   — RepoSource deleted entirely
pub async fn load_repos(set: &RepoSet) -> RepoData {
    for (i, repo) in set.iter().enumerate() {
        for (cpv, entry) in portage_repo::repo_entries(repo).await {
            if !seen.insert(cpv.clone()) { continue }
            cpns_set.insert(cpv.cpn);
            if i != set.main_index() { repo_of.insert(cpv.clone(), repo.name().to_string()); }
            versions.entry(cpv.cpn).or_default().push((cpv, entry));
        }
    }
    // aliases: set.aliases(), `source != set.main().name()` — unchanged
    RepoData { repo_name: set.main().name().to_string(), … }
}

// portage-cli/src/query/mod.rs
pub fn resolve_atom(set: &RepoSet, vdb: Option<&Vdb>, mode: ResolveMode, raw: &str) -> Result<Dep>
    //  … let cpns = set.find_cpns(raw);   ← replaces the repo-then-overlays loop
pub fn resolve_atoms(raw: &[String], set: &RepoSet, vdb: Option<&Vdb>, mode: ResolveMode) -> Vec<Dep>
pub fn matching_ebuilds(set: &RepoSet, vdb, mode, ebuilds: &[Ebuild], raw) -> …

// portage-cli/src/query/{depends,which}.rs
pub fn run(set: &RepoSet, vdb: Option<&Vdb>, mode: ResolveMode, atoms: &[String]) -> Result<()>
    //  … for item in set.ebuilds()? { … item.repo.cache_entry(item.ebuild.cpv()) … }
```

`3` parameters become `1` at `emerge.rs:341-348`, `dispatch.rs:362-370`, `world.rs:48-51`, `depgraph/mod.rs:265-277`. `depgraph`'s `repo_path_of` closure (`mod.rs:1490-1505`) becomes `set.by_name(name).unwrap_or(set.main()).path()` — no enum match, no `overlays` capture. `multi_repo` at its display sites (`output.rs:496`, `mod.rs:458/1472`, `TreeView.multi_repo`) becomes `set.is_multi()`.

### 5. Migration order

| step | scope | lands green? | notes |
|---|---|---|---|
| **1** | `portage-repo`: add `RepoSet` + `EbuildsAcross` + unit tests. No caller changes. | yes, independently | pure addition |
| **2a** | `portage-repo`: `cache_cpvs`/`cache_entries_parallel` carry the cache file mtime; `primary_entries` switches to the per-entry suspect rule | yes, independently | **behavior**: suspect set becomes per-entry. Bench `em -p @world` before/after |
| **2b** | `portage-repo`: `Repository::has_sync_marker`; gate the memo on it | yes, independently | no-op for gentoo today |
| **2c** | `portage-repo` + the two lines in `load_repos`: fold `overlay_entries` into `repo_entries`, rename `overlay.rs`→`entries.rs` | one commit, two crates | **behavior**: the guru trust/perf change (§3). Must come *after* 2a/2b, which are what make it safe |
| **3** | **atomic**: `load_repos(&RepoSet)`, delete `RepoSource`, `repo_open::repo_set_from_conf` replaces `merge_sources_from_conf`, `resolve_atom`/`resolve_atoms`/`matching_ebuilds` take `&RepoSet`; update `emerge.rs`, `dispatch.rs`, `world.rs`, `depgraph/mod.rs` | one commit | mechanical, ~7 files. Also the "main is always in the set" fix |
| **4** | `depends::run`/`which::run` take `&RepoSet` and enumerate `set.ebuilds()` | separate commit | **behavior**: both become overlay-aware; deletes the two "No overlay search here" comments. Needs its own tests |
| **5** (follow-up) | `DepgraphOpts { repos: &RepoSet }` replacing `repo_path` + `multi_repo`; emerge passes its set down | separate | removes a full duplicate build of the set per merge (see §6) |
| **6** (follow-up) | dedupe masters through the set; arena; `search.rs` | separate | out of scope here |

Why 3 must be atomic: `RepoSource` lives in `portage-resolve` and is consumed by `portage-cli`; a half-migration would have `emerge.rs`/`dispatch.rs` building *both* a `RepoSet` and a `Vec<RepoSource>`, i.e. walking repos.conf and opening every overlay twice more per invocation. Not worth a shim.

**Behavior changes to call out in commit messages, not ship silently:**
1. (2a) suspect selection is per-entry, not repo-wide — catches an in-place edit that `> sync_time` missed, and stops treating a whole tree as suspect after an unrelated top-level mtime bump.
2. (2c) cache-shipping overlays are no longer `_md5_`-verified per ebuild per call (the guru win, and the trust delta).
3. (2b) the memo is now permitted for any repo with a `timestamp.chk` — on this host that newly includes `guru`.
4. (3) the main repo is unconditionally in the merge set even if no repos.conf entry's `location` string-matches its path (today it is silently dropped).
5. (3) `resolve_atom`'s ambiguity candidate list is sorted across repos instead of main-first-then-overlay-order — changes the printed order and `--ask` numbering when a bare name is ambiguous across repos. Existing tests assert content, not order.
6. (4) `em depends` / `em which` now see overlay packages; `which` prints the highest-priority repo's ebuild for a shadowed cpv (i.e. the file the merge would actually build).

### 6. What does not fit cleanly — stated plainly

1. **`Location::Alias` repos have no `Repository`.** They travel in the set but cannot appear in `iter()`, `find_cpns()` or `ebuilds()`. So `em -p cross-riscv64-unknown-linux-gnu/gcc` works (full atom, materialised by `load_repos`) while the bare name still does not — unchanged behavior, but now visibly asymmetric *inside one type* instead of hidden behind two parameters. `find_cpns` could synthesise them from `aliases()` (the entry lists the exact dest categories and cpns); small follow-up, worth doing, not required here.

2. **The single-repo query applets.** `keywords`/`meta`/`uses`/`list`/`hasuse` open one `Repository` from a path and never see repos.conf; `matching_ebuilds` resolves names against a repo it then can't fully enumerate (its own comment at `query/mod.rs:276-281` says so). `RepoSet::single(repo)` adapts them with zero behavior change and is honest about it — better than today's `&[]` sentinel, which reads as "there are no overlays". Making them genuinely multi-repo needs per-command ebuild enumeration; explicitly out of scope, but `set.ebuilds()` is exactly the missing piece when someone does it.

3. **`RepoData` keeps its own weaker copy of "which repo".** `repo_of: HashMap<Cpv, String>` with "absent means main" (`repo.rs:735`, `repo_name_of`) is a second encoding of what `RepoSet` now knows exactly, keyed by string. It should eventually become an index into the set (`HashMap<Cpv, u16>`), which would also give `PlannedMerge`'s ebuild-path construction the right repo without a name lookup. Too many consumers to change in this pass; leave it, note it.

4. **`Repository::masters: Vec<Repository>` is still deep-owned, and the set makes the duplication visible.** Each of `crossdev`/`guru`/`pentoo` opens its own full copy of `gentoo` as a master, and `merge_sources_from_conf` runs **twice** per `em` invocation (`emerge.rs:341` then again inside `depgraph/mod.rs:266`, which also re-opens main at `:262` after `emerge.rs:271`) — that is 8 `Repository::open`s of `gentoo` per merge, each re-reading `layout.conf`, `profiles/repo_name`, `profiles/arch.list`. `RepoSet::from_ordered` taking pre-opened `Arc<Repository>`s is designed so the fix (open each conf entry once; resolve masters *from the set*, sharing the `Arc`) needs no API change — but it needs `masters: Vec<Arc<Repository>>` on `Repository`, which is the arena work. Step 5 above removes the ×2 for free.

5. **`load_repos` is sequential over repos** (`for source in sources { … .await }`). With `Arc<Repository>` the per-repo loads become independently spawnable and the priority merge just consumes the results in order — a real win once `guru` no longer dominates. Enabled by this design; not part of it.

6. **`ebuilds()`' error policy is a judgement call.** Main's enumeration failure propagates, everything else warns and is skipped. That preserves `depends`/`which`'s current "a broken main repo is a real error" behavior while gaining overlay coverage, and matches the leniency already applied to an unopenable repos.conf entry and to a master's unreadable categories file. It does mean a systematically broken overlay degrades quietly to a `tracing::warn!` — deliberate, and the alternative (fail the whole command because one overlay is mid-`git checkout`) is worse.

#### Critical files for implementation
- `/home/lu_zero/Sources/portage-cli/portage-repo/src/repo/repository.rs` (new `has_sync_marker`; the `Repository` API `RepoSet` composes; future `masters: Vec<Arc<…>>`)
- `/home/lu_zero/Sources/portage-cli/portage-repo/src/overlay.rs` (→ `entries.rs`: `repo_entries`, per-entry suspect rule, memo gate)
- `/home/lu_zero/Sources/portage-cli/portage-repo/src/cache.rs` (`cache_cpvs`/`cache_entries_parallel` must carry cache-file mtime)
- `/home/lu_zero/Sources/portage-cli/portage-resolve/src/repo.rs` (`load_repos(&RepoSet)`; delete `RepoSource`; lines 1202-1333 + tests at 2054-2270)
- `/home/lu_zero/Sources/portage-cli/portage-cli/src/repo_open.rs` (`repo_set_from_conf` replaces `merge_sources_from_conf`)
- `/home/lu_zero/Sources/portage-cli/portage-cli/src/query/mod.rs` (`resolve_atom`/`resolve_atoms`/`matching_ebuilds` on `&RepoSet`)
