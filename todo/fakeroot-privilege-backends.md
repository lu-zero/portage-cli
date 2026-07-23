# Unprivileged builds: consolidate the chown workarounds behind a privilege backend

STATUS: **v1.1 landed (2026-06-27)** — umbrella fakeroost session + merge chown +
facet-2 name resolution, all validated on `sys-apps/util-linux`; **hakoniwa backend
sketched** (opt-in `--privilege hakoniwa`, not yet wall-tested). **2026-06-28:
fakeroost issue #7 fix on PR #8** (`user-notif-hybrid` branch) — stat routed
through a seccomp `USER_NOTIF` thread pool lifts the supervisor ceiling ~2.7×
(~100k→~290k stat/s, 3.5×→4.7× parallelism) and now *beats* upstream `fakeroot`
under concurrency (which goes backwards, 0.68×). Confirms the scoping decision:
fake-root wraps only `src_install`/archive, not the compile (see Q6). **Binpkg
producer done (2026-06-28): `em -b` GPKG writer + `read_metadata` reader + `em
maint binhost` `Packages` index all landed and validated — see
[[em-stages-and-binhosts]] / PENDING.md (Binhosts).** **Per-package `__worker`
landed (2026-07-01)**: fakeroost/sudo are now scoped, not umbrellas — the
parent runs `pretend..compile` un-wrapped, then spawns a wrapped `em __worker`
for install+qmerge(+binpkg) per package (the Q6 scoping). Cross-phase shell
state crosses the process boundary via a variables-only `worker-env` dump
(`declare -p`, readonly filtered; `declare -f` deliberately omitted — brush's
function printer doesn't round-trip heredocs — the worker re-sources the
ebuild and `mark_phase_sourced` suppresses the phase loop's re-source). The
dump exposed a brush `$'...'` parser bug (a literal `"` swallowed the closing
quote) — fixed in the fork, `6038e073`. qmerge serialises across worker
processes on a `work_base/.merge.lock` flock (Q2 as designed). hakoniwa keeps
the umbrella (no per-syscall tax; container binds must span the run), as does
`em ebuild … install/qmerge` (no worker seam). Surfaced by the split, not yet
addressed: (a) `pkg_setup` now runs *unprivileged* in the compile parent —
portage runs it privileged; an ebuild that needs root-ish checks there will
diverge; (b) the VDB `environment.bz2` still embeds brush's `declare -f`
output, whose heredoc bodies don't re-source (fine for em, a compat gap for
consumers that re-source it — see [[parser-audit]]). **Scoping confirmed live
(2026-07-02)** with a uid/chown probe ebuild across every backend — and the
probe caught the worker wrap being a silent no-op: `fakeroot()` returns the
supervisor command and the return was discarded, so fakeroost degraded to
`none` for the whole install group (fixed `f3201cb`; `fakeroot()` is now
`#[must_use]` in both forks). Verified matrix: compile parent uid=1000
un-wrapped everywhere; install worker fake-uid 0 with `chown 123:456`
recorded through to the gpkg image tar (fakeroost + pseudoroot), real root +
real ownership under sudo, mapped-root umbrella under hakoniwa, single
process under `none`. **pseudoroot backend added (2026-07-02, `37e8d49`)**:
`--privilege pseudoroot` = LD_PRELOAD fake root (lu-zero/pseudoroot, same
`FakerootCommandExt` API), scoped exactly like fakeroost, no ptrace tax;
static binaries / raw syscalls escape it. Integration surfaced two
pseudoroot bugs — `run_session` leaked `__PSEUDOROOT_SUPERVISE` into the
child (env overrides don't remove inherited vars; fatal for
self-referential targets like `em __worker`, which re-enters supervision on
its own argv → ENOENT) and `ensure_default_env` clobbered inherited
`PSEUDOROOT_UID/GID` with the 0 defaults. Both shipped fixed in
**pseudoroot v0.2.0**; the workspace pins the tag (`c6b0ae9`) and the
backend runs from the plain git dependency (embedded interposer), matrix
re-verified. A third gap surfaced on the util-linux sweep (2026-07-03):
the interposer missed the LFS `stat64` family, so any
`_FILE_OFFSET_BITS=64` binary read *real* ownership while the chown hooks
stayed live — bzip2 preserves ownership across compression, so all 189
compressed doc/man files in the binpkg recorded the build user. Fixed in
pseudoroot `f3997ea` (LFS aliases; 0/588 leaks after, setuid mount 0/0;
fakeroost verified immune — ptrace intercepts syscalls, not symbols),
released as **v0.2.1**: the workspace pins the tag (`5acb4ce`) and the
backend runs from the plain git dependency again, no path patch. The ebuild
shell's clean env also stripped `LD_PRELOAD`/`PSEUDOROOT_*` from phase
children — now passed through exported when a session is active (portage
does the same for its sandbox preload). Goal: a correct
root-owned `@system` stage3 (setuid `mount`, `root:root`, file caps) without
running em as root. Supersedes the "decision point" in
[[stage-build-shakeout]] and the privilege half of [[build-clean-env]].

