//! Emerge resolve-and-merge orchestration.

use std::str::FromStr;

use anyhow::{Context, bail};
use camino::Utf8Path;

use crate::cli;
use crate::error::{self, Result};
use crate::merge::confirm_action;
use crate::merge::run_merge_plan;
use crate::vdb::open_cli_vdb;
use crate::{binpkg, ebuild, maint, preflight, preserve_libs, query, search};

pub(crate) fn parse_atoms(raw: &[String]) -> Vec<portage_atom::Dep> {
    raw.iter()
        .filter_map(|s| match portage_atom::Dep::from_str(s) {
            Ok(dep) => Some(dep),
            Err(e) => {
                eprintln!("warning: skipping invalid atom '{}': {}", s, e);
                None
            }
        })
        .collect()
}

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
fn expand_sets(raw: &[String], config_root: Option<&Utf8Path>, eroot: &Utf8Path) -> Vec<String> {
    // Build the resolver lazily, only when a set ref is actually present, so a
    // plain `em foo` (no sets) pays no profile-build cost.
    let mut out = Vec::with_capacity(raw.len());
    #[allow(unused_assignments)]
    // stack_holder's initial None may not be read if no sets are expanded
    let mut stack_holder: Option<portage_repo::ProfileStack> = None;
    let mut resolver: Option<portage_repo::SetResolver<'_>> = None;

    for s in raw {
        let Some(name) = portage_repo::set_name(s) else {
            out.push(s.clone());
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
                    out.push(s.clone());
                    continue;
                }
            }
        }

        // If we have a resolver, use it; otherwise skip (resolver creation failed earlier)
        if let Some(res) = resolver.as_ref() {
            match res.resolve(name) {
                Ok(atoms) => out.extend(atoms.iter().map(|d| d.to_string())),
                Err(e) => eprintln!("warning: skipping @{name}: {e}"),
            }
        } else {
            // Resolver creation failed for earlier set; push raw string
            out.push(s.clone());
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
        },
    )
    .await
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
    } = opts;
    let extra_use_override = extra_use_override.as_deref();
    let merge_flags = merge_flags_override.as_ref().unwrap_or(&cli.merge_flags);
    let resolved = cli.repo_path();
    let repo_path = camino::Utf8Path::new(&resolved);
    if !repo_path.is_dir() {
        bail!("repo not found at {resolved}");
    }
    let repo = portage_repo::Repository::open(repo_path.as_std_path())?;
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
    let parsed = query::resolve_atoms(&expanded, &repo, vdb.as_ref(), mode);
    let atoms: Vec<String> = parsed.iter().map(|d| d.to_string()).collect();
    if atoms.is_empty() {
        bail!("em: no valid atoms");
    }

    // World selection (real emerge's `_world_atom`): only the genuine
    // top-level invocation (`update_world`), and only the literal,
    // explicitly-named atoms (not `@set` refs — world_sets tracking isn't
    // implemented yet, see todo/cli-flag-parity.md), skipped entirely for
    // the same flag set real portage skips it for.
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
        .map(|f| (f.deep, f.newuse))
        .unwrap_or((cli.depgraph_flags.deep, cli.depgraph_flags.newuse));
    let binpkg_index = binpkg::open_local_index_for_preview(cli, merge_flags);
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
        nodeps,
        extra_use_override,
        binpkg_index: binpkg_index.as_ref(),
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
        // adding an already-satisfied `emerge foo` to world.
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

    if cli.pretend {
        return Ok(());
    }

    // --prefix/--local relocates distfiles and work trees under the outer
    // prefix (eprefix), not under a --target sysroot — see Roots::relocate_root.
    let relocate = roots.relocate_root();
    let distdir = relocate.map(|p| p.join("var/cache/distfiles"));
    let work_base = ebuild::default_work_base(relocate);

    // `-B` builds without merging — ask about what will actually happen.
    let verb = if merge_flags.buildpkgonly {
        "build"
    } else {
        "merge"
    };
    if merge_flags.ask && !confirm_action(verb, outcome.plan.len())? {
        println!(">>> Quitting.");
        return Ok(());
    }

    run_merge_plan(
        &outcome.plan,
        &outcome.build_blockers,
        &roots,
        &work_base,
        distdir.as_deref(),
        merge_flags,
        cli,
    )
    .await?;

    maint::world::add_atoms(Some(roots.merge_root()), &world_atoms);
    Ok(())
}

