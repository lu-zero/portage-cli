# `stages --stage1` into a disposable board `--root` can't find the crossdev toolchain's own libc headers — FIXED

Status: ✅ fixed and live-verified 2026-08-27. Found running `em --target
i586-pc-linux-gnu --root /board-pentium-mmx stages --stage1` for the real
`pentium-mmx` board against an already-working i586 crossdev toolchain.

## History

This was originally introduced by `c877d10` (the `--root`/`--target`
board-root decoupling), which was reverted the same day for an unrelated,
confirmed regression it caused elsewhere (`Roots::build_sysroot()`
silently losing the toolchain sysroot). The revert also removed the
board-root feature entirely, making this specific bug briefly
unreproducible (moot) until the feature was redone properly below.

## Root cause

`Cli::roots()`'s bare `--target`+`--root` case set `Roots::base` to the
board root (`merge_target`) instead of the crossdev toolchain's own
sysroot. `Roots::build_sysroot()` returns `None` whenever `base ==
target` — which this made true for every board-root build — silently
dropping the toolchain sysroot from the compiler's `SYSROOT`/`ESYSROOT`
build context. `gcc` still ran (via its own baked-in `--with-sysroot`
default), but lost whatever else `em`'s `SYSROOT` env plumbing was
supposed to layer on top, breaking header resolution for object files
that route through the fuller include path (`zlib.h`'s
`<sys/types.h>`) while leaving the bare `zconf.h`-only ones unaffected.

The fix reintroduces the board-root decoupling, but keeps `base` as the
toolchain sysroot unconditionally — never `merge_target` — and instead
fixes the *installed-view* VDB-union problem (a fresh board's plan being
wrongly satisfied by the shared sysroot's own VDB, `c877d10`'s original
motivation) via `Roots::with_target_only_installed_view()`, an existing
mechanism added earlier for the exact same class of bug under native
toolchain bootstrap (`--local`/`--prefix`). This keeps `build_sysroot()`
correct and the VDB fix both, instead of trading one for the other.

## The fix

`portage-cli/src/cli.rs`, `Cli::roots()`:

```diff
-        let outer = self.outer_roots();
-        let eroot = outer.merge_root().to_owned();
-        let sysroot = eroot.join("usr").join(tuple);
-        Roots::default()
-            .with_config(Some(sysroot.clone()))
-            .with_base(Some(sysroot.clone()))
-            .with_target(Some(sysroot))
+        let outer = self.outer_roots();
+        let has_own_build_context = outer.eprefix().is_some();
+        let anchor = if has_own_build_context {
+            outer.merge_root().to_owned()
+        } else {
+            camino::Utf8PathBuf::from("/")
+        };
+        let sysroot = anchor.join("usr").join(tuple);
+        let merge_target = if has_own_build_context {
+            sysroot.clone()
+        } else {
+            opt_path(&self.root).unwrap_or_else(|| sysroot.clone())
+        };
+        Roots::default()
+            .with_config(Some(sysroot.clone()))
+            .with_base(Some(sysroot))
+            .with_target(Some(merge_target))
```

Plus, unchanged from `c877d10` (never the buggy part): `outer_roots()`'s
bare-target special case (keeps host-side `cross-*` tool refresh landing
on the real host `/`, not the board root) and
`require_explicit_root_under_target` (`crossdev/mod.rs`, bails if
`--target` is set without `--root` for `stages`).

`portage-cli/src/crossdev/mod.rs`, `run_stage1`/`run_stage3`: their
native-plan merge steps now pass `target_only_installed_view: true`
(was `false`) — a no-op everywhere `base == target` already (every
other topology), and the actual fix for the board-root VDB-union
problem here.

## Verified

- Full workspace `cargo test`/`clippy`/`fmt`: clean, including 3 new
  regression tests (`cross_targets_sysroot_under_eroot` now also
  asserts `build_sysroot()` stays `Some(sysroot)`,
  `outer_roots_ignores_bare_root_under_target`,
  `require_explicit_root_under_target_rejects_bare_target`).
- Live, real crossdev-stages sandbox, fresh release binary: `em --target
  i586-pc-linux-gnu --root /board-pentium-mmx stages --stage1` — the
  exact same `sys-libs/zlib-1.3.2-r1` objects that previously failed
  (`gzlib.o`, `gzread.o`, `gzwrite.o`, `uncompr.o`, `minigzip.o`,
  `example.o`) now compile cleanly with identical flags, `libz.so`
  links, package installs. Run continued 12 packages further (15/97,
  up from a dead stop at 2/97) before hitting an unrelated new bug —
  see [[crossdev-stage1-readline-ncursesw-pkgconfig]].
