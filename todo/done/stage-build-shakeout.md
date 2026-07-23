# Stage-build shakeout (em --root @system, 2026-06-26)

First real `em toolchain --setup` → `em --root @system` into `/var/tmp/stage1-base`
on the 128-core box. Toolchain step: clean. `@system`: **148/163 merged**, 6
failures. The toolchain→@system sequence works (staging the toolchain first
clears the glibc↔gcc pre-flight cycle). Failure classes:

## FIXED — CBUILD unset → python configure "cross" (`50081f2`)

`dev-lang/python` died at configure: `Cross compiling required --host=HOST-TUPLE
and --build=ARCH`, with build==host==aarch64-unknown-linux-gnu. The host crossdev
`config.site` was a **red herring** (it gates on `CBUILD != CHOST`, a no-op when
CBUILD is unset). Real cause: em left **CBUILD unset**, so `econf` omits `--build`
(`${CBUILD:+--build=…}`), configure sees `--host` alone → `cross_compiling=maybe`
→ python's strict check dies. Portage defaults CBUILD to CHOST (`portageq envvar
CBUILD` = CHOST even with none in make.conf). Fixed: em sets `CBUILD=CHOST` when
unset (`shell.rs`). Verified: cpio's VDB env now has
`CBUILD="aarch64-unknown-linux-gnu"`.

## OPEN — `fowners` fails for root/other-user chowns (eselect, pam)

`die: fowners failed` in `src_install`. em's `fowners`
(`install.rs` `FownersCommand`) shells to the **host** `chown` with the owner
string verbatim. Two facets:

1. **Unprivileged chown (likely dominant).** The build runs as `lu_zero` under
   `~/.cache/em/build`; `chown root:shadow <file>` (pam's `unix_chkpwd`,
   eselect's files) → `EPERM` — a non-root user cannot chown to root. Portage
   handles this with `FEATURES=fakeroot`/userpriv handling (or a privileged
   merge). em has none, so any package that `fowners` to a foreign user fails.
   This will hit MANY packages, not just these two — it just happens these were
   the first in @system to fowners to root/other.
2. ✅ **Name resolution against the wrong root** (FIXED — facet 2,
   `907d914`). `fowners` now resolves `user[:group]` to numeric uid:gid against
   the target `<ESYSROOT|EROOT>/etc/{passwd,group}` (gated on an offset root) and
   chowns numerically, mirroring portage's `__resolve_owner`; the faking is the
   fakeroost session [[fakeroot-privilege-backends]]. Was: owner resolved against
   the **host** db, so a name absent on the host failed or chowned wrong.

Fix direction: resolve owner→uid:gid against `${ROOT}` (or `${EROOT}`)
passwd/group, and do the chown under fakeroot semantics (record ownership in the
image without real privilege) — i.e. a fakeroot-equivalent for the install
phase. Bigger than a one-liner; ties into [[build-clean-env]] (privilege/sandbox
model). The minimal hand-built stage1 didn't hit it because its packages
(glibc/bash/coreutils) fowners little; @system breadth exposes it.

## Transient-looking but actually 3 fetcher bugs

`popt`, `tar`, `psmisc` "could not be fetched" — NOT flakiness, three distinct
bugs. See [[distfile-fetch-reliability]] (investigating next):
- **popt**: `error decoding response body` on the upstream URL, **no Gentoo
  mirror fallback**.
- **tar**: `HTTP 404` on `alpha.gnu.org`, then `fetch: … ok` on a fallback — yet
  the package was **still marked failed** (success-after-fallback not registered).
- **psmisc**: a **truncated 139431-byte** file (expected 432208) cached in
  DISTDIR, fails manifest verify forever — **corrupt partial not discarded/refetched**.

## UPDATE 2026-06-26 — fixes landed, base at 160; the wall is privilege

After CBUILD (`50081f2`), fowners (`efdeb37`), and GENTOO_MIRRORS/make.globals
(`e0bae58`): re-ran `@system` into `/var/tmp/stage1-base` → **160 pkgs, python
built** (CBUILD validated end-to-end), pam/eselect/popt now merge. 3 of 70
remain, and they expose the boundary:

1. **util-linux — the fakeroot/privilege wall (blocks portage).** util-linux's
   *own* Makefile `install-exec-hook-mount` runs `chown root:root …/bin/mount`
   (setuid mount); unprivileged → `Operation not permitted`. This is **not** em's
   `fowners` (fixed) — it is the package's direct chown. portage RDEPENDs
   `sys-apps/util-linux`, so this blocks the self-extending base. A full `@system`
   stage with setuid binaries fundamentally needs **root or fakeroot**, exactly as
   catalyst runs stage builds as root. Options: (a) run `em` as root for stage
   builds (simplest, gives a real root-owned stage3); (b) integrate fakeroot
   (intercept/record chown unprivileged) — bigger, preserves the unprivileged
   model. The fowners fix only covers em's builtin; package-internal chowns need
   one of these. **This is the decision point for a real stage3.**
2. **bash — re-merge over a read-only file.** `copy image/usr/bin/bashbug →
   ROOT/usr/bin/bashbug: Permission denied`: the existing dest is mode 0555 (no
   write bit) and em's merge writes over it without `unlink`/chmod first. Portage
   unlinks before installing. Only bites on *re*-merge (a fresh root is fine).
   Clean fix: unlink (or chmod +w) the destination before overwriting.
3. **psmisc — fetch, two layered issues.** sourceforge returns a ~139 KB
   error/redirect page (not the tarball); the GENTOO_MIRRORS fallback now fires
   (the make.globals fix works) but builds the **flat** `distfiles/<file>` path,
   which 404s — modern mirrors use the **hashed** layout (`distfiles/<hash>/<file>`
   per the mirror `layout.conf`). See [[distfile-fetch-reliability]] — the mirror
   URL must honour the mirror layout, not assume flat.

Net: the unprivileged path reaches ~160/163; setuid/privileged packages
(util-linux) need root/fakeroot. For a real (root-owned) stage3, run `em` as root
— then `fowners` and Makefile chowns both work and the tree is properly owned.

## 2026-07-03 — resumed under pseudoroot: util-linux clears, two real findings

Resumed the same `/var/tmp/stage1-base` root's `@system` (67 pkgs) with
`--privilege pseudoroot` (v0.2.1, shipped 2026-07-03) to check whether the
util-linux privilege wall above is actually cleared now. **It is** — `sys-apps/
util-linux-2.42.1` merged clean unprivileged, no chown failures. 64/67 merged on
that pass.

**Self-inflicted false alarm (process hygiene, not a bug).** Mid-run I rebuilt
`target/release/em` (`cargo build --release`) while the background `@system` run
was using that exact binary. `spawn_install_worker`/`reexec` resolve the child via
`std::env::current_exe()` fresh at spawn time (`privilege.rs`), so a worker that
happened to spawn while cargo's linker was mid-replace of the file hit `pseudoroot:
failed to execute supervised command: No such file or directory (os error 2)` —
looked exactly like a pseudoroot bug, wasn't. **Lesson: never `cargo build
--release` the same binary a background `em` run is currently using — even for
an unrelated change.** Wait for the run to finish, or build to a different path.

**Real finding #1 — acct-group/acct-user stale VDB entries predate pseudoroot,
not a live bug.** `sys-apps/shadow`'s `fowners root:shadow` died: `invalid group
in /var/tmp/stage1-base/etc/group: :shadow`. Root cause chain:
- `acct-group.eclass`/`acct-user.eclass` **are already ROOT-aware** — `pkg_preinst`
  calls the real `groupadd`/`useradd` with `--prefix "${ROOT}"` when `ROOT` is set.
  em needs no shim here; nothing to build.
- But that same `pkg_preinst` gates on `[[ ${EUID} -ne 0 || -n ${EPREFIX} ]]` →
  `einfo "Insufficient privileges…"; return` — a **silent no-op**, not a die, not
  a failure the merge sees.
- `acct-group/shadow` (and 20 sibling acct-group/acct-user pkgs) in this test root
  were merged **2026-06-26 17:01**, a full week before the pseudoroot backend
  existed (`37e8d49`, 2026-07-02) — so `EUID` was the real unprivileged uid, the
  gate fired, group/user creation was skipped, and the VDB recorded a normal
  successful merge anyway (correct behaviour for that gate at the time — just
  stale data in *this* long-lived test root, not a reproducible bug against
  current em).
- Confirmed the fix is "re-merge, not code": `em --emptytree acct-group/shadow`
  under current pseudoroot → `* Adding group shadow` → written into
  `<root>/etc/group` correctly. Batch-re-merged all 21 acct-group + 5 acct-user
  pkgs in the root the same way; 20/27 landed clean this way (see finding #2 for
  the one that didn't).
- **Takeaway for future long-lived test roots**: any acct-group/acct-user package
  merged before a privilege backend existed (or under `--privilege none`) needs
  re-merging once a real backend is in place — its "installed" VDB state lies
  about whether the group/user actually exists on disk.

**Real finding #2 — ROOT-CAUSED: a `brush` process-substitution fd-lifecycle bug,
NOT pseudoroot/acct-user-specific.** Re-merging `acct-user/portage-0-r4` (already
installed → an `--emptytree` self-replace: `pkg_prerm`→`pkg_postrm` for the old
copy, then `pkg_preinst`→register→`pkg_postinst` for the new) hung indefinitely —
12+ min, 0% CPU, all 128 tokio worker threads parked (`futex_do_wait`/`ep_poll`,
genuinely idle). `build.log` showed every phase through `>>> pkg_postinst`
*starting*, nothing after; VDB registration already happened
(`counter=326` printed) — so the hang is strictly inside the `postinst` phase's
own execution.

Traced with `/proc/<pid>/fd` (no `strace` on this box): the worker process
(`em __worker`, pid 76946 in the reproduction) held **two** file descriptors
(11 and 15) open on the same pipe, and that pipe's *read* end was the stdin of
an orphaned `tee -a build.log` child (confirmed: `readlink /proc/76946/fd/11`
== `readlink /proc/<tee-pid>/fd/0` target). `tee` was blocked on `read()`
forever because the pipe's write end was never fully closed — the worker itself
still held it open.

**The construct responsible**: `EbuildShell::run_phase`
(`portage-repo/src/build/shell.rs:1698-1705`) builds, for every non-quiet phase,
```
{ func_name ; } > >(cd / && tee -a {log}) 2>&1
```
and `await`s it via `run_string`. Two things compound here: (1) `2>&1` duplicates
the process-substitution pipe's write end onto a second fd (stdout *and* stderr
both point at it — matches the 2 fds observed), and (2) a comment already in that
code (`"The process-sub body may be polled after the phase (and even after the
build tree is cleaned up)"`) shows a past session already knew brush's `>(...)`
completion is lazy/asynchronous and only patched *one* symptom of that (the
substituted `tee` starting from a deleted `${S}`) via the `cd /` hack — not the
underlying fd-closing gap. Somewhere in brush's handling of this compound command,
the write-end duplicates aren't both closed once the phase function returns, so
`tee` never sees EOF; whatever `run_string` awaits internally to consider the
command "done" apparently can't complete while that dangling reference exists,
so `run_phase`'s `.await` — and the whole worker — hangs.

**This is a `brush` bug (`~/Sources/brush`), not portage-cli merge logic**, and
it's *latent*, not new: any non-quiet phase invocation exercises this exact
construct. It almost certainly hasn't hung visibly before because (a) most phases
finish before/without straining whatever race window causes brush to fail to
close both fds, and (b) even when it *does* leak, if nothing downstream needs to
wait synchronously on that phase's completion signal in the same way, the orphan
`tee` is just silently left running in the background (reparented to init) rather
than blocking `em` itself — i.e. **this session's earlier "successful" merges may
have left orphaned `tee` processes behind unnoticed**; only this specific replace
(more phases run back-to-back in one shell → more chances to hit the race, and/or
more postinst output volume) surfaced it as a visible hang. Not proven
pseudoroot-specific — no evidence yet it's backend-dependent at all, since the
construct runs in the unwrapped brush shell regardless of privilege backend.

**Fix directions (not done today)**: (a) fix brush's `>(...)` + `2>&1` fd
lifecycle upstream — needs Luca's go-ahead per [[dont-commit-to-sibling-repos]];
or (b) stop depending on brush process substitution for phase dual-logging
entirely — spawn `tee` as a plain `std::process::Child` with `Stdio::piped()`
that portage-cli owns directly (explicit writer-closes-then-`.wait()`), removing
the dependency on brush's `>(...)` semantics for something that's purely
cosmetic console+file duplication. (b) is probably the more robust fix since it's
self-contained in portage-cli. **Before landing either fix, check for orphaned
`tee -a build.log` processes accumulated from earlier @system runs on this box**
(`pgrep -fa 'tee -a'`) — they may be harmless zombies-in-waiting, but worth a
sweep.

**Status:** `stage1-base` @system resume is paused here — 20/27 acct pkgs fixed,
`acct-user/portage` blocked on this hang, `@system` itself hasn't been resumed
since. [[fakeroot-privilege-backends]]

## 2026-07-03 (later) — hang ROOT-CAUSED for real and FIXED: tokio LIFO-slot
## stranding, not an fd-lifecycle leak

The fd-lifecycle theory above was wrong. Minimal repro (hangs deterministically
under unpatched brush, no em involved):

```bash
echo "res: $( { read -r x; echo got-$x; } < <( echo hi ) )"
```

**Any read-side process substitution inside a command substitution deadlocks.**
The acct-user trigger is `egetgroups`' `while read …; done < <( printf … | sort )`
running inside `old_groups=$(egetgroups …)` in `pkg_postinst` — before any
output, matching the empty log after `>>> pkg_postinst`.

Mechanism (three ingredients, all in brush):
1. `setup_process_substitution` (`brush-core/src/interp.rs`) runs the `<(…)`
   body via `tokio::spawn` and returns without ever awaiting/yielding.
2. Command substitutions execute their body as a *spawned task*
   (`invoke_command_in_subshell_and_get_output`), so inside `$( … )` the procsub
   spawn happens **on a tokio worker** — and a fresh spawn from a worker lands in
   that worker's **LIFO slot, which other workers cannot steal**.
