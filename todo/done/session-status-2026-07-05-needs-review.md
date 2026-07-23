# Session status 2026-07-05 — flagged for independent review

Written at the user's request because today's session had several places
where conclusions were reached from indirect evidence (log greps, VDB
existence checks) rather than clean, unambiguous test runs, and at least
one direct methodological mistake (below). Everything in this file should
be treated as **unverified claims to double-check**, not settled fact.
Cross-reference `stage-build-shakeout.md` findings #22-37 for the full
narrative; this file is the "what might be wrong" companion to that.

## Sandbox / working state

- `/var/tmp/cross-stage1-riscv64` is a real, populated sysroot with a lot
  of state built up over many sessions. It is NOT disposable — do not
  `rm -rf` or reset it without checking with the user first.
- `todo/stage-build-shakeout.md` was committed today (commits `d93a114`
  and earlier ones referenced in its own text) with findings #34-37. This
  file (the review-flag file) is NOT committed as of this writing —
  intentionally, so the user can look it over first.

## Things claimed fixed today that deserve a fresh look

All committed with unit tests and `cargo fmt/clippy/test` passing at the
time, but re-verify the *reasoning*, not just that tests pass:

1. `--nodeps` now skips `preflight::check()` (`main.rs`, commit `13bb26d`).
   Rationale: `--nodeps` should mean "no dependency verification at all,"
   matching real emerge. Worth checking: does this let genuinely broken
   `--nodeps` merges through silently in cases where the user *didn't*
   intend to break a cycle? No test covers the "attacker/mistake" case,
   only the intended cycle-breaking case.
2. `unpack` no longer dies on an unrecognized archive extension
   (`unpack.rs`, commit `fa27567`). Verified against `dev-build/meson`'s
   real ebuild/eclass behavior, but the fix is broad (any unrecognized
   extension anywhere silently no-ops) — worth checking this doesn't mask
   *genuine* mistakes elsewhere (e.g., a typo'd SRC_URI filename that
   really should have unpacked but silently doesn't now).
3. `unpack`'s `./`-relative path resolution now uses the shell's tracked
   `cwd` instead of the Rust process's OS-level CWD (`unpack.rs`, commit
   `bc236f7`). Verified by reverting and watching a test fail, which is
   solid evidence the *test* checks the right thing — but only one
   concrete case (`dev-python/installer`) was checked live end-to-end.
4. The gawk↔bison↔gettext↔libxml2↔meson↔python cycle "isn't really a
   cycle at runtime" claim (gawk's shipped tarball allegedly has a
   pre-generated bison parser it doesn't regenerate) — confirmed by one
   live build succeeding with bison absent. This is a bet on upstream
   gawk's tarball contents holding across versions/timestamps; it worked
   once, but it's an empirical observation, not something enforced by
   code. If gawk's version bumps, this could silently break again with
   no guard-rail catching it.

## The manual VDB patch — flagged as a hack, needs scrutiny

`app-alternatives/gpg-1-r3`'s on-disk VDB `IUSE` file was directly
overwritten by hand (not through `em`) to unblock `app-crypt/gpgme`:

```
echo "nls ssl reference freepg sequoia" > \
  /var/tmp/cross-stage1-riscv64/var/db/pkg/app-alternatives/gpg-1-r3/IUSE
```

