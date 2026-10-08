# API requirements for an `em` backend in crossdev-stages

Status: 🔴 not started. Written 2026-10-06 from a review of
`~/Sources/crossdev-stages` (`src/portage.rs`, `sandbox.rs`, `target.rs`,
`image.rs`) against `em 0.1.0`'s `--help` surface and the notes linked
below. Nothing here has been run end to end; items marked **unverified**
were read from help text or notes only.

Related: [[crossdev-gcc-version-flag]], [[em-stages-and-binhosts]],
[[em-stages-scenario-matrix]], [[activity-status]], [[root-topology-refactor]],
[[portageq-workalike-plan]]. Caller-side notes live in crossdev-stages'
own `todo/em-replaces-crossdev-gcc-pin.md`.

## Context

crossdev-stages drives real `crossdev` / `emerge` / `<tuple>-emerge`
inside a hakoniwa sandbox (an unpacked amd64 stage3) to build a cross
toolchain, a target stage bind-mounted at `/target`, and then a board
image. The plan on that side is a backend trait at *operation* level with
two implementations, real portage and `em`:

| Operation | Real backend today |
|-----------|--------------------|
| `sync` | `emerge-webrsync`, `getuto` |
| `install_host(atoms)` | `emerge -b -k <atoms>` (and `-G` for a few) |
| `setup_toolchain(spec)` → gcc version | ~12 calls: `eselect repository`, `crossdev --init-target`, keywords, host gcc emerge, `gcc-config`, `eselect profile`, `merge-usr`, `crossdev --gcc V --ex-pkg …`, `gcc-config` |
| `stage1` | `USE=build ROOT=/target <tuple>-emerge baselayout`; `packages.build`; `USE=build … portage` |
| `install_target(atoms)` | `ROOT=/target <tuple>-emerge -b -k <atoms>` |
| `install_prefix(atoms)` | `<tuple>-emerge -b -k <atoms>` (no `ROOT`, lands in `/usr/<tuple>`) |
| `update_target` | prefix refresh (gcc, binutils-libs, `-u system`), then `KERNEL_DIR=/usr/src/linux ROOT=/target <tuple>-emerge -b -k -e @world` |
| `installed_versions(cpn)` | `qlist -ICev` |

**Decided (Luca, 2026-10-06): the integration is a library surface, not a
subprocess.** crossdev-stages and similar tools link `portage_cli` and
call typed operations; the `em` binary copied into the sandbox stays a
testing convenience only. R0 states what that surface needs; R1–R9 are
the behaviours behind it, described with their CLI spelling because that
is how they exist and are tested today. Each must be reachable as a
library call with the same semantics.

## Requirements

### R0 — Library surface (the integration point)

What exists today: `portage-cli` is already a lib crate (`portage_cli`)
exporting `run(&cli::Cli)`, the `activity` types and a few error types.
That is a CLI entry point callable from Rust, not an API: the caller has
to build a `Cli` value, and output/exit behaviour is the binary's.

- **Typed operations, not a `Cli` struct.** One function (or method on a
  session/context object) per operation in the table above, taking domain
  types: tuple, roots, atoms as `portage_atom` types, a toolchain spec
  (tuple, profile, gcc pin, extra packages), merge options (buildpkg,
  usepkg, getbinpkg, jobs, load). Per AGENTS.md's "never downgrade a
  domain type" rule, no stringly-typed atoms or versions at the boundary.
- **Explicit roots.** Every call takes its root topology as a value
  (host root, sysroot, destination root — see R1). No reading of ambient
  `ROOT`/`SYSROOT`/`PORTAGE_CONFIGROOT` from the process environment, and
  no `em active` registration consulted implicitly.
