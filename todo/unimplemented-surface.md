# What is still unimplemented — survey 2026-09-02

Status: 🔵 survey only, nothing started. Every `bail!("not implemented")` in
the tree, grouped by whether it actually costs a user anything.

## 1. Config-file reconciliation — the sharpest gap

```
dispatch.rs:275  Applet::Dispatch => bail!("not implemented: dispatch-conf")
dispatch.rs:276  Applet::Etc      => bail!("not implemented: etc-update")
```

`em` implements the whole *producing* half of `CONFIG_PROTECT` and then has
nothing to consume it. `ebuild.rs` diverts a protected file whose content
changed to `._cfgNNNN_<name>` using portage's own `new_protect_filename`
numbering (`:2727`, `:2789`), records the *real* path in `CONTENTS` rather
than the sidecar, and tells the user about it:

```
{} protected config file(s) were installed with a ._cfg name.        (ebuild.rs:2297)
```

So a real `em` user accumulates `._cfg0000_*` files across every merge with
no supported way to review or merge them — they have to reach for the
system's own `etc-update`/`dispatch-conf`, which is fine on a host but not
under `--root`/`--prefix`/`--local`, where those tools do not know about the
offset. This is the one unimplemented item that breaks an ordinary workflow
rather than a convenience.

`ConfigProtect::longest_match` already answers "is this path protected", so
the matching half exists; what is missing is the interactive/`-a` merge tool
over the sidecars. See [[pms-config-protect]] for the settled semantics.

## 2. `eclean` — nothing at all (2 of 2)

```
dispatch.rs:526  CleanTarget::Dist => bail!("not implemented: eclean dist")
dispatch.rs:527  CleanTarget::Pkg  => bail!("not implemented: eclean pkg")
```

`em maint binpkg prune` exists but prunes by *build identity* (the
multi-instance work in [[binpkg-subtargets]]), not by "no longer in any
installed set / older than N / over N bytes", which is what `eclean-pkg`
does. PENDING already notes the port gap (gentoolkit is not a dependency).
`eclean dist` has no counterpart at all, though `portage-distfiles` owns the
DISTDIR layout it would need.

Disk-space housekeeping, so it degrades gracefully — a user can run
gentoolkit's on a host — but the same `--root`/`--prefix` caveat as above
applies.

## 3. `emaint` — 8 of 12 implemented

Implemented: `binhost`, `binpkg`, `cleanresume`, `moveinst`, `regen-use`,
`revisions`, `sync`, `world`.

```
dispatch.rs:285  all           dispatch.rs:306  logs
dispatch.rs:288  cleanconfmem  dispatch.rs:307  merges
dispatch.rs:284  (no subcmd)   dispatch.rs:308  movebin
```

`all` is just a fan-out over the others and is cheap once the rest exist.
`merges` (resume a partially-completed merge list) overlaps
`cleanresume`/[[activity-status]]'s resume state and is the most useful of
the remainder. `logs`, `cleanconfmem`, `movebin` are marginal.

## 4. Standalone applets

```
dispatch.rs:124  portageq   dispatch.rs:246  grep
```

`portageq` is a scripting/eclass query surface that `em query` already
covers most of in its own idiom; the gap is CLI-shape compatibility, which
matters only if something external shells out to `em portageq`. Real
eclasses call the *installed* portage's `portageq`, not ours. `grep` is a
`pquery`-shaped tree search — no known consumer.

Both currently bail cleanly (the debug lines they used to print before
bailing were dropped in `2b753fc`).

## Not a gap

`info.rs:540`'s `return "not implemented".to_string()` is a *label*, not a
stub: it is what `em --info` prints for a package set that is declared but
that `portage_repo` cannot resolve, distinguishing it from "not resolvable:
{err}". Correct as written.

## Suggested order, if this is ever picked up

1. `etc-update`/`dispatch-conf` — the only one that breaks a normal workflow,
   and the only one whose absence is worse under `em`'s own offset roots than
   on a host.
2. `eclean pkg`/`dist` — real housekeeping, and most of the underlying
   machinery (PKGDIR scan, DISTDIR layout, installed sets) already exists.
3. `emaint merges`, then `all`.
4. `portageq`/`grep` only if an external consumer turns up.
