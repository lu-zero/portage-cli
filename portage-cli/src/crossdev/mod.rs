//! `em crossdev` — set up a cross-compilation target, a `crossdev` workalike.
//!
//! Implements the **no-build setup** (`--init-target` / `--show-target-cfg`):
//! overlay creation (the `cross-*` symlink category + `metadata`/`profiles` + a
//! `repos.conf` entry), the cross sysroot `make.conf`, and the **direct**
//! `make.profile` symlink (`eselect profile` refuses a foreign arch). `--setup`
//! additionally derives the ordered [`stages::toolchain_plan`] bootstrap
//! (binutils → headers → gcc-stage1 → libc → gcc-stage2) and runs each step
//! through the shared merge path.
//!
//! The staged-bootstrap driver ([`run_staged`]) and the [`stages::BootstrapKind`]
//! plan are shared with the **native toolchain** ([`toolchain`], `em toolchain
//! --setup`): a self-hosting toolchain into `--root` (`CHOST == CBUILD`) is the
//! same `glibc ↔ gcc` cycle as a cross toolchain, broken the same staged way —
//! see `todo/em-root-characterization.md`.
//!
//! The install location follows em's root model: the sysroot is
//! `<EROOT>/usr/<CTARGET>`, so `em crossdev <t>` targets `/usr/<CTARGET>` (like
//! crossdev), `em --local crossdev <t>` targets `~/.gentoo/usr/<CTARGET>`, and
//! `em --prefix DIR`/`--root DIR` retarget under `DIR`.
//!
//! ## `cross-<CTARGET>/gcc` vs `sys-devel/gcc` — two different packages
//!
//! Easy to conflate, and doing so caused real confusion chasing a stage1
//! failure (`todo/stage-build-shakeout.md` finding #19): they are **not** the
//! same compiler at any point.
//!
//! - **`cross-<CTARGET>/gcc`** (this module's overlay category, built by
//!   [`stages::toolchain_plan`]) is the **host-side cross-compiler**: it runs
//!   on `CBUILD`, emits code for `CTARGET`, and is what every ordinary
//!   package's `PATH` resolves `<CTARGET>-gcc`/`riscv64-unknown-linux-gnu-gcc`
//!   to via `gcc-config` (see `env_d.rs`). It's built once during
//!   `--setup`/`--init-target` and only changes if you explicitly rebuild or
//!   upgrade it — nothing else in `em` re-solves or upgrades it implicitly.
//! - **`sys-devel/gcc`** is the ordinary, real-category ebuild for "the
//!   compiler built with `CHOST == CTARGET`" — i.e. a compiler that will
//!   *itself run on* whatever `CHOST` currently is, no matter which host that
//!   happens to be. Installed via `em stages --stage1`/plain `em` merges, its
//!   version is resolved completely independently of `cross-<CTARGET>/gcc`.
//!
//! Because these are separate, independently-resolved atoms, they can drift:
//! `em stages --stage1 --target <t>` installing a newer `sys-devel/gcc` into
//! the target sysroot does **not** upgrade the `cross-<t>/gcc` cross-compiler
//! actually used to *build* it — and GCC cannot reliably self-bootstrap a
//! newer major version using an older one as `CC_FOR_TARGET` (a real GCC
//! limitation, not an em bug). Keeping the two in sync is a `--update`/rebuild
//! concern — see `todo/stage-build-shakeout.md` finding #19 for the pending
//! `crossdev --update` support and version-mismatch warning.

mod config_plan;
mod multilib;
pub mod stages;
mod target;

use std::io::Write;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::{Cpn, Pf, Version};
use portage_atom_pubgrub::DepClass;
use portage_repo::{MakeConf, ProfileStack, ReposConf, Repository};
use portage_vdb::Vdb;

use crate::cli::{Cli, CrossdevArgs, DepgraphFlags, MergeFlags};
use crate::style::{C_LABEL, C_PKG};
use target::CrossTarget;

/// Merge a subcommand's own flattened depgraph flags with the top-level one,
/// args taking precedence — so `--deep`/`--newuse` work whether given before
/// or after the subcommand name.
fn merge_depgraph_flags(globals: &Cli, args: &DepgraphFlags) -> DepgraphFlags {
    DepgraphFlags {
        deep: args.deep || globals.depgraph_flags.deep,
        newuse: args.newuse || globals.depgraph_flags.newuse,
    }
}

/// Merge merge-behavior flags (`-j`, `--keep-going`, `--buildpkg`, …) from a
/// subcommand's own flattened [`MergeFlags`] with the top-level one, args
/// taking precedence — the same "either position works" merge
/// [`merge_depgraph_flags`] already does for `--deep`/`--newuse`, needed here
/// for the same reason (`em -j 80 stages --stage1` vs `em stages --stage1 -j
/// 80`, see `todo/stage-build-shakeout.md`).
fn merge_merge_flags(globals: &Cli, args: &MergeFlags) -> MergeFlags {
    let g = &globals.merge_flags;
    MergeFlags {
        ask: args.ask || g.ask,
        update: args.update || g.update,
        autounmask_write: args.autounmask_write || g.autounmask_write,
        oneshot: args.oneshot || g.oneshot,
        fetchonly: args.fetchonly || g.fetchonly,
        buildpkg: args.buildpkg || g.buildpkg,
        buildpkgonly: args.buildpkgonly || g.buildpkgonly,
        usepkg: args.usepkg || g.usepkg,
        usepkgonly: args.usepkgonly || g.usepkgonly,
        getbinpkg: args.getbinpkg || g.getbinpkg,
        getbinpkgonly: args.getbinpkgonly || g.getbinpkgonly,
        emptytree: args.emptytree || g.emptytree,
        tree: args.tree || g.tree,
        json: args.json || g.json,
        onlydeps: args.onlydeps || g.onlydeps,
        noreplace: args.noreplace || g.noreplace,
        jobs: args.jobs.or(g.jobs),
        load_average: args.load_average.or(g.load_average),
        keep_going: args.keep_going || g.keep_going,
        autounmask: args.autounmask || g.autounmask,
        autosolve_use: args.autosolve_use || g.autosolve_use,
        complete_graph: args.complete_graph || g.complete_graph,
        with_bdeps: args.with_bdeps || g.with_bdeps,
        exclude: if args.exclude.is_empty() {
            g.exclude.clone()
        } else {
            args.exclude.clone()
        },
        root_deps: args.root_deps || g.root_deps,
    }
}

/// The overlay name prefix crossdev uses for its `Location::Alias` repos.conf
/// entries.
const OVERLAY_NAME: &str = "crossdev";

/// The per-target overlay/section name (`crossdev.<tuple>`): each cross
/// target gets its own file/section, so setting up a second target on the
/// same prefix doesn't silently orphan the first one's alias (found live:
/// `crossdev --setup` for a second target left the first target's alias
/// untouched under `--setup`'s `FillGapsOnly` policy, since the fixed
/// single `crossdev.conf` name made "already present" true regardless of
/// which target it actually named).
fn overlay_name(target: &CrossTarget) -> String {
    format!("{OVERLAY_NAME}.{}", target.tuple)
}

pub async fn run(args: &CrossdevArgs, globals: &Cli) -> Result<()> {
    // `--target` is global (`Cli`, not `CrossdevArgs`): one flag, read the
    // same way whether it's setting a target up (here) or using an
    // already-set-up one elsewhere (`stages --stage1`, plain atom builds) —
    // not two separate flags that can disagree.
    let tuple = globals
        .target
        .clone()
        .ok_or_else(|| anyhow::anyhow!("em crossdev needs a target tuple: pass --target/-T"))?;
    let target = CrossTarget::parse(&tuple, args.llvm)?;

    let extras = ex_pkg_atoms(args)?;

    if args.show_target_cfg {
        return show_target_cfg(&target, globals, &extras);
    }
    if args.init_target {
        return init_target(
            &target,
            globals,
            args,
            &extras,
            config_plan::RefreshPolicy::Sync,
        );
    }
    if args.setup {
        return setup(&target, globals, args, &extras).await;
    }
    bail!(
        "em crossdev does setup only for now — pass --init-target to lay down the \
         overlay + sysroot config, --setup to bootstrap the cross toolchain, or \
         --show-target-cfg to preview the derived config"
    );
}

/// Parse `--ex-pkg CATEGORY/PN` atoms (plus `--ex-gdb`'s `dev-debug/gdb`
/// shorthand) into `(category, pn)` pairs — crossdev's own `--ex-pkg`/
/// `--ex-gdb`: extra packages built onto an already-established cross
/// target, after the base toolchain. These always run on the host (like
/// `binutils`/`gcc`), never the target sysroot — real crossdev's
/// `for_each_extra_pkg set_portage X` always takes `set_env`'s host-ABI
/// branch for them, regardless of what the package actually does.
fn ex_pkg_atoms(args: &CrossdevArgs) -> Result<Vec<Cpn>> {
    let mut atoms = Vec::new();
    for pkg in &args.ex_pkg {
        let cpn = Cpn::parse(pkg)
            .map_err(|e| anyhow::anyhow!("--ex-pkg {pkg:?} is not CATEGORY/PN: {e}"))?;
        atoms.push(cpn);
    }
    if args.ex_gdb {
        atoms.push(Cpn::new("dev-debug", "gdb"));
    }
    Ok(atoms)
}

/// `em crossdev <tuple> --setup`: bootstrap the cross toolchain into the prefix
/// (`/usr/<chost>`). The full intertwined sequence (binutils → headers →
/// gcc-stage1 → libc → gcc-stage2) — the compiler is not usable until the libc
/// step lands, so toolchain and stage1 libc are one bootstrap.
///
/// Lays down the FS config via `init_target`'s `FillGapsOnly` policy — only
/// creates what's missing, never touches an already-existing file, so a hand
/// edit made between an earlier explicit `--init-target` and this `--setup`
/// survives (a fresh target still gets everything written, since nothing
/// exists yet) — then runs each step of the ordered
/// [`StagePlan`](stages::StagePlan) through the shared merge path
/// ([`crate::emerge_atoms`]) — per-step `USE` override + `--nodeps`. With `-p`
/// each step prints its plan instead of building.
async fn setup(
    target: &CrossTarget,
    globals: &Cli,
    args: &CrossdevArgs,
    extras: &[Cpn],
) -> Result<()> {
    // `init_target` is itself `-p`/`-a`-aware now (see `config_plan`), so
    // this no longer needs its own `!globals.pretend` gate — under `-p` it
    // previews the config-plan changes instead of skipping them silently.
    // `FillGapsOnly`: this is `--setup`'s own *implied* config-laydown step,
    // not an explicit `--init-target` — only create what's missing, so a
    // hand edit made between an earlier `--init-target` and this `--setup`
    // survives. Trade-off: `--setup --ex-pkg X` against an already-
    // initialized target won't add X either; run `--init-target --ex-pkg X`
    // first for that (documented in docs/crossdev.md).
    init_target(
        target,
        globals,
        args,
        extras,
        config_plan::RefreshPolicy::FillGapsOnly,
    )?;
    // A self-contained `--root DIR` EPREFIX has no host-shared merged-usr
    // skeleton or libs, so the plan needs the same from-scratch treatment as
    // native. `outer_roots()`, not `roots()`: this must stay anchored to the
    // outer EROOT even if `--target` happens to also be set on this
    // invocation.
    let self_contained = globals.outer_roots().is_self_contained_root();
    let plan = stages::toolchain_plan(
        &stages::BootstrapKind::Cross(target.clone()),
        self_contained,
    );
    let mut out = anstream::stdout();
    let verb = if globals.pretend { "Plan" } else { "Bootstrap" };
    writeln!(
        out,
        "\n{C_LABEL}{verb} cross toolchain{C_LABEL:#} ({}) — {} steps:",
        target.tuple,
        plan.steps.len()
    )
    .ok();

    let post_step = {
        let target = target.clone();
        move |step: &stages::StageStep| post_step_cross(&target, globals, step)
    };
    // `--root-deps=rdeps` unconditionally: the whole point of this bootstrap is
    // building a toolchain (+ glibc) into a target that starts empty, where
    // plain DEPEND (`virtual/os-headers`, `acct-group/root`, …) genuinely can't
    // be satisfied yet. Matches crossdev's own `<CTARGET>-emerge` wrapper,
    // which always implies this flag — not user-togglable here.
    let mut merge_flags = merge_merge_flags(globals, &args.merge_flags);
    merge_flags.root_deps = true;
    // `bypass_cross_root: true` — this is `crossdev --setup <T>`, which since
    // the `--cross`/`-t` -> `--target` unification (`bcde18a`) always runs
    // with the global `--target` flag active (that's now the only way to
    // name the tuple). Before that unification the tuple came via crossdev's
    // own separate `-t` flag, which never touched `globals.target`, so
    // `cli.roots()` here was already the unsubstituted outer EROOT and
    // `false` was harmless; afterwards it silently started resolving every
    // toolchain-bootstrap step (`cross-<T>/binutils` and its own deps)
    // against the *sysroot* instead — reading the sysroot's target-arch
    // make.conf (`CHOST`/`CFLAGS=-march=...`) to build a package that must
    // compile as a *host*-arch tool. Found live 2026-07-09 actually running
    // `crossdev --setup` post-unification for the first time (only
    // `--init-target`, which never reaches `run_staged`, had been
    // live-tested since).
    run_staged(
        &plan,
        globals,
        merge_depgraph_flags(globals, &args.depgraph_flags),
        merge_flags,
        true,
        false,
        post_step,
    )
    .await?;

    if !globals.pretend {
        writeln!(
            out,
            "\n>>> cross toolchain {} ready in {}/usr/{}",
            target.tuple,
            globals.outer_roots().merge_root(),
            target.tuple,
        )
        .ok();
    }
    Ok(())
}

