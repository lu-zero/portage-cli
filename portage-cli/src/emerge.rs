//! Emerge resolve-and-merge orchestration

use std::io::Write;
use std::str::FromStr;

use anyhow::{Context, bail};
use camino::Utf8Path;

use crate::cli;
use crate::error::{self, Result};
use crate::merge::confirm_action;
use crate::merge::run_merge_plan;
use crate::query::depgraph::{TargetAtom, TargetOrigin};
use crate::vdb::open_cli_vdb;
use crate::{binpkg, ebuild, maint, preflight, preserve_libs, query, search};

/// Parse every token as a [`portage_atom::Dep`], failing the whole list on the first
/// invalid atom
///
/// Use this for destructive operations (`-c`/`--depclean`) where dropping a typo would
/// change the meaning of the command (e.g. a targeted depclean silently becoming a
/// full-system clean).
pub(crate) fn parse_atoms_strict(raw: &[String]) -> Result<Vec<portage_atom::Dep>> {
    raw.iter()
        .map(|s| portage_atom::Dep::from_str(s).with_context(|| format!("invalid atom '{s}'")))
        .collect()
}

/// Expand `@set` references in `raw` to concrete atoms, leaving plain atoms untouched
///
/// Sets are a portage-config concept (not PMS); resolution lives in
/// `portage_repo::SetResolver`. The profile stack comes from
/// `<config_root>/etc/portage/make.profile` (for `@system`/`@profile`); user sets,
/// `@world`, and `@selected` are read from `eroot`.
///
/// `@preserved-rebuild` is handled separately, inline below — a
/// VDB/preserve-libs-registry query `SetResolver` has no access to.
///
/// Failures (unknown set, bad profile link) are reported and the offending
/// token dropped, matching `parse_atoms`' tolerance of bad atoms — a typo
/// shouldn't abort the whole run, and `@system` against a host with no profile
/// is a configuration error, not a crash.
///
/// Each expanded atom keeps the name of the set it came from: whether an
/// unsatisfiable target aborts the run or merely warns depends on it (see
/// [`TargetOrigin`]).
/// Expand `@set` references; used by emerge and quickpkg.
pub(crate) fn expand_sets(raw: &[String], roots: &portage_resolve::Roots) -> Vec<TargetAtom> {
    // Build the resolver lazily, only when a set ref is actually present, so a
    // plain `em foo` (no sets) pays no profile-build cost.
    let config_root = roots.config();
    let eroot = roots.merge_root();
    let mut out = Vec::with_capacity(raw.len());
    #[allow(unused_assignments)]
    // stack_holder's initial None may not be read if no sets are expanded
    let mut stack_holder: Option<portage_repo::ProfileStack> = None;
    let mut resolver: Option<portage_repo::SetResolver<'_>> = None;

    for s in raw {
        let Some(name) = portage_repo::set_name(s) else {
            out.push(TargetAtom::explicit(s.clone()));
            continue;
        };

        // `@security` needs the GLSA repo + the configured arch + the VDB —
        // all derivable from `roots` (a system set should reflect actual
        // config, not `--repo`/`--arch` CLI overrides, so it deliberately
        // does NOT go through the `&Cli`-based applet helpers).
        if name == "security" {
            match crate::glsa::security_atoms_from_roots(roots) {
                Ok(atoms) => out.extend(atoms.iter().map(|d| TargetAtom {
                    atom: d.to_string(),
                    origin: TargetOrigin::Set(name.to_string()),
                })),
                Err(e) => crate::style::warn_line!("skipping @{name}: {e}"),
            }
            continue;
        }

        // VDB-aware built-in sets (@preserved-rebuild, …) need the live VDB
        // and/or related registries, none of which `SetResolver` (profile/
        // config-only, in `portage_repo`) has access to — route them through
        // the shared `resolve_vdb_set` instead of the generic resolver below.
        // `None` means "not a VDB-aware name"; fall through to `SetResolver`.
        if let Some(res) = maint::sets::resolve_vdb_set(name, eroot) {
            match res {
                Ok(atoms) => out.extend(atoms.iter().map(|d| TargetAtom {
                    atom: d.to_string(),
                    origin: TargetOrigin::Set(name.to_string()),
                })),
                Err(e) => crate::style::warn_line!("skipping @{name}: {e}"),
            }
            continue;
        }

        if resolver.is_none() {
            let portage_dir = config_root
                .unwrap_or(Utf8Path::new("/"))
                .join("etc/portage");
            let profile_link = portage_dir.join("make.profile");
            match std::fs::canonicalize(profile_link.as_std_path())
                .map_err(|e| anyhow::anyhow!("cannot resolve make.profile for @set expansion: {e}"))
                .and_then(|p| {
                    portage_repo::ProfileStack::build(p)
                        .map_err(|e| anyhow::anyhow!("failed to build profile stack: {e}"))
                })
                .and_then(|st| {
                    st.with_user_profile(portage_dir.join("profile").into_std_path_buf())
                        .map_err(|e| {
                            anyhow::anyhow!("failed to append site-local user profile: {e}")
                        })
                }) {
                Ok(st) => {
                    // get_or_insert (not `stack_holder = Some(st); ...unwrap()`) hands
                    // back the `&ProfileStack` directly, so there's nothing to unwrap.
                    let stack = stack_holder.get_or_insert(st);
                    resolver = Some(portage_repo::SetResolver::new(stack, eroot));
                }
                Err(e) => {
                    crate::style::warn_line!("cannot expand @{name}: {e}");
                    // Cannot expand any sets if resolver creation failed; push raw string
                    out.push(TargetAtom::explicit(s.clone()));
                    continue;
                }
            }
        }

        // If we have a resolver, use it; otherwise skip (resolver creation failed earlier)
        if let Some(res) = resolver.as_ref() {
            match res.resolve(name) {
                Ok(atoms) => out.extend(atoms.iter().map(|d| TargetAtom {
                    atom: d.to_string(),
                    origin: TargetOrigin::Set(name.to_string()),
                })),
                Err(e) => crate::style::warn_line!("skipping @{name}: {e}"),
            }
        } else {
            // Resolver creation failed for earlier set; push raw string
            out.push(TargetAtom::explicit(s.clone()));
        }
    }
    out
}
pub(crate) struct EmergeOpts<'a> {
    /// USE tokens forced for resolve + build (emerge syntax: `headers-only`, `-cxx`)
    ///
    /// Applied as a transient *conf-layer* override (`DepgraphOpts::extra_use_override`,
    /// catalyst's `CATALYST_USE` layer), not the process environment — env sits above
    /// `package.use` and would wipe package.use / break `--autosolve-use` under
    /// `USE="-* build"`.
    pub use_override: &'a [String],
    /// `--nodeps`: merge only the named atoms, no dependency expansion
    pub nodeps: bool,
    /// Override depgraph flags for this call; `None` → `cli.depgraph_flags`
    pub depgraph_flags: Option<crate::cli::DepgraphFlags>,
    /// Override merge flags for this call; `None` → `cli.merge_flags`
    ///
    /// The staged driver merges subcommand + top-level flags so either position works
    /// (`em -j 80 stages …` vs `em stages … -j 80`).
    pub merge_flags: Option<crate::cli::MergeFlags>,
    /// Install into the plain outer EROOT, ignoring `--target` sysroot substitution
    ///
    /// Used for host-side `cross-*` toolchain steps.
    pub use_outer_eroot: bool,
    /// Use only the target VDB as the installed view (no host-base sharing)
    /// Native toolchain bootstrap into an empty `--root`.
    pub target_only_installed_view: bool,
    /// Update world on a successful real user merge (emerge `_world_atom`)
    /// Staged/internal steps leave this false.
    pub update_world: bool,
    /// Replaying `-r`/`--resume` — resume-state save must not rotate backup
    /// Only [`resume_atoms`] sets this true.
    pub is_resume: bool,
    /// Optional activity bus; `None` → default live-FS sink for real merges
    pub activity: Option<crate::activity::ActivityBus>,
    /// Session correlation (outer job_id / parent_job_id for staged plans)
    pub activity_session: crate::activity::ActivitySessionOpts,
    /// In-memory crossdev aliases for this resolve
    ///
    /// Empty for normal emerges; staged crossdev `-p` passes the planned alias here.
    pub extra_aliases: &'a [portage_repo::RepoEntry],
    /// Directories ahead of the sanitised build `PATH`
    ///
    /// Empty for every merge but `em setup --local`'s own — see `setup::host_tools`.
    pub extra_path: &'a [camino::Utf8PathBuf],
}

