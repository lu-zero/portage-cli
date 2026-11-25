# emerge and crossdev parity, both directions

Status: 🔴 backlog, filed 2026-11-19 from an audit against the host tools
(Portage 3.0.83, crossdev-20260623). Move a row to ✅ when its item lands.

Two directions are kept separate on purpose:

- **Missing** — the real tool does it and `em` does not. This is the implementation queue.
- **Extra** — `em` does it and the real tool does not. Keep these; chasing parity must not drop one.

Behaviour here was summarized from the installed tools, never copied: their
source is GPL and this tree is MIT.

## Missing from `emerge`

| Item | Severity | Status | Detail |
|---|---|---|---|
| `EMERGE_DEFAULT_OPTS`, `EM_DEFAULT_OPTS`, `--ignore-default-opts` | high | ✅ 2026-11-21 | [[default-opts-and-set-search]] |
| `--clean`: keep the most recently installed version in each slot | medium | 🔴 | `-P/--prune` keeps the highest version and ignores slots |
| Autounmask family: `--autounmask-use/-license/-keep-keywords/-keep-masks/-unrestricted-atoms/-backtrack/-continue/-only` | medium | 🔴 | `--autounmask` is accepted but ignored; only `-write` acts |
| Binpkg trust and filters: `--binpkg-respect-use`, `--binpkg-changed-deps`, `--usepkg-include/-exclude`, `--getbinpkg-include/-exclude`, `--pkg-format` | medium | 🔴 | `-k/-K/-g/-G` take any available binpkg |
| Emerge-level actions: `--check-news`, `--read-news`, `--list-sets`, `--metadata`, `--config`, `--status`, `--rage-clean`, `--sync-submodule` | medium | 🔴 | most exist as separate applets, not as emerge flags |
| Rebuild selection: `--changed-deps`, `--changed-slot`, `--rebuilt-binaries[-timestamp]`, `--newrepo`, `--nobindeps` | medium-low | 🔴 | only `-N/--newuse` and `-U/--changed-use` exist |
| Per-invocation overrides: `--accept-properties`, `--accept-restrict`, `--dynamic-deps`, `--with-test-deps`, `--use-ebuild-visibility`, `--ignore-world`, `--fail-clean` | medium-low | 🔴 | config only today |
| Scheduler knobs: `--merge-wait-scope`, `--implicit-system-deps`, `--jobs-tmpdir-require-free-gb` | low-medium | 🔴 | merges are serialised |
| `-r/--resume` pins a merge list; em re-solves the saved atoms | medium | 🔴 | two history slots are kept |
| `--info [atom]`; em treats the atom as a merge target | low | 🔴 | `info_wins` requires zero atoms |
| `--color y\|n` alongside `auto\|always\|never` | low-medium | 🔴 | real scripts carry `y`/`n` |
| `--sysroot` / `SYSROOT` | low | ✅ | names the tree the compiler searches |
| Minor flags: `-A/--alert`, `--ask-enter-invalid`, `-d/--debug`, `--circular-deps-report`, `--select/-w`, `--onlydeps-with-{r,i}deps`, `--skipfirst`, `--quiet-unmerge-warn` | low | 🔴 | `-v`/`RUST_LOG` instead of `-d` |

## Missing from `em crossdev`

