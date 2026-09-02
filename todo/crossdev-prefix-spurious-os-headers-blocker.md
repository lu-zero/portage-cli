# `crossdev --setup --prefix` aborts on a blocker between two uninstalled packages

Status: 🔴 not started, found 2026-09-02 by `regression-matrix.sh --full`.
Blocks the `--prefix` crossdev path entirely — it never reaches the cross
toolchain, so it is upstream of everything in
[[crossdev-gcc-stage1-missing-without-headers]].

## Symptom

`em crossdev --target riscv64-unknown-linux-gnu --prefix P --setup` gets
through its baselayout step, then dies at the *host tools* resolve:

```
[ebuild   R    ] sys-apps/config-site-0 to /root/regress-crossdev-prefix/
[ebuild   R    ] sys-devel/crossdev-20260623 to /root/regress-crossdev-prefix/

[blocks B      ] sys-kernel/linux-headers ("sys-kernel/linux-headers" is soft blocking virtual/os-headers:0-0-r2)
      sys-kernel/linux-headers-6.18 is itself part of this plan — cannot coexist

 * Error: The above package list contains packages which cannot be
 * installed at the same time on the same system.
```

## Why this is wrong, three ways

Same sandbox, same binary, `crossdev --setup` **bare** succeeds (`EXIT=0`)
and resolves the identical two packages as `[ebuild N]`:

```
bare      [ebuild  N     ] sys-apps/config-site-0
          [ebuild  N     ] sys-devel/crossdev-20260623      → builds binutils, EXIT=0
--prefix  [ebuild   R    ] sys-apps/config-site-0
          [ebuild   R    ] sys-devel/crossdev-20260623      → EXIT=1
```

1. **`R` for something that is not installed.** The prefix contains exactly
   one package (`sys-apps/baselayout-2.18-r1`) and the sandbox's own `/` VDB
   is *empty* — 0 packages. Neither `config-site` nor `crossdev` is installed
   anywhere, so both should be `N`, as the bare run correctly shows.
2. **A blocker between two packages that are neither installed nor planned.**
   `sys-kernel/linux-headers` and `virtual/os-headers` are absent from the
   prefix, absent from `/`, and absent from the two-line plan just printed
   above the blocker.
3. **The blocker text contradicts the plan it is printed under.**
   "`sys-kernel/linux-headers-6.18` is itself part of this plan" — it is not;
   the plan has two entries and neither is it.

## Where the blocker comes from

`virtual/os-headers`' RDEPEND is USE-conditional on `prefix-guest`:

```sh
RDEPEND="
    !prefix-guest? ( kernel_linux? ( sys-kernel/linux-headers:0 ) )
    prefix-guest? ( !sys-kernel/linux-headers )
"
```

Under `prefix-guest` the virtual *blocks* native linux-headers — correct
upstream semantics, since a Prefix guest borrows the host's kernel headers
rather than installing its own. `em` sets `prefix-guest` for every `--prefix`
build (universal, not FreeBSD-only — see
[[freebsd-toolchain-prefix-guest-fix]]), so the branch is live here while it
is dead in the bare run. That explains why only `--prefix` trips it.

What it does **not** explain is why a conditional blocker inside an
uninstalled, unplanned virtual is being evaluated and treated as a fatal
conflict at all. Both the phantom `R` and the phantom plan membership point
at the installed view being wrong under `--prefix` rather than at blocker
classification itself — start there, not at
`classify_blockers`/[[blocker-enforcement]].

## Reproduction

```sh
./test-scripts/regression-matrix.sh --full          # sandbox em-regression
# or directly, against a fresh prefix:
sandbox run --name em-regression \
  /root/em-bin crossdev --target riscv64-unknown-linux-gnu --prefix /root/P --setup
```

The bare form is the control that should keep passing:

```sh
sandbox run --name em-regression \
  /root/em-bin crossdev --target riscv64-unknown-linux-gnu --setup
```

## Not caused by this session's changes

Worth stating because the run that found it was also the first exercise of
several same-day fixes: the failure is in resolution, and none of
`0018023` (phase stdin), `3b67885` (VDB `-MERGING-`), `8fddd53`
(`has_version` cross-atom roots), `43b3c45` (lock notice) or `0e943c2`
(interrupt) touch the resolver or the installed view. The bare run using the
same binary resolving the same two packages correctly is the direct control.