3. The parent then blocks the same worker thread in a synchronous `read(2)` on
   the procsub pipe (`SharedPipeReader::poll_read` does blocking I/O; the `read`
   builtin's async path goes through it) without returning to the scheduler
   loop. The body task never gets its first poll → EOF never comes → deadlock,
   no matter how many workers are idle.

Verified thread picture on a hung repro: 129 threads = 1 in `anon_pipe_read`
(the stuck worker), 1 in `ep_poll`, 127 parked in futex — identical to the em
hang above (the single `anon_pipe_read` thread went unnoticed among 128).
Top-level scripts don't hang because `block_on`'s main future is not a worker
task: its spawns go to the global inject queue, so any worker picks the body up
(this is also why the write-side `> >(tee)` construct only ever produced *late*
tees, never a visible hang — phases yield via external commands).

**Fix (in ~/Sources/brush working tree, UNCOMMITTED per repo policy):**
`setup_process_substitution` made async + `tokio::task::yield_now().await`
after the spawn. The yield forces one trip through the scheduler loop, which
polls the LIFO slot (body gets its first poll; from then on its wakeups are
reactor-driven and stealable) and re-queues the parent at the stealable end of
the run queue. Chosen over a oneshot started-handshake because a same-worker
wake would park the *parent* in the LIFO slot — yield can't strand either side.

Verified: minimal repros + 50× egetgroups replay pass; brush compat suite
2240 cases 0 failed (one PTY job-control test flaky in full-suite runs,
pre-existing, passes in isolation with and without the patch); end-to-end
`em --root /var/tmp/stage1-base --config-root / --privilege pseudoroot
--emptytree -1 acct-user/portage` — the exact hang — merges clean in seconds,
`pkg_postinst` runs `usermod` ("Updating user portage"). Orphan-tee sweep:
none found. Remaining: Luca to review/commit the brush patch, push, bump the
`Cargo.toml` rev pin; then resume `@system`.

**`@system` resumed and DONE (2026-07-03).** Rebuilt the release binary against
the patched `for-portage-repo` worktree (`9baec193`), re-ran
`em --root /var/tmp/stage1-base --config-root / --privilege pseudoroot
--keep-going @system` for the remaining packages: **50/50 merged, 0 failures**,
no hangs, no orphaned `tee` processes afterward. The native
toolchain→stage1-base→`@system` pipeline under pseudoroot is now clean start to
finish on this box. Next real step is the actual `em stages --stage1` /
`packages.build` production path (see [[em-stages-and-binhosts]]) rather than
the ad-hoc full-`@system` proxy this shakeout has used throughout.

## 2026-07-03 — first cross-stage1 attempt: three from-scratch gaps found + fixed

Tried a genuinely fresh cross-stage1 (`em crossdev -t riscv64-unknown-linux-gnu
--setup --root /var/tmp/cross-stage1-riscv64 --privilege pseudoroot`, no reuse of
`~/.gentoo`) — the self-contained `--root` crossdev path (own empty VDB, no
host-shared libs) described in `todo/crossdev-target.md`'s design table had never
actually been exercised end-to-end. It wasn't ready:

1. **No `repos.conf`/no `gentoo` main-repo entry for a bare `--root` EPREFIX.**
   `--root DIR` retargets `config` (not just `base`/`target`) away from the host
   — unlike `--prefix`, which only offsets the install target and keeps config
   shared. `em crossdev --init-target`'s `main_repo()` only ever looked at the
   *target's own* `repos.conf`, which is empty on a truly fresh root, so it
   failed immediately with "no main repo configured". Fixed: `main_repo()` now
   falls back to the host's `repos.conf`, then to the hardcoded
   `/var/db/repos/gentoo` default (mirroring `Cli::repo_path`'s existing
   fallback). `ensure_repos_conf` now also writes a `gentoo.conf` entry into the
   EPREFIX's own `repos.conf` (not just the crossdev overlay) whenever
   `roots.config()` is `Some` (i.e. genuinely retargeted, not host-shared) — so
   subsequent builds resolve without needing a `--config-root /` workaround.
2. **No `make.profile` for a bare `--root` EPREFIX.** Same root cause: no host
   config sharing means no profile either. Fixed: `ensure_prefix_profile` links
   the EPREFIX's `make.profile` to the *host's* resolved profile (canonicalizing
   `/etc/portage/make.profile`) — the EPREFIX builds host-arch packages
   (the crossdev toolchain lands on `ROOT=/`-equivalent), so unlike the target
   sysroot (which links the *target* arch profile), it needs the host's own.
   No-op for `--local`/`--prefix` (already host-shared).
3. **Cross binutils kept `debuginfod` unconditionally**, assuming the cross
   EPREFIX is always host-rooted (deps pre-satisfied). A self-contained `--root`
   EPREFIX is exactly as empty as native's, so it hit the same explosion native
   already avoids (elfutils → curl → c-ares/nghttp2/nghttp3 → …, dozens of extra
   packages) — and needs the same missing bare-FS `baselayout` skeleton step
   native has, for the same `/usr/lib/../lib64` osdir reason. Fixed:
   `toolchain_plan` takes a new `self_contained: bool`; when true (a bare
   `--root` EPREFIX, detected via `roots.config().is_some()`), `Cross` now gets
   the same `baselayout` step + `-debuginfod` binutils USE that `Native` always
   had. Existing host-shared behaviour (`self_contained = false`) unchanged.

**Also observed, resolved as a side effect**: `die: ERROR: 23.0 merged-usr
profile, but disk is split-usr` (from `profiles/releases/23.0/profile.bashrc`)
fired repeatedly during the *first* (broken) attempt, once per package — but
did **not** actually stop the run; packages kept registering right after each
one. A 4th bug was found landing the fixes above: the `baselayout` StageStep
was still being cross-rewritten to `cross-<tuple>/baselayout` by `atom()`
(which unconditionally rewrites every component for `BootstrapKind::Cross`) —
but baselayout isn't part of the cross overlay's package set at all (only the
toolchain components are symlinked there), so it failed outright with "no
ebuilds found". Fixed: baselayout now always uses the literal
`sys-apps/baselayout` atom, bypassing the cross rewrite, for both `Native` and
self-contained `Cross`. Once baselayout actually ran (creating the real
`bin -> usr/bin` etc. merged-usr symlinks), the "merged-usr" die disappeared
entirely (0 occurrences on the clean re-run) — so it was never a separate
die-flag-propagation bug, just a faithful symptom of the missing skeleton.

**Status (2026-07-03)**: the four plumbing gaps above (repos.conf/profile/
baselayout-category/debuginfod) are fixed and verified — the retry got past all
of them cleanly (baselayout, binutils, os-headers, kernel-headers, libc-headers
all merged) and reached real compilation: `[6/8] gcc-stage1` (cross-riscv64
gcc-15.2.1's host-side build).

**5th finding — OPEN, architectural, not yet fixed.** `gcc-stage1`'s plan (11
packages) pulled in a full **`sys-libs/glibc-2.43-r2`** (host-arch, non-headers,
NOT the `cross-riscv64-…/glibc` already built for the target) — correctly: the
cross compiler binaries (`riscv64-unknown-linux-gnu-gcc`) are themselves
HOST-ARCH executables that need a working HOST libc to link against, and the
self-contained EPREFIX had none. The solver did the right thing; the EPREFIX
just doesn't have what it needs yet. The build then failed compiling
`libiberty/obstack.c`:
```
error: request for member 'extra' in something not a structure or union
error: unknown type name '_OBSTACK_SIZE_T'
```
— an `obstack.h` struct-layout mismatch. The actual compile command shows
`-I/var/tmp/cross-stage1-riscv64/usr/include` (the EPREFIX's own, just-built
glibc headers) listed **before** `-I…/libiberty/../include` (gcc's own bundled,
version-matched `obstack.h`), so the compiler picks up the freshly-built
glibc's copy instead of gcc's own — the two aren't ABI-compatible at this
combination of versions/build state.

Tried hypothesis 1 first (build a full native aarch64 toolchain into the same
EPREFIX before layering crossdev on top): `em toolchain --setup --root
/var/tmp/cross-stage1-riscv64 --privilege pseudoroot` ran baselayout→binutils→
kernel-headers→**full native glibc** cleanly, then hit the **exact same**
`libiberty/obstack.c` failure building plain `sys-devel/gcc` — **not** a cross
package, and with a toolchain that had *just* successfully built the glibc it
was choking on. That rules hypothesis 1 out completely: it was never about
needing a toolchain first.

**Root-caused (6th finding): `setup::bootstrap`'s own `--root`-mode bashrc was
the bug, and it's a regression from *this session's* earlier fix (finding 1
above).** Before finding 1, native `toolchain()`/crossdev `init_target()`
never called `setup::bootstrap` at all for a self-contained root (that's
*why* it needed the repos.conf/profile fix in the first place) — so no bashrc
file existed, and none of this ever fired. Adding `ensure_self_contained_prefix`
(which calls `setup::bootstrap`) fixed repos.conf/profile but, as a side
effect, *also* started writing `BASHRC_PREFIX` — which unconditionally exports
`CPPFLAGS="-I${ROOT}/usr/include …"` whenever `$ROOT` is set and non-`/`,
**with no distinction between "a `--prefix DIR` layered on a shared host base"
(what it was designed for — the host's own headers are already found by
normal search, so the prefix needs an explicit assist) and "a self-contained
`--root DIR` that IS the whole system"** (no such gap — SYSROOT/CHOST wiring
already resolves everything through the compiler's normal/cross search order).
For the self-contained case this extra `-I<ROOT>/usr/include` doesn't just do
nothing: it lands ahead of a package's own project-local `-I` flags (gcc's
`libiberty/../include`) and shadows the version-matched local `obstack.h` with
the ROOT's own ABI-mismatched one from its just-built glibc.

**Fixed**: `setup::bootstrap` now checks `roots.build_sysroot()` — `None` means
base == target (a genuine self-contained `--root`, no separate host base to
layer over) — and writes an **empty** bashrc there instead of `BASHRC_PREFIX`.
`--prefix DIR` (`build_sysroot()` is `Some`) and `--local` (its own
`BASHRC_LOCAL`) are unaffected. Two new tests in `setup.rs` lock in both sides.

**7th finding: no `MAKEOPTS` at all for a self-contained `--root`, so every
build defaulted to serial.** Retried with the bashrc fix — gcc's own build got
past `obstack.c` cleanly this time, but "taking way too long" turned out to be
real: `ps aux` showed a single `cc1plus` at a time on this 128-core box, over
an hour into gcc's full multi-stage bootstrap. Cause: `make_conf_template`
writes a purely commented placeholder (`# Profile and base make.conf come from
the host…`) — true for `--local`/`--prefix` (which share the host's real
make.conf, `MAKEOPTS="-j80 -l80"` on this box), but **false** for a
self-contained `--root`: its own `etc/portage/make.conf` is the *only* one
ever read, and it had no `MAKEOPTS` line at all, so every build (baselayout,
binutils, glibc, and this gcc bootstrap) had been running effectively `-j1`
the whole time. **Fixed**: `make_conf_template` takes the same
`self_contained` flag as the bashrc fix and, when true, writes a real
`MAKEOPTS` — the host's own value if readable (`MakeConf::load_default`),
else `-j<nproc>`. `--local`/`--prefix` keep the pure-comment template
(unaffected — they already inherit the host's real `MAKEOPTS`). Two new tests
lock this in.

**Native toolchain bootstrap CONFIRMED working from scratch (2026-07-03).**
With MAKEOPTS fixed, `em toolchain --setup --root /var/tmp/cross-stage1-riscv64
--privilege pseudoroot` ran all 5 steps clean (23 packages), and the resulting
`aarch64-unknown-linux-gnu-gcc` compiles and links a working executable
(verified directly). This is the first time a fully self-contained native
toolchain has been built from an empty `--root` end to end.

**8th finding: cross host-tool `ESYSROOT` only ever accounted for `--local`'s
`EPREFIX`, silently collapsing to the bare host path for a plain `--root`.**
Layering `em crossdev --setup --root <same dir>` on top of the now-working
native toolchain got through binutils/os-headers/kernel-headers/libc-headers
cleanly, then `gcc-stage1`'s own `libgcc` configure died: `cannot compute
suffix of object files: cannot compile`. The actual `./configure` invocation
showed `--with-sysroot=/usr/riscv64-unknown-linux-gnu` — a bare **host**
path, not `<our-root>/usr/riscv64-unknown-linux-gnu`. Traced to
`shell.rs::set_build_roots`'s cross-host-tool `ESYSROOT` special-case (from
the 2026-06-25 `~/.gentoo` cross bootstrap): `esysroot =
format!("{eprefix}/usr/{tuple}/")` — built from `eprefix` alone. `eprefix` is
only ever set for `--local` (Gentoo-Prefix); a plain `--root DIR` sets `ROOT`,
not `EPREFIX`, so `eprefix` was empty here and the whole expression collapsed
to the bare `/usr/<tuple>/` — the *host's own*, unrelated real crossdev
sysroot (which happens to exist on this box), not our test root's. The
build-tree `xgcc` then looked for target CRT/headers there instead of
`<our-root>/usr/riscv64-unknown-linux-gnu`, and libgcc's configure probe
couldn't compile.

**Fixed**: the cross-host-tool branch now builds from `root_str` (== `EROOT`,
already computed a few lines above and set to `ROOT+EPREFIX` universally) 
instead of bare `eprefix`. For `--local`, `root_str` == the eprefix path
already (identical result, no behaviour change — the 2026-06-25 fix stays
intact). For a plain `--root DIR`, `root_str` is the actual offset root, so
`ESYSROOT` now correctly resolves to `<DIR>/usr/<tuple>/`. No unit test added
(this function has no existing test scaffolding to extend — would need a full
synthetic `EbuildShell` + cross-category package fixture); validating via the
live cross bootstrap re-run instead.

**9th finding: the ESYSROOT fix (#8) was correct but incomplete — it doesn't
reach the actual failure, because `toolchain.eclass` computes its own
`PREFIX` from `EPREFIX` directly, bypassing ESYSROOT entirely for the cross
build path.** Re-ran with the ESYSROOT fix; `gcc-stage1`'s libgcc configure
failed at the *exact same point*, but the actual `--with-sysroot=` value was
now proven to come from a **different** eclass computation than the one
`ESYSROOT` feeds. Root cause, traced in `/var/db/repos/gentoo/eclass/
toolchain.eclass`:
- Line 274: `PREFIX=${TOOLCHAIN_PREFIX:-${EPREFIX}/usr}` — a top-level eclass
  variable, computed straight from the real `EPREFIX` env var (NOT ESYSROOT).
- For the `is_crosscompile` branch (which fires for `cross-<tuple>/gcc`),
  `--with-sysroot="${PREFIX}"/${CTARGET}` uses this `PREFIX`, and — this is
  the key structural fact — **the cross branch never emits
  `--with-build-sysroot` at all** (that flag only exists in the native/
  `else` branch, gated on `${ESYSROOT}`). So for a cross package there is no
  eclass-provided path back to ESYSROOT whatsoever; fixing ESYSROOT alone
  can't have touched this.
- On a **real, unprefixed Gentoo host** this is fine: `EPREFIX=""` →
  `PREFIX=/usr`, and `--with-sysroot=/usr/<CTARGET>` is *correct* because
  the whole crossdev bootstrap — kernel-headers, libc-headers, eventually
  gcc itself — installs everything to that same bare, unoffset path on the
  same real root. The freshly-built, not-yet-installed `xgcc`, invoked
  directly from its own build tree (not through a chroot) during its own
  `libgcc` configure, finds real content there because earlier steps put it
  there, unoffset, on the same filesystem.
- Our self-contained `--root DIR` breaks that assumption: earlier steps (
  linux-headers, libc-headers) installed into `<DIR>/usr/<CTARGET>` (correctly
  offset via `ROOT`), but gcc's own internal build computes its baked-in
  sysroot path from `EPREFIX` (empty, since only `--local` sets it) — so it
  looks at bare `/usr/<CTARGET>` on the host filesystem instead, which either
  doesn't exist or (worse, as here) is the *host's own separate, unrelated*
  real crossdev sysroot.

**Fixed, more substantially this time**: rather than patch this one flag,
`run_phase` (`shell.rs`) now treats a `cross-<tuple>/{binutils,gcc,gdb,
clang-crossdev-wrappers}` build as EPREFIX-style *regardless of `--local`* —
when `eprefix` is otherwise empty, it's set to `root_str` (and `ROOT`
correspondingly to `/`), mirroring exactly what `--local` already does for
every package. This is deliberately NOT a narrow flag patch: `EPREFIX` back-
feeds `PREFIX`/`--prefix`/`--with-sysroot` inside the eclass, AND determines
`ED` (`= D + EPREFIX`) — and DESTDIR+prefix is a *physical* install-path
convention (`make install DESTDIR=${D}` really writes under
`${D}${prefix}/...`), so whatever the eclass bakes into `--prefix` must also
be what our own merge step looks for inside the image. Flipping only
`ESYSROOT` (a pure DEPEND-resolution hint) could never have fixed this;
`EPREFIX`/`ROOT`/`ED` needed to move together, reusing the *already-correct*,
already-tested EPREFIX-subtree merge logic (`ebuild.rs::ed_image_dir`)
generically instead of inventing a new merge path for this one package class.

**Why this doesn't reopen the ESYSROOT/SYSROOT-doubling trap the #8 comment
warned about**: SYSROOT already equals `root_str` for a plain `--root` build
(unlike `--local`, where it's host `/`) — which is *already correct* for a
self-contained host toolchain, since it must link against the root's own
just-built native libc, not the real host's. ESYSROOT for this package class
is computed straight from `root_str`, independent of `eprefix` — so flipping
`eprefix` for `EPREFIX`/`ROOT`/`ED` does not double-count anything there.

**Left a structural note in the code** (`shell.rs`, right above the flip) for
next time this function needs touching: it derives six PMS location variables
(`ROOT`, `EPREFIX`, `ED`, `EROOT`, `SYSROOT`, `ESYSROOT`) through a chain of
locals, with two independent package-class special-cases (this one, and the
ESYSROOT one) that used to re-derive the same `category`/`pn` filter twice —
now unified into one `cross_host_tool_tuple`. If a third special-case ever
shows up, that function is worth extracting into a small `RootVars` value
type built by one function from `(category, pn, root_str, build_sysroot,
build_eprefix)`, so the cross-variable invariants (ED must track EPREFIX;
ESYSROOT must not double-count a flipped EPREFIX) are enforced in one place
instead of by convention scattered across a ~150-line function.

No unit test added for either shell.rs fix (#8 or #9) — this function has no
existing test scaffolding to extend (would need a full synthetic
`EbuildShell` + cross-category package fixture, a non-trivial harness this
file doesn't have precedent for). Validating both via the live from-scratch
cross bootstrap re-run instead; if this area gets touched again, building that
fixture is worth doing then rather than continuing to rely solely on live
runs.

**gcc-stage1 confirmed fixed**: re-ran with the EPREFIX/ROOT/ED fix —
`libgcc`'s configure now compiles successfully, and the plan advanced cleanly
to `[7/8] libc` (full glibc, built with the freshly-working stage1 compiler).

**10th finding: `<root>/usr/bin` was never on the build `PATH` for a
self-contained `--root`, so any package doing a live PATH-based tool lookup
for something this same root already installed silently failed.** `glibc`'s
own `pkg_setup` sanity check died: `linux-headers version too low!`, reporting
`(0.0.0 >= 3.2.0)`. `sys-libs/glibc`'s `get_kheader_version()` runs
`$(tc-getCPP ${CTARGET})` — a live PATH lookup for `riscv64-unknown-linux-gnu-
cpp` — and pipes a tiny `#include <linux/version.h>` probe through it. The
wrapper was verified to exist and resolve correctly on disk
(`<root>/usr/bin/riscv64-unknown-linux-gnu-cpp` →
`<root>/usr/aarch64-unknown-linux-gnu/riscv64-unknown-linux-gnu/gcc-bin/15/…`,
correctly `em select`-activated after the gcc-stage1 step) — the problem was
purely that `<root>/usr/bin` was never on `PATH` at all for this build, so the
lookup found nothing and `get_kheader_version` silently returned empty
(`tail -n 1` of no output), read as version `0.0.0` — not a missing-file
error, a *wrong-answer* one.

Why nothing else needed this until now: `--local`'s `BASHRC_LOCAL` already
adds `<EPREFIX>/usr/bin` to `PATH` (sourced per-phase from the config
overlay); the existing "cross-CC auto-export" `PATH` prepend
(`shell.rs::run_phase`, a few lines above the EPREFIX flip) only fires when
`CHOST != CBUILD` — which never happens for this whole staged bootstrap,
since the "cross" in a `cross-<tuple>/*` build lives entirely in `CTARGET`
(parsed by `toolchain.eclass`), not in `CHOST`/`CBUILD` (both stay the host
arch throughout `em crossdev --setup`/`em toolchain --setup`). Every earlier
step's own tool invocations were either absolute-path (gcc's own `-B` flags,
baked in at configure time) or didn't need a *live* PATH search for a
same-root tool at all — glibc's `tc-getCPP` is the first one that does.