- **Where the work executes — the main design question.** crossdev-stages
  runs on the host, which need not be Gentoo; resolves and builds must
  happen against a sandbox rootfs (an unpacked amd64 stage3) entered
  through a hakoniwa user-namespace container with specific bind mounts
  (`/target`, `/build`, `/cache`, `/var/log`). Two shapes, pick one:
  1. *Caller-supplied executor.* The library does config reading and
     resolution in-process against root paths on the host filesystem, and
     spawns every phase/helper process through an executor the caller
     provides (crossdev-stages' own container builder). `privilege.rs`
     already has a `Hakoniwa` backend; this generalises it to "run inside
     this rootfs with these mounts". Path translation between host view
     (`<sandbox>/usr/T`) and container view (`/usr/T`) must be explicit.
  2. *Re-exec inside the sandbox.* The library offers a helper entry
     point; the caller's own binary re-executes itself inside the
     container and calls the library there, where all paths are native.
     Simpler for the library, but requires the caller binary to be
     runnable inside the stage3 (static link or bind-mount).
     **The channel back already exists** (Luca, 2026-10-07): the
     install-worker re-emit path. The parent binds a Unix socket and
     passes `--activity-reemit-path`; the child's
     `JsonlFdSink::connect_reemit` writes `ActivityEvent`s as JSONL
     (`to_jsonl_line` / `from_jsonl_line` round-trip) and
     `spawn_install_worker_with_reemit` (`privilege.rs`) feeds them into
     the parent's `ActivityBus`. Shape 2 is that same mechanism with the
     container boundary in place of the privilege boundary, so the
     requirement reduces to:
     - make the re-emit spawn usable with a caller-supplied command
       builder (the hakoniwa container), not only the built-in privilege
       backends;
     - the socket must be reachable inside the container (bind under a
       directory the caller mounts — `work_base` already is one);
     - the event stream must carry what the caller needs as a *result*,
       not only progress: terminal success/failure per session with the
       typed error payload (package, phase, log path), and for
       `setup_toolchain` the resolved gcc version (R2).
  3. *hakoniwa closure* (Luca, 2026-10-07). hakoniwa has
     `unsafe Container::command_from_closure(F)` with
     `F: Fn() -> i32 + Send + Sync + 'static` (present in 1.7.x, which
     crossdev-stages already locks; neither repo uses it yet). The
     container is set up as for a normal command, then the closure runs
     in the forked child *instead of* an exec, and its `i32` becomes the
     exit status (panics are caught and reported as failure). The caller
     builds its usual container and passes a closure that calls the typed
     library operation directly.
     - Gains over shape 2: no re-exec, so the caller binary does not have
       to be runnable inside the stage3; arguments are captured as Rust
       values instead of being serialised onto a command line; paths are
       still native inside the container.
     - The closure only returns an `i32`, so results and events still
       need the channel — but it can be an inherited fd (a pipe or
       socketpair created before the fork, wrapped with
       `JsonlFdSink::from_owned_fd`), with no socket path to make visible
       inside the container.
     - **Fork-without-exec hazards**, which the `unsafe` is about and
       which this surface must be designed around:
       - The child has only the forking thread. Locks held by other
         threads at fork time (allocator, `tracing` subscriber, stdio)
         stay locked. crossdev-stages is `#[tokio::main]` multi-thread
         and `em` builds a multi-thread runtime, so the fork point must
         be controlled: fork from a single-threaded helper started before
         any runtime, or guarantee quiescence. `em` defaults to mimalloc;
         its behaviour in a forked child of a threaded process needs
         checking.
       - The parent's tokio runtime is unusable in the child. The library
         entry called from the closure must be synchronous from the
         caller's point of view and build its own runtime inside the
         child (so R0's "Async" point becomes: offer a blocking entry
         that owns a fresh runtime).
       - After the root switch, anything loaded lazily comes from the
         sandbox rootfs while the process runs the host's libc: NSS
         lookups (`getpwnam`), gconv, locales. The library must not rely
         on those inside the closure (read `passwd`/`group` files
         directly), or the caller must link statically.
       - Process-global state set by the caller before the fork (signal
         dispositions, cwd, umask, inherited fds) is the child's starting
         state; the entry point should reset what it depends on.
     - To check: whether any `em` path re-executes its own binary
       (install-worker under a privilege backend). Inside the container
       the caller is already mapped root, so `RealRoot` should apply and
       no worker should be needed.
  Shape 1 is the cleanest API but the most work; shape 2 reuses a
  mechanism already built and tested; shape 3 gives the typed call with
  no binary inside the sandbox, at the price of the fork constraints
  above. **Leaning shape 3 if a single-threaded fork point proves
  practical, otherwise shape 2; not yet decided.**
- **No process-global side effects.** No `std::process::exit`, no writes
  to stdout/stderr except through a caller-supplied sink, no changing the
  process cwd/umask/env, no installing signal handlers unless asked (see
  [[signal-handling]]). Two sessions on different roots must be able to
  coexist in one process.
- **Typed errors.** Distinguish at least: config/usage error, resolve
  failure (with the conflict), required config changes
  (`ConfigChangesNeeded` exists), build failure (package, phase, log
  path), fetch failure, and cancellation. The caller decides what is
  retryable.
- **Progress and logs.** The `ActivityBus` / `ActivityEvent` stream is the
  progress interface (R9, now required rather than nice-to-have). Build
  logs still land on disk under `PORT_LOGDIR` (R7).
- **Cancellation and resume.** A cancellation handle that stops cleanly
  between merges, and a resume that does not redo completed merges.
- **Async.** `run` is `async`; crossdev-stages already runs on tokio.
  State whether the library needs to own the runtime or runs on the
  caller's, and that it does not block it for the length of a build.
- **Packaging.** crossdev-stages takes its workspace deps from crates.io
  (`gentoo-stages 0.4`, `portage-atom 0.5.1`). The surface therefore has
  to be published and semver-managed — either `portage-cli` itself or a
  slimmer facade crate without the clap/CLI dependency tree. Same
  coordination caveat as [[gentoo-stages-workspace-types]].

### R0a — How both sides use hakoniwa today (read 2026-10-07)

`em` (`portage-cli/src/privilege.rs`, `main.rs`):

- hakoniwa is a *privilege* backend, not a rootfs switch. `hakoniwa::reexec`
  builds a container on `rootfs("/")` (the host's own root), identity-binds
  the trees a build touches (merge root, eprefix, config overlay, `/tmp`,
  `/var/tmp`, every repo read-only, `DISTDIR` read-write), binds the `em`
  binary read-only and re-executes it with `EM_PRIVILEGE_ACTIVE=hakoniwa`.
  There is no way to say "enter this other rootfs".
- It is an umbrella: `maybe_supervise` runs in `main` **before the tokio
  runtime is built**, so `em` already forks from a single-threaded point —
  but that guarantee lives in the binary's `main`, not in the library.
- `self_invocation()` uses `current_exe()`. Linked into another program,
  that is the caller's binary, which has no `em` argument parser.
- Process-global state on this path: `PRIVILEGE_REQUEST` (`OnceLock`),
  the `EM_PRIVILEGE_ACTIVE` env marker, the whole environment forwarded
  into the container, and `resolved_root` falling back to `$ROOT`.
- The id-map code (`read_subid`, `idmaps_for`) is a copy of
  crossdev-stages', and says so in its comment.
- Internals are `Cli`-shaped: `emerge_atoms(&Cli, &[String], EmergeOpts)`
  is `pub(crate)`; `crossdev::run`, `toolchain` and `stage1` all take
  `&Cli`. The pieces that are already library-shaped: `Roots`
  (`portage-resolve`, built through `with_*` methods) and `EmergeOpts`,
  which already carries `activity: Option<ActivityBus>`, the session
  correlation ids and a conf-layer `use_override`.
- Dependency: the `lu-zero/hakoniwa` git fork at `5f77bb1`.

crossdev-stages (`src/container.rs`):

- hakoniwa *is* the rootfs switch. `SandboxRunner::build_container` uses
  `rootdir(<stage3>)` with `RootdirRW`, the same namespaces and id maps,
  a `resolv.conf` fallback, and bind mounts for `/var/log`, `/target`,
  `/build`, `/cache`, `/scripts`. Everything runs as `bash --login -c`.
- The container is also used for non-portage work: kernel and bootloader
  build scripts, `genimage`, `ldconfig -r /target`, `sandbox run`, the
  interactive shell, and unpack/destroy of root-owned trees (those three
  on `rootfs("/")`, each with its own copy of the setup).
- `main` is `#[tokio::main]`; containers are spawned from inside the
  runtime, which is fine today because every spawn execs.
- Dependency: crates.io `hakoniwa` 1.7.0.

Consequences for R0: the two would need one hakoniwa (fork vs crates.io)
to link together; the id-map and container setup is already duplicated
across the repos; and the "typed operations" requirement means peeling
`&Cli` off four entry points, with `Roots`/`EmergeOpts` as the starting
point rather than a blank page.

### R0b — Inverting: `em` owns the container (Luca, 2026-10-07)

Instead of crossdev-stages building a container and calling `em` inside
it, `em`'s hakoniwa backend grows from "mapped root on `/`" to "mapped
root in a given rootfs", and the caller hands it a description of the
box. Levels, from least to most inverted:

1. **Shared container crate.** Move the common setup (id maps, namespace
   set, rootdir/rootfs container, `resolv.conf` handling, unpack/destroy
   of mapped-owner trees) into one workspace crate that both `em` and
   crossdev-stages use — next to `gentoo-stages`, which is already the
   shared-with-crossdev-stages crate. Removes today's duplication and the
   fork/crates.io split. Worth doing whatever else is decided.
2. **`em` enters a caller-described rootfs for its own operations.** The
   library takes a container spec (rootdir, extra binds, log dir) with
   each session and does the fork/enter itself, for portage operations
   only. crossdev-stages keeps its runner for scripts, `genimage` and the
   shell, built from the same shared crate so both agree on the box.
   - The fork point, the channel (re-emit or inherited fd), the runtime
     rebuild and the "no lazy libc loading" rule become `em`-internal
     invariants instead of a contract the caller must honour. That is the
     main argument for inverting.
   - It does not remove the choice between closure and re-exec; it moves
     it inside `em`. Re-exec needs a worker hook the *caller's* `main`
     calls first (the `em __worker` pattern, since `current_exe()` is the
     caller) and a binary that runs against the stage3's libc. Closure
     needs the fork-hazard list under shape 3.
   - `rootfs("/")` identity binds no longer apply: with a foreign rootdir
     the repos, `DISTDIR`, `PKGDIR` and work trees are the sandbox's own,
     and only caller-named binds cross the boundary.
3. **`em` is the sandbox provider.** `em` also exposes "run this command
   in the box", and crossdev-stages has no container code. Pulls a
   general sandbox API into `em`'s semver surface for little gain over
   level 1+2; not recommended.
4. **No stage3 sandbox at all.** `em --local` / `em setup` /
   `em toolchain` bootstrap the build host themselves, so the amd64
   stage3 and the rootfs switch disappear and only the privilege box on
   `/` remains — what `em` does today. The far end of the inversion and
   dependent on [[local-bootstrap]] being finished; noted, not planned.

**Leaning level 1 + 2**, with the closure-vs-re-exec choice made by the
spike below. Not decided.

### R1 — Three-root installs: tuple + separate destination root

`install_target` and `stage1` build for tuple `T` using the cross
toolchain and sysroot config at `/usr/T`, but install into a *different*
root, `/target`. `--target T` is documented as sugar for
`--config-root <sysroot> --root <sysroot>`.

- Needed: `em --target T --root /target [-b -k] <atoms>` must resolve with
  the sysroot's config and cross context, install runtime deps into
  `/target`, and put build deps where the real wrapper's
  `--root-deps=rdeps` puts them.
- **Unverified by running**, but the topology is modelled:
  `ActivityEvent::SessionStart` has a `base_root` field documented as
  "board-root topology only, `--target T --root R`", and
  [[root-topology-refactor]] records `em --root R --target T crossdev
  --init-target` as live-verified. What still needs a run is a plain atom
  merge and `em stages` under that combination, including where build
  deps land. Still the first thing to check; everything after toolchain
  setup depends on it.
- Same combination is needed for `em stages --stage1/--stage3`.

### R2 — Toolchain setup as one call, with a gcc pin

`em --target T crossdev --setup --ex-pkg A --ex-pkg B` already matches
`setup_toolchain`. Missing pieces:

- **gcc pin.** A bare slot (`16`) or a version prefix (`16.2`), per
  [[crossdev-gcc-version-flag]]. crossdev-stages will write the pin as
  config (keywords plus a mask window) itself, so the hard requirement is
  only that `resolve_gcc_version` and `maybe_weave_in_gcc_update` honour
  such config written before the call. A first-class `--gcc` is the
  better end state.
- **The pin must cover the sysroot's native `sys-devel/gcc` too.** With
  real crossdev today the pin governs only the host/cross compiler; the
  prefix gets `sys-devel/gcc:16 **` and the native gcc pulled by
  `packages.build` resolves to the live `16.3.9999`. An `em` pin should
  not reproduce that.
- **Report the resolved version.** The caller stores the exact gcc
  version in a marker for idempotency. Needed: a way to read it back
  without scraping logs (`--show-target-cfg` in a parseable form, or an
  `em portageq best_version` call against the right root).
- **Profile choice.** crossdev-stages selects a real profile for the
  sysroot (not crossdev's `embedded` default); [[crossdev-target]] says
  `em` follows that fix. Needed: either `em crossdev` takes the profile,
  or `em --target T select profile set <P>` before `--setup` is honoured.
- **Idempotent re-run.** Per the caller-side note, `--setup` fills gaps
  only, so a changed pin needs `--init-target` as well. Needed: a
  documented "re-run with changed spec" invocation that is safe to issue
  every time.

### R3 — Stage semantics the caller can choose

`em stages --stage1` is catalyst-shaped (`--nodeps` baselayout,
`USE="-* build"`); `--stage3` is `-e -uD --with-bdeps @system`.
crossdev-stages does `packages.build` with normal USE, then portage with
`USE=build`, and for update an emptytree `@world` with `KERNEL_DIR` set.

- Open decision (Luca): accept `em`'s semantics for the `em` backend, or
  require parity. If parity: needs per-invocation USE override and a
  `@world` emptytree with an extra env var passed to builds.
- Either way: `em stages` must accept R1's root combination, and must be
  resumable after a failed package without redoing completed merges.

### R4 — Sysroot merge order: baselayout before `acct-*`

Found in crossdev-stages on 2026-10-05 with real portage 3.0.82.2: in the
sysroot, `acct-user/portage` merged before `sys-apps/baselayout`, so
baselayout's `pkg_postinst` skipped copying its `passwd` (one already
existed) and the sysroot was left with a one-line `passwd` lacking
`root`. `fowners root:portage` in `app-admin/eselect`'s install phase
resolves against `${ESYSROOT}/etc/passwd` and then fails.

- Needed: `em crossdev --setup` (and any sysroot-populating resolve) must
  guarantee baselayout is merged into the sysroot before any `acct-user`
  / `acct-group` package, regardless of `--jobs`.
- Worth a regression check that `em`'s own `fowners` lookup uses the same
  root as portage's (`ESYSROOT` in `src_install`).

### R5 — Non-interactive, fully parameterised, meaningful failures

- Every option the caller needs must be a parameter (and, for the CLI, a
  flag); no reliance on env-var prefixes (`ROOT=`, `USE=`, `KERNEL_DIR=`)
  or a login shell's `PATH`. If a build needs an extra env var (R3), a
  parameter to pass it.
- Never prompt. `--autounmask-write` behaviour must be deterministic:
  either it writes and continues, or it fails with a distinct exit code.
- Failures are distinguishable — typed errors in the library (R0), and
  matching distinct exit codes in the CLI: resolve failure, build
  failure, bad usage.
- Atoms straight from board files (`target-packages.txt`,
  `sandbox-packages.txt`) are passed unmodified, including slot and
  version-glob forms (`=cat/pkg-1.2*`); confirm the glob form is accepted
  or document the alternative.

### R6 — Config written by the caller is honoured

crossdev-stages writes these directly into `/etc/portage` (host) and
`/usr/T/etc/portage` (sysroot) and will keep doing so for both backends:
`make.conf` (CHOST, CFLAGS, MAKEOPTS, FEATURES, LLVM_TARGETS,
ACCEPT_KEYWORDS, GENTOO_MIRRORS, PORTAGE_BINHOST), `package.use/*`,
`package.accept_keywords/*`, `package.mask/*`, `package.env/*` with
`env/*.conf` (per-package CFLAGS workarounds), and the `make.profile`
link plus `profile/` directory copied into `/target/etc/portage`.

- Needed: `em` reads all of the above identically to portage for the
  sysroot and for `/target`.
- `em pkg keyword|use|mask|env` and `em select profile` with `--target`
  are a nicer route but not required.
- `em crossdev --init-target` must not overwrite caller-written files in
  the sysroot config (or must document exactly which it owns).

### R7 — Binary packages and logs in the places the caller expects

- `-b -k` everywhere, with `PKGDIR` per sysroot so two boards sharing a
  tuple but not CFLAGS do not poison each other — see [[binpkg-subtargets]].
- `-g` with `PORTAGE_BINHOST` from `make.conf` for the host side.
- Build logs under `PORT_LOGDIR` (`/var/log/portage/<arch>`), and an
  `emerge.log`-equivalent, because `/var/log` is bind-mounted out of the
  sandbox and is where failures get diagnosed.

### R8 — Queries

- Installed versions of a package in a given root, one per line, newest
  identifiable: `em portageq match` / `best_version` with `--root` or
  `--target`. Replaces `qlist -ICev sys-devel/gcc`.
- Host `CHOST`: `em portageq envvar CHOST` (**unverified** that `envvar`
  exists; not in the subcommand list read on 2026-10-06).
- `em env` after toolchain changes; `em select compiler set` if compiler
  activation is still a separate step under `em` (ideally `--setup` leaves
  the right compiler active and the caller never calls it).

### R9 — Progress

Required, via the in-process `ActivityBus` channel described in
[[activity-status]] (which already anticipates crossdev-stages as an
in-process consumer and lets the caller pass an outer `job_id` to span
several operations). `--activity-fd` / `--activity-jsonl` remain for the
CLI. The event types become part of the semver-managed surface (R0).

## Plan (2026-10-07)

Two repos, ordered so each phase is useful if the next one slips. `em`
phases are in this repo; `cs` phases are in crossdev-stages.

| # | Repo | Phase | Unblocks | State |
|---|------|-------|----------|-------|
| 1 | em | **Toolchain version preferences** — `--binutils/--gcc/--kernel/--libc`, resolved and written as config for both the cross and the sysroot packages ([[crossdev-gcc-version-flag]]) | R2 pin; the live-gcc leak | flags written 2026-10-07 and dropped the same day as the wrong shape; superseded by a spec file ([[cross-sysroot-spec]], branch `cross-sysroot-spec`, not merged) |
| 2 | em | **Preference axes beyond GCC+glibc** — LLVM-model slot/runtimes, `--profile` and a flavour → profile mapping, GCC model + LLVM flavour | clang as main compiler, other libcs | planned in [[crossdev-gcc-version-flag]] |
| 3 | em | **Sysroot ordering** — baselayout before any `acct-*` in sysroot-populating resolves, with a regression test (R4) | from-scratch stage1 | open |
| 4 | em | **Verify the three-root path** — plain atom merge and `em stages` under `--target T --root R` (R1), `em portageq envvar` (R8) | everything after toolchain setup | open |
| 5 | cs | **Safety net** — exec trait under the runner, recording fake, snapshot the real backend's commands | every later cs refactor | open |
| 6 | cs | **One config writer** — host-side writer for all `/etc/portage` trees; real-backend gcc pin as a version window; baselayout-first prefix fix | the two open cs bugs | open |
| 7 | cs | **Operation-level backend trait** with only the real backend behind it; `sandbox.rs`/`target.rs` keep markers, know no commands | the seam | open |
| 8 | both | **Shared container crate** — id maps, namespace set, rootdir container, `resolv.conf`, unpack/destroy (R0b level 1); one hakoniwa source | linking the two | open |
| 9 | em | **Execution spike** — `rootdir` variant of the hakoniwa backend; closure vs re-exec in a bare sandbox (R0, R0b) | the library shape | open |
| 10 | em | **Typed surface** — peel `&Cli` off `crossdev::run`/`toolchain`/`stage1`/`emerge_atoms` starting from `Roots` + `EmergeOpts`; blocking entry that owns its runtime; results on the activity stream | R0 | open |
| 11 | cs | **`em` backend** behind the trait, `setup_toolchain` first, one board | the goal | open |
| 12 | both | **Compare** a full board build across both backends by installed package set | confidence | open |

Phases 1–4 need no decision. 5–7 need none either and are worth doing
without `em`. 8–10 wait on the execution-shape decision; 11 on the stage
semantics decision.

## Open decisions (Luca)

1. Stage semantics for the `em` backend: `em stages` as-is, or parity
   with today's sequence (R3).
2. ~~Subprocess or library~~ — **library** (decided 2026-10-06). Follow-up
   decision: execution shape 1 (caller-supplied executor), 2 (re-exec
   inside the sandbox over the existing activity re-emit socket) or 3
   (hakoniwa closure with an inherited-fd channel), see R0; and who owns
   the container — the caller, or `em` (R0b, leaning `em` for its own
   operations on top of a shared container crate).
3. Wait for `--gcc`, or ship on the config-written pin (R2).

## How to attack

0. Decide R0's execution shape; it determines whether the other
   requirements are met by parameterising existing code paths (shapes 2
   and 3) or by threading an executor and path mapping through them
   (shape 1), and whether `em` or the caller owns the container (R0b).
   First a spike, inside `em` where the pre-runtime fork point already
   exists: give `privilege::hakoniwa` a `rootdir` variant, run
   `em -p <atom>` in a bare crossdev-stages sandbox both ways —
   `command_from_closure` with events over an inherited pipe, and re-exec
   of a bound binary — and record what breaks in each. Then repeat the
   closure variant from a threaded tokio caller.
1. Verify R1 with a pretend run in a bare sandbox:
   `em --target T --root /target -p sys-apps/baselayout`, then one real
   leaf package. If it does not work, that is the blocker to fix first.
2. Verify R2's config-written pin with `em --target T crossdev
   --show-target-cfg` before a real `--setup`.
3. Write a regression test for R4 (parallel sysroot populate, assert
   `passwd` has `root`).
4. Fill in the **unverified** items above.
5. Sketch the typed surface for one operation (`setup_toolchain`) and
   pilot it from crossdev-stages on one board (i586 or riscv64, where
   pilot sandboxes already exist) before generalising to the rest.
