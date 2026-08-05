# Activity, progress, and ETA

`em` emits a structured stream of **activity events** (session / package /
phase transitions) that front-ends, dashboards, and ETA consume. The flat
`emerge.log` transcript is an **optional compatibility sink**, not the control
plane — live status and history live in typed on-disk state instead.

Design rationale and the full event schema live in
[`todo/activity-status.md`](../todo/activity-status.md); this page is the
user-facing how-to.

## What is recorded, and where

Everything is rooted under each merge root's cache tree, so `--root`/`--prefix`
offsets are self-contained:

```
<merge_root>/var/cache/edb/em-activity/
  live/<job_id>/
    session.json          # one job: argv, pid, plan, flags, heartbeat
    inflight/{host,target}/<cat>/<pf>.json   # packages in flight + current phase
  history/merges.jsonl    # one line per finished package action (ETA substrate)
```

Optional dual-write (off by default):

```
<merge_root>/var/log/emerge.log   # Portage line format; enable with --emergelog
```

Durable sinks (live FS, history, emerge.log) run on background threads, so
emitting phase events never blocks the async merge scheduler or the ebuild
phase loop. At session end the worker threads are joined, so the final
`SessionEnd`/history record reaches disk before `em` returns.

## Console output (verbosity)

The terminal is itself an activity sink (`HumanStdoutSink`) — the `>>> Emerging
(N of M) cpv` and `=== (N of M) <Phase> cpv` banners are *projections* of the
same `PkgStart` / `PhaseEnter` events that `--activity-fd` and `em log current`
consume, not parallel ad-hoc prints. Verbosity is decided in one place:

| Flags | Console | Build/phase output |
|-------|---------|--------------------|
| default | `>>> Emerging (N of M)` + per-phase `===` banners | copied to terminal **and** `build.log` |
| `-j>1` | banners (one line/pkg; readable) | `build.log` only (no interleaving) |
| `-q` | nothing (failures still print) | `build.log` only |
| `-v` | banners + per-phase elapsed time | copied to both |

### The phase's terminal