| Item | Severity | Status | Detail |
|---|---|---|---|
| Partial stages `-s0..-s4`, `--ex-only` | high | ✅ 2026-11-25 | `--stage N`; [[em-crossdev-stages-and-clean]] |
| `-C/--clean`: uninstall a target's config, overlay, wrappers and sysroot | high | ✅ 2026-11-25 | `-T <tuple> -C`; [[em-crossdev-stages-and-clean]] |
| Component versions `--b/--g/--k/--l` and LLVM equivalents with depend atoms; `-S/--stable` | high | 🟡 partly | spec pins exist, no CLI ranges or stable selector; [[crossdev-target]], [[cross-sysroot-spec]] |
| Per-component `--[bdgkl]cat/pkg/env` overrides | medium | 🔴 | component names are fixed by the tuple |
| `--ex-gcc` extra frontends (ada/d/fortran/...) | medium | 🔴 | only `--ex-gdb` and `--ex-pkg` |
| `-A/--abis`, `--host-abi` | medium | 🔴 | see [[crossdev-stage1-abi-x86-32-multilib-mismatch]] |
| `-f/--force` | medium | 🔴 | `-f` already means `--fetchonly` in em |
| `-P/--portage` pass-through, `--show-fail-log` | low-medium | 🔴 | fixed merge flags, no fail-log dump |
| `--overlays`, `--ov-output`, per-component overlays | medium | 🟡 decided | auto-discovery instead; [[crossdev-target]] |
| Short letters collide with emerge's (`-b`, `-g`, `-k`, `-r`, `-u`, `-a`, `-p`, `-t`) | medium | 🟡 decide | real crossdev command lines are not portable |
| `--show-target-cfg` emits shell-evaluable `key=value` | low-medium | 🟡 decided | em prints a human table |

## Extra: `em` additions to keep

| Addition | What it is | Documented in |
|---|---|---|
| Structured activity bus | `--activity-fd`/`--activity-jsonl`, versioned events, `em log current/list/time/predict`, ETA history | [[activity-status]], `docs/user/activity.md` |
| Privilege backends and worker split | `--privilege`, pseudoroot/hakoniwa/sudo/none, compile unwrapped and install in `em __worker` | `docs/design/worker-build-tree.md` |
| `--cycle-split` | Builds a package twice to break a hard cycle; capped at 8 | `docs/design/architecture.md` |
| `--json` depgraph and `--info --json` | Structured plan with edges, flags and cycle data | `docs/user/cli/emerge.md` |
| Cross-target TOML spec | `--print-spec`, `--target file.toml`, pins, `[package-use]`, per-toolchain env | `docs/design/cross-sysroot-spec.md` |
| Resumable bootstrap milestones | `--setup -r` skips completed toolchain steps | `docs/design/cross-sysroot-spec.md` |
| Root topology | `--local`, `--config-root`, `--target`, VDB union planning | `docs/design/root-topology.md`, `docs/user/root-model.md` |
| em-only applets | `active`, `setup`, `toolchain`, `stages`, `maint binpkg` | README applet table |
| Binpkg identity | CPV ∩ USE ∩ CHOST ∩ ISA hash, multi-instance PKGDIR, `fingerprint` | `docs/user/binhost.md` |
| `--autosolve-use` | Solver chooses flags to satisfy REQUIRED_USE | `docs/design/architecture.md` |
| `ResolveMode::Ask` | Numbered prompt for an ambiguous bare package name | `docs/design/architecture.md` |
| `FEATURES=candy` | Per-package phase bars with ETA, not emerge's spinner | `docs/user/activity.md` |
| Opt-in `--emergelog`, root-aware `clean`/`etc`/`read` | emerge.log format off by default; sidecars follow the selected root | `docs/user/activity.md` |

Not em-only, do not track as additions: `--fuzzy-search`, `--search-similarity`,
`--regex-search-auto`, `--ex-pkg`, `--ex-gdb`, `-L/--llvm`, `--init-target`,
PTY phase output, and the `--root`/`--prefix`/`--config-root` flag names.
The root and cross *models* behind those names are additions; the spellings are not.

## Decided divergences — do not re-implement

- `--keep-going` continues past failures. `--autounmask` is a no-op; `--complete-graph`
  defaults to repairing installed packages.
- crossdev config is regenerated rather than preserved from `/etc/portage/crossdev`;
  no per-target wrapper scripts; narrower tuple set; overlay auto-discovery;
  `--show-target-cfg` is a human table.

## Order

1. Rows marked 🟡 decide first: short-option collisions and the two `--show-target-cfg`
   output forms need a decision, not code.
2. Doc fixes: `--ex-gdb`/`--ex-pkg` examples write config with `--init-target`;
   a separate `--setup` builds them.
3. User-visible gaps go in `docs/user/applets.md` and `docs/user/crossdev.md`;
   this note is the implementation queue.
4. High-severity rows before minor flags.