/// [`EmergeOpts`] minus its borrowed `use_override` (already folded into
/// `extra_use_override` by the time [`emerge_atoms_inner`] needs it) — keeps
/// that function to a single options argument instead of one parameter per
/// field.
struct ResolvedEmergeOpts<'a> {
    nodeps: bool,
    depgraph_flags: Option<crate::cli::DepgraphFlags>,
    merge_flags: Option<crate::cli::MergeFlags>,
    use_outer_eroot: bool,
    target_only_installed_view: bool,
    extra_use_override: Option<String>,
    update_world: bool,
    is_resume: bool,
    activity: Option<crate::activity::ActivityBus>,
    activity_session: crate::activity::ActivitySessionOpts,
    extra_aliases: &'a [portage_repo::RepoEntry],
    extra_path: &'a [camino::Utf8PathBuf],
}

pub(crate) async fn emerge_atoms(
    cli: &cli::Cli,
    raw_atoms: &[String],
    opts: EmergeOpts<'_>,
) -> Result<()> {
    // A conf-layer `USE=` assignment for this step, sourced at the same
    // position as a real make.conf (`resolve_use_flags`'s `extra_use_override`)
    // — not a process-env mutation, so it correctly sits *below*
    // `package.use` (layer 5) instead of wiping it.
    let extra_use_override = (!opts.use_override.is_empty())
        .then(|| format!("USE=\"{}\"\n", opts.use_override.join(" ")));
    emerge_atoms_inner(
        cli,
        raw_atoms,
        ResolvedEmergeOpts {
            nodeps: opts.nodeps,
            depgraph_flags: opts.depgraph_flags,
            merge_flags: opts.merge_flags,
            use_outer_eroot: opts.use_outer_eroot,
            target_only_installed_view: opts.target_only_installed_view,
            extra_use_override,
            update_world: opts.update_world,
            is_resume: opts.is_resume,
            activity: opts.activity,
            activity_session: opts.activity_session,
            extra_aliases: opts.extra_aliases,
            extra_path: opts.extra_path,
        },
    )
    .await
}

/// `--eta` estimate for `outcome.plan`, formatted for terminal display
/// Shared by the `-p`/`--pretend` preview and the `-a`/`--ask` confirmation
/// prompt, so "am I about to start a 25-minute build" is visible at the
/// point the user is actually asked to confirm, not only under a dry-run
/// preview.
fn eta_message(
    roots: &portage_resolve::Roots,
    merge_flags: &cli::MergeFlags,
    outcome: &query::depgraph::DepgraphOutcome,
) -> String {
    let store = crate::activity::DurationStore::load(roots.merge_root());
    let pkgs: Vec<_> = outcome
        .plan
        .iter()
        .map(|p| crate::activity::EtaPkg {
            cpn: p.cpv.cpn.to_string(),
            cpv: p.cpv.to_string(),
        })
        .collect();
    let jobs = merge_flags.jobs.unwrap_or(1);
    // Prefer critical-path list-schedule when the depgraph gave us
    // build_blockers (same graph as --jobs parallel merge).
    let eta = crate::activity::estimate_remaining_with_blockers(
        &store,
        &pkgs,
        &outcome.build_blockers,
        jobs,
        15,
    );
    crate::activity::format_eta(&eta)
}

