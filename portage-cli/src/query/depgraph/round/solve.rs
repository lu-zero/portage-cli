//! The solve half of a round: provider construction, the USE co-solve fixpoint, and the widened retry

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    InstalledPackage as SolverInstalledPackage, InstalledPolicy, InstalledVersion, PackageDeps,
    PackageVersions, PortageDependencyProvider, PortagePackage, PortageVersionSet, SlotMap,
    UseFlagRequirement, UseOverride,
};
use portage_resolve::{effective_use, installed, repo};

use super::RoundCtx;
use crate::query::depgraph::package_needs_use_reinstall;
use crate::query::depgraph::package_use::{
    CosolveOutcome, FlagPin, MatchedFlag, UseConflict, cosolve_use_deps,
};
use crate::query::depgraph::provider_cache::ProviderKey;

type Solution = pubgrub::SelectedDependencies<PortagePackage, Version>;
type Targets = [(PortagePackage, PortageVersionSet)];

/// A round's settled solve
pub(super) struct Solved {
    pub(super) key: ProviderKey,
    pub(super) provider: PortageDependencyProvider,
    pub(super) solution: Solution,
    /// `package.use` plus the flags the co-solve fixpoint forced
    pub(super) package_use: Vec<(Dep, Vec<UseOverride>)>,
    pub(super) applied_reqs: Vec<UseFlagRequirement>,
    pub(super) matched_flags: Vec<MatchedFlag>,
    pub(super) use_conflicts: Vec<UseConflict>,
    /// Whether candidates outside keyword/mask/license acceptance were visible
    pub(super) widened: bool,
}

fn resolve_failed(e: pubgrub::PubGrubError<PortageDependencyProvider>) -> anyhow::Error {
    crate::error::ResolveFailed(format!(
        "resolution failed:\n{}",
        portage_atom_pubgrub::format_solve_error(e)
    ))
    .into()
}

