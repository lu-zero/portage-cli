# `em setup --local` host prerequisites + `--extra-path`

Status: 🟢 steps 1–7 landed 2026-08-13 (`26cffca`). Verified on Linux and
now real FreeBSD 14.4 (2026-08-14, see below): a full
`em --local DIR setup` completes, the check passes/farms correctly, and
`package.provided` comes out complete. **The macOS half is unexercised** —
`host_tools/macos.rs` has never run.

Live-verified 2026-08-14 on a fresh crossdev-stages Gentoo sandbox and a
real, unmodified Debian 12 container: each missing hard tool produced the
clean pre-flight message, not an opaque mid-phase failure; a real
`sys-apps/baselayout` merge completed on both. Found two real gaps in the
same "opaque failure" class the hard-tool table exists to prevent, both
now closed:
- `bzip2`/`xz` missing ⇒ `tar (child): bzip2: Cannot exec` mid-unpack
  (most Gentoo distfiles are `.tar.bz2`/`.tar.xz`). Added to the hard-tool
  table below.
- `git` missing ⇒ raw OS error from `src_unpack`'s repo sync. Not added to
  the table — building with `--features sync-gix` (pure-Rust git, off by
  default) sidesteps needing a host `git` at all, which is the better fix
  if this is ever hit for real.

**Live-verified 2026-08-14 on real FreeBSD 14.4 aarch64** (a genuine
qemu VM, bypassing incus — see `incus-docker-firewall-and-idmap` memory
for why): the actual target class this feature exists for, no native GNU
sed/grep/make at all. Full `em setup --local` bootstrap now completes
end-to-end including a real `sys-apps/baselayout` merge, after three more
real bugs found+fixed:
- `resolve_raw_path` (`ec833da`): banner-less hard tools never fell back
  to the raw PATH the way sed/grep did — wrong on FreeBSD, where
  `/usr/local` is `pkg`'s own canonical prefix, not a shadow copy.
- `make` needed a GNU banner too (`d7250f9`): base FreeBSD `make` is BSD
  make, a different incompatible implementation a plain PATH search finds
  before `gmake`.
- `mkdir_p_mode` root-component bug (`7217f81`) — **not a host-tool-check
  bug at all**, a real `em` install-helper bug: walking an absolute path's
  components, `mkdir("/")` gets called for every `do*`/`new*` helper
  (harmless `EEXIST` on Linux, fatal `EISDIR` on FreeBSD). Found because
  baselayout's `dosym`/`doins`/`dodoc` are unconditional, so this broke
  the very first real package merge attempted on FreeBSD.

The macOS half remains unexercised.
Companion: [`todo/local-bootstrap.md`](./local-bootstrap.md),
[`todo/local-bootstrap-provided.md`](./local-bootstrap-provided.md)

`em setup --local` bootstraps a standalone prefix from an empty VDB, so the
host's own tools run every early phase. Two of them must be **GNU**:
`sys-apps/baselayout`'s `src_install` runs a bare `sed -i` (BSD sed needs
`-i ''`), `prefix.eclass`'s `hprefixify` runs `sed -r -i`, glibc's
`src_install` runs `grep -Z`. On a non-GNU host that surfaces as an opaque
mid-phase failure today.

Meanwhile `EbuildShell::init_build_env` deliberately strips `$HOME` and
`/usr/local` from the phase PATH (a `~/.local/bin` uv python once broke
distutils-r1 wheel builds), which is exactly where a hand-installed or
Homebrew GNU sed lives.

**Scope: `--local` only.**

| Mode | Why it is out of scope |
|------|------------------------|
| `--prefix` | Layered on a host Gentoo: borrows the host VDB for BDEPEND and already symlinks `HOST_BASE_TOOLS` (incl. `grep`) into `${EPREFIX}/usr/bin`. Its sed/grep are the host's, GNU by construction. |
| `--root` | Self-contained offset, no EPREFIX, different config topology. Same empty-VDB class, but not this task — `stages`/`crossdev` must see no change. |

---

## The anti-leak contract

| Layer | Knows | Enforced by |
|-------|-------|-------------|
| `portage-repo` | "prepend these dirs to the sanitised PATH" | No OS, tool, or bootstrap knowledge in the crate. Empty ⇒ today's behaviour bit-identical |
| `em setup --extra-path DIR` | user-supplied dirs | On the `setup` subcommand only, never global; errors without `--local` |
| `setup/host_tools/` | GNU-ness, Homebrew, MacPorts | `mod host_tools;` **private** inside `setup/` ⇒ any other caller is a compile error. Per-OS candidates behind `#[cfg]`; the check itself is generic and runs on Linux too |

