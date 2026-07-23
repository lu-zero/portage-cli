# Activity status & timing (structured; emerge.log optional)

STATUS: 🟢 PR1–7 + follow-ups landed 2026-07-22 — bus, live FS, history JSONL,
phases, `em log current|list|time|predict` (critical-path when session has
plan/blockers), `em -p --eta`, `--activity-fd`/`--activity-jsonl`, opt-in
`--emergelog`, install-worker LiveFs + Unix-socket re-emit to parent bus.
`HumanStdoutSink` (sink `[5]`) then re-matched against real emerge's own
`_emerge/{MergeListItem,EbuildBuild,Binpkg,JobStatusDisplay}.py` (2026-07-22,
ffa27f5): real emerge's exact wording (`Emerging`/`Emerging binary`/
`Fetching`), its colour palette (`C_COUNT`/`C_PKG`/`C_PKG_BINARY` in
`style.rs`), a redrawn `Jobs: N of M complete, R running` line for
`--jobs > 1`, and `-q` no longer swallowing `PkgEnd` failures (real
`--quiet` never silences errors). **Corrected again same day (1892f21)**
against a real `--jobs N` trace the user supplied: ffa27f5's "one headline
`=== ...` banner per package" was still wrong — that message
(`EbuildBuild`'s `logger.log`) only ever reaches `emerge.log`
(`_emerge/Scheduler.py`'s `_emerge_log_class`), never the terminal. Real
emerge's terminal instead shows `_emerge/PackageMerge.py`'s
`>>> Installing (N of M) cpv[ to ROOT/]` / `>>> Completed (...)` around the
qmerge/merge phase, plus a `for`/`to ROOT/` suffix on all three banners
(`Emerging`/`Installing`/`Completed`) whenever `ROOT != "/"` — now
reproduced using `SessionStart`'s real `merge_root`/`host_root` strings.
Default output shows nothing between `Emerging` and `Installing` again;
`verbose >= 1` keeps the old every-phase view as an intentional `em`-only
extra (never real emerge behavior, so nothing lost keeping it opt-in).

**Open gap: load average, for full `Jobs:` line parity.** Real emerge's
`JobStatusDisplay._display_status` pads the `Jobs: N of M complete[, R
running][, F failed]` line out to the terminal width and appends
`Load avg: 1.23, 1.10, 0.98` (`_emerge/getloadavg.py`: `os.getloadavg()`,
falling back to parsing `/proc/loadavg`'s first three fields on platforms
without it — three decimals-of-precision rule in
`JobStatusDisplay._load_avg_str` depends on the max of the three: `<10` →
`.2f`, `<100` → `.1f`, else `.0f`). `em`'s `draw_status` in
`activity/human.rs` has no load-average field at all yet — needs a small
`getloadavg()`-equivalent (read `/proc/loadavg`, split on whitespace, parse
the first three floats) appended to the line the same way.

**While checking this, found a related dead flag, same class as the
`-K`/`-X` bugs from the 2026-07-21 parser audit
([[parser-audit-full-pass-2026-07-21]]):** `-l`/`--load-average` is parsed
(`cli/merge_flags.rs`), threaded through `-r`/`--resume` and crossdev's
flag-merge helpers (`maint/resume.rs`, `crossdev/mod.rs`), but **never once
read** by the actual scheduler — `merge/mod.rs`/`emerge.rs` have no
`load_average` reference at all. Real emerge's `--load-average` throttles
*starting new parallel jobs* when load is already too high (checked before
each new `FuturesUnordered` slot in `merge_parallel`); `em` currently
ignores the flag entirely and always starts up to `--jobs N` regardless of
system load. Fixing both the display and the throttle at the same time
makes sense — they need the same underlying load-average read.

**`--eta` on `-a`/`--ask`, and prettier output — DONE (2026-07-23).** The
eta-computation block (`DurationStore::load` / `EtaPkg` collection /
`estimate_remaining_with_blockers`) is now a shared `eta_message(roots,
merge_flags, outcome)` helper in `emerge.rs`, called from both the
`-p`/`--pretend` branch and right before the `confirm_action(...)` call on
the `-a`/`--ask` path — an `-a`-confirmed real run now sees the same
estimate a `-p` preview does, right where the user is actually asked to
say yes. `format_eta` (`activity/history.rs`) is also reworked: the
wall-time headline is bold (`style::C_BOLD`, `"ETA ~{C_BOLD}0s{C_BOLD:#}
wall (critical-path, 1 job)"`), the known/unknown/serial detail moved to
its own indented line, and the "N unknown package time(s)" clause is
dropped entirely when `unknown == 0`. Since the string now carries
embedded style codes, both call sites (`emerge.rs` and `dispatch.rs`'s
`em log predict`) switched from plain `print!` to `write!(anstream::stdout(),
…)` so the codes are stripped on non-tty output — live-verified both ways
(`script(1)` pty capture shows the escape codes; piping to a file shows
none).

**Open polish (take if you want):** richer emerge.log timestamps (`chrono_like`
is still `unix {secs}` — Portage uses ctime-style local time); `PkgKind::Binpkg`
is still never actually emitted at `PkgStart` (`merge/mod.rs::pkg_kind` only
distinguishes `FetchOnly`/`Source` — the binpkg-reuse decision happens later,
inside `act_on_package`) so the binary-merge wording/colour path above is
correct but currently unreachable; fixing it means hoisting that reuse
decision earlier or re-classifying at `PkgEnd`.

## Problem

`emerge.log` is a flat, append-only **text transcript** of free-form lines.
It is adequate for coarse history and what qlop/genlop/emlop already parse.
It is a **poor** source of truth for live dashboards, ETA, and — critically —
**orchestration front-ends**.

Front-ends we care about (especially **crossdev-stages**, and anything that
will drive `em` as a **library** rather than only as a CLI):

- start one or more merge sessions (process **or** in-process `emerge_atoms`)
- need a **stream of structured events** (session / package / phase / result)
- must not scrape stdout or emerge.log to know “gcc is in compile for 9m”
- want the same events whether they embedded `portage_cli` or spawned `em`

`em` today has stdout banners, per-package `build.log`, and resume markers —
none of them are a clean progress API.

The existing stub surface already expects a better world:

```text
em log current   # Show currently running merges
em log list      # Show merge history
em log time      # Show merge times for a package
```

(README maps `log` → genlop-class tooling; all three subcommands still bail.)

## Goals

1. **Structured event bus (primary)** — every meaningful transition emits a
   typed `ActivityEvent`. Front-ends consume a **channel** (in-process) or a
   **JSONL event stream** (subprocess). Disk snapshots and emerge.log are
   *sinks* of that bus, not the API.
2. **Live status** — O(sessions) answer: N ongoing activities; for each, plan
   progress and every in-flight package’s phase + timings (via bus + optional
   on-disk mirror for external observers / crash recovery).
3. **Duration store** — durable timings for ETA on updates.
4. **emerge.log compatibility** — optional dual-write; never the control plane.
5. **Library-first** — `portage_cli` already exposes `run` / `emerge_atoms`;
   activity must plug in without requiring the binary.

Non-goals (v1):

- Replacing per-package `build.log` (debug detail stays there).
- Perfect critical-path scheduling theory for ETA (good median-based estimate first).
- Full elog / split-log FEATURES parity.
- Replacing crossdev-stages’ own stage orchestration (it remains the driver;
  em only emits per-merge events).

---

## Architecture: one bus, many sinks

```
                    emit(ActivityEvent)
   merge / ebuild / unmerge ─────────────────┐
                                              ▼
                                    ┌──────────────────┐
                                    │  Activity bus      │
                                    │  (fan-out)         │
                                    └────────┬─────────┘
               ┌──────────────┬──────────────┼──────────────┬────────────┐
               ▼              ▼              ▼              ▼            ▼
        [1] Channel     [2] Live FS     [3] History    [4] emerge.log  [5] Human
        mpsc/watch      inflight/       JSONL          compat lines    stdout
        (library /      session.json    (ETA)          (optional)      (CLI)
         stages UI)     (em log current,
                         other processes)
```

**Rule:** call sites in merge/ebuild only know `bus.emit(event)`. They never
format emerge.log strings or know about JSONL paths. Sinks register at session
start based on how the caller invoked em.

| Consumer | How it attaches |
|----------|-----------------|
| **crossdev-stages** (in-process, future) | `ActivityBus::channel()` → `Receiver<ActivityEvent>` or async stream |
| **crossdev-stages** (subprocess `em` today) | `em … --activity-jsonl -` or FD / socket; stages reads lines |
| **em log current** / external tools | read live FS snapshot (sink 2), or attach JSONL |
| **qlop / genlop / emlop** | emerge.log sink only |
| **CLI user** | human stdout sink (existing `>>>` banners, can be driven by same events) |

This is the important product point: **structured mode is not “a nicer log
file.”** It is the API surface orchestrators use to drive and observe em.

---

## ActivityEvent (wire + Rust)

Typed events, `Serialize` for JSONL/subprocess, cheap `Clone` for fan-out.

```rust
pub enum ActivityEvent {
    SessionStart {
        job_id: String,
        pid: u32,
        started_at: f64,
        argv: Vec<String>,
        merge_root: String,
        host_root: String,
        mode: ActivityMode,       // Merge | Unmerge | Depclean | …
        plan_total: u32,
        flags: SessionFlags,      // jobs, emptytree, update, deep, …
    },
    SessionHeartbeat { job_id: String, at: f64, completed: u32, failed: u32 },
    SessionEnd {
        job_id: String,
        at: f64,
        ok: bool,
        completed: u32,
        failed: u32,
        seconds: f64,
    },

    PkgStart {
        job_id: String,
        cpv: String,
        cpn: String,
        merge_root: MergeRoot,    // host | target
        index: u32,
        of: u32,
        kind: PkgKind,            // Source | Binpkg | FetchOnly
        at: f64,
    },
    PhaseEnter {
        job_id: String,
        cpv: String,
        merge_root: MergeRoot,
        phase: String,            // fetch | compile | qmerge | …
        at: f64,
    },
    PhaseLeave {
        job_id: String,
        cpv: String,
        merge_root: MergeRoot,
        phase: String,
        at: f64,
        seconds: f64,
    },
    PkgEnd {
        job_id: String,
        cpv: String,
        cpn: String,
        merge_root: MergeRoot,
        kind: PkgKind,
        ok: bool,
        at: f64,
        seconds: f64,
        phases: Vec<(String, f64)>,
        error: Option<String>,
    },
}
```

JSONL wire form (subprocess / file):

```json
{"v":1,"event":"phase_enter","job_id":"…","cpv":"sys-devel/gcc-14.2.1","phase":"compile","at":1721600110.5,…}
```

Version field `v` for forward compatibility. Unknown events: readers skip.

### Bus API (library)

```rust
/// Fan-out hub for one process (or one staged multi-step driver).
pub struct ActivityBus { /* inner: broadcast or list of sinks */ }

impl ActivityBus {
    pub fn new() -> Self;
    /// In-process consumer (crossdev-stages UI, tests, embedding apps).
    pub fn subscribe(&self) -> broadcast::Receiver<ActivityEvent>; // or mpsc
    /// Install a sink (live FS, history, emergelog, stdout).
    pub fn add_sink(&self, sink: Arc<dyn ActivitySink>);
    pub fn emit(&self, event: ActivityEvent);
}

pub trait ActivitySink: Send + Sync {
    fn on_event(&self, event: &ActivityEvent);
}
```

Threading: merge is async/single-threaded task model today (`FuturesUnordered`
without `Send` shells). Prefer **`tokio::sync::broadcast`** or a
`Vec<mpsc::UnboundedSender>` drained on emit from the same runtime. Sinks that
do disk I/O should be fast (append / write_atomic) or push to a background
task if needed.

### Plugging into `emerge_atoms` / staged driver

Today `run_staged` calls `emerge_atoms` in a loop with only stdout headers.
Target shape:

```rust
// Library / stages
let bus = ActivityBus::new();
let mut rx = bus.subscribe();
bus.add_sink(LiveFsSink::new(roots));
bus.add_sink(HistorySink::new(roots));
// bus.add_sink(EmergeLogSink::new(…));  // optional compat

tokio::spawn(async move {
    while let Ok(ev) = rx.recv().await {
        stages_ui.on_activity(ev);  // progress bar, step panel, …
    }
});

emerge_atoms(cli, atoms, EmergeOpts {
    activity: Some(bus.clone()),
    …
}).await?;
```

CLI `em` builds a default bus (live FS + history + human stdout + optional
emergelog) inside `run` / `emerge_atoms_inner` when no bus is passed.

**Subprocess mode** for current crossdev-stages (still shells out):

```bash
em -uDN @world --activity-jsonl=/path/to/fifo
# or --activity-jsonl=-  → stdout side-channel is wrong; use FD:
em -uDN @world --activity-fd=3  3>events.jsonl
```

Stages opens the FD/fifo and deserialises `ActivityEvent` the same as the
in-process channel. One schema for both.

### What stages gets without parsing logs

| Need | Event(s) |
|------|----------|
| Step still running overall | `SessionStart` … no `SessionEnd` yet |
| Packages in flight + phase | last `PhaseEnter` per cpv with no `PkgEnd` (or maintain local state machine from stream) |
| Timing so far | `PhaseLeave.seconds`, `PkgEnd.seconds` |
| ETA for remainder | local projection + history sink / `estimate_remaining` using plan_total and completed |
| Failure | `PkgEnd { ok: false, error }` then maybe `SessionEnd { ok: false }` |

Helper (optional crate-side):

```rust
/// Fold events into a LiveSession snapshot (same shape as em log current).
pub struct LiveProjection { … }
impl LiveProjection {
    pub fn apply(&mut self, ev: &ActivityEvent);
    pub fn inflight(&self) -> &[InflightPkg];
    pub fn eta(&self, history: &DurationStore) -> Option<Eta>;
}
```

Front-ends can either fold themselves or use `LiveProjection` so UI and
`em log current` share one reducer.

---

## On-disk sinks (A/B/C) — still required

The bus is necessary but not sufficient alone:

| Sink | Why keep it |
|------|-------------|
| **Live FS** | Other processes / `em log current` / crash recovery when no channel is attached |
| **History JSONL** | ETA across reboots; `em log time` |
| **emerge.log** | Ecosystem tools |

When a channel subscriber exists **and** live FS is enabled, both see the same
events. Channel is low-latency; FS is durable/observable.

Resume markers (em-resume.done/) stay a *separate* concern:
  “which packages already finished *this* interrupted job?”
  — same `job_id` as `SessionStart.job_id` for correlation.

---

## A. Live status (FS sink detail)

### Layout (root-scoped)

```
<merge_root>/var/cache/edb/em-activity/
  live/<job_id>/
    session.json          # job meta (rewritten rarely)
    inflight/
      host/<cat>/<pf>.json
      target/<cat>/<pf>.json
  # optional: pid file for stale detection
```

Host “live system” uses `/` as merge_root → `/var/cache/edb/em-activity/…`
unless we later honour a global override (see path policy).

**Why a directory of inflight files, not one big status JSON?**

Same lesson as resume completions: under `--jobs N`, many packages update
phase independently. One shared JSON + mutex is the emerge.log pain in a new
hat. Independent `create`/`write_atomic`/`unlink` per package keeps writers
non-contending; readers readdir + parse small files.

### `session.json` (stable-ish)

```json
{
  "job_id": "18f2a…-12345",
  "pid": 12345,
  "started_at": 1721600000.12,
  "argv": ["em", "-uDN", "@world"],
  "merge_root": "/",
  "host_root": "/",
  "mode": "merge",
  "plan_total": 40,
  "flags": { "jobs": 8, "emptytree": false, "update": true, "deep": true },
  "heartbeat_at": 1721600123.0
}
```

- Written at job start (after pretend/ask), heartbeat every few seconds or on
  each package event from the scheduler loop.
- Removed (or moved to `done/`) when the job exits cleanly.
- Stale if `pid` is dead **and** `heartbeat_at` is older than a threshold
  (crash / kill −9): `em log current` can mark “stale” and offer cleanup.

### Inflight package file

Path mirrors resume markers: `inflight/{host|target}/{cat}/{pf}.json`

```json
{
  "cpv": "sys-devel/gcc-14.2.1_p20241221",
  "cpn": "sys-devel/gcc",
  "merge_root": "target",
  "index": 13,
  "of": 40,
  "kind": "source",
  "pkg_started_at": 1721600100.0,
  "phase": "compile",
  "phase_started_at": 1721600110.5,
  "phases_done": [
    { "phase": "fetch", "seconds": 2.1 },
    { "phase": "unpack", "seconds": 8.0 },
    { "phase": "prepare", "seconds": 1.2 },
    { "phase": "configure", "seconds": 45.0 }
  ]
}
```

Lifecycle:

| Event | Action |
|-------|--------|
| package starts (`act_on_package` / scheduler) | create inflight file, `phase: "starting"` |
| phase boundary (`run_one_phase` enter/leave) | update `phase`, `phase_started_at`, append to `phases_done` on leave |
| package success | unlink inflight; append **B** history record; resume `mark_completed`; optional C line |
| package failure | unlink inflight; append **B** with `ok: false`; optional C failure line |
| job end | remove `live/<job_id>/` (or rename to archive) |

Binpkg / fetch-only set `kind` accordingly; phases may be just `qmerge` or `fetch`.

### Answering the dashboard questions

```text
em log current
```

1. List `em-activity/live/*/` (and other roots if multi-root later).
2. Drop stale sessions (dead pid + old heartbeat).
3. For each session: read `session.json` + readdir `inflight/**`.
4. Render:

```text
2 ongoing activities

[1] em -uDN @world  (pid 12345, started 12m ago)  root=/
    plan 12/40 done, 3 failed, 8 jobs
    inflight (3):
      sys-devel/gcc-14.2.1   compile   9m12s (pkg 14m total)   [13/40]
      dev-lang/rust-1.83     configure 1m03s                   [14/40]
      app-misc/foo-1.0       qmerge    4s     (binpkg)         [15/40]

[2] em --root /var/tmp/stage …  (pid 13001, …)
    …
```

No emerge.log parse required.

### Hooks in the code (minimal set)

| Site | Event |
|------|--------|
| `emerge_atoms_inner` after save/resume job id | `session_start` |
| `merge_sequential` / `merge_parallel` package start | `pkg_start` |
| `ebuild::run_one_phase` (enter/exit) | `phase_enter` / `phase_leave` |
| package Ok/Err in merge loops | `pkg_end` |
| end of `run_merge_plan` / emerge | `session_end` |
| unmerge/depclean paths | separate `mode` sessions or same schema with `mode: unmerge` |

Phase hooks are the only slightly invasive bit; everything else already has a
natural call site next to resume markers / stdout banners.

---

## B. Duration history (ETA substrate)

### Layout

Append-only **JSONL** (one object per finished package action), root-scoped:

```
<merge_root>/var/cache/edb/em-activity/history/merges.jsonl
```

Optional later: rotate by month (`merges-2026-07.jsonl`) if files grow large.

Example record:

```json
{
  "ts_end": 1721601000.0,
  "job_id": "18f2a…",
  "cpn": "sys-devel/gcc",
  "cpv": "sys-devel/gcc-14.2.1_p20241221",
  "merge_root": "target",
  "kind": "source",
  "ok": true,
  "seconds": 892.4,
  "phases": {
    "fetch": 2.1,
    "unpack": 8.0,
    "prepare": 1.2,
    "configure": 45.0,
    "compile": 800.0,
    "install": 20.0,
    "qmerge": 16.1
  }
}
```

**Why not only emerge.log for this?**  
JSONL is trivial to scan tail-first, filter by `cpn`, and compute median without
regex archaeology. Phase breakdown enables smarter ETA (“compile-heavy left”).

**Write path:** single `OpenOptions::append` write of one line per package end
(same durability model as emergelog, but structured). No rewrite of prior
history. Optional short flock if we want multi-process append safety on NFS
edge cases; on local disk append of one line is usually enough.

### ETA for an update plan

Inputs:

- Current plan: list of remaining `(cpn, cpv, kind)` (from depgraph / remaining
  after resume filter).
- History: last *K* successful `ok: true` records per `cpn` (default K=15–20,
  version-agnostic first; optional prefer same major version).

Estimate:

```
seconds_i = median(history[cpn_i].seconds)   # or mean; emlop-style median
            fallback: global default or “unknown”
```

Parallelism:

- **Naive (v1):** `sum(seconds_i) / jobs` — simple, often optimistic.
- **Better (v2):** keep build-order blockers; estimate critical path with
  `jobs` workers (same graph `run_merge_plan` already has).

Surface:

```text
em log predict          # if a live session exists, ETA for its remainder
em -p -uDN @world --eta # optional: print estimate next to pretend (later)
```

`em log time [atom]` becomes a history query (median/mean/last N), not a
genlop shell-out.

### Unmerge / sync / depclean

Same JSONL file with `"kind": "unmerge" | "sync" | …` **or** separate
`history/unmerges.jsonl`. Prefer one file + `kind` field for simpler `em log`.

---

## C. emerge.log compatibility (optional dual-write)

Keep Portage line format as a **projection** of the same events, not the store:

```
{ts}: Started emerge on: …
{ts}:  *** emerge -uDN @world
{ts}:  >>> emerge (13 of 40) sys-devel/gcc-14.2.1 to /
{ts}:  === (13 of 40) Compiling/Merging (sys-devel/gcc-14.2.1::gentoo)
{ts}:  ::: completed emerge (13 of 40) sys-devel/gcc-14.2.1 to /
{ts}:  *** exiting successfully.
```

- Default: **on** for host-ish roots so existing muscle memory works; or off
  until `FEATURES`/config says so — product decision.
- Path: Portage default `/var/log/emerge.log` for live `/`; root-scoped
  `<root>/var/log/emerge.log` for offsets (stages/cross).
- Implementation: one function `compat_emergelog(line)` called from the same
  event hooks that update A/B.
- Unknown/extra lines (e.g. explicit FAILURE) are fine; qlop ignores what it
  does not know.

**Do not** reverse-parse emerge.log to build live status or ETA once A/B exist.

---

## API sketch

```rust
// portage-cli/src/activity.rs  (public enough for library consumers)
// or maint::activity with re-export from lib.rs

pub enum ActivityEvent { /* see above */ }
pub struct ActivityBus { … }
pub trait ActivitySink: Send + Sync { fn on_event(&self, event: &ActivityEvent); }

pub struct LiveProjection { … }  // reducer: events → dashboard snapshot
pub struct DurationStore { … }   // history JSONL reader + median/ETA

// EmergeOpts gains:
//   pub activity: Option<ActivityBus>,
```

`job_id` = same id resume already assigns (`save` → job_id), so resume markers,
live status, history rows, and bus events share one correlation key.

**lib.rs:** export `ActivityEvent`, `ActivityBus`, `LiveProjection` (and
eventually a stable `emerge_atoms` / run opts that take the bus) so
crossdev-stages can depend on `portage-cli` as a library without scraping CLI
output.

---

## `em log` UX (land the stubs)

| Command | Reads | Output |
|---------|--------|--------|
| `em log current` | A (`live/`) | N sessions, inflight packages, phases, times |
| `em log list [-n N]` | B (tail of JSONL) and/or C | recent completed actions |
| `em log time [atom]` | B | last/median/mean durations |
| `em log predict` (new) | A + B + optional plan | ETA for live remainder |

Optional: `em log gc` to drop stale live sessions / rotate history.

---

## Path / multi-root policy

**Recommendation:** root-scoped under each merge root’s
`var/cache/edb/em-activity/` (and compat log under that root’s `var/log/`).

- Host BDEPEND entries in a cross plan write **host** activity under host root.
- Target packages under target root.
- `em log current` without flags: current CLI roots (same as other applets);
  `--all-roots` later if needed.

Avoid putting stage activity only in host `/var/log/emerge.log` — that is how
offsets become invisible.

---

## Rollout plan (PR-sized)

Order is deliberate: **bus + channel first**, so stages can integrate before
every sink is perfect.

| PR | Deliverable |
|----|-------------|
| **1** | `ActivityEvent` + `ActivityBus` (broadcast subscribers + direct sinks); recording sink for tests |
| **2** | Hook merge start/pkg start/pkg end + session end; share `job_id` with resume; `ActivitySessionOpts` (job_id / parent_job_id); CLI default bus |
| **3** | `LiveFsSink` + `em log current`; `LiveProjection` reducer shared with CLI |
| **4** | Phase enter/leave in `run_one_phase`; phase fields on events |
| **5** | `HistorySink` JSONL + `em log list` / `time` + `em log predict` + `em -p --eta` (same helper) |
| **6** | Subprocess wire: `--activity-fd` / `--activity-jsonl` JSONL stream (stages without linking) |
| **7** | Opt-in emerge.log sink; docs; README un-stub `log`; export bus from `lib.rs` |

PR1–2: stages (or a demo) can `subscribe()` in-process and render progress
from events alone. PR6 covers today’s shell-out stages. Disk/ETA/compat
follow without changing the event schema.

---

## Risks & mitigations

| Risk | Mitigation |
|------|------------|
| Phase hooks miss worker split (Compile vs Install process) | Session continues across worker; phase updates from both parent and `__worker` with same job_id/cpv (pass job_id in WorkerArgs) |
| Stale live dirs after kill −9 | pid + heartbeat; `em log current` shows stale; gc on next start |
| Disk spam from phase updates | write_atomic small JSON only; no fsync every phase unless configured |
| ETA garbage for rare packages | show “unknown”; don’t invent numbers; optional warm-up from emerge.log import once |
| Double-counting with resume markers | different dirs; same job_id for correlation only |

---

## Decisions (locked 2026-07-22)

Settled with the user before implementation. Do not re-litigate in code review
without an explicit design change.

### Product / sinks

| # | Choice | Decision |
|---|--------|----------|
| 1 | emerge.log dual-write | **Opt-in only.** Default off. Enable via config / FEATURES-style flag / CLI when someone wants qlop/genlop/emlop. Structured bus + live FS + history remain the real API. |
| 2 | ETA surface in v1 | **`em log predict` and `em -p --eta`.** Predict is the dedicated query; pretend can print an estimate without making emerge.log the source. Library exposes the same helper stages can call. |
| 3 | Import host emerge.log → JSONL | **No import in v1.** History starts when em starts recording. Avoid fragile one-shot parsers; optional later if ETAs feel cold-start bad. |
| 4 | In-process subscribe | **`tokio::sync::broadcast` (or equivalent).** Multiple listeners (UI + tests + optional side sinks) without a single-consumer bottleneck. Lagging subscribers may drop progress events — acceptable for dashboards; history/live FS sinks must not use a lossy broadcast path for durability (they are direct sinks on emit). |
| 5 | Staged multi-step sessions | **Caller chooses via API.** Default when unspecified: **one session (`job_id`) per `emerge_atoms` invocation**. Callers (crossdev-stages, `run_staged`) may open an outer session and/or pass a parent correlation id — see below. |

### Already-agreed architecture (not reopened)

1. **Primary API:** typed `ActivityEvent` bus + broadcast subscribe (library) / JSONL FD (subprocess).  
2. **Primary durable sinks:** live FS snapshot + history JSONL.  
3. **emerge.log:** optional compatibility sink only (decision #1).  
4. **Concurrency:** event emit is fan-out; live FS uses per-package files (no global status lock).  
5. **job_id:** shared with resume markers.  
6. **ETA math:** median of last K successful merges per Cpn; wall uses critical-path list-schedule when `build_blockers` / live-session plan is available (`em -p --eta`, `em log predict`).
7. **Path:** root-scoped under `var/cache/edb/em-activity/`.  
8. **Front-ends:** prefer in-process `subscribe()` long-term; `--activity-fd` while still spawning `em`.

### Decision #4 detail — broadcast vs durable sinks

```text
emit(event)
  ├─► broadcast to subscribers   (lossy OK if lagging — UI only)
  └─► for sink in direct_sinks:  (live FS, history, emergelog — must not drop)
        sink.on_event(event)
```

Subscribers never carry the durability obligation. History/live FS register as
**direct sinks**, not as broadcast consumers.

### Decision #5 detail — session granularity API

```rust
pub struct ActivitySessionOpts {
    /// If set, reuse this job_id / continue an outer session instead of
    /// minting a new one at emerge_atoms start. Resume markers still key
    /// off whatever job_id is active for *completion* of packages in this
    /// merge (see note below).
    pub job_id: Option<String>,
    /// Optional parent id for UI correlation only (e.g. stage plan id).
    /// Appears on SessionStart / all child events as `parent_job_id`.
    pub parent_job_id: Option<String>,
}
```

**Defaults:**

| Caller | Behavior |
|--------|----------|
| CLI `em <atoms>` | New session per invocation; no parent. |
| `run_staged` without opts | **New session per step** (each `emerge_atoms`); optional `parent_job_id` = stage-run id for stages UI. |
| crossdev-stages library | Free to pass outer `job_id` for a single progress bar across steps, or parent-only correlation — its choice. |

**Resume note:** package completion markers must remain unambiguous. If a
caller reuses one `job_id` across multiple `emerge_atoms` steps, markers
accumulate under that id (good for “continue whole stage1”). If they use
per-step ids (default), resume is per-step. Document this on `ActivitySessionOpts`.

**Event field:** every `ActivityEvent` carries `job_id`; when present also
`parent_job_id: Option<String>` for tree UIs.

### ETA (#2) — pretend integration

- `em log predict` — if a live session exists, ETA for its remainder; else
  error or “nothing running.”
- `em -p … --eta` — after the plan is built (pretend path), estimate total
  wall time from history + `jobs` without starting a session. No live
  inflight. Stages can call the same pure function on a plan slice.

---

## Relation to recent work

- Resume markers (`em-resume.done/<job_id>/…`) remain “completed for this job.”  
- Activity live/inflight is “running now.”  
- History JSONL is “how long things took.”  
- The **bus** is how orchestrators observe all of the above without scraping.  
- Do not fold these into emerge.log again — that is how live status became
  unserviceable.
