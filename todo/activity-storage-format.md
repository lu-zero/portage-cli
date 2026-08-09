# Activity storage format — is JSON/JSONL good enough long-term?

STATUS: 🔵 not started — raised 2026-08-09 after fixing the `em regen` O(N²)
`LiveFsSink` hang (see `todo/for-sonnet.md` 2026-08-09, [[activity-status]]).
That fix removed the *quadratic* cost; this note is about whether the
*linear* cost of the current formats is itself going to bite next.

## Where it's used today

- `live/<job_id>/{session.json,progress.json}` — small, write-mostly,
  rewritten via `write_atomic` (tempfile + rename). Fine at this size;
  not the concern here.
- `history/merges.jsonl` — append-only, one line per finished package
  action, **never rotates**. `DurationStore::load` parses the whole file
  on every `em log list`/`time`/`predict` and every `-p --eta`. A
  long-lived host (or a `--jobs`-heavy CI box) accumulates this forever;
  `em regen` deliberately skips it (`HistorySink`'s `ActivityMode::Regen`
  guard), but ordinary merges do not, so it's still an unbounded
  full-file-parse-per-query on the hot ETA path. Flagged by the Opus
  review during the regen-hang investigation, explicitly out of scope
  for that fix.

## Questions worth answering before touching it

- Does `merges.jsonl` actually grow large enough in practice to matter
  (measure on a real long-lived host), or is this premature?
- If it does: rotate-by-size/date (`merges-2026-08.jsonl` etc., already
  sketched as "optional later" in `todo/activity-status.md`) is the
  cheap fix and keeps the JSONL format. A tail-read (last N lines
  without a full parse) would cover `em log list`/`time` but not
  `median_seconds`/`global_median_seconds`, which need a real window of
  history, not just the tail.
- Is JSON/JSONL the wrong tool for `merges.jsonl` specifically (e.g. an
  embedded index — sqlite, or a small binary log with a separate
  offset index) once it needs range queries by `cpn`/`job_id`, or is
  "rotate + still parse each shard fully" enough for the query patterns
  `em log` actually has? No decision made — needs real data first.
- `live/*.json` (session/progress/inflight) are small and write-mostly;
  no evidence they need anything other than what they have now. Don't
  conflate them with the `merges.jsonl` growth question.

## Non-goal for now

Do not speculatively swap formats without a measured problem — this is
a "watch it" note, not a design decision.
