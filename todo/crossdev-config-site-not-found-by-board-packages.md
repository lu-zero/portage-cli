# Board-destined native packages never find `config.site` — `CONFIG_SITE` is never exported

Status: ✅ fixed and live-verified 2026-08-29. This was exactly the
failure [[crossdev-config-site-embedded-library]] was supposed to
eliminate; every prior "it works" observation (including that item's
own "Live-verified" note) was riding on leftover sandbox state, not on
`em` actually wiring it correctly.

## The failure

`sys-apps/diffutils` (a plain native package, cross-built for the
board during `stages --stage1 --prefix P --root B --target T`) dies:

```
checking whether strcasecmp works... configure: error: cannot run test program while cross compiling
```

— the exact gnulib `AC_RUN_IFELSE`-without-cross-fallback case real
`crossdev`'s config.site cache-answer library exists specifically to
paper over.

## Root cause (confirmed)

1. `ensure_config_site_packages` correctly installs `sys-apps/config-site`
   + `sys-devel/crossdev` into the outer prefix `P`
   (`/root/prefix-riscv/usr/share/config.site`,
   `config.site.d/80crossdev.conf`, `crossdev/include/site/*` — all
   present, verified).
2. `diffutils`'s own `configure` invocation (from `config.log`):
   `--prefix=/usr` — **empty `EPREFIX`**, which is *correct*: `board-riscv`
   is a real target root filesystem, not a Gentoo-Prefix install, so
   packages built into it should get `EPREFIX=""`.
3. Autoconf's own `config.site` auto-discovery only ever checks
   `${--prefix value}/share/config.site` — i.e. literal `/usr/share/config.site`
   on whatever filesystem the build process sees. Since board packages
   correctly get `--prefix=/usr` (not prefix-anchored), that search can
   **never** reach `P/usr/share/config.site`, no matter how correctly
   step 1 populated it.
4. `em` never exports the `CONFIG_SITE` environment variable anywhere
   (`grep -rn CONFIG_SITE` across `portage-repo/src/build/` and
   `portage-cli/src/`: zero matches). Autoconf also honors `$CONFIG_SITE`
   directly (bypassing the `--prefix`-relative search entirely) — this
   is the mechanism em needs and doesn't use.

Every test that appeared to show this working (the x86_64 stage1 run
earlier today, and the original 2026-08-26 i586 verification recorded
in [[crossdev-config-site-embedded-library]]) reused a sandbox
(`em-i586-check`) whose **literal bare host** `/usr/share/config.site`
already existed from unrelated prior testing (dated 2026-08-28) —
accidentally satisfying autoconf's path-based search regardless of
`EPREFIX`. Confirmed by absence in a genuinely fresh sandbox
(`em-riscv-clean`): zero "loading site script" lines in diffutils'
build log at all.

## The fix (landed)

Two parts, both needed — the first alone was silently insufficient:

1. `portage-repo/src/build/shell.rs`'s per-phase env setup (right after
   `EROOT`) now sets `CONFIG_SITE` to `<build_broot>/usr/share/config.site`
   whenever `build_broot` is `Some` — the existing field already
   threading `Cli::host_roots()`'s merge root into every phase (used for
   BDEPEND PATH lookups); no new plumbing needed.
2. **`set_var` alone doesn't reach a real subprocess.** `run_phase`
   `export`s a fixed whitelist of variable names via a literal bash
   `export ...` command (`CATEGORY PN ... EPREFIX ED EROOT SYSROOT
   ESYSROOT BROOT ...`) so `configure`/`make` subprocesses inherit them
   — `CONFIG_SITE` wasn't in that list, so step 1's value was correctly
   *set* but never *exported*. First implementation only did step 1,
   confirmed insufficient by a live rerun (zero "loading site script"
   lines, identical failure) before adding `CONFIG_SITE` to the export
   list fixed it for real.

New tests: `config_site_points_at_build_broot_for_a_board_destined_package`
and `config_site_unset_without_a_build_broot` (`shell/tests.rs`). The
first also asserts real subprocess export via `export -p | grep -c
'^declare -x CONFIG_SITE='` (matching the existing `TERM`/`COLUMNS`
pattern) — checking `shell.get_var` alone would have missed the
export-list gap, exactly as it did during implementation.

**Live-verified**: riscv64, real (non-`-p`) `stages --stage1
--prefix P --root B --target riscv64-unknown-linux-gnu` in a genuinely
fresh sandbox — `diffutils`'s build log now shows `configure: loading
site script /root/prefix-riscv/usr/share/config.site` (+ the full
crossdev chain) and `checking whether strcasecmp works... (cached)
yes`. Full stage1 run completed: `EXIT=0`, all 102 packages, "stage1
ready in /root/board-riscv".

## Follow-up

Re-check every prior "config.site works" claim
([[crossdev-config-site-embedded-library]], `i586-full-run-findings.md`)
— they were never actually verified clean before this fix, though the
mechanism should now cover them too.
