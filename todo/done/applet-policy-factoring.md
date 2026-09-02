# Factor out repeated `UseEnv` → `ResolvePolicy` construction

Status: ✅ landed 2026-08-21 (`5951b21`) as
`ResolvedPolicy::from_use_env`/`as_policy`, confirmed flat against
firefox/qtbase/texlive. The proposal below is kept as the design record.

Opened 2026-08-21, end of the package.mask `::repo` / repo-interning /
duplicate-cpv-collapse session (commits `b3a8380`, `697de0f`, `c3a4964`).

## The duplication

Three applets each independently build the four `Accept*` structs
(`AcceptKeywords`/`AcceptLicenses`/`AcceptProperties`/`AcceptRestrict`)
from a `portage_resolve::use_env::UseEnv`, then wire them plus the raw
`UseEnv` fields into a `portage_resolve::repo::ResolvePolicy`:

- `portage-cli/src/pkg.rs`'s `resolve_active_use` (pre-existing)
- `portage-cli/src/query/depgraph/mod.rs`'s `depgraph()` (this session —
  now also sequenced right before `repo::collapse_duplicates`, see
  `c3a4964`)
- `portage-cli/src/maint/world.rs`'s `TreeView::load` (this session,
  same `collapse_duplicates` sequencing; `TreeView` already hand-rolls
  the "own the `Accept*` structs + a `.policy()` accessor" shape as
  struct fields — the natural template for the shared type below)

Same ~15-line block, three times, drifting slightly each time (`pkg.rs`
clones `env.*` fields since it needs `env` for something else after;
`depgraph()`/`world.rs` move them). Any future fifth `Accept*`
field, or a fix to how one of the four is constructed, needs finding
and fixing in three places.

## Proposed shape

Lives in `portage-resolve` (where `UseEnv`/`ResolvePolicy`/`Accept*`
already live — no new cross-crate coupling):

```rust
/// Owns the resolved Accept* decisions plus the UseEnv fields
/// ResolvePolicy borrows from — build once, then .as_policy() wherever
/// a ResolvePolicy<'_> is needed.
pub struct ResolvedPolicy {
    accept_keywords: AcceptKeywords,
    accept_licenses: AcceptLicenses,
    accept_properties: AcceptProperties,
    accept_restrict: AcceptRestrict,
    package_mask: Vec<Dep>,
    package_unmask: Vec<Dep>,
    defaults: UseLayer,
    conf: UseLayer,
    env_use: UseLayer,
    package_use: Vec<(Dep, Vec<UseOverride>)>,
    profile_package_use: Vec<ProfileUseNode>,
    force_mask: ForceMask,
}

impl ResolvedPolicy {
    /// `arch` is the caller's already-resolved accept-arch (depgraph()'s
    /// cross-target logic stays at the call site, not baked in here).
    pub fn from_use_env(env: UseEnv, arch: Interned<DefaultInterner>) -> Self { ... }
    pub fn as_policy(&self) -> ResolvePolicy<'_> { ... }
}
```

Optionally, a second convenience for the two call sites that also load
a `RepoSet` (`depgraph()`, `TreeView::load` — `pkg.rs` targets one
already-open `Repository`, doesn't need this half):

```rust
/// tokio::join!(load_repos, build_use_env) + collapse_duplicates, in one call.
pub async fn load_and_resolve(
    set: &RepoSet,
    config_root: &Utf8Path,
    config_overlay: Option<&Utf8Path>,
    extra_use_override: Option<...>,
    arch: Interned<DefaultInterner>,
) -> Result<(RepoData, ResolvedPolicy)>
```

## Scope check before starting

- Confirm no fourth/fifth call site exists by the time this is picked
  up (`grep -rn "AcceptKeywords::new(\|repo::ResolvePolicy {" portage-cli/src`).
- `pkg.rs` clones several `env.*` fields into the `Accept*`
  constructors because it still needs `env` afterward (for `defaults`/
  `conf` etc., which `ResolvePolicy` also borrows) — check whether
  `ResolvedPolicy::from_use_env` taking `env` by value still lets
  `pkg.rs`'s post-policy code reach what it needs via the new struct's
  fields, or if a `&UseEnv`-taking variant is needed there instead.
- `TreeView` should probably become a thin wrapper around
  `ResolvedPolicy` (plus `data`/`multi_repo`) rather than keeping its
  own duplicate fields, once this lands.
