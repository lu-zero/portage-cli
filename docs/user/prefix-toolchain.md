# Bootstrapping a usable compiler under `--prefix`

Give a `--prefix` overlay its own gcc (and anything built on it, e.g. clang)
and actually **use** the result.

Topology: [`root-topology.md`](../design/root-topology.md). EPREFIX / multi-root
pitfalls: [`em-prefix-experiment.md`](../design/em-prefix-experiment.md).

## The ladder

```sh
em setup --prefix P
em toolchain --prefix P --setup    # baselayout → binutils → headers → glibc → gcc
em select gcc set --prefix P N     # activate P's gcc
em --prefix P llvm-core/clang      # builds with P's gcc, not the host's
em active set --prefix P           # register P as the active context
eval "$(em active env)"            # PATH + LD_LIBRARY_PATH for this shell
```

`em toolchain --setup` resolves against the prefix VDB only (host packages do
not count as already installed). The libc step runs unless the active profile
has `USE=prefix-guest`. It does not need `package.provided`.

## Why `em active env` matters

Packages built into `P` record `EPREFIX=P`. Runtime search still has to find
`P`'s libraries:

- **`PATH`**: LLVM installs under `usr/lib/llvm/<slot>/bin`, only reachable
  via the prefix's `etc/env.d`.
- **`LD_LIBRARY_PATH`**: the host `ld.so.cache` does not know `P`'s
  `libstdc++`. `em active env` exports this from `P/etc/ld.so.conf`.

Invoking `P/usr/lib/llvm/22/bin/clang-22` without that environment fails with
`error while loading shared libraries: libstdc++.so.6`. The same gap hits a
build-time tool a package compiles and re-executes (e.g. `llvm-min-tblgen`);
`em`'s build shell applies the prefix `ld.so.conf` there — see
[`em-prefix-experiment.md`](../design/em-prefix-experiment.md).

## `em select gcc` is not optional

Without `toolchain --setup`, `llvm-core/clang` still builds under `--prefix`,
but with the **host** compiler. The result cannot link against a libc of its
own. Giving `P` its own gcc is what makes the difference.

## `--local`

`em active env`'s `LD_LIBRARY_PATH` export is keyed on the activated path's
`ld.so.conf`, so `--local` gets the same fix. A standalone `--local` prefix
does not share the host VDB and needs its own libc; see
[`stages-and-testing.md`](./stages-and-testing.md).
