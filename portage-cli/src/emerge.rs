//! Emerge resolve-and-merge orchestration.

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

/// Parse every token as a [`portage_atom::Dep`], failing the whole list on the
/// first invalid atom. Use this for destructive operations (`-c`/`--depclean`)
/// where dropping a typo would change the meaning of the command (e.g. a
/// targeted depclean silently becoming a full-system clean).
pub(crate) fn parse_atoms_strict(raw: &[String]) -> Result<Vec<portage_atom::Dep>> {
    raw.iter()
        .map(|s| portage_atom::Dep::from_str(s).with_context(|| format!("invalid atom '{s}'")))
        .collect()
}

/// Expand `@set` references in `raw` to concrete atoms, leaving plain atoms
/// untouched. Sets are a portage-config concept (not PMS); resolution lives in
/// `portage_repo::SetResolver`. The profile stack comes from
/// `<config_root>/etc/portage/make.profile` (for `@system`/`@profile`); user
/// sets, `@world`, and `@selected` are read from `eroot` (the install target).
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
pub(crate) fn expand_sets(
    raw: &[String],
    config_root: Option<&Utf8Path>,
    eroot: &Utf8Path,
) -> Vec<TargetAtom> {
    // Build the resolver lazily, only when a set ref is actually present, so a
    // plain `em foo` (no sets) pays no profile-build cost.
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
                }) {
                Ok(st) => {
                    // get_or_insert (not `stack_holder = Some(st); ...unwrap()`) hands
                    // back the `&ProfileStack` directly, so there's nothing to unwrap.
                    let stack = stack_holder.get_or_insert(st);
                    resolver = Some(portage_repo::SetResolver::new(stack, eroot));
                }
                Err(e) => {
                    eprintln!("warning: cannot expand @{name}: {e}");
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
                Err(e) => eprintln!("warning: skipping @{name}: {e}"),
            }
        } else {
            // Resolver creation failed for earlier set; push raw string
            out.push(TargetAtom::explicit(s.clone()));
        }
    }
    out
}
pub(crate) struct EmergeOpts<'a> {
    /// USE tokens (emerge syntax: `headers-only`, `-cxx`) forced on top of the
    /// configured USE, for both the resolve and the build — applied as a
    /// transient conf-layer override (`DepgraphOpts::extra_use_override`),
    /// matching where catalyst's own `CATALYST_USE` actually lands
    /// (`make.conf`, layer 3), NOT the process environment (layer 4, which
    /// would sit above `package.use` and incorrectly wipe it — found live,
    /// `em stages --stage1`'s `USE="-* build"` silently defeated
    /// `--autosolve-use` for exactly this reason until this was fixed
    /// 2026-07-12).
    pub use_override: &'a [String],
    /// `--nodeps`: merge only the named atoms, no dependency expansion.
    pub nodeps: bool,
    /// Override depgraph flags (deep, newuse) for this call. When None, uses
    /// the values from cli.depgraph_flags.
    pub depgraph_flags: Option<crate::cli::DepgraphFlags>,
    /// Override merge-behavior flags (jobs, keep_going, buildpkg, …) for this
    /// call. When None, uses the values from `cli.merge_flags` — same
    /// override/fallback shape as `depgraph_flags` above, needed for the same
    /// reason: the staged driver (`crossdev::run_staged`) must see whichever
    /// of the subcommand's own flattened `MergeFlags` or the top-level one
    /// the user actually set (`em -j 80 stages --stage1` vs `em stages
    /// --stage1 -j 80`), not just the top-level `Cli`'s copy.
    pub merge_flags: Option<crate::cli::MergeFlags>,
    /// Install into the plain `--local`/`--prefix`/`--root` EROOT, ignoring any
    /// `--target` sysroot substitution. Needed for `cross-<CTARGET>/gcc` steps
    /// woven into a `--target`-active `stages --stage1` run: that package's
    /// eclass always installs under the outer EROOT (`crossdev/mod.rs`'s module
    /// doc), never the target sysroot subdirectory `roots()` would otherwise
    /// substitute in.
    pub bypass_cross_root: bool,
    /// Drop the general `VDB(base) ∪ VDB(target)` installed-view sharing
    /// (`Roots::with_target_only_installed_view`) for this call. The native
    /// toolchain bootstrap (`crossdev::mod.rs`'s `toolchain()`) sets this: it
    /// is unconditionally self-contained regardless of `--root`/`--prefix`
    /// topology, and must not let the host's already-installed
    /// `virtual/os-headers`/etc. stand in for a copy actually merged into
    /// the target.
    pub target_only_installed_view: bool,
    /// Add explicitly-named atoms to the world file on a successful,
    /// non-oneshot/non-buildpkgonly/non-fetchonly/non-onlydeps real run —
    /// matching real emerge's `_world_atom`. Only the genuine top-level
    /// `em <atoms>` invocation sets this; internal staged-build steps
    /// (crossdev/stages) are not a user package selection and must not
    /// pollute the world file.
    pub update_world: bool,
    /// This call is itself replaying a previous `-r`/`--resume` — see
    /// `maint::resume::save`'s doc: a resumed run's own resume-state save
    /// must not supersede-with-backup (same logical job, not a new one).
    /// Only ever `true` from [`resume_atoms`]; every other call site is a
    /// fresh invocation.
    pub is_resume: bool,
    /// Optional activity bus (crossdev-stages / library). When `None`, a
    /// default bus with a live-FS sink is used for real merges.
    pub activity: Option<crate::activity::ActivityBus>,
    /// Session correlation (outer job_id / parent_job_id for staged plans).
    pub activity_session: crate::activity::ActivitySessionOpts,
}