**Fixed**: `run_phase` now unconditionally prepends `<root>/usr/bin` to
`PATH` when self-contained (`build_eprefix` and `build_sysroot` both `None`,
`root_str != "/"` — the identical `self_contained` condition used by the
`setup.rs` bashrc/make.conf fixes, finding #6/#7). Deliberately scoped to
self-contained only, not plain `--prefix`: a `--prefix` build already shares
a working host PATH, and unconditionally preferring the prefix's own
`usr/bin` there would be a new preference-order change with no reported gap
motivating it. Verified: `riscv64-unknown-linux-gnu-cpp` is now found and
`get_kheader_version` reads the real `6.18` from the just-installed headers.

No unit test added (same reasoning as #8/#9 — no existing `EbuildShell` test
fixture for a self-contained cross build; a stray full-suite run showed 11
transient failures on the first `cargo test -p portage-repo`, reproduced clean
on immediate retry — pre-existing parallel-test flakiness unrelated to this
change, most likely from `run_phase`'s process-global
`std::env::set_current_dir` racing across parallel test threads; not
chased further, but worth remembering if `portage-repo`'s test suite flakes
again).

**FULL CROSS TOOLCHAIN BOOTSTRAP COMPLETE, FROM SCRATCH, VERIFIED (2026-07-03).**
With the PATH fix, the retry sailed through glibc and `[8/8] gcc-stage2`
(the final, full compiler) and reported `>>> cross toolchain
riscv64-unknown-linux-gnu ready in /var/tmp/cross-stage1-riscv64/usr/
riscv64-unknown-linux-gnu`. Verified directly, not just trusting the exit
code: `riscv64-unknown-linux-gnu-gcc`/`-g++` both compile **and link** real
RISC-V executables (`file` confirms `ELF 64-bit LSB pie executable, UCB
RISC-V, RVC, double-float ABI`) for a plain C program and a C++ one
(`#include <iostream>`, exercising libstdc++ too). This is the first time a
*genuinely self-contained* cross toolchain — native host toolchain bootstrapped
first, cross toolchain layered on top, zero host-state sharing — has been
built end-to-end with `em`, on this or any prior session.

**Ten from-scratch gaps found and fixed this session**, all in service of this
one result: (1) no `repos.conf`/no `gentoo` main-repo entry for a bare `--root`
EPREFIX; (2) no `make.profile` for a bare `--root` EPREFIX; (3) cross binutils
unconditionally kept `debuginfod`, exploding the dependency closure; (4)
`baselayout` was wrongly cross-rewritten to a nonexistent overlay atom; (5)
missing `virtual/os-headers` merge for the EPREFIX itself; (6) `BASHRC_PREFIX`'s
CPPFLAGS injection broke self-contained builds (a regression introduced by
fix #1, within this same session); (7) no `MAKEOPTS` at all for a self-contained
root (silently serial builds); (8)+(9) cross-host-tool `ESYSROOT`/`EPREFIX`/
`ROOT`/`ED` only ever accounted for `--local`, not plain `--root`; (10)
`<root>/usr/bin` never on `PATH` for a self-contained root. All ten are
documented above with root cause, fix, and reasoning; #6 is a cautionary tale
worth remembering — a fix in this exact area introduced a regression that
took a second full cycle to catch, so changes here need the live re-test, not
just "it compiles."

**Cross-stage1 attempted for the first time — the `--cross` composition
"just worked" (2026-07-03).** With the toolchain solid, tried `em --root
/var/tmp/cross-stage1-riscv64 --cross riscv64-unknown-linux-gnu stages
--stage1 -p` — **zero new code needed**: `--cross`'s existing root-model
composition (config==base==target==the sysroot) plus `em stages --stage1`
(built earlier this session for the *native* case) combined correctly on the
first try. The dry-run plan is clean: `Root-aware cross plan: CHOST=
riscv64-unknown-linux-gnu CBUILD=aarch64-unknown-linux-gnu`, baselayout then
the riscv profile's own `packages.build` (~67 packages, `USE="-* build"`),
everything targeting `/var/tmp/cross-stage1-riscv64/usr/
riscv64-unknown-linux-gnu/` (the target sysroot, not the host or the EPREFIX).
This confirms the hypothesis from the start of this session's cross-stage1
work: the missing piece was never the CLI/plan composition (`em stages
--stage1` + `--cross`), it was the ten toolchain-bootstrap gaps above — once
a real toolchain exists in the root, cross-stage1 falls out for free.

**11th finding (a pre-existing bug, not part of the ten above): `packages`
removal never handled the `-*cat/pkg` form.** Resolving the riscv profile's
`packages` file (needed for `stage1_packages`'s version-qualification step)
hit `error: atom parse error: invalid dep: *sys-apps/busybox` — the very
first profile stack this codebase had ever loaded that uses this removal
syntax (`profiles/arch/riscv/packages` has `-*sys-apps/busybox`, removing
`default/linux`'s `*sys-apps/busybox` system add). `Profile::packages_raw`
(`portage-repo/src/repo/profile.rs`) stripped only the leading `-` before
parsing, leaving a bare `*` that `Dep::parse` doesn't understand. Per PMS, a
removal line echoes the *original* text of the addition it cancels (`*`
marker and all) — the marker doesn't change what gets removed (`Remove`
matches by dep identity regardless of whether the retained entry was
System or Plain). Fixed: strip an optional `*` after the `-` too. One new
test (`packages_removal_echoes_the_star_marker_of_the_add_it_cancels`).

**12th finding: `-j`/`-l`/`--keep-going` weren't `global = true`, so they were
rejected after any subcommand** — the exact same class of bug already fixed
once for `-p`/`-a`/`-D` (see `todo/crossdev-target.md`'s Stage-C notes:
"Also fixed: `-p`/`-a`/`-D` were not `global = true` in clap"), just never
hit for these three since nobody had tried `em stages --stage1 -j N
--keep-going` before. The execution path already threads them correctly —
`run_staged` (used by `stages`/`crossdev`/`toolchain`) calls the *same*
`emerge_atoms`/`emerge_atoms_inner`/`run_merge_plan` chain the default
no-subcommand flow uses, which already reads `cli.jobs`/`cli.keep_going` —
this is purely a clap argument-*position* gap (these flags work fine placed
*before* the subcommand name, e.g. `em -j 80 --keep-going stages --stage1`;
only *after* the subcommand name do they need `global = true` to parse).

**Tried `global = true` as the fix, reverted.** Marked `jobs`/`load_average`/
`keep_going`/`autounmask_write` `global = true` (matching the existing
`-p`/`-a`/`-D`/`--root`/`--cross` precedent) — but this is inconsistent with
how the *other* merge-behavior flags (`autounmask`, `autosolve_use`,
`buildpkg`, `usepkg`, …) are handled, and scatters `global = true` across many
individual fields on the monolithic `Cli` struct rather than grouping them.
Reverted per direction: these belong in a shared mixin struct (matching how
`DepgraphFlags` is already flattened into `ToolchainArgs`/`CrossdevArgs`/
`StagesArgs`), not sprinkled as individual global flags — a proper fix needs
to decide where that mixin lives and how `run_staged`/`emerge_atoms_inner`
read from it, which is real design work, not a one-line change. **Deferred**:
for now, place merge-behavior flags (`-j`, `-l`, `--keep-going`,
`--autounmask`, `--autounmask-write`, `--autosolve-use`, …) *before* the
subcommand name — that already works correctly today, no code change needed
for that ordering.

**Status**: the full cross-stage1 *plan* is now proven correct end-to-end,
for the first time, and `-j 80`/`--keep-going`/`--autosolve-use`/
`--autounmask-write` all work correctly when placed before the subcommand.
`--autosolve-use` correctly resolved the REQUIRED_USE conflicts (curl needing
`ssl`, util-linux's `su`↔`pam`, a cascading `ngtcp2[gnutls]` need).

**Blocked on a pre-existing, already-documented gap, not a new one**:
running the real build now hits exactly what `todo/PENDING.md` already
flagged — "packages.build DEPEND-into-ROOT residuals: `acct-group/root`,
`sys-fs/e2fsprogs`, util-linux ordering — re-test now that the DEPEND-trim
sysroot fix landed" ([[em-root-characterization]]). The pre-flight dependency
check reports `sys-apps/util-linux` needs `acct-group/root` and
`app-arch/libarchive` needs `sys-fs/e2fsprogs[…]` — neither present in the
resolved closure for this `--cross` target sysroot. This is the first time
that pre-existing gap has been reproduced at real scale (a full ~65-package
stage1 closure, not a single leaf package) — confirms it's still open, but
it's a distinct, pre-existing body of work (cross/ROOT-offset dependency-
closure correctness) from this session's ten toolchain-bootstrap fixes above,
not something to blindly extend into.

**12th finding, resolved: `MergeFlags` mixin, following the `DepgraphFlags`
precedent exactly.** Added `portage-cli/src/cli/merge_flags.rs`
(`#[derive(clap::Args)]`, 21 fields: `update`, `autounmask_write`, `oneshot`,
`fetchonly`, `buildpkg`, `usepkg`, `usepkgonly`, `getbinpkg`,
`getbinpkgonly`, `emptytree`, `tree`, `json`, `onlydeps`, `noreplace`,
`jobs`, `load_average`, `keep_going`, `autounmask`, `autosolve_use`,
`complete_graph`, `with_bdeps`, `exclude`), flattened both at the top-level
`Cli` (bare `em <atoms>` path) and into `ToolchainArgs`/`CrossdevArgs`/
`StagesArgs`. `crossdev/mod.rs` gained `merge_merge_flags(globals, args) ->
MergeFlags` mirroring the existing `merge_depgraph_flags` (bool fields OR'd,
`Option<T>` fields `.or()`'d, args wins), and `merge_depgraph_flags` itself
was generalized to take `&DepgraphFlags` instead of `&CrossdevArgs` so all
three call sites (`setup`/`toolchain`/`stage1`) can share it — `toolchain()`
and `stage1()` previously passed `args.depgraph_flags.clone()` straight
through with **no merge with the global copy at all**, a second latent
instance of the same position bug, fixed alongside this. `EmergeOpts` gained
a `merge_flags: Option<MergeFlags>` field (same override/fallback shape as
`depgraph_flags`), threaded through `emerge_atoms` → `emerge_atoms_inner`,
which now resolves `let merge_flags = merge_flags_override.as_ref()
.unwrap_or(&cli.merge_flags)` and reads every merge-behavior value off that
instead of `cli.X` directly.

**Important correction made mid-implementation**: initially wired
`equery depgraph`'s handler to read `globals.merge_flags.{emptytree,
autounmask_write, onlydeps, with_bdeps}` — i.e. reached into the full
merge-behavior mixin from a query-only command. Caught: `query::depgraph::
DepgraphOpts` (what that command actually calls) only ever consumes 7 of
the 21 `MergeFlags` fields (`empty`, `autounmask_write`, `autosolve_use`,
`onlydeps`, `with_bdeps`, plus `deep`/`nodeps` from elsewhere) — the other
14 (`buildpkg`, `usepkg`, `jobs`, `keep_going`, …) are meaningless for a
command that only resolves and prints, never merges. The `Depgraph` query
variant already had its own precedent for this: it declared a *local*
`autosolve_use` field and OR'd it with the global one
(`*autosolve_use || globals.autosolve_use`) rather than relying solely on
the global — one bespoke field per thing actually used, not a blanket
mixin. Fixed by giving the `Depgraph` variant its own local `emptytree`/
`onlydeps`/`with_bdeps` fields, each OR'd with the matching
`globals.merge_flags` field the same way `autosolve_use` already was.
Lesson for future mixin work: a mixin belongs on a consumer only if that
consumer actually reads most of its fields — a display-only command that
needs a handful of resolve-level knobs should declare exactly those, not
flatten the merge-behavior grab-bag "for convenience".

**Second correction, same review pass**: `autounmask_write` was in that
group too, and shouldn't have been — checked `query::depgraph::depgraph`'s
body (`portage-cli/src/query/depgraph/mod.rs:698,720`) and confirmed
`autounmask_write` genuinely writes `package.use`/mask/keyword fixes to
`<config_root>/etc/portage`, even when the caller is `equery depgraph`. A
read-only query command mutating host config as a side effect of `--help`-
adjacent flag is exactly the kind of thing that bites someone later (typo a
flag on a "just show me" command, get a surprise `/etc/portage` write).
Fixed: dropped `autounmask_write` from the `Depgraph` variant entirely and
hardcoded `autounmask_write: false` in its `DepgraphOpts` construction —
it still *reports* autounmask candidates (that path is unconditional,
independent of the flag), it just never persists them. The write-capable
`--autounmask-write` stays exposed only where a merge can actually follow
(bare `em`, `toolchain`, `crossdev`, `stages`). General principle for the
next mixin pass: check not just "does this consumer read the field" but
"does this consumer *write anything to disk*, and should a command of this
kind be allowed to."

Verified: `em -j 8 --keep-going stages --stage1 -p --root <dir>` and
`em stages --stage1 -j 8 --keep-going -p --root <dir>` now behave
identically regardless of flag position (both parse and reach the same
later failure/success point) — the bug that started this whole mixin
detour. Full workspace build + `cargo test -p portage-cli` (120 passed) +
`cargo test -p portage-repo` (124 passed + doctests) + `cargo fmt --check`
+ `cargo clippy --all-targets` all clean after the change.

**13th finding, resolved: `acct-group/root`/`sys-fs/e2fsprogs` missing from
the `--cross` stage1 plan — `--root-deps=rdeps` was permanently "on" for any
cross-arch target, when it should only apply during toolchain bootstrap.**
`sys-apps/util-linux`'s `DEPEND="${RDEPEND} virtual/os-headers
acct-group/root"` puts these two atoms in DEPEND *only* (not RDEPEND). Real
crossdev's `--root-deps=rdeps` (a documented work-around for the crossdev
bootstrap cycle: a still-empty target sysroot can't yet satisfy plain DEPEND
while its own toolchain is being built into it) makes the solver drop
DEPEND-only requirements for target-merge entries, keeping only RDEPEND.
`root_aware::detect()`/`CrossContext::root_deps_rdeps(host_arch)` derived
this purely from the sysroot's own `CHOST`/`CBUILD` (`--cross` sets
`CBUILD=<host>` permanently in the sysroot's `make.conf`, by design — real
crossdev sysroots keep that forever), so the exemption stayed "on" for
*any* package resolved against that sysroot, indefinitely — not just during
the toolchain build. Caught by the user's framing: "cross has special ways
to build the cross sysroot and toolchain, but to build a full stage1 they
aren't really needed since a cross stage1 is a normal stage1 with a
different compiler" — i.e. once the toolchain exists, ordinary stage1
packages (util-linux, e2fsprogs, acct-group/root, …) should get *full*
DEPEND resolution against the target, same as a native build; only the
toolchain-into-empty-sysroot bootstrap itself has the cycle problem
`--root-deps=rdeps` works around.

Fixed by making `--root-deps=rdeps` an explicit, caller-supplied input
instead of something auto-derived from `CHOST`/`CBUILD`:
- Removed `CrossContext::root_deps_rdeps()` entirely (dead code, single
  caller) — replaced `provider.set_root_deps_rdeps(cross.root_deps_rdeps(arch))`
  in `query::depgraph::depgraph()` with a plain `root_deps_rdeps: bool` field
  on `DepgraphOpts`, supplied by the caller.
- Added `--root-deps` to the `MergeFlags` mixin (mirroring real emerge's
  `--root-deps[=rdeps]`, modelled as a plain boolean since `rdeps` is the
  only value that ever exists) — per the CLI-mixin-scoping lesson above,
  *not* a plain global field.
- `em crossdev --setup`'s `setup()` forces `merge_flags.root_deps = true`
  unconditionally after computing the merged flags — matching real
  crossdev's `<CTARGET>-emerge` wrapper, which always implies the flag; not
  user-togglable there.
- `em toolchain --setup`, `em stages --stage1`, the bare `em <atoms>` path,
  and `equery depgraph` all default to `false` (full DEPEND resolution),
  overridable per-invocation via `--root-deps` if ever needed (e.g. to
  reproduce a similar bootstrap-cycle problem outside the crossdev flow).

Command → default table:
| command | `--root-deps` default | why |
|---|---|---|
| `em <atoms>` | off | ordinary install, not bootstrap |
| `em crossdev --setup` | **on**, forced | building the toolchain + glibc into a still-empty target |
| `em toolchain --setup` | off (moot) | native, `CHOST==CBUILD` always |
| `em stages --stage1` | off | toolchain already exists; ordinary packages |
| `equery depgraph` | off | display-only |

Verified: `em -p --root <dir> --cross riscv64-unknown-linux-gnu --autosolve-use
app-arch/libarchive sys-apps/util-linux` now resolves 34 packages (was 30) —
`acct-group/root` and `sys-fs/e2fsprogs` present. Passing `--root-deps`
explicitly reproduces the old (30-package) behavior, confirming the override
works both ways. Note: `-p`/`--pretend` never reaches `preflight::check`
at all (`emerge_atoms_inner` returns right after the depgraph exit-code
check, before `preflight::check` is even called) — so a clean `-p` run only
proves the *plan* is right, not that preflight agrees. See the real-run
follow-up below.

**14th finding: confirmed with an actual (non-pretend) run — the
`--root-deps` fix is correct and complete, but two more, pre-existing bugs
block the real build.** Ran `em --autosolve-use --keep-going -v --root
/var/tmp/cross-stage1-riscv64 --cross riscv64-unknown-linux-gnu stages
--stage1` for real (no `-p`) against the same riscv64 target. It failed at
`preflight::check` (before any package actually built) with 5 "needs:"
lines. `diff`'d against the very first pre-session capture of this exact
command (`cross-stage1-riscv64-stage1d.log`): the **only** change is
`sys-apps/util-linux-2.42.1 needs: acct-group/root` is gone (the fix), and a
new-looking `sys-fs/e2fsprogs-1.47.4 needs: sys-apps/util-linux` line
appeared — not a regression, just e2fsprogs's own pre-existing dependency
becoming visible for the first time now that e2fsprogs is finally in the
plan at all (before the fix it was silently dropped, so its deps were never
even checked). The other three lines were already present in the original
capture, untouched by any of today's work:
- `app-arch/libarchive-3.8.7 needs: sys-fs/e2fsprogs[abi_x86_32(-)?,…]`
- `sys-libs/libxcrypt-4.4.38-r1 needs: sys-libs/glibc[-crypt(-)]`
- `sys-devel/gcc-16.1.1_p20260606 needs: sys-libs/glibc[cet(-)?]`

Filed as a **very important, pending** blocker in `todo/PENDING.md` (top of
the stage-building section) rather than fixed here — two distinct bugs:
1. USE-dep conditional-default syntax (`flag(-)?`/`flag(+)?`/`-flag(-)`,
   EAPI 7+) not evaluated correctly — riscv64 lacks `abi_x86_*`/`crypt`/`cet`
   in IUSE entirely, so these should trivially pass regardless of arch.
2. `sys-apps/util-linux` install-order bug — both `e2fsprogs` and `python`
   DEPEND on it, but the solver places it *after* both (line 170 vs. 166/169
   in the plan) — a real topological-sort/edge-registration gap, not a
   preflight false positive.

Task #8 (`em stages --stage1` against a real `--cross` target) is now
blocked on these two, not on `--root-deps`/`acct-group/root` (that part is
done). Full command + log: `todo/PENDING.md`'s stage-building section,
top entry.

**15th finding, resolved: finding #14's "util-linux install-order bug"
traced and fixed — it was never a real cycle.** Checked directly with real
portage (`qdepends`/`equery` on this host): `sys-apps/util-linux`'s real
ebuild only depends on `dev-lang/python` behind `python? ( ${PYTHON_DEPS} )`
— with `python` off (as it is in this `USE="-* build"` stage1 set), that
dependency legitimately does not apply. `dev-lang/python`'s own dependency
on `sys-apps/util-linux` is unconditional and one-way. So there is no real
cycle at all in the actual Gentoo dependency graph; em was fabricating one.

Root-caused with a from-scratch `portage-atom-pubgrub` test
(`scratch_two_cycles_chained_plus_dependents`, since removed — it didn't
reproduce, which is what narrowed this down) plus reading the actual
`em --json` edge dump for the real riscv64 plan: em's `--json`/`--tree`
output showed a phantom `DEPEND sys-apps/util-linux -> dev-lang/python`
edge that shouldn't exist. Traced to two compounding bugs:

1. **The real bug**: `cede_required_use`
   (`portage-cli/src/query/depgraph/repo.rs`) — Level-C `--autosolve-use`
   ceding of REQUIRED_USE flags to the solver. util-linux's own
   `REQUIRED_USE="python? ( ${PYTHON_REQUIRED_USE} ) su? ( pam )"` has two
   *independent* top-level clauses; only `su? ( pam )` was violated here
   (confirmed by the `--autosolve-use` report: `-su … because: su? ( pam )`).
   But the code checked `ru.unsatisfied(&enabled).is_empty()` (correctly,
   whole-expression) and then called `collect_required_use_flags(ru, …)` on
   the **whole** `ru` tree regardless — collecting `python` too, even though
   `python? ( … )` was independently satisfied (python off, vacuously true).
   That needlessly ceded `python` to the solver (`SolverDecided`) as if it
   were genuinely undecided, turning it into a solver-owned virtual
   choice node purely as a side effect of the *unrelated* `su`/`pam`
   violation.
2. A latent, secondary issue in `dependency_graph()`'s virtual-choice-node
   expansion (`portage-atom-pubgrub/src/graph.rs`): once a flag is ceded, a
   synthetic two-version "choice" package models on/off, and edge extraction
   walks *every* version of that choice node (`vdata.versions.values()`)
   rather than only the one actually selected in `solution` — so even a
   correctly-ceded flag whose "off" branch was chosen can still leak the
   "on" branch's dependencies into the graph. Not fixed (bug 1 alone
   resolves the observed case — no ceding, no virtual node, no phantom
   edge — but this is worth revisiting if a *genuinely* ceded flag ever
   shows the same symptom).

