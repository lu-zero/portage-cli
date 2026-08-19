# `repo` USE_ORDER layer

Status: 🔴 not started. Split out of the 2026-08-19 defaults/conf USE-fold
work (`use-defaults-package-use`). Related: [[use-stable-in-defaults]]

## What Portage does

Default `USE_ORDER` is
`env:pkg:conf:defaults:pkginternal:features:repo:env.d` (high → low).
`regenerate()` walks the reverse, so `repo` sits **below** IUSE defaults
and **above** `env.d`.

`configdict["repo"]` is rebuilt in `setcpv` per CPV (config.py ~1904)
from each repository that owns the package, masters first then the repo
itself:

- `profiles/make.defaults` at the **repository root** (not a profile
  node) — `_repo_make_defaults[repo.name]`, loaded once at config init
- if stable: `profiles/use.stable`
- matching `profiles/package.use` (`_repo_puse_dict`)
- if stable: `profiles/package.use.stable`

Same files as the profile stack, but looked up under `$REPO/profiles/`
and keyed by repo name. A package from `guru` does not inherit gentoo's
repo-level `package.use`.

## What `em` does

The 2026-08-19 fold models `pkginternal < defaults < conf < pkg < env`.
Nothing occupies the `repo` slot. `portage-repo` reads
`profiles/package.mask` at the repo root (`Repository::repo_package_mask`)
but not `profiles/make.defaults` / `profiles/package.use` there.

Profile-stack `make.defaults` / `package.use` are a different path
(`ProfileStack`) and already fold correctly.

## Why it waited

gentoo, guru, pentoo, and the crossdev overlay on this host have no
repo-root `profiles/make.defaults` or `profiles/package.use`. The layer
is for an overlay that wants repo-wide USE without putting it in every
profile. No live canary until such an overlay shows up (or we plant one).

## How to attack

1. Read `$repo/profiles/make.defaults` through the same incremental
   source as a profile node (`source_make_defaults` / `source_incremental`).
2. Read `$repo/profiles/package.use` with the existing atom-flags parser.
3. New `UseLayer` below IUSE in `resolve_effective_use`, keyed by the
   candidate's repo, not the profile stack. Masters first, then the repo
   (Portage's `repos_with_profiles` order).
4. `-*` here wipes `env.d` only; IUSE / defaults / conf still apply on
   top. Unit-test that, plus "guru package.use does not leak onto a
   gentoo CPV".
5. `use.stable` / `package.use.stable` at repo root can wait on
   [[use-stable-in-defaults]] — same EAPI gate, same empty-tree reality.