/// Resolve and (unless `--pretend`) merge `raw_atoms` with the global config in
/// `cli`, plus the per-call [`EmergeOpts`]. Factored out of [`run_emerge`] so the
/// crossdev staged-build driver can run each toolchain step through the very
/// same path.
/// [`EmergeOpts`] minus its borrowed `use_override` (already folded into
/// `extra_use_override` by the time [`emerge_atoms_inner`] needs it) — keeps
/// that function to a single options argument instead of one parameter per
/// field.
struct ResolvedEmergeOpts {
    nodeps: bool,
    depgraph_flags: Option<crate::cli::DepgraphFlags>,
    merge_flags: Option<crate::cli::MergeFlags>,
    bypass_cross_root: bool,
    target_only_installed_view: bool,
    extra_use_override: Option<String>,
    update_world: bool,
    is_resume: bool,
    activity: Option<crate::activity::ActivityBus>,
    activity_session: crate::activity::ActivitySessionOpts,
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
            bypass_cross_root: opts.bypass_cross_root,
            target_only_installed_view: opts.target_only_installed_view,
            extra_use_override,
            update_world: opts.update_world,
            is_resume: opts.is_resume,
            activity: opts.activity,
            activity_session: opts.activity_session,
        },
    )
    .await
}

/// `--eta` estimate for `outcome.plan`, formatted for terminal display.
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