/// Cross post-step hook: activate the freshly-built toolchain
/// (`<CTARGET>-*` wrappers via `binutils-config`/`gcc-config`), and after the
/// full libc lands (not the headers-only bootstrap step) bridge the ABI osdir
/// symlinks so the next gcc step links target code against it.
fn post_step_cross(target: &CrossTarget, globals: &Cli, step: &stages::StageStep) -> Result<()> {
    activate_toolchain(target, globals, step)?;
    if step.label == "libc" {
        link_abi_osdirs(target, globals)?;
    }
    Ok(())
}

/// Run each step of a staged [`stages::StagePlan`] through the shared merge path
/// ([`crate::emerge_atoms`]), printing per-step progress. `post_step` fires
/// after each *built* step (skipped under `-p`) for flavour-specific activation
/// — cross activates `<CTARGET>-*` wrappers + ABI osdirs; native is a no-op.
/// This is the shared driver both `--setup` (cross) and `stage1` (native) run.
///
/// `bypass_cross_root` forces every step's merge into the plain outer EROOT
/// even when `globals.target` is set — for `cross-*` toolchain plans woven
/// into a `--target`-active `stage1` run (see `maybe_weave_in_gcc_update`),
/// which must never install under `--target`'s sysroot substitution.
async fn run_staged(
    plan: &stages::StagePlan,
    globals: &Cli,
    depgraph_flags: crate::cli::DepgraphFlags,
    merge_flags: MergeFlags,
    bypass_cross_root: bool,
    target_only_installed_view: bool,
    post_step: impl Fn(&stages::StageStep) -> Result<()>,
) -> Result<()> {
    let mut out = anstream::stdout();
    for (i, step) in plan.steps.iter().enumerate() {
        // Flush the step header before building so progress shows immediately
        // (and survives the `process::exit` on a step that needs config changes,
        // which does not flush buffered stdout). The header names the step, so a
        // failure needs no extra context.
        writeln!(
            out,
            "\n{C_LABEL}[{n}/{total}] {label}{C_LABEL:#}{flags}",
            n = i + 1,
            total = plan.steps.len(),
            label = step.label,
            flags = step_flags(step),
        )
        .ok();
        out.flush().ok();
        crate::emerge_atoms(
            globals,
            &step.atoms,
            crate::EmergeOpts {
                use_override: &step.use_override,
                nodeps: step.nodeps,
                depgraph_flags: Some(depgraph_flags.clone()),
                merge_flags: Some(merge_flags.clone()),
                bypass_cross_root,
                target_only_installed_view,
            },
        )
        .await?;

        if !globals.pretend {
            post_step(step)?;
        }
    }
    Ok(())
}

/// Whether `atom`'s package name is `pkg` — handles both a bare atom
/// (`cross-<T>/gcc`) and a version-pinned one (`=cross-<T>/gcc-16.1.1...`,
/// as [`stages::gcc_refresh_plan`] uses to force an exact upgrade rather than
/// a same-version reinstall). A bare `ends_with("/gcc")` check misses the
/// pinned form entirely — caught live: it silently skipped activating the
/// freshly-built compiler, leaving the *old* slot active for the very build
/// this refresh existed to fix.
fn atom_is_package(atom: &str, pkg: &str) -> bool {
    match atom.rsplit_once('/') {
        Some((_, rest)) => {
            rest == pkg
                || rest
                    .strip_prefix(pkg)
                    .and_then(|v| v.strip_prefix('-'))
                    .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
        }
        None => false,
    }
}

/// Run the prefix-side `binutils-config`/`gcc-config` after the step that built
/// the tool, creating the `<EROOT>/usr/bin/<CTARGET>-*` wrappers. Keyed off the
/// step's package so it fires once per toolchain component.
///
/// Always activates against `globals.outer_roots()`, never `globals.roots()`:
/// `cross-<CTARGET>/*` toolchain packages always install into the plain outer
/// EROOT (see this module's doc comment), regardless of whether the *caller*
/// (`setup()` vs `stage1()`'s woven-in refresh) has `--target` set on
/// `globals` for its own, unrelated purposes. For `setup()` the two are the
/// same root anyway (it never sets `--target`), so this is a no-op change
/// there.
///
/// `outer_roots()`, not `base_roots()`: found live 2026-07-09 alongside the
/// matching `emerge.rs` fix — `base_roots()`'s `merge_root()` is the BROOT
/// view (host `/` under `--prefix`), not the outer EROOT the toolchain
/// actually merges into after that fix. Activating against the wrong root
/// would look for the just-built binutils/gcc in the wrong place under
/// `--prefix` specifically (host `/`, not the prefix).
fn activate_toolchain(target: &CrossTarget, globals: &Cli, step: &stages::StageStep) -> Result<()> {
    let Some(atom) = step.atoms.first() else {
        return Ok(());
    };
    let tuple = &target.tuple;
    let roots = globals
        .outer_roots()
        .with_own_config_root_if_self_contained();
    let activated = if atom_is_package(atom, "binutils") {
        crate::select::activate_binutils(&roots, tuple)?
    } else if atom_is_package(atom, "gcc") {
        let activated = crate::select::activate_compiler(&roots, tuple)?;
        // Piggyback the pkg-config wrapper on gcc's own activation step: by
        // this point the toolchain is ready and nothing else in the plan
        // gates on it (real crossdev's own `cross-pkg-config` has no build
        // step at all — it's a static script plus a per-target symlink).
        // See `select/pkgconf.rs`'s module doc for why this needs to exist.
        crate::select::activate_pkgconf(&roots, tuple)?;
        activated
    } else {
        return Ok(());
    };
    if activated {
        println!("    activated {} for {tuple}", step.label);
    }
    Ok(())
}

/// Native twin of [`activate_toolchain`]: run the `binutils-config`/
/// `gcc-config` equivalent after each native toolchain step builds, so
/// `<EROOT>/usr/bin/<CHOST>-*` wrappers actually exist. `em toolchain --setup`
/// used to pass `|_| Ok(())` as its post_step here — see `todo/PENDING.md`'s
/// "Native toolchain activation via `em select`" entry. `tuple` is the host's
/// own CHOST (native means `CHOST == CBUILD`; `--root`/`--prefix`/`--local`
/// all inherit the host's profile/make.conf for a native build, so
/// `select::get_chost` already resolves it correctly there — no separate
/// per-topology CHOST is ever written for native, unlike the cross sysroot's
/// own `CHOST=<tuple>`).
fn activate_native_toolchain(globals: &Cli, step: &stages::StageStep) -> Result<()> {
    let Some(atom) = step.atoms.first() else {
        return Ok(());
    };
    let tuple = crate::select::get_chost(globals)?;
    let roots = globals
        .outer_roots()
        .with_own_config_root_if_self_contained();
    let activated = if atom_is_package(atom, "binutils") {
        crate::select::activate_binutils(&roots, &tuple)?
    } else if atom_is_package(atom, "gcc") {
        let activated = crate::select::activate_compiler(&roots, &tuple)?;
        crate::select::activate_pkgconf(&roots, &tuple)?;
        activated
    } else {
        return Ok(());
    };
    if activated {
        println!("    activated {} for {tuple}", step.label);
    }
    Ok(())
}

/// Render a step's USE override / `--nodeps` as a compact suffix for the plan.
fn step_flags(step: &stages::StageStep) -> String {
    let mut parts = Vec::new();
    if step.nodeps {
        parts.push("--nodeps".to_string());
    }
    if !step.use_override.is_empty() {
        parts.push(format!("USE=\"{}\"", step.use_override.join(" ")));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  [{}]", parts.join(" "))
    }
}

/// `em toolchain --setup`: bootstrap a self-hosting native toolchain into
/// `--root` (`CHOST == CBUILD`, `SYSROOT == ROOT`). The native twin of the
/// crossdev `--setup`, sharing its staged driver but with the *native* plan
/// (baselayout → binutils → os-headers → full glibc → full gcc): the seed
/// compiler at `BROOT=/` builds glibc directly, so there is no two-stage gcc
/// (that is cross-only — see [`stages`]). Plain `::gentoo` atoms, none of the
/// cross overlay/wrapper/sysroot-make.conf ceremony — the host profile and
/// make.conf configure it (`--config-root /` by default).
///
/// This is the *toolchain* primitive only — the compiler the stages build
/// against. The actual stage production (stage1 `packages.build`, stage3
/// `--emptytree @system`) lives in `em stages` (see
/// `todo/em-stages-and-binhosts.md`). Requires `--root <dir>` (a toolchain into
/// `/` is meaningless). With `-p` each step prints its plan instead of building.
pub(crate) async fn toolchain(args: &crate::cli::ToolchainArgs, globals: &Cli) -> Result<()> {
    if !args.setup {
        bail!(
            "em toolchain does setup only for now — pass --setup to bootstrap the \
             native toolchain into --root"
        );
    }
    // outer_roots(), not roots(): a native toolchain bootstrap must anchor to
    // the outer EROOT even if a global --target happens to also be set.
    let merge_root = globals.outer_roots().merge_root().to_owned();
    if merge_root.as_str() == "/" {
        bail!(
            "em toolchain --setup needs --root <dir>: a native toolchain into / is \
             meaningless (use the host toolchain directly, or pass --root <empty>)"
        );
    }
    if !globals.pretend {
        ensure_self_contained_prefix(globals)?;
    }
    let plan = stages::toolchain_plan(&stages::BootstrapKind::Native, true);
    let mut out = anstream::stdout();
    let verb = if globals.pretend { "Plan" } else { "Bootstrap" };
    writeln!(
        out,
        "\n{C_LABEL}{verb} native toolchain{C_LABEL:#} into {merge_root} — {} steps:",
        plan.steps.len()
    )
    .ok();
    // `--root-deps=rdeps` unconditionally, same reasoning as `crossdev --setup`
    // above: this always bootstraps into a self-contained, still-empty ROOT
    // (`toolchain_plan(..., true)`), where plain DEPEND can't be satisfied yet
    // either. Without it, `sys-libs/glibc`'s own DEPEND closure
    // (`virtual/os-headers` → `linux-headers` → `perl` → `virtual/libcrypt` →
    // `libxcrypt` → `glibc`) is a real tree cycle that pre-flight can never
    // clear — confirmed live: it fails identically in a real (non-pretend)
    // run even after steps 1-3 have genuinely merged, not just in isolated `-p`.
    let mut merge_flags = merge_merge_flags(globals, &args.merge_flags);
    merge_flags.root_deps = true;
    run_staged(
        &plan,
        globals,
        merge_depgraph_flags(globals, &args.depgraph_flags),
        merge_flags,
        false,
        true,
        move |step: &stages::StageStep| activate_native_toolchain(globals, step),
    )
    .await?;
    if !globals.pretend {
        writeln!(out, "\n>>> native toolchain ready in {merge_root}").ok();
    }
    Ok(())
}

