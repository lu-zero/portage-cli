# What is still unimplemented — survey 2026-09-02

Status: 🟡 `em clean` and `em etc` implemented 2026-09-02; `portageq`/`grep`
and three `emaint` subcommands remain.
Every `bail!("not implemented")` in the tree, grouped by whether it actually
costs a user anything.

## 1. Config-file reconciliation — IMPLEMENTED 2026-09-02

`em etc` (aliases `config`, `dispatch`) closes what was the sharpest gap:
`em` wrote `._cfgNNNN_` sidecars on every protected merge and nothing
consumed them, which a host's own `etc-update` could not do under an offset
root.

One command rather than two front-ends, since `etc-update` and
`dispatch-conf` differ in UX and not in the job. Listing, `diff`,
interactive `merge`, and `--auto`/`--use-new`/`--use-old` batch modes; each
pending file classified identical / comments-only / modified / new so
`--auto` can clear the ones needing no decision. Full writeup in
[`docs/user/applets.md`](../docs/user/applets.md#em-etc-etc-update-dispatch-conf).

Only `dispatch-conf`'s RCS archival of superseded versions is not carried
over.

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

`em clean all` runs all three sweeps — distfiles, binary packages, and the
`build.log` files `em maint logs` reports on — announcing each step. It keeps
going when a step fails (an unreadable `PKGDIR` should not cost you the
distfile sweep) and returns the first error at the end, so a script still
sees a non-zero exit.

`em clean pkg` is distinct from the pre-existing `em maint binpkg prune`,
which prunes by *build identity* (the multi-instance work in
[[binpkg-subtargets]]) rather than by "no cpv in the tree / not installed".
Both are useful; they answer different questions.

Still not covered, if anyone wants closer `eclean` parity: the interactive
mode, and `--destructive`'s "keep only the newest version" flavour.

## 3. `emaint` — 10 of 13, and the help no longer lies

`em maint --help` used to list all 13 subcommands with confident
descriptions while 5 were `bail!` stubs, so picking one from the help
produced "not implemented". Fixed 2026-09-02 by implementing what was
tractable and making the rest say what they actually are:

- **`logs`** — implemented. Deliberately *not* portage's target: real
  `emaint logs` cleans `PORTAGE_LOGDIR`, which for `em` holds only elog
  output (`em read` owns that, with its own `--delete`). `em` keeps each
  package's `build.log` under
  `<work_base>/<root-key>/<category>/<PF>/` after a successful merge on
  purpose, and that is what accumulates — 47 logs / 85 MiB on this host at
  the time of writing. `--fix` to remove, `-t 30d` to bound by age.
- **`movebin`** — implemented, sharing `moveinst`'s `profiles/updates/`
  reader and report-only for the same reason it is: renaming a GPKG
  container is not enough, since the archive metadata and the `Packages`
  index both name the old cpv.
- **`cleanconfmem`** — cannot be implemented and no longer pretends to be
  pending: it discards stale entries from portage's config tracker
  (`/var/lib/portage/config`), which `em` never writes. Now says so.
- **`merges`** — still unavailable, but the error says why: `em` keeps no
  failed-merge registry; a failure is reported at the end of the run and in
  its build log.
- **`all`** — **removed** 2026-09-02 (Luca). An aggregate over `maint` was
  the wrong shape: `em maint`'s subcommands are overwhelmingly checks and
  reports, so an "all" that ran them would mostly be a no-op wrapper, while
  the thing actually worth batching is the *cleaning*. That is
  `em clean all`, which sweeps distfiles, binary packages and the retained
  build logs in one pass — see §2.

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
