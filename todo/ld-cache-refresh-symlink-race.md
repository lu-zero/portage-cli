# The post-merge ld cache refresh recreates soname symlinks under running builds

Status: ✅ fixed in `ldconfig` 0.2.0; `em` moved to it 2026-10-08 (see the
last section). Hit
while building host clang in a fresh crossdev-stages sandbox
(`em-riscv-clang-glibc`, arm64) with `em --jobs 6` on a 128-core host.

## Symptom

`media-libs/libjpeg-turbo` failed mid-compile, one of two builds running
while another package was being installed:

```
/usr/libexec/gcc/aarch64-unknown-linux-gnu/16/cc1: error while loading
shared libraries: libmpfr.so.6: cannot open shared object file
```

`dev-libs/mpfr` was not part of the merge. A serial rerun (`--jobs 1`) was
started to get past it.

## Evidence

- In the sandbox's `/usr/lib64`, 121 soname symlinks carry an mtime
  inside that run (120 in one minute), `libmpfr.so.6` among them. The
  libraries they point at are unchanged and months old.
- `maint/env.rs::refresh_ld_cache` rebuilds `ld.so.cache` through the
  `ldconfig` crate with `update_symlinks(true)`. I did not instrument it;
  the timing and the breadth (links for libraries nothing touched) make
  it the likely writer.
- The crate's link update removes the link and then creates it
  (`symlinks.rs`: `remove_file` followed by `symlink`). Between the two
  the soname does not exist, and any process starting then fails to load
  it.

## Why correct links were rewritten (established 2026-10-07)

`em` depends on the published `ldconfig` 0.1.1. Its
`symlinks.rs::should_create_symlink` reads the link's target, which is
relative (`libmpfr.so.6.2.2`), and calls `canonicalize()` on it. That
resolves against the process's current directory, not the link's own
directory, so it fails and the bare relative name is compared with the
library's absolute canonical path. They never match, so **every soname
link is removed and recreated on every refresh**.

The unreleased 0.2.0 in `~/Sources/ldconfig` ("Match glibc 2.43 scanning,
linking, and cache building") replaces this with a dev/ino comparison and
leaves a correct link alone, as glibc does. It still removes and then
creates when a link really has to change — which is also what glibc's own
`create_links` does.

## Not established

- That `refresh_ld_cache` was the writer in this run: inferred from the
  timing and from the 0.1.1 behaviour above, not instrumented.

## Fix directions

- Release `ldconfig` 0.2.0 and move `em` to it: correct links are left
  alone, which removes nearly every occurrence.
- In `ldconfig`, when a link must change, create the new one under a
  temporary name and `rename` it over the old, so the soname never
  disappears. Stricter than glibc, and cheap.
- Independently: `em` could run the refresh once per merge batch rather
  than after each package, which narrows the window but does not close it.

## Impact

Any parallel merge on a live root can fail spuriously in an unrelated
package, more often the more jobs run. It looks like a broken toolchain
and is not reproducible on retry.

## `em` moved to `ldconfig` 0.2.0 (2026-10-08)

0.2.0 is published with the atomic link replacement. Done:

- Workspace requirement `ldconfig = "0.2"`.
- `maint/env.rs::refresh_ld_cache` rewritten. 0.2 changed what the
  arguments mean: the config path is named *inside* the root, and
  directories stay as the target sees them. The old call passed the real
  path plus the root, which 0.2 resolves to a doubled path, finds nothing,
  and builds an **empty cache for any root other than `/`** without an
  error. Now `SearchPaths::from_file("/etc/ld.so.conf", Some(root))`
  followed by `.with_system()`.
- A test builds the cache of a scratch root and checks the entries are
  the in-root paths.

Not changed, on purpose: the two `from_file` calls that build an
`LD_LIBRARY_PATH` (`ebuild/mod.rs`, `active.rs`). Under 0.1.1 they also
picked up `/lib`, `/usr/lib`, … ; under 0.2 they return only what the
config names, which is what their own comments ask for. An earlier
version of this note said to add `.with_system()` there too; that was
wrong.

`ldconfig` 0.2.0 skipped a *relative* `include` under a root (and lost
every include when the root was written as `./dir`), so a root with the
usual `include ld.so.conf.d/*.conf` lost those directories. Fixed in
0.2.1; the workspace requirement is `ldconfig = "0.2.1"` so 0.2.0 cannot
be selected, and the scratch-root test uses a relative include.
