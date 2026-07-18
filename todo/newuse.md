# `--newuse` / `-N` — reinstall when USE (or IUSE) changed

STATUS: **not started.** The `-N`/`--newuse` **flag is parsed** (`DepgraphFlags`)
but **not consumed** by the resolver or merge skip logic. emerge's `-N/--newuse`
reinstalls an installed package when its effective USE differs from what it was
built with (also when IUSE gained/lost a flag that changes the enabled set).

**Not the same as `-uD`:** [[deep-in-slot-upgrades]] (done 2026-07-18) upgrades
*versions* in the deep graph. `--newuse` forces rebuilds of *same CPV* when USE
drifted — orthogonal, still open.

## The gap

`em` decides "already satisfied / skip" purely on **installed CPV vs planned
CPV** — it does not compare the *installed USE* (recorded in the VDB `USE` /
`IUSE` files) against the *planned USE*. So a pure USE change (no version bump)
is invisible: `em foo` after editing `package.use` is a no-op where
`emerge -N foo` rebuilds.

This is the general form of the cross-toolchain two-stage skip
([[crossdev-target]]): `glibc[headers-only]` → `glibc[]` and `gcc[stage1 USE]` →
`gcc[stage2 USE]` are the same CPV with different USE; `--newuse` semantics would
naturally rebuild them. (The explicit-target *replace* default — see below — is
a separate, blunter mechanism that also fixes the toolchain case; `--newuse` is
the precise, deps-included version emerge users expect.)

## Mechanism (emerge parity)

- Compare planned effective USE (from `effective_use`) against the VDB-recorded
  USE for the installed CPV. Reinstall (`[ebuild  rR ]` / `N` reason) when they
  differ, restricted to flags in the package's current IUSE.
- `--newuse` applies to the whole graph (deps too); `--changed-use`/`-U` is the
  variant that ignores flags added/removed from IUSE. Implement `-N` first.
- Read installed USE from `var/db/pkg/<cat>/<pf>/USE` + `IUSE` (already parsed by
  `portage-vdb`? verify) and intersect with current IUSE.

## Coordination

- Resolver display + the merge-skip decision (`run_merge_plan`, main.rs) must
  agree: a USE-changed package is in the plan AND not skipped at merge time.
- Pairs with the **explicit-target replace default** ([[reinstall-default]]):
  that one forces a rebuild of *named* atoms regardless of USE; `--newuse` forces
  a rebuild of *any* graph node whose USE drifted.
