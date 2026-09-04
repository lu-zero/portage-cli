# Stages setup and testing (with crossdev-stages)

Operator-facing how-to for bootstrapping a
ROOT with `em` and validating it inside a
[crossdev-stages](https://github.com/lu-zero/crossdev-stages) sandbox.
Stage **specs** (catalyst-style stage4 recipes with package lists, USE
overlays, bindist knobs, …) are **out of scope** — that is a larger design
problem. This doc covers what exists today: toolchain → stage1 → stage3.

Related:

- Root topology: [`root-topology.md`](../design/root-topology.md), [`root-model.md`](./root-model.md)
- Prefix / multi-root path experiment: [`em-prefix-experiment.md`](../design/em-prefix-experiment.md)
- Crossdev CLI: [`crossdev.md`](./crossdev.md)
- Live harness notes: [`test-scripts/README.md`](../../test-scripts/README.md), [`testing.md`](../design/testing.md)

---

## Mental model

`em` splits **toolchain** from **stage production** (same split as
catalyst / crossdev-stages):

| Step | Command | What it does | USE |
|------|---------|--------------|-----|
| **Toolchain** | `em toolchain --setup --root R` | Native self-hosting compiler + libc into `R`: baselayout → binutils → headers → glibc → gcc | step-local overrides inside the staged driver (not a stage recipe) |
| **Cross toolchain** | `em crossdev --target T --setup` | Cross tools into `/usr/<T>` (host-side) + sysroot skeleton | eclass/crossdev model |
| **Stage1** | `em stages --stage1 --root R` | Bootstrap set: baselayout (`USE=build`, `--nodeps`) then profile `packages.build` | **forced** conf-layer `USE="-* build ${BOOTSTRAP_USE}"`; **`--autosolve-use` always on** (IUSE `+` defaults preferred when `-*` wiped them) |
| **Stage3** | `em stages --stage3 --root R` | Emptytree rebuild of `@system` | **none injected** — profile + ROOT `make.conf` + `package.use` only |
| **Stage4** | *(not implemented)* | Would need a stage specification language | TBD |

There is **no stage2** in the em path (crossdev model: toolchain is
built fresh; catalyst’s stage2 is the “rebuild toolchain inside the
chroot” step we skip).

Stage3 is intentionally thin: one `emerge_atoms` call with forced
`-e -uD --with-bdeps` and `-b` (seed PKGDIR), atoms `@system`, empty
`use_override`. Same shape as catalyst’s
`targets/stage3/chroot.sh` (`run_merge -e --update --deep --with-bdeps=y @system`).

```
                    ┌─────────────────┐
   host seed CC ──► │ toolchain --setup│ ──► R has <chost>-gcc + libc
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │ stages --stage1 │ ──► packages.build under USE=-* build
                    └────────┬────────┘
                             │
                    ┌────────▼────────┐
                    │ stages --stage3 │ ──► emptytree @system (USE from ROOT)
                    └─────────────────┘
```

For **cross** targets, “toolchain” is `crossdev --setup`; stage1/stage3 into
the sysroot use global `--target T` (sysroot substitution). See
[`crossdev.md`](./crossdev.md).

---

## Prerequisites

### On the development host

```text
Sources/
  portage-cli/        # this repo — cargo build --release -p portage-cli
  crossdev-stages/    # sibling checkout (sandbox / board pipeline)
  brush/              # optional path patch for day-to-day em development
```

- Linux with user namespaces (for crossdev-stages / hakoniwa sandboxes).
- Network access for stage3 tarball + distfiles (first sandbox setup).
- A built `em`: `cargo build --release -p portage-cli` → `target/release/em`.

### crossdev-stages roles (do not confuse)

| Term | Meaning |
|------|---------|
| **Sandbox** | Unpacked **host-arch** stage3 under `~/.cache/crossdev-stages/sandboxes/<name>/`. Isolated build environment; run `em` **inside** it via `sandbox run`. |
| **Target** | Cross-compiled root for a foreign arch (crossdev-stages’ own stage pipeline). Separate from `em stages` into `--root`. |
| **Board** | Hardware image recipe (`boards/<name>/`); image pipeline on top of a target. |

This doc uses sandboxes as a **clean host root + chroot/userns** so live
`em` builds do not trash the developer machine. You can also run the same
`em` commands on a bare host under `--root` / `--prefix` / `--local` without
crossdev-stages (see topology matrix below).

---

## Topology flags (where things land)

| Flag | Config | Install / VDB | Typical use |
|------|--------|---------------|-------------|
| `--root R` | `R` (unless `--config-root`) | `R` | Stage / offset: empty tree, full closure |
| `--prefix P` | host `/` by default | `P` | Overlay on host tools (seed compiler model) |
| `--local [D]` | `D` (default `~/.gentoo`) | `D` | Standalone prefix; own BROOT |
| bare | host | host | Dogfood host (careful) |
| `--target T` | sysroot under EROOT | sysroot for target packages | Cross install into `/usr/<T>` |

Stage builds into `/` are rejected (`em stages` bails if merge root is `/`).

---

## Full setup — native (em only)

Assume a dedicated directory that will become the stage root (host path or
path inside a sandbox).

### 1. Toolchain

```bash
em toolchain --root /path/to/stage \
   --setup \
   --autounmask-write \
   --jobs 8 \
   --privilege auto   # or EM_PRIVILEGE=pseudoroot / sudo
```

- Produces a self-hosting native toolchain in the root
  (`baselayout → binutils → os-headers → glibc → gcc`).
- Activates wrappers via `em select` post-steps.
- Prefer **`--root`** or **`--prefix`** for full toolchains; **`--local`**
  still hits known hard-cycle partials (see regression-matrix “KNOWN-PARTIAL”).

### 2. Stage1

```bash
em stages --root /path/to/stage \
   --stage1 \
   --autosolve-use \
   --autounmask-write \
   --jobs 8
```

- **USE:** forced to catalyst-style bootstrap:
  - step baselayout: `USE=build`, `--nodeps`
  - step packages.build: `USE="-* build ${BOOTSTRAP_USE}"` (profile
    `BOOTSTRAP_USE` re-applied after `-*` since it isn't itself part of the
    profile's `USE` fold, so `em` splices it back in explicitly, same as
    catalyst); merge always uses `--autosolve-use` so REQUIRED_USE after the
    wipe (e.g. `app-alternatives` `^^`) is ceded, preferring ebuild `+` IUSE
    defaults
- Seeds **PKGDIR** with `-b` by default (stage runs force buildpkg).
- Requires a working toolchain already in the root.

### 3. Stage3

```bash
em stages --root /path/to/stage \
   --stage3 \
   --autosolve-use \
   --autounmask-write \
   --jobs 8
```

- **USE: not overridden** — whatever the stage root’s profile + `make.conf`
  + `package.use` already say. If you need lean/bindist USE, write it into
  the root’s config **before** this step.
- Forces: `--emptytree` (`-e`), `--update` (`-u`), `--deep` (`-D`),
  `--with-bdeps`, and `-b` (PKGDIR seed).
- Target set: **`@system` only** (not `@world`).

Or both stage steps in one invocation:

```bash
em stages --root /path/to/stage --stage1 --stage3 --autosolve-use --jobs 8
```

### 4. Pretend first

Always smoke the resolver before a multi-hour build:

```bash
em stages --root /path/to/stage --stage1 -p --autosolve-use
em stages --root /path/to/stage --stage3 -p --autosolve-use
# or emptytree canary without stages:
em --root /path/to/stage -pe @system --with-bdeps -p
```

Exit code 1 with a printed “changes needed” block means
`--autounmask-write` / config fix, then re-run (same as emerge).

---

## Full setup — cross (em + `--target`)

Example tuple: `riscv64-unknown-linux-gnu`.

```bash
T=riscv64-unknown-linux-gnu
ROOT=/path/to/cross-root   # board destination stages installs into

# 1) Cross toolchain + sysroot skeleton (implies --init-target)
#    No --root here: crossdev never takes one (none of its actions read
#    it) — bare crossdev builds the toolchain at the real host /usr/$T.
em crossdev --target "$T" --setup --autounmask-write --jobs 8

# 2) Stage1 *into the sysroot* (uses --target substitution)
em stages --target "$T" --root "$ROOT" --stage1 --autosolve-use --jobs 8

# 3) Stage3 emptytree @system into the sysroot
em stages --target "$T" --root "$ROOT" --stage3 --autosolve-use --jobs 8
```

Canaries (after toolchain is up):

```bash
# Foreign-arch package.accept_keywords path (host file often has
#   cross-…/linux-headers riscv ~riscv -arm64 -~arm64)
em -p cross-riscv64-unknown-linux-gnu/linux-headers

# Cross emptytree of a leaf (historical task #17 class)
em --root "$ROOT" --target "$T" -pe sys-apps/systemd-utils --with-bdeps
```

Do **not** run `em setup --target T` before `crossdev --setup`: with
`--target` set, `setup` writes into the **sysroot** and can poison
`ACCEPT_KEYWORDS` with the host arch (caught live; documented in
`regression-matrix.sh`).

---

## Using crossdev-stages as the test harness

### Install / build crossdev-stages

```bash
cd ~/Sources/crossdev-stages
cargo build --release
# binary: target/release/crossdev-stages
```

Default workspace: `~/.cache/crossdev-stages/`.

### Create and prepare a sandbox

```bash
CDS=~/Sources/crossdev-stages/target/release/crossdev-stages
NAME=em-stages-draft   # pick a unique name; avoid reusing dirty sandboxes

$CDS sandbox setup --name "$NAME"
$CDS sandbox prepare --name "$NAME" --bare
# optional: install host crossdev tooling for board pipelines
# $CDS sandbox crossdev --name "$NAME" --arch riscv64 --board k1
```

### Drop a fresh `em` into the sandbox

Scripts use copy-then-rename to avoid “Text file busy”:

```bash
REPO=~/Sources/portage-cli
cargo build --release -p portage-cli --manifest-path "$REPO/Cargo.toml"
SB=~/.cache/crossdev-stages/sandboxes/$NAME/root

cp "$REPO/target/release/em" "$SB/em-bin.new"
chmod +x "$SB/em-bin.new"
mv "$SB/em-bin.new" "$SB/em-bin"
```

### Run commands inside the sandbox

```bash
$CDS sandbox run --name "$NAME" -- /root/em-bin --version

# Native full stack (paths are *inside* the sandbox)
$CDS sandbox run --name "$NAME" -- \
  /root/em-bin toolchain --root /root/my-stage --setup --autounmask-write --jobs 8

$CDS sandbox run --name "$NAME" -- \
  /root/em-bin stages --root /root/my-stage --stage1 --autosolve-use --jobs 8

$CDS sandbox run --name "$NAME" -- \
  /root/em-bin stages --root /root/my-stage --stage3 -p --autosolve-use
# drop -p for a real stage3 (long)
```

Interactive shell:

```bash
$CDS sandbox enter --name "$NAME"
```

### Cleanup

```bash
$CDS sandbox destroy --name "$NAME"
# or bulk: $CDS maint clean --sandboxes
```

**Gotcha:** reusing an old sandbox without destroy/setup has caused
misleading failures (stale make.conf, half-built trees, wrong
`ACCEPT_KEYWORDS`). Prefer destroy → setup → prepare for each campaign;
`test-scripts/` does this by design.

---

## Automated / semi-automated tests in this repo

| Harness | What it covers | Typical command |
|---------|----------------|-----------------|
| `test-scripts/regression-matrix.sh` | Toolchain × `--root`/`--prefix`/`--local`; stages `--stage1` `-p` (and real with `--full`); crossdev `--setup` matrix | `./test-scripts/regression-matrix.sh` / `--full --jobs 16` |
| `test-scripts/test-crossdev-flavours.sh` | riscv64 matrix wrapper + same-arch cross reject | `./test-scripts/test-crossdev-flavours.sh --full` |
| `test-scripts/test-binpkg-identity-sandbox.sh` | Multi-`build_env_key` binpkg reuse | `./test-scripts/test-binpkg-identity-sandbox.sh` |
| `test-scripts/test-crossdev-binpkg-sandbox.sh` | Host/target CHOST dual PKGDIR | `./test-scripts/test-crossdev-binpkg-sandbox.sh` |
| Unit / CI | solvers, USE, keywords, loadavg, clean FEATURES, … | see [`AGENTS.md`](../../AGENTS.md) / [`testing.md`](../design/testing.md) |

Env overrides shared by the scripts:

```bash
export CROSSDEV_STAGES_DIR=~/Sources/crossdev-stages
export SANDBOX=em-regression          # default name for matrix
export CROSS_TARGET=riscv64-unknown-linux-gnu
```

### Suggested “did today’s changes break stages?” ladder

1. **Fast (minutes)**  
   - `cargo test -p portage-cli --lib` (or nextest + `--doc`)  
   - `cargo test -p portage-resolve --lib accept_keywords`  
   - Host: `em stages --stage3 -p --root $(mktemp -d) --config-root /`  
     (resolver + `@system` plan; no install)

2. **Sandbox pretend (tens of minutes)**  
   - `./test-scripts/regression-matrix.sh`  
   - Manual: install `em-bin`, run `stages --stage1 -p` and `stages --stage3 -p` under `--root` inside the sandbox

3. **Real builds (hours)**  
   - `./test-scripts/regression-matrix.sh --full`  
   - Manual: `toolchain --setup` → `stages --stage1` → `stages --stage3` under one `--root`  
   - Cross: `crossdev --setup` then stage1/stage3 with `--target`

4. **Canaries** (always useful after keyword / accept_keywords / roots changes)  
   - `em -p cross-<tuple>/linux-headers` (foreign-arch `package.accept_keywords`)  
   - `em -p sys-fs/btrfs-progs` on a `~arch` host (global testing keywords)

---

## USE cheat sheet (stage1 vs stage3)

| Layer | Stage1 | Stage3 |
|-------|--------|--------|
| Profile | yes | yes |
| ROOT `make.conf` | yes (then overridden) | **yes, decisive** |
| Forced override | `-* build` + `BOOTSTRAP_USE`; autosolve-use on | **none** |
| Process `USE=` | conf-layer override wins for the step | normal stacking |
| Goal | minimal bootstrap set | full `@system` as configured |

To experiment with lean stage3 USE without a stage-spec language:

```bash
# write into the stage root before --stage3
echo 'USE="-* bindist"' >> /path/to/stage/etc/portage/make.conf
em stages --root /path/to/stage --stage3 -p
```

(Whether that matches releng's private catalyst USE is a separate
question — often it will not.)

---

## Privilege and jobs

- Unprivileged stage roots: prefer `--privilege auto` (defaults toward
  pseudoroot) or explicit `--privilege pseudoroot` / `fakeroost` / `sudo`.
- Parallelism: `-j` / `--jobs` on stages/toolchain; load throttle via
  `-l` / `--load-average` (1-minute load; first job always starts).
- `MAKEOPTS` for the ebuild compile is still make.conf / package.env;
  sandbox scripts often set `MAKEOPTS=-j16` in the environment.

---

## What is not covered here (future work)

- **Stage4 / stage specifications** — package sets, USE overlays, news,
  locale, bindist, catalyst `spec` parity. Needs a design, not another
  boolean flag.
- **First-class “run everything in crossdev-stages target stage1/update”**
  that shells out to `em` instead of emerge — crossdev-stages still has
  its own bash/portage paths for board images; integration is optional.
- **Automated CI for full stage3** — too long and too privileged for
  GitHub Actions; stay in `test-scripts/` + human runs.

---

## Quick reference commands

```bash
# Build em
cargo build --release -p portage-cli

# Native stack (host paths)
em toolchain --root /var/tmp/em-stage --setup --autounmask-write -j8
em stages --root /var/tmp/em-stage --stage1 --autosolve-use -j8
em stages --root /var/tmp/em-stage --stage3 --autosolve-use -j8

# Sandbox harness
crossdev-stages sandbox setup --name em-stages-draft
crossdev-stages sandbox prepare --name em-stages-draft --bare
# copy em-bin as above, then:
crossdev-stages sandbox run --name em-stages-draft -- \
  /root/em-bin stages --root /root/s --stage3 -p --autosolve-use

# Repo regression matrix
./test-scripts/regression-matrix.sh
./test-scripts/regression-matrix.sh --full --jobs 16
```