async fn emerge_atoms_inner(
    cli: &cli::Cli,
    raw_atoms: &[String],
    opts: ResolvedEmergeOpts,
) -> Result<()> {
    let ResolvedEmergeOpts {
        nodeps,
        depgraph_flags: depgraph_flags_override,
        merge_flags: merge_flags_override,
        bypass_cross_root,
        target_only_installed_view,
        extra_use_override,
        update_world,
        is_resume,
        activity: activity_override,
        activity_session,
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
    let mode = if merge_flags.update {
        query::ResolveMode::PreferInstalled
    } else {
        query::ResolveMode::Error
    };
    // Root model (docs/root-model.md): config from roots.config (host for a
    // --prefix overlay), installed view = VDB(base) ∪ VDB(target), and the
    // plan installs into target. `bypass_cross_root` (woven-in `cross-*`
    // toolchain steps only) uses the plain outer EROOT instead — that
    // category always installs there, never into `--target`'s sysroot
    // substitution (see `crossdev/mod.rs`'s module doc).
    //
    // `outer_roots()`, not `base_roots()`: found live 2026-07-09 (independent
    // review) — `base_roots()`'s `merge_root()` is deliberately the *BROOT*
    // view (host `/` under `--prefix`, `base.target: None`), not the outer
    // EROOT this comment already says bypass steps need. Under `--root` the
    // two happen to coincide (no eprefix, `outer_roots()` returns
    // `base_roots()` unchanged), which is why this went unnoticed: every
    // `bypass_cross_root` case tested before today was `--root`. Under
    // `--prefix P`, `base_roots()` merged every crossdev toolchain step onto
    // the real host `/` instead of `P` — silently "worked" for binutils
    // (whose real-arch binaries just landed on host `/usr/bin`, harmless to
    // notice) but broke `linux-headers`/`glibc[headers-only]`, whose
    // build-against-sysroot path never saw the merged headers.
    let roots = if bypass_cross_root {
        cli.outer_roots()
    } else {
        cli.roots()
    };
    let roots = if target_only_installed_view {
        roots.with_target_only_installed_view()
    } else {
        roots
    };
    // `Cli::broot()` (not `roots`): stays overlay-aware under `--target`
    // substitution, so a `MergeRoot::Host` entry's `-p` display matches its
    // real merge destination even when `roots` has had its own overlay-ness
    // cleared by the sysroot substitution. See `DepgraphOpts::host_merge_root`.
    let host_roots = cli.broot();
    // `--target <tuple>` targets `<EROOT>/usr/<tuple>`; fail early with a setup
    // hint if that sysroot has not been laid down by `em crossdev --init-target`
    // (otherwise the profile/make.conf read fails with an opaque ENOENT). Skipped
    // for `bypass_cross_root`: those steps target the outer EROOT on purpose, not
    // the sysroot this check is guarding.
    if let Some(tuple) = cli.target.as_deref().filter(|_| !bypass_cross_root) {
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
    let expanded = expand_sets(raw_atoms, roots.config(), roots.merge_root());
    // Resolved one at a time (not via `resolve_atoms`) so each atom keeps the
    // provenance `expand_sets` gave it; unresolvable ones are warned about and
    // dropped, exactly as `resolve_atoms` does.
    let atoms: Vec<TargetAtom> = expanded
        .iter()
        .filter_map(
            |t| match query::resolve_atom(&repo, vdb.as_ref(), mode, &t.atom) {
                Ok(dep) => Some(TargetAtom {
                    atom: dep.to_string(),
                    origin: t.origin.clone(),
                }),
                Err(e) => {
                    eprintln!("warning: {e}");
                    None
                }
            },
        )
        .collect();
    if atoms.is_empty() {
        bail!("em: no valid atoms");
    }

    // World selection (real emerge's `_world_atom`): only the genuine
    // top-level invocation (`update_world`), and only the literal,
    // explicitly-named atoms (not `@set` refs — world_sets tracking isn't
    // implemented yet).
    // CLI flag parity work is tracked in todo/cli-flag-parity.md.
    // Skipped entirely for the same flag set real portage skips it for.
    let should_update_world = update_world
        && !cli.pretend
        && !merge_flags.oneshot
        && !merge_flags.buildpkgonly
        && !merge_flags.fetchonly
        && !merge_flags.onlydeps;
    let world_atoms: Vec<portage_atom::Dep> = if should_update_world {
        raw_atoms
            .iter()
            .filter(|s| !s.starts_with('@'))
            .filter_map(|s| match portage_atom::Dep::parse(s) {
                Ok(d) => Some(d),
                Err(e) => {
                    eprintln!("warning: skipping invalid world atom '{s}': {e}");
                    None
                }
            })
            .collect()
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
        repo_path,
        atoms: &atoms,
        arch: &cli.arch,
        format,
        verbose: cli.verbose,
        empty: merge_flags.emptytree,
        autounmask_write: merge_flags.autounmask_write,
        autosolve_use: merge_flags.autosolve_use,
        multi_repo: cli.repo.is_none(),
        roots: &roots,
        host_merge_root: host_roots.merge_root(),
        onlydeps: merge_flags.onlydeps,
        with_bdeps: merge_flags.with_bdeps,
        root_deps_rdeps: merge_flags.root_deps,
        deep: depgraph_flags.0,
        update: merge_flags.update,
        newuse: depgraph_flags.1,
        changed_use: depgraph_flags.2,
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
    })
    .await?;

    // A non-zero resolver exit means USE/mask changes are needed (the change
    // block was already printed). Surface it as a typed error so the normal
    // Result flow yields exit 1 — `main` prints it quietly, and the staged
    // driver stops at the step that needs the change, with step context.
    // Checked before the `--pretend` return (not just for a real run) so
    // `-p`/`-a` show the same signal a real run would hit.
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
        return Ok(());
    }

    // Pre-flight: fail fast with a clear message if any plan entry's build
    // dependencies won't be present when it builds, rather than mid-build.
    // Run before the `--pretend` return (not just for a real run), so `-p`/
    // `-a` surface whether the plan is preflight-clean, the same way the
    // merge plan itself is already shown under `-p` — a preview could
    // otherwise never reveal a plan that would fail preflight during a real
    // run. Skipped under `--nodeps` regardless of `--pretend`: that flag
    // already means "merge only the named atoms, no dependency expansion or
    // verification" (matching emerge's own `--nodeps`) — the guard-rail
    // would otherwise still block on real, unconditional BDEPEND that
    // --nodeps deliberately opted out of checking (e.g. a genuine bootstrap
    // cycle like gawk -> bison -> gettext -> libxml2 -> meson -> python ->
    // gawk, which has no valid dependency order and must be seeded out of
    // order somewhere).
    if !nodeps {
        preflight::check(&outcome.plan, &roots, &outcome.provided)?;
    }

    // Shared by the `-p` preview below and the `-a` confirm prompt further
    // down — same "bind, write, flush" shape `activity/human.rs` uses for
    // every styled stdout write, so ANSI codes still strip cleanly on
    // non-tty output.
    let print_eta = || {
        let mut out = anstream::stdout();
        let _ = write!(out, "{}", eta_message(&roots, merge_flags, &outcome));
        let _ = out.flush();
    };

    if cli.pretend {
        if cli.eta {
            print_eta();
        }
        return Ok(());
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
        if cli.eta {
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
    crate::activity::attach_jsonl_outputs(
        &activity,
        cli.activity_fd,
        cli.activity_jsonl.as_deref(),
    )?;
    if cli.emergelog {
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
        host_root: cli.broot().merge_root().to_string(),
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
    Ok(())
}

/// Run the default emerge path for a parsed CLI invocation.
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
            bypass_cross_root: false,
            target_only_installed_view: false,
            update_world: true,
            is_resume: false,
            activity: None,
            activity_session: Default::default(),
        },
    )
    .await
}