## Implemented — v1 (umbrella session)

Shipped the simplest correct slice (model B below), all in `main()` — no scheduler
or flock changes, since the whole build+merge stays in one process:

- `privilege.rs` — `Backend{RealRoot,Fakeroost,Hakoniwa,Sudo}` + `detect()` (RealRoot
  when euid==0 or already inside a session; else map the request, default Fakeroost)
  + `maybe_supervise()`. Selected by the global `--privilege
  <auto|fakeroost|hakoniwa|sudo|none>` flag (clap, env `EM_PRIVILEGE`, so it shows
  in `--help`). `sudo` re-execs `sudo -E em …` for **real** root (root-owned tree +
  real setuid, catalyst-style), opt-in only / never auto-selected; `none` disables
  wrapping.
- `main()` calls `fakeroost::init()` first (before the tokio runtime), then for an
  unprivileged *building* invocation (`will_build`: emerge merge path +
  `ebuild`/`crossdev`/`toolchain`, not `--pretend`) re-execs em once under
  fakeroost (`EM_PRIVILEGE_ACTIVE` guards against re-supervising). The whole run —
  resolve, all builds, all merges — shares one ptrace+seccomp ownership table.
- `ebuild.rs` merge now `lchown`s each merged path to its image owner
  (`preserve_owner`) — a real gap even for **root** installs before this.
- The three EPERM workarounds are **kept** but now **inert under fakeroost**: each
  guards on `getuid`/`geteuid`/`EUID`, which fakeroost fakes to 0, so they take the
  privileged branch (real/faked chown, `0:0` default). They remain as graceful
  degradation when fakeroost is unavailable; remove once a real `@system` run
  confirms supervision is universal.
- Verified: `fakeroost` works on this kernel (`chown 0:0` unprivileged → `stat`
  reports `0:0`); em re-execs only on build paths, transparently.
- **Validated on the real wall (2026-06-27)**: `em --root /var/tmp/stage1-base
  sys-apps/util-linux` unprivileged — the package's own Makefile
  `chown root:root .../bin/mount` (the install-exec-hook that previously killed
  the build) now runs faked, util-linux merges (in VDB), and `mount` lands setuid
  (`-rwsr-xr-x`). On-disk owner is the build user (live unprivileged merge keeps
  it; the faked root owner is session-only — real `root:root` needs the in-session
  tar). This clears the wall that blocked `sys-apps/portage` and the self-extending
  `@system` base.

Deltas from the design: umbrella session instead of the per-package `__worker`
(deferred optimisation — keeps the resolver out of ptrace, enables independent
parallel sessions); RealRoot+Fakeroost+Sudo backends done in v1; **hakoniwa landed
as an opt-in umbrella sketch in v1.1** (fakeroot system binary + auto-detect chain
still behind the seam). Facet 2 (target-passwd name resolution) is done (`907d914`).

## Implemented — v1.1 (hakoniwa umbrella sketch)

- Workspace dep `hakoniwa = "1.7.1"` (crates.io release; no git patch).
- `Backend::Hakoniwa` + `--privilege hakoniwa` / `EM_PRIVILEGE=hakoniwa` (opt-in;
  `auto` still maps to fakeroost).
- `reexec_hakoniwa(cli)`: `hakoniwa::Container::new()` with `uidmap(0)`/`gidmap(0)`
  (build-user→ns root), `rootfs("/")` for RO FHS prefixes, then `bindmount_rw` for
  the merge root, config overlay, `/tmp`, `/var/tmp`, and prefix-local cache/tmp when
  `--local`/`--prefix` relocate distfiles.
- Preflight `userns_available()`: `unprivileged_userns_clone` knob + `newuidmap`/
  `newgidmap` on `PATH`.
- Inner em runs with `EM_PRIVILEGE_ACTIVE=hakoniwa`, `getuid()`→0 inside the box —
  real `chown`/`setuid` syscalls (real-in-a-box family), no `fakeroost::init()` loop.
- **Not yet validated** on the util-linux wall; bind-mount coverage may need more
  paths (`/var/cache/portage`, host distdirs, cwd) once exercised.