/// Run the default emerge path for a parsed CLI invocation.
pub(crate) async fn run_emerge(cli: &cli::Cli) -> Result<()> {
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
        },
    )
    .await
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
    if atoms.is_empty() {
        bail!("-C/--unmerge needs at least one atom");
    }

    let vdb = open_cli_vdb(cli)?;

    // The same installed package can match two atoms given on the command
    // line (e.g. "foo" and "cat/foo") -- dedup on Cpv identity (Hash + Eq
    // already, no need to stringify) rather than sort+dedup by Display,
    // which would also scramble the natural match order for no reason.
    let mut seen = std::collections::HashSet::new();
    let mut matched: Vec<portage_vdb::InstalledPackage> = Vec::new();
    let mut unmatched: Vec<&str> = Vec::new();
    for raw in atoms {
        let pkgs = crate::vdb::find_packages(&vdb, raw);
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
            "-C/--unmerge: no installed package matched ({})",
            unmatched.join(", ")
        );
    }

    // Every cpv this invocation is committed to removing — not just the one
    // being processed at a time — so a multi-atom `em -C a b` where `a`
    // needs a lib only `b` provides doesn't falsely think `b` still
    // provides it. See `preserve_libs::find_libs_to_preserve`'s doc.
    let exclude: std::collections::HashSet<portage_atom::Cpv> =
        matched.iter().map(|p| p.cpv().clone()).collect();

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

    println!("\n>>> These are the packages that would be unmerged:\n");
    for pkg in &matched {
        println!(" {pkg}");
    }
    println!();

    if cli.pretend {
        // Preview what preserve-libs would keep, without registering or
        // touching disk (read-only load, no store). One shared graph for
        // the whole batch — see `preserve_libs::build_link_graph`'s doc.
        let registry = preserve_libs::PreservedLibsRegistry::load(&root);
        let graph = preserve_libs::build_link_graph(&vdb, &exclude, &registry, &root);
        for pkg in &matched {
            if let Ok(old_contents) = pkg.contents() {
                let preserved = preserve_libs::find_libs_to_preserve(&graph, pkg, &old_contents);
                preserve_libs::report_preserved(pkg.cpv(), &preserved, &vdb);
            }
        }
        return Ok(());
    }

    if cli.merge_flags.ask && !confirm_action("unmerge", matched.len())? {
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
            "warning: no usable profile at {}/etc/portage/make.profile — unmerging without profile defaults",
            roots.config().unwrap_or(Utf8Path::new("/"))
        );
    }

    // One shared graph + registry for the whole batch, not rebuilt per
    // package — see `preserve_libs::build_link_graph`'s doc.
    let mut registry = preserve_libs::PreservedLibsRegistry::load(&root);
    let graph = preserve_libs::build_link_graph(&vdb, &exclude, &registry, &root);

    let mut failures = 0usize;
    for pkg in &matched {
        println!(">>> Unmerging {pkg}...");
        if let Err(e) = ebuild::unmerge_standalone(
            &mut shell,
            pkg,
            &work_base,
            &root,
            &vdb,
            &graph,
            &mut registry,
        )
        .await
        {
            eprintln!("!!! failed to unmerge {pkg}: {e:#}");
            failures += 1;
            continue;
        }
        println!(">>> unmerge success: {pkg}");
    }
    registry.reclaim(&vdb, &root);
    registry.store();

    // Refresh ld.so.cache / profile.env after removals — same as merge does
    // after each package, so libraries just deleted don't linger in the
    // dynamic linker cache until the next unrelated install.
    if let Err(e) = maint::env::env_update(&root) {
        eprintln!("warning: env-update after unmerge failed: {e:#}");
    }

    if failures > 0 {
        bail!(
            "{failures} of {} package(s) failed to unmerge",
            matched.len()
        );
    }
    Ok(())
}