/// Resolve and (unless `--pretend`) merge `raw_atoms` with the global
/// config in `cli`, plus the per-call [`EmergeOpts`]. Factored out of
/// [`run_emerge`] so the crossdev staged-build driver can run each
/// toolchain step through the very same path.
async fn emerge_atoms_inner(
    cli: &cli::Cli,
    raw_atoms: &[String],
    opts: ResolvedEmergeOpts<'_>,
) -> Result<()> {
    let ResolvedEmergeOpts {
        nodeps,
        depgraph_flags: depgraph_flags_override,
        merge_flags: merge_flags_override,
        use_outer_eroot,
        target_only_installed_view,
        extra_use_override,
        update_world,
        is_resume,
        activity: activity_override,
        activity_session,
        extra_aliases,
        extra_path,
    } = opts;
    let extra_use_override = extra_use_override.as_deref();
    let merge_flags = merge_flags_override.as_ref().unwrap_or(&cli.merge_flags);
    let resolved = cli.repo_path();
    let repo_path = camino::Utf8Path::new(&resolved);
    if !repo_path.is_dir() {
        bail!("repo not found at {resolved}");
    }
    let repo = crate::repo_open::open(repo_path.as_std_path())?;
    let vdb = open_cli_vdb(cli).ok();
    // `-a` takes precedence over `-u`: explicitly asking to be asked beats a
    // silent auto-pick. Neither still hard-errors on ambiguity (a mutating
    // command shouldn't silently guess), but that error now names the
    // installed candidate and suggests `-u` — real emerge just dumps the
    // candidate list with no such hint, regardless of any flag. See
    // docs/design/architecture.md's "Ambiguity and partial-failure policy" section.
    let mode = if merge_flags.ask {
        query::ResolveMode::Ask
    } else if merge_flags.update {
        query::ResolveMode::PreferInstalled
    } else {
        query::ResolveMode::Error
    };
    // Root model (docs/user/root-model.md): config from roots.config, installed
    // view = VDB(base) ∪ VDB(target), merge into target. `use_outer_eroot`
    // skips `--target` sysroot substitution for host-side `cross-*` steps.
    // Use `outer_roots()`, not `base_roots()`: the latter is BROOT (host `/`
    // under `--prefix`), not the outer EPREFIX those packages install into.
    let roots = if use_outer_eroot {
        cli.outer_roots()
    } else {
        cli.roots()
    };
    let roots = if target_only_installed_view {
        roots.with_target_only_installed_view()
    } else {
        roots
    };
    // `Cli::host_roots()` (not `roots`): stays overlay-aware under `--target`
    // substitution, so a `MergeRoot::Host` entry's `-p` display matches its
    // real merge destination even when `roots` has had its own overlay-ness
    // cleared by the sysroot substitution. See `DepgraphOpts::host_merge_root`.
    let host_roots = cli.host_roots();
    // `--target <tuple>` targets `<EROOT>/usr/<tuple>`; fail early with a setup
    // hint if that sysroot has not been laid down by `em crossdev --init-target`
    // (otherwise the profile/make.conf read fails with an opaque ENOENT). Skipped
    // for `use_outer_eroot`: those steps target the outer EROOT on purpose, not
    // the sysroot this check is guarding.
    if let Some(tuple) = cli.target.as_deref().filter(|_| !use_outer_eroot) {
        let cfg = roots.config().unwrap_or_else(|| camino::Utf8Path::new("/"));
        if !cfg.join("etc/portage/make.conf").exists() {
            bail!(
                "cross target '{tuple}' is not set up at {cfg}\n  \
                 run: em crossdev -t {tuple} --init-target"
            );
        }
    }
    // Expand @set references (e.g. @system, @world) to concrete atoms before
    // resolution. Sets are read from the config root's profile (@system) and
    // the merge target (@world/@selected, user sets).
    let expanded = expand_sets(raw_atoms, &roots);
    // A set-only request (e.g. `@preserved-rebuild`, `@world` on an
    // up-to-date system) legitimately expanding to nothing is not a typo —
    // unlike a bad literal atom, which always yields at least one
    // `TargetAtom::explicit` entry from `expand_sets` even when invalid (it
    // only ever drops *set* references). Report it the same way real emerge
    // does for an empty selection, rather than falling into the generic
    // "no valid atoms" error below.
    if expanded.is_empty()
        && !raw_atoms.is_empty()
        && raw_atoms.iter().all(|s| portage_repo::is_set_ref(s))
    {
        println!(">>> Nothing to merge; quitting.");
        return Ok(());
    }
    // So bare-name atoms (not just `cat/pkg`) can resolve to an overlay-only
    // package, not just the main repo — see `query::resolve_atom`'s doc.
    let mut set = crate::repo_open::repo_set_from_conf(repo, &roots, cli.repo.is_none());
    // Caller-supplied aliases first so a pretend crossdev plan can inject the
    // target about to be written; on-disk entries with the same name still
    // apply (load_repos skips already-seen CPVs). Prepended here (not inside
    // `depgraph()`) so the same set is shared with `resolve_atom` below and
    // the solver — one build, one repo world for the whole invocation.
    set.prepend_aliases(extra_aliases);
    // Resolved one at a time (not via `resolve_atoms`) so each atom keeps the
    // provenance `expand_sets` gave it; unresolvable ones are warned about and
    // dropped, exactly as `resolve_atoms` does.
    let atoms: Vec<TargetAtom> = expanded
        .iter()
        .filter_map(
            |t| match query::resolve_atom(&set, vdb.as_ref(), mode, &t.atom) {
                Ok(dep) => Some(TargetAtom {
                    atom: dep.to_string(),
                    origin: t.origin.clone(),
                }),
                Err(e) => {
                    crate::style::warn_line!("{e}");
                    None
                }
            },
        )
        .collect();
    if atoms.is_empty() {
        // No extra "!!! no valid atoms" line: each atom that failed already
        // printed its own warning above (unresolvable, ambiguous + "-u"
        // hint, ...) — a generic follow-up line adds nothing and reads as a
        // second, unrelated failure. `NoValidAtoms` just drives the exit
        // code; main.rs recognises it and stays quiet.
        return Err(error::NoValidAtoms.into());
    }

    // World selection (real emerge's `_world_atom`): only the genuine
    // top-level invocation (`update_world`), and only the literal,
    // explicitly-named atoms go to `world` — a `@set` ref's *members* never
    // do (they're recorded as the set reference itself, in `world_sets`,
    // via `world_set_refs` below).
    //
    // Computed once, under the *display* gate — real emerge's
    // `check_system_world` suppresses "would be added to world" bolding for
    // `--oneshot` alone, so a `-p` preview still bolds what a real run would
    // record (see `DepgraphOpts::world_additions`). `select_world_atoms`
    // warns on unparsable atoms, so it must not run twice.
    let world_additions: Vec<portage_atom::Dep> = if update_world && !merge_flags.oneshot {
        select_world_atoms(&atoms)
    } else {
        Vec::new()
    };
    // What actually gets written to the world file after a successful merge
    // is narrower still: skipped for the same extra flag set real portage
    // skips the write for, `--pretend` included. `world_set_refs` shares
    // this same narrower gate directly (no display half): a `@set` ref never
    // appears as a plan row of its own, so there is nothing for it to bold.
    let world_atoms: Vec<portage_atom::Dep> = if !cli.pretend
        && !merge_flags.buildpkgonly
        && !merge_flags.fetchonly
        && !merge_flags.onlydeps
    {
        world_additions.clone()
    } else {
        Vec::new()
    };
    let world_set_refs: Vec<String> = if update_world
        && !merge_flags.oneshot
        && !cli.pretend
        && !merge_flags.buildpkgonly
        && !merge_flags.fetchonly
        && !merge_flags.onlydeps
    {
        select_world_set_refs(&atoms)
    } else {
        Vec::new()
    };
    let format = if merge_flags.json {
        cli::DepgraphFormat::Json
    } else if merge_flags.tree {
        cli::DepgraphFormat::Tree
    } else {
        cli::DepgraphFormat::Pretty
    };
    let depgraph_flags = depgraph_flags_override
        .as_ref()
        .map(|f| (f.deep, f.newuse, f.changed_use))
        .unwrap_or((
            cli.depgraph_flags.deep,
            cli.depgraph_flags.newuse,
            cli.depgraph_flags.changed_use,
        ));
    let binpkg_index = binpkg::open_local_index_for_preview(cli, merge_flags).await;
    let outcome = query::depgraph::depgraph(query::depgraph::DepgraphOpts {
        set,
        atoms: &atoms,
        world_additions: &world_additions,
        arch: &cli.arch,
        format,
        verbose: cli.verbose,
        empty: merge_flags.emptytree,
        autounmask_write: merge_flags.autounmask_write,
        // `--pretend` pops `--ask` in real portage; matched here by simply
        // never treating it as interactive under `-p` — a `-pa` preview must
        // never prompt.
        ask: merge_flags.ask && !cli.pretend,
        autosolve_use: merge_flags.autosolve_use,
        roots: &roots,
        host_merge_root: host_roots.merge_root(),
        onlydeps: merge_flags.onlydeps,
        with_bdeps: merge_flags.with_bdeps,
        root_deps_rdeps: merge_flags.root_deps,
        deep: depgraph_flags.0,
        update: merge_flags.update,
        newuse: depgraph_flags.1,
        changed_use: depgraph_flags.2,
        noreplace: merge_flags.noreplace,
        nodeps,
        extra_use_override,
        binpkg_index: binpkg_index.as_ref(),
        exclude: &merge_flags.exclude,
        // On `-r`, drop packages already finished in a prior attempt so the
        // `-p` preview and the merge plan agree (critical under `--emptytree`,
        // where VDB presence alone is not a completion marker).
        resume_completed: if is_resume {
            maint::resume::completed_keys(roots.merge_root())
        } else {
            std::collections::HashSet::new()
        },
        complete_graph: merge_flags.complete_graph,
    })
    .await?;

    // Shared by the `-p` preview below and the `-a` confirm prompt further
    // down — same "bind, write, flush" shape `activity/human.rs` uses for
    // every styled stdout write, so ANSI codes still strip cleanly on
    // non-tty output.
    let print_eta = || {
        let mut out = anstream::stdout();
        let _ = write!(out, "{}", eta_message(&roots, merge_flags, &outcome));
        let _ = out.flush();
    };

    // Shown here (before the `ConfigChangesNeeded` bail-out below), not only
    // once the plan is fully clean: the plan preview itself (`Total:`/`Size
    // of downloads:`) is already printed by `depgraph()` above even when USE
    // changes are required, so a `--pretend` run with `--eta` on a plan that
    // needs config changes must still show it — previously it silently never
    // printed in that case, since the old call site was below this
    // early-return.
    if cli.pretend && merge_flags.eta && !outcome.plan.is_empty() {
        print_eta();
    }

    // Non-zero resolver exit: printed plan is not installable (USE/mask/license
    // or a PMS 8.3.2 hard blocker). The block was already printed; quiet exit 1.
    // Checked before `--pretend` so `-p`/`-a` match a real run.
    if outcome.exit_code != 0 {
        return Err(error::ConfigChangesNeeded.into());
    }

    if outcome.plan.is_empty() {
        // Nothing needed building (already installed/up to date), but the
        // explicit atoms are still a real selection — matches real emerge
        // adding an already-satisfied `emerge foo` to world. A resume whose
        // remaining work is empty (everything completed last time) must also
        // clear the saved job so the next `-r` doesn't loop on an empty plan.
        if is_resume && update_world {
            maint::resume::clear(roots.merge_root());
        }
        maint::world::add_atoms(Some(roots.merge_root()), &world_atoms);
        maint::world::add_set_refs(Some(roots.merge_root()), &world_set_refs);
        return Ok(());
    }

    // Pre-flight: fail fast with a clear message if any plan entry's build
    // dependencies won't be present when it builds, rather than mid-build.
    // Run before the `--pretend` return, so `-p`/`-a` surface whether the
    // plan is preflight-clean, same as the merge plan itself under `-p` —
    // a preview could otherwise never reveal a plan that would fail
    // preflight during a real run.
    //
    // Skipped under `--nodeps` regardless of `--pretend`: that flag already
    // means "no dependency expansion or verification" (matching emerge's
    // own `--nodeps`) — the guard-rail would otherwise still block on real
    // BDEPEND that `--nodeps` opted out of (a genuine bootstrap cycle with
    // no valid dependency order that must be seeded out of order somewhere).
    if !nodeps {
        preflight::check(
            &outcome.plan,
            &roots,
            &outcome.provided,
            &outcome.hard_cycle_edges,
        )?;
    }

    if cli.pretend {
        return Ok(());
    }

    // PMS 8.3.2: a strong block must not be ignored. `-p` already printed
    // `>>> would unmerge:` and exited 0 (emerge parity). Step 2 auto-unmerge
    // is not implemented, so a real merge refuses rather than ignore the block.
    if outcome.strong_unmerge_pending {
        return Err(error::BlockerUnmergeRequired.into());
    }

    // --prefix/--local relocates distfiles and work trees under the outer
    // prefix (eprefix), not under a --target sysroot — see Roots::relocate_root.
    let relocate = roots.relocate_root();
    let distdir = relocate.map(|p| p.join("var/cache/distfiles"));
    let work_base = ebuild::default_work_base(relocate);

    // Ask about what will actually happen (`-f`/`-B` don't install).
    let verb = if merge_flags.fetchonly {
        "fetch"
    } else if merge_flags.buildpkgonly {
        "build"
    } else {
        "merge"
    };
    if merge_flags.ask {
        if merge_flags.eta {
            print_eta();
        }
        if !confirm_action(verb, outcome.plan.len())? {
            println!(">>> Quitting.");
            return Ok(());
        }
    }

    // Persist enough to replay this invocation via `-r`/`--resume` if it
    // gets interrupted or fails — only for the genuine top-level user
    // selection (`update_world`, same gate the world-file write below
    // uses), never for an internal staged-build step. Saved *after* the
    // pretend/ask early-returns above, so a declined or previewed run
    // never touches this. See `maint::resume`'s module doc for why this
    // persists the invocation (atoms + flags) rather than a pinned package
    // list the way real portage's own `mtimedb["resume"]` does.
    //
    // `job_id` names the on-disk marker tree for per-package completion
    // (independent files under `em-resume.done/<job_id>/…` — not a locked
    // JSON rewrite on every package).
    let resume_job_id = if update_world {
        Some(maint::resume::save(
            roots.merge_root(),
            maint::resume::ResumeState::new(
                raw_atoms.to_vec(),
                merge_flags.clone(),
                cli::DepgraphFlags {
                    deep: depgraph_flags.0,
                    newuse: depgraph_flags.1,
                    changed_use: depgraph_flags.2,
                },
                nodeps,
            ),
            is_resume,
        )?)
    } else {
        None
    };

    // Activity bus: caller-supplied or default live-FS + history under this root.
    let activity =
        activity_override.unwrap_or_else(|| crate::activity::default_cli_bus(roots.merge_root()));
    // Terminal banners render from the bus (one verbosity decision point); the
    // `quiet` here is the user's `-q`, NOT the jobs-derived phase-log quiet —
    // `-j>1` keeps banners while still sending build output to build.log.
    crate::activity::attach_human_stdout(&activity, cli.quiet, cli.verbose);
    let activity_args = cli.effective_activity();
    crate::activity::attach_jsonl_outputs(
        &activity,
        activity_args.activity_fd,
        activity_args.activity_jsonl.as_deref(),
    )?;
    if activity_args.emergelog {
        crate::activity::attach_emergelog(&activity, roots.merge_root());
    }
    // Prefer resume job_id so markers and activity share one correlation key.
    let job_id = crate::activity::resolve_job_id(&activity_session, resume_job_id.as_deref());
    // Surface tracing diagnostics (warn/error/info from libraries) onto this
    // session's bus as ActivityEvent::Diagnostic for the duration of the merge.
    crate::activity::set_session(activity.clone(), &job_id);
    let parent_job_id = activity_session.parent_job_id.clone();
    let live_root = roots.merge_root().to_owned();
    let session_started = crate::activity::ActivityEvent::now();
    let mode = if merge_flags.fetchonly || merge_flags.fetch_all_uri {
        crate::activity::ActivityMode::FetchOnly
    } else if merge_flags.buildpkgonly {
        crate::activity::ActivityMode::BuildpkgOnly
    } else {
        crate::activity::ActivityMode::Merge
    };
    let mut argv: Vec<String> = std::env::args().collect();
    if argv.is_empty() {
        argv.push("em".into());
    }
    let activity_plan: Vec<crate::activity::ActivityPlanPkg> = outcome
        .plan
        .iter()
        .map(|p| crate::activity::ActivityPlanPkg {
            cpn: p.cpv.cpn.to_string(),
            cpv: p.cpv.to_string(),
            merge_root: p.merge_root.into(),
        })
        .collect();
    activity.emit(crate::activity::ActivityEvent::SessionStart {
        v: crate::activity::ACTIVITY_EVENT_VERSION,
        job_id: job_id.clone(),
        parent_job_id: parent_job_id.clone(),
        pid: std::process::id(),
        started_at: session_started,
        argv,
        merge_root: live_root.to_string(),
        host_root: cli.host_roots().merge_root().to_string(),
        mode,
        plan_total: outcome.plan.len() as u32,
        flags: crate::activity::SessionFlags {
            jobs: merge_flags.jobs,
            emptytree: merge_flags.emptytree,
            update: merge_flags.update,
            deep: depgraph_flags.0,
            keep_going: merge_flags.keep_going,
            fetchonly: merge_flags.fetchonly || merge_flags.fetch_all_uri,
            buildpkgonly: merge_flags.buildpkgonly,
        },
        plan: activity_plan,
        blockers: outcome.build_blockers.clone(),
    });

    let merge_result = run_merge_plan(crate::merge::MergePlanRequest {
        plan: &outcome.plan,
        blockers: &outcome.build_blockers,
        roots: &roots,
        work_base: &work_base,
        distdir: distdir.as_deref(),
        merge_flags,
        globals: cli,
        extra_path,
        resume_job: resume_job_id.as_deref().map(|id| crate::merge::ResumeJob {
            root: roots.merge_root(),
            job_id: id,
        }),
        activity: Some(crate::merge::ActivityTrack {
            bus: activity.clone(),
            job_id: job_id.clone(),
            parent_job_id: parent_job_id.clone(),
            live_root: live_root.clone(),
        }),
    })
    .await;

    let ok = merge_result.is_ok();
    // SessionEnd counters: best-effort zeros here; merge loop emits per-pkg.
    // Live projection/disk already tracked completed/failed via PkgEnd.
    activity.emit(crate::activity::ActivityEvent::SessionEnd {
        v: crate::activity::ACTIVITY_EVENT_VERSION,
        job_id,
        parent_job_id,
        at: crate::activity::ActivityEvent::now(),
        ok,
        completed: 0,
        failed: 0,
        seconds: crate::activity::ActivityEvent::now() - session_started,
    });
    // Stop mirroring tracing diagnostics onto this (now-ending) session.
    crate::activity::clear_session();

    merge_result?;

    if update_world {
        maint::resume::clear(roots.merge_root());
    }
    maint::world::add_atoms(Some(roots.merge_root()), &world_atoms);
    maint::world::add_set_refs(Some(roots.merge_root()), &world_set_refs);
    Ok(())
}