- **2026-06-28: WORKING.** `em --privilege hakoniwa toolchain --setup` builds and
  merges sys-apps/baselayout into the ROOT. Four things were needed: the
  lu-zero/hakoniwa `.oldproc` rmdir fix (below), `Runctl::RootdirRW` (else the
  whole root is remounted RO and the rw build binds become RO), subuid/subgid
  *range* maps (`uidmaps`/`gidmaps`, not a lone `uid→0`), and targeted binds for
  what `rootfs("/")` omits (/var/db/repos RO, /var/cache/distfiles RW, the em
  binary) — build scratch lives in the writable ephemeral tmpfs root, so no $HOME
  bind. em commit `0384088`, hakoniwa fork `5f77bb1`. Remaining: the
  fakeroost/hakoniwa/sudo benchmark (Q6), now unblocked.

  *(Historic — the bug that blocked it.)* `em --privilege hakoniwa` re-execs,
  prints the banner, then dies before any build with:
  `hakoniwa: rmdir("/.oldproc-<uuid>") => Device or resource busy (os error 16)`.
  Root cause is in hakoniwa 1.7.1 `runc/unshare.rs`: to swap in a private procfs it
  binds the host `/proc` to `.oldproc-<uuid>`, then (lines 314-315) does a **lazy**
  `umount2(MNT_DETACH)` immediately followed by `rmdir`. With grok's `rootfs("/")`
  (the whole host root → a *recursive* `/proc` bind carrying every submount), the
  detached unmount hasn't settled when the rmdir runs → EBUSY, and the container
  aborts. Not a bind-coverage gap; the proc-remount teardown races. Fix options:
  (a) fork hakoniwa to make the `.oldproc` rmdir non-fatal / retry (1-line, mirrors
  the [[fakeroost-fork]] pattern — after MNT_DETACH the empty dir is harmless);
  (b) keep the host PID ns: `container.share(Namespace::Pid)` **and** drop the
  default `procfsmount` (guarded by `MountProcfsEPERM`), so no oldproc dance at all;
  (c) upstream a fix. Left for grok (owns this backend) per the 2026-06-27 steer.

---
Original design (the target end-state):

## The problem (one root cause, three patches)

Unprivileged builds cannot `chown` to root/foreign users. Today this is swallowed
in three places, each of which **discards** the intended ownership instead of
recording it — so the merged tree / binpkg / stage carries wrong ids, no setuid,
no file capabilities:

1. `build/stubs.rs` — bash `chown()`/`chgrp()` overrides return success on EPERM
   when non-root. Only catches chowns run *directly in ebuild bash*.
2. `build/commands/install.rs` `FownersCommand` — `fowners` shells to `chown`,
   swallows EPERM when non-root (`efdeb37`).
3. `build/commands/inst_owner.rs` — `PORTAGE_INST_UID/GID` default to the process
   uid in unprivileged mode so `install -o <self>` succeeds.

The deepest case escapes all three: util-linux's *own* Makefile
`chown root:root .../bin/mount` is a child-process chown, not interceptable by a
bash function. A real fake-root layer is required.

## What portage does (confirmed, portage-3.0.79)

- `FEATURES=fakeroot`, and only when `uid != 0` **and** `fakeroot_capable`
  (`/usr/bin/fakeroot` exists+executable) — `config.py:1492`, `doebuild.py:2098`.
- `process.spawn_fakeroot` (`process.py:172`) runs:
  `fakeroot -s ${T}/fakeroot.state -i ${T}/fakeroot.state -- bash -c <cmd>`.
- The **`-s`/`-i` state file is the crux**: portage spawns each phase as a
  *separate* `fakeroot` process, so the faked-ownership table must be **saved
  after install and re-loaded for qmerge/misc-functions** to carry ownership
  across the phase boundary (`MiscFunctionsProcess.py:47`, `EbuildPhase.py:124`).
- Applied to the install + merge phases only (not compile); orthogonal to the
  sandbox (`free`/`sesandbox`/`droppriv`) and `userpriv`.
- **Opt-in**: not in default FEATURES — a root merge does real chowns; fakeroot is
  the unprivileged / `ebuild` / binpkg path. em's directive is more aggressive:
  **auto-enable whenever unprivileged** (a deliberate divergence, noted below).

So portage = the external libfakeroot binary + a per-build state file. Backend
"fakeroot (system)" below mirrors it exactly; the others improve on it.

## fakeroost (confirmed from source: koca-build/fakeroost 0.1.1)

