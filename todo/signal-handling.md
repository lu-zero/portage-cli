# Signal handling: Ctrl+Z wedges the console, Ctrl+C corrupts the VDB

Status: 🔴 not started, root-caused 2026-09-02. Reported symptom (Luca): on
`em cat/pkg`, "I couldn't get back the console after ctrl+z, I had to use
kill to continue again."

`em` installs **no signal handlers at all**. Verified on a live process:

```
SigBlk: (none)
SigIgn: PIPE                      # Rust runtime default
SigCgt: BUS, SEGV, RT-33          # Rust stack guard + glibc NPTL, nothing of ours
```

No `tokio::signal`, no `ctrl_c`, no `sigaction` anywhere in the workspace.
Every job-control and termination signal takes its default action.

## 1. Ctrl+Z wedges the console — root cause found

**Not** an `em` suspend bug. `em` itself suspends and resumes correctly;
measured on `em regen -j 2` (258 threads):

| | state | CPU delta |
|---|---|---|
| running | `Sl` | — |
| after `SIGTSTP` | `Tl` (all 258 threads `T`) | 0 ticks over 4s |
| after `SIGCONT` | `Sl` | +602 ticks over 3s |

The wedge comes from the **build children**, via brush:

- `brush_core::ProcessGroupPolicy` defaults to `NewProcessGroup`
  (`interp.rs`, `#[default]`), and neither `portage-repo` nor `portage-cli`
  ever sets `SameProcessGroup`.
- Build-phase stdin is the user's real terminal — stated outright in
  `portage-repo/src/build/commands/mod.rs`: "A non-redirected stdin (the
  real terminal) is inherited." The pty (`build/pty.rs`) is opened `NOCTTY`
  and only captures stdout/stderr, so it does not shield stdin.
- So in `brush-core/src/commands.rs`, `new_pg` is true and
  `child_stdin_is_terminal` is true for every external command a phase runs
  (`gcc`, `make`, `install`, `patch`, …). Each one therefore gets
  `cmd.process_group(0)` **and** `cmd.take_foreground()` — a `tcsetpgrp()`
  making *itself* the terminal's foreground process group.

Consequence: Ctrl+Z delivers SIGTSTP to whichever compiler currently owns
the terminal, not to `em`. That one child stops; `em` is in a background
process group, never sees the signal, carries on, and spawns the next
command — which grabs the foreground again. The shell's job never registers
as stopped, so `fg` has nothing to resume and the console stays hijacked.
Killing `em` is the only way out, exactly as reported.

### Fix

Two independent changes, both worth doing:

1. **`ProcessGroupPolicy::SameProcessGroup` for build-phase execution.**
   `em` is not an interactive shell and must not do job control: build
   children belong in `em`'s own process group so the terminal delivers
   SIGTSTP/SIGINT to the whole tree at once. This is the actual fix for the
   reported symptom.
2. **Redirect phase stdin from `/dev/null`.** Real portage does this unless
   `FEATURES=interactive`; a build that reads stdin is a bug, and today such
   a build silently steals the user's keystrokes instead of getting EOF. It
   also makes `child_stdin_is_terminal` false, closing the `take_foreground`
   path for good rather than relying on the policy alone.

Neither needs a signal handler. Do these before anything below.

## 2. Ctrl+C can leave a half-written VDB entry

`portage_vdb::Vdb::register` (`portage-vdb/src/write.rs`) creates the final
`<cat>/<pf>/` directory and then writes `EAPI`, `CATEGORY`, `SLOT`, `USE`,
`IUSE`, … one `std::fs::write` at a time, in place. There is no temp-dir
plus rename, and `-MERGING-` (portage's own marker for an interrupted
merge) appears nowhere in the workspace.

A SIGINT between `create_dir_all` and the last field leaves a directory that
every later read treats as a real installed package with fields missing —
the stale-VDB shape that has bitten this project before. `ebuild.rs`'s
merge critical section (`lock_merge_flock` / `merge_gate`) serialises
*concurrent `em` processes* but does nothing about interruption.

