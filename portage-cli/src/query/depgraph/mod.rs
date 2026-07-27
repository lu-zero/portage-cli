mod autounmask;

pub use portage_atom_pubgrub::MergeRoot;
mod output;
mod package_use;
mod targets;

pub use targets::{TargetAtom, TargetOrigin};

use portage_resolve::{
    bdepend_trim, conflicts, depend_trim, download_size, effective_use, host_copies, installed,
    repo, required_use, root_aware, subslot, use_env,
};
// Not referenced directly here (only via a `force_mask::ForceMask` value
// returned from `use_env`), but `c7.rs`/`host_copies.rs`'s own tests still
// reach it through `super::force_mask`/`super::super::force_mask` — keep the
// binding alive for them.
#[allow(unused_imports)]
use portage_resolve::force_mask;

use std::collections::{HashMap, HashSet};

use camino::Utf8Path;
use gentoo_core::Arch;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Operator, Version};
use portage_atom_pubgrub::{
    CededFlag, DepClass, DepEdge, InstalledPackage as SolverInstalledPackage, InstalledPolicy,
    PortageDependencyProvider, PortagePackage, PortageVersionSet, UseFlagRequirement, UseOverride,
    build_slot_map,
};

use crate::cli::DepgraphFormat;

/// One entry of the resolved merge list, in install order — everything the
/// build loop needs to emerge it.
pub struct PlannedMerge {
    /// Where this package is merged (`BROOT` host vs target `ROOT`).
    pub merge_root: MergeRoot,
    /// The identity to build/register under (display + work-dir naming +
    /// VDB category) — for a cross-derived package this is the *virtual*
    /// cpv (`cross-<tuple>/gcc-...`), which may differ from the real cpn
    /// `ebuild_path` was resolved through. Kept as a real `Cpv`, not a
    /// formatted string, so nothing downstream has to re-derive it by
    /// parsing a path or string.
    // Design note: this decoupling is documented in todo/cross-derive-on-the-fly.md,
    // "The merge-path decoupling".
    pub cpv: Cpv,
    /// Absolute path to the ebuild.
    pub ebuild_path: camino::Utf8PathBuf,
    /// Effective enabled USE flags for this build: the global config and
    /// per-package overrides resolved per the displayed plan (including
    /// profile-injected implicit flags like `elibc_glibc`/`kernel_linux`,
    /// which USE conditionals test).
    pub use_flags: Vec<Interned<DefaultInterner>>,
    /// `DEPEND` (build-against-sysroot), pre-USE-evaluation, for the pre-flight
    /// build-dependency check (see `preflight`). Empty when no cache entry.
    pub depend: Vec<portage_atom::DepEntry>,
    /// `BDEPEND` (build-host tools), pre-USE-evaluation, for the pre-flight
    /// build-dependency check.
    pub bdepend: Vec<portage_atom::DepEntry>,
    /// This cpv is already installed yet the resolver kept it in the plan — an
    /// explicitly-requested target (emerge reinstalls these by default) or a
    /// same-version USE rebuild. The merge loop must build it rather than treat
    /// the VDB entry as a resume-skip.
    pub reinstall: bool,
}

/// What [`depgraph`] resolved.
pub struct DepgraphOutcome {
    /// Process exit code: `1` when configuration changes are required to
    /// realise the displayed plan (matching `emerge -p`), `0` otherwise.
    pub exit_code: i32,
    /// The merge list in install order.
    pub plan: Vec<PlannedMerge>,
    /// For each `plan` entry, the indices of earlier entries that must finish
    /// building before it can build — its in-plan build-time dependencies
    /// (`DEPEND`/`BDEPEND` edges). Restricted to earlier indices, so it is
    /// always acyclic; the `--jobs` scheduler uses it to parallelise builds
    /// while respecting build order. Empty entry ⇒ no in-plan build deps.
    pub build_blockers: Vec<Vec<usize>>,
    /// `package.provided` CPVs the system supplies, each with the repo slot it
    /// maps onto (derived from the version's slot series). The pre-flight build
    /// check seeds these as present so a build dep on an externally-provided
    /// package (e.g. the host interpreter, `dev-lang/python:3.14`) is not
    /// reported missing — the solver already treats it as satisfied.
    pub provided: Vec<(Cpv, Option<String>)>,
}

pub struct DepgraphOpts<'a> {
    pub repo_path: &'a Utf8Path,
    /// The root targets, each carrying the provenance that decides whether an
    /// unsatisfiable one aborts the run or just warns (see [`TargetOrigin`]).
    pub atoms: &'a [TargetAtom],
    pub arch: &'a Arch,
    pub format: DepgraphFormat,
    pub verbose: u8,
    pub empty: bool,
    pub autounmask_write: bool,
    pub autosolve_use: bool,
    /// Load every repo from `repos.conf` (overlays sourced as needed). Off
    /// when the user pinned a repo with `--repo`.
    pub multi_repo: bool,
    /// The resolved root set (config / base / target / BROOT). See
    /// docs/root-model.md. `roots.satisfaction_root(DepClass::Bdepend)`
    /// answers the Host-routed BDEPEND/IDEPEND question directly — `roots`
    /// carries BROOT correctly even under an active `--target` sysroot
    /// substitution, so a separate `host_roots` field is no longer needed
    /// (see `Cli::roots`'s doc comment).
    pub roots: &'a portage_resolve::Roots,
    /// Where a `MergeRoot::Host` plan entry actually merges — `Cli::broot()`'s
    /// `merge_root()`. Passed separately from `roots` because `roots` can be
    /// `--target`-substituted (its `eprefix`/`is_overlay()` cleared), which
    /// would make the `-p` display fall back to the real host even under an
    /// unprivileged `--prefix` overlay; `Cli::broot()` is computed from
    /// `base_roots()` and stays overlay-aware regardless of `--target`.
    pub host_merge_root: &'a Utf8Path,
    /// `--onlydeps`: drop the explicitly-requested targets from the plan,
    /// keeping only their dependencies (emerge's `--onlydeps`).
    pub onlydeps: bool,
    /// Include BDEPEND in resolution (emerge's `--with-bdeps`). Default false
    /// (exclude BDEPEND) to match emerge's default.
    pub with_bdeps: bool,
    /// emerge's `--root-deps[=rdeps]`: only RDEPEND (not DEPEND) is required
    /// to be satisfiable in the merge target. Caller-supplied rather than
    /// auto-derived from cross-arch detection: it's a property of *which
    /// operation* is running (`em crossdev --setup` bootstrapping a still-empty
    /// target always needs it; `em stages --stage1` building ordinary packages
    /// against an already-working toolchain should not), not of the sysroot's
    /// CHOST/CBUILD alone.
    // See todo/stage-build-shakeout.md for design rationale.
    pub root_deps_rdeps: bool,
    /// `--deep`: re-examine transitive deps. With [`Self::update`], enables
    /// in-slot upgrades for packages in the graph (emerge `-uD`). Alone, bumps
    /// `:*` any-slot deps to the newest slot rather than keeping a satisfying
    /// installed slot.
    pub deep: bool,
    /// `--update`: prefer newest accepted versions. Combined with [`Self::deep`]
    /// for transitive in-slot upgrades; alone only affects atom disambiguation
    /// at the CLI and root-target selection (roots already take best in-slot).
    pub update: bool,
    /// `--newuse` / `-N`: rebuild installed packages in the graph when planned
    /// USE or IUSE differs from the VDB.
    pub newuse: bool,
    /// `--changed-use` / `-U`: like `newuse` but only for enabled-flag flips
    /// among shared IUSE (ignore pure IUSE add/drop).
    pub changed_use: bool,
    /// `--noreplace` / `-n`: leave a named target alone when an installed
    /// version already satisfies it, rather than reinstalling it.
    pub noreplace: bool,
    /// `--nodeps` (emerge `-O`): merge only the named atoms, no dependency
    /// expansion. Used by the staged toolchain bootstrap.
    pub nodeps: bool,
    /// A transient conf-layer USE override for this resolve, e.g. `em stages
    /// --stage1`'s `USE="-* build ${BOOTSTRAP_USE}"` (catalyst's own
    /// recipe). Folded at the conf layer (after real `make.conf`, before
    /// `package.use`/env), matching where catalyst actually places
    /// `CATALYST_USE` — NOT the process environment, which would sit above
    /// `package.use` and incorrectly wipe it. See
    /// `portage_repo::build::profile::resolve_use_flags`'s
    /// `extra_use_override` doc.
    pub extra_use_override: Option<&'a str>,
    /// See `output::PrettyCtx::binpkg_index`'s doc — passed straight through
    /// to the `Pretty` printer so `-p` can show `[binary ...]`.
    pub binpkg_index: Option<&'a portage_binpkg::BinpkgIndex>,
    /// `-X`/`--exclude`: package atoms to never install (emerge's own
    /// wording — "won't install any ebuild or binary package that matches
    /// any of the given atoms"). Applied as a post-solve filter on `order`
    /// (see the filter site below), before *any* consumer — the `-p`/
    /// `--tree`/`--json` preview and the final `PlannedMerge` merge-loop
    /// plan alike — is built from it, so every display and the actual merge
    /// agree. Not integrated into the pubgrub solve itself: a deliberate,
    /// documented simplification — if an excluded package is a genuine hard
    /// dependency of something else still in the plan, that other
    /// package's own preflight/build will fail with a clear
    /// missing-dependency error rather than the solver reporting the
    /// conflict up front the way real portage's backtracking search can.
    pub exclude: &'a [String],
    /// Packages already finished in a prior attempt of a `-r`/`--resume`
    /// job (`maint::resume::completed_keys`). Dropped from `order` the same
    /// way `--exclude` is — so `-p` and the merge plan omit completed work,
    /// which is required for correct `--emptytree` resume (VDB presence is
    /// not a completion marker there). Empty for every non-resume call.
    pub resume_completed: HashSet<(MergeRoot, String)>,
    /// `--complete-graph`: when a deep update (`-uD`) moves a `~`-pinned
    /// family (e.g. `llvm`/`clang`) but leaves a retained installed dependent
    /// behind (`lldb`, whose pin the move now breaks), pull that dependent
    /// into the plan too rather than stopping the chain halfway. Gated
    /// behind an explicit flag because there is no emerge parity to validate
    /// against and the policy can revert an upgrade a dependent has no
    /// satisfying version for — see todo/slot-chain-completion.md.
    pub complete_graph: bool,
}

