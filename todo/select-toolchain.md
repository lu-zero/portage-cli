# `em select` — toolchain activation (gcc / binutils / linker / clang)

STATUS: **activation mostly done** (cross + native `post_step`, `pkgconf`
wrapper 2026-07-17). **`--root` silently defaulting to the host's `gcc` on
`PATH` is correct as-is** (matches catalyst's seed-compiler model — not a
bug, confirmed by the user directly). `--prefix`/`--local` should instead
prefer their own built toolchain once one exists — split by the user
(2026-07-23) into three separately-landed phases:

1. **✅ `em select` correctly populates the confdir under `--prefix`/`--local`
   — DONE 2026-07-23.** See "Phase 1" section below — found and fixed a
   real bug (`get_chost`'s host-fallback path), confirmed the rest of the
   mechanism (`em toolchain --setup`'s real-ebuild-postinst-triggered
   `gcc-config` activation) already worked correctly end-to-end.
2. **🔴 Awareness** — the build path (`shell.rs`'s `init_build_env`) should
   prefer a `--prefix`/`--local`'s own activated toolchain over the host's
   once one exists, via broadening the `chost != cbuild` gate. Not started.
3. **🔴 `em active --prefix/--local <path>`** — a new command, persistent
   state under `~/.local/state/em/` (XDG state dir — also flagged: evaluate
   full XDG Base Directory compliance more broadly at some point, not just
   for this) + a shell-eval export mode. Not started; needs its own design
   pass (state format, interaction with an explicit `--prefix` flag, whether
   a bare `em <pkg>` should pick up a registered "current" prefix).

See [[PENDING]] row 1.

## Phase 1 — `em select` confdir population under `--prefix`/`--local` (2026-07-23)

Live-tested end-to-end in a fresh crossdev-stages sandbox
(`em-prefix-select-0723`, unprivileged `builder` user, real
`--prefix /home/builder/prefix`), since static code-reading alone couldn't
resolve the open question of *how* the very first `env.d` profile entry for
a freshly-merged `sys-devel/gcc` gets created (`select::env_d::activate_latest`
only ever *picks the newest among already-listed* profiles — it never
synthesizes one from nothing, and `list_all_profiles`/`collect_profiles`
only *read* existing `env.d` files, never scan installed packages/`gcc-bin`
dirs directly).

**Answer, confirmed live**: real `sys-devel/gcc`'s own ebuild
`pkg_postinst` (via `toolchain.eclass`) calls the real `gcc-config` binary
itself — `em` faithfully executes real ebuild/eclass shell code, and
`gcc-config`'s own script is `EPREFIX`/`EROOT`-aware, so it correctly wrote
the profile entry into the **prefix's own** `etc/env.d/gcc/`, not the
host's. `em toolchain --setup --prefix /home/builder/prefix` completed 8/8
packages and the log showed the real `gcc-config` message verbatim
(`* Switching native-compiler to aarch64-unknown-linux-gnu-16 ...`).
`em select compiler list` then correctly showed **both** profiles, labeled
`(host)`/`(prefix)`, with the prefix's own marked active (`*`):
```
[1] aarch64-unknown-linux-gnu-15 (host)
[2] aarch64-unknown-linux-gnu-16 * (prefix)
```
and the installed wrapper directory (`<prefix>/usr/bin`) contained not just
`<chost>-gcc` but also bare `gcc`/`cc`/`g++`/`c++` symlinks (mirrors real
`gcc-config`'s own `gcc-bin/<ver>/` layout exactly — resolves an earlier,
mistaken worry that `em`'s own wrapper installer only creates
CHOST-prefixed names). So the confdir-population mechanism itself needed
**no code change at all** — `em toolchain --setup --prefix DIR` (already
wired to the general `toolchain` applet, not just the internal stage flow,
`crossdev/mod.rs:588`) was already complete and correct.

**The one real bug found and fixed**: `select::get_chost` (`select/mod.rs`)
derives the default `--target` for `select compiler/binutils/linker
{show,set}` when none is given explicitly, by grepping for a `CHOST=` line
in `config_portage_dir(globals).join("make.conf")`, falling back to a
legacy `/etc/make.conf` (pre-2006 flat-file layout, essentially never
populated on a modern system) if that literal path happens to exist. Under
`--prefix`, `config_portage_dir` correctly resolves to the **prefix's own**
`etc/portage/make.conf` — but that file **never has a `CHOST=` line by
design** (the generated template's own header says "Profile and base
make.conf come from the host"). With no working fallback, `get_chost`
silently fell through to its own last-resort guess,
`format!("{}-unknown-linux-gnu", globals.arch)` — using `Cli::arch`'s
Gentoo arch name ("arm64"), not the real CHOST tuple ("aarch64"). Result:
`em --prefix DIR select compiler show` (no explicit `--target`) reported
`(no compiler profile set for target 'arm64-unknown-linux-gnu')` even
though `list` correctly showed a real, active `aarch64-unknown-linux-gnu-16`
profile one line above it — `show`/`set` were looking for a target that
doesn't exist. Same bug, same fix, for `binutils`/`linker` (all three share
`get_chost` via the identical `run()` pattern in each module).

**Fix**: when `is_prefix_context(globals)` is true, add the host's real
`/etc/portage/make.conf` to the fallback chain. Two new tests in
`select/mod.rs` (`get_chost_under_prefix_falls_back_to_host_make_conf`,
`get_chost_host_context_is_unaffected`) — both read the real host
`/etc/portage/make.conf` directly for their expected value (matching the
existing environment-honest test convention already used by
`binpkg.rs::host_root_skips_the_root_relative_branch`), skip gracefully on
a host with no `CHOST=` set at all.

**Also dropped, same commit**: the legacy `/etc/make.conf` (pre-2006
flat-file layout) fallback that used to sit alongside this. Traced its
origin — added in `0f5784e` (2026-06-23, "feat(select): add gcc and
binutils subcommands...", AI-generated/"Mistral Vibe") as a generic
"check some system make.conf" guess, without the modern
`/etc/portage/make.conf` convention or `--prefix`'s "host provides base
config" model in mind. Confirmed safe to remove: essentially never
populated on any system relevant today, and it was a real footgun, not
just dead weight — any stray file some other tool left at that exact path
with an unrelated `CHOST=` line would have silently outranked the correct
value.

**Follow-up in the same pass**: `get_chost` was also hand-rolling its own
`line.starts_with("CHOST=")` scan with manual quote-stripping instead of
using this project's own centralized make.conf parser,
`portage_repo::MakeConf` (real `brush-parser` winnow AST parse — handles
quoting/trailing comments/later-assignment-wins correctly, already used
elsewhere for exactly this, e.g. `binpkg.rs::resolve_pkgdir_for_roots`).
Rewrote `get_chost` to use `MakeConf::load(path).get("CHOST")` instead —
same file-path resolution, more robust parsing, no behavior change for the
common case (re-verified live in the sandbox).

Live-verified after both fixes: `show`/`set` (no `--target`) both correctly
resolve `aarch64-unknown-linux-gnu-16`, and `set` still successfully
re-activates and reports the right target.

Consolidates the former `select-{compiler,binutils,linker,clang}.md`. These are
the `eselect`/`*-config` workalikes that *activate* a built toolchain (write
`env.d` state + the `usr/bin/<T>-*` wrappers). The build half is done; this is
the activation half — the seam where `em select` meets the toolchain/stages work
([[em-stages-and-binhosts]], [[em-root-characterization]], [[crossdev-target]]).

## Implemented (2026-06-24), all in `portage-cli/src/select/`

- **compiler** (`gcc-config`/`eselect gcc`): `list`/`show`/`set`, per-arch
  grouping, `--config-root`/`--local`/`--prefix`-aware. `install_wrappers` reads
  env.d `GCC_PATH` → `usr/bin/<T>-<tool>` + `<T>-cc`, re-rooted under EPREFIX.
- **binutils** (`binutils-config`): same shape; two-level layout
  `usr/libexec/gcc/<T>/<tool>` → `binutils-bin/<VER>`, then `usr/bin/<T>-<tool>`.
  Bin dir located on disk (cross nests `usr/<CBUILD>/<T>/binutils-bin`, native
  `usr/<T>/binutils-bin`).
- **linker**: ld/lld/mold selection via the same `env.d/linker/` mechanism.
- **clang** (Option A): LLVM *slot* selection (`/usr/lib/llvm/<SLOT>`), distinct
  from the env.d mechanism. `list`/`show`/`set`.

Shared engine: `select/env_d.rs` (the `EnvDProfile` trait + `activate_latest`,
`set_profile`, `install_wrappers`).

## `pkgconf` (2026-07-17), a deliberately different shape

`select/pkgconf.rs` picks between `pkgconf`/`pkg-config` and creates the
`<CTARGET>-pkg-config` wrapper real crossdev provides (see the 2026-07-17
task #17 addendum in `todo/PENDING.md` for the bug this closes — a from-scratch
riscv64 cross build had no way to satisfy `tc-getPKG_CONFIG`'s `$PATH` search
for it). Doesn't use the `EnvDProfile`/`env_d.rs` machinery at all: unlike
gcc/binutils/linker, there's no versioned profile to track.

**First cut was a bare symlink to the chosen backend — caught as
insufficient** (Luca: "the symlink to pkgconf is not enough, we need to
provide the same features"). Read real crossdev's own `cross-pkg-config`
script directly (`/usr/bin/cross-pkg-config`, present on this host from a
real system crossdev install) rather than guessing: it self-derives
`PKG_CONFIG_SYSROOT_DIR` from `ROOT`/`SYSROOT`/`ESYSROOT` at invocation time,
probes the ABI-correct `libdir` (`LIBDIR_${ABI}`, else asks the compiler),
sets `PKG_CONFIG_SYSTEM_LIBRARY_PATH`/`_INCLUDE_PATH` +
`PKG_CONFIG_FDO_SYSROOT_RULES=1` (so pkgconf suppresses redundant `-I`/`-L`
for paths the cross-compiler already searches by default — the reason a real
`zlib --cflags --libs` through the wrapper correctly comes back as just
`-lz`, not `-I<sysroot>/usr/include -L<sysroot>/usr/lib64 -lz`), sanitizes an
inherited `PKG_CONFIG_PATH` (unlike `_LIBDIR` it *adds* to the search list),
and sanity-checks the output for host `-I`/`-L` leaks.

**Second correction, same session — one shared script, not one copy per
target.** Real crossdev doesn't write a full script per `<CTARGET>`; it
ships *one* `cross-pkg-config` and symlinks every target's
`<CTARGET>-pkg-config` to it, deriving `CHOST` from `${0##*/}` (its own
invoked name) rather than baking a target in. Matched that shape exactly:
`SCRIPT_TEMPLATE` is written once per root as `em-cross-pkg-config`
(`em`-prefixed so it's unambiguous this isn't a real system crossdev
install), and `<CTARGET>-pkg-config` is a plain symlink to it — the only
piece genuinely specific to *this* installation is the resolved backend
path (`REAL_PKG_CONFIG=`), baked in once by `select pkgconf set`/
auto-activation and shared by every target. This isn't just less
duplication — it's more correct: the backend choice (`pkgconf` vs
`pkg-config`) is a host-tool decision with no reason to vary between cross
targets under the same root, and a later fix to the derivation logic now
reaches every already-configured target automatically instead of needing
each one regenerated by hand. Everything else is still self-derived at
invocation time, so the wrapper works standalone too (just
`SYSROOT`/`ESYSROOT`/`CHOST` set), not only inside an em-managed build
phase. The shared script carries all state (`show`/`list` read its own
`REAL_PKG_CONFIG=` line) — still no env.d state needed.

Auto-activated alongside gcc in both `activate_toolchain` (cross) and
`activate_native_toolchain` (native); idempotent per target (skips if that
target's symlink already exists) and idempotent on the backend choice
(skips writing the shared script if one already exists, whether from an
earlier auto-run for a different target or a deliberate `em select pkgconf
set`) — `set` is the one path that deliberately always rewrites it, since
it's an explicit, root-wide choice.

## Auto-activation from the build — the real consolidation

The ebuild `pkg_postinst` runs the **host** `binutils-config`/`gcc-config`, which
activates into `/`, not the build root. So the merge driver must activate via
`em select` after the toolchain steps. Status by flavour:

- **cross** — DONE. `crossdev --setup`'s `post_step_cross` calls
  `select::activate_binutils` then `select::activate_compiler` (gcc references the
  binutils tools, so binutils first), plus `link_abi_osdirs` after libc.
- **native** (`em toolchain --setup`) — **DONE 2026-07-17** (`activate_native_toolchain`
  as real `post_step`; EPREFIX-aware re-root of wrappers — see [[PENDING]]).
  Older notes below about a no-op `post_step` / dangling symlink are historical;
  the remaining gap is **build-shell PATH preference** for native offsets
  (`shell.rs` still gates the CHOST-prefixed tool block on `chost != cbuild`).

### Blocker: `env_d` is config-root-keyed, must be merge-root-aware

`select/env_d.rs` derives every path from `config_portage_dir` (the **config
root**). For a native `em toolchain --setup --root <R> --config-root /`:
- `list_all_profiles` reads `<config>/etc/env.d/...` = the **host** profiles, not
  `<R>`'s freshly-built one → `activate_latest` would pick a host profile.
- `set_profile` + `install_wrappers` write under `env_d::eprefix` = the config
  root → wrappers land on the **host**, not `<R>`. Wrong (and harmful).

The toolchain's env.d + wrappers belong in the **install target (merge_root)**,
because that's where the binaries are. Fix: thread an explicit
`eprefix = roots.merge_root()` through the *activation* path
(`activate_latest → set_profile → install_wrappers`, plus the profile-read in
`list_all_profiles`/`env_d_dir`), leaving the user-facing CLI (`list`/`show`/`set`)
on the config root. Note `install_wrappers` is a trait method on `EnvDProfile`, so
the signature change touches all four modules + their tests — a contained but
real refactor, hence not done in the unslop pass.

For the common cases this is a no-op: plain (`merge_root == / == config`),
`--local` (`merge_root == ~/.gentoo == overlay root`), and `--cross`
(`merge_root == sysroot == config`). Only `--root --config-root /` diverges —
exactly the toolchain/stage case we need.

Then wire native: factor `crossdev::activate_toolchain` to take a `tuple: &str`
(it only uses `target.tuple`), give `em toolchain --setup` a `post_step` that
activates with `select::get_chost(globals)` as the tuple (no `link_abi_osdirs` —
that's cross-only), and cross keeps its current hook. One shared activation seam.

### Validation
`<R>/usr/bin/<chost>-gcc hello.c -o hello --sysroot=<R> && file hello` →
working ELF; and the stages then build via `<chost>-gcc`. (Today the same works
only via the full `usr/<chost>/gcc-bin/<ver>/<chost>-gcc` path.)

### 2026-07-16 re-check: confirmed live, and the gap is one layer deeper than written above

Verified against real, already-built native toolchain roots on this host
(`/var/tmp/stage1-native`, `/var/tmp/stage1-base`, both aarch64-on-aarch64):
`usr/bin/` has only gcc's own versioned install (`aarch64-unknown-linux-gnu-
gcc-16`), no bare `<chost>-gcc` wrapper, no `gcc`/`cc`; `etc/env.d/gcc/` has
only the ebuild-written profile file, no `config-<chost>` — confirming
`activate_latest`/`set_profile` never ran for these, exactly as `post_step`
being `|_| Ok(())` in both `toolchain()` and `stage1()` (`crossdev/mod.rs`)
predicts.

**Second, deeper gap found tracing what would actually consume that wrapper**:
`portage-repo/src/build/shell.rs`'s cross-toolchain-selection block
(~line 1170-1220, sets `CC`/`CXX`/`PATH` to `<chost>-*`) is explicitly gated
on `chost != cbuild`, skipped for "native (CBUILD unset, or CHOST == CBUILD)"
by design — and a native `--root` build always ends up `CHOST == CBUILD`
(an earlier block in the same file defaults `CBUILD` to `CHOST` when unset).
`PATH` itself (~line 1071-1085) is built from the **host's own**
`/usr/bin:/bin` (sanitised of `$HOME`/`/usr/local`), with no ROOT-scoped
prepend in the native case. So even after `em select`'s wrapper gap is fixed,
nothing in the native build path would put that wrapper ahead of the host's
real `gcc` on `PATH` — a native stage build's `CC` silently resolves to the
**host's** compiler today, not the freshly-built ROOT one. This is exactly
the "host lib leaks in" failure mode `em-stages-and-binhosts.md`'s design
question #5 warns about; it just wasn't previously pinned to this specific
code path.

Closing this needs two changes, not one: (a) this doc's already-planned
`env_d.rs` merge-root-awareness + a real native `post_step` (unblocks `em
select`/the wrapper itself), and (b) a native-offset branch in `shell.rs`'s
toolchain selection, parallel to the existing cross one, that prepends the
ROOT's own toolchain dir to `PATH` even when `CHOST == CBUILD`. (a) alone
creates the wrapper but doesn't make the build shell use it.

### 2026-07-16, continued: checked against real portage/crossdev's own model — (b) is a *widened condition*, not a new branch

Read `toolchain-funcs.eclass` (`/var/db/repos/gentoo/eclass/toolchain-funcs.eclass`)
and real crossdev's own `<tuple>-emerge` wrapper (`/usr/bin/riscv64-unknown-
linux-gnu-emerge`) to check em's model against the real one, since (b) above
was framed as "add a native branch" — that's not quite what real portage does.

**Real portage's algorithm** (`_tc-getPROG`, `tc-getPROG`/`tc-getBUILD_PROG`):
for every one of `tc-getCC`/`tc-getCXX`/`tc-getAR`/`tc-getAS`/`tc-getLD`/
`tc-getNM`/`tc-getRANLIB`/`tc-getSTRIP`/`tc-getOBJCOPY`/`tc-getOBJDUMP`/
`tc-getCPP`/`tc-getF77`/`tc-getFC`/`tc-getPKG_CONFIG`/`tc-getRC`/`tc-getDLLWRAP`/
`tc-getGCJ`/`tc-getGO`/`tc-getHIPCXX` (and their `tc-getBUILD_*` twins, keyed
off `CBUILD` instead of `CHOST`):
1. If the plain var (`CC`, `AR`, …) is already exported, use it verbatim —
   **no CHOST check at all**. (`tc-getBUILD_*` also checks `BUILD_CC`/
   `CC_FOR_BUILD`/`HOSTCC` first, plus the bare var too when
   `tc-is-cross-compiler` is false.)
2. Otherwise, search `$PATH` for `${CHOST}-<tool>` (or `${CBUILD}-<tool>` for
   the `BUILD_*` family) and use the resolved short name if found.
3. Otherwise, fall back to the bare tool name (`gcc`, `ar`, …).

Critically, **step 2 always runs, native or cross** — `tc-is-cross-compiler`
(`${CBUILD:-CHOST} != CHOST`) only changes whether the `BUILD_*` family also
accepts the bare var as a last resort; it never gates the CHOST-prefix search
itself. This is *why* a plain, non-offset Gentoo system's `tc-getCC` still
finds `${CHOST}-gcc` (e.g. `x86_64-pc-linux-gnu-gcc`) — `gcc-config` always
creates that CHOST-prefixed symlink, cross or not, and real portage's own
build always goes through it, not a bare `gcc`.

**Real crossdev's `<tuple>-emerge` wrapper doesn't set `CC`/`CXX`/... at all**
— only `CHOST`, `SYSROOT`, `PORTAGE_CONFIGROOT`, `CBUILD` (queried via
`portageq envvar`), and `BUILD_{CFLAGS,CXXFLAGS,CPPFLAGS,LDFLAGS}`. It relies
entirely on step 2 above finding the CHOST-prefixed wrapper on `$PATH` — which
works because either (a) the build is a real chroot (catalyst stage-building:
the ROOT's own `/usr/bin` *is* `/usr/bin` from inside), or (b) it's a genuine
crossdev cross-compiler, whose CHOST-prefixed binary is a host-native
executable installed straight onto the shared host `/usr/bin` (never inside
a sysroot at all).

**em doesn't chroot**, so neither (a) nor (b) holds for a `--root`/`--prefix`
offset — `$PATH` never naturally contains the offset's own `usr/bin`. em's
`shell.rs` compensates for this today, but only for the `chost != cbuild`
case: it explicitly sets `CC`/`CXX`/`AR`/`NM`/`RANLIB`/`STRIP`/`OBJCOPY`/
`OBJDUMP`/`READELF`/`LD` to `${chost}-<tool>` and prepends a
crossdev-sysroot-shaped bin dir (`build_config_root`'s grandparent `/bin`) to
`PATH` — i.e. em already *does* exactly the extra plumbing real portage gets
for free from chrooting, just gated behind "is this genuinely cross" rather
than "is there an offset toolchain directory to prefer regardless of arch".

So (b) isn't a parallel native branch — it's **broadening this existing
block's condition** so it also fires for a plain same-arch `--root <dir>`
offset once `em select` has written that dir's own wrapper (from fix (a)),
using the offset's own `usr/bin` instead of the crossdev-shaped
`build_config_root` grandparent path. The two cases (`--root` native offset,
`--cross`/`--prefix`) should end up sharing one "does this topology have its
own toolchain bin dir, and if so prefer it" check rather than being two
separate code paths keyed on arch difference — the arch difference is
irrelevant to *this* mechanism in real portage too.

**Two smaller gaps found doing this comparison:**
- em's explicit tool list omitted `AS` (assembler) and `PKG_CONFIG` — both
  reasonably common (`AS` for anything with hand-written asm, `PKG_CONFIG` for
  most `configure`/`meson` builds); `CPP`/`F77`/`FC`/`RC`/`DLLWRAP`/`GCJ`/`GO`/
  `HIPCXX` are genuinely rare and fine to keep deferred. **Fixed 2026-07-16**
  (both added to the explicit tool list).
- em never sets `PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR`/`PKG_CONFIG_PATH`
  itself. **Checked, no fix needed**: real crossdev's `${CTARGET}-pkg-config`
  wrapper (`/usr/bin/cross-pkg-config` on this host, symlinked per target) is
  a shell script that derives `PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR`
  itself at invocation time from `$ESYSROOT`/`$SYSROOT`/`$ROOT` (whichever is
  set) — the exact same vars em already exports correctly (`portage-repo/src/
  build/shell.rs`'s `ROOT`/`EROOT`/`SYSROOT`/`ESYSROOT`/`BROOT` block). So
  once the wrapper itself is *findable* (the PATH/broot fix below), it needs
  no further help from em.
- No gap found for `BUILD_CC`/`CC_FOR_BUILD`/`HOSTCC`: em sets none of them,
  but doesn't need to — `CBUILD` is already exported correctly, and
  `tc-getBUILD_CC`'s own PATH search for `${CBUILD}-gcc` against the host's
  real (untouched) `$PATH` already works, matching real portage's own
  reliance on that same fallback.

### 2026-07-16, correction: the native wrapper IS created — it's just a dangling symlink outside a chroot

The "confirmed live" re-check above was against *stale* test roots
(`/var/tmp/stage1-native` etc., built weeks earlier by an older `em`). Redid
it in a brand-new `crossdev-stages` sandbox with the current release binary:
`em toolchain --setup --root /root/native-test` (fresh aarch64-on-aarch64,
`em-native-0716` sandbox) **does** end with `* Switching native-compiler to
aarch64-unknown-linux-gnu-16 ... [ok]`, and both `etc/env.d/gcc/config-
aarch64-unknown-linux-gnu` (`CURRENT=aarch64-unknown-linux-gnu-16`) and the
bare `usr/bin/aarch64-unknown-linux-gnu-gcc` wrapper exist afterward — i.e.
activation genuinely runs (most likely via gcc.eclass's own `pkg_postinst`
invoking the real system `gcc-config`, correctly ROOT-scoped by em's already-
correct `ROOT`/`EROOT` env — not `em select`'s own reimplementation, whose
`post_step` is still the no-op described above).

The actual bug: `em toolchain --setup` builds a native, same-arch toolchain
using the **same crossdev-shaped layout** as a real cross target (matching
`em-stages-and-binhosts.md`'s "this gets the ROOT a host-usable `<ROOT>/usr/
bin/<chost>-gcc`, the same shape as crossdev's `<tuple>-gcc`" design) — so
gcc-config writes the wrapper as `usr/bin/<chost>-gcc -> /usr/<chost>/gcc-bin/
<ver>/<chost>-gcc`, an **absolute, ROOT-relative** symlink target. That's
correct real-portage behaviour *inside a chroot* (where `/usr/...` resolves
to the chroot's own root) — real crossdev's cross-compiler wrapper is safe
unchrooted only because it's a host-native binary living on the *real* host
`/usr/bin`, never inside a sysroot. A same-arch toolchain built *into* an
offset has no such out: outside a chroot, the symlink target resolves
against the **real host filesystem**, where `/usr/aarch64-unknown-linux-gnu/
gcc-bin/16/...` doesn't exist. Confirmed directly:

```
$ /root/native-test/usr/bin/aarch64-unknown-linux-gnu-gcc --version
bash: /root/native-test/usr/bin/aarch64-unknown-linux-gnu-gcc: No such file or directory
```

So the wrapper exists, activation ran, and it's still unusable outside a
chroot — a dangling symlink instead of a missing one, but the same
underlying fact: **em doesn't chroot, so a native `--root`'s own toolchain
has no safe way to be invoked from outside that root**, which is exactly why
`em stages --stage1 --root <R>` (verified working, same sandbox) doesn't
even try to use it — `CHOST == CBUILD` there, so the `shell.rs` cross-
toolchain block never fires, and the build falls back to the *host's own*
`gcc` via the untouched `$PATH` instead. That matches catalyst's own stage1
model (built with the seed's compiler, not a ROOT-native one) by accident,
not by the "build a fresh toolchain first, like crossdev" design
`em-stages-and-binhosts.md` actually wants — closing that gap for real still
needs the `LD_LIBRARY_PATH`-scoping (or heavier sandboxing) approach noted
above, not just a wrapper fix.

### 2026-07-16, fixed: the PATH-prepend/CC-set condition now keys off `broot`, not `build_config_root`

Implemented the "broaden the condition" plan above — but the correct broadening
turned out to be narrower and more precise than "also handle native `--root`":
the existing `chost != cbuild` block's own bin-dir source was wrong on its own
terms, independent of the (still-deferred) native-offset question. It derived
`prefix_bin` from `build_config_root` (`PORTAGE_CONFIGROOT`) via a
crossdev-sysroot-shaped grandparent-of-`<tuple>` computation — a proxy that
only coincidentally produced the right answer for the one topology
(`--prefix --cross`) it was written for, not a principled "where do
BDEPEND-class build tools live" answer.

The correct source, already established elsewhere in this codebase for
exactly this question, is `Cli::broot()`'s merge root (what `merge/mod.rs`
already computes once per run as `host_roots`, `let host_roots =
globals.broot();`): the real host `/` for a privileged `--root` offset, the
prefix itself for an unprivileged `--prefix` overlay (which cannot write the
real host). `EbuildShell` had no notion of this at all — `set_build_roots`
only ever received `config_root`/`sysroot`/`eprefix`. Fixed by:
- Adding a `build_broot: Option<Utf8PathBuf>` field to `EbuildShell`, set via
  a 4th `set_build_roots` param.
- Threading `broot: Option<&Utf8Path>` through `run_inner`/`build_and_merge`/
  `merge_binpkg`/`run_install_worker`/`run` (`ebuild.rs`), the `WorkerArgs`
  IPC struct + `--broot` worker CLI flag (`privilege.rs`/`cli.rs`), and every
  `merge/mod.rs` call site — each now passes `Some(host_roots.merge_root())`
  (a value merge/mod.rs already had in scope) instead of nothing.
- The toolchain-selection block in `shell.rs`'s `init_build_env` now computes
  `prefix_bin` from `build_broot.join("usr/bin")` directly (no
  grandparent-of-parent arithmetic needed — `broot` is already a root
  directory, unlike the old sysroot-shaped `build_config_root`).

This is a pure correctness fix, not an extension to native `--root`: for a
plain native offset the whole block still doesn't run (`chost == cbuild`
there), so the already-reverted-once native-ROOT-toolchain hazard (see the
2026-07-16 entry above) is untouched. For the cases the block *does* run
(`--cross`, `--cross --prefix`, `--cross --local`), `broot` resolves to
exactly the same directory `build_config_root`'s grandparent trick used to
land on by construction, so no regression — confirmed both by two new unit
tests (`cross_toolchain_selection_uses_broot_not_config_root`,
`cross_toolchain_selection_no_op_when_tool_unreachable`,
`portage-repo/src/build/shell/tests.rs`) and by a live check against this
host's real riscv64 crossdev toolchain (`em --target riscv64-unknown-linux-gnu
ebuild <probe> setup`): `CC`/`CXX`/`AS`/`LD`/`PKG_CONFIG` all resolved
correctly to `/usr/bin/riscv64-unknown-linux-gnu-<tool>`. Full workspace
check/clippy -D warnings/fmt/test suite clean.

**2026-07-16, further verified end-to-end in a fresh `crossdev-stages`
sandbox** (`em-prefix-0716`, arm64): `em --prefix /root/prefix-test --target
riscv64-unknown-linux-gnu crossdev --setup --buildpkg` built the full cross
toolchain (binutils → headers → gcc-stage1 → libc → gcc-stage2) and
activated it into the prefix (`usr/bin/riscv64-unknown-linux-gnu-gcc` — a
real, resolvable symlink this time, since the cross-compiler is host-native).
Then `em --prefix /root/prefix-test --target riscv64-unknown-linux-gnu
sys-libs/zlib` — a real, non-pretend build, not just the `ebuild ... setup`
probe from before — compiled and linked entirely via
`/root/prefix-test/usr/bin/riscv64-unknown-linux-gnu-gcc`/`-ar`, merging
cleanly. This is the actual end-to-end path the `broot` fix targets, not
just the mechanism in isolation.

### 2026-07-17, fixed: native `--root`'s wrapper was genuinely dangling, not just unusable "in principle"

Picked this back up to close the "native ROOT toolchain hazard" left open above.
Re-confirmed the 2026-07-16 finding still held (a fresh check against
`regress-toolchain-root`, a `--root` native toolchain built by
`regression-matrix.sh`): `readlink -f usr/bin/aarch64-unknown-linux-gnu-gcc`
failed (exit 1) — the wrapper existed but its symlink target
(`/usr/aarch64-unknown-linux-gnu/gcc-bin/16/...`, no ROOT prefix) resolves
against the real host filesystem, not the offset, because real
`gcc-config`/`binutils-config` (invoked for real by toolchain.eclass's own
`pkg_postinst`, correctly ROOT-scoped by em's `ROOT`/`EROOT` env) has no
EPREFIX concept for a plain `ROOT=` offset — unlike `--prefix`/`--local`
(EPREFIX set), where the same real tool already writes a correctly re-rooted,
resolvable symlink (confirmed working with no fix needed).

**Fix**: gave `em toolchain --setup`'s native path a real `post_step` (it was
`|_| Ok(())`, exactly as this doc's "NOT done; the open item" line said) —
`activate_native_toolchain` (`crossdev/mod.rs`), the direct native twin of
the existing `activate_toolchain` cross helper: after each step, if the
step's atom is `binutils`/`gcc`, re-activate via `select::activate_binutils`/
`activate_compiler` against `outer_roots().with_own_config_root_if_self_contained()`,
using `select::get_chost(globals)` as the tuple (native means `CHOST ==
CBUILD`, and `--root`/`--prefix`/`--local` all inherit the host's own
profile/make.conf for a native build — no separate per-topology CHOST is
ever written for native, unlike the cross sysroot's own `CHOST=<tuple>`).
This re-activation runs `em`'s own `install_wrappers`, which — unlike real
gcc-config — re-roots `GCC_PATH`/`LDPATH`/etc under `env_d::eprefix(roots)`
before creating the `usr/bin/<CHOST>-*` symlinks, so for the self-contained
`--root` case (`eprefix(roots) == the offset itself`, via
`with_own_config_root_if_self_contained`) the result is a fully offset-scoped
absolute symlink target, not the real gcc-config's plain `/usr/...` one.

**Verified live** against the already-built `regress-toolchain-root` fixture
by re-running `toolchain --setup` (idempotent re-invocation re-triggers
`post_step` for the packages already present): both wrappers now resolve —
```
$ readlink -f usr/bin/aarch64-unknown-linux-gnu-gcc
/root/regress-toolchain-root/usr/aarch64-unknown-linux-gnu/gcc-bin/16/aarch64-unknown-linux-gnu-gcc
$ usr/bin/aarch64-unknown-linux-gnu-gcc --version
aarch64-unknown-linux-gnu-gcc (Gentoo 16.1.1_p20260613 p3) ...
```
Same for `usr/bin/aarch64-unknown-linux-gnu-{as,ld}` (binutils). `--prefix`
re-checked too (already worked before this fix, since real gcc-config is
EPREFIX-aware there) — no regression, activation just runs twice
(idempotent). Full workspace clippy -D warnings/build clean.

**Still explicitly not addressed** (per the 2026-07-16 entries above, and out
of scope for this fix): the wrapper now *exists and works when invoked
directly*, but nothing in the native (`chost == cbuild`) build path prefers
it over the host's own `gcc` on `$PATH` — `shell.rs`'s cross-toolchain
selection block is still gated on `chost != cbuild`. `em stages --stage1
--root <R>` (native) still silently uses the host's compiler, matching
catalyst's seed-compiler model by accident. Broadening that gate to also
prefer a same-arch offset's own toolchain dir (this doc's still-open
"broadening the condition" plan, distinct from today's wrapper fix) remains
undone.

## Open: clang linker config (Option B)

`-fuse-ld=` lives in `/etc/clang/<SLOT>/gentoo-linker.cfg`, not env.d. Decide:
fold into `em select linker`, a `em select clang linker` subcommand, or flags on
`em select clang set`. Low priority.
