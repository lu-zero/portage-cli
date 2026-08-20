# Absolute symlink rewrite (PMS 13.4.1)

Status: 🔴 not started. Related: [[pms-compliance]].

## PMS

Table 13.2: EAPI 0–8 rewrite any absolute symlink whose target starts with
`D`, stripping that prefix, and notice. EAPI 9 leaves targets unmodified.

## What `em` does

`walk_image` copies `readlink` unchanged (`portage-cli/src/ebuild.rs`).
EAPI 9 is correct; EAPI 8 (still most of the tree) is not. Compression
retargeting in `postprocess.rs` is a different rewrite.

## How to attack

1. `Eapi::rewrites_d_symlinks()` → `*self < Eapi::Nine`.
2. In `walk_image` (or just before), if the target is absolute and has `D`
   as a prefix, strip it and `einfo` a notice.
3. Tests: EAPI 8 `${D}/usr/bin/foo` → `/usr/bin/foo`; EAPI 9 unchanged.