Fix: `ru.unsatisfied(&enabled)` (already computed, previously only checked
for emptiness) now drives which clauses get scanned — `collect_required_use_flags`
is called per violated clause, not on the whole tree. One new regression
test, `independently_satisfied_clause_is_not_ceded_by_an_unrelated_violation`
(`portage-cli/src/query/depgraph/repo.rs`), confirmed to fail on the old
code and pass on the fix (verified by hand-reverting the fix line, temporarily under a "TEMP" edit, retested, then restored).

Verified against the real riscv64 target: the phantom `util-linux -> python`
edge is gone from `--json` output (only the real one-way `python -> util-linux`
edge remains); install order is now correct (`util-linux` before `e2fsprogs`
before `python` before `glibc`/`gcc`/`libarchive`/`libxcrypt`); the
`su`/`pam` REQUIRED_USE flip (the genuinely violated clause) still reports
correctly, confirming the fix doesn't suppress real ceding. `em
--autosolve-use --keep-going stages --stage1 --root <dir> --cross
riscv64-unknown-linux-gnu` (no `-p`) now passes `preflight::check`
entirely and starts building for real (gcc configure/compile underway) —
first time this has gotten past pre-flight for a real (non-pretend) run.

**Still open, unresolved**: the USE-dep conditional-default syntax question
(`flag(-)?`/`flag(+)?`/`-flag(-)`) from finding #14 — `libarchive needs:
e2fsprogs[abi_x86_32(-)?,…]`, `libxcrypt needs: glibc[-crypt(-)]`, `gcc
needs: glibc[cet(-)?]` — turned out to be a **non-issue**, not a bug:
`Dep::matches_cpv` deliberately never evaluates USE-dep brackets at all
(by design — see its doc comment, "answers the has_version-style question");
`preflight::check`'s `Avail::atom_satisfied` only calls `matches_cpv`, so
these three lines were always just an artifact of `util-linux` (and
therefore everything chained off it) being mis-ordered by the phantom-edge
bug — once ordering is correct, these self-resolve (confirmed: they no
longer appear in the real run's failures, because there are no failures —
preflight now passes clean). No further work needed on USE-dep conditional
parsing for this issue.

**16th finding, resolved: with preflight fixed, the real (`--keep-going`)
build ran to completion and hit a *third*, distinct bug — 38/127 merged,
89 failed.** Failures clustered into a handful of systemic categories, not
89 independent ones: ~23 unrelated packages' own patches all failing
`eapply`, 12 `econf` exit-77s, 8 `eltpatch` failures, 5 `emake` failures, 3
`aclocal` failures, 1 `eprefixify` failure. The common thread, found by
reading one actual failing build log instead of just the summary line:

```
/usr/bin/eltpatch: line 220: /var/tmp/cross-stage1-riscv64/usr/riscv64-unknown-linux-gnu/usr/bin/sed: cannot execute binary file: Exec format error
```

`eltpatch` (and `eapply`/`aclocal`/etc., all HOST-side build tools that run
*during* the build, not part of what's being installed) was finding and
trying to execute the **target sysroot's own, just-merged riscv64 `sed`**
— binaries that cannot run on this (host-arch) machine at all. User's
question ("crossdev emerge doesn't have this problem, shall we look in
detail why") was the right prompt: checked what `eprefixify`/`eltpatch`
actually are (`eprefixify` is Prefix-specific, in `prefix.eclass`;
`eltpatch` is a normal, universal `app-portage/elt-patches` tool, not
Prefix-specific — an assumption worth correcting) and traced the actual
PATH contents at failure time.

Root cause: an earlier fix *this same session* (`todo/stage-build-shakeout.md`
finding on ESYSROOT/EPREFIX/PATH, `portage-repo/src/build/shell.rs`)
unconditionally prepended `<root_str>usr/bin` to `PATH` for any
self-contained `--root` build, reasoning only about the case where
`root_str` is the top-level EROOT (host-arch-executable — the motivating
case was glibc's `tc-getCPP ${CTARGET}` needing the EROOT's own
`<CTARGET>-cpp` wrapper during `em crossdev --setup`'s toolchain
bootstrap). It didn't account for `em stages --stage1 --cross`'s *ordinary*
packages, whose own `root_str` **is** the foreign-arch target sysroot
itself (`<EROOT>/usr/<tuple>/`) — for those, `<root_str>usr/bin` contains
riscv64 binaries, and prepending it ahead of the host's real `/usr/bin`
broke every host-side tool invocation in that package's own build.

Fixed by hoisting the already-existing `cross_host_tool_tuple` signal
(category + package name — the same one gating the EPREFIX/ESYSROOT flip a
few lines below, previously computed twice, now once) earlier in
`run_phase`, and gating the PATH-prepend on `self.build_config_root.is_none()
|| cross_host_tool_tuple.is_some()` — i.e. either no `--cross` is active at
all (the plain self-contained-native-root case, where `root_str` really is
host-arch), or this specific package really is one of the host-side
toolchain tools (whose `root_str` really is the EROOT). Ordinary packages
under an active `--cross` no longer get the prepend at all.

Verified: `net-dns/c-ares` and `sys-apps/attr` (both previously failed with
this exact `eltpatch`/`sed` Exec-format error) now build and merge cleanly
against the riscv64 `--cross` target. Full workspace build/tests/fmt/clippy
clean. Not yet re-run: the full 127-package `--keep-going` stage1 build,
to see how many of the other 89 failures this alone clears versus what
remains (the `econf` exit-77s and `aclocal` failures may be a separate,
fourth issue — not yet investigated).
[[em-stages-and-binhosts]] [[crossdev-target]] [[em-root-characterization]]

## 17. `CTARGET` leaking sysroot-wide into every package's `econf` (fixed)

Re-ran the full riscv64 `--cross` stage1 build with the PATH fix in place.
New failure: `dev-db/sqlite-3.53.2-r1`'s `src_configure` died with
`econf failed (configure exited 1)` — sqlite's own custom (non-autoconf)
`configure` script rejected `--target=riscv64-unknown-linux-gnu` outright
(`Error: Unknown option --target`).

`em`'s `econf` builtin (`portage-repo/src/build/commands/econf.rs`)
unconditionally appends `--target=$CTARGET` whenever `CTARGET` is non-empty —
which matches real portage's own `econf` exactly
(`${CTARGET:+--target=${CTARGET}}` in
`/usr/lib/portage/python3.13/phase-helpers.sh`), so this was never an
econf-logic bug. The real question was *why* `CTARGET` was set at all for an
ordinary package build.

Root cause: `em crossdev`'s generated target `make.conf`
(`portage-cli/src/crossdev/mod.rs` `make_conf_body`) wrote
`CTARGET=<tuple>` unconditionally into the **sysroot-wide** make.conf, so
every ordinary package built into that sysroot inherited it — not just the
host-side cross-toolchain packages. Checked real crossdev's own template
(`/usr/share/crossdev/etc/portage/make.conf`) directly: it sets `CHOST`/
`CBUILD` only, never `CTARGET`. `CTARGET` there only ever applies to the
`cross-<CTARGET>/{binutils,gcc,gdb,linux-headers,glibc}` builds, scoped via
`package.env` (`write_cross_env`, same file, already correct) and read by
`toolchain.eclass` off `CATEGORY`.

Fixed by deleting the stray `CTARGET={tuple}` line from `make_conf_body`.
Added a regression test (`crossdev::tests::make_conf_body_never_sets_ctarget`)
asserting the generated body never contains a `CTARGET=` line. Patched the
already-generated make.conf in the live test sysroot by hand (new sysroots
created after this fix won't need it).

Along the way, made two of the same mistake this session: raced a manual
`em ... dev-db/sqlite` test invocation against the still-running
`--keep-going` stage1 build (same ROOT — caught and killed before real
damage), then rebuilt `target/release/em` via `cargo build --release`
*while that same long-lived run was still executing*. `spawn_install_worker`
(`portage-cli/src/privilege.rs`) re-execs `current_exe()` fresh per package,
not once at startup, so overwriting the binary mid-run tore the in-flight
packages' install-worker spawns and produced misleading
`install worker exited with status 127` failures on `sys-devel/gettext`,
`dev-libs/gmp`, `dev-libs/nettle` — which briefly looked like a real
regression before mtime correlation (binary replaced at T, next package's
`build.log` written at T+15s) proved it was self-inflicted, not a code bug.
Lesson: always `pgrep -af "target/release/em"` before rebuilding the binary
or launching a second `em` invocation against the same ROOT.

Noted but deferred (task #11): `em` only has a merge-phase flock
(`lock_merge_flock` in `portage-cli/src/ebuild.rs`, serializing the
qmerge/VDB-write critical section across concurrent `em` processes sharing a
build tree) — unlike real portage, which takes a lock for the *whole run*
scoped to the config root, so a second `emerge` just waits/refuses up front.
Worth adding a whole-invocation flock later.

Not yet re-run after this fix: the full 127-package `--keep-going` stage1
build, to see how much of the remaining failure count (the `econf` exit-77
cluster, `aclocal` failures, and whatever else) this alone clears.
[[em-stages-and-binhosts]] [[crossdev-target]] [[em-root-characterization]]

## 18. `CHOST`/profile vars invisible to real subprocesses — allow-list → sourced-env sweep (fixed)

Re-ran the fixed stage1 build; `dev-libs/openssl` failed differently:
`Configuring OpenSSL version 3.6.3 for target linux-aarch64` (should be
`linux64-riscv64`), then real compile errors — `crypto/aes/aes-sha1-armv8.S`
(ARM64-only assembly) assembled with the *correct* riscv64 cross-`gcc`
(`-mabi=lp64d -march=rv64gc` etc.), producing "unrecognized opcode" errors.
Mismatch: right compiler, wrong `Configure` target.

Root cause: `dev-libs/openssl`'s `src_configure` runs `bash
"${FILESDIR}/gentoo.config"` — a genuine external subprocess, not an em Rust
builtin — to map `$CHOST` to an OpenSSL `Configure` target string. `em`'s
`init_build_env` (`portage-repo/src/build/shell.rs`) only ever exported a
hand-maintained list of names (`CATEGORY PN PV ... MOPREFIX ABI
CONF_LIBDIR`) to real subprocesses; `CHOST` (and `CBUILD`/`CTARGET`/`ARCH`)
were never on it. Invisible to `gentoo.config`, `CHOST` read empty there, so
`sslout` was empty and OpenSSL's own `Configure` fell back to `uname`-based
autodetection — correctly identifying the **build host's** real aarch64
kernel, since that's genuinely what `uname -m` reports on this machine.
Meanwhile `CC`/`CFLAGS` were already correct because the `econf` Rust builtin
forwards them explicitly, bypassing brush's export mechanism entirely — the
same asymmetry (Rust builtins read brush's variable table in-process; real
subprocesses only inherit exported vars) that made this invisible until a
package's build script needed a raw subprocess.

User's follow-up ("that list is used in other places? feels brittle, what
does portage do?") reframed this from "add 4 missing names" to "the
allow-list model itself is wrong": every other profile-derived variable
(`ELIBC`, `KERNEL`, `USERLAND`, `MULTILIB_ABIS`, `DEFAULT_ABI`,
`PKG_CONFIG_PATH`, …) has the identical latent bug. Checked real portage's
`config.environ()`: not an allow-list — it exports its *entire* settings
dict minus a small internal denylist, because portage builds the whole
OS-level process environment before the ebuild's bash even starts.

Fixed properly (not just patched): `EbuildShell::export_sourced_env`
(`portage-repo/src/build/shell.rs`) exports **every** variable currently in
the shell's environment (brush's `Env::iter()`, all vars regardless of
export flag) minus a small bash-mechanics denylist (`is_bash_internal_var`),
flipping each variable's export bit directly via brush's
`ResolvedVarRefMut::base_var_mut` — no generated `export a b c ...` string
round-tripped through the interpreter. Called from `apply_profile_env`
(`portage-cli/src/ebuild.rs`) right after profile/make.conf sourcing and
again after package.env sourcing. `init_build_env`'s original identity-var
list is untouched — those are em-synthesized per-package values (CATEGORY,
PF, S, T, D, …), not sourced from a file, so they can't come from the sweep
and still need their own explicit export.

New regression test: `export_sourced_env_reaches_a_real_subprocess`
(`shell.rs`) — sets an arbitrary var (`MULTILIB_ABIS`, standing in for any
profile var em doesn't specifically know about), calls
`export_sourced_env`, then spawns a *real* external subprocess via command
substitution and asserts it inherited the value. Verified it fails without
the fix (manually reverted the function body, confirmed the assertion
failed, restored it). Documented the architecture choice in
`docs/build-environment.md`.

Verified live: `dev-libs/openssl` now configures for `linux64-riscv64` and
merges cleanly into the riscv64 `--cross` sysroot (the pre-existing,
unrelated `error: command not found: diropts` during post-install is
non-fatal — `diropts` is a real portage ebuild-helper em never implemented;
noted as a separate, minor gap, not chased further here).

Also this round: made the "don't race a running `em` process" mistake a
second time (launched a manual single-package test against the same ROOT
while the `--keep-going` run was still live) — generalized the earlier
rebuild-specific lesson into `check-for-live-em-process` memory covering
*any* action that touches a live invocation's shared state, not just
rebuilds.

Not yet re-run: the full stage1 `--keep-going` build with this fix, to see
how much of the remaining failure list (curl/elfutils/binutils were
downstream of the openssl failure and should clear too) it resolves.
[[em-stages-and-binhosts]] [[crossdev-target]] [[em-root-characterization]]

## 19. `use_with`/`use_enable` empty-arg bug (fixed), and the final tally

Re-ran with the CHOST-export fix: `openssl` now merges cleanly, but
`net-libs/gnutls` failed compiling `dlwrap/brotlienc.h` (`brotli/encode.h:
No such file or directory`) despite the plan showing `USE="... -brotli
..."`. Root cause: `gnutls-3.8.13.ebuild` calls `$(use_with brotli '' link)`
— real portage's `use_with()` resolves the feature name with bash `${2:-$1}`
(empty-or-unset fallback), so an explicitly empty second argument falls back
to the flag name (`brotli`). `em`'s `UseWithCommand`/`UseEnableCommand`
(`portage-repo/src/build/commands/use_flag.rs`) used
`self.feature.as_deref().unwrap_or(&self.flag)` — `Option::unwrap_or` only
catches `None`, not `Some("")`, so the literal empty string stayed empty,
producing `--without-` instead of `--without-brotli`. `./configure` warned
it was an unrecognized option and ignored it entirely, leaving brotli
auto-detected regardless of the requested USE flag (confirmed:
`app-arch/brotli-1.2.0-r1` is installed on the *host*, and gnutls's
`configure` found it via the host's own pkg-config, unrelated to the
sysroot).