This was done with the user's explicit "yes please" after being blocked
once by the permission classifier. It is **not a code fix** — it directly
hand-edits on-disk package metadata outside of `em`'s own write path. The
real bug (`ebuild.rs`'s `iuse: env.iuse` sourcing from post-execution
shell state instead of the metadata cache — see finding #36) is still
open. Things worth checking:
- Does this hand-edited IUSE file actually match what a *correct* fix
  would have written, or could it be subtly wrong (e.g., flag ordering,
  a missing flag) in a way that causes a different problem downstream?
- Is this the *only* package in the sysroot affected by this bug class,
  or could other `app-alternatives/*` packages (or any eclass-injects-
  IUSE-then-ebuild-overwrites-it case) have the same silent VDB gap,
  undetected because nothing downstream happened to need the dropped
  flags?

## The exit-code confusion (a real process mistake, now understood)

Earlier in the session a background command was structured as:
```
em ... > logfile 2>&1
echo "exit=$?"
```
The task-completion notification said "exit code 0" even though the log
clearly showed real package failures. Root cause: `echo` always exits 0,
and with no `&&`/`set -e` between the two lines, the *sequence's* exit
status is the `echo`'s, not `em`'s — so the notification's "exit code 0"
never reflected `em`'s own status. This was worked around afterward by
writing `EM_EXIT=$?` *into the same redirected log file* instead of to
stdout, which does capture it correctly (confirmed working in later runs
in this session). But: **any exit-code reasoning from earlier in this
session (or prior sessions) that relied on the task-notification's
reported exit code rather than an explicit in-log marker should be
treated as unverified.** I don't have a full list of which past
conclusions may have been affected by this — it's worth an independent
grep through this session's/prior sessions' bash calls for the
`> file 2>&1 ; echo` pattern without an intervening `EM_EXIT=`-style
capture.

## "Stage1 is complete" conclusion — re-derive independently

Claim: `em stages --stage1` on `base_roots()` is fully complete, based on:
- A run reporting "4 of 53 failed to merge" (bzip2, xz-utils, gettext×2).
- Manually checking `find .../var/db/pkg/<cpn>-[0-9]* -maxdepth 0` for
  each of those 4 cpns and finding all 4 present.
- One config.log (`gettext-runtime/config.log` under the `.arm64`
  builddir) showing `configure: exit 0` at its tail despite the
  enclosing `build.log` recording a failure — interpreted as "a
  concurrent `--jobs 16` duplicate attempt overwrote the file after
  succeeding."

This is a *plausible* explanation but was not verified by, e.g., forcing
a clean single-`--jobs 1` re-run of just those packages from scratch and
confirming *zero* failures reported. The VDB-presence check proves the
packages ended up installed; it does not by itself prove the *specific*
reported failure was a race rather than some other transient issue that
happened to not matter. Recommend: re-verify by wiping just those 4
build dirs and re-running with `--jobs 1` (or reading through *all* the
interleaved per-package build.log timestamps for those 4 packages across
the whole run, not just the final state of one config.log) before fully
trusting this conclusion.

## Currently open / NOT resolved (in progress when interrupted)

Retrying the original target command:
```
em --autosolve-use --privilege pseudoroot --root /var/tmp/cross-stage1-riscv64 \
  --cross riscv64-unknown-linux-gnu --emptytree sys-apps/systemd-utils \
  --with-bdeps --keep-going --jobs 16 --buildpkg
```
Latest run (`systemd-utils-final2.log`): 99 of 101 merged, 2 failed:

1. **`sys-apps/systemd-utils-260.1-r1`** — meson `src_configure` fails:
   `Program python3 (jinja2) found: NO` /
   `ERROR: python3 is missing modules: jinja2`. Checked:
   `dev-python/jinja2-3.1.6` **is** installed in
   `/var/tmp/cross-stage1-riscv64/var/db/pkg/dev-python/jinja2-3.1.6`
   (the Host-flavor location), but **not** found under
   `/var/tmp/cross-stage1-riscv64/usr/riscv64-unknown-linux-gnu/var/db/pkg/`
   (the Target-flavor location). This smells like the same class of
   Host/Target root-conflation bug found earlier this session (#28/#30/
   #31/#32/#33) — systemd-utils is being configured as the Target
   (riscv64) flavor, and its BDEPEND python module was only installed
   for the Host (aarch64) python, not visible to whichever `python3`
   the Target-flavor `configure` actually invokes. **Not yet root-caused
   or fixed** — this is exactly where the session was interrupted.
2. **`app-text/opensp-1.5.2-r10`** — `make: -c: No such file or
   directory` / `Error 127` on target `config.h` at `Makefile:311`,
   inside `src_compile`. Not yet investigated at all beyond the raw
   error line — looks like a malformed `$(MAKE)` or `$(SHELL)` variable
   substitution (an empty variable leaving a bare `-c` as the "command"),
   but this is a guess, not confirmed by reading the actual Makefile
   rule or `em`'s env at that point.

## Recommended independent-review checklist

- [ ] Re-derive the "stage1 is clean" conclusion from a fresh, isolated
      run rather than trusting the VDB-presence spot check.
- [ ] Check whether the jinja2/opensp failures are both instances of the
      same Host/Target BDEPEND-visibility bug, or two unrelated issues.
- [ ] Review the manual VDB `IUSE` patch for correctness and check for
      other packages silently affected by the same eclass-IUSE-overwrite
      pattern.
- [ ] Spot-check the 4 commits from today (`13bb26d`, `fa27567`,
      `bc236f7`, plus the `preflight`/`--nodeps` one) for correctness
      independent of the reasoning written up in
      `stage-build-shakeout.md`.
- [ ] Grep this and prior sessions for the `echo "exit=$?"`-after-
      redirect pattern and flag any conclusion that leaned on a
      task-notification's reported exit code instead of an in-log
      marker.