/// Run the default emerge path for a parsed CLI invocation
pub(crate) async fn run_emerge(cli: &cli::Cli) -> Result<()> {
    // emerge -r/--resume: replaces the whole action, same precedence real
    // emerge gives it (checked first, ahead of every other action flag).
    if cli.resume {
        return resume_atoms(cli).await;
    }
    // emerge -C: remove the matching installed packages directly, no
    // dependency graph at all. Checked first: -C together with -s/-S makes
    // no sense, and real emerge treats -C as its own action too.
    if cli.unmerge {
        return unmerge_atoms(cli, &cli.atoms).await;
    }
    // emerge --depclean / -c: the safe alternative to -C, walking the
    // installed dependency graph first.
    if cli.depclean {
        return crate::depclean::run(cli).await;
    }
    // emerge -P/--prune: like -C, but only the non-highest-version matches.
    if cli.prune {
        return prune_atoms(cli, &cli.atoms).await;
    }
    // emerge -W/--deselect: world-file-only, no removal at all.
    if cli.deselect {
        return deselect_atoms(cli, &cli.atoms);
    }
    // emerge -s / -S: the arguments are search patterns, not atoms.
    if cli.search || cli.searchdesc {
        return search::run_emerge_style(&cli.search_repos(), &cli.atoms, cli.searchdesc).await;
    }
    emerge_atoms(
        cli,
        &cli.atoms,
        EmergeOpts {
            use_override: &[],
            nodeps: cli.nodeps,
            depgraph_flags: None,
            merge_flags: None,
            use_outer_eroot: false,
            target_only_installed_view: false,
            update_world: true,
            is_resume: false,
            activity: None,
            activity_session: Default::default(),
            extra_aliases: &[],
            extra_path: &[],
        },
    )
    .await
}