/// `-r`/`--resume`: replay the last saved merge (`maint::resume`). Atoms are
/// not accepted alongside this flag — the package list comes from the saved
/// state, matching real emerge (`--resume`'s favorites/mergelist come only
/// from `mtimedb`, never the command line).
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
            bypass_cross_root: false,
            target_only_installed_view: false,
            update_world: true,
            is_resume: true,
            activity: None,
            activity_session: crate::activity::ActivitySessionOpts {
                job_id: Some(state.job_id.clone()).filter(|s| !s.is_empty()),
                parent_job_id: None,
            },
        },
    )
    .await
}

/// Match `atoms` against `vdb`, deduping by Cpv identity (the same installed
/// package can match two atoms given on the command line, e.g. "foo" and
/// "cat/foo" — Hash + Eq already, no need to stringify, and this preserves
/// the natural match order a sort+dedup by Display would scramble for no
/// reason). `label` prefixes the "no atom matched anything" error (e.g.
/// "-C/--unmerge", "-P/--prune"). Shared by both: their own match set differs
/// downstream (`-C` keeps every match, `-P` drops each Cpn's highest), but
/// "find installed packages for these atoms, report unmatched ones, bail if
/// nothing matched at all" is identical.
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
            eprintln!("!!! no installed package matches '{raw}'");
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
/// `depclean` is the safe "and clean up what's no longer needed"
/// alternative). Every installed slot/version matching any given atom is
/// removed; there is no plan to preview beyond the match list itself, so
/// `--pretend` just prints what would be removed and `--ask` confirms
/// against that same list.
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
/// env-update sequence. `verb`/`past` name the action in user-facing output
/// (`confirm_action`, error/progress messages) — the only thing that differs
/// between the two callers. Both verbs end in "e" ("unmerge"/"prune"), so the
/// gerund is derived rather than passed separately.
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
    // the separate `broot()` for BDEPEND-class tooling, never `roots()`'s
    // own value. Computed before the `--pretend` return too, since `-p`
    // also needs `root` to preview any preserve-libs findings.
    let roots = cli.roots();
    let root = roots.merge_root().to_owned();
    let broot = cli.broot();

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
    shell.set_build_roots(
        roots.config(),
        roots.build_sysroot(),
        roots.eprefix(),
        Some(broot.merge_root()),
    );
    let config_overlay = roots.eprefix().map(|e| e.join("etc/portage"));
    if !ebuild::apply_profile_env(&mut shell, roots.config(), config_overlay.as_deref()).await? {
        eprintln!(
            "warning: no usable profile at {}/etc/portage/make.profile — {} without profile defaults",
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
            eprintln!("!!! failed to {verb} {pkg}: {e:#}");
            failures += 1;
            continue;
        }
        println!(">>> {verb} success: {pkg}");
    }
    registry.reclaim(vdb, &root);
    registry.store();

    // Refresh ld.so.cache / profile.env after removals — same as merge does
    // after each package, so libraries just deleted don't linger in the
    // dynamic linker cache until the next unrelated install.
    if let Err(e) = maint::env::env_update(&root) {
        eprintln!("warning: env-update after {verb} failed: {e:#}");
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
