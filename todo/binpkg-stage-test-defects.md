# Four defects found by installing a stage from binary packages

Status: ✅ all four fixed 2026-10-09, each with a test. Found in sandbox
`em-binpkg-stage` (arm64): native `em toolchain --setup` and
`em stages --stage1` from source with `-b`, then the same into a second
root from packages. Context in [[phase-order-and-binpkg]].

| # | Defect | Symptom | Fix | Test |
|---|--------|---------|-----|------|
| 1 | The `Packages` index copied multi-line values (`DEPEND`) as they were; a blank line inside one ends the entry | sqlite and libpcre2: entry read back without `PATH`, "tar failed" on the package directory | `8a6b82e8`: every field folded onto one line | `index_roundtrips_a_written_gpkg` has a multi-line `DEPEND` |
| 2 | The list of configured variables that shields a package's saved ones was taken after `pkg_pretend` had sourced the ebuild; the ebuild was then sourced twice, which skips its eclasses | `app-alternatives/awk`: `USE flag 'mawk' not in IUSE_EFFECTIVE` in `pkg_postinst` | `2465f7d8`: list taken at unpack time, no second sourcing | the scratch repository now has a metadata cache (`3abf0bec`), so `a_package_built_with_a_merge_…` takes the real path; fails without the fix |
| 3 | The tar rename of image members (`./x` → `image/x`) also rewrote symlink targets | every relative link in a package corrupt: `etc/mtab -> image./proc/self/mounts`; ca-certificates' links removed as broken | `656be482`: rename limited to member names and hard link targets | `extract_image_roundtrip` has `../` and `./` links and a hard link |
| 4 | A package's build-env key was derived from the recorded `CFLAGS`, which the ebuild may have changed | glibc on arm64 (`-mbranch-protection=none`): "no matching binpkg" under `-K`, rebuilt from source under `-k` | the key of the *configured* flags is recorded as `BUILD_ENV_KEY` | `the_recorded_key_wins_over_the_flags_the_ebuild_left`; the group test checks the index field |

Checked live after the fixes:

- 1 and 2: the second root installs, both steps exit 0, 141 packages in
  each root with identical CONTENTS path lists.
- 3: baselayout and ca-certificates rebuilt and installed from the new
  packages, 297 symlinks identical to the source install.
- 4: glibc rebuilt with `-b`, listed with key `generic`, installed with
  `-K` into an empty root.

Not redone: the whole stage with all four fixes in one run. The
packages in the sandbox's `/root/pk` predate fixes 3 and 4.

Why the unit tests missed 2: without a metadata cache entry the
`pkg_pretend` check assumes the ebuild defines it and isolates the
phase, which happens to hide the late snapshot. Real trees have the
cache.