Fix: register into `<cat>/-MERGING-<pf>/` and `rename` into place as the
last step. Matching that directory-naming convention is an interface match,
not a source copy, so it is fine under the licensing rule. A stale
`-MERGING-` dir then becomes detectable and discardable on the next run.

## 3. Suspend time pollutes the ETA history

`ActivityEvent::now()` is `SystemTime::now()` — wall clock. Package
durations are start/end deltas of that, appended to
`var/cache/edb/em-activity/history/merges.jsonl` and used as ETA training
data (`activity/history.rs`). Suspending a merge for ten minutes writes a
ten-minute-longer duration for that package, permanently skewing every
future estimate for it.

`HistorySink` already excludes `ActivityMode::Regen` "so it must not pollute
the merge-duration history used for ETA" — the same reasoning applies to
suspended time. Wants either a monotonic `Instant` for the duration or a
SIGCONT-aware correction. Related: [[activity-storage-format]],
[[activity-status]].

## 4. Locks are held while suspended

`.builddir.lock` and `.merge.lock` (`ebuild.rs`) are `flock`s on open fds.
The kernel releases them when a process *dies*, so Ctrl+C leaks nothing —
but a **suspended** `em` holds them indefinitely, and a second `em` on the
same root blocks on the acquire with no message about why. A "waiting for
the build lock held by pid N" diagnostic after a short timeout would cost
little. Related: [[workdir-dual-root]].

## 5. Children are orphaned if `em` is killed directly

No `process_group`, `pre_exec`, `kill_on_drop`, or child-killing `Drop`
anywhere in `portage-cli`/`portage-repo`. A terminal Ctrl+C reaches the
whole foreground group so it mostly works out; `kill -INT <em-pid>` from
elsewhere kills only `em` and leaves `gcc`/`make` running against a work
directory nobody owns any more. Fixing item 1 makes the process-group
membership deliberate, at which point signalling the group on shutdown
becomes straightforward.

## Non-issues — checked, do not "fix"

- **Hidden cursor after an interrupt.** indicatif here never emits
  `ESC[?25l`; a captured `em regen` run under a pty contains 0 hide-cursor
  and 0 show-cursor sequences (118 `ESC[2K` line clears instead). There is
  no invisible-cursor bug to fix.
- **SIGWINCH.** `activity/human.rs` re-reads `terminal_size()` on every
  redraw, so a resize self-heals. No handler needed.
- **The pty stealing the terminal.** `build/pty.rs` opens with `NOCTTY`,
  never calls `setsid`/`TIOCSCTTY`, and only captures stdout/stderr.
- **SIGPIPE.** Ignored by the Rust runtime; `em query list | head` exits 0.

## Verification recipe

Suspend/resume of `em` itself, measuring CPU rather than buffered output:

```sh
em regen -j 2 -o /tmp/out /var/db/repos/gentoo & EM=$!
cpu() { awk '{print $14+$15}' /proc/$1/stat; }
sleep 3; kill -TSTP $EM; A=$(cpu $EM); sleep 2
[ "$(cpu $EM)" = "$A" ] && echo "stopped cleanly"
kill -CONT $EM; sleep 2; [ "$(cpu $EM)" -gt "$A" ] && echo "resumed"
```

Signal dispositions of a live process:

```sh
grep -E '^Sig(Blk|Ign|Cgt)' /proc/$(pgrep -x em)/status
```

Note that a stop signal sent to an **orphaned** process group is discarded
by the kernel — driving this from a detached `script`/`setsid` wrapper makes
`em` look like it ignores SIGTSTP when it does not.

For item 1, the direct check is whether a build child leaves `em`'s process
group during a real merge:

```sh
em cat/pkg &            # in a real terminal
ps -o pid,pgid,stat,comm -g $(ps -o pgid= -p $(pgrep -x em) | tr -d ' ')
```