In the two rows that reach the terminal, the phase's stdout/stderr is a **pty**
(`portage-repo/src/build/pty.rs`, real portage's `portage/util/_pty.py`), read
by `em` and copied to the console and the log. It is not a nicety: the `e*`
implementation every *external* Gentoo tool uses (`gentoo-functions`, hence
`binutils-config`, `gcc-config`, and the `eltpatch` behind `elibtoolize`)
re-checks `[ -t 1 ]` on **every** call before it will colour anything or place
`eend`'s indicator with cursor motion. Through a pipe both fail, so those tools
rendered flat and line-wrapped no matter what `TERM`/`COLUMNS` said.

Consequences worth knowing:

- **`build.log` contains ANSI escapes** for anything that colours itself when
  attached to a terminal — the phase's own `e*` output, and also `gcc`,
  `cmake`, `ninja` and friends. Real portage's logs have the same property for
  the same reason. `OPOST` is turned off on the slave (as `_pty.py` does), so
  what does *not* get in is `\r\n`: line endings stay one byte.
- **No pty when there is no terminal** — piped output, `-q`, and `-j>1` all
  keep the previous `tee`, so CI logs and captured output are unchanged.
- The reader is a plain OS thread, not a `tokio` task, because the phase
  blocks on writing once the pty buffer fills and a starved reader would
  deadlock the merge. It stops when the last slave closes, with a bounded
  linger so a background process that inherited fd 1 cannot hold a merge open.

`-v` stops there: it is a display flag, so it does not raise the tracing floor.
`em`'s own debug/trace logs need `-vv`/`-vvv`, and those still leave dependency
targets at `WARN` — brush-core traces every word it expands (`expansion`,
`commands`, `parse`, … targets), which buries a build log. `RUST_LOG` replaces
the filter outright when that detail is what you want
(`RUST_LOG=expansion=debug em …`); see `portage-cli/src/diag.rs`.

Build-helper status (`>>> Unpacking …`) and ebuild-layer lines (`fetch:` …,
`Created binary package`) honour the same quiet state via the shared
`QuietFlag` / `EbuildShell::quiet()`, so nothing escapes verbosity. Library
callers that don't attach the sink get the events on the bus to render their own
UI (see `attach_human_stdout`).

## `em log` — history and live status

| Command | Reads | Output |
|---------|-------|--------|
| `em log current` | `live/` | Ongoing sessions: per-job plan progress + every in-flight package's phase and elapsed time |
| `em log list [-n N]` | tail of `history/merges.jsonl` | Recent finished actions (default 20) |
| `em log time [atom]` | `history/merges.jsonl` | Last/median/mean durations for a package (or global median with no atom) |
| `em log predict` | `live/` + `history/` | ETA for a **running** session's remainder (errors if nothing is active) |

`em log current` drops sessions whose pid is dead and heartbeat is stale, so a
killed `-9` run does not linger forever.

## ETA

- `em -p ... --eta` — after building the plan (pretend path), prints a
  wall-time estimate from history. When the depgraph supplies build-order
  blockers it uses a **critical-path list-schedule** at `--jobs N`; otherwise a
  naive `sum / jobs`. Unknown packages (no history yet) are reported as
  `unknown` rather than fabricated.
- `em log predict` — same math, applied to whatever a live session has left.

Both share one pure helper, so library callers (e.g. `crossdev-stages`) can
estimate a plan slice the same way.

## Driving `em` as a subprocess: `--activity-fd` / `--activity-jsonl`

Front-ends that shell out to `em` (instead of embedding `portage_cli`) read the
same typed events over a side channel:

```bash
# Stream events on file descriptor 3 (preferred — stdout carries human output)
em -uDN @world --activity-fd=3  3> /tmp/em-events.jsonl

# …or append to a path
em -uDN @world --activity-jsonl=/tmp/em-events.jsonl
```

One JSON object per line, versioned for forward compatibility:

```json
{"v":1,"event":"session_start","job_id":"19b3…-12345","pid":12345,"plan_total":40,…}
{"v":1,"event":"pkg_start","job_id":"…","cpv":"sys-devel/gcc-14.2.1","index":13,"of":40,…}
{"v":1,"event":"phase_enter","job_id":"…","cpv":"sys-devel/gcc-14.2.1","phase":"compile",…}
{"v":1,"event":"phase_leave","job_id":"…","phase":"compile","seconds":812.4,…}
{"v":1,"event":"pkg_end","job_id":"…","ok":true,"seconds":901.2,…}
{"v":1,"event":"session_end","job_id":"…","ok":true,…}
```

`--activity-fd=-` (stdout) is rejected — it collides with human output; use an
FD. Unknown event types must be skipped by readers (forward compat). The
`job_id` is the same key resume markers use, so completion, live status,
history, and the bus all correlate.

## emerge.log compatibility

```bash
em -uDN @world --emergelog       # or: EM_EMERGELOG=1 em -uDN @world
```

Writes Portage's classic line format to `<merge_root>/var/log/emerge.log`, so
`qlop`/`genlop`/`emlop` keep working. Default is off; structured bus + live FS
+ history remain the real API.

## Manual test recipes

These assume an offset root so you do not touch the live system; drop
`--root` to run against `/`.

```bash
ROOT=/tmp/em-act

# 1. History + ETA need at least one prior build. Build something tiny:
em --root $ROOT -1 app-misc/foo 2>/dev/null || true

# 2. Pretend with an ETA (reads history written by step 1 / prior runs):
em --root $ROOT -p --eta app-misc/foo

# 3. While a build runs in one terminal, watch it from another:
em --root $ROOT -1v sys-apps/foo &
em --root $ROOT log current     # in-flight packages + phase + elapsed
em --root $ROOT log predict     # ETA for the running session

# 4. After it finishes:
em --root $ROOT log list -n 10
em --root $ROOT log time sys-apps/foo

# 5. Stream the typed event feed to a file and inspect it:
em --root $ROOT -1 app-misc/foo --activity-jsonl=/tmp/ev.jsonl
jq . /tmp/ev.jsonl | less
# or via FD:
em --root $ROOT -1 app-misc/foo --activity-fd=3 3>/tmp/ev.jsonl

# 6. emerge.log dual-write (then read with qlop if installed):
EM_EMERGELOG=1 em --root $ROOT -1 app-misc/foo
cat $ROOT/var/log/emerge.log
```

Inspecting the on-disk state directly is also useful:

```bash
find $ROOT/var/cache/edb/em-activity -type f
cat $ROOT/var/cache/edb/em-activity/live/*/session.json | jq .
cat $ROOT/var/cache/edb/em-activity/history/merges.jsonl | jq -s .
```

## Library use

`portage_cli` re-exports `ActivityBus`, `ActivityEvent`, `ActivitySink`,
`BackgroundSink`, `LiveProjection`, and `DurationStore`. Embedders and
`crossdev-stages` can `bus.subscribe()` for an in-process `broadcast::Receiver`
and/or attach their own sinks, sharing one event schema with the subprocess
wire form above. See `todo/activity-status.md` § "Plugging into emerge_atoms".
