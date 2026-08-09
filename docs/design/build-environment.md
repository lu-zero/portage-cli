# The ebuild build shell's environment: sourced-env sweep vs. hand-listed exports

`em` runs each package's phases in an in-process [`brush`](https://github.com/reubeno/brush)
shell (`portage-repo/src/build/shell.rs`'s `EbuildShell`). Two different kinds
of code read that shell's variables, and they see them very differently:

- **em's own Rust builtins** (`econf`, `emake`, the `do*`/`new*` install
  helpers, …) read brush's variable table *in-process*
  (`shell.env_str("CHOST")` etc.) — export status is irrelevant to them.
- **Anything the ebuild/eclass shell code spawns as a real subprocess**
  (`bash "${FILESDIR}/gentoo.config"`, a raw `$(some-tool)`, …) only inherits
  variables brush has explicitly marked *exported* — exactly like real bash:
  a plain `VAR=value` assignment (which is what `source`ing make.conf/profile
  produces) is **not** inherited by child processes on its own.

## Real portage doesn't hand-list what to export

Real portage's `config.environ()` (`portage/package/ebuild/config.py`) is a
**deny-list**: it exports its entire settings dict — every make.conf/profile
value (`CHOST`, `CBUILD`, `ELIBC`, `KERNEL`, `MULTILIB_ABIS`, `DEFAULT_ABI`,
…) — minus a small, explicit `special_env_vars.environ_filter` (internal
portage-only knobs: `ACCEPT_KEYWORDS`, `CONFIG_PROTECT`,
`EMERGE_DEFAULT_OPTS`, `DEPEND`/`RDEPEND`, …). This works because portage
constructs the *entire OS-level process environment* of the ebuild-running
bash before that bash even starts — every sourced value is implicitly
"exported" from process birth, with zero explicit `export` statements needed.

`em` instead sources make.conf/profile into an **already-running** brush
shell (`ProfileStack::configure_shell` → `source_incremental` →
`EbuildShell::source_make_defaults`/`source_env_file`), so those values start
out as plain, non-exported shell variables — correct bash semantics for
`source`, but invisible to any real subprocess until something explicitly
exports them.

## What this file used to do wrong (and the fix)

`init_build_env` used to carry a **hand-maintained allow-list**
(`export CATEGORY PN PV … MOPREFIX ABI CONF_LIBDIR`) of the only variables it
would ever export. That list only covers the identity variables *em itself
synthesizes* per package (`CATEGORY`, `PF`, `S`, `T`, `D`, `EBUILD_PHASE`, …) —
it never covered arbitrary profile/make.conf-derived variables, so anything
not explicitly named was invisible to a real subprocess. This is exactly how
`CHOST` went missing: `dev-libs/openssl`'s own `bash
"${FILESDIR}/gentoo.config"` subshell saw no `$CHOST` at all, so its
`Configure` fell back to `uname`-based autodetection and silently picked the
**build host's** real kernel architecture under a `riscv64` `--target` build —
while `CC`/`CFLAGS` (forwarded explicitly by the `econf` Rust builtin,
bypassing export entirely) were already correctly cross-targeted. The
mismatch produced a build that used the right compiler but the wrong
assembly-optimization target.

The list only ever grew reactively, one silently-broken package at a time —
the same latent bug exists for any other profile-derived variable
(`ELIBC`, `KERNEL`, `USERLAND`, `MULTILIB_ABIS`, `DEFAULT_ABI`,
`PKG_CONFIG_PATH`, …) some eclass expects as a real, subprocess-inherited env
var.

**Fix**: `EbuildShell::export_sourced_env` (`portage-repo/src/build/shell.rs`)
replicates portage's actual model at em's own layer — it exports **every**
variable currently in the shell's environment (via brush's `Env::iter()`,
which returns all vars regardless of export flag), skipping a small denylist
of genuinely bash/brush-internal names (`is_bash_internal_var`: `_`,
`PIPESTATUS`, `BASH_*`, `FUNCNAME`, `LINENO`, `RANDOM`, `SECONDS`, `PPID`,
`SHELLOPTS`, `BASHOPTS`, `GROUPS`, `HISTCMD` — bash mechanics that either
can't be exported or would error as readonly). It flips each variable's
export bit directly via brush's `ResolvedVarRefMut::base_var_mut` — pure
metadata mutation, not a generated `export a b c …` string re-parsed by the
interpreter. It's called from `apply_profile_env` in
`portage-cli/src/ebuild.rs` right after profile/make.conf sourcing, and again
after the package.env sourcing loop — the two points where "config we just
sourced from files" changes.

`init_build_env`'s original identity-var list stays as-is. It's not the
brittle part: those variables are computed by em itself (not sourced from a
profile file), so they can never come "for free" from the sweep above and
genuinely need their own explicit export.

## Why not go further

- **Full parity with portage's `environ_filter`**: not needed — that deny-list
  exists to keep portage's *internal Python config-object* keys (hundreds of
  `EMERGE_*`/`PORTAGE_*` knobs with no em equivalent) out of the ebuild
  environment. `em`'s brush shell doesn't carry that internal cruft, so the
  small bash-mechanics denylist is the correctly-scoped equivalent.
- **`set -a` (allexport)** around the sourcing calls, instead of an explicit
  post-hoc sweep: real bash would do this automatically, but it's unverified
  whether brush's `source_script` honors the `-a` shopt identically for
  assignments made *during* a sourced script. The explicit sweep doesn't
  depend on that and is trivially unit-testable
  (`export_sourced_env_reaches_a_real_subprocess` in `shell.rs`).