/// `em stages --stage1`: emerge the profile's `packages.build` bootstrap set
/// into `--root` — baselayout (USE=build, `--nodeps`) then the minimal stage1
/// package list (USE="-* build"), mirroring catalyst's `stage1/chroot.sh`.
/// Requires the ROOT's own toolchain already built (`em toolchain --setup`);
/// stage1 assumes a working `<chost>-gcc` is already in the root, it does not
/// build one (that's [`toolchain`]). With `-p` it prints the plan instead of
/// building.
pub(crate) async fn stage1(args: &crate::cli::StagesArgs, globals: &Cli) -> Result<()> {
    if !args.stage1 {
        bail!(
            "em stages does stage1 only for now — pass --stage1 to emerge the \
             profile's packages.build bootstrap set into --root"
        );
    }
    let merge_root = globals.roots().merge_root().to_owned();
    if merge_root.as_str() == "/" {
        bail!("em stages --stage1 needs --root <dir>: a stage1 into / is meaningless");
    }
    let stack = profile_stack(globals)?;
    let bootstrap_use = bootstrap_use(&stack, globals).await;
    let plan = stages::stage1_plan(&stack, &bootstrap_use)?;
    let refresh = maybe_weave_in_gcc_update(&stack, globals).await;
    let mut out = anstream::stdout();
    let verb = if globals.pretend { "Plan" } else { "Bootstrap" };

    // The `cross-<CTARGET>/gcc` refresh (if needed) is a separate run: it
    // always installs into the outer EROOT (`bypass_cross_root: true`),
    // never `--target`'s sysroot substitution the stage1 packages below use.
    if let Some((target, refresh_plan)) = &refresh {
        writeln!(
            out,
            "\n{C_LABEL}{verb} cross-compiler refresh{C_LABEL:#} ({}) — {} steps:",
            target.tuple,
            refresh_plan.steps.len()
        )
        .ok();
        let post_step = {
            let target = target.clone();
            move |step: &stages::StageStep| post_step_cross(&target, globals, step)
        };
        run_staged(
            refresh_plan,
            globals,
            merge_depgraph_flags(globals, &args.depgraph_flags),
            merge_merge_flags(globals, &args.merge_flags),
            true,
            false,
            post_step,
        )
        .await?;
    }

    writeln!(
        out,
        "\n{C_LABEL}{verb} native stage1{C_LABEL:#} into {merge_root} — {} steps:",
        plan.steps.len()
    )
    .ok();
    run_staged(
        &plan,
        globals,
        merge_depgraph_flags(globals, &args.depgraph_flags),
        merge_merge_flags(globals, &args.merge_flags),
        false,
        false,
        |_| Ok(()),
    )
    .await?;
    if !globals.pretend {
        writeln!(out, "\n>>> stage1 ready in {merge_root}").ok();
    }
    Ok(())
}

/// If this is a cross build and the stage1 set includes `sys-devel/gcc`,
/// check whether `gcc-config`'s currently *active* `cross-<CTARGET>/gcc` is
/// new enough to build it, and if not, return a
/// [`stages::gcc_refresh_plan`] to run (into the outer EROOT, via
/// `bypass_cross_root` — see [`run_staged`]) before the stage1 plan itself.
///
/// `sys-devel/gcc` (`CHOST == CTARGET`) builds single-pass, not as a
/// self-hosting bootstrap (`toolchain.eclass`'s `is_crosscompile()` is false
/// for it) — the active cross-compiler is its *only* build tool. GCC's own
/// target libraries (e.g. `libatomic`) can pass driver flags only a
/// matching-or-newer major version understands, so an older active
/// cross-compiler silently breaks deep inside a target library's own
/// `configure` — see `todo/stage-build-shakeout.md`.
///
/// Best-effort: any failure determining compatibility (no active compiler
/// yet is the *expected* "needs building" case and always weaves in; an
/// unparseable slot, an LLVM cross target with no `cross-<CTARGET>/gcc`
/// package at all, or a resolve failure are all treated as "can't tell,
/// leave the plan alone" rather than blocking the stage1 run).
async fn maybe_weave_in_gcc_update(
    stack: &ProfileStack,
    globals: &Cli,
) -> Option<(CrossTarget, stages::StagePlan)> {
    let tuple = globals.target.clone()?;
    let stage1_atoms = stack.stage1_packages().ok()?;
    if !stage1_atoms.iter().any(|d| d.cpn.package.as_str() == "gcc") {
        return None;
    }
    let needed_version = resolve_gcc_version(globals).await?;
    let needed_slot = needed_version.split(['.', '_']).next()?;
    let target = CrossTarget::parse(&tuple, false).ok()?;
    let active_slot = crate::select::current_compiler_slot(
        &globals
            .outer_roots()
            .with_own_config_root_if_self_contained(),
        &target.tuple,
    );
    if gcc_needs_refresh(active_slot.as_deref(), needed_slot) {
        let refresh_plan = stages::gcc_refresh_plan(&target, &needed_version);
        Some((target, refresh_plan))
    } else {
        None
    }
}

/// Whether the active cross-compiler slot is too old to build a
/// `needed_slot` `sys-devel/gcc`: nothing activated yet (`None`) or a
/// strictly older slot. A newer-or-equal active slot is assumed fine (GCC is
/// generally backward compatible as a *build tool*, and this is a numeric
/// gate, not exact-match, to avoid gratuitous rebuilds). An unparseable
/// slot (either side) is treated as "can't tell" rather than "needs
/// refresh" — GCC's own SLOT is always a plain integer, so this should never
/// actually happen; if it does, silently doing nothing is safer than
/// forcing an unwanted rebuild.
fn gcc_needs_refresh(active_slot: Option<&str>, needed_slot: &str) -> bool {
    let Ok(needed_num) = needed_slot.parse::<u32>() else {
        return false;
    };
    match active_slot {
        None => true,
        Some(active) => active.parse::<u32>().is_ok_and(|n| n < needed_num),
    }
}

/// The exact version `sys-devel/gcc` would resolve to for this invocation's
/// config (`ACCEPT_KEYWORDS`/masks), e.g. `"16.1.1_p20260606"`. A lightweight
/// `--nodeps` resolve of the single atom, reusing the same `depgraph()`
/// machinery every merge already goes through. GCC's own `SLOT` is always
/// its major version (`gcc.eclass`: `SLOT="$(ver_cut 1)"`), so callers needing
/// just the slot take the version's first component.
async fn resolve_gcc_version(globals: &Cli) -> Option<String> {
    let repo_path_str = globals.repo_path();
    let roots = globals.roots();
    // See `DepgraphOpts::host_merge_root`: `Cli::broot()` stays overlay-aware
    // under `--target` substitution, unlike `roots`.
    let host_roots = globals.broot();
    let outcome = crate::query::depgraph::depgraph(crate::query::depgraph::DepgraphOpts {
        repo_path: Utf8Path::new(&repo_path_str),
        atoms: &["sys-devel/gcc".to_string()],
        arch: &globals.arch,
        format: crate::cli::DepgraphFormat::Pretty,
        verbose: 0,
        empty: false,
        autounmask_write: false,
        autosolve_use: false,
        multi_repo: globals.repo.is_none(),
        roots: &roots,
        host_merge_root: host_roots.merge_root(),
        onlydeps: false,
        with_bdeps: false,
        root_deps_rdeps: false,
        deep: false,
        nodeps: true,
        extra_use_override: None,
        binpkg_index: None,
    })
    .await
    .ok()?;
    let merge = outcome
        .plan
        .iter()
        .find(|m| m.cpv.cpn.category == "sys-devel" && m.cpv.cpn.package == "gcc")?;
    Some(merge.cpv.version.to_string())
}

/// Build the [`ProfileStack`] for the invocation's config-root (host `/`
/// unless `--config-root`/`--root` offsets it), resolving
/// `etc/portage/make.profile` the same way `@system`/`@world` expansion does.
fn profile_stack(globals: &Cli) -> Result<ProfileStack> {
    let roots = globals.roots();
    let config_root = roots.config().unwrap_or(Utf8Path::new("/"));
    let profile_link = config_root.join("etc/portage/make.profile");
    let canon = std::fs::canonicalize(profile_link.as_std_path())
        .with_context(|| format!("cannot resolve make.profile at {profile_link}"))?;
    ProfileStack::build(canon).context("failed to build profile stack")
}

/// The profile's `BOOTSTRAP_USE` variable (`profiles/base/make.defaults`),
/// after sourcing the profile chain — see [`stages::stage1_plan`]'s doc for
/// why stage1 must re-add this after its `-*` clear. Read directly off the
/// shell rather than through `ProfileEnv::merge`: `profile_env`'s per-layer
/// capture only tracks `USE`/`USE_EXPAND`-family variables, and
/// `BOOTSTRAP_USE` is neither — it's a plain, non-incremental profile
/// variable, so its value just needs to still be sitting in the shell after
/// the profile chain has been sourced (same pattern as `use_env.rs`'s
/// `split_var` reading `USE_EXPAND`/`USE_EXPAND_HIDDEN` post-resolve).
/// Best-effort: any failure sourcing the profile chain just means no extra
/// flags get restored, the same "can't tell, leave the plan alone" posture
/// as [`maybe_weave_in_gcc_update`], rather than blocking the whole stage1
/// run over a variable that fixes correctness, not availability.
async fn bootstrap_use(stack: &ProfileStack, globals: &Cli) -> Vec<String> {
    async fn try_read(stack: &ProfileStack, globals: &Cli) -> Result<Vec<String>> {
        let repo = main_repo(globals)?;
        let mut shell = repo.shell().await?;
        stack.profile_env(&mut shell).await?;
        let mut use_tokens: Vec<String> = shell
            .get_var("BOOTSTRAP_USE")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        // `ELIBC`/`KERNEL` (`USE_EXPAND_IMPLICIT` in every profile,
        // profiles/base/make.defaults) are folded as ordinary USE_EXPAND
        // tokens (`elibc_glibc`, `kernel_linux`) at the profile's own
        // "defaults" layer — the *same* layer BOOTSTRAP_USE's own flags
        // need re-adding from, and for the identical reason: `-*` wipes
        // them just as it wipes everything else below it (verified against
        // real portage's config.py `regenerate()`, not assumed). Real
        // catalyst's own CATALYST_USE construction must re-add these too,
        // for its stage1 to end up with a working `virtual/libc` — found
        // live: without this, `sys-libs/glibc` never enters the stage1
        // plan at all (`virtual/libc`'s `elibc_glibc? ( sys-libs/glibc )`
        // RDEPEND silently never fires), and `gcc`'s own `libgcc` build
        // then fails `fatal error: stdio.h: No such file or directory`.
        for var in ["ELIBC", "KERNEL"] {
            if let Some(val) = shell.get_var(var).filter(|v| !v.is_empty()) {
                use_tokens.push(format!("{}_{val}", var.to_lowercase()));
            }
        }
        Ok(use_tokens)
    }
    try_read(stack, globals).await.unwrap_or_default()
}

/// `EROOT`/prefix the overlay, `repos.conf`, and `package.env` are written under
/// (`~/.gentoo` for `--local`), so an unprivileged setup is writable + readable.
///
/// `outer_roots()`, not `roots()`: this is the outer EROOT the overlay lives
/// in, which must stay stable even if a global `--target` happens to also be
/// set on the invocation (`roots()` would already be `--target`'s sysroot
/// substitution, doubly-nesting anything joined onto it below).
fn setup_root(globals: &Cli) -> Utf8PathBuf {
    globals.outer_roots().merge_root().to_owned()
}

/// The target sysroot `<EROOT>/usr/<CTARGET>` (EROOT = `/` by default, the prefix
/// for `--local`/`--prefix`, the root for `--root`).
fn sysroot(target: &CrossTarget, globals: &Cli) -> Utf8PathBuf {
    globals
        .outer_roots()
        .merge_root()
        .join("usr")
        .join(&target.tuple)
}

