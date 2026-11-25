# Weave em's activity into crossdev-stages (uniform quiet/fancy output)

Status: plan, 2026-11-25. Decisions (Luca): a pipe, reuse em's candy bars, then improve them.

## Facts

- em already emits JSONL activity events (`--activity-fd N`, `--activity-jsonl PATH`) on emerge,
  crossdev, toolchain, stages and setup: session start/end, package start/end with build-log path
  and failed phase, phase enter/leave, plan failures, diagnostics. `FEATURES=candy`
  (`activity/candy.rs`) draws indicatif bars from those same events.
- crossdev-stages runs em through hakoniwa with stdio inherited, so em's text and its own tracing
  lines interleave. hakoniwa 1.8 closes every fd >= 3 in the child (`close_extra_fds_exclude`), so
  an inherited pipe fd cannot reach em.
- Each crossdev step is its own session with no shared id, and the step headers (`[8/12] compiler-rt`,
  "done by an earlier run") are stdout text, not events.

## Channel: a FIFO in the shared log dir

The sandbox's `/var/log` is the host log dir bind-mounted. crossdev-stages `mkfifo`s
`<log_dir>/em-activity.fifo`, opens it O_RDWR (so em closing its end between steps is not EOF), and
passes `--activity-jsonl /var/log/em-activity.fifo`. A reader thread decodes lines into events until
the em process exits. No em change is needed for the channel itself.

## em side

1. `StepStart` / `StepEnd` / `StepSkipped` events (label, index, of, level), from the crossdev,
   toolchain and stages plan loops.
2. One run id: every step's merge session gets it as `parent_job_id` (the `activity_session` hook in
   `EmergeOpts` exists; the crossdev loop passes the default today).
3. `-q` with an activity output keeps the terminal to failures only (check what it prints today).

## No shared crate (Luca: "we can reimplement them")

crossdev-stages decodes the JSONL itself (its own serde types for the events it reads, skipping
unknown ones, per `docs/user/activity.md`) and draws its own indicatif bars, modelled on candy.

## crossdev-stages side

1. One helper for every em call: adds the FIFO flag; in quiet/fancy also `-q` and em's stdout/stderr
   to `<log_dir>/em-<run>.log`.
2. Renderer modes: `plain` (today; default off a TTY), `quiet` (a line per pipeline phase and em step;
   on failure the package, phase, build-log tail and host log path), `fancy` (candy, plus a pipeline bar
   above it).
3. Its own tracing goes through the bars' `println` so lines do not tear them.
4. Portage backend: no events, so quiet and the pipeline bar only.

## Candy improvements to look at

- A step row (`[8/12] compiler-rt`) and the pipeline row above the per-package bars.
- Skipped steps collapse into one line; finished packages leave a one-line summary with their time.
- A failure panel: failed phase, last lines of the build log, its path.
- ETA per step from the duration history, not only per plan.

## Order

1. em: step events and the shared run id, with tests on the event stream. ✅ `step_start`/`step_skipped`/
   `step_end` from `run_staged`, step sessions parented to the run id.
2. crossdev-stages: FIFO reader, quiet mode, then fancy with its own bars. ✅ uncommitted there
   (`src/activity.rs`, `--output plain|quiet|fancy`, `SandboxRunner::run_em`); live in llvm1 2026-11-25.
3. Candy improvements (both sides), live in a sandbox build.
4. Open: the `[n/N]` step headers are stdout text that ignores `-q`; harmless once em's stdout goes to a log.
