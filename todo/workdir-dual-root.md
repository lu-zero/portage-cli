# Workdir isolation for dual-root / dual-plan builds

Status: 🟡 near-term landed on master (2026-08-06) — per-root builddirs +
builddir flock + parallel schedule barrier; multi-`em` plan awareness still
future  
Live proof: [[clang-crossbuild-prefix-local-test-plan]] findings **#3 / #4**  
Portage reference: `_emerge/Scheduler._prevent_builddir_collisions`,
`EbuildBuildDir` (host Portage as of 2026-08)

---

## Problem

Under `--prefix` + `--target`, the plan can contain **two merges of the same
CPV** (host-arch + target-arch — dual plan entries). `default_work_base()`
keys the work directory only on outer prefix + `category/pf`, so both share
one `WORKDIR` / `build.log` / `temp/`.

With high `--jobs`, they run concurrently → race (Sonnet 2026-08-06:
`clang-*-config` packages, every phase logged twice, `newins` fail; goal
package never built). Deterministic at `--jobs 80` on that graph.

Portage uses the **same** builddir key (`$PORTAGE_TMPDIR/portage/$CATEGORY/$PF`)
but:

1. **Serializes** same-CPV merges to different `$ROOT`s via digraph edges  
   (`_prevent_builddir_collisions` — “sometimes the same exact cpv needs to
   be merged to both $ROOTs”).
2. **Locks** the builddir for the duration of a merge (`EbuildBuildDir`).

bash-crossdev mostly **avoids** the case (host tools under `cross-*`
category; target pkgs via separate emerge + `PORTAGE_TMPDIR` under sysroot).

---

## Near-term fix (do this)

### 1. Per-target (per install-root) builddirs

Include enough of the install identity in the work path so host-root and
target-root (sysroot) merges of the same CPV **do not share** a directory.

Sketch (exact encoding TBD):

```text
$work_base / <root-key> / <category> / <pf>
```

Where `root-key` is derived from merge root / EROOT / sysroot path (stable,
short hash or sanitized path). Outer-prefix-only base stays for
distdir/cache if needed; **build tree** is per install root.

Also covers same-CPV to two different cross sysroots under one outer prefix
if that ever appears in one process.

### 2. Lock + schedule like Portage

Even with (1), keep Portage’s two layers:

| Layer | Behaviour |
|-------|-----------|
| **Schedule** | Same builddir key (or same CPV+root collision class) must not run concurrent merges in **this** process — digraph / job-queue barrier equivalent to `_prevent_builddir_collisions` |
| **Lock** | flock (or equivalent) on the workdir for the whole merge — survives accidental dual scheduling and **helps two concurrent `em` processes** that happen to pick the same path |

Locks alone: two `em`s can still thrash (one waits); better than corrupt
WORKDIR. Scheduling: clean single-process dual-root plans.

**Note:** Portage comment says main-process lock holding makes file locks
insufficient *within one emerge* for dual-ROOT same-CPV — hence digraph
edges. em should implement **both** schedule barrier and lock.

### Exit criteria (near-term)

- Dual plan entries for `llvm-core/clang` / config packages under
  `--prefix --target` do not share a workdir.
- High `--jobs` run does not double-log phases into one `build.log`.
- Unit or integration test: two merges same CPV different merge roots get
  different work paths; scheduler refuses concurrent same-path merges.

---

## Future (interesting, not near-term): multi-`em` plan awareness

A **package lock** helps if two concurrent `em` processes build the same
work path (second waits). That is not optimal for a chain like:

```text
bar[a] → baz → bar[b]   # same CPV, two roles/roots, or two sessions
```

Ideas for a later design note / feature:

1. **Shared activity / plan registry** (XDG state, unix socket, or extend
   existing activity bus): each `em` publishes “building CPV @ root-key”
   and critical-path set.
2. **Second `em` on overlap:**
   - **Pause** until the other process finishes the contested package / path, or
   - **Error out** with a clear message if plans would corrupt each other
     (stricter CI mode).
3. **Critical-path handshake:** if session A holds `bar[a]` and session B
   needs `bar[b]` with a different root-key, per-target builddirs already
   allow parallel; if same path, wait or fail.

Out of scope until near-term isolation lands. Track as polish / multi-session
hardening, not a blocker for single-process dual-root.

---

## Non-goals (this todo)

- Fixing *why* dual plan entries exist (visibility / dual-root model) — may
  shrink later; isolation must hold even if dual entries remain.
- Dropping BuildClass / package.provided bootstrap — separate todos.
- Matching Portage by *only* serializing without per-root dirs — we prefer
  **per-target dirs +** lock/schedule so high jobs can still parallel
  different roots.

---

## Implementation sketch

1. Change `default_work_base` / work_dir construction to take merge root
   (or `PlannedMerge` identity); thread from merge loop.
2. Add workdir flock around `run_inner` / phase groups (mirror
   `EbuildBuildDir`; may reuse patterns from `work_base/.merge.lock`).
3. Scheduler / job pool: before starting a job, ensure no in-flight job
   shares the same workdir key; if dual CPV same key still possible, edge
   them.
4. Tests + re-run clang Scenario A step 3 at high jobs.

---

## References

- Portage: `doebuild.py` `PORTAGE_BUILDDIR`, `_emerge/Scheduler.py`
  `_prevent_builddir_collisions`, `_emerge/EbuildBuildDir.py`
- em: `portage-cli/src/ebuild.rs` `default_work_base`
- Live: [[clang-crossbuild-prefix-local-test-plan]] #3, #4
- Discussion: dual CPV only (not foo RDEPEND bar); multi-em future above