A **ptrace + seccomp** supervisor, pure Rust — *not* an LD_PRELOAD shim (so it
fakes ownership even for static binaries and under Docker's default seccomp).

- API: `fakeroost::init()` as the **first line of `main`** — detects the
  `__FAKEROOST_SUPERVISE` re-exec marker; a normal launch returns immediately, the
  supervisor launch runs the trace loop and exits with the child's status.
  `FakerootCommandExt::fakeroot()` on `std::process::Command` rewrites it to
  re-exec **our own binary** (`/proc/self/exe`) in supervisor mode, which then
  forks+traces the real target.
- **Whole-tree coverage**: the supervisor sets
  `PTRACE_O_TRACEFORK|TRACEVFORK|TRACECLONE`, so every descendant is auto-traced
  and shares **one** `OwnershipTable`. → covers em's **in-process** merge *and*
  every child (`make`/`install`/`chown`/eclass `chown 0:0`) in one session.
- Faked syscalls: `chown/lchown/fchown/fchownat` (record, skip real),
  `stat/lstat/fstat/newfstatat/statx` (overlay faked uid/gid/mode/rdev/nlink),
  `getuid/geteuid/...`→0, `setuid/.../capset`→success, `mknod/mknodat`
  (placeholder + record), `*xattr` (security.capability + ACLs), `chmod` (record +
  real). `unlink`/`rename` evict the table entry.
- Keyed by **(dev, inode)** — survives hardlinks/renames. Untracked ⇒ "owned by
  root" (real mode/rdev/nlink preserved).
- **No state save/load across processes** (no `-s/-i` equivalent): the table lives
  for the supervised run only.

## Why em is a *better* fit than portage's split model

em runs install **and** qmerge in **one** process — one carried build shell,
`build_and_merge` → `run_inner` over phases `[…, install, qmerge]` (`ebuild.rs:136`,
[[build-clean-env]]). So a single supervised worker holds install+qmerge in **one
in-memory table** — em needs **no `fakeroot.state` file** that portage requires
only because it spawns phases separately. The one requirement: the qmerge copy
(em's in-process `std::fs` at `ebuild.rs:1285`) must run *inside* the supervised
worker so the faked image-owner is visible and the chown into ROOT is recorded.

## `__worker`: the single entry point

`ebuild::build_and_merge` (`ebuild.rs:136`) is already the per-package
build+merge unit, and every argument is serializable except one:

| arg | worker flag |
|-----|-------------|
| `ebuild_path` | `--ebuild <path>` |
| `use_flags: &[Interned]` | `--use "a b c"` |
| `work_base` / `root` / `distdir` / `config_root` / `sysroot` / `eprefix` | matching flags |
| `quiet` | `--quiet` |
| `merge_gate: &Mutex` | **cross-process** flock on `work_base/.merge.lock`, held around qmerge |

Introduce a hidden `em __worker …` subcommand (mirroring the existing `em __helper`
precedent at `cli.rs:415`) whose body is one `build_and_merge` call. The dispatch
in `merge_sequential` (`main.rs:447`) and `merge_parallel` (`main.rs:583`) changes
from *call `build_and_merge` in-process* to *build the `em __worker` `Command`, let
the backend decorate it, spawn, await*. The `--jobs` scheduler (`Scheduler`,
`main.rs:483`) is untouched — it already awaits child build subprocesses.

`em`'s `main` gains `fakeroost::init()` as its first statement (a no-op unless this
exe was re-exec'd as the supervisor); the tokio `merge_gate` Mutex becomes an
flock so qmerge stays globally serial across worker *processes*.

## The `PrivilegeBackend` trait — the one seam

```
trait PrivilegeBackend {
    /// Spawnable command for one `em __worker` unit, wrapped in whatever
    /// provides root (fake or real) for the whole worker process tree.
    fn worker_command(&self, em_exe: &Path, args: &WorkerArgs) -> Command;
}
```

`detect()`: `euid==0` ⇒ `RealRoot`; else the configured backend, default **auto =
best available**. All backends converge on how `em __worker` is launched:

| backend | launch | family |
|---|---|---|
| **RealRoot** (root / `--jobs` in-proc) | `em __worker` (or keep in-process); real chowns | — |
| **fakeroost** *(default unpriv.)* | `Command::new(em).arg("__worker").fakeroot()` + `init()` in `main` | fake+acct |
| **fakeroot** (system) | `fakeroot -s/-i <state> -- em __worker` (portage's exact recipe) | fake+acct |
| **sudo** | `sudo em __worker` — real root, real setuid | real-in-box |
| **hakoniwa** *(v1.1: umbrella sketch)* | `Container::new().uidmap(0).rootfs("/")` + rw binds → `em …` (`hakoniwa` 1.7.1) | real-in-box |

"fake+accounting" (fakeroot/fakeroost) vs "real-in-a-box" (sudo/hakoniwa) are two
families behind the same `worker_command`. Auto-detect order when unprivileged:
fakeroost (pure-Rust, always linked) → fakeroot (binary on PATH) → hakoniwa
(userns available) → sudo (allowed) → degraded warn.

## The "real-in-a-box" family is NOT self-sufficient for packaging metadata (2026-06-28)

Confirmed once hakoniwa actually ran builds: a real userns box (hakoniwa) gets the
*build* right but **not** the full packaging metadata on its own. Two gaps, both
inherent to "real, just mapped":

1. **Device nodes.** hakoniwa drops `CAP_MKNOD` and 1.7.x exposes no API to keep it,
   so a build that `mknod`s a char/block device fails `EPERM` inside the box. (FIFOs
   and regular files are fine.) Only `sudo` (real root) can really create them.
   fakeroot/fakeroost *fake* them (regular file + recorded `rdev`/mode) with no priv.
2. **On-disk ownership is the *mapped* id, not 0:0.** With the subuid/subgid range
   maps, a `chown 0:0` inside the box lands as the **caller's** uid on disk, and
   `chown portage` lands as a **subuid** (100000+…). The files only *look* like
   `0:0`/`portage` from **inside** the box (the userns view). So a stage3/binpkg tar
   must run **inside** the container to record correct ownership; tar from the host
   sees the subuid ids. (`sudo` writes real `0:0` on disk; fakeroot fakes `0:0` in
   its accounting.)

**Implication — fakeroot likely belongs *on top*, for every backend, not as an
alternative to them.** The clean separation is:

- **session backend** (hakoniwa ≫ fakeroost for speed; sudo when real root is wanted)
  — fast namespace isolation + a working `/dev`,`/proc`, real parallel builds; and
- **a metadata layer** that makes ownership + device nodes correct *regardless* of
  what the host actually stored. fakeroost (or `tar`-inside-the-box for ownership
  only) is that layer.

So the realistic stack is **fakeroost *inside* hakoniwa**, but scoped: keep
fakeroost's ptrace cost **off the compile** and wrap only `src_install` + the
image/archive step — the phases where device nodes and ownership metadata are
actually produced. This scoping is right for two independent reasons, both now
measured (see Q6):

1. **Even with the issue #7 fix, fakeroost still carries a per-trapped-syscall
   tax** (the USER_NOTIF pool lifted the *ceiling* ~2.7× — 100k→290k stat/s,
   4.7× effective parallelism — but native is ~50M stat/s; the gap is still
   ~150×). Wrapping the whole `make -j` tree pays that on every header stat. A
   compile spends the vast majority of its syscalls in the build, not in install,
   so scoping fakeroost to `src_install` removes the tax from the hot path.

2. **The original `fakeroot` (LD_PRELOAD) is *worse* — it goes backwards under
   load** (59k stat/s at 1 worker collapsing to 40k at 128, 0.68× effective
   parallelism, because its state lives behind a global lock). fakeroost-with-the-
   fix is strictly better than upstream fakeroot under concurrency, but neither is
   free. So: don't wrap the build with *any* fake-root layer; wrap only the phase
   that needs it.

Ownership alone can instead be handled by archiving inside the box (no ptrace at
all); the residual that *requires* the fake layer is device nodes. **TODO**:
decide per-phase scoping (fakeroost only around `src_install`/`__stage-pack`),
and whether ownership goes via in-box `tar` (cheap) with fakeroost reserved for
`mknod` packages only. Ties into [[#future-tar--binpkg--stage-artifacts-none-exist-yet]] and Q1.

## What collapses once a backend records ownership

- `FownersCommand`: drop the EPERM-swallow → always real `chown` (faked + recorded);
  still resolve owner *name*→uid:gid against the **target** passwd/group (the second
  facet in [[stage-build-shakeout]]), then chown numerically.
- `stubs.rs` `chown`/`chgrp` overrides: **delete** — child chowns are faked for real.
- `inst_owner.rs`: back to portage's `0:0` default (the faker grants root).
- `ebuild.rs:1285` merge: **add** the missing chown — set each ROOT file's owner to
  its image-file owner (real when privileged, faked otherwise). This is a genuine
  gap even for **root** installs today: the copy never chowns, so non-root-owned
  files (`acct-user/*` dirs, etc.) land owned by whoever ran em. Ownership is **not**
  recorded in `CONTENTS` (it has no owner/mode field — like portage); it is captured
  at *archive* time instead — see Q1.

## Q1 RESOLVED — artifact ownership is captured at archive time, not stored

`CONTENTS` has **no** owner/mode field (confirmed: `portage_vdb::ContentsEntry` =
`{kind, path, md5, mtime, target}`, like portage), and fakeroost has **no**
cross-process state. So ownership cannot be reconstructed after a worker exits —
it must be read by the **archiver while it runs inside the fakeroost session** that
recorded the chowns. The resolution is therefore about *scoping the session to
cover the archiver*, and it splits by artifact:

- **Live unprivileged install** (`em --root <prefix>`): no artifact. fakeroost only
  stops the chown-EPERM death; on-disk files stay build-user owned (fine for a user
  prefix). Per-worker session suffices, nothing to preserve.
- **binpkg** (`em -b`): build the archive from the **image `${D}`** at end of
  `src_install`, *inside the same worker session* (the image already carries the
  faked chowns). Exactly portage's model (it packs the binpkg under fakeroot).
  → binpkg is the **canonical, durable carrier of ownership** (GPKG stores it in tar).
- **stage3** (`em stages`): do **not** tar a live unprivileged ROOT (its on-disk
  owners are build-user). Instead **assemble from binpkgs**: extract the
  already-correctly-owned binpkgs into a fresh ROOT and tar it, all under **one
  short umbrella fakeroost session** covering only extract+tar. Decouples the
  per-package builds (each a quick, parallel session) from the stage pack (one
  session over re-pack), and matches catalyst's "seed + packages → stage".

So no ownership store is added anywhere; the binpkg is the intermediate that holds
it, and every tar runs in-session. Detail in "Future: tar / binpkg" below.

## Open implementation questions

1. **Parallel workers each have their own table.** Independent is fine: each writes
   root:root into the shared ROOT; a later worker stat-ing those files gets the
   "untracked ⇒ root" default — correct for the common case. Only non-root-owned
   installed files (rare) could be misread cross-worker. Acceptable; revisit if it
   bites.
2. **Merge gate cross-process**: flock on `work_base/.merge.lock` vs a parent-held
   semaphore. flock is simplest and survives worker crashes. *(✅ 2026-07-01:
   flock landed — taken in `run_inner` around merge/qmerge alongside the
   in-process gate.)*
3. **Worker arg round-trip**: `WorkerArgs` must fully reconstruct `build_and_merge`
   input; confirm the worker re-derives FEATURES/EPREFIX from `--config-root`
   rather than the parent's in-memory state. *(✅ 2026-07-01: the worker
   rebuilds profile/package.env/FEATURES from `--config-root`; only the
   resolved USE and the root paths cross as flags. Cross-phase *shell* state
   crosses via the `worker-env` variables dump — `declare -f` deliberately
   excluded (brush's printer doesn't round-trip heredocs, see
   [[parser-audit]]), so functions defined dynamically *during a phase* —
   as opposed to by the re-sourced ebuild/eclasses — do not survive into the
   worker. Rare; revisit if it bites.)*
4. **RealRoot stays in-process** (no spawn) for speed; spawn only when faking.
5. **fakeroost robustness on the 128-core `@system` run**: ptrace adds a per-syscall
   trap on the filtered set — confirm it survives the heavy `make -j` trees.
   *(2026-06-27: it does — survives the toolchain + libc, after the bad-path
   passthrough fork fix [[fakeroost-fork]].)*
6. **Benchmark the backends — fakeroost vs hakoniwa vs sudo (real root).** The
   2026-06-27 native stage3 smoke showed the fakeroost toolchain (esp. the gcc
   3-stage bootstrap) running *noticeably* slower than a normal build. Expected:
   fakeroost is ptrace+seccomp, so every trapped syscall (stat/chown/chmod/mknod/
   xattr) costs two context switches (entry+exit stop) — and a gcc bootstrap is
   overwhelmingly `stat()`. hakoniwa (userns, in `Backend::Hakoniwa` already) has
   ~zero per-syscall overhead; sudo (real root) has none. Measure wall-time of the
   *same* target (e.g. `em toolchain --setup` into a fresh ROOT, or a fixed
   `@system` slice) under `--privilege fakeroost` / `hakoniwa` / `sudo`, same
   `MAKEOPTS`/box. If hakoniwa is close to sudo and far faster than fakeroost, it
   should likely become the default unprivileged backend (fakeroost staying as the
   no-userns fallback, e.g. restricted containers). Capture numbers here.
   *Early numbers (2026-06-27, 128-core arm64, `em toolchain --setup`, `-j80`,
   targets under `~/.cache/em-testing/`):*
   - **fakeroost** (pre-fix): killed at **131 min**, still in the gcc-16 bootstrap
     (never finished). Single `cc1plus` at a time, load ~4 on 128 cores — the
     single-threaded ptrace supervisor serialized every traced `stat()` from the
     parallel make. (Upstream perf issue: koca-build/fakeroost#7.)
   - **sudo** (real root): **completed in 21:43** (`/usr/bin/time -v`, exit 0, 23
     pkgs, max RSS 2.26 GB), load ~13 during the gcc bootstrap (real parallelism).
     ≥6× faster than pre-fix fakeroost, which never finished.
   - **hakoniwa**: backend now works (v1.1 fixed). A first toolchain benchmark run
     (2026-06-28) surfaced a *separate* regression — the cwd anchor (`b23ab2f`)
     pointed the process cwd at WORKDIR, which the post-merge cleanup deletes, so
     step 2 died `failed to start ebuild shell: ENOENT`. Fixed (`5248e0d`: anchor to
     work_root); hakoniwa toolchain now proceeds 1→2→building. **Benchmark TODO**:
     re-run `em --privilege hakoniwa toolchain --setup` to completion for the
     wall-time vs sudo (21:43) — expected ≈ sudo (userns, ~no per-syscall cost).

   *Synthetic stat benchmark (2026-06-28, 128-core arm64, `bench/run.sh` —
   `stat-loop` over 512 distinct files, 20k calls/worker, fakeroost at the
   USER_NOTIF-pool default of 3 servants):*

   | workers | native | fakeroost (#8 fix) | fakeroot (system) |
   |---:|---:|---:|---:|
   | 1 | 1.65 M | 56 K | 59 K |
   | 8 | 7.85 M | 279 K | 44 K |
   | 16 | 14.8 M | 278 K | 41 K |
   | 128 | 48.7 M | 259 K | 40 K |
   | **eff. parallelism** | 29.6× | **4.64×** | **0.68×** |

   Two takeaways:
   - **fakeroost #8 lifts the ceiling ~2.7× over main** (was ~100 K flat → ~280 K,
     parallelism 3.5×→4.6×) by routing stat through a seccomp `USER_NOTIF` thread
     pool instead of the single ptrace loop. So "impractical, never finished" is no
     longer the whole story — but it's still ~150× behind native on raw stat
     throughput, and a gcc bootstrap is stat-dominated. So **wrapping the whole
     build remains the wrong move**; the scoping in the design (fakeroost around
     `src_install`/archive only, see above) is right.
   - **The system `fakeroot` (LD_PRELOAD) is *worse* than fakeroost under load**:
     it goes *backwards* (0.68× effective parallelism — its faked-ownership state
     is behind a global lock). So if a fake-root layer is needed for an
     unprivileged build, fakeroost is the better choice, not upstream fakeroot.

   **Updated conclusion**: fakeroost is correctness-good and, with #8, no longer
   catastrophically slow — but it is still a per-syscall tax that doesn't belong on
   the compile. The plan stands: **hakoniwa (or sudo) as the build session;
   fakeroost scoped to `src_install` + archive only** for the ownership/device-node
   metadata. Finish the hakoniwa wall-time number to confirm it's ≈ sudo and should
   be the default unprivileged backend; fakeroost stays the no-userns fallback and
   the metadata layer. **Re-run the gcc wall-time under fakeroost-#8** to get a
   real (non-synthetic) post-fix number — expect materially better than the 131-min
   kill but still well behind sudo.

## tar / binpkg / stage artifacts

**Producer done (2026-06-28).** `em -b`/`--buildpkg` writes GPKGs via
`portage_binpkg::write_gpkg`, `read_metadata` reads them back, and `em maint
binhost` builds the `Packages` index — all validated against host portage. What
remains here is the **consumer** (`-k`/`--getbinpkg` reuse) and the **stage3
re-pack** (`em stages` extract-from-binpkgs under one umbrella session). The
fakeroost-specific archiver traps below still apply to the *unprivileged* path
(fakeroost-scoped `src_install`/archive); the privileged path (RealRoot/sudo/
hakoniwa) just uses `tar` directly. This is where Q1's "capture at archive time"
landed.

### What the archiver must preserve (the fakeroost-specific traps)

fakeroost fakes ownership in its table, not on disk, so a naive `std::fs` walk sees
the *real* (build-user, placeholder) files. Correct output requires the archiver to
read through faked `stat`/`getxattr`, which only happens **in-session**. It must emit:

- **owner/gid** numeric, from faked `stat` (`--numeric-owner`; untracked ⇒ 0:0).
- **mode + setuid/setgid bits** (so `mount`/`ping` keep their bits).
- **device nodes**: fakeroost stores `mknod` as a *placeholder regular file* plus a
  recorded `(type, rdev)`; faked `stat` returns the char/block mode+rdev. The
  archiver must emit a real device-node tar entry **from the faked stat**, not copy
  the placeholder. (A plain copy would ship a 0-byte regular file.)
- **file capabilities / ACLs**: fakeroost fakes `security.capability` and ACL
  xattrs via `*setxattr`; the archiver must read them with faked `getxattr`
  (`tar --xattrs`, pax format).
- **hardlinks** (em's merge already tracks `(dev,ino)` at `ebuild.rs:1273`),
  symlinks, mtime.

→ Two implementation options:
- **(a) shell out to GNU `tar --numeric-owner --xattrs --format=pax`** inside the
  session — handles owners/devnodes/caps/hardlinks/ACLs natively, least code.
- **(b) a Rust archiver** (the `tar` crate doesn't do xattrs/devnodes out of the
  box) — more control, more code. Start with (a); revisit if a dependency-free
  static em matters.

### Format

Target **GPKG** (Gentoo's current default: an outer tar of `image.tar` +
`metadata.tar`, each optionally `zstd`/`gzip`, optional GPG per
`BINPKG_GPG_*`/`BINPKG_COMPRESS`). The legacy xpak `.tbz2` (tarball + appended
metadata blob) is read-only-compat territory; don't emit it. The binhost
`Packages` index and `--getbinpkg` consumer are tracked separately in
[[em-stages-and-binhosts]] / PENDING.md (binhost section).

### Where it slots in

- **binpkg**: a new step in the worker between `install` and `qmerge`, gated on
  `buildpkg`, packing `${D}` → `${PKGDIR}/<cat>/<pf>.gpkg` *within the worker's
  fakeroost session*. Metadata tar = the VDB env/`*.ebuild`/`USE`/deps em already
  computes for the VDB write.
- **stage3**: `em stages` final step = extract the selected binpkgs into a clean
  ROOT + emit the stage tar, under **one umbrella fakeroost session** (Q1). Ties
  into the privilege backend the same way — `em __stage-pack` could be a second
  worker-like entry point decorated by `worker_command`.

### Privileged path

Under `RealRoot`/`sudo`/`hakoniwa` the on-disk owners are already real (root, or
ns-mapped root), so the archiver needs no fakeroost session — option (a) `tar` just
works. The fakeroost path is the only one needing the in-session constraint; the
trait hides that (the archiver always runs via `worker_command`, which is a no-op
wrapper for RealRoot).

### DONE (2026-06-28) — the producer + reader + index

Steps 1–6 below (GPKG format, image.tar, metadata.tar, container assembly,
`--buildpkg` wiring, validation) all landed. The GPKG container format is
documented at the top of `portage-binpkg/src/gpkg.rs`; the writer shells to GNU
`tar`+`zstd` (option (a)); metadata.tar is the VDB dir (em already writes every
field during merge); `-b`/`--buildpkg` fires after qmerge inside the privilege
session (`ebuild.rs` `build_binpkg`). Validated: host portage reads,
Manifest-verifies, and decompresses em's gpkg.

The follow-on that landed in the same session: `read_metadata` (the reader,
needed by the `-k` consumer and the index) and `em maint binhost` (the `Packages`
index) — both validated against host portage's `binarytree`. Commits `2f88678`
`0499edc` `72179e9` `65b2438` `359e65b` (producer), `1b46a62` `413364f`
(reader + index).

### `-k`/`-g` consumers — DONE (see `todo/em-stages-and-binhosts.md`)

`-k`/`--usepkg` local reuse (validity check: version + USE + ABI + (sub)slot,
`_match_use` bug-#453400 rule) and `-g`/`--getbinpkg` remote (`portage-distfiles`
transport, `Packages.gz`/`binrepos.conf`) both landed and are validated
end-to-end — this was the last "NEXT" item pointed at from here. What's still
open: signing (`BINPKG_GPG_*`) and stage3 re-pack (`em stages`
extract-from-binpkgs under one umbrella fakeroost session — see Q1).

### GPKG container format (reverse-engineered + validated against a real host gpkg)

Container = a **plain (uncompressed) tar**, all members owned `0/0`, in this
**strict order**:

1. `<basename>/gpkg-1` — **0-byte** format marker, **must be first**
   (`gpkg.py` `gpkg_version = "gpkg-1"`; verify reads it first).
2. `<basename>/metadata.tar.<c>`
3. `<basename>/image.tar.<c>`
4. `<basename>/Manifest` — **must be last** ("ignored since at the end").

- `<basename>` = the package **PF** (e.g. `gentoo-functions-1.7.6`), i.e.
  `basename.split("/")[-1]` — *no* category, *no* build-id. The **container
  filename** is `<PF>-<BUILD_ID>.gpkg.tar` (build-id in the name + `metadata/BUILD_ID`).
- `<c>` = `BINPKG_COMPRESS` suffix; **default `zstd` → `.zst`** (make.globals).
- **image.tar**: members under the `image/` prefix = the `${D}` tree, ustar/pax,
  `--numeric-owner` (host writes `root/root`); pax + `--xattrs` for caps/ACLs;
  real device-node entries; setuid bits preserved.
- **metadata.tar**: members under `metadata/` = the VDB entry dir — every xpak
  field file (`PF CATEGORY SLOT KEYWORDS USE IUSE *DEPEND RESTRICT LICENSE EAPI
  DEFINED_PHASES INHERITED FEATURES CHOST CBUILD C*FLAGS LDFLAGS DESCRIPTION
  HOMEPAGE repository …`), plus `CONTENTS`, `environment.bz2`,
  `NEEDED`/`NEEDED.ELF.2`/`REQUIRES`, `SIZE`/`BUILD_TIME`/`BUILD_ID`/`COUNTER`/
  `BINPKGMD5`, and `<PF>.ebuild`. **em already writes all of these to the VDB
  during merge** — metadata.tar = tar that dir under `metadata/`.
- **Manifest** lines: `DATA <member> <size> SHA512 <hex> BLAKE2B <hex>`, one per
  container member **including `gpkg-1`** (the 0-byte file has the well-known
  empty-string SHA512/BLAKE2B). em's `portage-repo::Manifest` already speaks this.