/// `-r`/`--resume`: replay the last saved merge (`maint::resume`)
///
/// Atoms are not accepted alongside this flag — the package list comes from the saved
/// state, matching real emerge (`--resume`'s favorites/mergelist come only from `mtimedb`,
/// never the command line).
///
/// Flag overlay is [`maint::resume::merge_resume_flags`] (not the
/// subcommand-vs-global OR helper): job-shape flags start from the saved
/// job and the current CLI can only *add* bools (e.g. `-r --keep-going`);
/// ephemeral UI (`-a`/`--tree`/`--json`) comes only from this invocation;
/// `-X` unions into the saved exclude list. See that function's doc for
/// why clap cannot express "turn a saved flag off" on `-r`.
async fn resume_atoms(cli: &cli::Cli) -> Result<()> {
    if !cli.atoms.is_empty() {
        bail!("-r/--resume replays the last saved merge; atoms are not accepted together with it");
    }

    let roots = cli.roots();
    let Some(state) = maint::resume::take_for_resume(roots.merge_root())? else {
        bail!("-r/--resume: nothing to resume");
    };

    let merge_flags = maint::resume::merge_resume_flags(&state.merge_flags, &cli.merge_flags);
    let depgraph_flags =
        crate::crossdev::merge_depgraph_flags_fields(&state.depgraph_flags, &cli.depgraph_flags);
    let nodeps = state.nodeps || cli.nodeps;

    emerge_atoms(
        cli,
        &state.atoms,
        EmergeOpts {
            use_override: &[],
            nodeps,
            depgraph_flags: Some(depgraph_flags),
            merge_flags: Some(merge_flags),
            use_outer_eroot: false,
            target_only_installed_view: false,
            update_world: true,
            is_resume: true,
            activity: None,
            activity_session: crate::activity::ActivitySessionOpts {
                job_id: Some(state.job_id.clone()).filter(|s| !s.is_empty()),
                parent_job_id: None,
            },
            extra_aliases: &[],
            extra_path: &[],
        },
    )
    .await
}

