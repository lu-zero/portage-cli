# Bootstrapping a usable compiler under `em --prefix`

How to give a `--prefix` overlay its own real toolchain (gcc, and anything
built on top of it, e.g. `llvm-core/clang`) and actually **use** the result —
not just install it. Live-verified end to end 2026-08-08 (commit `2db6198`):
`llvm-core/clang` built successfully under a `--prefix`'s own gcc, and the
installed `clang-22` compiled, linked, and ran a real hello-world.

Topology background: [`root-topology.md`](./root-topology.md). Design
context for the wider multi-root bug class:
[`em-prefix-experiment.md`](./em-prefix-experiment.md). For `--local`
(standalone, no host sharing) see
[`local-bootstrap.md`](./local-bootstrap.md).

---

## The ladder

```sh
em --prefix P toolchain --setup    # bootstrap baselayout → binutils → headers → glibc → gcc into P
em --prefix P select gcc set N     # activate P's own gcc (also wires clang's toolchain-detection cfg)
em --prefix P llvm-core/clang      # (or any package) — now builds using P's own gcc, not the host's
em --prefix P active set           # register P as the active context
eval "$(em active env)"            # PATH + LD_LIBRARY_PATH for the current shell
clang-22 --version                 # works: found on PATH, runs without missing-library errors
```

No `package.provided` seeding needed here — unlike `--local`, `--prefix`
shares the host's VDB for `DEPEND`/`BDEPEND` satisfaction, so there's no
hard-cycle bootstrap problem to solve first.

## Why the last two steps matter

A binary installed under `P` (e.g. `P/usr/lib/llvm/22/bin/clang-22`) uses the
plain **host** ELF interpreter — `--prefix` is not a true relocatable EPREFIX
Gentoo Prefix with its own patched loader. That means:

- **`PATH`**: without `em active`, the binary isn't even findable (it isn't
  under `/usr/bin`; e.g. LLVM installs under `usr/lib/llvm/<slot>/bin`, only
  reachable via the prefix's own `etc/env.d`).
- **`LD_LIBRARY_PATH`**: even once found, the binary needs its own
  just-installed libraries (e.g. `libstdc++.so.6` from `P`'s own gcc) — the
  host's own `/etc/ld.so.cache` doesn't know about `P` at all. `em active
  env` exports this too, read from `P/etc/ld.so.conf`.

Skipping `em active` and just invoking `P/usr/lib/llvm/22/bin/clang-22`
directly reproduces the classic "prefix-confined, unusable" failure
(`error while loading shared libraries: libstdc++.so.6`) even after a
successful build — this was the entire 2026-08-05 finding, since superseded.

The same `LD_LIBRARY_PATH` gap also broke the **build itself**: a build-time
tool a package compiles and re-executes mid-build (e.g. `llvm-core/llvm`'s
own `llvm-min-tblgen`) hit the identical error. Fixed the same way, in
`em`'s own build shell (not `em active`'s concern — see
[`em-prefix-experiment.md`](./em-prefix-experiment.md)'s "Related code"
table).

## Negative control: `em select gcc` isn't optional

Without `toolchain --setup` first (no gcc of its own), `llvm-core/clang`
still builds under `--prefix` — but using the **host's** clang/gcc for the
build-time bits, and the result is the original 2026-08-05 "prefix-confined"
compiler: builds fine, structurally unusable (no libc of its own to link
against). Giving `P` its own gcc is what makes the difference.

## Status

Live-verified 2026-08-08. `--local` (standalone, no host sharing) not yet
re-verified against this same ladder — see
[`todo/for-sonnet.md`](../todo/for-sonnet.md) for the pending check.
