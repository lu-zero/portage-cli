# What is still unimplemented — survey 2026-09-02

Status: 🟡 `em clean dist`/`pkg` implemented 2026-09-02; the rest open.
Every `bail!("not implemented")` in the tree, grouped by whether it actually
costs a user anything.

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
- **`all`** — designed, not yet written. The earlier "defer until the
  subcommand set settles" framing was wrong: `all` never has to mean
  *everything*, and real `emaint` does not either. It runs a **subset**, and
  the fix for the ambiguity is simply to **print the subset it covers** so
  the user is never guessing.

  The dividing line is check-vs-mutate, which `em`'s subcommands already
  fall on cleanly:

  | in `all` (reports only) | excluded (mutates, or needs an argument) |
  |---|---|
  | `world` (no `--fix`) | `binhost` — writes an index |
  | `cleanresume` (no `--fix`) | `binpkg` — requires an action |
  | `moveinst` — report-only | `regen-use` — writes `use.local.desc` |
  | `movebin` — report-only | `revisions` — purges history |
  | `logs` (no `--fix`) | `sync` — network, mutates the tree |
  | `cleanconfmem` — no-op | `merges` — unavailable |

  So `em maint all` is "every check `em` can make without changing
  anything", which is also what makes it safe to suggest running. It should
  open by naming the tasks it is about to run, and keep going past a task
  that fails rather than aborting the sweep — one unreadable PKGDIR should
  not hide the world-file result. A non-zero exit if any task reported a
  problem, so it is usable from a cron job.

  Everything it needs already exists; this is a fan-out plus a header, not
  new machinery.

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