Detection produces the **default value of `--extra-path`** — one knob, one
value, one code path. User-supplied dirs are also searched as candidates, so
`--extra-path ~/.local/bin` is itself a valid fix.

---

## Steps

1. **Delete `portage-cli/src/host_tools.rs`** — untracked, never compiled
   (not in `lib.rs`, imports a `portage_repo::EnvOverride` that does not
   exist), hand-copies the PATH sanitiser, and breaks the Unslop Rules.

2. **`setup/mod.rs` streamline**, no behaviour change: one `Mode` enum
   (`Local`/`Prefix`/`RootOffset`) derived once from `Roots`, replacing the
   duplicated `has_eprefix`/`base_eq_target` computation in `run`/`preview`/
   `bootstrap` and the trailing `(bool, bool)` match. Single hook point for
   step 5.

3. **`portage-repo/src/build/shell.rs`**: extract the PATH sanitiser into a
   pure `pub fn` (raw PATH + home → dirs); `init_build_env` calls it; add
   `set_extra_path(Vec<Utf8PathBuf>)`, prepended ahead of it. Kills the
   hand-sync that step 1 deletes.

4. **`setup/host_tools/`** — per-tool ladder, every candidate verified
   behaviourally by its `--version` banner, never trusted by location:
   1. `--extra-path` dirs
   2. sanitised phase PATH (Linux stops here)
   3. raw process PATH incl. `$HOME` / `/usr/local`
   4. system-specific: `brew --prefix <formula>` when `brew` is present, else
      the two standard Homebrew prefixes' `opt/<formula>/libexec/gnubin`;
      MacPorts `/opt/local/bin/g<tool>`

   Prefixed-name-only hits (`gsed`, `ggrep`) are symlinked under the correct
   name into `$XDG_STATE_HOME/em/bootstrap-bin` (`xdg::em_state_dir()`), and
   that dir becomes the default `--extra-path`.

5. **Prerequisite report**, run before any file is written. `provided.rs`'s
   `TIER1` gains a requirement class:

   | Class | Tools | Missing ⇒ |
   |-------|-------|-----------|
   | Hard | C/C++ compiler, `make`, `python3`, `tar`, `bzip2`, `xz`, `patch`, `awk`; GNU-specific `sed`/`grep` | hard fail, named, with the fix |
   | Buildable | the rest of the table | omit from `package.provided` (never an invented version) + info that the prefix will build it |

   Present tools keep `pick_version` as-is. `provided.rs` probes through the
   resolved binaries, closing the gap where it can claim a `sed` no phase can
   see.

6. **Wiring, setup-only**: carrier threaded `EmergeOpts` → `ResolvedEmergeOpts`
   → `MergePlanRequest` → `MergeRun` → `ActionFlags` → `RootContext` →
   `RunInner`/`WorkerStep` → `WorkerArgs` → `__worker` CLI → `dispatch.rs` →
   `set_extra_path`, mirroring `self_contained_bootstrap`. All six existing
   `EmergeOpts` sites write the empty opt-out visibly.

7. **Tests + `cargo fmt`/clippy**: pure-function tests for the sanitiser and
   the ladder (fake `--version` scripts in tempdirs), a test that a non-setup
   emerge carries an empty value, existing setup tests unchanged.

---

## Decisions (settled, do not re-open)

- `--extra-path` is the knob, at CLI and `portage-repo` level. Detection fills
  its default; it is not a second mechanism.
- No symlink farm inside the prefix. Prefixed-name-only tools go to the XDG
  state dir.
- Missing **hard** tool ⇒ fail; missing **buildable** tool ⇒ omit + info. The
  old "claim the oldest tree version" fallback stays only for a host tool that
  exists but is older than everything in the tree.
- `em setup --extra-path` without `--local` is an error, not a silent no-op.

## Follow-ups found while doing this

- `pick_version` can pick a **live (`9999`) version** for a provided entry:
  on this host `python3 --version` is 3.13.12 and the closest tree version
  `<=` that is `dev-lang/python-3.12.9999`. Harmless for version ops, but
  claiming a live ebuild as system-supplied is wrong on its face. Pre-dates
  this work; excluding live versions from the candidate set is a two-line
  fix, not taken without a decision.
- Nothing checks these prerequisites again at `em --local toolchain
  --setup`, which is the next step that needs the same GNU sed/grep
  (`hprefixify`, glibc). The carrier makes it a one-line opt-in when that
  is wanted.

## Known limitation (record, don't solve)

Under the Linux hakoniwa backend the XDG farm dir sits outside the targeted
bind set. Irrelevant today — Linux hosts stop at rung 2 with their own GNU
tools, macOS uses pseudoroot's full-FS view — but it is where this bites first
if the farm ever becomes load-bearing on Linux.