/// Match `atoms` against `vdb`, deduping by Cpv identity (the same installed
/// package can match two atoms given on the command line, e.g. "foo" and
/// "cat/foo" — Hash + Eq already, preserving natural match order instead of
/// scrambling it with a sort+dedup by Display). `label` prefixes the "no
/// atom matched anything" error (e.g. "-C/--unmerge", "-P/--prune").
///
/// Shared by both: their match set differs downstream (`-C` keeps every
/// match, `-P` drops each Cpn's highest), but "find installed packages for
/// these atoms, report unmatched ones, bail if nothing matched" is identical.
fn match_installed_atoms(
    vdb: &portage_vdb::Vdb,
    atoms: &[String],
    label: &str,
) -> Result<Vec<portage_vdb::InstalledPackage>> {
    if atoms.is_empty() {
        bail!("{label} needs at least one atom");
    }

    let mut seen = std::collections::HashSet::new();
    let mut matched: Vec<portage_vdb::InstalledPackage> = Vec::new();
    let mut unmatched: Vec<&str> = Vec::new();
    for raw in atoms {
        let pkgs = crate::vdb::find_packages(vdb, raw);
        if pkgs.is_empty() {
            crate::style::warn_line!("no installed package matches '{raw}'");
            unmatched.push(raw.as_str());
            continue;
        }
        for pkg in pkgs {
            if seen.insert(pkg.cpv().clone()) {
                matched.push(pkg);
            }
        }
    }

    if matched.is_empty() {
        bail!(
            "{label}: no installed package matched ({})",
            unmatched.join(", ")
        );
    }
    Ok(matched)
}

/// `-C`/`--unmerge`: remove the installed packages matching `atoms`
/// directly, without any dependency graph at all — matches real emerge's
/// `-C` semantics (a dangerous removal with zero dependency checking;
/// `depclean` is the safe alternative). Every installed slot/version
/// matching any given atom is removed; there is no plan to preview beyond
/// the match list, so `--pretend` prints it and `--ask` confirms against it.
async fn unmerge_atoms(cli: &cli::Cli, atoms: &[String]) -> Result<()> {
    let vdb = open_cli_vdb(cli)?;
    let matched = match_installed_atoms(&vdb, atoms, "-C/--unmerge")?;
    remove_matched_packages(cli, vdb, matched, "unmerge", "unmerged").await
}

/// `-P`/`--prune`: like `-C`, except each matched package's *highest*
/// installed version is always kept — only the older versions among the
/// matched set are removal candidates. No dependency graph at all, same
/// caveat as `-C` (real emerge recommends `--depclean` for a
/// dependency-aware clean instead). Requires at least one atom, same as
/// `-C` — there is no "prune everything" bare form.
async fn prune_atoms(cli: &cli::Cli, atoms: &[String]) -> Result<()> {
    let vdb = open_cli_vdb(cli)?;
    let candidates = match_installed_atoms(&vdb, atoms, "-P/--prune")?;
    let matched = drop_highest_version_per_cpn(candidates);

    if matched.is_empty() {
        println!(">>> Nothing to prune (only one installed version per matched package).");
        return Ok(());
    }

    remove_matched_packages(cli, vdb, matched, "prune", "pruned").await
}

/// From `candidates` (already matched installed packages, possibly several
/// versions per `Cpn`), drop each `Cpn`'s single highest version — real
/// emerge's `--prune` rule ("removes all but the highest installed version
/// of a package"). Relative order of the survivors is otherwise unchanged.
fn drop_highest_version_per_cpn(
    candidates: Vec<portage_vdb::InstalledPackage>,
) -> Vec<portage_vdb::InstalledPackage> {
    let mut highest: std::collections::HashMap<portage_atom::Cpn, portage_atom::Cpv> =
        std::collections::HashMap::new();
    for pkg in &candidates {
        highest
            .entry(pkg.cpv().cpn)
            .and_modify(|best| {
                if pkg.cpv() > best {
                    *best = pkg.cpv().clone();
                }
            })
            .or_insert_with(|| pkg.cpv().clone());
    }
    candidates
        .into_iter()
        .filter(|pkg| highest.get(&pkg.cpv().cpn) != Some(pkg.cpv()))
        .collect()
}

/// Real emerge's world-atom selection (`_world_atom`): only the literal,
/// explicitly-named atoms make it onto the world set, never `@set`
/// expansions. Takes the already-*resolved* `atoms` — disambiguated via
/// `query::resolve_atom` upstream — not the raw command-line strings:
/// re-parsing them independently here used to reject any bare package name
/// as an invalid cpn even though the same atom had resolved fine already.
fn select_world_atoms(atoms: &[TargetAtom]) -> Vec<portage_atom::Dep> {
    atoms
        .iter()
        .filter(|t| t.origin == TargetOrigin::Explicit)
        .filter_map(|t| match portage_atom::Dep::parse(&t.atom) {
            Ok(d) => Some(d),
            Err(e) => {
                crate::style::warn_line!("skipping invalid world atom '{}': {e}", t.atom);
                None
            }
        })
        .collect()
}