impl RoundCtx<'_> {
    /// Solve `targets`, plus the installed packages in `repairs` the plan must keep satisfied
    ///
    /// With `--autounmask` widening, a strict solve that fails or drops acceptance-filtered hard
    /// dependencies is retried once with widened candidate supply; a failed retry keeps the strict result.
    pub(super) fn solve(&self, targets: &Targets, repairs: &Targets) -> anyhow::Result<Solved> {
        match self.attempt(targets, repairs, false) {
            Ok(strict)
                if self.autounmask_widen
                    && repo::has_widening_eligible_drops(
                        strict.provider.dropped_deps(),
                        self.data,
                    ) =>
            {
                tracing::debug!(
                    "acceptance-filtered hard deps were dropped; retrying with widened candidate supply"
                );
                Ok(self.attempt(targets, repairs, true).unwrap_or_else(|e| {
                    tracing::debug!("widened retry failed ({e}); keeping the degraded strict plan");
                    strict
                }))
            }
            Ok(strict) => Ok(strict),
            Err(strict_err) if self.autounmask_widen => {
                tracing::debug!("strict solve failed; retrying with widened candidate supply");
                self.attempt(targets, repairs, true).map_err(|_| strict_err)
            }
            Err(e) => Err(e),
        }
    }

    /// The USE co-solve fixpoint and its final solve
    ///
    /// The fixpoint forces the `[flag]` USE-deps of the plan on through synthetic `package.use`, as
    /// `emerge -p` previews the graph with the USE changes it will ask for. A failed
    /// `--autosolve-use` solve falls back to fixed USE so the plan and its advisory still appear.
    fn attempt(
        &self,
        targets: &Targets,
        repairs: &Targets,
        widened: bool,
    ) -> anyhow::Result<Solved> {
        let CosolveOutcome {
            package_use,
            applied: applied_reqs,
            matched: matched_flags,
            conflicts: use_conflicts,
            solved,
        } = cosolve_use_deps(
            self.package_use.clone(),
            self.data,
            |pu| {
                let (key, provider, result) =
                    self.build_and_solve(targets, repairs, widened, self.autosolve_use, pu);
                match result {
                    Ok(sol) => Some((key, provider, sol)),
                    Err(_) => {
                        self.providers.put(key, provider);
                        None
                    }
                }
            },
            |(_, provider, _)| provider.use_flag_requirements().to_vec(),
            |pkg, ver, flag| self.profile_pin(pkg, ver, flag),
        );

        let (key, provider, solution) = match solved {
            Some(solved) => solved,
            None => {
                let (key, provider, result) = self.build_and_solve(
                    targets,
                    repairs,
                    widened,
                    self.autosolve_use,
                    &package_use,
                );
                match result {
                    Ok(sol) => (key, provider, sol),
                    Err(_) if self.autosolve_use => {
                        crate::style::warn_line!(
                            "--autosolve-use could not satisfy REQUIRED_USE; \
                         falling back to a fixed-USE plan.",
                        );
                        self.providers.put(key, provider);
                        let (key, provider, result) =
                            self.build_and_solve(targets, repairs, widened, false, &package_use);
                        (key, provider, result.map_err(resolve_failed)?)
                    }
                    Err(e) => return Err(resolve_failed(e)),
                }
            }
        };
        Ok(Solved {
            key,
            provider,
            solution,
            package_use,
            applied_reqs,
            matched_flags,
            use_conflicts,
            widened,
        })
    }

    /// Whether the profile's `use.force`/`use.mask` holds `flag` of `pkg-ver`
    fn profile_pin(
        &self,
        pkg: &PortagePackage,
        ver: &Version,
        flag: Interned<DefaultInterner>,
    ) -> Option<FlagPin> {
        let cache = self.data.cache_entry(pkg.cpn(), ver)?;
        let cpv = Cpv::new(*pkg.cpn(), ver.clone());
        let stable = self
            .accept_keywords
            .is_stable(&cache.metadata.keywords, &cpv, pkg.slot());
        let iuse = effective_use::iuse_set(cache, &self.force_mask.iuse_injection);
        let slot = pkg.slot().map(portage_atom::Slot::from_name);
        let (forced, masked) = self
            .force_mask
            .effective(&cpv, slot.as_ref(), stable, &iuse);
        if masked.contains(&flag) {
            Some(FlagPin::Masked)
        } else {
            forced.contains(&flag).then_some(FlagPin::Forced)
        }
    }

    /// One solve with the provider for `package_use`, reused from an earlier one when it matches
    fn build_and_solve(
        &self,
        targets: &Targets,
        repairs: &Targets,
        widened: bool,
        autosolve_use: bool,
        package_use: &[(Dep, Vec<UseOverride>)],
    ) -> (
        ProviderKey,
        PortageDependencyProvider,
        Result<Solution, pubgrub::PubGrubError<PortageDependencyProvider>>,
    ) {
        // Only packages reachable from these are converted, which keeps the per-iteration rebuild cheap.
        let mut seeds: Vec<Cpn> = targets
            .iter()
            .chain(repairs)
            .filter(|(pkg, _)| !pkg.is_virtual())
            .map(|(pkg, _)| *pkg.cpn())
            .collect();
        if !self.emptytree_native {
            seeds.extend(self.target_installed.iter().map(|e| e.cpn));
        }
        seeds.extend(self.provided.iter().map(|cpv| cpv.cpn));
        let key = ProviderKey {
            package_use: package_use.to_vec(),
            autosolve_use,
            widened,
            seeds: seeds.iter().copied().collect(),
        };
        let mut provider = match self.providers.take(&key) {
            Some(provider) => provider,
            None => self.build_provider(seeds, package_use, autosolve_use, widened),
        };
        let resolve_start = Instant::now();
        let result = provider.resolve_with_repairs(targets.to_vec(), repairs.to_vec());
        tracing::debug!(
            elapsed_ms = resolve_start.elapsed().as_secs_f64() * 1000.0,
            "solver resolve"
        );
        (key, provider, result)
    }

    fn build_provider(
        &self,
        seeds: Vec<Cpn>,
        package_use: &[(Dep, Vec<UseOverride>)],
        autosolve_use: bool,
        widened: bool,
    ) -> PortageDependencyProvider {
        let adapter = self.adapter(package_use, autosolve_use, widened);
        let policy = adapter.policy();
        let slot_map: &SlotMap = if widened {
            self.widened_slot_map
                .as_ref()
                .expect("widened phase without a widened slot map")
        } else {
            self.slot_map
        };
        // Flags the fixpoint forced follow the original `package.use`.
        let forced_use = package_use.get(self.package_use.len()..).unwrap_or(&[]);
        let mut installed = installed_versions(
            self.target_installed,
            forced_use,
            self.rebuilding_installed_cpvs,
            self.emptytree_native,
            |e, pkg| self.needs_use_rebuild(e, pkg, &policy),
        );
        for (cpv, slot) in self.provided_avail {
            let slot = slot.as_deref().map(Interned::intern);
            installed
                .entry(cpv.cpn)
                .or_default()
                .push(InstalledVersion {
                    version: cpv.version.clone(),
                    built_use: portage_atom_pubgrub::UseConfig::new(),
                    kept: false,
                    meta: recorded_meta(slot, None, PackageDeps::default(), Vec::new(), false),
                });
        }

        let provider_start = Instant::now();
        let mut provider = PortageDependencyProvider::new_for_targets_with_bdeps_and_slot_map(
            portage_atom_pubgrub::WithInstalled {
                inner: adapter,
                installed,
            },
            seeds,
            self.solve_with_bdeps,
            slot_map,
        );
        tracing::debug!(
            elapsed_ms = provider_start.elapsed().as_secs_f64() * 1000.0,
            "provider build"
        );
        self.apply_solve_policy(&mut provider);
        for (pkg, version) in self.sysroot_installed {
            provider.add_sysroot_installed(pkg.clone(), version.clone());
        }
        self.register_installed(&mut provider, &policy);
        self.register_provided(&mut provider, &policy);
        // A BDEPEND the build host already has is satisfied there, unless a USE-dep needs a flag it lacks.
        for e in self.host_installed {
            provider.add_host_installed(
                e.package.clone(),
                e.version.clone(),
                e.active_use.clone(),
                e.iuse.clone(),
            );
        }
        provider
    }

    fn apply_solve_policy(&self, provider: &mut PortageDependencyProvider) {
        provider.set_cross_active(self.cross.active);
        provider.set_is_cross_arch(self.cross.is_cross_arch());
        provider.set_root_deps_rdeps(self.root_deps_rdeps);
        for (package, versions, use_deps, label) in self.root_use_deps {
            let credit = format!("{label} (argument)");
            provider.add_root_use_deps(
                package.clone(),
                versions.clone(),
                use_deps.clone(),
                &credit,
            );
        }
        provider.set_nodeps(self.nodeps);
        provider.set_rebuild_tree(self.emptytree_native);
        provider.set_prefer_newest_slot(self.deep || self.emptytree_native);
        // `-uD` upgrades in-slot across the whole solve; `-N` alone must not (Portage parity).
        provider.set_prefer_update(self.update && self.deep && !self.emptytree_native);
        provider.set_walk_satisfied_build_deps(
            self.use_reinstall_mode.is_some() && self.deep && !self.emptytree_native,
        );
        provider.set_kept_build_deps(self.solve_with_bdeps && self.deep);
        // Without `-u`, a root target an installed version satisfies keeps it.
        provider.set_selective_no_update(self.selective && !self.update);
    }

    /// `-N`/`-U` drift, checked on root targets and, under `--deep`, on every installed package
    fn needs_use_rebuild(
        &self,
        e: &installed::VdbEntry,
        pkg: &PortagePackage,
        policy: &repo::ResolvePolicy<'_>,
    ) -> bool {
        let checked = self.deep
            || self
                .root_pkgs
                .iter()
                .any(|r| r.cpn() == pkg.cpn() && r.slot() == pkg.slot());
        checked
            && self
                .use_reinstall_mode
                .is_some_and(|mode| package_needs_use_reinstall(mode, e, pkg, self.data, policy))
    }

    /// The VDB's installed packages, with their blockers, as Favor or Rebuild
    fn register_installed(
        &self,
        provider: &mut PortageDependencyProvider,
        policy: &repo::ResolvePolicy<'_>,
    ) {
        for (e, blockers) in self.target_installed.iter().zip(self.installed_blockers) {
            let pkg = match e.slot.filter(|s| !s.is_empty()) {
                Some(s) => PortagePackage::slotted(e.cpn, s),
                None => PortagePackage::unslotted(e.cpn),
            };
            provider.add_installed_blockers(&pkg, blockers);
            let policy = if self.emptytree_native || self.needs_use_rebuild(e, &pkg, policy) {
                InstalledPolicy::Rebuild
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
    }

    /// `package.provided` CPVs: installed on ROOT unless the VDB has that slot, and on the build host
    ///
    /// The system supplies them everywhere, so a BDEPEND on one does not pull a repo version onto the host.
    fn register_provided(
        &self,
        provider: &mut PortageDependencyProvider,
        policy: &repo::ResolvePolicy<'_>,
    ) {
        let in_vdb: HashSet<(Cpn, Option<String>)> = self
            .target_installed
            .iter()
            .map(|e| (e.cpn, e.slot.map(|s| s.to_string())))
            .collect();
        for (cpv, slot) in self.provided_avail {
            let pkg = match slot {
                Some(s) => PortagePackage::slotted(cpv.cpn, Interned::intern(s)),
                None => PortagePackage::unslotted(cpv.cpn),
            };
            if !in_vdb.contains(&(cpv.cpn, slot.clone())) {
                let (active_use, iuse) = self.provided_use(&pkg, &cpv.version, policy);
                provider.add_installed(SolverInstalledPackage {
                    package: pkg.clone(),
                    version: cpv.version.clone(),
                    policy: InstalledPolicy::Provided,
                    active_use,
                    iuse,
                });
            }
            provider.add_host_installed(pkg, cpv.version.clone(), Vec::new(), Vec::new());
        }
    }

    /// The enabled flags and IUSE a provided version would have been built with
    fn provided_use(
        &self,
        pkg: &PortagePackage,
        version: &Version,
        policy: &repo::ResolvePolicy<'_>,
    ) -> (
        Vec<Interned<DefaultInterner>>,
        Vec<Interned<DefaultInterner>>,
    ) {
        use portage_atom_pubgrub::UseFlagState;
        let Some(cache) = repo::find_cache(self.data, pkg, version) else {
            return (Vec::new(), Vec::new());
        };
        let cfg = effective_use::effective_use(policy, pkg, version, cache, true, &[]);
        let iuse = effective_use::iuse_set(cache, &policy.force_mask.iuse_injection);
        let active = iuse
            .iter()
            .copied()
            .filter(|f| matches!(cfg.get(*f), UseFlagState::Enabled))
            .collect();
        (active, iuse.into_iter().collect())
    }
}

/// Installed versions per package, as recorded in the VDB.
///
/// Not kept, and so reduced with today's desired USE when the repository
/// offers them: `rebuilding`, `use_rebuild`, packages in `forced_use`, and
/// everything under `--emptytree`.
fn installed_versions(
    target_installed: &[installed::VdbEntry],
    forced_use: &[(Dep, Vec<UseOverride>)],
    rebuilding: &HashSet<Cpv>,
    emptytree: bool,
    use_rebuild: impl Fn(&installed::VdbEntry, &PortagePackage) -> bool,
) -> HashMap<Cpn, Vec<InstalledVersion>> {
    let forced: HashSet<Cpn> = forced_use.iter().map(|(dep, _)| dep.cpn).collect();
    let kept = |e: &installed::VdbEntry| {
        if emptytree || forced.contains(&e.cpn) {
            return false;
        }
        if rebuilding.contains(&Cpv::new(e.cpn, e.version.clone())) {
            return false;
        }
        let pkg = match e.slot.filter(|s| !s.is_empty()) {
            Some(s) => PortagePackage::slotted(e.cpn, s),
            None => PortagePackage::unslotted(e.cpn),
        };
        !use_rebuild(e, &pkg)
    };
    let mut installed: HashMap<Cpn, Vec<InstalledVersion>> = HashMap::new();
    for e in target_installed {
        // VDB `USE` lists every enabled flag, implicit ones (ARCH, ELIBC_*,
        // KERNEL_*, ABI_*) included; anything it does not list was off.
        let mut built_use = portage_atom_pubgrub::UseConfig::new();
        for flag in &e.active_use {
            built_use.enable(*flag);
        }
        let slot = e.slot.filter(|s| !s.is_empty());
        let empty_any_of_matches = e.eapi.empty_any_of_matches();
        installed.entry(e.cpn).or_default().push(InstalledVersion {
            version: e.version.clone(),
            built_use,
            kept: kept(e),
            meta: recorded_meta(
                slot,
                e.subslot,
                e.deps.clone(),
                e.iuse.clone(),
                empty_any_of_matches,
            ),
        });
    }
    installed
}

/// Solver metadata for an installed version the repository may not offer
fn recorded_meta(
    slot: Option<Interned<DefaultInterner>>,
    subslot: Option<Interned<DefaultInterner>>,
    deps: PackageDeps,
    iuse: Vec<Interned<DefaultInterner>>,
    empty_any_of_matches: bool,
) -> PackageVersions {
    PackageVersions {
        slot,
        subslot,
        repo: None,
        iuse,
        iuse_defaults: HashMap::new(),
        deps,
        required_use: None,
        empty_any_of_matches,
        needs_unmask: false,
        live: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_atom_pubgrub::UseFlagState;

    fn entry(cpn: &str, version: &str, active_use: &[&str]) -> installed::VdbEntry {
        installed::VdbEntry {
            cpn: Cpn::try_new(cpn).expect("test cpn parses"),
            slot: Some(Interned::intern("0")),
            subslot: None,
            version: version.parse().expect("test version parses"),
            active_use: active_use.iter().map(|f| Interned::intern(f)).collect(),
            iuse: Vec::new(),
            eapi: portage_metadata::Eapi::Eight,
            deps: PackageDeps::default(),
        }
    }

    fn cpv(s: &str) -> Cpv {
        Cpv::parse(s).expect("test cpv parses")
    }

    #[test]
    fn installed_versions_take_vdb_use_and_skip_rebuilds() {
        let installed = [
            entry("net-misc/curl", "8.0", &["ssl", "amd64"]),
            entry("app-misc/reinstalled", "1.0", &["foo"]),
            entry("app-misc/drifted", "1.0", &[]),
            entry("app-misc/forced", "1.0", &[]),
        ];
        let rebuilding: HashSet<Cpv> = [cpv("app-misc/reinstalled-1.0")].into();
        let forced = [(
            Dep::parse("app-misc/forced").expect("test atom parses"),
            Vec::new(),
        )];
        let drifted = Cpn::try_new("app-misc/drifted").expect("test cpn parses");
        let versions = installed_versions(&installed, &forced, &rebuilding, false, |e, _| {
            e.cpn == drifted
        });
        assert_eq!(versions.len(), 4, "every install is listed");
        let kept: Vec<&Cpn> = versions
            .iter()
            .filter(|(_, v)| v[0].kept)
            .map(|(cpn, _)| cpn)
            .collect();
        assert_eq!(
            kept,
            [&cpv("net-misc/curl-8.0").cpn],
            "only curl stays installed"
        );
        let curl = &versions[&cpv("net-misc/curl-8.0").cpn][0].built_use;
        assert_eq!(curl.get(Interned::intern("ssl")), UseFlagState::Enabled);
        assert_eq!(curl.get(Interned::intern("amd64")), UseFlagState::Enabled);
        // Not in VDB USE: built without it, whatever today's default says.
        assert_eq!(curl.get(Interned::intern("http2")), UseFlagState::Disabled);

        let none = installed_versions(&installed, &[], &HashSet::new(), true, |_, _| false);
        assert!(
            none.values().flatten().all(|v| !v.kept),
            "--emptytree rebuilds everything"
        );
    }
}