/// The configured main repo (`gentoo`) — the real ebuilds the overlay links to.
///
/// A self-contained `--root DIR` target (no `--local`/`--prefix` host-config
/// sharing) starts with no `repos.conf` of its own — that's exactly the
/// "stage1 from scratch" case, and `--init-target` is what's supposed to lay
/// one down. So this can't rely solely on the target's own config-root: it
/// falls back to the *host's* `repos.conf`, then to portage's own well-known
/// default location (mirroring `Cli::repo_path`'s fallback), so the very first
/// `--init-target` on a fresh root can still find the real ebuild tree to
/// symlink/reference.
pub(crate) fn main_repo(globals: &Cli) -> Result<Repository> {
    let target_conf = globals.outer_roots().repos_conf().ok();
    let host_conf = ReposConf::load_rooted(Utf8Path::new("/"), &[]).ok();
    let entry = target_conf
        .as_ref()
        .and_then(|c| c.main_repo().or_else(|| c.find("gentoo")))
        .or_else(|| {
            host_conf
                .as_ref()
                .and_then(|c| c.main_repo().or_else(|| c.find("gentoo")))
        });
    match entry {
        Some(e) => Repository::open(e.location.as_path().unwrap_or(std::path::Path::new(".")))
            .with_context(|| format!("opening main repo at {}", e.location.as_path().map(|p| p.display().to_string()).unwrap_or_else(|| "(virtual)".to_string()))),
        None => Repository::open("/var/db/repos/gentoo")
            .context("no main repo configured in repos.conf (target or host) and the default /var/db/repos/gentoo is not a repo either"),
    }
}

fn show_target_cfg(target: &CrossTarget, globals: &Cli, extras: &[Cpn]) -> Result<()> {
    let mut out = anstream::stdout();
    let row = |out: &mut dyn Write, k: &str, v: &str| {
        writeln!(out, "  {C_LABEL}{k:<9}{C_LABEL:#} {v}").ok();
    };
    let model = if target.llvm { "LLVM/Clang" } else { "GCC" };
    row(&mut out, "Target", &target.tuple);
    row(&mut out, "Model", model);
    row(&mut out, "Category", &target.category());
    row(&mut out, "ARCH", &target.gentoo_arch());
    row(&mut out, "Profile", &target.profile_path());
    row(&mut out, "Sysroot", sysroot(target, globals).as_str());
    row(&mut out, "CFLAGS", target.cflags());
    writeln!(out, "  {C_LABEL}Packages{C_LABEL:#}").ok();
    let category = target.category();
    for (cat, pkg, _) in target.packages() {
        writeln!(out, "    {C_PKG}{category}/{pkg}{C_PKG:#} → {cat}/{pkg}").ok();
    }
    if !extras.is_empty() {
        writeln!(out, "  {C_LABEL}Extra (--ex-pkg, host-arch){C_LABEL:#}").ok();
        for cpn in extras {
            let pkg = cpn.package;
            writeln!(out, "    {C_PKG}{category}/{pkg}{C_PKG:#} → {cpn}").ok();
        }
    }
    Ok(())
}

/// Lay down the overlay + sysroot config for `target`. Collects every file
/// `em` wants in a particular state as a [`config_plan::ConfigEntry`], then
/// hands the whole batch to [`config_plan::apply`] — so this now honours
/// `-p` (preview instead of writing) and `-a` (confirm before writing) like
/// any other mutating `em` path, instead of writing blindly.
///
/// `extras` are `--ex-pkg`/`--ex-gdb` atoms (crossdev's own "Extra Fun"):
/// additional packages onto the established cross target, beyond
/// [`CrossTarget::packages`]'s fixed base set — always host-arch, matching
/// real crossdev's `set_env` treatment of `--ex-pkg`.
///
/// `policy` distinguishes the explicit `--init-target` flag (`Sync`: always
/// reconcile to the freshly-computed state, including re-detecting a hand
/// edit as drift) from `--setup`'s own implied config-laydown step
/// (`FillGapsOnly`: only create what's missing, so a hand edit made between
/// an earlier `--init-target` and this `--setup` survives).
fn init_target(
    target: &CrossTarget,
    globals: &Cli,
    args: &CrossdevArgs,
    extras: &[Cpn],
    policy: config_plan::RefreshPolicy,
) -> Result<()> {
    let ask = merge_merge_flags(globals, &args.merge_flags).ask;
    // For a retargeted prefix (`--local`/`--prefix`/`--root`) bootstrap it first:
    // `setup::bootstrap` writes the prefix `bashrc` that re-adds `<EROOT>/usr/bin`
    // to the build PATH (the shell sanitiser strips `$HOME` paths, so a `--local`
    // prefix's own bin is otherwise invisible). That is what makes the cross
    // toolchain wrappers we install reachable by the gcc-stage builds. Idempotent.
    // Kept outside the config plan below (a separate, already pretend-aware
    // subsystem) — only actually bootstraps for real when not previewing.
    let roots = globals.outer_roots();
    if roots.merge_root().as_str() != "/" && !globals.pretend {
        crate::setup::bootstrap(&roots)?;
    }
    let gentoo_path = main_repo(globals)?.path().to_owned();
    let sysroot = sysroot(target, globals);
    let category = target.category();

    let mut entries = Vec::new();
    entries.extend(self_contained_prefix_entries(globals, &gentoo_path)?);
    // Derive the cross packages on the fly: a `Location::Alias` repos.conf
    // entry declares `cross-<tuple>/<pkg>` as a virtual alias for its real
    // `::gentoo` package, materialised in-memory at load time. No on-disk
    // symlink overlay. See todo/cross-derive-on-the-fly.md.
    entries.push(alias_repo_conf_entry(
        globals,
        &gentoo_path,
        target,
        &category,
        extras,
    )?);
    entries.extend(cross_env_entries(target, globals, &gentoo_path, extras)?);
    entries.extend(sysroot_config_entries(
        target,
        &sysroot,
        globals.outer_roots().merge_root(),
        &gentoo_path,
    )?);
    entries.extend(sysroot_repos_conf_entries(
        &sysroot,
        &gentoo_path,
        target,
        &category,
        extras,
    ));

    if !config_plan::apply(entries, globals.pretend, ask, policy)?.applied() {
        return Ok(());
    }

    println!(">>> cross target {} ready", target.tuple);
    println!("    alias:     {category}  (derived from ::gentoo)");
    println!("    sysroot:  {sysroot}");
    // The toolchain itself is a HOST build (compiler lands on /), so it resolves
    // with host config — NOT the sysroot (that fights the cross make.conf ROOT).
    println!(
        "    toolchain: em -p {}/gcc          # host build of the cross compiler",
        target.category()
    );
    Ok(())
}

/// Write the virtual `Location::Alias` repos.conf entry that derives
/// `cross-<tuple>/<pkg>` packages from `::gentoo` at resolve time — the
/// in-memory replacement for the old on-disk symlink overlay. The entry maps
/// the destination cross category to the real `(category, package)` set from
/// [`CrossTarget::packages`], the single source of truth. See
/// `todo/cross-derive-on-the-fly.md`.
///
/// The real packages' existence under `gentoo` is verified up front (a missing
/// source package would later surface as a resolver `NoVersions` with no hint
/// at the cause); the alias declaration itself is always written so a partial
/// tree still resolves the packages that *are* present.
fn alias_repo_conf_entry(
    globals: &Cli,
    gentoo: &Utf8Path,
    target: &CrossTarget,
    category: &str,
    extras: &[Cpn],
) -> Result<config_plan::ConfigEntry> {
    // Validate every source package exists under ::gentoo, with a clear error
    // naming the cross package it's needed for, before declaring the alias.
    // Covers `--ex-pkg`/`--ex-gdb` extras too — same requirement, same error
    // shape, so a typo'd or nonexistent extra is rejected up front instead of
    // surfacing later as an opaque resolver `NoVersions`.
    for (real_cat, pkg, _) in target.packages() {
        let dst = gentoo.join(real_cat).join(pkg);
        if !dst.is_dir() {
            bail!("{real_cat}/{pkg} not found at {dst} (needed for {category}/{pkg})");
        }
    }
    for cpn in extras {
        let dst = gentoo
            .join(cpn.category.as_str())
            .join(cpn.package.as_str());
        if !dst.is_dir() {
            bail!(
                "--ex-pkg {cpn} not found at {dst} (needed for {category}/{})",
                cpn.package
            );
        }
    }

    let conf_dir = setup_root(globals).join("etc/portage/repos.conf");
    let name = overlay_name(target);
    Ok(config_plan::ConfigEntry::Alias {
        path: conf_dir.join(format!("{name}.conf")),
        name,
        category: category.to_owned(),
        packages_line: alias_packages_line(target, extras),
    })
}

/// The self-contained-`--root`-only config entries (`gentoo.conf` +
/// `make.profile` link) that both `em toolchain --setup` (native) and `em
/// crossdev --setup`/`--init-target` (cross) need before merging anything:
/// - a `gentoo` `repos.conf` entry, for a self-contained `--root DIR` target
///   only (`roots.is_self_contained_root()` — unlike `--local`/`--prefix`,
///   which merge this same directory onto the host's real repos.conf as an
///   extra source, so already resolve `gentoo` from there);
/// - a `make.profile` link, same self-contained-only condition — the EPREFIX
///   builds *host-arch* packages (the crossdev toolchain lands on
///   `ROOT=/`-equivalent, and a native toolchain always is host-arch), so it
///   links the *host's* resolved profile, unlike the cross target sysroot,
///   which links the target's own arch profile.
///
/// Without this a self-contained `--root` target has no way to resolve any
/// ebuild at all — the "stage1 from scratch" gap found 2026-07-03 doing a
/// real from-scratch native + cross toolchain bootstrap, see
/// [[stage-build-shakeout]]. `gentoo_path` here is the already-resolved
/// `::gentoo` repo path (`init_target`'s `setup::bootstrap` call handles the
/// rest of the EPREFIX skeleton separately, outside the config plan).
fn self_contained_prefix_entries(
    globals: &Cli,
    gentoo_path: &Utf8Path,
) -> Result<Vec<config_plan::ConfigEntry>> {
    let roots = globals.outer_roots();
    if !roots.is_self_contained_root() {
        return Ok(Vec::new());
    }
    let conf_dir = setup_root(globals).join("etc/portage/repos.conf");
    let mut entries = vec![config_plan::ConfigEntry::CreateOnly {
        path: conf_dir.join("gentoo.conf"),
        desired: format!("[gentoo]\nlocation = {gentoo_path}\n"),
    }];
    entries.extend(prefix_profile_entries(globals)?);
    Ok(entries)
}

/// Native toolchain (`em toolchain --setup`) entry point: bootstrap the
/// EPREFIX skeleton and apply the self-contained-`--root`-only config
/// entries eagerly, no preview/confirm — this path is already externally
/// gated by `!globals.pretend` at its one call site. Returns the resolved
/// `::gentoo` repo path.
fn ensure_self_contained_prefix(globals: &Cli) -> Result<Utf8PathBuf> {
    let roots = globals.outer_roots();
    if roots.merge_root().as_str() != "/" {
        crate::setup::bootstrap(&roots)?;
    }
    let gentoo_path = main_repo(globals)?.path().to_owned();
    config_plan::apply_now(&self_contained_prefix_entries(globals, &gentoo_path)?)?;
    Ok(gentoo_path)
}