/// The other half of real emerge's world selection: a `@name` set typed
/// directly on the command line, when `name` is a `world-candidate` set
/// (real portage's `usersets` only). Recorded as the literal `@name`
/// reference, never its expanded members — those are exactly what
/// `select_world_atoms` above excludes. Deduplicated: several members of
/// the same set all carry the same `Set(name)` origin.
fn select_world_set_refs(atoms: &[TargetAtom]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    atoms
        .iter()
        .filter_map(|t| match &t.origin {
            TargetOrigin::Set(name) if portage_repo::is_world_candidate(name) => Some(name.clone()),
            _ => None,
        })
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

/// `-W`/`--deselect`: remove `atoms` (or `@set` names) from the world file
/// only — no removal, no dependency graph, no VDB access at all beyond the
/// world file itself. Matches real emerge's own `--deselect` action.
fn deselect_atoms(cli: &cli::Cli, atoms: &[String]) -> Result<()> {
    if atoms.is_empty() {
        bail!("-W/--deselect needs at least one atom or @set");
    }
    let roots = cli.roots();
    let removed = maint::world::remove_atoms(Some(roots.merge_root()), atoms)?;
    println!(
        ">>> Removed {removed} entr{} from the world file",
        if removed == 1 { "y" } else { "ies" }
    );
    Ok(())
}

/// Shared removal core for `-C`/`--unmerge` and `-P`/`--prune`: given an
/// already-computed, non-empty set of installed packages to remove, run the
/// identical preview / `--pretend` / `--ask` / preserve-libs / removal /
/// env-update sequence. `verb`/`past` name the action in user-facing
/// output — the only thing that differs between the two callers. Both end
/// in "e" ("unmerge"/"prune"), so the gerund is derived, not passed.
async fn remove_matched_packages(
    cli: &cli::Cli,
    vdb: portage_vdb::Vdb,
    matched: Vec<portage_vdb::InstalledPackage>,
    verb: &str,
    past: &str,
) -> Result<()> {
    let stem = verb.strip_suffix('e').unwrap_or(verb);
    let gerund = format!("{stem}ing");
    // Every cpv this invocation is committed to removing — not just the one
    // being processed at a time — so a multi-atom `em -C a b` where `a`
    // needs a lib only `b` provides doesn't falsely think `b` still
    // provides it. See `preserve_libs::find_libs_to_preserve`'s doc.
    let exclude: std::collections::HashSet<portage_atom::Cpv> =
        matched.iter().map(|p| p.cpv().clone()).collect();

    println!("\n>>> These are the packages that would be {past}:\n");
    for pkg in &matched {
        println!(" {pkg}");
    }
    println!();

    run_unmerge_batch(cli, &vdb, &matched, &exclude, verb, &gerund).await
}

/// The removal core shared by `-C`/`-P` ([`remove_matched_packages`]) and
/// `-c`/`--depclean` (`depclean::run_with_targets`): `--pretend` preview,
/// `--ask`, the profile-env'd shell, the one-graph-per-batch preserve-libs
/// registry, the per-package removal loop, and the final `env-update`.
/// `verb`/`gerund` name the action in user-facing output — `gerund` verbatim
/// in the per-package progress line, lower-cased in the no-profile warning.
pub(crate) async fn run_unmerge_batch(
    cli: &cli::Cli,
    vdb: &portage_vdb::Vdb,
    packages: &[portage_vdb::InstalledPackage],
    exclude: &std::collections::HashSet<portage_atom::Cpv>,
    verb: &str,
    gerund: &str,
) -> Result<()> {
    // Same root/shell setup `dispatch.rs`'s `Applet::Ebuild` arm uses:
    // `roots()` for config/sysroot/eprefix (this *is* a merge-target
    // operation, unlike `select`'s config-root-only concerns — see
    // `select-target-flag-collision-fix` memory for why those differ), and
    // the separate `host_roots()` for BDEPEND-class tooling, never `roots()`'s
    // own value. Computed before the `--pretend` return too, since `-p`
    // also needs `root` to preview any preserve-libs findings.
    let roots = cli.roots();
    let root = roots.merge_root().to_owned();
    let broot = cli.host_roots();

    if cli.pretend {
        // Preview what preserve-libs would keep, without registering or
        // touching disk (read-only load, no store). One shared graph for
        // the whole batch — see `preserve_libs::build_link_graph`'s doc.
        let registry = preserve_libs::PreservedLibsRegistry::load(&root);
        let graph = preserve_libs::build_link_graph(vdb, exclude, &registry, &root);
        for pkg in packages {
            if let Ok(old_contents) = pkg.contents() {
                let preserved = preserve_libs::find_libs_to_preserve(&graph, pkg, &old_contents);
                preserve_libs::report_preserved(pkg.cpv(), &preserved, vdb);
            }
        }
        return Ok(());
    }

    if cli.merge_flags.ask && !confirm_action(verb, packages.len())? {
        println!(">>> Quitting.");
        return Ok(());
    }
    // Scratch trees for pkg_prerm/postrm land where builds would
    // (`emerge_atoms_inner`'s relocation rule).
    let work_base = ebuild::default_work_base(roots.relocate_root());

    let repo = crate::crossdev::main_repo(cli)?;
    let mut shell = repo.shell().await.context("creating shell")?;
    let ld_library_path =
        ebuild::build_ld_library_path(roots.build_eprefix(), roots.build_sysroot());
    shell.set_build_roots(
        roots.config(),
        roots.build_sysroot(),
        roots.build_eprefix(),
        Some(broot.merge_root()),
        ld_library_path.as_deref(),
    );
    shell.set_terminal(crate::style::terminal_config());
    if !ebuild::apply_profile_env(&mut shell, roots.config(), roots.config_overlay()).await? {
        crate::style::warn_line!(
            "no usable profile at {}/etc/portage/make.profile — {} without profile defaults",
            roots.config().unwrap_or(Utf8Path::new("/")),
            gerund.to_lowercase()
        );
    }

    // One shared graph + registry for the whole batch, not rebuilt per
    // package — see `preserve_libs::build_link_graph`'s doc.
    let mut registry = preserve_libs::PreservedLibsRegistry::load(&root);
    let graph = preserve_libs::build_link_graph(vdb, exclude, &registry, &root);

    let mut failures = 0usize;
    for pkg in packages {
        println!(">>> {gerund} {pkg}...");
        if let Err(e) = ebuild::unmerge_standalone(
            &mut shell,
            pkg,
            &work_base,
            &root,
            vdb,
            &graph,
            &mut registry,
        )
        .await
        {
            crate::style::error_line!("failed to {verb} {pkg}: {e:#}");
            failures += 1;
            continue;
        }
        println!(">>> {verb} success: {pkg}");
    }
    registry.reclaim(vdb, &root);
    registry.store();

    // `pkg_prerm`/`pkg_postrm` queue their messages in this process (there is
    // no worker seam on a removal), so this is what prints them.
    crate::elog::finalize_echo();

    // Refresh ld.so.cache / profile.env after removals — same as merge does
    // after each package, so libraries just deleted don't linger in the
    // dynamic linker cache until the next unrelated install.
    if let Err(e) = maint::env::env_update(&root) {
        crate::style::warn_line!("env-update after {verb} failed: {e:#}");
    }

    if failures > 0 {
        bail!(
            "{failures} of {} package(s) failed to {verb}",
            packages.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pkg(vdb_root: &std::path::Path, cat: &str, pf: &str) {
        let dir = vdb_root.join(cat).join(pf);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SLOT"), "0").unwrap();
        std::fs::write(dir.join("EAPI"), "8").unwrap();
    }

    fn open_vdb(dir: &std::path::Path) -> portage_vdb::Vdb {
        let root = camino::Utf8PathBuf::try_from(dir.to_owned()).unwrap();
        portage_vdb::Vdb::open(root.join("var/db/pkg")).unwrap()
    }

    // Same skip-if-absent precedent as `elfscan.rs`'s own tests: a real
    // system `.so`, so `@preserved-rebuild` wiring can be exercised without
    // hand-synthesizing an ELF file.
    fn real_system_lib() -> Option<(camino::Utf8PathBuf, crate::elfscan::ElfInfo)> {
        for candidate in ["/usr/lib64/libz.so.1", "/lib64/libz.so.1"] {
            let path = camino::Utf8PathBuf::from(candidate);
            if let Some(info) = crate::elfscan::scan_file(path.as_std_path())
                && info.soname.is_some()
            {
                return Some((path, info));
            }
        }
        None
    }

    #[test]
    fn expand_sets_resolves_preserved_rebuild_via_the_vdb() {
        let Some((lib_path, info)) = real_system_lib() else {
            return;
        };
        let soname = info.soname.unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let eroot = camino::Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let vdb_root = eroot.join("var/db/pkg");
        let pkg_dir = vdb_root.join("app-misc").join("consumer-1.0");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("SLOT"), "0").unwrap();
        std::fs::write(pkg_dir.join("CONTENTS"), "obj /usr/bin/consumer bbbb 0\n").unwrap();
        std::fs::write(
            pkg_dir.join("NEEDED.ELF.2"),
            format!("X86_64;/usr/bin/consumer;;;{soname};{}\n", info.category),
        )
        .unwrap();

        // Registry paths resolve against `eroot` in production (real root ==
        // real VDB root); copy the real lib into the tempdir at the same
        // relative location so that join still finds a real, parseable ELF.
        let staged_lib = eroot.join(lib_path.as_str().trim_start_matches('/'));
        std::fs::create_dir_all(staged_lib.parent().unwrap()).unwrap();
        std::fs::copy(lib_path.as_std_path(), staged_lib.as_std_path()).unwrap();

        let mut reg = preserve_libs::PreservedLibsRegistry::load(&eroot);
        reg.register(
            &portage_atom::Cpv::parse("sys-libs/libfoo-1.0").unwrap(),
            "0",
            1,
            vec![lib_path],
        );
        reg.store();

        let expanded = expand_sets(
            &["@preserved-rebuild".to_string()],
            &portage_resolve::Roots::for_test(eroot.as_str()),
        );
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0].atom, "app-misc/consumer:0");
        assert_eq!(
            expanded[0].origin,
            TargetOrigin::Set("preserved-rebuild".to_string())
        );
    }

    #[test]
    fn expand_sets_preserved_rebuild_is_empty_with_no_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let eroot = camino::Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(eroot.join("var/db/pkg")).unwrap();

        let expanded = expand_sets(
            &["@preserved-rebuild".to_string()],
            &portage_resolve::Roots::for_test(eroot.as_str()),
        );
        assert!(expanded.is_empty());
    }

    #[test]
    fn drop_highest_version_per_cpn_keeps_only_the_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb_root = tmp.path().join("var/db/pkg");
        write_pkg(&vdb_root, "app-misc", "foo-1.0");
        write_pkg(&vdb_root, "app-misc", "foo-2.0");
        write_pkg(&vdb_root, "app-misc", "foo-1.5");

        let vdb = open_vdb(tmp.path());
        let installed: Vec<_> = vdb.packages().into_iter().collect();

        let removable = drop_highest_version_per_cpn(installed);

        let versions: Vec<String> = removable.iter().map(|p| p.cpv().to_string()).collect();
        assert_eq!(versions.len(), 2);
        assert!(versions.contains(&"app-misc/foo-1.0".to_string()));
        assert!(versions.contains(&"app-misc/foo-1.5".to_string()));
        assert!(!versions.contains(&"app-misc/foo-2.0".to_string()));
    }

    #[test]
    fn drop_highest_version_per_cpn_is_a_noop_with_a_single_version() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb_root = tmp.path().join("var/db/pkg");
        write_pkg(&vdb_root, "app-misc", "foo-1.0");

        let vdb = open_vdb(tmp.path());
        let installed: Vec<_> = vdb.packages().into_iter().collect();

        assert!(drop_highest_version_per_cpn(installed).is_empty());
    }

    #[test]
    fn drop_highest_version_per_cpn_treats_each_cpn_independently() {
        let tmp = tempfile::tempdir().unwrap();
        let vdb_root = tmp.path().join("var/db/pkg");
        write_pkg(&vdb_root, "app-misc", "foo-1.0");
        write_pkg(&vdb_root, "app-misc", "foo-2.0");
        write_pkg(&vdb_root, "app-misc", "bar-1.0");

        let vdb = open_vdb(tmp.path());
        let installed: Vec<_> = vdb.packages().into_iter().collect();

        let removable = drop_highest_version_per_cpn(installed);

        let versions: Vec<String> = removable.iter().map(|p| p.cpv().to_string()).collect();
        assert_eq!(versions, vec!["app-misc/foo-1.0".to_string()]);
    }

    #[test]
    fn select_world_atoms_accepts_an_already_resolved_bare_name() {
        // Regression test for the 2026-08-04 bug: `atoms` here holds the
        // *resolved* form (what `query::resolve_atom` turned bare "gcc"
        // into), not the raw command-line string — selection must use that,
        // not re-parse "gcc" on its own and reject it as an invalid cpn.
        let atoms = vec![TargetAtom::explicit("sys-devel/gcc")];
        let world = select_world_atoms(&atoms);
        assert_eq!(world.len(), 1);
        assert_eq!(world[0].to_string(), "sys-devel/gcc");
    }

    #[test]
    fn select_world_atoms_drops_set_expansions() {
        let atoms = vec![TargetAtom {
            atom: "sys-devel/gcc".to_string(),
            origin: TargetOrigin::Set("world".to_string()),
        }];
        assert!(select_world_atoms(&atoms).is_empty());
    }

    #[test]
    fn select_world_set_refs_keeps_a_world_candidate_set_once() {
        let atoms = vec![
            TargetAtom {
                atom: "app-misc/foo".to_string(),
                origin: TargetOrigin::Set("myset".to_string()),
            },
            TargetAtom {
                atom: "app-misc/bar".to_string(),
                origin: TargetOrigin::Set("myset".to_string()),
            },
        ];
        assert_eq!(select_world_set_refs(&atoms), vec!["myset".to_string()]);
    }

    #[test]
    fn select_world_set_refs_drops_non_candidate_builtins_and_explicit_atoms() {
        let atoms = vec![
            TargetAtom::explicit("sys-devel/gcc"),
            TargetAtom {
                atom: "app-misc/foo".to_string(),
                origin: TargetOrigin::Set("world".to_string()),
            },
            TargetAtom {
                atom: "app-misc/bar".to_string(),
                origin: TargetOrigin::Set("system".to_string()),
            },
        ];
        assert!(select_world_set_refs(&atoms).is_empty());
    }

    #[test]
    fn match_installed_atoms_bails_on_empty_input() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("var/db/pkg")).unwrap();
        let vdb = open_vdb(tmp.path());
        assert!(match_installed_atoms(&vdb, &[], "-P/--prune").is_err());
    }

    #[test]
    fn match_installed_atoms_bails_when_nothing_matches() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("var/db/pkg")).unwrap();
        let vdb = open_vdb(tmp.path());
        let err =
            match_installed_atoms(&vdb, &["app-misc/foo".to_string()], "-P/--prune").unwrap_err();
        assert!(err.to_string().contains("-P/--prune"));
    }
}
