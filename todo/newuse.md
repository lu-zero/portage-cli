# `--newuse` / `-N` and `--changed-use` / `-U`

STATUS: **implemented** (2026-07-18), Portage-aligned (no eager BDEPEND mode).

## What it does

| Flag | Effect |
|------|--------|
| `-N` / `--newuse` | Rebuild packages **already in the depgraph** when planned USE/IUSE differs from the VDB |
| `-U` / `--changed-use` | Same, but only for **enabled** flag flips (ignore pure IUSE add/drop) |

Detection: `portage_resolve::use_reinstall` (Portage `_reinstall_for_flags` + filter for stale never-enabled IUSE tokens).

Wiring:

1. `add_installed` → `InstalledPolicy::Rebuild` when drift is detected  
2. `-N` alone → same-CPV `[R]` if that version is still available  
3. `-N` + `-uD` → prefer newest (upgrade) when drift forces a rebuild  
4. Plan filter keeps Rebuild same-version rows as `[R]`

## What it deliberately does *not* do

**No eager re-injection of host-satisfied BDEPEND under `-N` alone.**  
That mode listed dozens of tool `[R]`s (`setuptools`, …) emerge never shows on shallow `-uNp`. Dropped for Portage parity.

Missing python impls are still forced when atoms carry USE-deps, e.g.
`mako[python_targets_python3_14(-)]` (host satisfaction fails → package enters the plan).

## Cleanup after `PYTHON_TARGETS` changes

Use the same path as emerge:

```bash
em -uNDp @world    # pretend
em -uND @world     # real
```

Not shallow `-uNp`. Deep update brings the tree into the graph; `-N` rebuilds USE drift; atom USE-deps catch tools that only provide a dropped impl.

## Related

- [[deep-in-slot-upgrades]] — `-uD` version upgrades  
- [[deep-slot-bump]] — `:*` slot bumps under `-D` / emptytree  
