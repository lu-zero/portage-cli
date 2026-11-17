# crossdev: sysroot profile mapping is wrong for some arches

STATUS: open (found 2026-09-27 while fixing the musl subprofile, 50b92529).

`CrossTarget::profile_path` (`portage-cli/src/crossdev/target.rs`) maps
anything outside riscv/x86 to `default/linux/<gentoo arch>/23.0`. Checked
against `profiles/profiles.desc`:

- **arm**: `default/linux/arm/23.0` exists and is in `profiles.desc`, but it picks no float ABI. Per-ISA ones are
  `armv7a_hf`, `armv6j_hf`, … (`armv7a-…-gnueabihf` → `armv7a_hf`).
- **ppc64le**: `Arch::from_chost` yields keyword `ppc64`, so a
  `powerpc64le-…` tuple gets the big-endian `default/linux/ppc64/23.0`
  instead of `default/linux/ppc64le/23.0`.
- **mips**: needs ABI/endianness (`o32`, `n64`, `mipsel/…`), same shape as arm.
- **LLVM model (`-L`)**: a musl target may want `…/musl/llvm` (exists for
  amd64/arm64); decide whether `cross_llvm-*` sysroots should use it.

`sysroot_config_entries` already bails on a missing profile directory, but
`default/linux/arm/23.0` exists as a non-leaf parent, so that check passes.
Validating against the `profiles.desc` leaves would catch it at `--init-target`.

Checked 2026-11-17 against the host tree: `loong/23.0` and `hppa/23.0` are not leaves (`la64v100/lp64d`, `hppa1.1`/`hppa2.0`);
`s390/23.0` is probably wrong for s390x; `mips` has no leaf; riscv32 gets the rv64 profile.
The `/musl` subprofile is missing for loong, s390, mips and sparc; `--init-target` already stops on a missing directory.
