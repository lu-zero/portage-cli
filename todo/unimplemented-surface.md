# What is still unimplemented — survey 2026-09-02

Status: 🟡 survey; `em clean dist`/`pkg` implemented 2026-09-02, the rest open. Every `bail!("not implemented")` in
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

## 2. `eclean` — IMPLEMENTED 2026-09-02

`em clean dist` and `em clean pkg` now exist (`portage-cli/src/clean.rs`),
with `--deep` (keep only what installed packages reference), `--size-limit`
and `--time-limit`, honouring the global `-p`.

The DISTDIR walk delegates to `mirrordist::scan_distdir` rather than
repeating it — that function was already parameterised on the reference set,
so the two commands differ only in *policy* (which files count as
referenced) and cannot drift on how a DISTDIR is read. `clean` adds its own
whitelist for portage bookkeeping a mirror dir never contains: `<file>.lock`
(a live fetch lock) and `.layout.conf.<mirror>`.

Still not covered, if anyone wants closer `eclean` parity: the interactive
mode, and `--destructive`'s "keep only the newest version" flavour.

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
2. ~~`eclean pkg`/`dist`~~ — done, see above.
3. `emaint merges`, then `all`.
4. `portageq`/`grep`: **measured, not worth it.** A scan of
   `/var/db/repos/{gentoo,guru,crossdev,pentoo}` found **zero** `portageq`
   calls in any inheritable `*.eclass` and zero in any live ebuild — the only
   hits are `eclass/tests/` (a hand-run harness that is never `inherit`ed),
   OpenRC init scripts that run post-install, and maintainer scripts. Nothing
   `em` executes as a phase calls it, so the wrong-root risk is theoretical.
   Revisit only if a real overlay ebuild turns up; `envvar`, `has_version`,
   `match` and `best_version` would cover the shapes that exist in the wild.