Fixed by filtering empty strings to `None` before `unwrap_or`, matching bash
`:-` semantics, for both `use_with` and `use_enable`. New regression test:
`use_with_and_use_enable_treat_empty_feature_arg_as_omitted` (`shell.rs`) —
verified it fails without the fix (`--without-` instead of
`--without-brotli`), confirmed it passes with it.

Rebuilt and re-ran the full stage1 `--keep-going` build a final time.
**Result: 44 of 46 packages merged** (up from 38 of 127 on the very first
attempt this session). The two remaining failures are a different class of
issue, not build-execution bugs:

- `sys-devel/gcc-16.1.1`: fails self-building `libatomic` because
  `riscv64-unknown-linux-gnu-cc` resolves to the **already-installed older**
  GCC 15.2.1 (from the earlier toolchain bootstrap stage), which rejects a
  GCC-16-only configure-test flag (`-fno-link-libatomic`). Confirmed by Luca:
  a known, expected GCC bootstrap limitation (mixing bootstrap-compiler and
  final-compiler major versions in one pass), not an em bug — see
  [[gcc-bootstrap-compiler-version-mismatch]] memory. Don't re-investigate.
- `sys-apps/shadow`: `configure: error: crypt() not found` — `sys-libs/
  libxcrypt` is entirely absent from the sysroot's VDB (never planned, not a
  build failure). A depgraph-completeness gap (missing `virtual/libcrypt`
  resolution for this profile/USE combination), a different layer from the
  four build-execution bugs above — not investigated further this session.

