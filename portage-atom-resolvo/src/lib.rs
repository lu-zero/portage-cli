//! Bridge between [`portage_atom`] and the [`resolvo`] dependency solver.
//!
//! This crate maps Portage package atoms, versions, and dependency trees onto
//! resolvo's generic solver interface, enabling SAT-based dependency resolution
//! for Gentoo-style package managers.
#![warn(missing_docs)]

mod pool;
mod provider;
mod repository;
mod version_match;

pub use pool::{
    DepClass, DepEdge, InstalledPolicy, InstalledSet, PackageDeps, PackageMetadata, PackageName,
    PortagePool, UseConfig, VersionConstraint,
};
pub use portage_atom::DepEntry;
pub use portage_atom::interner;
pub use provider::PortageDependencyProvider;
pub use repository::{InMemoryRepository, PackageRepository};
pub use version_match::version_matches;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::interner::Interned;
    use portage_atom::{Blocker, Cpv, Dep};
    use resolvo::{DenseIndex, Problem, Solver, VersionSetId};

    use crate::pool::{DepClass, InstalledSet, PackageDeps, PackageMetadata, UseConfig};
    use crate::provider::PortageDependencyProvider;
    use crate::repository::InMemoryRepository;
    use portage_atom::DepEntry;

    /// Helper: build a [`PackageMetadata`] from a CPV string.
    /// Deps are placed in `depend` (build-time) for simplicity.
    fn pkg(cpv: &str, slot: &str, deps: Vec<DepEntry>) -> PackageMetadata {
        PackageMetadata {
            cpv: Cpv::parse(cpv).unwrap(),
            slot: Some(Interned::intern(slot)),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps {
                depend: deps,
                ..PackageDeps::default()
            },
        }
    }

    /// Helper: build a [`PackageMetadata`] with a sub-slot.
    fn pkg_subslot(cpv: &str, slot: &str, subslot: &str, deps: Vec<DepEntry>) -> PackageMetadata {
        PackageMetadata {
            cpv: Cpv::parse(cpv).unwrap(),
            slot: Some(slot.into()),
            subslot: Some(subslot.into()),
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps {
                depend: deps,
                ..PackageDeps::default()
            },
        }
    }

    #[test]
    fn solve_single_package_no_deps() {
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lang/rust-1.75.0", "0", vec![]));
        repo.add(pkg("dev-lang/rust-1.76.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());

        let req = provider.intern_requirement(&Dep::parse(">=dev-lang/rust-1.75.0").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        // Solver should pick exactly one version (the newest: 1.76.0).
        assert_eq!(solution.len(), 1);
        let meta = solver.provider().package_metadata(solution[0]);
        assert_eq!(meta.cpv, Cpv::parse("dev-lang/rust-1.76.0").unwrap());
    }

    #[test]
    fn solve_with_dependency_chain() {
        let mut repo = InMemoryRepository::new();

        // app-misc/foo-1.0 depends on >=dev-lib/bar-2.0
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse(">=dev-lib/bar-2.0").unwrap())],
        ));
        repo.add(pkg("dev-lib/bar-2.0", "0", vec![]));
        repo.add(pkg("dev-lib/bar-3.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());

        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        // Should install foo-1.0 and bar-3.0 (newest satisfying >=2.0).
        assert_eq!(solution.len(), 2);

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(cpvs.contains("dev-lib/bar-3.0"));
    }

    #[test]
    fn solve_any_of() {
        let mut repo = InMemoryRepository::new();

        // app-misc/foo-1.0 depends on || ( dev-lib/bar dev-lib/baz )
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::AnyOf(vec![
                DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap()),
                DepEntry::Atom(Dep::parse("dev-lib/baz").unwrap()),
            ])],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/baz-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());

        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        // Should install foo-1.0 plus exactly one of bar or baz.
        assert_eq!(solution.len(), 2);
        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(cpvs.contains("dev-lib/bar-1.0") || cpvs.contains("dev-lib/baz-1.0"));
    }

    #[test]
    fn solve_use_conditional_included() {
        let mut repo = InMemoryRepository::new();

        // app-misc/foo-1.0 depends on ssl? ( dev-lib/openssl )
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::UseConditional {
                flag: "ssl".into(),
                negate: false,
                children: vec![DepEntry::Atom(Dep::parse("dev-lib/openssl").unwrap())],
            }],
        ));
        repo.add(pkg("dev-lib/openssl-3.0.0", "0", vec![]));

        let use_config = UseConfig::from(
            ["ssl"]
                .into_iter()
                .map(Interned::intern)
                .collect::<HashSet<_>>(),
        );
        let mut provider = PortageDependencyProvider::new(&repo, &use_config);
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        // With ssl enabled, openssl should be pulled in.
        assert_eq!(solution.len(), 2);
        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(cpvs.contains("dev-lib/openssl-3.0.0"));
    }

    #[test]
    fn solve_use_conditional_excluded() {
        let mut repo = InMemoryRepository::new();

        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::UseConditional {
                flag: "ssl".into(),
                negate: false,
                children: vec![DepEntry::Atom(Dep::parse("dev-lib/openssl").unwrap())],
            }],
        ));
        repo.add(pkg("dev-lib/openssl-3.0.0", "0", vec![]));

        // ssl NOT in use_config => conditional deps excluded.
        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        // Without ssl, only foo should be installed.
        assert_eq!(solution.len(), 1);
        let meta = solver.provider().package_metadata(solution[0]);
        assert_eq!(meta.cpv.to_string(), "app-misc/foo-1.0");
    }

    #[test]
    fn solve_slot_separation() {
        let mut repo = InMemoryRepository::new();

        // Two python slots.
        repo.add(pkg("dev-lang/python-3.11.5", "3.11", vec![]));
        repo.add(pkg("dev-lang/python-3.12.1", "3.12", vec![]));

        // app needs both slots.
        repo.add(pkg(
            "app-misc/myapp-1.0",
            "0",
            vec![
                DepEntry::Atom(Dep::parse("dev-lang/python:3.11").unwrap()),
                DepEntry::Atom(Dep::parse("dev-lang/python:3.12").unwrap()),
            ],
        ));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/myapp").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        // Should install myapp + both python slots.
        assert_eq!(solution.len(), 3);
        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/myapp-1.0"));
        assert!(cpvs.contains("dev-lang/python-3.11.5"));
        assert!(cpvs.contains("dev-lang/python-3.12.1"));
    }

    #[test]
    fn solve_version_exact() {
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lang/rust-1.75.0", "0", vec![]));
        repo.add(pkg("dev-lang/rust-1.76.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());

        let req = provider.intern_requirement(&Dep::parse("=dev-lang/rust-1.75.0").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        assert_eq!(solution.len(), 1);
        let meta = solver.provider().package_metadata(solution[0]);
        assert_eq!(meta.cpv, Cpv::parse("dev-lang/rust-1.75.0").unwrap());
    }

    #[test]
    fn solve_slot_star_matches_any_slot() {
        let mut repo = InMemoryRepository::new();

        // Two python slots.
        repo.add(pkg("dev-lang/python-3.11.9", "3.11", vec![]));
        repo.add(pkg("dev-lang/python-3.12.4", "3.12", vec![]));

        // app depends on python:* — any slot is fine.
        repo.add(pkg(
            "app-misc/myapp-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lang/python:*").unwrap())],
        ));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/myapp").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        // Should install myapp + one python (solver picks one, any slot ok).
        assert_eq!(solution.len(), 2);
        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/myapp-1.0"));
        assert!(cpvs.contains("dev-lang/python-3.11.9") || cpvs.contains("dev-lang/python-3.12.4"));
    }

    #[test]
    fn blocker_types_recorded() {
        let mut repo = InMemoryRepository::new();

        // app-misc/foo-1.0 has a weak blocker on bar and a strong blocker on baz.
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![
                DepEntry::Atom(Dep::parse("!dev-lib/bar").unwrap()),
                DepEntry::Atom(Dep::parse("!!dev-lib/baz").unwrap()),
            ],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/baz-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());

        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        // foo should be installed; bar and baz should be excluded by blockers.
        assert_eq!(solution.len(), 1);
        let meta = solver.provider().package_metadata(solution[0]);
        assert_eq!(meta.cpv.to_string(), "app-misc/foo-1.0");

        // Verify the blocker types were recorded correctly.
        let pool = solver.provider().pool();
        let vs_count = pool.version_set_count();
        let mut found_weak = false;
        let mut found_strong = false;
        for i in 0..vs_count {
            let vs_id = VersionSetId::from_index(i);
            if let Some(blocker) = solver.provider().blocker_type(vs_id) {
                let constraint = pool.resolve_version_set(vs_id);
                if constraint.cpn.package == "bar" {
                    assert_eq!(blocker, Blocker::Weak);
                    found_weak = true;
                } else if constraint.cpn.package == "baz" {
                    assert_eq!(blocker, Blocker::Strong);
                    found_strong = true;
                }
            }
        }
        assert!(found_weak, "weak blocker for bar not found");
        assert!(found_strong, "strong blocker for baz not found");
    }

    #[test]
    fn rebuild_trigger_tracked() {
        let mut repo = InMemoryRepository::new();

        // app-misc/foo-1.0 depends on dev-lib/bar:= (rebuild trigger)
        // and dev-lib/baz:* (no rebuild trigger)
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![
                DepEntry::Atom(Dep::parse("dev-lib/bar:=").unwrap()),
                DepEntry::Atom(Dep::parse("dev-lib/baz:*").unwrap()),
            ],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/baz-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());

        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        assert_eq!(solution.len(), 3);

        // Check that bar's version set is a rebuild trigger, baz's is not.
        let pool = solver.provider().pool();
        let vs_count = pool.version_set_count();
        let mut bar_is_trigger = false;
        let mut baz_is_trigger = false;
        for i in 0..vs_count {
            let vs_id = VersionSetId::from_index(i);
            let constraint = pool.resolve_version_set(vs_id);
            if constraint.cpn.package == "bar" {
                bar_is_trigger = solver.provider().is_rebuild_trigger(vs_id);
            } else if constraint.cpn.package == "baz" {
                baz_is_trigger = solver.provider().is_rebuild_trigger(vs_id);
            }
        }
        assert!(bar_is_trigger, "bar:= should be a rebuild trigger");
        assert!(!baz_is_trigger, "baz:* should NOT be a rebuild trigger");
    }

    #[test]
    fn solve_subslot_matching() {
        let mut repo = InMemoryRepository::new();

        // Two versions of libfoo in slot 0 with different sub-slots.
        repo.add(pkg_subslot("dev-lib/libfoo-1.0", "0", "1", vec![]));
        repo.add(pkg_subslot("dev-lib/libfoo-2.0", "0", "2", vec![]));

        // app needs libfoo:0/2 — only libfoo-2.0 has sub-slot "2".
        repo.add(pkg(
            "app-misc/myapp-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/libfoo:0/2").unwrap())],
        ));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());

        let req = provider.intern_requirement(&Dep::parse("app-misc/myapp").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        assert_eq!(solution.len(), 2);
        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/myapp-1.0"));
        assert!(
            cpvs.contains("dev-lib/libfoo-2.0"),
            "should pick libfoo-2.0 (sub-slot 2), got: {:?}",
            cpvs
        );
    }

    #[test]
    fn solve_solver_decided_flag_not_needed() {
        // foo has ssl? ( openssl ). ssl is solver_decided.
        // Nothing else requires openssl → solver picks minimal: foo only.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::UseConditional {
                flag: "ssl".into(),
                negate: false,
                children: vec![DepEntry::Atom(Dep::parse("dev-lib/openssl").unwrap())],
            }],
        ));
        repo.add(pkg("dev-lib/openssl-3.0.0", "0", vec![]));

        let use_config = UseConfig {
            solver_decided: ["ssl"].into_iter().map(Interned::intern).collect(),
            ..UseConfig::default()
        };
        let mut provider = PortageDependencyProvider::new(&repo, &use_config);
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        // Minimal solution: just foo — the solver should not enable the
        // ssl flag (no need to pull in openssl).
        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(
            !cpvs.contains("dev-lib/openssl-3.0.0"),
            "openssl should NOT be in solution: {:?}",
            cpvs
        );
    }

    #[test]
    fn solve_solver_decided_flag_negated() {
        // foo has !ssl? ( libressl ). ssl is solver_decided.
        // The solver biases toward flag-off, so NotUSE_ssl is selected,
        // making !ssl? active → libressl pulled in.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::UseConditional {
                flag: "ssl".into(),
                negate: true,
                children: vec![DepEntry::Atom(Dep::parse("dev-lib/libressl").unwrap())],
            }],
        ));
        repo.add(pkg("dev-lib/libressl-3.9.0", "0", vec![]));

        let use_config = UseConfig {
            solver_decided: ["ssl"].into_iter().map(Interned::intern).collect(),
            ..UseConfig::default()
        };
        let mut provider = PortageDependencyProvider::new(&repo, &use_config);
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(
            cpvs.contains("dev-lib/libressl-3.9.0"),
            "libressl should be pulled in (flag biased off): {:?}",
            cpvs
        );
        // virtual/NotUSE_ssl should be selected, virtual/USE_ssl should not.
        assert!(
            cpvs.iter().any(|c| c.contains("NotUSE_ssl")),
            "virtual/NotUSE_ssl should be selected: {:?}",
            cpvs
        );
        assert!(
            !cpvs.iter().any(|c| c.contains("virtual/USE_ssl")),
            "virtual/USE_ssl should NOT be selected: {:?}",
            cpvs
        );
    }

    #[test]
    fn solve_solver_decided_flag_both_directions() {
        // foo has ssl? ( openssl ) and !ssl? ( libressl ).
        // Nothing else in the solution. ssl is solver_decided.
        // Solver biases off → libressl pulled in, openssl skipped.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![
                DepEntry::UseConditional {
                    flag: "ssl".into(),
                    negate: false,
                    children: vec![DepEntry::Atom(Dep::parse("dev-lib/openssl").unwrap())],
                },
                DepEntry::UseConditional {
                    flag: "ssl".into(),
                    negate: true,
                    children: vec![DepEntry::Atom(Dep::parse("dev-lib/libressl").unwrap())],
                },
            ],
        ));
        repo.add(pkg("dev-lib/openssl-3.0.0", "0", vec![]));
        repo.add(pkg("dev-lib/libressl-3.9.0", "0", vec![]));

        let use_config = UseConfig {
            solver_decided: ["ssl"].into_iter().map(Interned::intern).collect(),
            ..UseConfig::default()
        };
        let mut provider = PortageDependencyProvider::new(&repo, &use_config);
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        // Bias is off → libressl in, openssl out.
        assert!(
            cpvs.contains("dev-lib/libressl-3.9.0"),
            "libressl should be in solution (flag off): {:?}",
            cpvs
        );
        assert!(
            !cpvs.contains("dev-lib/openssl-3.0.0"),
            "openssl should NOT be in solution (flag off): {:?}",
            cpvs
        );
    }

    #[test]
    fn solve_solver_decided_flag_forced_on_by_conflict() {
        // foo has ssl? ( openssl ) and !ssl? ( libressl ).
        // openssl and libressl mutually exclude each other.
        // bar requires openssl independently.
        // ssl is solver_decided.
        //
        // Since bar forces openssl into the solution, and libressl
        // (from !ssl?) would conflict, the solver must choose ssl=on.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![
                DepEntry::UseConditional {
                    flag: "ssl".into(),
                    negate: false,
                    children: vec![DepEntry::Atom(Dep::parse("dev-lib/openssl").unwrap())],
                },
                DepEntry::UseConditional {
                    flag: "ssl".into(),
                    negate: true,
                    children: vec![DepEntry::Atom(Dep::parse("dev-lib/libressl").unwrap())],
                },
            ],
        ));
        repo.add(pkg(
            "dev-lib/openssl-3.0.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("!!dev-lib/libressl").unwrap())],
        ));
        repo.add(pkg(
            "dev-lib/libressl-3.9.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("!!dev-lib/openssl").unwrap())],
        ));
        repo.add(pkg(
            "app-misc/bar-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/openssl").unwrap())],
        ));

        let use_config = UseConfig {
            solver_decided: ["ssl"].into_iter().map(Interned::intern).collect(),
            ..UseConfig::default()
        };
        let mut provider = PortageDependencyProvider::new(&repo, &use_config);
        let reqs = vec![
            provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap()),
            provider.intern_requirement(&Dep::parse("app-misc/bar").unwrap()),
        ];
        let problem = Problem::new().requirements(reqs);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(cpvs.contains("app-misc/bar-1.0"));
        assert!(
            cpvs.contains("dev-lib/openssl-3.0.0"),
            "openssl required by bar: {:?}",
            cpvs
        );
        assert!(
            !cpvs.contains("dev-lib/libressl-3.9.0"),
            "libressl conflicts with openssl: {:?}",
            cpvs
        );
        // virtual/USE_ssl should be selected (flag forced on).
        assert!(
            cpvs.iter().any(|c| c.contains("virtual/USE_ssl")),
            "virtual/USE_ssl should be selected: {:?}",
            cpvs
        );
    }

    #[test]
    fn solve_solver_decided_flag_not_needed_skips_virtuals() {
        // foo has NO use-conditional deps at all. ssl is solver_decided
        // but foo doesn't reference it, so no virtual packages should
        // appear in the solution.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap())],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));

        let use_config = UseConfig {
            solver_decided: ["ssl"].into_iter().map(Interned::intern).collect(),
            ..UseConfig::default()
        };
        let mut provider = PortageDependencyProvider::new(&repo, &use_config);
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert_eq!(cpvs.len(), 2);
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(cpvs.contains("dev-lib/bar-1.0"));
        // No virtual USE packages should appear.
        assert!(
            !cpvs.iter().any(|c| c.contains("virtual/")),
            "no virtual packages expected: {:?}",
            cpvs
        );
    }

    // ── Repository constraint tests ──────────────────────────────────────

    #[test]
    fn solve_repo_constraint_matches() {
        // Two versions of the same package in different repos.
        // A dep with ::gentoo should only pick the gentoo version.
        let mut repo = InMemoryRepository::new();
        repo.add(PackageMetadata {
            cpv: Cpv::parse("dev-lib/foo-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: Some("gentoo".into()),
            dependencies: PackageDeps::default(),
        });
        repo.add(PackageMetadata {
            cpv: Cpv::parse("dev-lib/foo-2.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: Some("guru".into()),
            dependencies: PackageDeps::default(),
        });

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("dev-lib/foo::gentoo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: Vec<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert_eq!(cpvs, vec!["dev-lib/foo-1.0"]);
    }

    #[test]
    fn solve_repo_constraint_excludes() {
        // Only a guru version exists; dep requires ::gentoo → unsolvable.
        let mut repo = InMemoryRepository::new();
        repo.add(PackageMetadata {
            cpv: Cpv::parse("dev-lib/foo-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: Some("guru".into()),
            dependencies: PackageDeps::default(),
        });

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("dev-lib/foo::gentoo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        assert!(solver.solve(problem).is_err());
    }

    #[test]
    fn solve_repo_constraint_no_repo_on_candidate() {
        // Candidate has no repo set; dep requires ::gentoo → no match.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lib/foo-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("dev-lib/foo::gentoo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        assert!(solver.solve(problem).is_err());
    }

    #[test]
    fn solve_no_repo_constraint_accepts_any() {
        // Dep without ::repo should accept candidates from any repo.
        let mut repo = InMemoryRepository::new();
        repo.add(PackageMetadata {
            cpv: Cpv::parse("dev-lib/foo-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: Some("guru".into()),
            dependencies: PackageDeps::default(),
        });

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("dev-lib/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        assert_eq!(solution.len(), 1);
    }

    // ── USE dep constraint tests ─────────────────────────────────────

    #[test]
    fn solve_use_dep_enabled_matches() {
        // foo depends on bar[ssl]. bar has ssl enabled → should resolve.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/bar[ssl]").unwrap())],
        ));
        repo.add(PackageMetadata {
            cpv: Cpv::parse("dev-lib/bar-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec!["ssl".into()],
            use_flags: ["ssl".into()].into_iter().collect(),
            repo: None,
            dependencies: PackageDeps::default(),
        });

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        assert_eq!(solution.len(), 2);
    }

    #[test]
    fn solve_use_dep_enabled_no_match() {
        // foo depends on bar[ssl]. bar does NOT have ssl.
        // USE-dep constraints are not enforced during solving (deferred to
        // post-solve validation), so the solver finds a solution regardless.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/bar[ssl]").unwrap())],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        assert_eq!(solution.len(), 2);
    }

    #[test]
    fn solve_use_dep_disabled_matches() {
        // foo depends on bar[-debug]. bar does NOT have debug → should resolve.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/bar[-debug]").unwrap())],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        assert_eq!(solution.len(), 2);
    }

    #[test]
    fn solve_use_dep_disabled_no_match() {
        // foo depends on bar[-debug]. bar HAS debug.
        // USE-dep constraints are not enforced during solving (deferred to
        // post-solve validation), so the solver finds a solution regardless.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/bar[-debug]").unwrap())],
        ));
        repo.add(PackageMetadata {
            cpv: Cpv::parse("dev-lib/bar-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec!["debug".into()],
            use_flags: ["debug".into()].into_iter().collect(),
            repo: None,
            dependencies: PackageDeps::default(),
        });

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        assert_eq!(solution.len(), 2);
    }

    #[test]
    fn solve_use_dep_picks_matching_version() {
        // foo depends on bar[ssl]. bar-1.0 has ssl off, bar-2.0 has ssl on.
        // Solver should pick bar-2.0.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/bar[ssl]").unwrap())],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));
        repo.add(PackageMetadata {
            cpv: Cpv::parse("dev-lib/bar-2.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec!["ssl".into()],
            use_flags: ["ssl".into()].into_iter().collect(),
            repo: None,
            dependencies: PackageDeps::default(),
        });

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("dev-lib/bar-2.0"));
        assert!(!cpvs.contains("dev-lib/bar-1.0"));
    }

    #[test]
    fn solve_use_dep_conditional() {
        // foo depends on bar[ssl?]. Parent has ssl enabled → bar must have ssl.
        // bar-1.0 has ssl → should resolve.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/bar[ssl?]").unwrap())],
        ));
        repo.add(PackageMetadata {
            cpv: Cpv::parse("dev-lib/bar-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec!["ssl".into()],
            use_flags: ["ssl".into()].into_iter().collect(),
            repo: None,
            dependencies: PackageDeps::default(),
        });

        let use_config = UseConfig::from(
            ["ssl"]
                .into_iter()
                .map(Interned::intern)
                .collect::<HashSet<_>>(),
        );
        let mut provider = PortageDependencyProvider::new(&repo, &use_config);
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        assert_eq!(solution.len(), 2);
    }

    #[test]
    fn solve_use_dep_conditional_inactive() {
        // foo depends on bar[ssl?]. Parent has ssl DISABLED → no constraint.
        // bar-1.0 has no ssl → should still resolve.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/bar[ssl?]").unwrap())],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        assert_eq!(solution.len(), 2);
    }

    // ── Dep class separation tests ───────────────────────────────────

    #[test]
    fn solve_deps_from_multiple_classes() {
        // foo has DEPEND on bar, RDEPEND on baz, BDEPEND on qux.
        // All three should appear in the solution.
        let mut repo = InMemoryRepository::new();
        repo.add(PackageMetadata {
            cpv: Cpv::parse("app-misc/foo-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps {
                depend: vec![DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap())],
                rdepend: vec![DepEntry::Atom(Dep::parse("dev-lib/baz").unwrap())],
                bdepend: vec![DepEntry::Atom(Dep::parse("dev-lib/qux").unwrap())],
                ..PackageDeps::default()
            },
        });
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/baz-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/qux-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert_eq!(cpvs.len(), 4);
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(cpvs.contains("dev-lib/bar-1.0"));
        assert!(cpvs.contains("dev-lib/baz-1.0"));
        assert!(cpvs.contains("dev-lib/qux-1.0"));
    }

    #[test]
    fn solve_pdepend_treated_as_requirement() {
        // foo has PDEPEND on bar. For now it's treated as a hard requirement.
        let mut repo = InMemoryRepository::new();
        repo.add(PackageMetadata {
            cpv: Cpv::parse("app-misc/foo-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps {
                pdepend: vec![DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap())],
                ..PackageDeps::default()
            },
        });
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        assert_eq!(solution.len(), 2);
    }

    #[test]
    fn solve_empty_dep_classes_ignored() {
        // foo has only RDEPEND on bar, all other classes empty.
        let mut repo = InMemoryRepository::new();
        repo.add(PackageMetadata {
            cpv: Cpv::parse("app-misc/foo-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps {
                rdepend: vec![DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap())],
                ..PackageDeps::default()
            },
        });
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        assert_eq!(solution.len(), 2);
    }

    // ── Circular dependency / install ordering tests ─────────────────

    #[test]
    fn solve_cyclic_via_pdepend() {
        // A RDEPEND B, B PDEPEND A.
        // Solver should select both A and B.
        let mut repo = InMemoryRepository::new();
        repo.add(PackageMetadata {
            cpv: Cpv::parse("app-misc/aaa-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps {
                rdepend: vec![DepEntry::Atom(Dep::parse("app-misc/bbb").unwrap())],
                ..PackageDeps::default()
            },
        });
        repo.add(PackageMetadata {
            cpv: Cpv::parse("app-misc/bbb-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps {
                pdepend: vec![DepEntry::Atom(Dep::parse("app-misc/aaa").unwrap())],
                ..PackageDeps::default()
            },
        });

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/aaa").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert_eq!(cpvs.len(), 2);
        assert!(cpvs.contains("app-misc/aaa-1.0"));
        assert!(cpvs.contains("app-misc/bbb-1.0"));
    }

    #[test]
    fn install_order_no_cycle() {
        // A DEPEND B, B has no deps. install_order → [B, A].
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/aaa-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/bbb").unwrap())],
        ));
        repo.add(pkg("dev-lib/bbb-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/aaa").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let order = solver.provider().install_order(&solution).unwrap();
        let names: Vec<String> = order
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        // B must come before A.
        let pos_b = names.iter().position(|n| n == "dev-lib/bbb-1.0").unwrap();
        let pos_a = names.iter().position(|n| n == "app-misc/aaa-1.0").unwrap();
        assert!(
            pos_b < pos_a,
            "bbb should be installed before aaa, got: {:?}",
            names
        );
    }

    #[test]
    fn install_order_pdepend_deferred() {
        // A RDEPEND B, B PDEPEND A.
        // install_order should succeed (PDEPEND edge is deferred).
        // B should come before A (A needs B at runtime).
        let mut repo = InMemoryRepository::new();
        repo.add(PackageMetadata {
            cpv: Cpv::parse("app-misc/aaa-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps {
                rdepend: vec![DepEntry::Atom(Dep::parse("app-misc/bbb").unwrap())],
                ..PackageDeps::default()
            },
        });
        repo.add(PackageMetadata {
            cpv: Cpv::parse("app-misc/bbb-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps {
                pdepend: vec![DepEntry::Atom(Dep::parse("app-misc/aaa").unwrap())],
                ..PackageDeps::default()
            },
        });

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/aaa").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let order = solver.provider().install_order(&solution).unwrap();
        let names: Vec<String> = order
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        // B installed first, then A. B's PDEPEND on A is satisfied after.
        let pos_b = names.iter().position(|n| n == "app-misc/bbb-1.0").unwrap();
        let pos_a = names.iter().position(|n| n == "app-misc/aaa-1.0").unwrap();
        assert!(
            pos_b < pos_a,
            "bbb should be installed before aaa (PDEPEND deferred), got: {:?}",
            names
        );
    }

    #[test]
    fn dependency_graph_labels() {
        // A has DEPEND B, RDEPEND C, PDEPEND D.
        // dependency_graph() should return 3 edges with correct classes.
        let mut repo = InMemoryRepository::new();
        repo.add(PackageMetadata {
            cpv: Cpv::parse("app-misc/aaa-1.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps {
                depend: vec![DepEntry::Atom(Dep::parse("dev-lib/bbb").unwrap())],
                rdepend: vec![DepEntry::Atom(Dep::parse("dev-lib/ccc").unwrap())],
                pdepend: vec![DepEntry::Atom(Dep::parse("dev-lib/ddd").unwrap())],
                ..PackageDeps::default()
            },
        });
        repo.add(pkg("dev-lib/bbb-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/ccc-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/ddd-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/aaa").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();
        assert_eq!(solution.len(), 4);

        let edges = solver.provider().dependency_graph(&solution);
        assert_eq!(edges.len(), 3, "expected 3 edges, got: {:?}", edges);

        // Verify each edge class.
        let find_edge = |target_pkg: &str| -> DepClass {
            edges
                .iter()
                .find(|e| {
                    solver
                        .provider()
                        .package_metadata(e.to)
                        .cpv
                        .to_string()
                        .contains(target_pkg)
                })
                .unwrap_or_else(|| panic!("no edge to {}", target_pkg))
                .class
        };
        assert_eq!(find_edge("bbb"), DepClass::Depend);
        assert_eq!(find_edge("ccc"), DepClass::Rdepend);
        assert_eq!(find_edge("ddd"), DepClass::Pdepend);
    }

    // ── Installed-package / favored / locked tests ──────────────────

    #[test]
    fn favored_prefers_installed_version() {
        // Repo has rust 1.75 and 1.76; installed=1.75 Favored; req=dev-lang/rust → picks 1.75.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lang/rust-1.75.0", "0", vec![]));
        repo.add(pkg("dev-lang/rust-1.76.0", "0", vec![]));

        let mut installed = InstalledSet::new();
        installed.add_favored(pkg("dev-lang/rust-1.75.0", "0", vec![]));

        let mut provider =
            PortageDependencyProvider::with_installed(&repo, &UseConfig::default(), &installed);
        let req = provider.intern_requirement(&Dep::parse("dev-lang/rust").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        assert_eq!(solution.len(), 1);
        let meta = solver.provider().package_metadata(solution[0]);
        assert_eq!(meta.cpv, Cpv::parse("dev-lang/rust-1.75.0").unwrap());
    }

    #[test]
    fn favored_yields_to_constraint() {
        // Repo has rust 1.75 and 1.76; installed=1.75 Favored; req=>=1.76 → picks 1.76.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lang/rust-1.75.0", "0", vec![]));
        repo.add(pkg("dev-lang/rust-1.76.0", "0", vec![]));

        let mut installed = InstalledSet::new();
        installed.add_favored(pkg("dev-lang/rust-1.75.0", "0", vec![]));

        let mut provider =
            PortageDependencyProvider::with_installed(&repo, &UseConfig::default(), &installed);
        let req = provider.intern_requirement(&Dep::parse(">=dev-lang/rust-1.76.0").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        assert_eq!(solution.len(), 1);
        let meta = solver.provider().package_metadata(solution[0]);
        assert_eq!(meta.cpv, Cpv::parse("dev-lang/rust-1.76.0").unwrap());
    }

    #[test]
    fn locked_forces_version() {
        // Installed=1.75 Locked; req=dev-lang/rust → picks 1.75.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lang/rust-1.75.0", "0", vec![]));
        repo.add(pkg("dev-lang/rust-1.76.0", "0", vec![]));

        let mut installed = InstalledSet::new();
        installed.add_locked(pkg("dev-lang/rust-1.75.0", "0", vec![]));

        let mut provider =
            PortageDependencyProvider::with_installed(&repo, &UseConfig::default(), &installed);
        let req = provider.intern_requirement(&Dep::parse("dev-lang/rust").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        assert_eq!(solution.len(), 1);
        let meta = solver.provider().package_metadata(solution[0]);
        assert_eq!(meta.cpv, Cpv::parse("dev-lang/rust-1.75.0").unwrap());
    }

    #[test]
    fn locked_causes_failure_on_conflict() {
        // Installed=1.75 Locked; req=>=1.76 → solve fails.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lang/rust-1.75.0", "0", vec![]));
        repo.add(pkg("dev-lang/rust-1.76.0", "0", vec![]));

        let mut installed = InstalledSet::new();
        installed.add_locked(pkg("dev-lang/rust-1.75.0", "0", vec![]));

        let mut provider =
            PortageDependencyProvider::with_installed(&repo, &UseConfig::default(), &installed);
        let req = provider.intern_requirement(&Dep::parse(">=dev-lang/rust-1.76.0").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        assert!(solver.solve(problem).is_err());
    }

    #[test]
    fn installed_not_in_repo_injected() {
        // Repo has only 1.76; installed=1.75 Favored → 1.75 injected, picked.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lang/rust-1.76.0", "0", vec![]));

        let mut installed = InstalledSet::new();
        installed.add_favored(pkg("dev-lang/rust-1.75.0", "0", vec![]));

        let mut provider =
            PortageDependencyProvider::with_installed(&repo, &UseConfig::default(), &installed);
        let req = provider.intern_requirement(&Dep::parse("dev-lang/rust").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        assert_eq!(solution.len(), 1);
        let meta = solver.provider().package_metadata(solution[0]);
        assert_eq!(meta.cpv, Cpv::parse("dev-lang/rust-1.75.0").unwrap());
    }

    #[test]
    fn installed_with_slots() {
        // Repo has python 3.11.9 and 3.12.4; installed=3.11.5 Favored (not in repo).
        // The installed 3.11.5 is injected and picked for :3.11.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lang/python-3.11.9", "3.11", vec![]));
        repo.add(pkg("dev-lang/python-3.12.4", "3.12", vec![]));

        let mut installed = InstalledSet::new();
        installed.add_favored(pkg("dev-lang/python-3.11.5", "3.11", vec![]));

        let mut provider =
            PortageDependencyProvider::with_installed(&repo, &UseConfig::default(), &installed);
        let req = provider.intern_requirement(&Dep::parse("dev-lang/python:3.11").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        assert_eq!(solution.len(), 1);
        let meta = solver.provider().package_metadata(solution[0]);
        assert_eq!(meta.cpv, Cpv::parse("dev-lang/python-3.11.5").unwrap());
    }

    // ── ExactlyOneOf (^^) and AtMostOneOf (??) tests ──────────────

    #[test]
    fn solve_exactly_one_of() {
        // ^^ ( bar baz ) with both available → mutual exclusion ensures
        // exactly one alternative is selected.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::ExactlyOneOf(vec![
                DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap()),
                DepEntry::Atom(Dep::parse("dev-lib/baz").unwrap()),
            ])],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/baz-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(
            cpvs.contains("dev-lib/bar-1.0") || cpvs.contains("dev-lib/baz-1.0"),
            "at least one alternative should be selected: {:?}",
            cpvs
        );
        // Mutual exclusion: at most one should be selected.
        assert!(
            !(cpvs.contains("dev-lib/bar-1.0") && cpvs.contains("dev-lib/baz-1.0")),
            "mutual exclusion violated — both bar and baz selected: {:?}",
            cpvs
        );
    }

    #[test]
    fn solve_at_most_one_of_optional() {
        // ?? ( bar baz ) with no other dep pulling them in → the "none"
        // virtual is selected (biased first in the union), so neither
        // bar nor baz is installed.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::AtMostOneOf(vec![
                DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap()),
                DepEntry::Atom(Dep::parse("dev-lib/baz").unwrap()),
            ])],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/baz-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        // Neither bar nor baz should be installed.
        assert!(
            !cpvs.contains("dev-lib/bar-1.0"),
            "bar should not be installed: {:?}",
            cpvs
        );
        assert!(
            !cpvs.contains("dev-lib/baz-1.0"),
            "baz should not be installed: {:?}",
            cpvs
        );
    }

    #[test]
    fn solve_exactly_one_of_with_use_conditional() {
        // ^^ ( ssl? ( openssl ) libressl ) with ssl enabled → both are
        // candidates but mutual exclusion ensures exactly one is selected.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::ExactlyOneOf(vec![
                DepEntry::UseConditional {
                    flag: "ssl".into(),
                    negate: false,
                    children: vec![DepEntry::Atom(Dep::parse("dev-lib/openssl").unwrap())],
                },
                DepEntry::Atom(Dep::parse("dev-lib/libressl").unwrap()),
            ])],
        ));
        repo.add(pkg("dev-lib/openssl-3.0.0", "0", vec![]));
        repo.add(pkg("dev-lib/libressl-3.9.0", "0", vec![]));

        let use_config = UseConfig::from(
            ["ssl"]
                .into_iter()
                .map(Interned::intern)
                .collect::<HashSet<_>>(),
        );
        let mut provider = PortageDependencyProvider::new(&repo, &use_config);
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(
            cpvs.contains("dev-lib/openssl-3.0.0") || cpvs.contains("dev-lib/libressl-3.9.0"),
            "at least one alternative should be selected: {:?}",
            cpvs
        );
        // Mutual exclusion: at most one should be selected.
        assert!(
            !(cpvs.contains("dev-lib/openssl-3.0.0") && cpvs.contains("dev-lib/libressl-3.9.0")),
            "mutual exclusion violated — both openssl and libressl selected: {:?}",
            cpvs
        );
    }

    #[test]
    fn solve_exactly_one_of_excludes_second() {
        // ^^ ( bar baz ) plus an independent dep on bar → solver picks bar
        // (satisfies both the direct dep and the ^^ group), baz is excluded
        // by mutual exclusion.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![
                DepEntry::ExactlyOneOf(vec![
                    DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap()),
                    DepEntry::Atom(Dep::parse("dev-lib/baz").unwrap()),
                ]),
                DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap()),
            ],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/baz-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(
            cpvs.contains("dev-lib/bar-1.0"),
            "bar should be selected: {:?}",
            cpvs
        );
        assert!(
            !cpvs.contains("dev-lib/baz-1.0"),
            "baz should be excluded by mutual exclusion: {:?}",
            cpvs
        );
    }

    #[test]
    fn solve_at_most_one_of_with_independent_dep() {
        // ?? ( bar baz ) plus independent dep on bar → bar installed,
        // baz excluded by mutual exclusion.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![
                DepEntry::AtMostOneOf(vec![
                    DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap()),
                    DepEntry::Atom(Dep::parse("dev-lib/baz").unwrap()),
                ]),
                DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap()),
            ],
        ));
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/baz-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(
            cpvs.contains("dev-lib/bar-1.0"),
            "bar should be selected: {:?}",
            cpvs
        );
        assert!(
            !cpvs.contains("dev-lib/baz-1.0"),
            "baz should be excluded by mutual exclusion: {:?}",
            cpvs
        );
    }

    #[test]
    fn solve_exactly_one_of_three_way() {
        // ^^ ( a b c ) → exactly one selected.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::ExactlyOneOf(vec![
                DepEntry::Atom(Dep::parse("dev-lib/aaa").unwrap()),
                DepEntry::Atom(Dep::parse("dev-lib/bbb").unwrap()),
                DepEntry::Atom(Dep::parse("dev-lib/ccc").unwrap()),
            ])],
        ));
        repo.add(pkg("dev-lib/aaa-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/bbb-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/ccc-1.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert!(cpvs.contains("app-misc/foo-1.0"));
        // Exactly one of aaa, bbb, ccc should be selected.
        let count = ["dev-lib/aaa-1.0", "dev-lib/bbb-1.0", "dev-lib/ccc-1.0"]
            .iter()
            .filter(|&&p| cpvs.contains(p))
            .count();
        assert_eq!(
            count, 1,
            "exactly one of aaa/bbb/ccc should be selected: {:?}",
            cpvs
        );
    }

    #[test]
    fn locked_does_not_affect_other_slots() {
        // Locked python:3.11; req=python:3.12 → 3.12 selected freely.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lang/python-3.11.9", "3.11", vec![]));
        repo.add(pkg("dev-lang/python-3.12.4", "3.12", vec![]));

        let mut installed = InstalledSet::new();
        installed.add_locked(pkg("dev-lang/python-3.11.9", "3.11", vec![]));

        let mut provider =
            PortageDependencyProvider::with_installed(&repo, &UseConfig::default(), &installed);
        let req = provider.intern_requirement(&Dep::parse("dev-lang/python:3.12").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        assert_eq!(solution.len(), 1);
        let meta = solver.provider().package_metadata(solution[0]);
        assert_eq!(meta.cpv, Cpv::parse("dev-lang/python-3.12.4").unwrap());
    }

    #[test]
    fn installed_deps_are_resolved() {
        // Installed foo (not in repo) depends on bar (in repo) → both selected.
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));

        let mut installed = InstalledSet::new();
        installed.add_favored(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/bar").unwrap())],
        ));

        let mut provider =
            PortageDependencyProvider::with_installed(&repo, &UseConfig::default(), &installed);
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();
        assert_eq!(cpvs.len(), 2);
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(cpvs.contains("dev-lib/bar-1.0"));
    }

    #[test]
    fn solve_any_of_with_all_of_groups() {
        let mut repo = InMemoryRepository::new();

        // Models the python_gen_any_dep pattern:
        // || ( ( dev-lang/python:3.14 dev-python/sphinx ) ( dev-lang/python:3.13 dev-python/sphinx ) )
        // The solver must pick one group: both python AND sphinx from the same group.
        repo.add(pkg(
            "app-misc/foo-1.0",
            "0",
            vec![DepEntry::AnyOf(vec![
                DepEntry::AllOf(vec![
                    DepEntry::Atom(Dep::parse("dev-lang/python:3.14").unwrap()),
                    DepEntry::Atom(Dep::parse("dev-python/sphinx").unwrap()),
                ]),
                DepEntry::AllOf(vec![
                    DepEntry::Atom(Dep::parse("dev-lang/python:3.13").unwrap()),
                    DepEntry::Atom(Dep::parse("dev-python/sphinx").unwrap()),
                ]),
            ])],
        ));
        repo.add(pkg("dev-lang/python-3.13.0", "3.13", vec![]));
        repo.add(pkg("dev-lang/python-3.14.0", "3.14", vec![]));
        repo.add(pkg("dev-python/sphinx-7.0.0", "0", vec![]));

        let mut provider = PortageDependencyProvider::new(&repo, &UseConfig::default());
        let req = provider.intern_requirement(&Dep::parse("app-misc/foo").unwrap());
        let problem = Problem::new().requirements(vec![req]);

        let mut solver = Solver::new(provider);
        let solution = solver.solve(problem).unwrap();

        let cpvs: HashSet<String> = solution
            .iter()
            .map(|&sid| solver.provider().package_metadata(sid).cpv.to_string())
            .collect();

        // foo + virtual/allof choice + exactly one python slot + sphinx = 4
        assert!(cpvs.contains("app-misc/foo-1.0"));
        assert!(cpvs.contains("dev-python/sphinx-7.0.0"));
        // Exactly one python slot, not both
        assert!(cpvs.contains("dev-lang/python-3.14.0") || cpvs.contains("dev-lang/python-3.13.0"));
        assert!(
            !(cpvs.contains("dev-lang/python-3.14.0") && cpvs.contains("dev-lang/python-3.13.0"))
        );
        // Both members of the chosen group must be present
        if cpvs.contains("dev-lang/python-3.14.0") {
            assert!(cpvs.contains("dev-python/sphinx-7.0.0"));
        }
        if cpvs.contains("dev-lang/python-3.13.0") {
            assert!(cpvs.contains("dev-python/sphinx-7.0.0"));
        }
    }
}
