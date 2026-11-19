# Profile Settings for Benchmarking

## Thalia Profile

- **Profile**: `default/linux/arm64/23.0`
- **Keyword**: `~arm64` (accepts testing)
- **Architecture**: `arm64` (little-endian)

## USE Flags (from profile chain)

### Regular USE flags

```
acl bzip2 crypt gdbm iconv ipv6 libtirpc ncurses nls openmp pam pcre readline
seccomp split-usr ssl test-rust unicode xattr zlib
```

### USE_EXPAND derived flags

```
ELIBC=glibc
KERNEL=linux
PYTHON_SINGLE_TARGET=python3_13
PYTHON_TARGETS=python3_13
LLVM_TARGETS=AArch64
CPU_FLAGS_ARM=edsp v8 vfp vfp-d32 vfpv3 vfpv4
VIDEO_CARDS=fbdev
ABI_X86= (empty)
```

### Forced flags

```
arm64 cpu_flags_arm_edsp cpu_flags_arm_v8 cpu_flags_arm_vfp
cpu_flags_arm_vfp-d32 cpu_flags_arm_vfpv3 cpu_flags_arm_vfpv4 elibc_glibc
kernel_linux llvm_targets_AArch64 split-usr test-rust
```

### Masked flags (notable)

```
minimal perl_features_debug perl_features_quadmath test
verify-provenance amd64 x86 multilib abi_x86_* cpu_flags_arm_neon
cpu_flags_arm_aes cpu_flags_arm_sha1 cpu_flags_arm_sha2
```

## Expanded USE flags for solver

When constructing `UseConfig` for the benchmark, these flags should be enabled
(as lower-case USE_EXPAND prefixed names):

```rust
// Regular USE
"acl", "bzip2", "crypt", "gdbm", "iconv", "ipv6", "libtirpc",
"ncurses", "nls", "openmp", "pam", "pcre", "readline", "seccomp",
"split-usr", "ssl", "test-rust", "unicode", "xattr", "zlib",

// USE_EXPAND: ELIBC
"elibc_glibc",

// USE_EXPAND: KERNEL
"kernel_linux",

// USE_EXPAND: PYTHON_TARGETS
"python_targets_python3_13",

// USE_EXPAND: PYTHON_SINGLE_TARGET
"python_single_target_python3_13",

// USE_EXPAND: LLVM_TARGETS
"llvm_targets_AArch64",

// USE_EXPAND: CPU_FLAGS_ARM
"cpu_flags_arm_edsp", "cpu_flags_arm_v8", "cpu_flags_arm_vfp",
"cpu_flags_arm_vfp-d32", "cpu_flags_arm_vfpv3", "cpu_flags_arm_vfpv4",

// Forced (architecture)
"arm64",
```

## Keyword Acceptance

Accept both **stable** (`arm64`) and **testing** (`~arm64`) keywords:

```rust
fn keyword_accepts(keywords: &[Keyword], arch: &str) -> bool {
    keywords.iter().any(|kw| {
        kw.arch.as_str() == arch
            && matches!(kw.stability, Stability::Stable | Stability::Testing)
    })
}
```

## Benchmark Results (2025-05-16)

### With profile USE flags + stable+testing keywords

| Solver   | Packages | Time  |
|----------|----------|-------|
| PubGrub  | 316      | 3.0ms |
| Resolvo  | 88       | 1.1ms |
| Portage  | 246      | 2.9s  |

### PubGrub vs Portage overlap

- 227/245 (93%) shared by CPN
- 18 missing (Rust/LLVM toolchain, libusb, oniguruma, lsb-release, binutils-libs)
- 0 false positives

### Remaining gaps (18 packages)

1. **Rust/LLVM toolchain** (11): pulled via `native-extensions?` or `test? ( test-rust? )`
   nested USE conditionals on python packages
2. **Libusb** (2): `dev-libs/libusb`, `virtual/libusb`
3. **Misc** (5): `oniguruma`, `lsb-release`, `binutils-libs`, `dev-libs/libusb`, `eselect-rust`

## Known Issues

- `slots_for()` in bench adapter must apply keyword filtering (live ebuilds
  like `*-9999` have no KEYWORDS and would create phantom slot entries)
- USE-conditional evaluation depends on correct profile flag state
- `virtual/perl-*` packages are pulled via `dev-lang/perl` PDEPEND behind
  `!minimal?` conditionals (correctly handled when `minimal` is disabled)