Four real, independently-verified bugs found and fixed today, all committed:
PATH/eltpatch cross-target sysroot leak (#16), `CTARGET` sysroot-wide leak
(#17), the `CHOST`/allow-list → sourced-env-sweep architecture fix (#18),
and `use_with`/`use_enable` empty-arg handling (#19).
[[em-stages-and-binhosts]] [[crossdev-target]] [[em-root-characterization]]

## 20. `sys-devel/gcc` vs `cross-<CTARGET>/gcc` version drift (root-caused, follow-ups tracked)

Dug into *why* `sys-devel/gcc-16.1.1`'s self-build fails
(`gcc-bootstrap-compiler-version-mismatch` memory — confirmed by Luca as a
known GCC limitation, not an em bug, so not chased as a bug). The follow-up
question — why the active cross-compiler is gcc-15 when gcc-16 was visible
the whole time — traced to two genuinely **separate, independently-resolved
atoms** that are easy to conflate:

- `cross-riscv64-unknown-linux-gnu/gcc-15.2.1_p20260214` — the *host-side
  cross-compiler* `em crossdev --setup`'s toolchain bootstrap built (per
  `cross-stage1-riscv64-toolchain4.log`, 2026-07-03), currently active via
  `gcc-config` (`riscv64-unknown-linux-gnu-gcc` on `PATH` resolves to its
  `gcc-bin/15/` wrapper). That log shows it as `[ebuild R]` (reinstall) —
  already satisfied the atom from an even earlier attempt, so nothing
  re-resolved or upgraded it since; a plain `emerge` (or `em`) merge never
  force-upgrades an atom that's already satisfied without `--update`.
- `sys-devel/gcc-16.1.1_p20260606` — the *target's own* compiler
  (`CHOST == CTARGET`), resolved fresh today by `em stages --stage1`'s own
  independent atom resolution (nothing installed yet under that category in
  this sysroot), picking the newest visible version.

Both ebuilds have been present in the tree (same commit,
`367e22eb0c`/2026-06-14) the entire time — this isn't a stale-tree issue, just
two unrelated resolutions drifting apart over days. GCC then can't
self-bootstrap `sys-devel/gcc-16` using the older `cross-*/gcc-15` as its own
`CC_FOR_TARGET`, which is the actual proximate failure.

Documented the `cross-<CTARGET>/gcc` vs `sys-devel/gcc` distinction in
`portage-cli/src/crossdev/mod.rs`'s module doc and `docs/root-model.md`
(task #12, done).

**Correction after actually chasing task #13**: the "`--update` doesn't
work" framing above was wrong. Traced it precisely (instrumented
`target_package`'s filter in `repo.rs` with temporary `eprintln!`s, removed
before commit): `--update`/`--deep`/`root_targets` all behave correctly —
`cross-riscv64-unknown-linux-gnu/gcc` genuinely never sees anything newer
than `gcc-15.2.1_p20260214` because of a **keyword-acceptance gate**, not a
solver bug. The cross-compiler is a host-side tool, correctly resolved
against the *outer* self-contained root's own config (not the target
sysroot's `ACCEPT_KEYWORDS="riscv ~riscv"`) — and that outer root's
auto-generated `make.conf` (`em setup`) only ever set `MAKEOPTS`, leaving
`ACCEPT_KEYWORDS` unset (portage's stable-only default). The real host's
own `/etc/portage/make.conf` has `ACCEPT_KEYWORDS="~arm64"`; `gcc-15.2.1_
p20260214` is the last release with a *stable* `arm64` keyword — every
version since only carries `~arm64` (testing), rejected outright by the
stable-only fallback, regardless of `--update`/`--deep`. **Fixed** (task
#13, done): `em setup`'s self-contained root now mirrors the host's real
`ACCEPT_KEYWORDS` into its own `make.conf`, the same way it already mirrors
`MAKEOPTS` — `host_accept_keywords()` in `portage-cli/src/setup.rs`, new
test `self_contained_root_gets_host_accept_keywords`.

Also hit and fixed in passing (task #16): `em crossdev`'s own `-t`/
`--target` collided with `MergeFlags`' `-t`/`--tree` once flattened
together — clap's `debug_assertions` catch this and panic on *any*
`em crossdev` invocation in a dev/debug build (release builds skip the
check, so it was silently latent all session, only surfacing once a debug
build was needed for the `eprintln!` instrumentation above). Dropped the
short alias from `--tree`.

Task #14 (a safety check comparing a `gcc` package's own version against
the currently `gcc-config`-active compiler, warning before wasting a full
compile cycle) is still open — real value now that the actual gate
(`ACCEPT_KEYWORDS`) is fixed and version drift can still happen for other
reasons (a stale cross-toolchain simply never rebuilt after `sys-devel/
gcc` bumps a major version).
[[em-stages-and-binhosts]] [[crossdev-target]] [[em-root-characterization]]

## 21. Task #14 closed: auto-detect + weave in a cross-compiler refresh (fixed, verified live)

Built on finding #20: rather than just warning, `stage1()` now compares the
active `cross-<CTARGET>/gcc` slot against what `sys-devel/gcc` would
actually resolve to, and — if behind — weaves in a `gcc_refresh_plan`
(`gcc-stage1` → `gcc-stage2`, pinned to the exact resolved version via an
`=` atom) before the stage1 packages run. Getting this to actually work
end-to-end (not just print a correct `-p` plan) needed two more bugs found
only by real, non-pretend verification:

- **Wrong install root for the refresh.** The woven-in `cross-*/gcc` steps
  were installing into `--cross`'s sysroot substitution instead of the
  plain outer EROOT (where that category always lives — confirmed by
  comparing `em crossdev --setup`, which never sets `--cross`, against
  `stage1 --cross`, which does, for the identical atom). Fixed with
  `EmergeOpts::bypass_cross_root`, threaded through `run_staged` and
  `emerge_atoms_inner` so `stage1()` can run the refresh against
  `Cli::base_roots()` while its own packages keep using `Cli::roots()`.
- **The activation never actually happened.** Even with the root fixed,
  `gcc-config`'s active profile silently stayed on the old slot. Two
  layered causes: (a) `select::activate_compiler`/`activate_binutils`
  read `globals.roots()` internally — the whole `select::env_d`
  profile-selection chain was `&Cli`-coupled — so it looked in the
  `--cross` sysroot instead of the outer root the toolchain actually
  lives in; refactored the entire chain to take an explicit `Roots`
  instead, so crossdev can hand it `Cli::base_roots()` regardless of
  whether the *caller* has `--cross` set. (b) Once roots were right,
  `activate_toolchain`'s `atom.ends_with("/gcc")` match missed the
  refresh's version-pinned atoms (`=cross-.../gcc-16.1.1_...`) entirely —
  replaced with a proper `atom_is_package` package-name check.
- **Bonus, unrelated bug found along the way**: the cross sysroot's own
  auto-generated `make.conf` never set `MAKEOPTS` at all (unlike the
  self-contained `--root`'s generator) — since it's the *only* config
  `sys-devel/gcc` and every ordinary stage1 package reads, every such
  build ran fully serial (one `cc1plus` at a time on a 128-core host).
  Fixed by sharing `setup::host_makeopts()` between both generators.

Verified live end-to-end on the riscv64 cross sysroot: the previously
broken `sys-devel/gcc-16.1.1` `libatomic` build (finding #20's proximate
failure) now succeeds using the auto-refreshed, correctly-activated
cross-compiler. 35/36 stage1 packages merged; the one remaining failure
(`sys-apps/shadow-4.19.4`, `crypt() not found`) is unrelated, not yet
investigated.
[[em-stages-and-binhosts]] [[crossdev-target]]

## 22. `sys-apps/shadow` missing `sys-libs/libxcrypt` (fixed, `b371720`)

The `shadow-4.19.4` failure from finding #21 turned out not to be a shadow
ebuild bug (verbatim reaction: "sounds like a broken package that a
retarded systemd fanboy managed to crap up" — it wasn't; the ebuild is
fine). Root cause: `bdepend_trim`'s `runtime_required_cpns` scanned only
the *displayed* `order` (post "already installed, nothing to do" filter)
to decide which CPNs are still runtime-required, so it could reach
elsewhere. `virtual/libcrypt`'s newer slot needs `sys-libs/libxcrypt`
(`elibc_glibc`); `virtual/libcrypt` itself was already installed and thus
invisible in `order`, but it's still the *sole reason* `libxcrypt` is
required — scanning only `order` made `libxcrypt` look orphaned and it
was wrongly trimmed. Fixed by computing a separate `full_solution_order`
(every real package the solver selected, before the display filter) in
`depgraph()` and scanning *that* for runtime-required CPNs instead.
Regression test `already_installed_package_excluded_from_order_still_pins_its_rdepend`
(with a negative-control sanity check reproducing the pre-fix bug).
[[stage-build-shakeout]]

## 23. `app-crypt/gnupg` `dirmngr.service` staging failure (fixed, `0e95ec1`)

Flagged as "sounds like a broken package" again — again an `em` bug, not
gnupg's. `PhaseGroup::Install`'s `clean_subs()` wiped `work`/`image`/`temp`/
`homedir` before its `["install", "qmerge"]` phase list ran. `temp` (`${T}`,
PMS-defined cross-phase scratch space, same lifetime class as `WORKDIR`)
is where `src_prepare` staged gnupg's systemd unit templates
(`GNUPG_SYSTEMD_UNITS`) for `src_install_all`'s later `systemd_douserunit`
`doins` call — legal PMS use of `T`, but `Install` never re-runs `prepare`,
so wiping `temp` destroyed the staged files with nothing left to
repopulate them. Fixed: `Install`'s `clean_subs()` now wipes only
`image`/`homedir`, matching the persistence `work/` already got. Full
write-up with PMS references: `docs/worker-build-tree.md`.
[[stage-build-shakeout]]

## 24. Attempting a riscv64 stage3 (`--emptytree @system`) — PKGDIR fail-fast (fixed, `510e226`)

Next requested step: a full riscv64 stage3 via the existing `--emptytree
@system` engine (no `em stages --stage3` CLI flag exists yet — deliberately
"tried raw" instead of building that plumbing first). First `--buildpkg`
attempt: 50/110 merged, 37 failed, all `install worker exited with status
1` with **no visible error** — looked like the concurrency ceiling
(`--jobs 80`) itself was broken, since stage1 never hit anything like it.
Root cause (found by comparing what differs structurally between stage1
and stage3, not by staring at logs): `resolve_pkgdir`/`build_binpkg`
defaulted an unset `PKGDIR` to the real host's `/var/cache/binpkgs`
*unconditionally* — correct for a host build, wrong (and unwritable) for
any `--root`/`--cross`/`--local`/`--prefix` merge root. The resulting
`EACCES` mid-build appears to destabilize the privilege backend for
several packages at once under high concurrency (not fully proven — see
#25, which turned out to be a real, separate, low-probability crash in
the same backend). Fixed two ways: (a) `PKGDIR` now defaults to
`<merge_root>/var/cache/binpkgs` — reduces to the real host default when
`merge_root` is `/`, no special-case branch needed (Luca: *"the / or not
path building looks silly, / + var/cache/binpkgs -> /var/cache/binpkgs"* —
simplified accordingly); (b) `run_merge_plan` gained a `--buildpkg`
preflight (`check_pkgdir_writable`: create + write + remove a probe file)
so a misconfigured PKGDIR fails once, immediately, with a clear message —
never again 40 packages deep into a `--keep-going` run before anyone
notices. [[stage-build-shakeout]]

## 25. fakeroost rare ptrace race under real load → switched `auto` default to pseudoroot (`42d001e`)

After #24's fix, PKGDIR-permission errors vanished but 20/60 packages
still failed identically: qmerge (VDB write) succeeded, `--buildpkg`
packing then killed the whole install worker with **no printed error** —
confirmed live by checking the VDB directly (`binutils`, `pam`, `bash`,
`sed`, `tar`, … were all *actually merged*, just missing their `.gpkg.tar`
and reported as failed). Traced with a temporary `#[track_caller]` patch
to fakeroost's `Error::Errno` `From` impl (local-only, reverted, never
committed to the fork — see [[dont-commit-to-sibling-repos]]): the
supervisor's own event loop died on `fakeroost: syscall failed: ENOENT`
from `path::stat_target`'s `nix::sys::stat::lstat` (the `unlink_commit`
call site, `path.rs:56`), reproducible but **not deterministic** — one
`--jobs 1` single-package rebuild of `sys-devel/binutils` hit it
immediately, then 7 more consecutive retries (including a 10-package
`--jobs 8` batch) all succeeded clean. A real, rare race in the ptrace
supervisor under load, not something worth chasing to a fix inside
fakeroost itself right now.

Pragmatic fix (Luca: *"wait, we are using fakeroost? let's switch to
pseudoroot"*): `--privilege pseudoroot` already existed as a flag; `auto`
just preferred fakeroost. Flipped the priority in
`Backend::auto_backend()` — pseudoroot (LD_PRELOAD, no ptrace tax, and
structurally can't hit this ptrace-supervisor-specific race) is now tried
first, fakeroost second. Full `--emptytree @system` re-run under
`--privilege pseudoroot`: **54/57 merged**, and critically the 3 remaining
failures were genuine build errors with clear messages (see #26) — zero
recurrence of the silent post-qmerge death. [[stage-build-shakeout]]
[[pseudoroot-backend]] [[fakeroost-fork]]

## 26. Three real dependency-resolution bugs, found only once the privilege-backend noise was gone

With #25's switch, the stage3 run's remaining 3 failures were finally
legible root causes instead of privilege-backend noise — each reported by
the user as "a broken ebuild" and each actually an `em` bug:

- **`net-libs/libtirpc[python]`... no — `sys-apps/iproute2` linked
  `-ltirpc` despite `USE=-nfs`** (`1a7e7c4`). iproute2's `./configure`
  auto-detects optional RPC support via plain `${PKG_CONFIG} libtirpc
  --exists`. The generated cross sysroot `make.conf` never set
  `PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR`, so `pkg-config` searched
  the **host's** default paths and found the host's own installed
  `net-libs/libtirpc` — not in `DEPEND` (USE=-nfs) and not in the target
  sysroot at all. `HAVE_RPC` got set, `-ltirpc` got linked, the link
  failed since the library genuinely isn't in the sysroot. Fixed: the
  sysroot's `.pc` files record paths as if the sysroot were `/`
  (`prefix=/usr`, not the host-absolute path) — exactly what
  `PKG_CONFIG_SYSROOT_DIR` is for. `PKG_CONFIG_LIBDIR` (replaces, not
  additive like `_PATH`) points *only* at the sysroot's pkgconfig dirs, so
  no host `.pc` ever leaks into a foreign-arch cross build again.
- **`sys-apps/systemd-utils` meson: "python3 is missing modules: jinja2"**
  — two stacked bugs, both real, only the second one actually explaining
  why the package never got scheduled at all:
  - `Avail::atom_satisfied` (BDEPEND-availability check, trim +
    preflight) went through `Dep::matches_cpv`, whose own doc comment
    says it explicitly does *not* evaluate USE-dep brackets — so any
    USE-conditioned atom was "satisfied" the moment *any* build of that
    CPN existed in VDB, regardless of USE. The host's installed
    `jinja2-3.1.6` is built for `python_targets_python3_13` only; the
    BDEPEND needed `[python_targets_python3_14(-)]`. Fixed (`762e645`):
    `vdb_avail_entries` now carries installed USE+IUSE, so the simple
    `[flag]`/`[-flag]` forms get checked for real (`Conditional`/`Equal`
    forms still can't be evaluated here — no parent-flag context — same
    as before).
  - That fix alone didn't change the plan at all — because BDEPEND was
    never even reaching this check. `cross_target_runtime_deps` (the
    dependency function for a `--cross` Target-root package actually
    being built) called `append_unsatisfied_broot` for IDEPEND but never
    for BDEPEND, despite its own neighbouring comment already claiming
    "unsatisfied BDEPEND schedule via Host-root nodes when `with_bdeps`
    is on" — documented intent, never implemented. Fixed (`9c0354e`):
    added the missing call, gated on whether the package is genuinely
    *being built* (always pulls BDEPEND, matching the native
    `broot_filtered` equivalent's own `--with-bdeps`-independent
    behaviour) vs. already-installed-and-kept (never pulls it, also
    matching native).
  - Both fixes verified together: `dev-python/jinja2` +
    `dev-python/markupsafe` now correctly appear in a `--cross
    --with-bdeps -p` plan as Host-root entries.
- **`net-misc/dhcpcd` `libudev.h: No such file or directory`** — not a
  separate bug at all, a downstream cascade of the systemd-utils failure
  above (`virtual/udev`'s non-systemd branch is
  `sys-apps/systemd-utils[udev]`, which never built, so the headers it
  would have installed were never there). Confirmed fixed once
  systemd-utils's BDEPEND scheduling was fixed and it could actually
  attempt its build.

Real (non-pretend) re-verification: `net-misc/dhcpcd` and
`sys-apps/iproute2` both now build and merge cleanly end-to-end.
[[stage-build-shakeout]]

## 27. FIXED — `distutils-r1_python_install` false-died on a scriptless package built as a cross BDEPEND

Surfaced only as a *consequence* of #26's BDEPEND-scheduling fix actually
letting `dev-python/jinja2` attempt to build (previously it silently never
did). `markupsafe` (jinja2's own dep, also newly scheduled) hit a
transient `ln: ... File exists` on a first attempt — resolved by cleaning
a stale work dir from repeated manual testing, not a real bug; retried
clean and it now builds fine. `jinja2` itself did *not* self-resolve:
consistently died at `distutils-r1.eclass:1387` —
```
cd "${reg_scriptdir}" && find . -mindepth 1 | sort > ...
pipestatus || die "listing ${reg_scriptdir} failed"
```
`${reg_scriptdir}` = `${BUILD_DIR}/install/usr/bin`.

**Root cause, found by exhaustive elimination**: NOT a missing directory
at all. `_distutils-r1_post_python_compile` runs fine and correctly
populates `usr/bin` with the `python3.14`/`python3`/`python` dispatch
stubs + `pyvenv.cfg` — confirmed directly on disk, both from a raw
filesystem check right after the Compile phase and again right before the
Install phase's own phase loop starts. The directory is there and
readable the whole time. The `pipestatus || die` misfires because
`PIPESTATUS` itself is corrupted: `capture_variables` (the
Compile→Install `__worker` handoff, `ebuild.rs`) dumps `declare -p` and
restores it verbatim in the worker — and that dump includes bash's own
`PIPESTATUS` array (`declare -a PIPESTATUS=([0]="1")`, a leftover from
whatever single command last set it during compile). Once the worker
`source`s that line, brush *never resizes PIPESTATUS again* for the rest
of that process — unlike real bash, which unconditionally replaces the
whole array on every new pipeline regardless of any prior explicit
`declare`. So the eclass's own two-stage `(cd && find) | sort` pipe
genuinely succeeds, but `pipestatus()` reads the stale 1-element leftover
and reports failure. Confirmed with a 3-line repro with no em/eclass
involved at all — see [[brush-pipestatus-not-reset]] for the brush-side
root cause (`brush-core/src/variables.rs`'s `convert_to_indexed_array`
unconditionally destroying a `Dynamic` value's getter/setter binding).

**Fix landed** (`5902b73`): `capture_variables` now excludes `PIPESTATUS`
and bash's other dynamic/special vars (`FUNCNAME`, `BASH_LINENO`,
`BASH_SOURCE`, `BASH_ARGV`/`BASH_ARGC`/`BASH_ARGV0`, `BASH_CMDS`,
`BASH_COMMAND`, `BASH_SUBSHELL`, `BASH_ALIASES`) from the worker-env dump
— they're bash-maintained runtime state, never meant to cross a process
boundary. `dev-python/jinja2` now builds and merges cleanly under both
`--privilege pseudoroot` and `--privilege sudo`. The brush bug itself is
still open upstream, tracked separately since it's no longer blocking
anything here. [[stage-build-shakeout]]

## 28. Merge-execution ignored per-entry `merge_root` — Host BDEPEND silently built into the wrong root

Surfaced immediately after #27's fix: with the PIPESTATUS bug gone,
`dev-python/jinja2` built and merged cleanly in the standalone
`--emptytree dev-python/jinja2 --cross ...` repro, and the full
`--emptytree @system` run got much further (57/59 merged, only
`sys-devel/binutils` and `sys-apps/systemd-utils` failing — down from the
#22-27 baseline). But `sys-apps/systemd-utils` still died identically to
the *original* bug report: `meson.build:1695: ERROR: python3 is missing
modules: jinja2`, even though jinja2 "succeeded" earlier in the same run.

**Root cause**: `main.rs`'s merge loop (`merge_sequential`/`merge_parallel`,
both called from `run_merge_plan`) computed a single, plan-wide
`merge_root = roots.merge_root()` *once*, outside the per-package loop, and
used it for every `ebuild::build_and_merge`/`merge_binpkg` call regardless
of that entry's own `PlannedMerge.merge_root` field. So even though the
solver correctly classified jinja2 as `MergeRoot::Host` (an unsatisfied
BDEPEND scheduled onto BROOT — see #26/`9c0354e`) and the printed plan
correctly showed it with no `to /path` suffix, the actual build still ran
with `--sysroot /var/tmp/cross-stage1-riscv64/usr/riscv64-unknown-linux-gnu`
and merged into *that* sysroot's own VDB — confirmed directly:
`/var/tmp/cross-stage1-riscv64/usr/riscv64-unknown-linux-gnu/var/db/pkg/dev-python/jinja2-3.1.6`
existed, while the real host's own jinja2 (`/var/db/pkg/dev-python/jinja2-3.1.6`,
`python_targets_python3_13` only) was untouched. jinja2 "succeeded" from
`em`'s point of view but never became available where `systemd-utils`'s
build actually looks for it — the exact same die as before #26/#27, just
one layer deeper.

(Self-inflicted confound found and cleaned along the way: the *very first*
retest of this showed jinja2 apparently already "satisfied" and dropped
from the plan entirely — turned out to be `target_installed_cpvs`, a bare
`HashSet<Cpv>` with no `MergeRoot` in the key, matching my own earlier
manual `--emptytree dev-python/jinja2 --cross ...` test runs that had
installed jinja2 into the *sysroot's* VDB as a Target package. Removing
that leftover VDB/binpkg entry restored the correct signal. Real bug
confirmed independently of that confound — see the merge-root trace above.)

**Fix**: `main.rs` gained `entry_roots(planned, roots, host_roots) -> &Roots`
— a pure, unit-tested helper picking `host_roots` for
`planned.merge_root == MergeRoot::Host`, else `roots`. `run_merge_plan` now
computes `host_roots = globals.base_roots()` once and threads it into both
`merge_sequential`/`merge_parallel`, which call `entry_roots(...)` per
package instead of using one shared `roots`/`merge_root` for the whole
plan. `cli::Roots` gained a `#[cfg(test)]` `for_test(target: &str)`
constructor so `entry_roots` is testable without a full CLI parse. Two new
tests: `host_entry_installs_into_outer_eroot_not_the_cross_sysroot`,
`target_entry_uses_the_plans_own_root`. 141 tests pass, clippy/fmt clean.

**Why `base_roots()` (the `--root` offset) and not the bare system `/`**:
discussed live with the user. The bare host `/` would work today (it
already has jinja2, just for the wrong python target) but defeats the
point of an unprivileged `--root`/`--cross` build: it would need real
write access to `/usr` and would silently depend on whatever happens to
already be on the real machine. `base_roots()` keeps the whole build
self-contained under the `--root` offset, matching how `--local`/`--prefix`
Gentoo Prefix already isolates itself (sharing at most the host kernel/libc).
See [[em-root-characterization]] (Tier 1 item 2) — this is the *same*,
already-tracked "unsatisfied-BROOT Host scheduling" gap from 2026-06-27,
not a new discovery; today closed its solver and merge-execution halves.

**Still open — this is now an environment/bootstrap gap, not a code bug**:
routing jinja2 to `base_roots()` is correct, but *this session's* outer
EROOT (`/var/tmp/cross-stage1-riscv64`) was only ever bootstrapped with
the minimal cross-toolchain-support set (`sys-devel/{binutils,gcc}`,
`sys-apps/{baselayout,gentoo-functions}` — 38 VDB entries total, all
`cross-riscv64-unknown-linux-gnu/*` plus that handful). No native Python at
all. So jinja2's own build now fails differently there: `gpep517`'s
`patch_sysconfig` can't find `_sysconfigdata` under
`base_roots()`'s `usr/lib/python3.14` because nothing ever installed a
native Python at that root. Fixing this needs the outer `--root` offset to
carry a **full native stage1** (not just enough to bootstrap the
cross-compiler) — exactly the work `em-root-characterization.md`'s "Stage1
from-scratch into `--root`" section already tracks, just now with a
concrete, motivating BDEPEND case (jinja2/gpep517/flit-core, and by
extension any python-build-time tool: sphinx, cython, setuptools_scm).

**2026-07-09 — independently re-verified, then root-caused for real (this
write-up's first pass, below, misdiagnosed it).** Rebuilt `em` fresh and
re-ran the exact task-#17 target (`--autosolve-use --privilege pseudoroot
--root /var/tmp/cross-stage1-riscv64 --cross riscv64-unknown-linux-gnu
--emptytree sys-apps/systemd-utils --with-bdeps --keep-going --jobs 16
--buildpkg`) as a clean invocation (not a VDB-presence check):
`docbook-xml-dtd-4.2-r3` merges, `systemd-utils` still dies identically,
`Program python3 (jinja2) found: NO`. Traced it through meson's own source
(`mesonbuild/modules/python.py`, `find_installation`) and confirmed the
actual `meson` process is the bare host's own `dev-build/meson`, under the
host's own `/usr/bin/python3.13` — jinja2 was built into `base_roots()`'s
`python3.14` site-packages, invisible to that process.

The first-pass conclusion here ("this EROOT needs a full native stage1")
was wrong about *why*. The user corrected the framing: real
`ROOT=/tmp/staging emerge pkg` / `{target}-emerge` always resolve `BDEPEND`
against the **real host `/`** — only the install *target* moves. `em`'s own
`docs/root-topology.md` already specifies this (scenario 1: `--root` is
`Dual { broot: /, target: <offset> }`), but `cli.rs::base_roots()` had
`base: R, target: R` for plain `--root` — BROOT was the *offset*, not the
host. So `bdepend_avail::initial_bdepend`/`preflight`/`load_host_installed`
all checked jinja2's BDEPEND satisfaction against the *offset's* (mostly
empty) VDB instead of the real host's — which already has jinja2 for
python3.13, exactly what the host's own `meson` needs. **Fixed**, in two
passes — full story in `todo/root-topology-refactor.md`. Short version:
`--root`'s BROOT is now always the host `/`, but that lives in a new,
dedicated `Cli::broot()`, not `base_roots()` itself (a first attempt to
change `base_roots()` directly broke crossdev's own unprivileged
toolchain-install path, which relies on `base_roots()` staying "the outer
EROOT" for `--root`, a genuinely different question from BROOT). The old
self-contained-BROOT-in-an-offset workflow (what this sysroot was actually
doing) now has its own explicit name: `--local DIR` (parameterized to
accept a path, previously hardcoded to `~/.gentoo`). Unit-tested
(`cli.rs`'s `root_broot_is_host_not_offset`); live-reverified that
`em --root R --cross riscv64-unknown-linux-gnu crossdev -t
riscv64-unknown-linux-gnu --init-target` completes cleanly and
unprivileged (this exact command was what caught the first pass's
regression). Not yet re-verified end-to-end against a full rebuilt riscv64
sysroot through `sys-apps/systemd-utils` itself (the old one was wiped) —
that's a long real build, left as a follow-up.

**`app-text/opensp` is not part of `sys-apps/systemd-utils`'s own
dependency closure** (checked the ebuild — no reference at all); the
2026-07-05 "2 failures" must have come from a broader target in that
session. Review-checklist item "are jinja2/opensp the same bug class?" is
answered: no, and opensp doesn't reproduce from this target.

## 29. `sys-devel/binutils` cross build fails with `#error unsupported ABI` — upstream binutils bug, not em

The #28 write-up left this as an unexamined `make exited 2` amid parallel
`-j80` output. User pushed back on whether it was even real vs. leftover
noise from repeated testing in this session — worth checking directly
rather than assuming. It is real, and root-caused precisely.

**The failing command**, found by grepping the (huge, `-j80`-interleaved)
build.log for `error:` past the expected `Werror`/`LOCALEDIR` noise:
```
gcc -c -I. -W -Wall ... -I/var/tmp/cross-stage1-riscv64/usr/riscv64-unknown-linux-gnu/usr/include sysinfo.c
...
/var/tmp/cross-stage1-riscv64/usr/riscv64-unknown-linux-gnu/usr/include/bits/wordsize.h:22:3: error: #error unsupported ABI
```
Plain `gcc` (no cross prefix — correctly, since `sysinfo` is a
build-machine codegen helper that generates `sysroff.c`/`.h` for
`dlltool`'s PE support at build time, per `binutils/Makefile.am`'s
`sysinfo$(EXEEXT_FOR_BUILD)` rule) is compiling `sysinfo.c` with the
**target** riscv64 sysroot's own `/usr/include` — an aarch64 host gcc
choking on riscv64-specific glibc header content it was never meant to see.

**Root cause, confirmed straight from binutils' own upstream source**
(`binutils/Makefile.am`, not anything em-generated):
```
AM_CFLAGS          = $(WARN_CFLAGS) $(ZLIBINC) $(ZSTD_CFLAGS)
AM_CFLAGS_FOR_BUILD = $(WARN_CFLAGS_FOR_BUILD) $(ZLIBINC) $(ZSTD_CFLAGS)
```
`ZSTD_CFLAGS` (correctly `-I<sysroot>/usr/include`, for the actual
cross-compiled binutils binaries linking `libzstd`) is **reused verbatim**
in `AM_CFLAGS_FOR_BUILD` — the native/build-machine helper flags. There is
no `ZSTD_CFLAGS_FOR_BUILD` upstream. `sysinfo.c` doesn't even use zstd;
this is dead/vestigial inclusion that's harmless when CBUILD and CTARGET
share compatible glibc header layouts, and only breaks when they
structurally differ enough to trip the ABI `#error` guards (aarch64 host
vs. riscv64 target, here). **Deterministic, 100% reproducible, confirmed
independent of any prior test-session state — not "noise from an unclean
setup."** This would hit real `emerge` too, under the same
CBUILD/CTARGET/USE=zstd combination; not em-specific at all.

User pushed back further: "why isn't zstd being picked up [for the build
machine]?" — traced one level deeper into `config/zstd.m4` (unpacked a
fresh source copy to check, since the merged build's own work tree is
gone by the time a package succeeds):
```m4
AC_DEFUN([AC_ZSTD], [
  ...
  PKG_CHECK_MODULES(ZSTD, [libzstd >= 1.4.0], [...])
])
```
There is exactly **one** `PKG_CHECK_MODULES` call for zstd in the entire
binutils build, called once at top-level configure using whatever
`$PKG_CONFIG`/`PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR` is active —
correctly scoped to the target sysroot by this session's earlier
`PKG_CONFIG_SYSROOT_DIR`/`LIBDIR` fix (`1a7e7c4`; that fix is right and
necessary for the *actual* target-linked zstd to resolve at all).
Binutils has **no build-machine-specific zstd detection anywhere in its
own build system** — not a missed detection, a code path that was never
written. `Makefile.am` then applies that single, correctly-target-scoped
result to both the real target build and the native `sysinfo.c` helper.
So "zstd isn't being picked up for the build machine" because binutils
never attempts to look it up there at all; there's nothing for em to fix
or configure differently on its side.

**User pushback, re-verified with a live controlled A/B (not archaeology)**:
challenged twice — first whether this was test-session noise, then whether
the whole finding was simply fabricated, reasonably given "we built
binutils for crossdev and host enough times" without ever hitting this.
Re-ran the actual merge path (`em --emptytree sys-devel/binutils --cross
...`, not the `em ebuild` debug harness — which turned out to not even set
`PKG_CONFIG_SYSROOT_DIR`/`LIBDIR`, a separate, real gap worth noting
later) twice back to back on the same live sysroot: `package.use -zstd`
removed → same exact `bits/wordsize.h:22:3: error: #error unsupported
ABI` reproduces, byte-for-byte, at a fresh build.log line; restored → the
exact same build merges clean. Direct, live, repeatable cause and effect.

**Reconciles with "built binutils enough times" — this really is a rare
combination**: `cross-riscv64-unknown-linux-gnu/binutils` (the crossdev
toolchain package) and the host's own native `sys-devel/binutils` both
have `CBUILD == CHOST` — binutils itself is never actually cross-compiled
in either case, so `CFLAGS`/`CFLAGS_FOR_BUILD` never diverge and the
`ZSTD_CFLAGS` reuse is inert. This build is different: it's `sys-devel/
binutils` cross-compiled *to run natively on riscv64* inside the
sysroot's own `@system` closure — a genuine `CBUILD(aarch64) ≠
CHOST(riscv64)` compile of binutils itself, which almost nobody needs
(ordinary crossdev usage never builds a target-native copy of binutils).
That's exactly why this specific upstream bug basically never surfaces in
normal use, and why it took a full from-scratch self-hosting stage3 to
hit it.

**Workaround (verified live)**: disable `zstd` for this cross binutils
build — it only gates optional debug-info decompression support, unrelated
to binutils' actual function:
```
# <sysroot>/etc/portage/package.use/sys-devel-binutils
sys-devel/binutils -zstd
```
Rebuilt clean afterward: `em --emptytree sys-devel/binutils --cross
riscv64-unknown-linux-gnu ...` → merged, binpkg created. No em code change
needed or appropriate here — this is upstream binutils' bug to fix (drop
`$(ZSTD_CFLAGS)` from `AM_CFLAGS_FOR_BUILD`, or add a real
`ZSTD_CFLAGS_FOR_BUILD` autoconf check), not something `em` should paper
over generically.

**How to resume**: rebuild release, retry the full `--emptytree @system`
run with the `package.use` override above in place. `sys-apps/systemd-utils`
(blocked on the native-stage1-at-`base_roots()` gap, #28) is now the only
known remaining failure out of 59. [[stage-build-shakeout]]

## 30. Cleanup pass — the same "hardcoded bare host" bug as #28, twice more (fixed, `732aefe`)

Requested directly ("let's do a full pass and clean up this mess, there is
too much duplication and hardcoding") after #28 fixed the solver's own
`load_host_installed()` reading the bare host VDB regardless of where a
Host BDEPEND merge actually lands (`base_roots()`). Auditing every other
`Vdb::open_default()`/`None ⟹ bare host` call site in `bdepend_avail.rs`
and `query/depgraph/*.rs` found the *exact same bug*, independently
duplicated, in two more places that never got touched by the #28 fix:

1. `Avail::initial_bdepend()` — hardcoded `vdb_avail_entries(None)`
   (bare host) unconditionally, ignoring its own `roots` parameter
   entirely. This is what `preflight::check()` uses, so the pre-flight
   guard-rail was still checking BDEPEND satisfaction against the wrong
   root even after #28 fixed the solver side.
2. `bdepend_trim::TrimCtx`/`avail_for_consumer()` (the post-solve
   within-run BDEPEND trim pass) — same call, same bug, via
   `Avail::initial_bdepend(ctx.roots)` where `ctx.roots` is the
   (possibly `--cross`-substituted) solver root, not `base_roots()`.

Fixed both by threading `host_roots: &Roots` (= `Cli::base_roots()`,
already computed at every call site for the #28/`load_host_installed`
fix, no new plumbing needed) into `Avail::initial_bdepend`,
`preflight::check`, and `TrimCtx`, mirroring the established convention.
`TrimCtx.roots` became dead once its only reader switched to
`host_roots` — removed rather than left unused. Also deduped
`vdb_cpvs()`/`vdb_avail_entries()`'s identical `Vdb::open` match arms
(one now delegates to the other).

Audited the remaining `None ⟹ bare host` sites and confirmed they're
legitimate, not the same bug: `installed.rs::load_one`'s `None` case is
only reached when both `roots.base()`/`roots.target()` are genuinely
unset (bare host, correctly); `search.rs`'s `Vdb::open_default()` is in
a command that takes no `Roots` parameter at all today (`em search` has
no `--root` support, a separate pre-existing feature gap, not a
hardcoded bypass of an available parameter — left alone).

Added a regression test (`initial_bdepend_reads_the_given_root_not_the_bare_host`)
mirroring `load_host_installed`'s existing one. Full
`cargo build/clippy/test/fmt --check` clean across the workspace.

This is a correctness/consistency fix, not a new capability — it doesn't
change the `sys-apps/systemd-utils` outcome (#28's note): that failure is
a real native-stage1-at-`base_roots()` bootstrap gap, and in *this*
session's setup `base_roots()` is the `--root /var/tmp/cross-stage1-riscv64`
offset (not bare host).

**Correction, see #31**: the claim just above ("the preflight failure list
there was always reporting real, not virtual, missing packages") turned
out to be only half right — #31 found a second, genuinely virtual cause
mixed into that same failure list.

## 31. `preflight::check` checked a Host entry's own DEPEND against the wrong Avail set (fixed)

Asked directly ("why did it fail though?") after reporting #30's fix and
the huge (~50-package) pre-flight failure list from re-running the native
`dev-python/jinja2` build into `base_roots()`. Cross-referencing the
printed plan against `preflight.rs`'s bookkeeping (not just re-asserting
"real bootstrap gap") found a second, distinct, and previously
unconfirmed bug — the "why do even DEPEND-only relationships like
`dev-lang/perl` needing `sys-libs/gdbm` fail despite gdbm appearing
earlier in the plan" question flagged as unresolved earlier this session.

The plan lists `sys-libs/gdbm` and `dev-lang/perl` *twice each*: once with
no `to ...` suffix (`MergeRoot::Target`, going into the `--cross` sysroot)
and once with `to /var/tmp/cross-stage1-riscv64/` (`MergeRoot::Host`,
going into `base_roots()`). `gdbm`-Host is earlier in the plan than
`perl`-Host, so `perl`-Host's `>=sys-libs/gdbm-1.8.3:=` DEPEND should see
it as already merged. It didn't, because `check()`'s loop did:

```rust
collect_unsatisfied(&depend, &depend_avail, &mut missing);   // always
collect_unsatisfied(&bdepend, &bdepend_avail, &mut missing); // always
...
match planned.merge_root {
    MergeRoot::Host => bdepend_avail.record_merge_bdepend(cpv),       // only bdepend_avail
    MergeRoot::Target => bdepend_avail.record_target_merge(&mut depend_avail, cpv), // both
}
```

Every entry's own `DEPEND` was checked against `depend_avail` regardless
of its `merge_root` — but `depend_avail` only grows from `Target` merges.
A `Host` entry's `DEPEND` on *another* `Host`-merged package is checked
against a set that never received that package, because recording a
`Host` merge only updates `bdepend_avail`. Since a `Host` package is
*built at* `base_roots()`/BROOT, its own `DEPEND` should be checked
against the same view as its `BDEPEND` — not `depend_avail`, which
represents the Target/base sysroot, a different root entirely.

Fixed by branching the DEPEND check on `merge_root`, same as the existing
BDEPEND-recording branch:

```rust
match planned.merge_root {
    MergeRoot::Host => collect_unsatisfied(&depend, &bdepend_avail, &mut missing),
    MergeRoot::Target => collect_unsatisfied(&depend, &depend_avail, &mut missing),
}
collect_unsatisfied(&bdepend, &bdepend_avail, &mut missing);
```

Two regression tests added (`host_entry_depend_satisfied_by_earlier_host_entry`,
`target_entry_depend_not_satisfied_by_host_only_entry` — the latter a
negative control confirming Host merges still don't leak into the
Target/base-system view). Both needed an **isolated** `Roots` — the first
attempt used `Roots::default()` and the negative-control test failed for
the wrong reason: `Roots::default()`'s `merge_root()`/`base()` fall
through to the *real* bare host `/var/db/pkg`, and this dev machine
already has `sys-libs/gdbm`/`dev-lang/perl` installed, satisfying the
atom regardless of the bug. Fixed by extending `Roots::for_test` to also
set `base` (matching a real `--root DIR` invocation, where base == target)
so both tests run against an empty tempdir VDB, hermetically.

This means the earlier ~50-package failure list was a mix of two causes:
some packages genuinely missing at `base_roots()` (the real bootstrap
gap, #28/#30's note stands for those), and others — anything with a
`Host`-routed DEPEND on another `Host`-merged package earlier in the plan
— spuriously reported due to this bug. Full re-run needed to know the
real remaining gap size; not yet done as of this writing (large, slow
build). Committed alongside #30's plumbing. Full
`cargo build/clippy/test/fmt --check` clean.

## 32. `order`'s "already installed" filter ignored `merge_root` — a real target system permanently masked its own Host BDEPEND (fixed)

Found immediately on retesting #31 live: `em --autosolve-use --privilege
pseudoroot --root /var/tmp/cross-stage1-riscv64 --cross
riscv64-unknown-linux-gnu --emptytree sys-apps/systemd-utils --with-bdeps
--keep-going --jobs 1 --buildpkg` still failed pre-flight with ~59
missing deps, but `dev-lang/perl` itself never appeared *anywhere* in the
plan — not as a Target entry (correctly; it's already built in the
`--cross` sysroot from the earlier successful stage3 run) and not as a
`Host` entry either, even though `base_roots()` genuinely has no perl
and multiple `Host`-routed `dev-perl/*` modules need one (their BDEPEND
on `dev-lang/perl[perl_features_...]` comes from `perl-module.eclass`,
which injects the *same* perl atom into both `DEPEND` and `BDEPEND` —
confirmed by reading the eclass and a real ebuild).

**Initial wrong hypothesis, caught before acting on it**: first assumed
this was the already-documented "self-inflicted confound" (stale VDB
leftovers from before the #28 `entry_roots()` fix) and nearly proposed
deleting `dev-lang/perl`/`python`/`python-exec*` from the `--cross`
sysroot's VDB. Checked first — that sysroot has a full, legitimate
150+-package `@system` closure (portage, openrc, udev, openssh, gcc,
glibc...), the real result of the earlier successful stage3 run. Would
have destroyed real completed work over a misdiagnosis.

**Actual root cause**: `depgraph/mod.rs`'s `order` filter (~line 548,
"drop packages already installed at this version") checks a package
against `target_installed_cpvs: HashSet<Cpv>` — but `Cpv` carries only
`(cpn, version)`, no `merge_root`. A `Host`-routed `dev-lang/perl`
requirement (needed unsatisfied at `base_roots()`) has the *same*
`(cpn, version)` as the `--cross` sysroot's own legitimately-installed
perl, so it matches `target_installed_cpvs` and gets silently dropped
from `order` — never built, never displayed, and its BDEPEND stays
permanently unsatisfied. This isn't test pollution; it will happen for
*any* real target build that happens to already have a same-named tool
(perl, python, gettext, m4, autoconf...) installed at the target, which
is common. The same bug was duplicated at the `PlannedMerge.reinstall`
assignment further down (~line 957).

Fixed by building a parallel `host_installed_cpvs: HashSet<Cpv>` (from
the already-loaded `host_installed`) and branching both sites on
`pkg.merge_root()` — mirroring the pattern already used everywhere else
this session (`entry_roots()`, `Avail::initial_bdepend`, `preflight`'s
DEPEND check). `output::print_tree`'s action-tag display (~line 842)
still uses `target_installed_cpvs` unconditionally — left alone since it
only covers explicitly-requested root atoms (always real Target atoms in
practice), lower risk, and out of scope for this fix; worth revisiting
in the "make it more rational" pass.

No unit test added — `depgraph()` is a heavy integration surface (real
repo/VDB/use_env loading via `tokio::join!`), unlike the narrower pure
functions fixed in #26/#30/#31. Verified via `cargo build/clippy/test
--workspace` (clean) plus a live re-run of the actual failing command.

**This is the fourth Host/Target root-conflation bug found this session**
(`load_host_installed`, `Avail::initial_bdepend`, `preflight`'s DEPEND
check, now `order`'s installed-filter) — same root cause pattern each
time: code written assuming `--cross` is the only reason two roots ever
matter, so a bare `Cpv`/`None`-sentinel/single `Avail` set silently
conflates Host and Target once a self-contained `--root` offset with its
*own* Host-side bootstrap needs entered the picture. Once this run's
live result is in, worth a deliberate pass to make the model harder to
get wrong again (e.g. a `Cpv`-keyed set that's illegal to query without
naming which root) rather than fixing each call site as found.

## 33. Live re-run after #32: real progress, plus a *different*, non-root bug — reinstall fallback breaks install order

Rebuilt release with #32's fix and re-ran the same
`sys-apps/systemd-utils --cross ... --emptytree ... --with-bdeps` command
live. Confirmed improvement: `dev-lang/perl-5.42.2` now appears in the
plan at all (previously invisible, silently dropped by #32's bug) — but
still only as a single `Target`-routed `[ebuild R]` reinstall entry, not
also as the `Host` entry the BDEPEND-driven closure needs. `preflight`
still fails, but for a new, structurally different reason.

Root cause (distinct from #28/#30/#31/#32 — not a Host/Target root
mix-up): the perl reinstall entry is appended by a *separate* fallback
block, after the main `order` construction:

```rust
// Fallback: any reinstall the solver didn't route through install_order
// (rare) is appended so it is not silently dropped.
{
    let in_order: HashSet<Cpn> = order.iter().map(|(pkg, _)| *pkg.cpn()).collect();
    let to_reinstall = provider.reinstall_deps()
        .into_iter()
        .filter(|r| !in_order.contains(r.package.cpn()))
        ...;
    order.extend(to_reinstall);
}
```

This unconditionally appends at the *end* of `order`, regardless of
where the package's dependents sit. In this run perl lands at plan
position 157, but `dev-perl/YAML-Tiny` (which needs it) is at position
110 — so when `preflight::check`'s within-run-visibility loop reaches
YAML-Tiny, perl hasn't been recorded yet, and the DEPEND/BDEPEND check
still fails. `reinstall_deps()` producing a package `install_order()`
didn't naturally place is exactly the "(rare)" case this fallback's own
comment anticipates — worth understanding *why* the solver's normal
`install_order()` didn't route perl through its proper topological slot
before deciding how to fix the append point.

The failure list also changed shape (now surfacing things like
`app-portage/elt-patches`, `autoconf`/`automake` `||`-groups,
`app-alternatives/{ninja,yacc,awk}`) — not yet sorted out how much of
that is newly-visible *real* gap at `base_roots()` vs. more instances of
this same ordering issue elsewhere in the plan.

User asked to pause, update the todo, and step back to review whether
the Host/Target model (four fixes in `preflight.rs`/`bdepend_avail.rs`/
`depgraph/mod.rs` today) is sound or a pile of hacks, before chasing
#33 as another one-off patch. After choosing "solver-level fix", root
was found via live instrumentation (temporary `eprintln!`s in the
pubgrub crate, removed after — see below), not more guessing:

1. `append_unsatisfied_broot` correctly creates the Host-perl edge
   (confirmed live: `satisfied=false root=Host` for every one of the 34
   BDEPEND edges on perl in this run).
2. `pubgrub::resolve()`'s returned `solution` correctly *includes*
   `dev-lang/perl:0@host @ 5.42.2` — the solve itself is 100% correct.
3. `full_order` (from `provider.install_order(&solution)`) also
   includes it — but at index 183, *after* `dev-perl/YAML-Tiny:0@host`
   at index 128 — YAML-Tiny is Host-routed too (not Target, contrary to
   what the printed plan's missing `to .../` suffix suggested at a
   glance — misleading initial read, corrected by the trace) and needs
   perl as its own BDEPEND. A dependency landing *after* its consumer is
   a real ordering bug, not the Host/Target-view bug this session had
   been fixing all day.

**Root cause, in `portage-atom-pubgrub/src/graph.rs`'s
`dependency_graph()`** (used only by `install_order`'s topological
sort): `let Some(data) = self.packages.get(pkg) else { continue };` —
a **direct** lookup, bypassing the alias-resolving `self.package_data(pkg)`
that `get_dependencies()` correctly uses elsewhere. `self.packages` is
keyed by whatever identity the construction-time BFS discovers, always
`Target`-flavored for a real package (`ensure_host_instances`/
`host_aliases` exist specifically to redirect a `Host`-flavored lookup
to its `Target` twin's data). This raw lookup **always misses for every
`Host`-flavored solved package**, silently producing zero outgoing
edges for it. So a `Host` package's own BDEPEND on *another* `Host`
package (perl on perl's own build tools, or here, indirectly, YAML-Tiny
on perl) never gets an ordering edge at all — `install_order`'s
Kahn's-algorithm tie-break (`comp_key`, "largest ready first" by
package-string comparison) then decides their relative order
arbitrarily, and `"dev-perl/YAML-Tiny..."` happens to sort after
`"dev-lang/perl..."` — hence perl lands *after* its own consumer.

**Fixed**: `self.packages.get(pkg)` → `self.package_data(pkg)` (one
line). Added `host_package_bdepend_on_another_host_package_orders_correctly`
in `graph.rs` — deliberately named so the *broken* tie-break would also
get the order wrong for the wrong reason (a first attempt using names
where alphabetical order happened to coincide with correct dependency
order passed regardless of the fix, i.e. didn't actually discriminate —
caught by reverting the fix and confirming the test still passed before
trusting it; the corrected version fails cleanly with the bug reverted
and passes with the fix). Full `cargo build/clippy/test/fmt --check`
clean across the workspace (30/30 test binaries).

This is a **fifth**, structurally distinct bug from today's four
Host/Target root-conflation fixes (#28/#30/#31/#32) — not "is X already
there in the right root", but "does a Host node's own dependency data
get found at all when building the ordering graph". Both bug families
share the same underlying cause, though: `Host` was bolted onto an
architecture built around a single `PortagePackage` identity space, and
each fix found a different place that never got updated for the
dual-identity (`host_aliases`) reality. Whether that's "a pile of
hacks" or "one real gap found in five places" is exactly the
soundness-review question still open — see the note above and
[[em-root-characterization]] for the broader tracking doc this arc
belongs to. Next: rebuild release and re-run the actual failing
`sys-apps/systemd-utils --cross ...` command to see the *real*
remaining gap at `base_roots()`, now that both #32 and #33 are fixed.

**Live re-run confirmed #32 and #33 both actually fixed**: re-ran the
same `sys-apps/systemd-utils --cross ...` command with the rebuilt
release binary. The entire perl/dev-perl mass of failures (~30+ lines)
is completely gone — no `dev-lang/perl` or `dev-perl/*` unsatisfied
entries anywhere, and the plan now resolves all the way through to
`sys-apps/systemd-utils` itself. The remaining ~34 pre-flight failures
are a different, much smaller, coherent set: `app-portage/elt-patches`,
`dev-build/{autoconf,automake,meson,cmake}` `||`-groups,
`dev-perl/Locale-gettext`. Two `weak(!)` blocker warnings also appeared
(`sys-libs/timezone-data` vs `glibc[vanilla(+)]`,
`app-alternatives/gpg` vs `gnupg[-alternatives(-)]`) — non-fatal per
PMS, not yet investigated.

## Architecture recap and the real remaining gap (not a code bug)

User pushback after I started manually building `elt-patches`/`cmake`/
`meson` as ad-hoc standalone atoms to chase the remaining ~34 failures,
and hit what looked like a fresh circular dependency (`sys-libs/gdbm`
needing `elt-patches` even in that combined request): **going in circles
reinventing bootstrapping instead of trusting the mechanism that already
works** (`em stages --stage1`). Restated architecture, confirmed correct:

- **Unprivileged**: `--stage1` populates a self-contained host root (not
  `/`) — this is what already works (task #7's from-scratch native
  bootstrap: binutils, gcc, baselayout, gentoo-functions + basic
  archive tools).
- **Privileged**: root is `/`, install directly on the real host —
  trivial, a real system already has everything.
- Build sysroot + cross-toolchain (crossdev) — done (task #7-#9).
- Build a cross `--stage1` into the sysroot — done (task #8): this
  produced the real, legitimate 150+-package `@system` closure found in
  the sysroot's own VDB during the #32 investigation.
- Build stage3/stage4 by using the **host root prepared in step 1** to
  satisfy Host BDEPEND at BROOT — this is exactly what the
  `sys-apps/systemd-utils --cross ...` command has been doing all
  along; `base_roots()` *is* that host root, just under-populated so far.

**Key insight on the remaining gap**: `--stage1`'s own design assumes a
baseline tier ("obviously available", the same tier as `tar`/`gzip`/
`bzip2`/`xz-utils` — all already present in this session's minimal
31-package seed) is present *before* it runs. `elt-patches`/`cmake`/
`meson`/`autoconf`/`automake`/`libtool`/`gettext`/`pkgconfig` belong in
that same baseline tier — they were never meant to be pulled in via
BDEPEND resolution during stage1 itself. Confirmed by reading real
ebuilds: `sys-apps/attr` and `dev-libs/libffi` both `inherit libtool`
(which sets `BDEPEND=">=app-portage/elt-patches-20250306"` via
`LIBTOOL_DEPEND`), then their own `BDEPEND="..."` line **overwrites** it
(plain `=`, not `+=`) — so in real bash-sourcing semantics, elt-patches
genuinely isn't part of their final computed BDEPEND either. Yet
`elibtoolize`'s `eltpatch` call in `src_prepare` still needs the binary
on `PATH` regardless of what's in the metadata — a known, informally
accepted Gentoo-tree quirk that real bootstrapping works around simply
by having these tools already present in any reasonable build
environment, not by relying on the dependency graph to conjure them.
Each of these tools resolves cleanly as a standalone atom (confirmed
live for `elt-patches`, `cmake`, `meson` individually) — the fix is
building them directly into the baseline, not chasing the solver for a
gap that isn't a bug.

**Plan**: extend the self-contained host root's baseline (currently just
binutils/gcc/baselayout/gentoo-functions/basic archive tools) with
`elt-patches`, `gettext`, `autoconf`, `automake`, `libtool`, `cmake`,
`meson`, `pkgconfig` — built one at a time (not combined, to avoid
whatever made the combined `elt-patches cmake meson` request pull in a
much larger, differently-ordered closure including gdbm) — then retry
`em --root <hostroot> stages --stage1 --with-bdeps` for real. Once that
succeeds, retry the `sys-apps/systemd-utils --cross ...` build to
confirm the whole pipeline closes.

Also noted, not yet investigated: `em stages --stage1` doesn't pass
`--with-bdeps` by default — without it, the plan is visibly smaller
(missing `meson` entirely). Given a from-scratch stage1 build is
*exactly* the scenario needing full BDEPEND resolution, this might be
worth defaulting on for `stages --stage1` specifically (mirroring how
`--emptytree` already forces `solve_with_bdeps` on for native builds) —
a candidate follow-up, not blocking the immediate baseline-extension work.

**Baseline-extension progress and the actual remaining wall**: set
`USE="-* build"` in the host root's `make.conf` (matching `--stage1`'s
own convention — building baseline tools with default USE pulled in a
much bigger closure, ~gnupg/portage-sized, not smaller). Built
`app-portage/elt-patches` alone (trivial, no real deps) — merged clean.
Re-ran `stages --stage1 --with-bdeps` for real: failure list dropped
from ~34 to just 4, all `meson`/`cmake` (`libxml2`, `json-c`,
`pax-utils` needing them). Real progress, elt-patches genuinely was the
main blocker.

**Then nearly repeated the "reinventing bootstrap" mistake a second
time**: went to hand-build `meson`+`cmake` next, hit what looked like
the same "dependency ordered after consumer" symptom (libxml2 before
meson in the plan) even for a single `dev-build/meson` atom request.
User pushback again ("why are you trying to install what is the
stage1") — right call: `--stage1`'s own solve *already* includes
meson/cmake in its plan (confirmed: they're there, just seemingly
misordered), so hand-seeding was working around a symptom rather than
understanding it.

**Root-caused via live instrumentation (temporary, removed after) —
this time it really is architectural, not a bug**: traced the exact
edge `dev-libs/libxml2:2@host → dev-build/meson:0@host` (class
`Bdepend`) — it exists, correctly, confirming `dependency_graph()`'s
earlier alias fix (#33) is working. But `install_order`'s own Tarjan SCC
pass places both nodes in the **same 74-member strongly-connected
component** — a genuine hard-edge cycle, not a bug. The code already
has a documented, accepted answer for this exact shape of problem (see
`install_order`'s doc comment: *"only if a genuine hard (build-time)
cycle remains, as with bootstrap cycles (`xz-utils` ↔ `elt-patches`), do
we fall back to a deterministic lexicographic tie-break"*) — there is no
correct linear order for a true cycle; something has to come first
somewhat arbitrarily. This one is just much bigger (74 nodes vs. 2),
almost certainly because `sys-apps/portage` (needed by nearly
everything as a Host BDEPEND target) has its own large RDEPEND/DEPEND
footprint that loops back through several of the same tools.

**Reframing**: manually seeding a package to break a *genuine* cycle
isn't "reinventing bootstrapping" — it's what real bootstrapping
(catalyst stage builds, real Portage's own SRC_URI ordering hacks for
xz-utils/elt-patches) already does for this exact problem shape. The
earlier correction stands for the *elt-patches* case (that one wasn't a
cycle at all, just a baseline-assumption gap) — but meson/cmake landing
in a real 74-node SCC is a different, legitimate reason to seed
directly. Not yet decided: whether to (a) just build meson/cmake
directly (as already attempted) and accept the pre-flight guard-rail
will still complain about the ~4 remaining entries until they're
present, or (b) narrow the cycle first (e.g. check whether
`sys-apps/portage`'s footprint can be trimmed for a build-only stage1,
shrinking the SCC and maybe eliminating the cross-linking entirely).
Paused here to discuss direction with the user rather than guess further.

## 34. Cycle actually broken, `--nodeps`+preflight bug fixed, a real `unpack` bug found — systemic issue resolved

User picked (a): seed the cycle directly, package by package, using
`--nodeps` — but `--nodeps` on a single atom *still* hit
`preflight::check()`, which turned out to check real ebuild metadata
unconditionally regardless of `--nodeps`. Real fix (not a workaround):
`--nodeps` already means "merge only the named atoms, no dependency
expansion or verification" (matching emerge's own semantics) —
`preflight::check()` should be skipped entirely when `--nodeps` is set,
since running it defeats the flag's whole purpose (seeding one member
of a genuine cycle, which by definition has no valid dependency order
for the guard-rail to check against). One-line fix in `main.rs`
(`13bb26d`), full `cargo build/clippy/test/fmt --check` clean.

With that in hand, traced the real cycle by hand and broke it link by
link, verifying each *actual build* (not just the plan) as I went:

1. `dev-build/samurai` (zero real deps — pure C, no python) selected as
   the `app-alternatives/ninja` provider instead of `dev-build/ninja`
   (which needs python) via `package.use`, breaking the
   `meson → ninja → python` sub-link.
2. Traced `dev-lang/python`'s own real prerequisites (sqlite, openssl,
   expat, gdbm, libffi, readline, ncurses, pkgconfig, unzip, perl,
   autoconf/automake/autoconf-archive/mime-types/mpdecimal/
   gentoo-common/gnuconfig, m4/gzip-alt/autoconf-wrapper/help2man) and
   built each with `--nodeps`, confirming empirically at each step
   that only real, satisfiable deps remained (no further hidden
   cycles in this branch).
3. Hit the *actual* remaining cycle: `sys-apps/gawk` (needed by
   `app-alternatives/awk`, needed by python's own BDEPEND) needs
   `sys-devel/bison`, which — per its own ebuild comment ("gettext IS
   required in RDEPEND because >=bison-3.7 links against it") —
   unconditionally needs `sys-devel/gettext`, which unconditionally
   needs `dev-libs/libxml2` (real DEPEND, not USE-gated), which needs
   `dev-build/meson`, which needs `python`, which needs `awk`, which
   needs `gawk`, which needs `bison` — a genuine, confirmed hard cycle.
   Broke it the same way real distributions do: built `gawk` with
   `--nodeps` first, betting that upstream gawk tarballs ship a
   pre-generated bison output and don't actually invoke bison unless
   timestamps force regeneration. Confirmed live — gawk's *actual
   build* succeeded with no bison present at all.
4. With `gawk` (and thus `app-alternatives/awk`) real, `python` merged
   for real (all prerequisites finally satisfied).
5. `dev-build/meson`'s *actual build* then hit a genuinely different,
   real `em` bug (see below) — fixed — after which meson merged too.
6. With `python` + `meson` real, `dev-libs/libxml2` → `sys-devel/gettext`
   → `sys-devel/bison` → `dev-build/cmake` all resolved and merged via
   **normal** (non-`--nodeps`) solving — confirming the cycle really is
   fully broken now, not just individually special-cased.

**Real `em` bug found and fixed along the way**: `dev-build/meson`'s
`unpack` phase died with `unknown archive type` on
`meson-reference-1.11.1.3` — a bare man page fetched via SRC_URI's `->`
rename (`meson-reference.3 -> meson-reference-${PV}.3`), not an archive
at all. `meson`'s `src_unpack` is just `default`, which calls
`unpack ${A}` unconditionally on *every* distfile including this one;
the man page is later installed straight from `${DISTDIR}` via `newman`
in `src_install`, never actually needing extraction. `em`'s `unpack`
treated any unrecognized suffix as fatal; real Portage's `unpack`
helper leaves such files untouched instead — this must have been
silently correct for every previously-tested ebuild only because none
of them happened to have a non-archive SRC_URI entry. Fixed in
`portage-repo/src/build/commands/unpack.rs` (`fa27567`), with a
regression test and a control test confirming recognized suffixes
still dispatch normally.

**Result, live-verified**: a full `--autosolve-use --with-bdeps` request
for `cmake libxml2 gettext bison` together resolved cleanly through
`preflight` (previously always failed here) and *ran* — 74 packages
merged, 28 failed. The originally-blocking systemic issue (elt-patches
baseline gap #30/#31/#32/#33, the meson/cmake cycle, the
`--nodeps`/preflight gap, the `unpack` bug) is fully resolved and
verified live.

## 35. Triage of the 28 remaining failures — 24 were `--jobs 16` races, 1 was a real `em` bug, 3 were unrelated flakes

Checked each of the 28 against the VDB directly rather than trusting the
combined (heavily interleaved, `--jobs 16`) log: only **4** were actually
still missing. The other 24 (`attr`, `libuv`, `libb2`, `e2fsprogs`, and
~20 more) had failed on one of two duplicate Host/Target build attempts
(a not-yet-ready shared prerequisite at that exact moment) while the
*other* attempt succeeded — individual `build.log` files for all of them
end cleanly at `pkg_postinst`, and the packages are genuinely installed.
Not a bug, just `--jobs 16` contention during a from-scratch bootstrap
where many packages' true readiness windows are narrow.

Of the 4 genuinely missing:
- `net-dns/c-ares`, `dev-libs/libgpg-error`, `sys-apps/findutils` — all
  three merged cleanly on retry at `--jobs 1`. `c-ares`'s config.log
  showed the compiler test program *actually compiling and linking*
  (`$? = 0`) followed immediately by `configure: result: no` — a `--jobs
  16`-induced race/flake in the build environment, not a real toolchain
  problem. `findutils`'s "could not determine how to read list of
  mounted file systems" likewise didn't recur at `--jobs 1`.
- `dev-python/installer` — a **real, distinct `em` bug**: `unpack`'s
  existence check for a `./`-relative archive used a bare
  `PathBuf::from(archive)`, resolving against the Rust process's own
  OS-level CWD instead of the shell's *tracked* working directory
  (`cwd`, already captured and used for the actual extraction) — those
  can diverge, since brush tracks `$PWD` independently of calling
  `std::env::set_current_dir`. `installer`'s `src_unpack` does
  `cp foo.whl foo.whl.zip && unpack "./foo.whl.zip"` (a real,
  PMS-legitimate pattern — PMS 12.3.11 says only bare filenames get
  looked up in `$DISTDIR`; anything with a path separator is used as
  given — independently confirmed live in `eclass/rpm.eclass`'s own
  `unpack "./${a}"`). The check always reported "not found" for a file
  that demonstrably existed in `cwd`. Fixed by resolving `./`-relative
  archives against `cwd.join(archive)`; extracted the whole
  path-resolution branch into a pure `resolve_src_path()` function so
  it's directly unit-testable without a full shell harness (`bc236f7`).
  Verified: reverting the fix made the new regression test fail
  cleanly; restoring it passes. `dev-python/installer` itself then
  merged cleanly on retry.

All 4 confirmed fixed/resolved and live-verified; the workspace's own
test suite hit one unrelated, pre-existing flaky test
(`reused_shell_does_not_leak_metadata_between_ebuilds` — passes in
isolation and on a clean re-run of the full suite) during this pass,
unconnected to any of today's changes.

**Next**: retry `em stages --stage1` for real, end-to-end, on
`/var/tmp/cross-stage1-riscv64` (now that all 4 real gaps are closed),
then retry `sys-apps/systemd-utils --cross riscv64-unknown-linux-gnu ...`
to confirm the full pipeline closes.

## 36. `app-alternatives/gpg`'s VDB-recorded IUSE missing its own alternatives flags — real, deferred `em` bug; unblocked via manual VDB patch

> **2026-07-18 re-verify: FIXED in tree.** `portage-repo` `inherit` implements
> Portage’s stash/`E_*` accumulate; `source_ebuild` appends eclass totals to
> ebuild values after sourcing. `inherit_*` brush_compat tests pass. Not on the
> pending queue anymore — demote any remaining manual VDB patches as obsolete
> once packages are re-merged under current `em`.

`app-crypt/gpgme` failed to resolve because `app-alternatives/gpg-1-r3`'s
installed VDB `IUSE` file only contained `nls ssl` — missing `reference`,
`freepg`, `sequoia`, the three flags `app-alternatives.eclass`'s
`_app-alternatives_set_globals()` injects at eclass-sourcing time
(`IUSE="+${ALTERNATIVES[*]%%:*}"`). The metadata cache computes IUSE
correctly (the solver's own printed plan showed `USE="reference -freepg
..."` as valid, toggleable flags) — the bug is in the VDB write path:
`ebuild.rs`'s `iuse: env.iuse` (~line 1982) sources IUSE from
`shell.collect_env()`, i.e. the *live, post-execution* shell state,
which only reflects the ebuild's own final `IUSE=` bash assignment. Since
`gpg-1-r3.ebuild` sets its own plain `IUSE="nls ssl"` *after* inheriting
`app-alternatives` (the same eclass-var-gets-overwritten-by-the-ebuild's-
own-plain-assignment pattern already found this session for
`libtool.eclass`'s `BDEPEND`), the eclass-injected flags never make it
into what gets written to the VDB, even though they're real, valid,
solver-visible USE flags.

Not rushed as a code fix — IUSE genuinely needs to be sourced from live
post-execution state for some dynamic cases (e.g. `PYTHON_TARGETS`), so
correctly disentangling "eclass computed it, ebuild silently dropped it"
from "ebuild legitimately mutated it at runtime" needs its own careful
design pass. Presented 3 options to the user (fix the real bug now /
manually patch the VDB / stop here); chose the manual patch to unblock
stage1 completion now, deferring the real fix:
```
echo "nls ssl reference freepg sequoia" > \
  /var/tmp/cross-stage1-riscv64/var/db/pkg/app-alternatives/gpg-1-r3/IUSE
```
(plus `package.use/app-alternatives-gpg`: `app-alternatives/gpg
reference`, `package.use/app-alternatives-ninja`: `app-alternatives/ninja
-reference samurai`, `package.use/net-misc-curl`: `net-misc/curl ssl` for
curl's `quic` REQUIRED_USE). This is a **files-only workaround, not a
code fix** — a fresh `em` build of this same ebuild elsewhere would
reproduce the bug. **TODO**: fix `ebuild.rs`'s IUSE-for-VDB sourcing
properly later.

## 37. `em stages --stage1` completes cleanly end-to-end on `base_roots()`

A final full retry (`stage1-final5.log`) reported "4 of 53 failed to
merge" (`app-arch/bzip2`, `app-arch/xz-utils`, `sys-devel/gettext` ×2) —
but checking the VDB directly (`find .../var/db/pkg/<cpn>-[0-9]* -maxdepth
0`) confirmed all 4 are genuinely installed. Root cause, confirmed via
the `gettext-runtime/config.log` for the failing attempt: its own tail
showed `configure: exit 0` (success) despite the enclosing `build.log`
recording a failure moments earlier — a concurrent `--jobs 16` duplicate
Host/Target attempt clobbered the shared workdir mid-build, exactly the
already-known race pattern from #35, not a new bug. Task #20 (complete
the full `stages --stage1` build on `base_roots()`) is done.

**Next**: retry the original task #17 target —
`em --autosolve-use --privilege pseudoroot --root
/var/tmp/cross-stage1-riscv64 --cross riscv64-unknown-linux-gnu
--emptytree sys-apps/systemd-utils --with-bdeps --keep-going --jobs 16
--buildpkg` — to confirm the full pipeline closes end-to-end.
