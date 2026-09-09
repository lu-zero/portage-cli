---
name: em-prefix-sandbox-test
description: Real (non-pretend) smoke test that em's --prefix topology still works — layout bootstrap, an actual compile+install, VDB registration, and the `em active` flow — inside a fresh crossdev-stages sandbox. Use after any change to CLI parsing, root/topology resolution, or privilege handling, and before merging a branch that touches those (e.g. deciding whether to swap em's CLI parser). Also use when the user asks to "test --prefix", "check the sandbox", or "run the prefix regression test".
---

# em --prefix sandbox smoke test

Runs `test-scripts/test-prefix-sandbox.sh`, a fast (~1-2 minute, once the
sandbox stage3 is cached) live regression check that `--prefix` actually
works end to end — not just that it parses. It builds `em` from a given
worktree, bootstraps a `--prefix` layout, does a real `--nodeps` build of
`sys-libs/zlib` into it (a small, dependency-light package — enough to
exercise the full compile → install → VDB path without paying for a full
toolchain bootstrap), and checks the `em active set/env/list` flow.

## When to use this

- After changing anything in `cli.rs`'s topology/root resolution, the
  `Cli`/`Applet` parser layer, or `privilege.rs`.
- Before recommending a merge/swap decision on a branch that touches CLI
  parsing (e.g. the `usage-rs` clap-migration branch) — `-p`/pretend and
  unit tests don't catch everything; this is the "did a real build still
  land in the right place" check. See `todo/usage-rs-cli-migration.md` for
  the current such branch and why this check matters for it specifically.
- Whenever the user asks to verify `--prefix` (or the broader root-topology
  matrix — see "Related" below) still works after a change.

## How to run it

```sh
./test-scripts/test-prefix-sandbox.sh --worktree /path/to/worktree [--sandbox NAME] [--keep]
```

- `--worktree DIR` — which `portage-cli` checkout to build `em` from.
  Omit to test the current repo; pass a sibling worktree (e.g.
  `../portage-cli-usage-rs`) to test a branch without switching your own
  checkout.
- `--sandbox NAME` — sandbox name (default `em-prefix-check`). Always
  destroyed and recreated fresh at the start — never reused across runs,
  and destroyed again on exit unless `--keep` is given.
- `--keep` — leave the sandbox up for manual poking afterward. Destroy it
  yourself when done: `(cd ~/Sources/crossdev-stages && cargo run --release -- sandbox destroy NAME)`.

Exit code is non-zero if any check failed; each check prints `PASS`/`FAIL`
plus a one-line detail, ending in `ALL CHECKS PASSED` / `SOME CHECKS
FAILED`.

Requires a `crossdev-stages` checkout at `~/Sources/crossdev-stages`
(override with `CROSSDEV_STAGES_DIR`) — see
[`test-scripts/README.md`](../../../test-scripts/README.md) for what that
tool is. First run may be slower if the stage3 tarball isn't cached yet;
subsequent runs against the same arch are fast.

## Hard safety rule — read before touching anything in this area

**Only ever drive the sandbox through `crossdev-stages sandbox run` /
`enter` / `setup` / `prepare` / `destroy`. Never `sudo chroot` into a
sandbox, and never hand-`mount --bind` things into one.** This project's
`.claude/settings.local.json` denies `Bash(sudo chroot *)` and
`Bash(chroot *)` outright specifically because of this — repeated
violations of that exact instinct caused real, unrecoverable data loss in
past sessions (see the `crossdev-stages-sandbox` memory for the incident
history). `test-prefix-sandbox.sh` follows this rule; if you're writing a
*new* script or extending this one, keep following it — don't reach for a
lower-level primitive because `sandbox run` feels awkward for something.
That feeling is the signal to ask the user, not to work around it.

Also always use a **fresh** sandbox — `test-prefix-sandbox.sh` destroys
and recreates its named sandbox unconditionally at the start. Never patch
an existing/stale sandbox by hand; if `sandbox run` misbehaves against an
existing sandbox, destroy and recreate it, don't debug the sandbox state.

## Interpreting a failure

- `setup --prefix` FAIL — layout bootstrap itself broke; check the CLI's
  applet dispatch for `Applet::Setup` and `Roots`/topology resolution.
- `build+install --prefix` FAIL — the merge pipeline broke under a
  `--prefix` root; check `Cli::roots()`/`overlay_root` and the build
  shell's `EPREFIX`/`ESYSROOT` derivation.
- `prefix file placement` FAIL but build reported success — files landed
  somewhere other than under the prefix; check `ed_image_dir`/merge-root
  resolution specifically, this is the "topology parsed but was applied
  to the wrong path" class of bug.
- `VDB registration` FAIL — merge completed but `--prefix`'s VDB path
  resolution is wrong (should be `<prefix>/var/db/pkg`, not the host's).
- `active set` / `active env` / `active list` FAIL — the `em active`
  subsystem itself, independent of the build path; check `active.rs`.

## Related

- `test-scripts/regression-matrix.sh` — the heavier root-topology matrix
  (bare / `--root` / `--prefix` / `--local`, real toolchain bootstraps in
  `--full` mode). Use that for a thorough pre-release check; use this
  skill for a quick "did I break `--prefix`" check during iteration.
- `docs/design/root-topology.md` — the topology model this is testing
  against.
- `crossdev-stages-sandbox` memory — full sandbox tooling reference and
  the incident history behind the "never sudo chroot" rule.
