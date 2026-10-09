//! Turn a settled solve into the plan [`super::depgraph`] returns.

use std::collections::{HashMap, HashSet};

use portage_atom::{Cpn, Cpv};
use portage_atom_pubgrub::{DepClass, MergeRoot, PortagePackage, UseFlagRequirement};

use portage_resolve::{conflicts, download_size, effective_use, repo, required_use, root_aware};

use super::prepare::Prepared;
use super::{
    AutounmaskPersist, DepgraphOutcome, PlannedMerge, ResolveMetrics, Resolved,
    actionable_autounmask_candidates, autounmask, output, package_use, round, targets,
};
use crate::cli::DepgraphFormat;

/// Report the settled round and build the merge plan.
pub(super) fn assemble(
    prepared: Prepared<'_>,
    resolved: Resolved,
    metrics: &ResolveMetrics,
) -> anyhow::Result<DepgraphOutcome> {
    let Resolved {
        outcome,
        repair_completed,
        repair_incomplete,
        resolve_secs,
    } = resolved;
    let Prepared {
        set,
        atoms,
        world_additions,
        arch,
        format,
        verbose,
        autounmask_write,
        autounmask_persist,
        ask,
        roots,
        binpkg_index,
        quiet,
        cross,
        config_root,
        vdb_snapshots,
        data,
        target_installed,
        broot_snapshot,
        resolved,
        use_expand,
        use_expand_hidden,
        distdir,
        installed,
        policy_facts,
        provided_avail,
        root_cpns,
        unsatisfiable,
        masked_root_targets,
        host_installed_cpvs,
        base_installed_cpvs,
        target_installed_cpvs,
        root_pkgs,
        pristine_package_use,
        ..
    } = prepared;
    let repo::ResolvedPolicy {
        accept_keywords,
        accept_licenses,
        accept_properties,
        accept_restrict,
        package_mask,
        package_unmask,
        defaults,
        conf,
        env_use,
        package_use: _,
        profile_package_use,
        force_mask,
    } = resolved;

    let round::RoundOutcome {
        provider,
        solution,
        order,
        edges,
        closure_blockers,
        package_use,
        applied_reqs,
        ceded,
        autounmask_candidates,
        widened_autounmask_candidates,
        slot_op_cpns,
        dep_conflicts,
        proposed,
        exclude_omitted,
        resume_omitted,
        round_metrics: _,
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
        accept_properties: &accept_properties,
        accept_restrict: &accept_restrict,
        defaults: &defaults,
        conf: &conf,
        env_use: &env_use,
        package_use: &package_use,
        profile_package_use: &profile_package_use,
        force_mask: &force_mask,
        facts: None,
    }
    .with_package_use(&policy_facts, &package_use);

    let flag_reqs: HashMap<&PortagePackage, &UseFlagRequirement> = provider
        .use_flag_requirements()
        .iter()
        .map(|r| (&r.package, r))
        .collect();

    let portage_dir = config_root
        .unwrap_or(camino::Utf8Path::new("/"))
        .join("etc/portage");

    let dropped_autounmask = actionable_autounmask_candidates(
        autounmask_candidates,
        &widened_autounmask_candidates,
        &solution,
        &order,
        &data,
    );

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
            &defaults,
            &env_use,
            &pristine_package_use,
            &profile_package_use,
            &conf,
        );
        if autounmask_write && !entries.is_empty() {
            package_use::write(&entries, &portage_dir.join("package.use"))?;
        }
        entries
    };

    // `order` is already exclude-filtered above, so `plan_entries` (and the
    // Pretty/JSON/Tree preview built from it) inherit the exclusion for free.
    let plan_entries = root_aware::build_plan(order.clone());

    // Verbose mode shows per-package download size and a total; skip the
    // Manifest/DISTDIR work entirely in plain mode. Shared by Pretty and
    // Tree — Tree renders the same per-package row, so it needs the same
    // sizes and the same `PrettyCtx`.
    let sizes = if verbose >= 1 {
        download_size::compute(
            set.main().path(),
            &distdir,
            &data,
            &order,
            &final_policy,
            &ceded,
        )
    } else {
        HashMap::new()
    };
    // Real emerge's `resolver/output.py::check_system_world`, both halves: a
    // row is bold when the package is already tracked in `@selected`, *or*
    // when this run's own targets would add it (`world_additions`, empty
    // under `--oneshot`). `roots` and not `host_merge_root`: the world file
    // that matters is the one a real merge would write —
    // `maint::world::add_atoms(Some(roots.merge_root()), ..)` — the same
    // config/eroot pair `emerge.rs::expand_sets` reads `@world` from.
    let selected: HashSet<Cpn> =
        crate::maint::world::selected_cpns(config_root, roots.merge_root(), world_additions);
    let pretty_ctx = output::PrettyCtx {
        data: &data,
        installed: &installed,
        installed_entries: &target_installed,
        defaults: &defaults,
        conf: &conf,
        env_use: &env_use,
        package_use: &package_use,
        profile_package_use: &profile_package_use,
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
        resolve_secs,
        selected: &selected,
        requested: &root_cpns,
    };

    if !quiet {
        match format {
            DepgraphFormat::Pretty => {
                output::print_pretty_rooted(&pretty_ctx, &plan_entries, &cross)
            }
            DepgraphFormat::Json => {
                output::print_json(&data, &order, &edges, &installed, &flag_reqs)?
            }
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
                            .or_else(|| {
                                order.iter().find(|(p, _)| p == pkg).map(|(_, v)| v.clone())
                            });
                        ver.map(|v| (pkg.clone(), v))
                    })
                    .collect();
                output::print_tree(&pretty_ctx, &roots, &edges, &order, &cross)
            }
        }
    }

    // Advisory warnings are emitted after the plan so the merge list reads
    // first and the caveats follow it (emerge lists issues at the bottom too).
    // The plan is still produced; a PMS 8.3.2 hard blocker conflict fails via
    // `exit_code` after this block.
    //
    //  - reverse-dependency constraints: a complete-graph check that emerge's
    //    default targeted `-p` skips (e.g. upgrading docutils past an installed
    //    package's `<` bound);
    //  - blockers (`!foo` / `!!foo`) and `::repo` constraints, which the solver
    //    does not model;
    //  - REQUIRED_USE, evaluated per-package against its effective USE.
    let (hard_conflict, unmerges, required_use_unsatisfied) = {
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

        let hits = provider.check_blockers_detailed(&solution);
        let classified = conflicts::classify_blockers(&hits, &target_installed, &proposed);
        output::report_blockers(&classified);
        let hard_conflict = conflicts::is_hard_conflict(&classified);
        let unmerges = conflicts::planned_unmerges(&classified);

        let repo_violations = provider.check_repo_constraints(&solution);
        if !repo_violations.is_empty() {
            output::report_repo_constraint_violations(&repo_violations);
        }

        let held_back = provider.check_held_back_targets(&solution);
        if !held_back.is_empty() {
            output::report_held_back_targets(&held_back);
        }

        let masked = super::masked_installed_kept(
            &solution,
            &masked_root_targets,
            &provider,
            &data,
            &final_policy,
        );
        if !masked.is_empty() {
            output::report_masked_installed(&masked);
        }

        let ru_violations = required_use::find_violations(&data, &order, &final_policy, &ceded);
        if !ru_violations.is_empty() {
            output::report_required_use(&ru_violations);
        }
        // PMS 7.3.4: an unsatisfied REQUIRED_USE means the version "must be
        // treated as masked" — the advisory above still prints (matching
        // emerge's own UX), but a plan containing one is not installable,
        // same as a hard blocker conflict.
        let required_use_unsatisfied = !ru_violations.is_empty();

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

        // A required dependency was filtered out of *every* version (keyword /
        // mask / license) and had no `||` alternative, so the solver dropped
        // it and the printed plan is silently incomplete. Surface these
        // unconditionally — like emerge, an unsatisfiable requirement must
        // never be hidden. Report in order of severity: mask → keywords →
        // license.
        //
        // Widened selections are different: the solve already applied them
        // in memory and the plan is installable as-is, so they neither
        // block the merge nor set the non-zero exit — they're reported as
        // informational.
        //
        // Persistence is invocation-mode-gated, not flag-gated (settled
        // 2026-08-25, see todo/autounmask-cascading-fresh-slot-vs-version-
        // pin.md): `-p` never writes, `-a` confirms, a real run writes
        // unconditionally. Printed after the plan (not before, like the
        // package.use prompt below) so the user sees what is actually
        // being installed before being asked to confirm a config write.
        let has_dropped = !dropped_autounmask.is_empty();
        let has_widened = !widened_autounmask_candidates.is_empty();
        if has_dropped || has_widened {
            autounmask::report(&dropped_autounmask, false);
            autounmask::report(&widened_autounmask_candidates, true);
            if matches!(
                autounmask_persist,
                AutounmaskPersist::Always | AutounmaskPersist::Ask
            ) {
                let mut all = dropped_autounmask.clone();
                all.extend(widened_autounmask_candidates);
                let confirmed = match autounmask_persist {
                    AutounmaskPersist::Ask => crate::config_plan::confirm_config_write(all.len())?,
                    AutounmaskPersist::Always => true,
                    AutounmaskPersist::Never => false,
                };
                if confirmed {
                    autounmask::write(&all, &portage_dir)?;
                } else {
                    println!(">>> Quitting.");
                }
            }
        }

        package_use::report(&use_change_entries);

        // Deliberate divergence from real emerge (which only ever writes
        // package.use via `--autounmask-write`, never interactively): with
        // `--ask` and no `--autounmask-write`, offer to write these now
        // instead of making the user re-run with `--autounmask-write` by
        // hand. Skipped when `--autounmask-write` already wrote them above.
        if ask && !autounmask_write && !use_change_entries.is_empty() {
            let write_confirmed =
                crate::config_plan::confirm_config_write(use_change_entries.len())?;
            if write_confirmed {
                package_use::write(&use_change_entries, &portage_dir.join("package.use"))?;
            } else {
                println!(">>> Quitting.");
            }
        }

        // World-family targets nothing acceptable satisfies. Advisory: emerge
        // keeps going and exits 0 for these, so they stay out of `exit_code`.
        if !unsatisfiable.is_empty() {
            output::report_unsatisfiable_targets(&unsatisfiable, &data, set.is_multi());
        }
        (hard_conflict, unmerges, required_use_unsatisfied)
    };

    // `Total:`/`Size of downloads:` print *after* the advisories above (not
    // right after the merge list, as it used to): the caller's `--eta`
    // estimate prints immediately after this, once `depgraph()` returns, and
    // the two must be adjacent — previously the advisory block sat between
    // them, splitting one logical summary in two.
    if matches!(format, DepgraphFormat::Pretty) && verbose >= 1 {
        use std::io::Write as _;
        let mut out = anstream::stdout();
        writeln!(out, "{}", output::total_line(&order, &installed, &sizes)).ok();
    }

    // The merge plan for the build loop: ebuild paths come from the package's
    // source repo (main or overlay), USE from the same effective fold the
    // displayed plan used.
    let repo_path_of = |cpv: &Cpv| -> camino::Utf8PathBuf {
        let name = repo::repo_name_of(&data, cpv);
        set.by_name(name.as_str())
            .unwrap_or(set.main())
            .path()
            .to_owned()
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
                    cache.metadata.depend.list().to_vec(),
                    cache.metadata.bdepend.list().to_vec(),
                    effective.enabled_flags(),
                )
            } else {
                let mut effective = portage_atom_pubgrub::resolve_effective_use(
                    &HashMap::new(),
                    &defaults,
                    &cpv,
                    pkg.slot(),
                    &package_use,
                    &env_use,
                    &profile_package_use,
                    &conf,
                );
                // No cache ⇒ no IUSE/keywords; still apply global force/mask
                // and ceded so build USE stays consistent with the solver.
                let empty_iuse = HashSet::new();
                let slot_key = pkg.slot().map(portage_atom::Slot::from_name);
                effective_use::apply_force_mask(
                    &mut effective,
                    &force_mask,
                    &cpv,
                    slot_key.as_ref(),
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
            // `Ebuild::from_path`, must match for VDB/gcc-config routing).
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
                    MergeRoot::Base => base_installed_cpvs.contains(&cpv),
                    MergeRoot::Target => target_installed_cpvs.contains(&cpv),
                },
            }
        })
        .collect();

    // Build-order adjacency for `--jobs`: earlier plan indices that must finish
    // before this entry may *start*. Include RDEPEND as well as DEPEND/BDEPEND:
    // `virtual/*` packages are empty and only RDEPEND their real providers, so
    // DEPEND-only blockers let consumers race the provider (sed vs acl under
    // high --jobs, 2026-08-07). Restricted to earlier indices so the relation
    // is acyclic — `install_order` already linearised soft RDEPEND cycles (and
    // drops soft edges that would cycle). A spurious blocker only costs
    // parallelism; a missing one risks building before a dep is merged.
    //
    // Keyed by the full `PortagePackage` (carries slot), not `(MergeRoot,
    // Cpn)` alone: a CPN present at two slots would otherwise collapse to
    // "last wins", which can misreport a satisfied edge as backwards.
    let index_of: HashMap<PortagePackage, usize> = plan_entries
        .iter()
        .filter(|e| !e.pkg.is_virtual())
        .enumerate()
        .map(|(i, entry)| (entry.pkg.clone(), i))
        .collect();
    // `to > from` on a hard edge means the dependency is scheduled *after*
    // the dependent — never true for a real DAG edge, so it only fires
    // inside a genuine hard cycle; recorded into `hard_cycle_edges` below.
    let mut build_blockers: Vec<Vec<usize>> = vec![Vec::new(); plan.len()];
    let mut hard_cycle_edges: Vec<(Cpv, Cpv)> = Vec::new();
    for e in &edges {
        let hard = matches!(e.class, DepClass::Depend | DepClass::Bdepend);
        if !hard && !matches!(e.class, DepClass::Rdepend) {
            continue;
        }
        let (Some(&from), Some(&to)) = (index_of.get(&e.from.0), index_of.get(&e.to.0)) else {
            continue;
        };
        if to < from && !build_blockers[from].contains(&to) {
            build_blockers[from].push(to);
        }
        if hard && to > from {
            let pair = (plan[from].cpv.clone(), plan[to].cpv.clone());
            if !hard_cycle_edges.contains(&pair) {
                hard_cycle_edges.push(pair);
            }
        }
    }
    // `root_closure`'s blocker-only edges, which already point strictly
    // backwards; same guard as the real-edge loop above, kept for symmetry.
    for (from_pkg, to_pkg) in &closure_blockers {
        let (Some(&from), Some(&to)) = (index_of.get(from_pkg), index_of.get(to_pkg)) else {
            continue;
        };
        if to < from && !build_blockers[from].contains(&to) {
            build_blockers[from].push(to);
        }
    }

    metrics.log(resolve_secs);

    Ok(DepgraphOutcome {
        // Non-zero when the displayed plan is not directly installable: USE
        // changes, unmask/keyword/license, a PMS 8.3.2 hard blocker conflict,
        // or a PMS 7.3.4 unsatisfied REQUIRED_USE.
        exit_code: if use_change_entries.is_empty()
            && dropped_autounmask.is_empty()
            && !hard_conflict
            && !required_use_unsatisfied
        {
            0
        } else {
            1
        },
        plan,
        build_blockers,
        hard_cycle_edges,
        provided: provided_avail,
        broot_snapshot: Some(broot_snapshot),
        vdb_snapshots: Some(vdb_snapshots),
        unmerges,
    })
}
