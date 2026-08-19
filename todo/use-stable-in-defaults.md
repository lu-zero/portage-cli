# `use.stable` / `package.use.stable` inside the defaults layer

Status: 🔴 not started. Split out of the 2026-08-19 defaults/conf USE-fold
work (`use-defaults-package-use`). Related: [[use-order-repo-layer]]

## What Portage does

`setcpv` rebuilds `configdict["defaults"]["USE"]` per CPV by walking the
profile chain. For each node, in order (config.py ~1956):

```
make.defaults[i]
if stable: use.stable[i]
matching package.use[i]
if stable: package.use.stable[i]
```

Comment in that loop: `package.use.stable > package.use > use.stable`.
`stable` is `_isStable(cpv)` — the same gate `em` already uses for
`use.stable.force`/`mask` (`AcceptKeywords::is_stable`).

These are ordinary USE tokens, not force/mask. A later `make.conf` `USE=`
still wins (they live in `defaults`, below `conf`). EAPI-gated
(`eapi_supports_use_stable`; Portage 3.0.x: EAPI 9).

## What `em` does

The 2026-08-19 fold matches the `make.defaults` / `package.use` half of that
loop (`ProfileUseNode`). It does **not** splice `use.stable` or
`package.use.stable`.

`em` already applies `use.stable.force`/`mask` and
`package.use.stable.force`/`mask` as post-fold force/mask
(`ForceMask`, `force_mask.rs`). Those are a different file type.

`portage-repo` has no `use.stable` / `package.use.stable` readers
(`STATUS.md` lists only the `*.force`/`*.mask` variants).

## Why it waited

Stock gentoo/guru/pentoo have **zero** `use.stable` / `package.use.stable`
files today. No live `em` vs emerge canary on this host. The insertion point
is now obvious (`ProfileUseNode` walk in `resolve_effective_use`); adding
the files without a consumer would be speculative.

## How to attack

1. `Profile::use_stable()` / `package_use_stable()` (dir-form via
   `read_lines` / `parse_atom_flags_list`, same as `package.use`).
2. Carry them on `ProfileUseNode`. Apply only when `resolve_effective_use`
   is told the CPV is stable — the fold currently has no `stable: bool`;
   force/mask gets it after the fold. Either pass it in, or apply these
   tokens in `effective_use` / `desired_use` next to `ForceMask::apply`.
3. Same later-node skip as `package.use` vs a child `make.defaults`.
4. Synthetic unit tests first; live canary needs a profile that actually
   ships the files (or a throwaway `/etc/portage/profile` node).
