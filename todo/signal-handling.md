# Signal handling: terminal state, and an interruptible VDB write

Status: 🟡 phase-stdin fix landed 2026-09-02; items 2-5 open. Reported symptom (Luca): on
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

## 1. Phase stdin was the caller's terminal — FIXED 2026-09-02

**A first root-cause theory was wrong and is recorded here so nobody
re-derives it.** The theory was that brush's `ProcessGroupPolicy` default of
`NewProcessGroup` made every build child `setpgid` + `tcsetpgrp` itself into
the terminal's foreground group. It does not: `default_exec_params()` derives
the policy from `options.enable_job_control`, which is `create_options
.interactive` — false for `em`'s embedded shell — so the policy is already
`SameProcessGroup` and brush never reaches its `take_foreground()` branch.
Measured directly through the real phase path: a command spawned by a phase
reports the same pgid and sid as `em` itself.

`em` also suspends and resumes correctly on its own. Measured on
`em regen -j 2` (258 threads):

| | state | CPU delta |
|---|---|---|
| running | `Sl` | — |
| after `SIGTSTP` | `Tl` (all 258 threads `T`) | 0 ticks over 4s |
| after `SIGCONT` | `Sl` | +602 ticks over 3s |

**What was actually wrong:** `run_phase` built its invocation with the phase's
stdout/stderr redirected (log, pty, or `tee` fallback) but never touched fd 0,
so every phase inherited whatever stdin `em` was started with — in an
interactive run, the user's terminal. `portage-repo/src/build/commands/mod.rs`
already said so outright: "A non-redirected stdin (the real terminal) is
inherited."

That matters because brush's `read` and `mapfile` builtins call
`AutoModeGuard::new(fd0)` and set `line_input(false)` with `echo_input`
following `-s` — they put **whatever fd 0 names** into non-canonical mode with
echo off, restored only when the guard drops. An ebuild or eclass calling
`read` therefore reconfigured the user's real terminal, and anything that
killed `em` before that guard dropped left the console in that state.

Fixed by redirecting all four invocation shapes from `/dev/null`, which is
what portage does outside `FEATURES=interactive`. Regression test:
`build::shell::tests::phase_stdin_is_dev_null` — it swaps its own fd 0 first,
because the test harness already hands tests `/dev/null` and the assertion
would otherwise hold with or without the fix (it did, on the first attempt).

**Still unconfirmed:** whether this is the whole of the reported Ctrl+Z wedge.
The mechanism is real and the fix is right on its own merits, but the symptom
was never reproduced in-session — it needs a real merge in a real terminal.
If it recurs after this fix, capture, from a *second* terminal:

```sh
stty -a < /dev/tty            # is the console left non-canonical / -echo?
ps -o pid,pgid,sid,stat,comm -g $(ps -o pgid= -p $(pgrep -x em) | tr -d ' ')
grep -E '^Sig(Blk|Ign|Cgt)' /proc/$(pgrep -x em)/status
```

A `stat` of `T` for `em` with a compiler still `R` would point back at process
groups after all; `-icanon`/`-echo` in `stty` would mean another terminal-mode
guard leaked somewhere.

## 2. Interruptible VDB write — FIXED 2026-09-02

`Vdb::register` created the final `<cat>/<pf>/` and then wrote `EAPI`,
`CATEGORY`, `SLOT`, `USE`, … one `std::fs::write` at a time, in place, so an
interrupt between the first and last field left a directory every later read
treats as a real installed package with fields missing. The merge critical
section (`lock_merge_flock` / `merge_gate`) serialises concurrent `em`
processes but did nothing about interruption.

Fields now land in `<cat>/-MERGING-<pf>` and the directory is renamed into
place once complete, reusing the rename-away/rename-in/remove-old dance
`portage_repo`'s regen cache already uses (`cache.rs`'s `swap_dir_target`) —
including its handling of stale scratch dirs from a previous crash.

Not atomic, and the code says so: `rename` cannot replace a non-empty
directory (`ENOTEMPTY`), so a rebuild of the same cpv still has a window
between the two renames where `pkg_dir` does not exist. At every other
instant it holds the complete old entry or the complete new one, never a mix.

Nor is it *durable*: nothing `fsync`s the entry or its parent directory, so
this protects against an interrupted process, not against power loss. That
is the same limitation `tempfile::NamedTempFile::persist` carries and
documents (tempfile #110/#111) — worth knowing before anyone reads
"rename into place" as crash-proof.

The three questions this note previously listed as open are answered:

- Both VDB scanners reject the name. `Category::packages` and
  `find_slot_occupant` each run the directory name through `Pf::parse`/
  `parse_cpv` and skip on failure, and `-MERGING-<pn>` is not a legal PMS
  package name. Tested directly rather than assumed —
  `an_unpublished_merging_entry_is_not_an_installed_package` and
  `a_merging_entry_is_never_a_slot_occupant`. The second matters most: a
  false occupant match would have made the merge unmerge a real package.
- A stale `-MERGING-` dir is cleared before reuse, so fields from an earlier
  crashed merge cannot survive into the new entry.
- `republishing_replaces_the_previous_entry_and_leaves_no_scratch_dirs`
  covers the rebuild path and asserts no `-MERGING-`/`-REPLACING-` dirs are
  left behind.

**Duplication to settle:** the dance now exists twice — here and in
`portage_repo::cache::swap_dir_target`. `portage-repo` depends on
`portage-vdb`, so it could be shared one-directionally, but a generic
filesystem helper is a poor fit for a crate whose job is the installed
package database, and the two differ in their scratch-dir naming
(`-MERGING-` is a portage interface convention, `.regen-old` is internal).
Left duplicated deliberately; a third caller should force the extraction.

**Not yet live-verified:** no real merge has run through this. The unit tests
cover both scanners and the rebuild path, but a `-p`-then-real merge on a
throwaway `--root` is the confirmation this still wants.

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