pub async fn depgraph(opts: DepgraphOpts<'_>) -> anyhow::Result<DepgraphOutcome> {
    let DepgraphOpts {
        repo_path,
        atoms,
        arch,
        format,
        verbose,
        empty,
        autounmask_write,
        autosolve_use,
        multi_repo,
        roots,
        onlydeps,
        with_bdeps,
        root_deps_rdeps,
        deep,
        update,
        newuse,
        changed_use,
        noreplace,
        nodeps,
        host_merge_root,
        extra_use_override,
        binpkg_index,
        exclude,
        resume_completed,
        complete_graph,
    } = opts;
    let exclude_atoms: Vec<Dep> = exclude
        .iter()
        .filter_map(|s| match Dep::parse(s) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("warning: skipping invalid --exclude atom '{s}': {e}");
                None
            }
        })
        .collect();
    // `create_depgraph_params`' `selective`, restricted to the flags em has: an
    // installed instance may satisfy a named target, so the target is left alone
    // instead of being reinstalled. `--emptytree` is the explicit opposite.
    let selective = (update || noreplace || newuse || changed_use) && !empty;
    let cross = root_aware::detect(roots, host_merge_root);
    let config_root = roots.config();
    let host_config_stage = cross.active && cross.sysroot.as_str() != cross.target.as_str();
    // Native `emerge -pe`: pretend nothing merged on TARGET, but BROOT still
    // satisfies BDEPEND (emerge sets `bdeps=auto` unless overridden).
    let emptytree_native = empty && !host_config_stage && !cross.active;
    let solve_with_bdeps = with_bdeps || emptytree_native;
    let repo = crate::repo_open::open(repo_path)
        .map_err(|e| anyhow::anyhow!("failed to open repo at {repo_path}: {e}"))?;

    let (overlays, alias_repos) = crate::repo_open::overlays_from_conf(&repo, roots, multi_repo);

    let (data, (target_installed, installed_blockers), host_installed, use_env_result) = tokio::join!(
        repo::load_repos(&repo, &overlays, &alias_repos),
        // Also precompute each installed package's blocker atoms on this task
        // (for `check_blockers`): the walk only needs the VDB, so it overlaps the
        // other concurrent loads instead of running serially before the solve.
        async {
            let ti = installed::load_target_installed(roots);
            let blockers: Vec<Vec<Dep>> =
                ti.iter().map(conflicts::installed_blocker_atoms).collect();
            (ti, blockers)
        },
        async { installed::load_host_installed(roots) },
        use_env::build_use_env(
            &repo,
            config_root,
            roots.config_overlay(),
            extra_use_override
        ),
    );
    let use_env = use_env_result?;
    let use_env::UseEnv {
        pre_env,
        env_use,
        expand: use_expand,
        expand_hidden: use_expand_hidden,
        package_use,
        package_mask,
        package_unmask,
        force_mask,
        accept_keywords,
        package_accept_keywords,
        accept_license,
        package_license,
        distdir,
        provided,
    } = use_env;

    // Map each `package.provided` CPV onto the repo slot(s) a `:slot` dep would
    // reference (the version sharing its major.minor series), so both the solver
    // (host-seed, below) and the pre-flight check treat it as present at that
    // slot. A CPV with no matching repo version is recorded slotless.
    let provided_avail: Vec<(Cpv, Option<String>)> = provided
        .iter()
        .flat_map(|cpv| {
            let mut slots: Vec<String> = Vec::new();
            if let Some(entries) = data.versions.get(&cpv.cpn) {
                for (rcpv, ce) in entries {
                    if same_slot_series(&rcpv.version, &cpv.version) {
                        let s = ce.metadata.slot.slot.to_string();
                        if !slots.contains(&s) {
                            slots.push(s);
                        }
                    }
                }
            }
            if slots.is_empty() {
                vec![(cpv.clone(), None)]
            } else {
                slots.into_iter().map(|s| (cpv.clone(), Some(s))).collect()
            }
        })
        .collect();

    // Fold global ACCEPT_KEYWORDS and per-package package.accept_keywords into a
    // single interned acceptance decision. A cross build accepts by the TARGET
    // arch (derived from the sysroot `CHOST`), not the host `--arch`, so the
    // target's keywords are honoured — a package keyworded `~riscv`/`riscv` is
    // accepted for a riscv sysroot even though the host is arm64. Without this
    // every target package would be filtered out (NoVersions).
    let accept_arch = cross.target_arch().unwrap_or(arch);
    let accept_keywords =
        repo::AcceptKeywords::new(accept_arch, &accept_keywords, package_accept_keywords);
    // Likewise fold global ACCEPT_LICENSE with per-package package.license.
    let accept_licenses = repo::AcceptLicenses::new(accept_license, package_license);

    let target_installed_cpvs: std::collections::HashSet<Cpv> = target_installed
        .iter()
        .map(|e| Cpv::new(e.cpn, e.version.clone()))
        .collect();
    // `Cpv` carries no `merge_root`, so a `Host`-routed requirement (e.g.
    // `dev-lang/perl` needed at `base_roots()` as a BDEPEND tool) must never be
    // checked against `target_installed_cpvs`: a real target system commonly
    // has its own same-named, same-version package (a *different* build, for
    // a different root) which would otherwise wrongly look "already
    // installed" here — see `todo/stage-build-shakeout.md` #32.
    let host_installed_cpvs: std::collections::HashSet<Cpv> = host_installed
        .iter()
        .map(|e| Cpv::new(*e.package.cpn(), e.version.clone()))
        .collect();
    // Under `--emptytree` the solver treats target packages as rebuilds (not
    // "already installed" for cede/ingest), while action tags still use the
    // real VDB via `target_installed_cpvs`.
    let empty_solver_cpvs = std::collections::HashSet::new();
    let solver_installed_cpvs: &std::collections::HashSet<Cpv> = if emptytree_native {
        &empty_solver_cpvs
    } else {
        &target_installed_cpvs
    };
    // `-N`/`-U` reinstall mode (orthogonal to emptytree Rebuild).
    let use_reinstall_mode = if newuse {
        Some(portage_resolve::use_reinstall::UseReinstallMode::Newuse)
    } else if changed_use {
        Some(portage_resolve::use_reinstall::UseReinstallMode::ChangedUse)
    } else {
        None
    };

    let mut installed: HashMap<Cpn, HashMap<Interned<DefaultInterner>, Version>> = HashMap::new();
    for e in &target_installed {
        let slot_key = e.slot.unwrap_or_else(|| Interned::intern(""));
        installed
            .entry(e.cpn)
            .or_default()
            .insert(slot_key, e.version.clone());
    }

    let target_policy = repo::ResolvePolicy {
        accept_keywords: &accept_keywords,
        package_mask: &package_mask,
        package_unmask: &package_unmask,
        accept_licenses: &accept_licenses,
        pre_env: &pre_env,
        env_use: &env_use,
        package_use: &package_use,
        force_mask: &force_mask,
    };
    let mut root_deps = Vec::new();
    let mut root_cpns: std::collections::HashSet<Cpn> = std::collections::HashSet::new();
    // Root targets with no acceptable candidate, dropped from the solve and
    // reported after the plan (world-family provenance only — anything else is
    // fatal below).
    let mut unsatisfiable: Vec<output::UnsatisfiableTarget> = Vec::new();
    for target in atoms {
        let atom = &target.atom;
        let dep = Dep::parse(atom).map_err(|e| anyhow::anyhow!("bad atom '{atom}': {e}"))?;
        let pkg = repo::target_package(&data, &dep, &target_policy);
        let vs = match &dep.version {
            Some(v) => {
                let op = dep.op.unwrap_or(Operator::GreaterOrEqual);
                PortageVersionSet::from_operator(op, dep.glob, v.clone())
            }
            None => PortageVersionSet::any(),
        };
        // `target_package` hands back an unslotted package when nothing the atom
        // matches survives keyword/mask/license filtering — an identity the
        // provider never registers, so leaving it in `root_deps` turns into an
        // opaque solver failure. Classify it here instead, the way portage does
        // in argument processing.
        if pkg.slot().is_none() {
            let reasons = repo::filter_reasons_for_atom(&data, &dep, &vs, &target_policy);
            let problem = if reasons.is_empty() {
                targets::TargetProblem::NoEbuilds
            } else {
                targets::TargetProblem::AllFiltered
            };
            let unsat = output::UnsatisfiableTarget {
                atom: atom.clone(),
                origin: target.origin.clone(),
                problem,
                reasons,
            };
            // `--emptytree` deliberately ignores what is installed, so a
            // satisfying VDB entry must not silence the atom there.
            let satisfied = !empty && targets::installed_satisfies(&dep, &installed);
            match targets::classify_root_target(&target.origin, satisfied, selective) {
                targets::RootTargetDecision::DropWithWarning => {
                    unsatisfiable.push(unsat);
                    continue;
                }
                targets::RootTargetDecision::DropSilently => continue,
                targets::RootTargetDecision::Fatal => {
                    anyhow::bail!(output::unsatisfiable_target_message(
                        &unsat, &data, multi_repo
                    ));
                }
            }
        }
        root_cpns.insert(dep.cpn);
        root_deps.push((pkg, vs));
    }

    // The whole-repository slot map (unslotted-dep resolution against
    // multi-slot packages) computed once, up front, and reused by every
    // co-solve fixpoint iteration below instead of being recomputed on each
    // provider rebuild — see `build_slot_map`'s doc comment for why that
    // recomputation is the single largest redundant cost in a per-iteration
    // rebuild (~20k CPNs' worth of keyword/mask/license filtering, up to ~8x
    // per invocation). Uses the pristine (pre-cosolve) `package_use`: license
    // acceptance can in principle depend on `package_use` through a
    // USE-conditional LICENSE expression, which *does* vary across
    // iterations, so a package whose license acceptance flips because of a
    // flag the fixpoint later cedes would see a stale slot entry here. That
    // is PMS-legal but vanishingly rare in the real tree, and not worth
    // recomputing the whole map every iteration to cover.
    let slot_map = build_slot_map(&repo::Adapter {
        data: &data,
        accept_keywords: &accept_keywords,
        package_mask: &package_mask,
        package_unmask: &package_unmask,
        accept_licenses: &accept_licenses,
        pre_env: &pre_env,
        env_use: &env_use,
        package_use: &package_use,
        force_mask: &force_mask,
        installed_cpvs: solver_installed_cpvs,
        autosolve_use: false,
    });

    // Sysroot VDB entries for `DEPEND` satisfaction under a cross build:
    // static for the whole invocation (doesn't depend on `pkg_use`), so
    // reading it from disk on every fixpoint iteration — as `build_and_solve`
    // used to, inline — was pure waste. Computed once here instead.
    let sysroot_installed: Vec<(PortagePackage, Version)> = if cross.active {
        installed::load_sysroot_entries(cross.sysroot.as_path())
            .into_iter()
            .map(|e| {
                let pkg = match e.slot.filter(|s| !s.is_empty()) {
                    Some(s) => PortagePackage::slotted(e.cpn, s),
                    None => PortagePackage::unslotted(e.cpn),
                };
                (pkg, e.version)
            })
            .collect()
    } else {
        Vec::new()
    };

    // Only names originally targeted (never a repair target added below) are
    // "explicit" for reinstall/tree-root/onlydeps purposes — computed once,
    // from the untouched `root_deps`, so a repair loop round can never make an
    // added dependent look like a user-requested target.
    let root_pkgs: Vec<PortagePackage> = root_deps.iter().map(|(p, _)| p.clone()).collect();
    let pristine_package_use = package_use.clone();

    /// Everything a single solve-and-plan attempt produces that the rest of
    /// [`depgraph`] needs — bundled so the `--complete-graph` repair loop
    /// below can re-run the whole pipeline with extra root targets and keep
    /// only the last (or last-good) attempt, without printing or writing
    /// anything until a round is actually settled on.
    struct RoundOutcome {
        provider: PortageDependencyProvider,
        solution: pubgrub::SelectedDependencies<PortagePackage, Version>,
        order: Vec<(PortagePackage, Version)>,
        edges: Vec<DepEdge>,
        package_use: Vec<(Dep, Vec<UseOverride>)>,
        applied_reqs: Vec<UseFlagRequirement>,
        ceded: Vec<CededFlag>,
        autounmask_candidates: Vec<repo::AutounmaskCandidate>,
        slot_op_cpns: HashSet<Cpn>,
        dep_conflicts: Vec<conflicts::Conflict>,
        exclude_omitted: usize,
        resume_omitted: usize,
    }

    // Build a provider (with the given cede policy) and run the solve against
    // `targets` (the root deps for this round — `root_deps` plus whatever
    // `--complete-graph` repair targets earlier rounds decided to add).
    // Factored so a failed --autosolve-use attempt can fall back to a
    // fixed-USE (Level A) solve instead of erroring — matching the doc
    // invariant.
    let solve_round =
        |targets: Vec<(PortagePackage, PortageVersionSet)>| -> anyhow::Result<RoundOutcome> {
            let build_and_solve = |autosolve_use: bool, pkg_use: &[(Dep, Vec<UseOverride>)]| {
                let adapter = repo::Adapter {
                    data: &data,
                    accept_keywords: &accept_keywords,
                    package_mask: &package_mask,
                    package_unmask: &package_unmask,
                    accept_licenses: &accept_licenses,
                    pre_env: &pre_env,
                    env_use: &env_use,
                    package_use: pkg_use,
                    force_mask: &force_mask,
                    installed_cpvs: solver_installed_cpvs,
                    autosolve_use,
                };
                // Outlives `adapter` (it borrows the same run-scoped refs, including
                // this iteration's `pkg_use`), so it stays usable after the move below.
                let iteration_policy = adapter.policy();
                // Closure-seeded ingestion: only packages reachable from the targets
                // and the installed set get converted (a few hundred for a typical
                // resolve), instead of the whole tree — this is what makes the
                // co-solve fixpoint's per-iteration provider rebuild affordable.
                let mut seeds: Vec<Cpn> = targets
                    .iter()
                    .filter(|(pkg, _)| !pkg.is_virtual())
                    .map(|(pkg, _)| *pkg.cpn())
                    .collect();
                if !emptytree_native {
                    seeds.extend(target_installed.iter().map(|e| e.cpn));
                }
                let mut provider =
                    PortageDependencyProvider::new_for_targets_with_bdeps_and_slot_map(
                        adapter,
                        seeds,
                        solve_with_bdeps,
                        &slot_map,
                    );
                provider.set_cross_active(cross.active);
                provider.set_is_cross_arch(cross.is_cross_arch());
                // crossdev `--root-deps=rdeps`: caller-supplied (see `DepgraphOpts::
                // root_deps_rdeps`) — a property of which operation is running, not of
                // the sysroot's CHOST/CBUILD.
                provider.set_root_deps_rdeps(root_deps_rdeps);
                provider.set_nodeps(nodeps);
                provider.set_rebuild_tree(emptytree_native);
                // `--deep` and native emptytree bump `:*` deps to the newest slot.
                provider.set_prefer_newest_slot(deep || emptytree_native);
                // `-uD`: in-slot upgrades for the whole solve (not emptytree Rebuild).
                // Also the path that re-opens host-satisfied build edges so deep tools
                // can upgrade/rebuild; `-N` alone must not do that (Portage parity).
                provider.set_prefer_update(update && deep && !emptytree_native);
                // `-n`/`-N`/`-U` without `-u`: a root target an installed version
                // already satisfies keeps that version instead of taking the newest.
                provider.set_selective_no_update(selective && !update);
                for (pkg, version) in &sysroot_installed {
                    provider.add_sysroot_installed(pkg.clone(), version.clone());
                }
                for (e, blockers) in target_installed.iter().zip(&installed_blockers) {
                    let pkg = match e.slot.filter(|s| !s.is_empty()) {
                        Some(s) => PortagePackage::slotted(e.cpn, s),
                        None => PortagePackage::unslotted(e.cpn),
                    };
                    provider.add_installed_blockers(&pkg, blockers);
                    let policy = if emptytree_native {
                        InstalledPolicy::Rebuild
                    } else if let Some(mode) = use_reinstall_mode {
                        // Compare VDB USE/IUSE to the planned fold for this CPV.
                        // Rebuild ⇒ no Favor + full build-dep expansion when selected.
                        if package_needs_use_reinstall(mode, e, &pkg, &data, &iteration_policy) {
                            InstalledPolicy::Rebuild
                        } else {
                            InstalledPolicy::Favor
                        }
                    } else {
                        InstalledPolicy::Favor
                    };
                    provider.add_installed(SolverInstalledPackage {
                        package: pkg,
                        version: e.version.clone(),
                        policy,
                        active_use: e.active_use.clone(),
                        iuse: e.iuse.clone(),
                    });
                }
                // `package.provided`: CPVs the system supplies externally. A dep edge
                // matching one is dropped before it becomes a solver constraint (like a
                // host-satisfied BDEPEND), so the package is neither built nor reported
                // as a dropped/autounmask candidate.
                provider.set_provided(&provided);
                // A `package.provided` CPV is supplied by the *system*, so it is present
                // on the build host (BROOT) too: seed it as host-installed so BDEPEND on
                // it (e.g. a build tool needing the interpreter) is satisfied without
                // resolving a repo version onto @host — otherwise a slot the repo can't
                // build (python:3.14 on arm64-macos) would be pulled to the newest
                // available (python-3.15.9999), conflicting with the provided slot.
                for (cpv, slot) in &provided_avail {
                    let pkg = match slot {
                        Some(s) => PortagePackage::slotted(cpv.cpn, Interned::intern(s)),
                        None => PortagePackage::unslotted(cpv.cpn),
                    };
                    provider.add_host_installed(pkg, cpv.version.clone(), Vec::new(), Vec::new());
                }
                // BROOT (the host) provides build tools: a BDEPEND already present there
                // is satisfied without building it into the plan — unless a USE-dep on
                // that edge demands a flag the host lacks, in which case the package is
                // rebuilt (the host entry carries USE/IUSE for that check).
                for e in &host_installed {
                    provider.add_host_installed(
                        e.package.clone(),
                        e.version.clone(),
                        e.active_use.clone(),
                        e.iuse.clone(),
                    );
                }
                let result = provider.resolve_targets(targets.clone());
                (provider, result)
            };

            // Auto-apply cross-package `[flag]` USE-deps by forcing the demanded flags
            // on real-IUSE targets via synthetic `package.use` and re-solving to a
            // fixpoint. This mirrors emerge's default *preview* semantics: `emerge -p`
            // computes the graph as if the needed USE changes were applied, prints a
            // mandatory "USE changes are necessary to proceed" block, and exits
            // non-zero. User-pinned flags are never forced. `--autosolve-use`
            // additionally cedes REQUIRED_USE flags to the solver (Level C).
            // The fixpoint hands back the final solve it converged on, so we reuse it
            // instead of solving again; `solved` is `None` when the fixpoint
            // failed/bailed and we must re-solve.
            let (package_use, applied_reqs, solved) = package_use::cosolve_use_deps(
                package_use.clone(),
                &data,
                |pu| {
                    let (provider, result) = build_and_solve(autosolve_use, pu);
                    result.ok().map(|sol| (provider, sol))
                },
                |(provider, _)| provider.use_flag_requirements().to_vec(),
            );

            let (provider, solution) = match solved {
                Some(solved) => solved,
                None => {
                    let (provider, result) = build_and_solve(autosolve_use, &package_use);
                    match result {
                        Ok(sol) => (provider, sol),
                        Err(_) if autosolve_use => {
                            // REQUIRED_USE could not be auto-satisfied; fall back to a
                            // fixed-USE solve so the plan + Level-A advisory still appear.
                            eprintln!(
                                "!!! --autosolve-use could not satisfy REQUIRED_USE; \
                             falling back to a fixed-USE plan."
                            );
                            let (provider, result) = build_and_solve(false, &package_use);
                            let sol = result.map_err(|e2| {
                                anyhow::anyhow!(
                                    "resolution failed:\n{}",
                                    portage_atom_pubgrub::format_solve_error(e2)
                                )
                            })?;
                            (provider, sol)
                        }
                        Err(e) => {
                            return Err(anyhow::anyhow!(
                                "resolution failed:\n{}",
                                portage_atom_pubgrub::format_solve_error(e)
                            ));
                        }
                    }
                }
            };

            // Fold Level-C ceded flag values into package.use for report/autounmask
            // consumers that still walk that list. Force/mask is **not** smuggled here
            // anymore — it is applied as a true post-fold step (like Portage) via
            // `effective_use::apply_force_mask` / `Adapter::desired_use`, so env-level
            // `USE="-* …"` cannot wipe forced flags on the display/build path.
            let ceded = provider.solved_use_decisions();
            let package_use: Vec<(Dep, Vec<UseOverride>)> = if ceded.is_empty() {
                package_use
            } else {
                let mut by_cpn: HashMap<Cpn, Vec<&CededFlag>> = HashMap::new();
                for c in &ceded {
                    by_cpn.entry(c.cpn).or_default().push(c);
                }
                let mut combined = package_use;
                for (pkg, ver) in solution.iter() {
                    if pkg.is_virtual() {
                        continue;
                    }
                    let Some(flags) = by_cpn.get(pkg.cpn()) else {
                        continue;
                    };
                    let atom = format!("={}/{}-{}", pkg.cpn().category, pkg.cpn().package, ver);
                    let Ok(dep) = Dep::parse(&atom) else { continue };
                    let overrides = flags
                        .iter()
                        .map(|c| UseOverride {
                            flag: c.flag,
                            enable: c.value,
                        })
                        .collect();
                    combined.push((dep, overrides));
                }
                combined
            };

            // `package_use` is settled from here on (no further rebind below), so
            // this is built once and reused by every remaining filter/size call
            // instead of each one re-listing the same 8 fields.
            let final_policy = repo::ResolvePolicy {
                accept_keywords: &accept_keywords,
                package_mask: &package_mask,
                package_unmask: &package_unmask,
                accept_licenses: &accept_licenses,
                pre_env: &pre_env,
                env_use: &env_use,
                package_use: &package_use,
                force_mask: &force_mask,
            };

            // Autounmask: detect filtered candidates from dropped deps. Filtered
            // to just this round's actionable set and reported once the repair
            // loop settles (see below).
            let autounmask_candidates =
                repo::find_autounmask_candidates(&data, provider.dropped_deps(), &final_policy);

            // Packages that need a same-version rebuild (USE change) must stay in the
            // merge list even though their installed CPV is unchanged — keep them in
            // their topological position rather than appending them after the target.
            let reinstall_cpns: std::collections::HashSet<Cpn> = provider
                .reinstall_deps()
                .iter()
                .map(|r| *r.package.cpn())
                .collect();

            // When a rebuild is forced on an installed package and a newer version is
            // available, favour the upgrade: build the newest version rather than
            // rebuilding the installed one (matching emerge, and required when the
            // installed version has been removed from the tree — it can't be rebuilt).
            let upgrades: HashMap<Cpn, Version> = provider
                .reinstall_deps()
                .iter()
                .filter_map(|r| r.upgrade_to.as_ref().map(|v| (*r.package.cpn(), v.clone())))
                .collect();

            // Every real (non-virtual) package the solver actually selected, before
            // the "already installed, nothing to display" filter below drops entries
            // like `virtual/libcrypt` from the visible plan. Kept around so
            // `bdepend_trim` can still see *their* dependency edges — an
            // already-installed package that's invisible in `order` can still be the
            // sole reason some other, not-yet-installed package (e.g.
            // `sys-libs/libxcrypt`, pulled in only via `virtual/libcrypt`'s RDEPEND)
            // is required; scanning only `order` made such a package look orphaned
            // and wrongly trimmable. See `todo/stage-build-shakeout.md`.
            let full_order: Vec<(PortagePackage, Version)> = provider
                .install_order(&solution)
                .into_iter()
                .filter(|(pkg, _)| !pkg.is_virtual())
                .collect();

            let mut order: Vec<_> = full_order
                .iter()
                .filter(|(pkg, ver)| {
                    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
                    // Drop packages already installed at this version, except:
                    //  - same-version USE rebuilds (reinstall_cpns), and
                    //  - explicitly-requested targets, which emerge reinstalls by
                    //    default ([ebuild R]) even when already at the best version —
                    //    unless the resolve is selective, where an up-to-date target is
                    //    left alone.
                    // "Already installed" is root-specific: a `Host` requirement
                    // (built into `base_roots()`) must only be dropped if it's
                    // installed *there*, never because the unrelated Target sysroot
                    // happens to have a same-named, same-version package.
                    let already_installed = match pkg.merge_root() {
                        MergeRoot::Host => host_installed_cpvs.contains(&cpv),
                        MergeRoot::Target => target_installed_cpvs.contains(&cpv),
                    };
                    // `-N`/`-U` registers USE-drift packages as `InstalledPolicy::Rebuild`
                    // and still selects the installed CPV for a same-version rebuild —
                    // those must stay in the plan ([R]), not be dropped as "already
                    // installed".
                    let use_rebuild = provider
                        .installed_policy(pkg)
                        .is_some_and(|p| matches!(p, InstalledPolicy::Rebuild));
                    !already_installed
                || reinstall_cpns.contains(pkg.cpn())
                || use_rebuild
                // Explicit target: reinstalled even at best version ([R]). Match
                // the resolved target *slot*, not the bare CPN — a sibling slot
                // merely pulled as a satisfied dep (e.g. python:3.13 under a
                // `python` target) must not be re-listed. Set provenance plays
                // no part: `emerge @world` reinstalls its members exactly as it
                // reinstalls a named atom (measured, see
                // todo/selective-resolution.md).
                || (!selective
                    && root_pkgs
                        .iter()
                        .any(|r| r.cpn() == pkg.cpn() && r.slot() == pkg.slot()))
                || emptytree_native
                })
                .cloned()
                .map(|(pkg, ver)| {
                    // Apply the favoured upgrade version if one was recorded.
                    let ver = upgrades.get(pkg.cpn()).cloned().unwrap_or(ver);
                    (pkg, ver)
                })
                .collect();

            // Fallback: any reinstall the solver didn't route through install_order
            // (rare) is appended so it is not silently dropped.
            {
                let in_order: std::collections::HashSet<Cpn> =
                    order.iter().map(|(pkg, _)| *pkg.cpn()).collect();
                let to_reinstall: Vec<(PortagePackage, Version)> = provider
                    .reinstall_deps()
                    .into_iter()
                    .filter(|r| !in_order.contains(r.package.cpn()))
                    .map(|r| {
                        let ver = r.upgrade_to.as_ref().unwrap_or(&r.version).clone();
                        (r.package.clone(), ver)
                    })
                    .collect();
                order.extend(to_reinstall);
            }

            // `-X`/`--exclude` (see `DepgraphOpts::exclude`'s doc): drop matching
            // packages from `order` itself, before any display path (Pretty/JSON/
            // Tree) or the final `PlannedMerge` list is built from it — so every
            // consumer agrees on what will actually happen, not just the merge loop.
            let mut exclude_omitted = 0usize;
            if !exclude_atoms.is_empty() {
                let before = order.len();
                order.retain(|(pkg, ver)| {
                    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
                    let slot = pkg.slot();
                    let slot = slot.as_ref().map(|s| s.as_str());
                    !exclude_atoms.iter().any(|d| d.matches_cpv(&cpv, slot))
                });
                // Printed once after the repair loop settles (an intermediate round's
                // count would otherwise be reported and then superseded).
                exclude_omitted = before.saturating_sub(order.len());
            }

            // `-r` completion progress: same post-solve drop as `--exclude`, so the
            // preview and merge omit work already finished in a prior attempt.
            let mut resume_omitted = 0usize;
            if !resume_completed.is_empty() {
                let before = order.len();
                order.retain(|(pkg, ver)| {
                    let cpv = Cpv::new(*pkg.cpn(), ver.clone()).to_string();
                    !resume_completed.contains(&(pkg.merge_root(), cpv))
                });
                resume_omitted = before.saturating_sub(order.len());
            }

            // Cross-arch host-config stage: pretend output lists target ROOT merges only
            // (emerge -p). A native offset instead keeps the Host build-dep merges (the
            // host-side installs needed to build the target packages), matching emerge.
            if host_config_stage && cross.is_cross_arch() {
                order.retain(|(pkg, _)| pkg.merge_root() == MergeRoot::Target);
            }

            let trim_ctx = bdepend_trim::TrimCtx {
                roots,
                data: &data,
                policy: final_policy,
                root_cpns: &root_cpns,
                reinstall_cpns: &reinstall_cpns,
            };
            if host_config_stage {
                // The trim drops DEPEND already satisfied on the *build* sysroot
                // (ESYSROOT), which is what the build links against. For a from-scratch
                // offset (`--root`, base == target) the shell builds with SYSROOT = ROOT,
                // so DEPEND must be satisfied in the ROOT, not the host config root —
                // `build_sysroot()` is `None` there, which we map to the target so the
                // trim is a no-op (nothing host-satisfied). Only a `--prefix` overlay
                // (base != target) has a distinct build sysroot to trim against.
                order = depend_trim::trim_sysroot_satisfied_depend(
                    order,
                    roots.build_sysroot().or(Some(cross.target.as_path())),
                    cross.target.as_path(),
                    &trim_ctx,
                );
            }

            // `-uD` only: do not post-trim host-satisfied BDEPEND tools (deep update
            // intentionally re-selected them). `-N` alone leaves normal trim — USE-drift
            // packages already in the graph are kept via `use_rebuild` / reinstall_cpns.
            let skip_bdepend_trim = update && deep && !emptytree_native;
            if !emptytree_native && !skip_bdepend_trim {
                // Built packages always carry their BDEPEND now (it's required to build
                // them), so always run the within-run trim to drop entries only needed
                // for BDEPEND already satisfied on BROOT or by an earlier kept entry —
                // matching emerge, which trims a built package's redundant build tools
                // regardless of `--with-bdeps`.
                order = bdepend_trim::trim_within_run_bdepend(order, &full_order, true, &trim_ctx);
            }
            // Native --emptytree lists the full deep closure straight from the solve
            // (the provider returns un-pruned deps under `rebuild_tree`); no post-solve
            // re-list. See todo/em-emptytree.md "AGREED REDESIGN".

            let edges: Vec<_> = provider
                .dependency_graph(&solution)
                .into_iter()
                .filter(|e| !e.from.0.is_virtual() && !e.to.0.is_virtual())
                .collect();

            // Emerge convention: list the explicitly-requested target(s) last.  Only
            // move a target that nothing else depends on (not a `to` in any edge), so
            // the order stays topologically valid for `em -p A B` where one target is a
            // dependency of another.
            {
                let depended_upon: std::collections::HashSet<Cpn> =
                    edges.iter().map(|e| *e.to.0.cpn()).collect();
                let (targets, rest): (Vec<_>, Vec<_>) = order.into_iter().partition(|(pkg, _)| {
                    root_cpns.contains(pkg.cpn()) && !depended_upon.contains(pkg.cpn())
                });
                order = rest;
                order.extend(targets);
            }

            // `--onlydeps`: build only the dependencies of the requested targets, not
            // the targets themselves. Drop them from the install order before the plan
            // is displayed and built, so the table, merge list, and `build_blockers`
            // indices all agree (emerge's `--onlydeps`).
            if onlydeps {
                order.retain(|(pkg, _)| !root_cpns.contains(pkg.cpn()));
            }

            // Slot-operator (`:=`) rebuilds: installed consumers whose VDB-recorded
            // subslot binding is invalidated by a planned dependency are pulled into
            // the plan as same-version rebuilds, placed right after their trigger
            // (emerge's __auto_slot_operator_replace_installed__ set). Both ends carry
            // the `r` (forced rebuild) marker in the output.
            let mut slot_op_cpns: std::collections::HashSet<Cpn> = Default::default();
            if !empty {
                let mut planned_slots: HashMap<Cpn, Vec<(Version, portage_atom::Slot)>> =
                    HashMap::new();
                for (pkg, ver) in &order {
                    if let Some(cache) = repo::find_cache(&data, pkg, ver) {
                        planned_slots
                            .entry(*pkg.cpn())
                            .or_default()
                            .push((ver.clone(), cache.metadata.slot));
                    }
                }
                let in_plan: std::collections::HashSet<Cpn> =
                    order.iter().map(|(pkg, _)| *pkg.cpn()).collect();
                for rb in subslot::find_rebuilds(&target_installed, &planned_slots, &in_plan) {
                    let pos = order
                        .iter()
                        .rposition(|(pkg, _)| rb.triggers.contains(pkg.cpn()))
                        .map_or(order.len(), |i| i + 1);
                    let pkg = match rb.slot.as_deref().filter(|s| !s.is_empty()) {
                        Some(s) => PortagePackage::slotted(rb.cpn, Interned::intern(s)),
                        None => PortagePackage::unslotted(rb.cpn),
                    };
                    order.insert(pos, (pkg, rb.version.clone()));
                    slot_op_cpns.insert(rb.cpn);
                    slot_op_cpns.extend(rb.triggers.iter().copied());
                }
            }

            // Native offset (same-arch `--root`/`--prefix`): schedule host build-copies
            // — a target package's build edges (`DEPEND`/`BDEPEND`/`IDEPEND`) the host
            // lacks are merged to BROOT (`/`) so the target can build against them
            // (emerge lists these `to /` alongside the ROOT runtime copy). Computed as a
            // post-solve walk over the finalized Target plan, not in the solver, to keep
            // the Target solve pristine (the dual-root aliasing balloons it otherwise).
            // `compute` returns the whole reordered plan (a no-op passthrough of
            // `order` for every non-native-offset case, including the common one
            // where nothing needs a host copy at all) — see its own doc comment for
            // why each copy is interleaved in front of its first consumer during the
            // walk, rather than spliced in as a separate, position-blind step.
            let host_copies_adapter = repo::Adapter {
                data: &data,
                accept_keywords: &accept_keywords,
                package_mask: &package_mask,
                package_unmask: &package_unmask,
                accept_licenses: &accept_licenses,
                pre_env: &pre_env,
                env_use: &env_use,
                package_use: &package_use,
                force_mask: &force_mask,
                installed_cpvs: solver_installed_cpvs,
                autosolve_use: false,
            };
            order = host_copies::compute(&order, &host_copies_adapter, roots, &cross);

            // Reverse-dependency constraints: a complete-graph check that emerge's
            // default targeted `-p` skips (e.g. upgrading docutils past an
            // installed package's `<` bound). Computed here (pure, no report yet)
            // so the `--complete-graph` repair loop can decide whether another
            // round is needed before anything is printed or written.
            //
            // Target-routed entries only: `order` also carries BROOT build copies
            // (`host_copies::compute`, above), and those install into the host, not
            // the VDB `target_installed` was read from. Counting one as replacing a
            // target package would hide a real conflict on that name.
            let proposed: Vec<conflicts::ProposedPkg> = order
                .iter()
                .filter(|(pkg, _)| !pkg.is_virtual() && pkg.merge_root() == MergeRoot::Target)
                .map(|(pkg, ver)| conflicts::ProposedPkg {
                    cpn: *pkg.cpn(),
                    slot: pkg.slot(),
                    version: ver.clone(),
                })
                .collect();
            let dep_conflicts = conflicts::find_conflicts(&target_installed, &proposed);

            Ok(RoundOutcome {
                provider,
                solution,
                order,
                edges,
                package_use,
                applied_reqs,
                ceded,
                autounmask_candidates,
                slot_op_cpns,
                dep_conflicts,
                exclude_omitted,
                resume_omitted,
            })
        };

    let mut solve_targets: Vec<(PortagePackage, PortageVersionSet)> = root_deps.clone();
    let mut repaired: HashSet<Cpn> = HashSet::new();
    let mut outcome = solve_round(solve_targets.clone())?;
    let mut repair_completed: Vec<Cpn> = Vec::new();
    let mut repair_incomplete: Vec<Cpn> = Vec::new();
    if complete_graph && update && !empty {
        const MAX_REPAIR_ROUNDS: usize = 3;
        for _ in 0..MAX_REPAIR_ROUNDS {
            // Retained-owner conflicts (`owner_replaced_by.is_none()`) are the
            // ones genuine chain repair can fix: an installed package whose
            // pin the plan just broke, not itself replaced this run. Each one
            // is re-targeted as a root dep by its own cpn/slot so the solver
            // picks a version compatible with the rest of the plan — see
            // todo/slot-chain-completion.md.
            let candidates: Vec<(PortagePackage, PortageVersionSet)> =
                conflicts::retained_owners(&outcome.dep_conflicts)
                    .filter(|c| repaired.insert(c.installed_cpn))
                    .map(|c| {
                        let pkg = match c.slot {
                            Some(s) => PortagePackage::slotted(c.installed_cpn, s),
                            None => PortagePackage::unslotted(c.installed_cpn),
                        };
                        (pkg, PortageVersionSet::any())
                    })
                    .collect();
            if candidates.is_empty() {
                break;
            }
            let mut next_targets = solve_targets.clone();
            next_targets.extend(candidates.iter().cloned());
            match solve_round(next_targets.clone()) {
                Ok(next) => {
                    repair_completed.extend(candidates.iter().map(|(pkg, _)| *pkg.cpn()));
                    solve_targets = next_targets;
                    outcome = next;
                }
                Err(_) => {
                    // Discard this round; keep the last good outcome rather than
                    // turning an advisory chain-completion attempt into a hard
                    // resolution failure.
                    repair_incomplete.extend(candidates.iter().map(|(pkg, _)| *pkg.cpn()));
                    break;
                }
            }
        }
    }

    let RoundOutcome {
        provider,
        solution,
        order,
        edges,
        package_use,
        applied_reqs,
        ceded,
        autounmask_candidates,
        slot_op_cpns,
        dep_conflicts,
        exclude_omitted,
        resume_omitted,
    } = outcome;

    if exclude_omitted > 0 {
        println!(
            ">>> --exclude: omitted {exclude_omitted} package{} from the plan",
            if exclude_omitted == 1 { "" } else { "s" }
        );
    }
    if resume_omitted > 0 {
        println!(
            ">>> resume: {resume_omitted} package{} already completed — omitted from plan",
            if resume_omitted == 1 { "" } else { "s" }
        );
    }

    if verbose >= 3 {
        output::report_dropped_deps(provider.dropped_deps(), &data, arch.as_str());
    }

    // `package_use` is settled from here on (no further rebind below), so this
    // is built once and reused by every remaining filter/size call instead of
    // each one re-listing the same 8 fields.
    let final_policy = repo::ResolvePolicy {
        accept_keywords: &accept_keywords,
        package_mask: &package_mask,
        package_unmask: &package_unmask,
        accept_licenses: &accept_licenses,
        pre_env: &pre_env,
        env_use: &env_use,
        package_use: &package_use,
        force_mask: &force_mask,
    };

    let flag_reqs: HashMap<&PortagePackage, &UseFlagRequirement> = provider
        .use_flag_requirements()
        .iter()
        .map(|r| (&r.package, r))
        .collect();

    let portage_dir = config_root
        .unwrap_or(camino::Utf8Path::new("/"))
        .join("etc/portage");

    // CPNs referenced in the raw dep data of newly-installed packages.
    let solution_cpns: HashSet<Cpn> = solution
        .iter()
        .filter(|(p, _)| !p.is_virtual())
        .map(|(p, _)| *p.cpn())
        .collect();
    let new_needed_cpns: std::collections::HashSet<Cpn> = order
        .iter()
        .filter(|(pkg, _)| !pkg.is_virtual())
        .flat_map(|(pkg, ver)| repo::cpns_for(&data, pkg.cpn(), ver))
        .collect();

    let autounmask_candidates: Vec<_> = autounmask_candidates
        .into_iter()
        .filter(|c| !solution_cpns.contains(&c.cpv.cpn) && new_needed_cpns.contains(&c.cpv.cpn))
        .collect();

    // A required dependency was filtered out of *every* version (keyword / mask
    // / license) and had no `||` alternative, so the solver dropped it and the
    // printed plan is silently incomplete. Surface these unconditionally — like
    // emerge, an unsatisfiable requirement must never be hidden, regardless of
    // `--autounmask`. The flag now only governs *writing* the fix:
    // `--autounmask-write` persists the keyword/mask/license changes.
    // Report in order of severity: mask → keywords → license.
    if !autounmask_candidates.is_empty() {
        autounmask::report(&autounmask_candidates);
        if autounmask_write {
            autounmask::write(&autounmask_candidates, &portage_dir)?;
        }
    }

    // emerge preview semantics: the plan was computed as if the needed USE
    // changes were applied (the co-solve fixpoint), so the changes the user
    // must make are mandatory output — `applied_reqs` (satisfied in the final
    // solve only because they were forced) plus any leftover unapplied demands
    // — judged against the *pristine* configuration. Reported after the merge
    // list (emerge puts caveats at the bottom); like emerge, the run exits
    // non-zero when changes are required.
    let use_change_entries = {
        let mut combined: Vec<_> = applied_reqs;
        combined.extend(provider.use_flag_requirements().to_vec());
        let root_atoms: Vec<String> = atoms.iter().map(|t| t.atom.clone()).collect();
        // What the user asked for, not what it expanded to: `@world` names
        // itself once instead of listing every atom it pulled in.
        let mut root_labels: Vec<String> = Vec::new();
        for t in atoms {
            let label = match &t.origin {
                targets::TargetOrigin::Explicit => t.atom.clone(),
                targets::TargetOrigin::Set(name) => format!("@{name}"),
            };
            if !root_labels.contains(&label) {
                root_labels.push(label);
            }
        }
        let entries = package_use::build_entries(
            &combined,
            &root_atoms,
            &root_labels,
            &edges,
            &pre_env,
            &env_use,
            &pristine_package_use,
        );
        if autounmask_write && !entries.is_empty() {
            package_use::write(&entries, &portage_dir.join("package.use"))?;
        }
        entries
    };

    let _display_adapter = repo::Adapter {
        data: &data,
        accept_keywords: &accept_keywords,
        package_mask: &package_mask,
        package_unmask: &package_unmask,
        accept_licenses: &accept_licenses,
        pre_env: &pre_env,
        env_use: &env_use,
        package_use: &package_use,
        force_mask: &force_mask,
        installed_cpvs: solver_installed_cpvs,
        autosolve_use: false,
    };
    // `order` is already exclude-filtered above, so `plan_entries` (and the
    // Pretty/JSON/Tree preview built from it) inherit the exclusion for free.
    let plan_entries = root_aware::build_plan(order.clone());

    match format {
        DepgraphFormat::Pretty => {
            // Verbose mode shows per-package download size and a total; skip the
            // Manifest/DISTDIR work entirely in plain mode.
            let sizes = if verbose >= 1 {
                download_size::compute(repo_path, &distdir, &data, &order, &final_policy, &ceded)
            } else {
                HashMap::new()
            };
            output::print_pretty_rooted(
                &output::PrettyCtx {
                    data: &data,
                    installed: &installed,
                    installed_entries: &target_installed,
                    pre_env: &pre_env,
                    env_use: &env_use,
                    package_use: &package_use,
                    use_expand: &use_expand,
                    use_expand_hidden: &use_expand_hidden,
                    flag_reqs: &flag_reqs,
                    sizes: &sizes,
                    slot_op_cpns: &slot_op_cpns,
                    verbose,
                    ceded: &ceded,
                    force_mask: &force_mask,
                    accept_keywords: &accept_keywords,
                    binpkg_index,
                },
                &plan_entries,
                &cross,
            )
        }
        DepgraphFormat::Json => output::print_json(&data, &order, &edges, &installed, &flag_reqs)?,
        DepgraphFormat::Tree => {
            let roots: Vec<_> = root_pkgs
                .iter()
                .filter_map(|pkg| {
                    let ver = edges
                        .iter()
                        .find_map(|e| {
                            if &e.from.0 == pkg {
                                Some(e.from.1.clone())
                            } else if &e.to.0 == pkg {
                                Some(e.to.1.clone())
                            } else {
                                None
                            }
                        })
                        .or_else(|| order.iter().find(|(p, _)| p == pkg).map(|(_, v)| v.clone()));
                    ver.map(|v| (pkg.clone(), v))
                })
                .collect();
            output::print_tree(&roots, &edges, &target_installed_cpvs)
        }
    }

    // Advisory warnings are emitted after the plan so the merge list reads
    // first and the caveats follow it (emerge lists issues at the bottom too).
    // These are non-fatal: the plan is still produced.
    //
    //  - reverse-dependency constraints: a complete-graph check that emerge's
    //    default targeted `-p` skips (e.g. upgrading docutils past an installed
    //    package's `<` bound);
    //  - blockers (`!foo` / `!!foo`) and `::repo` constraints, which the solver
    //    does not model;
    //  - REQUIRED_USE, evaluated per-package against its effective USE.
    {
        // `dep_conflicts` was computed per-round above (settled by the
        // `--complete-graph` repair loop, or from the single round when the
        // gate is off) — reporting it here, once, is the only place this
        // advisory prints.
        if !dep_conflicts.is_empty() {
            output::report_conflicts(&dep_conflicts, &use_expand);
        }
        if !repair_completed.is_empty() {
            let names: Vec<String> = repair_completed.iter().map(ToString::to_string).collect();
            println!(
                ">>> --complete-graph: completed the update chain by also pulling in {}",
                names.join(", ")
            );
        }
        if !repair_incomplete.is_empty() {
            let names: Vec<String> = repair_incomplete.iter().map(ToString::to_string).collect();
            println!(
                ">>> --complete-graph: could not extend the chain to include {} \
                 — leaving the plan as computed",
                names.join(", ")
            );
        }

        let mut violations = provider.check_blockers(&solution);
        violations.extend(provider.check_repo_constraints(&solution));
        if !violations.is_empty() {
            output::report_solver_violations(&violations);
        }

        let ru_violations = required_use::find_violations(&data, &order, &final_policy, &ceded);
        if !ru_violations.is_empty() {
            output::report_required_use(&ru_violations);
        }

        // Level-C: report the flags the solver flipped from their configured
        // value to satisfy REQUIRED_USE (they appear set in the plan via the
        // synthetic package.use above; this tells the user what changed).
        let flips: Vec<&portage_atom_pubgrub::CededFlag> =
            ceded.iter().filter(|c| c.flipped).collect();
        if !flips.is_empty() {
            output::report_autosolved_use(&flips, solution.iter(), &data);
        }

        // C5 advisory: a UseDecision is keyed per (cpn, flag), so when several
        // slots of one package are in the plan the same value bound all of them.
        let shared = output::shared_slot_decisions(&ceded, solution.iter());
        if !shared.is_empty() {
            output::report_shared_slot_use_decisions(&shared);
        }

        package_use::report(&use_change_entries);

        // World-family targets nothing acceptable satisfies. Advisory: emerge
        // keeps going and exits 0 for these, so they stay out of `exit_code`.
        if !unsatisfiable.is_empty() {
            output::report_unsatisfiable_targets(&unsatisfiable, &data, multi_repo);
        }
    }

    // The merge plan for the build loop: ebuild paths come from the package's
    // source repo (main or overlay), USE from the same effective fold the
    // displayed plan used.
    let repo_path_of = |cpv: &Cpv| -> camino::Utf8PathBuf {
        let name = repo::repo_name_of(&data, cpv);
        if name == data.repo_name {
            repo_path.to_owned()
        } else {
            overlays
                .iter()
                .find(|(o, _)| o.name() == name)
                .map(|(o, _)| o.path().to_owned())
                .unwrap_or_else(|| repo_path.to_owned())
        }
    };
    let plan: Vec<PlannedMerge> = plan_entries
        .iter()
        .filter(|e| !e.pkg.is_virtual())
        .map(|entry| {
            let pkg = &entry.pkg;
            let ver = &entry.version;
            let cpn = pkg.cpn();
            let cpv = Cpv::new(*cpn, ver.clone());
            let (depend, bdepend, mut flags) = if let Some(cache) =
                repo::find_cache(&data, pkg, ver)
            {
                let stable = accept_keywords.is_stable(&cache.metadata.keywords, &cpv, pkg.slot());
                let effective =
                    effective_use::effective_use(&final_policy, pkg, ver, cache, stable, &ceded);
                (
                    cache.metadata.depend.to_vec(),
                    cache.metadata.bdepend.to_vec(),
                    effective.enabled_flags(),
                )
            } else {
                let mut effective = portage_atom_pubgrub::resolve_effective_use(
                    &HashMap::new(),
                    &pre_env,
                    &cpv,
                    pkg.slot(),
                    &package_use,
                    &env_use,
                );
                // No cache ⇒ no IUSE/keywords; still apply global force/mask
                // and ceded so build USE stays consistent with the solver.
                let empty_iuse = HashSet::new();
                let slot_key = pkg.slot();
                effective_use::apply_force_mask(
                    &mut effective,
                    &force_mask,
                    &cpv,
                    slot_key.as_ref().map(|s| s.as_str()),
                    false,
                    &empty_iuse,
                );
                effective_use::apply_ceded(&mut effective, *cpn, &ceded);
                (Vec::new(), Vec::new(), effective.enabled_flags())
            };
            flags.sort();
            flags.dedup();
            // A cross-derived cpn (`cross-<tuple>/gcc`) has no on-disk tree of
            // its own — `real_cpn_of` redirects the *file* lookup to the real
            // package (`sys-devel/gcc`) it was cloned from, while `cpv`/the
            // displayed plan above still reports the cross cpv (the ebuild's
            // own CPV text, parsed back out of the directory name by
            // `Ebuild::from_path`, must match for VDB/gcc-config routing —
            // see `todo/cross-derive-on-the-fly.md`, "The merge-path
            // decoupling").
            let real_cpn = data.real_cpn_of.get(cpn).copied().unwrap_or(*cpn);
            let real_cpv = Cpv::new(real_cpn, ver.clone());
            let ebuild_path = repo_path_of(&real_cpv)
                .join(real_cpn.category.as_str())
                .join(real_cpn.package.as_str())
                .join(format!("{}-{}.ebuild", real_cpn.package, ver));
            PlannedMerge {
                merge_root: entry.merge_root,
                cpv: cpv.clone(),
                ebuild_path,
                use_flags: flags,
                depend,
                bdepend,
                // Kept in the plan despite being installed ⇒ an intentional
                // reinstall (explicit target / USE rebuild), not a resume-skip.
                // Root-specific for the same reason as the `order` filter
                // above: a `Host` entry must only count as "already
                // installed" against `base_roots()`, never the Target
                // sysroot's unrelated same-named package.
                reinstall: match entry.merge_root {
                    MergeRoot::Host => host_installed_cpvs.contains(&cpv),
                    MergeRoot::Target => target_installed_cpvs.contains(&cpv),
                },
            }
        })
        .collect();

    // Build-order adjacency for `--jobs`: for each plan entry, the indices of
    // *earlier* entries it depends on at build time (DEPEND/BDEPEND). Matching
    // is by CPN (an upgrade may remap the version), restricted to earlier
    // indices so the relation is acyclic — `install_order` already linearised
    // any cycle. A spurious blocker only costs parallelism; a missing one would
    // risk building before a dep is merged, so CPN matching errs on the safe
    // (more-blocking) side.
    let index_of: HashMap<(MergeRoot, Cpn), usize> = plan
        .iter()
        .enumerate()
        .map(|(i, p)| ((p.merge_root, p.cpv.cpn), i))
        .collect();
    let mut build_blockers: Vec<Vec<usize>> = vec![Vec::new(); plan.len()];
    for e in &edges {
        if !matches!(e.class, DepClass::Depend | DepClass::Bdepend) {
            continue;
        }
        let from_key = (e.from.0.merge_root(), *e.from.0.cpn());
        let to_key = (e.to.0.merge_root(), *e.to.0.cpn());
        let (Some(&from), Some(&to)) = (index_of.get(&from_key), index_of.get(&to_key)) else {
            continue;
        };
        if to < from && !build_blockers[from].contains(&to) {
            build_blockers[from].push(to);
        }
    }

    Ok(DepgraphOutcome {
        // Non-zero when the displayed plan needs config changes to be realised:
        // either USE changes (co-solve fixpoint) or unmask/keyword/license
        // changes for a required dep the solver had to drop. Either way the plan
        // as printed is not directly installable — emerge exits non-zero too.
        exit_code: if use_change_entries.is_empty() && autounmask_candidates.is_empty() {
            0
        } else {
            1
        },
        plan,
        build_blockers,
        provided: provided_avail,
    })
}