/// The whitespace-separated real-cpn list for `alias-packages`: the base set
/// from [`CrossTarget::packages`] in stage order, followed by any `--ex-pkg`/
/// `--ex-gdb` `extras`. The parser re-parses each token as a `Cpn`, so this
/// is pure config-file serialisation — no identity is carried as an opaque
/// string downstream.
fn alias_packages_line(target: &CrossTarget, extras: &[Cpn]) -> String {
    target
        .packages()
        .into_iter()
        .map(|(cat, pkg, _)| format!("{cat}/{pkg}"))
        .chain(extras.iter().map(Cpn::to_string))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Link a `make.profile` for a self-contained `--root DIR` EPREFIX (same
/// "stage1 from scratch" gap as [`ensure_repos_conf`]'s `gentoo.conf`): unlike
/// `--local`/`--prefix`, which share the host's own `make.profile` via config
/// sharing, plain `--root` has none of its own. The EPREFIX builds *host-arch*
/// packages (the crossdev toolchain lands on `ROOT=/`-equivalent, just
/// offset), so — unlike the target sysroot, which links the target's own
/// arch profile — this links the *host's* resolved profile. A no-op for
/// `--local`/`--prefix` (`!roots.is_self_contained_root()`, config already
/// comes from the host).
fn prefix_profile_entries(globals: &Cli) -> Result<Vec<config_plan::ConfigEntry>> {
    if !globals.outer_roots().is_self_contained_root() {
        return Ok(Vec::new());
    }
    let link = setup_root(globals).join("etc/portage/make.profile");
    // Already there: skip resolving the host profile entirely (matches the
    // original "create once, never re-verify" behaviour) rather than paying
    // the canonicalize cost just to report "unchanged".
    if link.exists() {
        return Ok(Vec::new());
    }
    let host_profile = std::fs::canonicalize("/etc/portage/make.profile")
        .context("resolving the host's make.profile")?;
    let host_profile = Utf8PathBuf::from_path_buf(host_profile)
        .map_err(|p| anyhow::anyhow!("host make.profile path {p:?} is not valid UTF-8"))?;
    Ok(vec![config_plan::ConfigEntry::Symlink {
        link,
        target: host_profile,
    }])
}

/// Write the cross sysroot `etc/portage/{make.conf,make.profile}`.
fn sysroot_config_entries(
    target: &CrossTarget,
    sysroot: &Utf8Path,
    outer_root: &Utf8Path,
    gentoo: &Utf8Path,
) -> Result<Vec<config_plan::ConfigEntry>> {
    let portage = sysroot.join("etc/portage");
    let mut entries = Vec::new();

    // Materialise an (empty) target package database. Without it the installed
    // loader finds no VDB at `<sysroot>/var/db/pkg` and falls back to the host
    // VDB, so host-installed packages wrongly satisfy target requests and the
    // cross plan comes up empty. An empty dir = "nothing installed in the
    // sysroot yet", which is what we want for a fresh target.
    entries.push(config_plan::ConfigEntry::Dir {
        path: sysroot.join("var/db/pkg"),
    });

    // Always regenerate: entirely em-managed (unlike the host's real
    // make.conf, never hand-edited), and its content (CTARGET/CFLAGS/`ROOT`)
    // is derived from `target`/`outer_root`, both of which can legitimately
    // change across `--init-target` re-runs (e.g. a different `--prefix`).
    // A create-only write here would leave it silently stale, the same class
    // of bug just fixed for the alias-packages entry.
    entries.push(config_plan::ConfigEntry::File {
        path: portage.join("make.conf"),
        desired: make_conf_body(target, sysroot, outer_root),
    });

    // Link make.profile DIRECTLY (absolute) to the target-arch profile — eselect
    // profile validates against the host arch and refuses a foreign one.
    let profile_dir = gentoo.join("profiles").join(target.profile_path());
    if !profile_dir.is_dir() {
        bail!(
            "target profile '{}' not found at {profile_dir}",
            target.profile_path()
        );
    }
    entries.push(config_plan::ConfigEntry::Symlink {
        link: portage.join("make.profile"),
        target: profile_dir,
    });
    Ok(entries)
}

/// `<sysroot>/etc/portage/repos.conf` entries referencing the host gentoo
/// (main) repo and the crossdev overlay, so a cross build with
/// `PORTAGE_CONFIGROOT=<sysroot>` still sees the ebuild tree — the sysroot has no
/// repos of its own (crossdev-stages copies the host `repos.conf` likewise).
fn sysroot_repos_conf_entries(
    sysroot: &Utf8Path,
    gentoo: &Utf8Path,
    target: &CrossTarget,
    category: &str,
    extras: &[Cpn],
) -> Vec<config_plan::ConfigEntry> {
    let dir = sysroot.join("etc/portage/repos.conf");
    let name = overlay_name(target);
    vec![
        config_plan::ConfigEntry::CreateOnly {
            path: dir.join("gentoo.conf"),
            desired: format!("[DEFAULT]\nmain-repo = gentoo\n\n[gentoo]\nlocation = {gentoo}\n"),
        },
        config_plan::ConfigEntry::Alias {
            path: dir.join(format!("{name}.conf")),
            name,
            category: category.to_owned(),
            packages_line: alias_packages_line(target, extras),
        },
    ]
}

/// The special cross `make.conf` body (crossdev `set_metadata`): `CHOST`/`CBUILD`
/// so the cross context is detectable, `ARCH`/keywords + target `CFLAGS`. `ROOT`
/// tracks the actual sysroot so a retargeted prefix (`--local`/`--prefix`, e.g.
/// `~/.gentoo/usr/<CTARGET>`) is honoured, not the hardcoded `/usr/<CTARGET>`.
///
/// Deliberately no `CTARGET` here — real crossdev's own target template
/// (`/usr/share/crossdev/etc/portage/make.conf`) never sets it either. `CTARGET`
/// only applies to the host-side `cross-<CTARGET>/{binutils,gcc,...}` builds
/// (`toolchain.eclass` reads it off `CATEGORY`, scoped via [`write_cross_env`]'s
/// `package.env`); leaking it into the sysroot-wide make.conf makes `econf` pass
/// `--target=` to *every* ordinary package, which custom (non-autoconf)
/// `configure` scripts like sqlite's reject outright.
///
/// `MAKEOPTS` mirrors the host's (like `setup::host_makeopts`, for the same
/// reason): without it, every `sys-*` package resolved against this sysroot
/// (`sys-devel/gcc` included) builds fully serial — this make.conf is the
/// *only* one they read, so there is no other source for build parallelism.
/// Caught live: a real stage1 build ran with a single `cc1plus` at a time on a
/// 128-core host because this was missing.
fn make_conf_body(target: &CrossTarget, sysroot: &Utf8Path, outer_root: &Utf8Path) -> String {
    let arch = target.gentoo_arch();
    let tuple = &target.tuple;
    let cbuild = host_chost();
    let makeopts = crate::setup::host_makeopts();
    format!(
        "# Autogenerated by `em crossdev` — cross sysroot for {tuple}.\n\
         CBUILD={cbuild}\n\
         CHOST={tuple}\n\
         ARCH=\"{arch}\"\n\
         ACCEPT_KEYWORDS=\"{arch} ~{arch}\"\n\
         ROOT=\"/\"\n\
         MAKEOPTS=\"{makeopts}\"\n\
         CFLAGS=\"{}\"\n\
         CXXFLAGS=\"${{CFLAGS}}\"\n\
         # The sysroot's own .pc files record paths as if it were \"/\" (e.g.\n\
         # `prefix=/usr`, not the host-absolute sysroot path) — PKG_CONFIG_SYSROOT_DIR\n\
         # prepends the real path onto whatever a .pc reports. PKG_CONFIG_LIBDIR\n\
         # (unlike _PATH) *replaces* pkg-config's default search list, so the host's\n\
         # own .pc files never leak into a foreign-arch cross build (found live:\n\
         # iproute2's ./configure auto-detected the host's net-libs/libtirpc via\n\
         # plain pkg-config, linked -ltirpc, then failed since the target sysroot\n\
         # never had it — see todo/stage-build-shakeout.md).\n\
         PKG_CONFIG_SYSROOT_DIR=\"{sysroot}\"\n\
         PKG_CONFIG_LIBDIR=\"{sysroot}/usr/lib64/pkgconfig:{sysroot}/usr/lib/pkgconfig:{sysroot}/usr/share/pkgconfig\"\n\
         # meson.eclass (and any buildsystem following the same convention) reads\n\
         # BUILD_PKG_CONFIG_LIBDIR for its *native* build-machine pkg-config search\n\
         # path, falling back to the target PKG_CONFIG_LIBDIR above when unset — the\n\
         # same host/target conflation bug as the bare zstd.m4 case in\n\
         # sys-devel/binutils (see todo/stage-build-shakeout.md #29), just for\n\
         # buildsystems that otherwise do the right thing. Point it at the outer\n\
         # EROOT's own native pkgconfig dirs (host BDEPEND packages build there —\n\
         # see [[em-root-characterization]] Tier 1 item 2), not the bare host `/`.\n\
         BUILD_PKG_CONFIG_LIBDIR=\"{outer_root}/usr/lib64/pkgconfig:{outer_root}/usr/lib/pkgconfig:{outer_root}/usr/share/pkgconfig\"\n",
        target.cflags(),
    )
}

/// The host's own installed `(version, slot)` pairs for `cat/pkg`, newest
/// first — queried against the build host's own BROOT
/// (`roots.satisfaction_root(DepClass::Bdepend)`), the same root a Host-arch
/// merge actually lands on and is checked against everywhere else this
/// session (`preflight`/`bdepend_avail`/`host_copies`).
fn host_installed_versions(
    roots: &portage_resolve::Roots,
    cat: &str,
    pkg: &str,
) -> Vec<(Version, String)> {
    let root = roots.satisfaction_root(DepClass::Bdepend);
    let Ok(vdb) = Vdb::open(root.join("var/db/pkg")) else {
        return Vec::new();
    };
    let Some(category) = vdb.category(cat) else {
        return Vec::new();
    };
    let mut out: Vec<(Version, String)> = category
        .packages()
        .collect_vec()
        .into_iter()
        .filter(|p| p.cpn().package.as_str() == pkg)
        .filter_map(|p| Some((p.cpv().version.clone(), p.slot_main().ok()?)))
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// Every version `cat/pkg` has an ebuild for in `gentoo` (the on-disk
/// `::gentoo` checkout), parsed from the ebuild filenames — no md5-cache
/// read needed, matching `alias_repo_conf_entry`'s own lightweight
/// filesystem-only existence check.
fn ebuild_versions(gentoo: &Utf8Path, cat: &str, pkg: &str) -> Vec<Version> {
    let dir = gentoo.join(cat).join(pkg);
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    read_dir
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?.strip_suffix(".ebuild")?;
            let pf = Pf::parse(name).ok()?;
            (pf.package.as_str() == pkg).then_some(pf.version)
        })
        .collect()
}

/// `{major}.{minor}.9999` from `version`'s first two numeric components —
/// the upper bound of its release branch (see [`host_arch_keyword_line`]):
/// includes every dated snapshot on that branch (`X.Y.Z_pDATE` sorts below
/// `X.Y.9999`) but excludes the branch's own live/rolling `X.Y.9999` ebuild
/// and any newer branch or slot.
fn branch_bound(version: &Version) -> String {
    let major = version.numbers.first().copied().unwrap_or(0);
    let minor = version.numbers.get(1).copied().unwrap_or(0);
    format!("{major}.{minor}.9999")
}

/// The `package.accept_keywords` line for a host-arch cross-category package
/// (`{category}/{pkg}`, aliasing the real `{real_cat}/{real_pkg}`): mirror
/// what the *host* would already select for the real package instead of
/// blanket-accepting the whole category regardless of version — which
/// silently preferred live `9999` ebuilds over perfectly good dated
/// snapshots/releases (`todo/root-topology-refactor.md`, the `dev-vcs/git`
/// finding, and this same overreach for the toolchain packages themselves —
/// confirmed live: `sys-devel/binutils`/`sys-devel/gcc` both carry real
/// `~riscv` keywords on recent non-live snapshots in this environment, so
/// a blanket `**` was never actually necessary to unblock riscv crossdev).
///
/// - Host already has a version of the real package installed, and its
///   exact ebuild still exists in the tree: pin exactly to that version —
///   the cross-alias tracks the host's own installed toolchain version, so
///   host and cross compiler don't silently drift apart.
/// - Installed but that exact ebuild is gone from the tree (or nothing is
///   installed at all): bound to the release branch of the reference
///   version (the installed one, or the newest available if none is
///   installed) via [`branch_bound`] — still `**` within that bound, since
///   some packages (`sys-devel/rust-std`) are permanently unkeyworded by
///   Gentoo convention, not "live" in the churning sense, and still need it.
fn host_arch_keyword_line(
    roots: &portage_resolve::Roots,
    gentoo: &Utf8Path,
    category: &str,
    pkg: &str,
    real_cat: &str,
    real_pkg: &str,
) -> String {
    let available = ebuild_versions(gentoo, real_cat, real_pkg);
    let installed = host_installed_versions(roots, real_cat, real_pkg);

    if let Some((version, slot)) = installed.first() {
        let slot_suffix = if slot != "0" {
            format!(":{slot}")
        } else {
            String::new()
        };
        if available.contains(version) {
            return format!("={category}/{pkg}-{version}{slot_suffix} **\n");
        }
        let bound = branch_bound(version);
        return format!("<{category}/{pkg}-{bound}{slot_suffix} **\n");
    }

    match available.iter().max() {
        Some(newest) => {
            let bound = branch_bound(newest);
            format!("<{category}/{pkg}-{bound} **\n")
        }
        // No ebuild at all — unreachable in practice (existence is already
        // validated up front by `alias_repo_conf_entry`), but a blanket `**`
        // is a safe, honest fallback rather than silently writing nothing.
        None => format!("{category}/{pkg} **\n"),
    }
}

/// Write the cross packages' `package.env` + `env/<category>/<pkg>.conf` into the
/// config root's `etc/portage` (where the host-side `cross-*` builds read it).
///
/// Each env file carries the collision-safety crossdev sets on every cross
/// package (`SYMLINK_LIB=no`, a `COLLISION_IGNORE` for the build-id tree) plus
/// the per-ABI multilib block from [`multilib`] (crossdev's `load_multilib_env`):
/// the target ABI's `CFLAGS_<abi>` (`-mabi=lp64d -march=rv64gc`) is what lets the
/// libc build for `<CTARGET>` instead of inheriting the host CFLAGS. em owns these
/// generated files (like crossdev, which regenerates them each run), so they are
/// rewritten rather than preserved.
fn cross_env_entries(
    target: &CrossTarget,
    globals: &Cli,
    gentoo: &Utf8Path,
    extras: &[Cpn],
) -> Result<Vec<config_plan::ConfigEntry>> {
    let eclass_dir = gentoo.join("eclass");
    let host_ml = multilib::query(&host_chost(), &eclass_dir)?;
    let target_ml = multilib::query(&target.tuple, &eclass_dir)?;

    let header = format!(
        "CTARGET={}\nSYMLINK_LIB=no\nCOLLISION_IGNORE=\"${{COLLISION_IGNORE}} /usr/lib/debug/.build-id\"\n",
        target.tuple
    );

    // Write into the outer EROOT's `etc/portage`, where the `cross-<tuple>/*`
    // builds read config (the staged driver routes them through
    // `outer_roots()` under `bypass_cross_root` — see `emerge.rs`; that's `/`
    // for `--root`/bare, the prefix for `--prefix`/`--local`, never `--target`'s
    // sysroot substitution). These are HOST-arch-built packages (binutils/gcc
    // produce target code, glibc/linux-headers carry target runtime info)
    // managed via package.env so `emerge -u cross-<tuple>/glibc` works —
    // exactly what real crossdev does (`/etc/portage/package.env/cross-<tuple>`).
    // Write into the build config the `cross-<tuple>/*` packages read — the
    // per-target CTARGET/ABI-CFLAGS env files plus the `package.env` mapping
    // that binds them to each cross package. The read path (`env_files_for`,
    // `ebuild.rs`) consults the config overlay *on top of* the config root, so
    // we write into the overlay when one exists (`--prefix`/`--local`: the
    // user-writable `<prefix>/etc/portage`, avoiding a privileged write to host
    // `/etc/portage`), and fall back to the bare config root otherwise
    // (`--root`/plain host). This keeps the cross env scoped to the prefix and
    // unprivileged, and is read back correctly in every mode — including
    // `bypass_cross_root` toolchain steps, whose `outer_roots()` preserves the
    // same `config_overlay`.
    let base = globals.base_roots();
    let portage = if let Some(overlay) = base.config_overlay() {
        overlay.to_owned()
    } else {
        base.merge_root().join("etc/portage")
    };
    let category = target.category();

    let env_dir = portage.join("env").join(&category);
    let mut entries = Vec::new();

    let mut mappings = String::new();
    // Host-arch tools (binutils/gcc/clang-crossdev-wrappers — see
    // `PackageArch` on `CrossTarget::packages`) run *on* the build host, not
    // the target, even though they live in the target-influenced
    // `cross-<tuple>` category. Their own keyword acceptance must never
    // depend on whichever arch happens to be active for a given invocation
    // (the sysroot's target arch, under `--target`, vs the bare host arch
    // otherwise) — found live 2026-07-09: a newer `cross-<tuple>/gcc`
    // resolved fine under `--target` (the generated sysroot make.conf's own
    // `ACCEPT_KEYWORDS="{arch} ~{arch}"` happens to cover it) but failed
    // outright without `--target` (the bare host's real, normally
    // stable-only ACCEPT_KEYWORDS does not). A blanket `**` "fixed" that but
    // overshot: it also silently preferred each package's live `9999` ebuild
    // over a perfectly good dated snapshot, whenever the solver's own
    // dual-root expansion or the toolchain packages' own root-atom
    // resolution had to pick a version at all (`todo/root-topology-
    // refactor.md`, "auto-unmasking is too eager"). `host_arch_keyword_line`
    // mirrors what the host would already select for the real package
    // instead — pinned to its installed version, or bounded to its release
    // branch — using `**` only within that scope, not the whole category.
    let mut keyword_entries = String::new();
    for (real_cat, pkg, arch) in target.packages() {
        let body = format!(
            "{header}{}",
            multilib::env_block(&host_ml, &target_ml, arch.is_target())
        );
        entries.push(config_plan::ConfigEntry::File {
            path: env_dir.join(format!("{pkg}.conf")),
            desired: body,
        });
        mappings.push_str(&format!("{category}/{pkg} {category}/{pkg}.conf\n"));
        if arch == target::PackageArch::Host {
            keyword_entries.push_str(&host_arch_keyword_line(
                &base, gentoo, &category, pkg, real_cat, pkg,
            ));
        }
    }
    // `--ex-pkg`/`--ex-gdb` extras: always the host-ABI branch, matching real
    // crossdev's `for_each_extra_pkg set_portage X` (set_env's `case ${l} in
    // K|L) target ;; *) host` always falls to the host branch for `l=X`) —
    // and get the same host-mirrored keyword line as the base host-arch
    // tools above (e.g. `sys-devel/rust-std`, permanently unkeyworded by
    // Gentoo convention — not live in the churning sense — still resolves,
    // since nothing installed on the host falls through to the newest
    // available version, branch-bounded).
    for cpn in extras {
        let pkg = cpn.package;
        let body = format!(
            "{header}{}",
            multilib::env_block(&host_ml, &target_ml, false)
        );
        entries.push(config_plan::ConfigEntry::File {
            path: env_dir.join(format!("{pkg}.conf")),
            desired: body,
        });
        mappings.push_str(&format!("{category}/{pkg} {category}/{pkg}.conf\n"));
        keyword_entries.push_str(&host_arch_keyword_line(
            &base,
            gentoo,
            &category,
            pkg.as_str(),
            cpn.category.as_str(),
            pkg.as_str(),
        ));
    }

    entries.push(config_plan::ConfigEntry::File {
        path: portage.join("package.env").join(&category),
        desired: mappings,
    });
    entries.push(config_plan::ConfigEntry::File {
        path: portage.join("package.accept_keywords").join(&category),
        desired: keyword_entries,
    });
    Ok(entries)
}

/// Create the ABI osdir compatibility symlinks the libc leaves out, so the cross
/// gcc finds the target CRT/libc.
///
/// `multilib.eclass` gives the **default ABI** the *un-suffixed* libdir (riscv
/// `LIBDIR_lp64d=lib64`, vs non-default `lp64 → lib64/lp64`), and glibc installs
/// its CRTs/`libc.so` straight into that bare `lib64`. But gcc searches the
/// ABI-suffixed osdir (`lib64/lp64d`), so without a bridge `<CTARGET>-gcc` (and
/// the gcc-stage2 self-build) fails with `cannot find Scrt1.o`. A real crossdev
/// sysroot carries `lib64/lp64d -> .` (and `usr/lib64/lp64d -> .`) — untracked
/// compat symlinks no package owns; em creates them here after the libc lands.
fn link_abi_osdirs(target: &CrossTarget, globals: &Cli) -> Result<()> {
    let sysroot = sysroot(target, globals);
    let gentoo = main_repo(globals)?;
    let ml = multilib::query(&target.tuple, &gentoo.path().join("eclass"))?;
    let default_abi = ml.default_abi();
    // Only the default ABI is bare-named (`lib64` rather than `lib64/<abi>`);
    // the others already install into their suffixed osdir, which gcc finds.
    let Some(libdir) = ml.libdir(default_abi) else {
        return Ok(());
    };
    if libdir.contains('/') || default_abi.is_empty() || default_abi == "default" {
        return Ok(());
    }
    for base in [sysroot.clone(), sysroot.join("usr")] {
        let dir = base.join(libdir);
        if !dir.is_dir() {
            continue;
        }
        let link = dir.join(default_abi);
        symlink_force(Utf8Path::new("."), &link)?;
        println!("    osdir compat: {link} -> .");
    }
    Ok(())
}

/// The host `CHOST` (= the target's `CBUILD`), read from the host `make.conf`.
fn host_chost() -> String {
    MakeConf::load_default()
        .ok()
        .and_then(|m| m.get("CHOST").map(str::to_owned))
        .unwrap_or_else(|| "unknown-host".to_owned())
}

/// Replace whatever is at `link` with a symlink to `dst` (absolute target, so it
/// resolves the same from a sysroot offset).
fn symlink_force(dst: &Utf8Path, link: &Utf8Path) -> Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(_) => std::fs::remove_file(link).with_context(|| format!("removing {link}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("stat {link}")),
    }
    std::os::unix::fs::symlink(dst, link).with_context(|| format!("linking {link} -> {dst}"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn crossdev_args(show_target_cfg: bool) -> CrossdevArgs {
        CrossdevArgs {
            llvm: false,
            init_target: false,
            setup: false,
            show_target_cfg,
            ex_pkg: Vec::new(),
            ex_gdb: false,
            depgraph_flags: crate::cli::DepgraphFlags::default(),
            merge_flags: crate::cli::MergeFlags::default(),
        }
    }

    /// Test-only compatibility shim: build the alias `ConfigEntry` and apply
    /// it immediately (no preview/confirm), matching the old
    /// `write_alias_repo_conf`'s eager-write behaviour the tests below assert
    /// against.
    fn write_alias_repo_conf(
        globals: &Cli,
        gentoo: &Utf8Path,
        target: &CrossTarget,
        category: &str,
    ) -> Result<()> {
        let entry = alias_repo_conf_entry(globals, gentoo, target, category, &[])?;
        config_plan::apply_now(std::slice::from_ref(&entry))
    }

    /// `--target` is global: `em --target T crossdev --show-target-cfg`
    /// reads it straight off `Cli`. One flag for both "set up" and "use" —
    /// no local `-t` to disagree with it. `--show-target-cfg` only prints
    /// (no filesystem writes), so `run()` is safe to exercise directly here.
    #[tokio::test]
    async fn run_reads_the_global_target() {
        let cli = crate::cli::Cli::parse_from([
            "em",
            "--target",
            "riscv64-unknown-linux-gnu",
            "crossdev",
            "--show-target-cfg",
        ]);
        let args = crossdev_args(true);
        let result = run(&args, &cli).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    /// Neither given: a clear error, not a panic or a silent bare-host guess.
    #[tokio::test]
    async fn run_without_target_is_an_error() {
        let cli = crate::cli::Cli::parse_from(["em", "crossdev", "--show-target-cfg"]);
        let args = crossdev_args(true);
        assert!(run(&args, &cli).await.is_err());
    }

    #[test]
    fn alias_packages_line_is_the_real_cpns_in_stage_order() {
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let line = alias_packages_line(&target, &[]);
        // Every token is a real ::gentoo cpn, in packages() order, no cross
        // category, no version — pure derivation source for Location::Alias.
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let expected: Vec<String> = target
            .packages()
            .into_iter()
            .map(|(c, p, _)| format!("{c}/{p}"))
            .collect();
        assert_eq!(
            tokens,
            expected.iter().map(|s| s.as_str()).collect::<Vec<_>>()
        );
        // Every token round-trips through Cpn::parse (the repos.conf reader
        // re-parses these, so an unparseable token would silently drop a
        // package from the derivation map).
        for tok in &tokens {
            assert!(
                portage_atom::Cpn::parse(tok).is_ok(),
                "alias-packages token {tok:?} is not a valid Cpn"
            );
        }
        assert!(!tokens.contains(&"sys-devel/gcc") || line.contains("sys-devel/gcc"));
    }

    #[test]
    fn ex_pkg_atoms_parses_category_pn() {
        let mut args = crossdev_args(false);
        args.ex_pkg = vec!["sys-devel/rust-std".to_owned()];
        let atoms = ex_pkg_atoms(&args).unwrap();
        assert_eq!(atoms, vec![Cpn::new("sys-devel", "rust-std")]);
    }

    #[test]
    fn ex_pkg_atoms_rejects_bad_shape() {
        let mut args = crossdev_args(false);
        args.ex_pkg = vec!["not-a-cpn".to_owned()];
        let err = ex_pkg_atoms(&args).expect_err("bare package name rejected");
        assert!(format!("{err:#}").contains("not-a-cpn"));
    }

    #[test]
    fn ex_gdb_is_sugar_for_ex_pkg_dev_debug_gdb() {
        let mut args = crossdev_args(false);
        args.ex_gdb = true;
        let atoms = ex_pkg_atoms(&args).unwrap();
        assert_eq!(atoms, vec![Cpn::new("dev-debug", "gdb")]);

        // Combines with explicit --ex-pkg atoms too, in order.
        args.ex_pkg = vec!["sys-devel/rust-std".to_owned()];
        let atoms = ex_pkg_atoms(&args).unwrap();
        assert_eq!(
            atoms,
            vec![
                Cpn::new("sys-devel", "rust-std"),
                Cpn::new("dev-debug", "gdb")
            ]
        );
    }

    /// `--ex-pkg` extras: validated for existence like the base set, appended
    /// to the alias-packages line, and always get the host-ABI env +
    /// `**` keyword treatment (real crossdev's `--ex-pkg` is always host-arch,
    /// regardless of what the package actually does).
    #[test]
    fn ex_pkg_extras_are_validated_aliased_and_host_classified() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let gentoo = root.join("gentoo");
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let category = target.category();
        for (cat, pkg, _) in target.packages() {
            std::fs::create_dir_all(gentoo.join(cat).join(pkg)).unwrap();
        }
        let globals = test_cli_at_root(root);

        // Missing extra: rejected up front, same shape as a missing base package.
        let missing = [Cpn::new("sys-devel", "rust-std")];
        let err = alias_repo_conf_entry(&globals, &gentoo, &target, &category, &missing)
            .expect_err("missing --ex-pkg source rejected");
        assert!(format!("{err:#}").contains("sys-devel/rust-std"));

        // Present: appended to the alias-packages line.
        std::fs::create_dir_all(gentoo.join("sys-devel").join("rust-std")).unwrap();
        let entry = alias_repo_conf_entry(&globals, &gentoo, &target, &category, &missing).unwrap();
        let config_plan::ConfigEntry::Alias { packages_line, .. } = &entry else {
            panic!("expected an Alias entry");
        };
        assert!(packages_line.ends_with("sys-devel/rust-std"));

        // `cross_env_entries`'s host-ABI/`**`-keyword treatment for extras
        // needs a real `multilib.eclass` (sourced via brush) that a bare
        // temp-dir fixture doesn't have — live-verified separately instead
        // (see todo/root-topology-refactor.md), same as the rest of
        // `write_cross_env`'s multilib-dependent behaviour, which has no
        // unit test either for the same reason.
    }

    #[test]
    fn branch_bound_uses_major_minor() {
        assert_eq!(
            branch_bound(&Version::parse("16.2.1_p20260523").unwrap()),
            "16.2.9999"
        );
        // Single-component version: minor defaults to 0.
        assert_eq!(branch_bound(&Version::parse("9").unwrap()), "9.0.9999");
    }

    fn write_ebuild(gentoo: &camino::Utf8Path, cat: &str, pkg: &str, version: &str) {
        let dir = gentoo.join(cat).join(pkg);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{pkg}-{version}.ebuild")), "").unwrap();
    }

    #[test]
    fn ebuild_versions_lists_versions_from_filenames() {
        let dir = tempfile::tempdir().unwrap();
        let gentoo = camino::Utf8Path::from_path(dir.path())
            .unwrap()
            .join("gentoo");
        write_ebuild(&gentoo, "sys-devel", "gcc", "15.2.1_p20260214");
        write_ebuild(&gentoo, "sys-devel", "gcc", "16.2.9999");

        let mut versions = ebuild_versions(&gentoo, "sys-devel", "gcc");
        versions.sort();
        assert_eq!(
            versions,
            vec![
                Version::parse("15.2.1_p20260214").unwrap(),
                Version::parse("16.2.9999").unwrap(),
            ]
        );
    }

    fn write_vdb_entry(root: &camino::Utf8Path, cat: &str, pf: &str, slot: &str) {
        let dir = root.join("var/db/pkg").join(cat).join(pf);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("EAPI"), "8").unwrap();
        std::fs::write(dir.join("SLOT"), slot).unwrap();
        std::fs::write(dir.join("CONTENTS"), "").unwrap();
        std::fs::write(dir.join("USE"), "").unwrap();
    }

    #[test]
    fn host_installed_versions_reads_the_given_broot() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        write_vdb_entry(root, "sys-devel", "binutils-2.46.0", "2.46");
        let roots = portage_resolve::Roots::for_test(root.as_str());

        let installed = host_installed_versions(&roots, "sys-devel", "binutils");
        assert_eq!(
            installed,
            vec![(Version::parse("2.46.0").unwrap(), "2.46".to_owned())]
        );
    }

    /// Nothing installed, no ebuilds at all: the safe fallback is a blanket
    /// `**` rather than silently writing nothing (existence is otherwise
    /// already validated up front by `alias_repo_conf_entry`).
    #[test]
    fn host_arch_keyword_line_falls_back_to_blanket_when_nothing_exists() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let gentoo = root.join("gentoo");
        let roots = portage_resolve::Roots::for_test(root.as_str());

        let line = host_arch_keyword_line(
            &roots,
            &gentoo,
            "cross-riscv64-unknown-linux-gnu",
            "gcc",
            "sys-devel",
            "gcc",
        );
        assert_eq!(line, "cross-riscv64-unknown-linux-gnu/gcc **\n");
    }

    /// Nothing installed, but ebuilds exist: bound to the newest available
    /// version's branch — this is the `sys-devel/rust-std` shape (never
    /// installed on the host, permanently unkeyworded, still needs `**`
    /// scoped to its own branch rather than a blanket category-wide grant).
    #[test]
    fn host_arch_keyword_line_bounds_to_newest_when_nothing_installed() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let gentoo = root.join("gentoo");
        write_ebuild(&gentoo, "sys-devel", "rust-std", "1.94.0");
        write_ebuild(&gentoo, "sys-devel", "rust-std", "1.95.0");
        let roots = portage_resolve::Roots::for_test(root.as_str());

        let line = host_arch_keyword_line(
            &roots,
            &gentoo,
            "cross-riscv64-unknown-linux-gnu",
            "rust-std",
            "sys-devel",
            "rust-std",
        );
        assert_eq!(
            line,
            "<cross-riscv64-unknown-linux-gnu/rust-std-1.95.9999 **\n"
        );
    }

    /// Installed, and that exact version's ebuild still exists in the tree:
    /// pin exactly to it (host and cross-compiler track the same version).
    #[test]
    fn host_arch_keyword_line_pins_the_installed_version_when_still_available() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let gentoo = root.join("gentoo");
        write_ebuild(&gentoo, "sys-devel", "binutils", "2.45.1");
        write_ebuild(&gentoo, "sys-devel", "binutils", "2.46.0");
        write_vdb_entry(root, "sys-devel", "binutils-2.46.0", "2.46");
        let roots = portage_resolve::Roots::for_test(root.as_str());

        let line = host_arch_keyword_line(
            &roots,
            &gentoo,
            "cross-riscv64-unknown-linux-gnu",
            "binutils",
            "sys-devel",
            "binutils",
        );
        assert_eq!(
            line,
            "=cross-riscv64-unknown-linux-gnu/binutils-2.46.0:2.46 **\n"
        );
    }

    /// Installed, but that exact version's ebuild is gone from the tree
    /// (e.g. cleaned up after a version bump): bound to the installed
    /// version's own branch instead of silently jumping to whatever's
    /// newest (which could be a different, newer branch/slot).
    #[test]
    fn host_arch_keyword_line_bounds_to_installed_branch_when_exact_version_gone() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let gentoo = root.join("gentoo");
        // The installed version itself is no longer in the tree; a newer
        // slot (17) is present but must not be silently preferred.
        write_ebuild(&gentoo, "sys-devel", "gcc", "16.2.1_p20260523");
        write_ebuild(&gentoo, "sys-devel", "gcc", "17.0.9999");
        write_vdb_entry(root, "sys-devel", "gcc-16.1.0", "16");
        let roots = portage_resolve::Roots::for_test(root.as_str());

        let line = host_arch_keyword_line(
            &roots,
            &gentoo,
            "cross-riscv64-unknown-linux-gnu",
            "gcc",
            "sys-devel",
            "gcc",
        );
        assert_eq!(
            line,
            "<cross-riscv64-unknown-linux-gnu/gcc-16.1.9999:16 **\n"
        );
    }

    /// `write_alias_repo_conf` emits a `Location::Alias` repos.conf entry that
    /// (a) parses back into the expected alias declaration, (b) is idempotent
    /// across re-runs with the same target, and (c) rejects a missing source
    /// package up front with a clear error. Covers the producer half of
    /// derive-on-the-fly in isolation from the prefix-bootstrap topology.
    #[test]
    fn write_alias_repo_conf_emits_a_parseable_alias_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let conf = root.join("etc/portage/repos.conf");
        let gentoo = root.join("gentoo");
        // Skeleton ::gentoo with just the source packages' dirs present.
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let category = target.category();
        for (cat, pkg, _) in target.packages() {
            std::fs::create_dir_all(gentoo.join(cat).join(pkg)).unwrap();
        }
        let globals = test_cli_at_root(root);

        write_alias_repo_conf(&globals, &gentoo, &target, &category).unwrap();
        let name = overlay_name(&target);
        let file = conf.join(format!("{name}.conf"));
        let body = std::fs::read_to_string(&file).unwrap();
        assert!(body.contains("alias-source = gentoo"));
        assert!(body.contains(&format!("alias-target = {category}")));
        assert!(body.contains("alias-packages = "));

        // Parses back into a Location::Alias with the full package set.
        let rc = portage_repo::ReposConf::load_from(std::slice::from_ref(&conf)).unwrap();
        let entry = rc.find(&name).expect("crossdev entry present");
        let portage_repo::Location::Alias { source, aliases } = &entry.location else {
            panic!("expected Location::Alias, got {:?}", entry.location);
        };
        assert_eq!(source, "gentoo");
        let pkgs = aliases
            .get(&category)
            .expect("alias target category present");
        let got: std::collections::HashSet<String> = pkgs.iter().map(|c| c.to_string()).collect();
        for (cat, pkg, _) in target.packages() {
            assert!(
                got.contains(&format!("{cat}/{pkg}")),
                "{cat}/{pkg} missing from parsed alias set {got:?}"
            );
        }

        // Idempotent: a second run with the same target doesn't rewrite.
        let body_before = std::fs::read_to_string(&file).unwrap();
        write_alias_repo_conf(&globals, &gentoo, &target, &category).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), body_before);
    }

    /// Two different cross targets coexist on the same prefix: setting up a
    /// second target must not orphan the first one's alias. Before
    /// per-target naming, both targets shared one fixed `crossdev.conf`/
    /// `[crossdev]` name, so the second `write_alias_repo_conf` call for a
    /// different target either clobbered the first (`Sync`) or, under
    /// `--setup`'s `FillGapsOnly` policy, silently no-op'd since the file
    /// was merely *present* — either way the first target stopped resolving
    /// (found live: `em crossdev -T aarch64... --local --setup` then `em
    /// crossdev -T riscv64... --local --setup` left only the aarch64 alias
    /// on disk, so `cross-riscv64-unknown-linux-gnu/binutils` resolved to
    /// "no ebuilds found").
    #[test]
    fn write_alias_repo_conf_lets_two_targets_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let conf = root.join("etc/portage/repos.conf");
        let gentoo = root.join("gentoo");
        let riscv64 = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let aarch64 = CrossTarget::parse("aarch64-unknown-linux-gnu", false).unwrap();
        for target in [&riscv64, &aarch64] {
            for (cat, pkg, _) in target.packages() {
                std::fs::create_dir_all(gentoo.join(cat).join(pkg)).unwrap();
            }
        }
        let globals = test_cli_at_root(root);

        write_alias_repo_conf(&globals, &gentoo, &riscv64, &riscv64.category()).unwrap();
        write_alias_repo_conf(&globals, &gentoo, &aarch64, &aarch64.category()).unwrap();

        let rc = portage_repo::ReposConf::load_from(std::slice::from_ref(&conf)).unwrap();
        for target in [&riscv64, &aarch64] {
            let name = overlay_name(target);
            let entry = rc
                .find(&name)
                .unwrap_or_else(|| panic!("{name} entry present"));
            let portage_repo::Location::Alias { aliases, .. } = &entry.location else {
                panic!("expected Location::Alias, got {:?}", entry.location);
            };
            assert!(
                aliases.contains_key(&target.category()),
                "{}'s alias missing after setting up the other target",
                target.category()
            );
        }
    }

    /// A stale alias file from an earlier run (a different package set, e.g.
    /// before `gdb` was removed from `CrossTarget::packages()`) must be
    /// refreshed, not left in place. `write_if_absent` alone would silently
    /// no-op here — this was a real, live bug: a re-run of `--init-target`
    /// after a `packages()` change never actually updated the alias, so a
    /// removed package kept resolving until the file was deleted by hand.
    #[test]
    fn write_alias_repo_conf_refreshes_a_stale_own_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let conf = root.join("etc/portage/repos.conf");
        let gentoo = root.join("gentoo");
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let category = target.category();
        for (cat, pkg, _) in target.packages() {
            std::fs::create_dir_all(gentoo.join(cat).join(pkg)).unwrap();
        }
        let globals = test_cli_at_root(root);

        let conf_dir = conf.clone();
        std::fs::create_dir_all(&conf_dir).unwrap();
        let name = overlay_name(&target);
        let file = conf_dir.join(format!("{name}.conf"));
        // Simulate a stale own-entry: our alias format, but a package set
        // that no longer matches what `packages()` computes now.
        std::fs::write(
            &file,
            format!(
                "[{name}]\nalias-source = gentoo\nalias-target = {category}\n\
                 alias-packages = sys-devel/binutils dev-debug/gdb\n"
            ),
        )
        .unwrap();

        write_alias_repo_conf(&globals, &gentoo, &target, &category).unwrap();
        let refreshed = std::fs::read_to_string(&file).unwrap();
        assert!(
            !refreshed.contains("dev-debug/gdb"),
            "stale alias-packages line was not refreshed: {refreshed:?}"
        );
        let expected = format!(
            "[{name}]\nalias-source = gentoo\nalias-target = {category}\n\
             alias-packages = {}\n",
            alias_packages_line(&target, &[])
        );
        assert_eq!(refreshed, expected);
    }

    /// A foreign, non-alias `[crossdev]` entry (e.g. a real crossdev/eselect-
    /// managed physical overlay pointing `location =` at a real repo
    /// directory) must never be touched — only an entry recognisably written
    /// by `em` itself (has an `alias-target =` key) is ever refreshed.
    #[test]
    fn write_alias_repo_conf_never_touches_a_foreign_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let conf = root.join("etc/portage/repos.conf");
        let gentoo = root.join("gentoo");
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let category = target.category();
        for (cat, pkg, _) in target.packages() {
            std::fs::create_dir_all(gentoo.join(cat).join(pkg)).unwrap();
        }
        let globals = test_cli_at_root(root);

        std::fs::create_dir_all(&conf).unwrap();
        let name = overlay_name(&target);
        let file = conf.join(format!("{name}.conf"));
        let foreign = format!("[{name}]\nlocation = /var/db/repos/{OVERLAY_NAME}\n");
        std::fs::write(&file, &foreign).unwrap();

        write_alias_repo_conf(&globals, &gentoo, &target, &category).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), foreign);
    }

    /// A source package missing from ::gentoo is rejected before any alias is
    /// written — the producer never declares a derivation it can't satisfy.
    #[test]
    fn write_alias_repo_conf_rejects_a_missing_source_package() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let gentoo = root.join("gentoo");
        // Empty ::gentoo: none of the source packages exist.
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let category = target.category();
        let globals = test_cli_at_root(root);
        let err = write_alias_repo_conf(&globals, &gentoo, &target, &category)
            .expect_err("missing source package rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("not found") && msg.contains(&category),
            "error should name the cross category and missing source: {msg}"
        );
    }

    /// Build a `Cli` whose roots resolve under `root`, so `setup_root`/config
    /// helpers used by the writer land inside the tempdir.
    fn test_cli_at_root(root: &camino::Utf8Path) -> Cli {
        use clap::Parser;
        // `--config-root` scopes both config reads and `setup_root` writes.
        Cli::parse_from([
            "em",
            "--config-root",
            root.as_str(),
            "--root",
            root.as_str(),
        ])
    }

    #[test]
    fn gcc_needs_refresh_cases() {
        // Nothing activated yet: always needs building.
        assert!(gcc_needs_refresh(None, "16"));
        // Older active slot: needs a refresh.
        assert!(gcc_needs_refresh(Some("15"), "16"));
        // Matching or newer active slot: fine as-is.
        assert!(!gcc_needs_refresh(Some("16"), "16"));
        assert!(!gcc_needs_refresh(Some("17"), "16"));
        // Unparseable slots: can't tell, don't force a rebuild.
        assert!(!gcc_needs_refresh(Some("not-a-number"), "16"));
        assert!(!gcc_needs_refresh(Some("15"), "not-a-number"));
    }

    #[test]
    fn atom_is_package_matches_bare_and_version_pinned_atoms() {
        // Bare atom, as toolchain_plan's own gcc-stage1/gcc-stage2 use.
        assert!(atom_is_package(
            "cross-riscv64-unknown-linux-gnu/gcc",
            "gcc"
        ));
        // Version-pinned atom, as gcc_refresh_plan uses to force an exact
        // upgrade — the bug this test guards: a bare `ends_with("/gcc")`
        // check misses this form entirely, silently skipping activation of
        // the freshly-built compiler.
        assert!(atom_is_package(
            "=cross-riscv64-unknown-linux-gnu/gcc-16.1.1_p20260606",
            "gcc"
        ));
        // Doesn't false-positive on an unrelated package with a shared prefix.
        assert!(!atom_is_package(
            "cross-riscv64-unknown-linux-gnu/gcc-doc",
            "gcc"
        ));
        assert!(!atom_is_package("sys-devel/binutils", "gcc"));
    }

    /// The sysroot-wide `make.conf` must never set `CTARGET`: unlike real
    /// crossdev, which scopes it via `package.env` to the host-side
    /// `cross-<CTARGET>/{binutils,gcc,...}` builds only, a sysroot-wide
    /// `CTARGET` leaks into every ordinary package's `econf` invocation
    /// (`--target=`), which non-autoconf `configure` scripts (e.g. sqlite's)
    /// reject outright.
    #[test]
    fn make_conf_body_never_sets_ctarget() {
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let body = make_conf_body(
            &target,
            Utf8Path::new("/usr/riscv64-unknown-linux-gnu"),
            Utf8Path::new("/"),
        );
        assert!(
            !body.lines().any(|l| l.starts_with("CTARGET=")),
            "sysroot make.conf must not set CTARGET:\n{body}"
        );
        assert!(body.contains("CHOST=riscv64-unknown-linux-gnu"));
    }

    /// The sysroot make.conf is the *only* config `sys-devel/gcc` and every
    /// other ordinary stage1 package resolved against `--target` ever reads —
    /// unlike the self-contained `--root`'s own make.conf
    /// (`setup::host_makeopts`'s doc comment), there is no fallback host
    /// config to inherit build parallelism from. Missing this made a real
    /// stage1 build run fully serial (one `cc1plus` at a time on a 128-core
    /// host) — caught live while chasing an unrelated gcc version-mismatch
    /// bug, the same class of gap as `self_contained_root_gets_real_makeopts`
    /// in `setup.rs`.
    #[test]
    fn make_conf_body_sets_makeopts() {
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let body = make_conf_body(
            &target,
            Utf8Path::new("/usr/riscv64-unknown-linux-gnu"),
            Utf8Path::new("/"),
        );
        assert!(body.contains("MAKEOPTS="), "sysroot make.conf:\n{body}");
        assert!(
            !body.contains("MAKEOPTS=\"\""),
            "must be non-empty:\n{body}"
        );
    }

    /// Regression test for the iproute2 stage3 failure: `./configure` ran
    /// plain `pkg-config`, found the *host's* `net-libs/libtirpc.pc`
    /// (`net-libs/libtirpc` isn't even in DEPEND — USE=-nfs — let alone
    /// installed in the target sysroot), and linked `-ltirpc` into a build
    /// that then failed since the library genuinely isn't in the sysroot.
    /// `PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR` must scope pkg-config to
    /// the sysroot so a foreign-arch cross build never sees host `.pc` files.
    #[test]
    fn make_conf_body_scopes_pkg_config_to_the_sysroot() {
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let sysroot = "/var/tmp/cross-stage1-riscv64/usr/riscv64-unknown-linux-gnu";
        let body = make_conf_body(
            &target,
            Utf8Path::new(sysroot),
            Utf8Path::new("/var/tmp/cross-stage1-riscv64"),
        );
        assert!(
            body.contains(&format!("PKG_CONFIG_SYSROOT_DIR=\"{sysroot}\"")),
            "sysroot make.conf:\n{body}"
        );
        assert!(
            body.contains("PKG_CONFIG_LIBDIR=")
                && body.contains(&format!("{sysroot}/usr/lib64/pkgconfig"))
                && body.contains(&format!("{sysroot}/usr/share/pkgconfig")),
            "PKG_CONFIG_LIBDIR must point into the sysroot only:\n{body}"
        );
        // PKG_CONFIG_LIBDIR *replaces* the default search list — the whole
        // point is that no host pkgconfig dir leaks in.
        assert!(
            !body.contains("PKG_CONFIG_PATH="),
            "must not additively leak the host's pkgconfig search path:\n{body}"
        );
    }

    /// meson.eclass (and any buildsystem following the same convention) reads
    /// `BUILD_PKG_CONFIG_LIBDIR` for its native build-machine pkg-config
    /// search path, falling back to the *target* `PKG_CONFIG_LIBDIR` when
    /// unset — the same host/target conflation that broke
    /// `sys-devel/binutils`'s bare `zstd.m4` check (#29), just for
    /// buildsystems that otherwise get this right. It must point at the
    /// outer EROOT (where Host BDEPEND packages actually build — see
    /// `entry_roots()` in `main.rs`), not the target sysroot and not the
    /// bare host `/`.
    #[test]
    fn make_conf_body_sets_build_pkg_config_libdir_to_the_outer_root() {
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let sysroot = "/var/tmp/cross-stage1-riscv64/usr/riscv64-unknown-linux-gnu";
        let outer_root = "/var/tmp/cross-stage1-riscv64";
        let body = make_conf_body(&target, Utf8Path::new(sysroot), Utf8Path::new(outer_root));
        assert!(
            body.contains(&format!(
                "BUILD_PKG_CONFIG_LIBDIR=\"{outer_root}/usr/lib64/pkgconfig"
            )),
            "BUILD_PKG_CONFIG_LIBDIR must point into the outer EROOT:\n{body}"
        );
        assert!(
            !body.contains(&format!("BUILD_PKG_CONFIG_LIBDIR=\"{sysroot}")),
            "BUILD_PKG_CONFIG_LIBDIR must not point into the target sysroot:\n{body}"
        );
    }
}