/// Whether an installed VDB entry must rebuild under `-N`/`-U` for USE/IUSE
/// drift relative to the planned fold for its CPV (or the newest same-slot
/// repo version when the exact CPV left the tree).
fn package_needs_use_reinstall(
    mode: portage_resolve::use_reinstall::UseReinstallMode,
    e: &installed::VdbEntry,
    pkg: &PortagePackage,
    data: &repo::RepoData,
    policy: &repo::ResolvePolicy,
) -> bool {
    use portage_atom_pubgrub::UseFlagState;
    use portage_resolve::use_reinstall::needs_use_reinstall;
    use std::collections::HashSet;

    // Prefer the installed CPV's cache entry; fall back to newest same-slot
    // version when the exact CPV left the tree.
    let (plan_ver, cache) = if let Some(c) = repo::find_cache(data, pkg, &e.version) {
        (e.version.clone(), c)
    } else {
        let Some((cpv, c)) = data.versions.get(&e.cpn).and_then(|vers| {
            vers.iter().rev().find(|(_, entry)| {
                let got = entry.metadata.slot.slot.as_str();
                match e.slot.as_ref().map(|s| s.as_str()) {
                    Some(want) => got == want || got.split('/').next() == Some(want),
                    None => true,
                }
            })
        }) else {
            return false;
        };
        (cpv.version.clone(), c)
    };
    // Stable-keyword decision is approximate here (any stable token); force
    // mask's stable sets rarely change reinstall detection vs the main USE fold.
    let stable = true;
    let cfg = effective_use::effective_use(policy, pkg, &plan_ver, cache, stable, &[]);
    let cur_iuse = effective_use::iuse_set(cache);
    let cur_enabled: HashSet<_> = cur_iuse
        .iter()
        .copied()
        .filter(|f| matches!(cfg.get(*f), UseFlagState::Enabled))
        .collect();
    let cpv = Cpv::new(e.cpn, plan_ver);
    let slot = e.slot.as_ref().map(|s| s.as_str());
    let (forced, masked) = policy.force_mask.effective(&cpv, slot, stable, &cur_iuse);
    let forced: HashSet<_> = forced.into_iter().chain(masked).collect();
    needs_use_reinstall(
        mode,
        &forced,
        &e.active_use,
        &e.iuse,
        &cur_enabled,
        &cur_iuse,
    )
}

/// Whether two versions plausibly belong to the same slot, used to map a
/// `package.provided` CPV onto the repo slot a `:slot` dep would reference.
///
/// Compares the leading numeric components up to the shorter version's length
/// (`3.14.0` vs `3.14.6` → same; `3.14.0` vs `3.15.9999` → different). Slots in
/// the tree are cut from a version prefix (`python` → `3.14`, `gcc` → `14`), so
/// a shared prefix is a good proxy without hard-coding any package's slot rule.
fn same_slot_series(a: &Version, b: &Version) -> bool {
    let n = a.numbers.len().min(b.numbers.len()).min(2);
    n > 0 && a.numbers[..n] == b.numbers[..n]
}
