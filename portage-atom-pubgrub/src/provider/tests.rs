use super::*;
use crate::repository::{InMemoryRepository, PackageDeps, PackageVersions};
use portage_atom::interner::Interned;
use portage_atom::{Cpn, Dep, DepEntry, DepList};
use pubgrub::DependencyProvider as _; // for choose_version in tests

fn empty_deps() -> PackageDeps {
    PackageDeps {
        depend: (vec![]).into(),
        rdepend: (vec![]).into(),
        bdepend: (vec![]).into(),
        pdepend: (vec![]).into(),
        idepend: (vec![]).into(),
    }
}

fn make_simple_repo() -> InMemoryRepository {
    let mut repo = InMemoryRepository::new();

    let openssl_cpv = portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap();
    repo.add_version(openssl_cpv, Some(Interned::intern("0")), None, empty_deps());

    let openssl_cpv2 = portage_atom::Cpv::parse("dev-libs/openssl-3.1.0").unwrap();
    repo.add_version(
        openssl_cpv2,
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );

    let rust_cpv = portage_atom::Cpv::parse("dev-lang/rust-1.75.0").unwrap();
    repo.add_version(
        rust_cpv,
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            depend: (DepEntry::parse(">=dev-libs/openssl-3.0.0").unwrap()).into(),
            rdepend: (DepEntry::parse(">=dev-libs/openssl-3.0.0").unwrap()).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    repo
}

#[test]
fn provider_constructs() {
    let mut repo = make_simple_repo();
    let config = UseConfig::new();
    let _provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };
}

#[test]
fn choose_highest_version() {
    let mut repo = make_simple_repo();
    let config = UseConfig::new();
    let provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };
    let openssl = PortagePackage::slotted(
        portage_atom::Cpn::parse("dev-libs/openssl").unwrap(),
        Interned::intern("0"),
    );
    let version = provider
        .choose_version(&openssl, &PortageVersionSet::any())
        .unwrap();
    assert_eq!(version, Some(Version::parse("3.1.0").unwrap()));
}

#[test]
fn resolve_simple() {
    let mut repo = make_simple_repo();
    let config = UseConfig::new();
    let provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };
    let root = PortagePackage::slotted(
        portage_atom::Cpn::parse("dev-lang/rust").unwrap(),
        Interned::intern("0"),
    );
    let result = pubgrub::resolve(&provider, root, Version::parse("1.75.0").unwrap());
    assert!(result.is_ok());
    let solution = result.unwrap();
    assert!(
        solution
            .get(&PortagePackage::slotted(
                portage_atom::Cpn::parse("dev-libs/openssl").unwrap(),
                Interned::intern("0"),
            ))
            .is_some()
    );
}

#[test]
fn multi_slot_installs_both_when_required() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("dev-lang/python-3.11.9").unwrap(),
        Some(Interned::intern("3.11")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-lang/python-3.12.4").unwrap(),
        Some(Interned::intern("3.12")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            depend: vec![
                DepEntry::Atom(Dep::parse("dev-lang/python:3.11").unwrap()),
                DepEntry::Atom(Dep::parse("dev-lang/python:3.12").unwrap()),
            ]
            .into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let provider = PortageDependencyProvider::new(repo);
    let root =
        PortagePackage::slotted(Cpn::parse("app-misc/myapp").unwrap(), Interned::intern("0"));
    let result = pubgrub::resolve(&provider, root, Version::parse("1.0").unwrap());
    assert!(result.is_ok());
    let solution = result.unwrap();
    assert!(
        solution
            .get(&PortagePackage::slotted(
                Cpn::parse("dev-lang/python").unwrap(),
                Interned::intern("3.11"),
            ))
            .is_some(),
        "python:3.11 should be in solution"
    );
    assert!(
        solution
            .get(&PortagePackage::slotted(
                Cpn::parse("dev-lang/python").unwrap(),
                Interned::intern("3.12"),
            ))
            .is_some(),
        "python:3.12 should be in solution"
    );
}

#[test]
fn resolve_slot_operator_equal() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            depend: (DepEntry::parse("dev-libs/openssl:=").unwrap()).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let provider = PortageDependencyProvider::new(repo);
    let root =
        PortagePackage::slotted(Cpn::parse("app-misc/myapp").unwrap(), Interned::intern("0"));
    let result = pubgrub::resolve(&provider, root, Version::parse("1.0").unwrap());
    assert!(result.is_ok());
    let solution = result.unwrap();
    assert!(
        solution
            .get(&PortagePackage::slotted(
                Cpn::parse("dev-libs/openssl").unwrap(),
                Interned::intern("0"),
            ))
            .is_some()
    );
}

#[test]
fn resolve_slot_operator_star() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            depend: (DepEntry::parse("dev-libs/openssl:*").unwrap()).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let provider = PortageDependencyProvider::new(repo);
    let root =
        PortagePackage::slotted(Cpn::parse("app-misc/myapp").unwrap(), Interned::intern("0"));
    let result = pubgrub::resolve(&provider, root, Version::parse("1.0").unwrap());
    assert!(result.is_ok());
}

#[test]
fn installed_favored_picks_installed_version() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.1.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
        None,
        None,
        PackageDeps {
            depend: (DepEntry::parse(">=dev-libs/openssl-3.0.0").unwrap()).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let mut provider = PortageDependencyProvider::new(repo);
    let openssl = PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").unwrap());
    provider.add_installed(InstalledPackage {
        package: openssl,
        version: Version::parse("3.0.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
    let solution = provider
        .resolve_targets(vec![(myapp, PortageVersionSet::any())])
        .unwrap();
    assert_eq!(
        solution.get(&PortagePackage::unslotted(
            Cpn::parse("dev-libs/openssl").unwrap()
        )),
        Some(&Version::parse("3.0.0").unwrap()),
        "should pick favored installed version 3.0.0 over 3.1.0"
    );
}

// Regression for the `@system` version-choice-ordering bug (see
// `todo/pubgrub-version-choice-heuristic.md`): a dependency with *fewer*
// in-range versions than a root target must not get decided (and committed
// to its installed version) before that root target's own dependency
// bound is even examined.
//
// `sys-libs/libacl` (2 versions, not a root target, installed at the older
// one) is under-constrained by `leader` (a bare, unversioned dep) but
// strictly constrained by `syncer`'s newest version (`>=libacl-2.0`).
// `syncer` has more in-range versions (3) than `libacl` (2), so plain
// minimum-remaining-values would decide `libacl` first and lock it to the
// installed `1.0` — silently holding both `libacl` and `syncer` back a
// version, exactly like `virtual/acl`/`net-misc/rsync` in the real bug.
// Both `leader` and `syncer` are root targets (`resolve_targets`); `libacl`
// is not.
#[test]
fn root_target_priority_avoids_premature_dependency_commitment() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("sys-libs/libacl-1.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("sys-libs/libacl-2.0").unwrap(),
        None,
        None,
        empty_deps(),
    );

    repo.add_version(
        portage_atom::Cpv::parse("sys-apps/leader-1.0").unwrap(),
        None,
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("sys-libs/libacl").unwrap()).into(),
            ..empty_deps()
        },
    );

    repo.add_version(
        portage_atom::Cpv::parse("net-misc/syncer-3.3.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("net-misc/syncer-3.4.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("net-misc/syncer-3.5.0").unwrap(),
        None,
        None,
        PackageDeps {
            rdepend: (DepEntry::parse(">=sys-libs/libacl-2.0").unwrap()).into(),
            ..empty_deps()
        },
    );

    let mut provider = PortageDependencyProvider::new(repo);
    let libacl = PortagePackage::unslotted(Cpn::parse("sys-libs/libacl").unwrap());
    provider.add_installed(InstalledPackage {
        package: libacl.clone(),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let leader = PortagePackage::unslotted(Cpn::parse("sys-apps/leader").unwrap());
    let syncer = PortagePackage::unslotted(Cpn::parse("net-misc/syncer").unwrap());
    let solution = provider
        .resolve_targets(vec![
            (leader, PortageVersionSet::any()),
            (syncer.clone(), PortageVersionSet::any()),
        ])
        .unwrap();

    assert_eq!(
        solution.get(&syncer),
        Some(&Version::parse("3.5.0").unwrap()),
        "root target syncer should get the newest version, not be held back \
         by a dependency the solver committed to prematurely"
    );
    assert_eq!(
        solution.get(&libacl),
        Some(&Version::parse("2.0").unwrap()),
        "libacl should upgrade to satisfy syncer's real requirement instead \
         of staying pinned at the installed version"
    );
}

// `-uD` / `prefer_update`: transitive deps upgrade in-slot even when the
// installed version still satisfies the atom (emerge deep update).
#[test]
fn prefer_update_upgrades_transitive_in_slot() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.1.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
        None,
        None,
        PackageDeps {
            rdepend: (DepEntry::parse(">=dev-libs/openssl-3.0.0").unwrap()).into(),
            ..empty_deps()
        },
    );

    let mut provider = PortageDependencyProvider::new(repo);
    provider.set_prefer_update(true);
    let openssl = PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").unwrap());
    provider.add_installed(InstalledPackage {
        package: openssl,
        version: Version::parse("3.0.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
    let solution = provider
        .resolve_targets(vec![(myapp, PortageVersionSet::any())])
        .unwrap();
    assert_eq!(
        solution.get(&PortagePackage::unslotted(
            Cpn::parse("dev-libs/openssl").unwrap()
        )),
        Some(&Version::parse("3.1.0").unwrap()),
        "prefer_update should pick newest in-slot over satisfying installed"
    );
}

// Flag off: same graph keeps the installed transitive dep (Favor contract).
#[test]
fn prefer_update_off_keeps_transitive() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.1.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
        None,
        None,
        PackageDeps {
            rdepend: (DepEntry::parse(">=dev-libs/openssl-3.0.0").unwrap()).into(),
            ..empty_deps()
        },
    );

    let mut provider = PortageDependencyProvider::new(repo);
    // prefer_update left false
    let openssl = PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").unwrap());
    provider.add_installed(InstalledPackage {
        package: openssl,
        version: Version::parse("3.0.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
    let solution = provider
        .resolve_targets(vec![(myapp, PortageVersionSet::any())])
        .unwrap();
    assert_eq!(
        solution.get(&PortagePackage::unslotted(
            Cpn::parse("dev-libs/openssl").unwrap()
        )),
        Some(&Version::parse("3.0.0").unwrap()),
        "without prefer_update, Favor keeps the installed transitive dep"
    );
}

// `--noreplace`/`-N`/`-U` without `-u`: a *root target* whose installed
// version still satisfies the atom keeps that version. Without the flag the
// same target takes the newest accepted one (emerge's reinstall default).
#[test]
fn selective_no_update_keeps_an_installed_root_target() {
    let build = |selective: bool| {
        let mut repo = InMemoryRepository::new();
        for v in ["3.0.0", "3.1.0"] {
            repo.add_version(
                portage_atom::Cpv::parse(&format!("dev-libs/openssl-{v}"))
                    .expect("openssl cpv parses"),
                None,
                None,
                empty_deps(),
            );
        }
        let mut provider = PortageDependencyProvider::new(repo);
        provider.set_selective_no_update(selective);
        let openssl =
            PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").expect("openssl cpn parses"));
        provider.add_installed(InstalledPackage {
            package: openssl.clone(),
            version: Version::parse("3.0.0").expect("version parses"),
            policy: InstalledPolicy::Favor,
            active_use: vec![],
            iuse: vec![],
        });
        let solution = provider
            .resolve_targets(vec![(openssl.clone(), PortageVersionSet::any())])
            .expect("solvable");
        solution.get(&openssl).cloned().expect("target is solved")
    };
    assert_eq!(
        build(true),
        Version::parse("3.0.0").expect("version parses")
    );
    assert_eq!(
        build(false),
        Version::parse("3.1.0").expect("version parses")
    );
}

// Installed version pruned from the tree: under prefer_update the transitive
// dep upgrades (without the flag, Favor keeps the stub — see
// `installed_version_removed_from_repo_kept_when_satisfying`).
#[test]
fn prefer_update_revbump_transitive() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("dev-python/b").unwrap()).into(),
            ..empty_deps()
        },
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-python/b-2.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );

    let mut provider = PortageDependencyProvider::new(repo);
    provider.set_prefer_update(true);
    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(
            Cpn::parse("dev-python/b").unwrap(),
            Interned::intern("0"),
        ),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    let b_ver = solution
        .iter()
        .find(|(p, _)| p.cpn().package.as_str() == "b")
        .map(|(_, v)| v.clone());
    assert_eq!(
        b_ver,
        Some(Version::parse("2.0").unwrap()),
        "prefer_update upgrades a transitive dep whose installed cpv left the tree"
    );
}

#[test]
fn installed_favored_upgrades_when_required() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.1.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
        None,
        None,
        PackageDeps {
            depend: (DepEntry::parse(">=dev-libs/openssl-3.1.0").unwrap()).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let mut provider = PortageDependencyProvider::new(repo);
    let openssl = PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").unwrap());
    provider.add_installed(InstalledPackage {
        package: openssl,
        version: Version::parse("3.0.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
    let solution = provider
        .resolve_targets(vec![(myapp, PortageVersionSet::any())])
        .unwrap();
    assert_eq!(
        solution.get(&PortagePackage::unslotted(
            Cpn::parse("dev-libs/openssl").unwrap()
        )),
        Some(&Version::parse("3.1.0").unwrap()),
        "should upgrade from favored 3.0.0 to 3.1.0 when required"
    );
}

#[test]
fn installed_locked_pins_version() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/openssl-3.1.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
        None,
        None,
        PackageDeps {
            depend: (DepEntry::parse(">=dev-libs/openssl-3.0.0").unwrap()).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let mut provider = PortageDependencyProvider::new(repo);
    let openssl = PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").unwrap());
    provider.add_installed(InstalledPackage {
        package: openssl,
        version: Version::parse("3.0.0").unwrap(),
        policy: InstalledPolicy::Lock,
        active_use: vec![],
        iuse: vec![],
    });

    let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
    let solution = provider
        .resolve_targets(vec![(myapp, PortageVersionSet::any())])
        .unwrap();
    assert_eq!(
        solution.get(&PortagePackage::unslotted(
            Cpn::parse("dev-libs/openssl").unwrap()
        )),
        Some(&Version::parse("3.0.0").unwrap()),
        "locked should pin to 3.0.0 even though 3.1.0 exists"
    );
}

#[test]
fn or_group_prefers_installed_alternative() {
    // || ( dev-libs/not-installed dev-libs/installed ) — installed is listed second.
    // Without installed preference the solver picks "not-installed" (higher choice version).
    // With installed preference it should pick "installed".
    let mut repo = InMemoryRepository::new();

    let not_inst = portage_atom::Cpv::parse("dev-libs/not-installed-1.0").unwrap();
    repo.add_version(not_inst, Some(Interned::intern("0")), None, empty_deps());

    let inst = portage_atom::Cpv::parse("dev-libs/installed-1.0").unwrap();
    repo.add_version(
        inst.clone(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );

    let consumer = portage_atom::Cpv::parse("app-misc/consumer-1.0").unwrap();
    repo.add_version(
        consumer,
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            depend: (DepEntry::parse("|| ( dev-libs/not-installed dev-libs/installed )").unwrap())
                .into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };

    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(
            Cpn::parse("dev-libs/installed").unwrap(),
            Interned::intern("0"),
        ),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let consumer_pkg = PortagePackage::slotted(
        Cpn::parse("app-misc/consumer").unwrap(),
        Interned::intern("0"),
    );
    let solution = provider
        .resolve_targets(vec![(consumer_pkg, PortageVersionSet::any())])
        .unwrap();

    let in_solution = |cpn: &str| {
        let pkg = PortagePackage::slotted(Cpn::parse(cpn).unwrap(), Interned::intern("0"));
        solution.get(&pkg).is_some()
    };

    assert!(
        in_solution("dev-libs/installed"),
        "installed package should be chosen"
    );
    assert!(
        !in_solution("dev-libs/not-installed"),
        "non-installed alternative should not be chosen"
    );
}

// A dropped `||` branch must keep a *multi-slot* sibling as an alternative.
// Regression for glibc's `BDEPEND=|| ( >=sys-devel/gcc-6.2
// >=llvm-runtimes/libgcc-18 )`: gcc is multi-slot (a `SlotChoice` virtual),
// > llvm-runtimes/libgcc is masked for the arch (dropped). The dropped branch
// > was recorded with no alternatives — because sibling collection skipped
// > virtual nodes — so autounmask falsely reported it as needing an unmask.
#[test]
fn dropped_or_branch_keeps_multislot_sibling() {
    let mut repo = InMemoryRepository::new();
    // gcclike: two slots → the `>=…-1` branch becomes a SlotChoice virtual.
    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/gcclike-1.0").unwrap(),
        Some(Interned::intern("1")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/gcclike-2.0").unwrap(),
        Some(Interned::intern("2")),
        None,
        empty_deps(),
    );
    // llvmlike is absent from the repo → the other branch drops.
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/consumer-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            depend: (DepEntry::parse("|| ( >=dev-libs/gcclike-1 dev-libs/llvmlike )").unwrap())
                .into(),
            ..empty_deps()
        },
    );
    let provider = {
        repo.set_use_config(UseConfig::new());
        PortageDependencyProvider::new(repo)
    };

    let dropped = provider
        .dropped_deps()
        .iter()
        .find(|d| d.package.cpn().package.as_str() == "llvmlike")
        .expect("llvmlike should be a dropped dep");
    assert!(
        !dropped.alternatives.is_empty(),
        "the dropped llvmlike branch must keep the multi-slot gcclike sibling \
         as an alternative (so autounmask does not falsely report it)"
    );
}

#[test]
fn or_group_no_preference_when_both_installed() {
    // || ( A B ) where both A and B are installed — solver falls through to
    // highest choice version (A, listed first), same as without installed preference.
    let mut repo = InMemoryRepository::new();

    for cpv in ["dev-libs/a-1.0", "dev-libs/b-1.0"] {
        repo.add_version(
            portage_atom::Cpv::parse(cpv).unwrap(),
            Some(Interned::intern("0")),
            None,
            empty_deps(),
        );
    }

    let consumer = portage_atom::Cpv::parse("app-misc/consumer-1.0").unwrap();
    repo.add_version(
        consumer,
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            depend: (DepEntry::parse("|| ( dev-libs/a dev-libs/b )").unwrap()).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };

    for cpn in ["dev-libs/a", "dev-libs/b"] {
        provider.add_installed(InstalledPackage {
            package: PortagePackage::slotted(Cpn::parse(cpn).unwrap(), Interned::intern("0")),
            version: Version::parse("1.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![],
            iuse: vec![],
        });
    }

    let consumer_pkg = PortagePackage::slotted(
        Cpn::parse("app-misc/consumer").unwrap(),
        Interned::intern("0"),
    );
    let solution = provider
        .resolve_targets(vec![(consumer_pkg, PortageVersionSet::any())])
        .unwrap();

    // With both installed, falls through to highest choice version = a (listed first).
    let in_sol = |cpn: &str| {
        solution
            .get(&PortagePackage::slotted(
                Cpn::parse(cpn).unwrap(),
                Interned::intern("0"),
            ))
            .is_some()
    };
    assert!(
        in_sol("dev-libs/a"),
        "first alternative chosen when both installed"
    );
    assert!(!in_sol("dev-libs/b"));
}

#[test]
fn rebuild_tree_slot_star_prefers_installed_newest_slot() {
    // Native `--emptytree`: every package is Rebuild, but `gcc:*` must still
    // bind to the installed slot — not the oldest repo slot (gcc-11).
    let mut repo = InMemoryRepository::new();

    for (cpv, slot) in [("sys-devel/gcc-11.0", "11"), ("sys-devel/gcc-16.0", "16")] {
        repo.add_version(
            portage_atom::Cpv::parse(cpv).unwrap(),
            Some(Interned::intern(slot)),
            None,
            empty_deps(),
        );
    }

    repo.add_version(
        portage_atom::Cpv::parse("app-misc/consumer-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            depend: (DepEntry::parse("sys-devel/gcc:*").unwrap()).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };
    provider.set_rebuild_tree(true);
    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(
            Cpn::parse("sys-devel/gcc").unwrap(),
            Interned::intern("16"),
        ),
        version: Version::parse("16.0").unwrap(),
        policy: InstalledPolicy::Rebuild,
        active_use: vec![],
        iuse: vec![],
    });

    let consumer_pkg = PortagePackage::slotted(
        Cpn::parse("app-misc/consumer").unwrap(),
        Interned::intern("0"),
    );
    let solution = provider
        .resolve_targets(vec![(consumer_pkg, PortageVersionSet::any())])
        .unwrap();

    assert_eq!(
        solution.get(&PortagePackage::slotted(
            Cpn::parse("sys-devel/gcc").unwrap(),
            Interned::intern("16"),
        )),
        Some(&Version::parse("16.0").unwrap()),
        "rebuild_tree must pick installed gcc:16, not oldest slot 11"
    );
    assert!(
        solution
            .get(&PortagePackage::slotted(
                Cpn::parse("sys-devel/gcc").unwrap(),
                Interned::intern("11"),
            ))
            .is_none(),
        "oldest gcc slot must not be scheduled"
    );
}

#[test]
fn or_group_prefers_installed_with_slot_nesting() {
    // Mirrors the real-world case: || ( >=A-1.0:* >=B-1.0:* ) where A has
    // multiple slots (triggering the choice→slot→pkg two-level nesting) and
    // only B is installed.  The solver should pick B.
    let mut repo = InMemoryRepository::new();

    // A has two slots (1.0 and 2.0) — not installed
    for (cpv, slot) in [("dev-libs/a-1.0", "1"), ("dev-libs/a-2.0", "2")] {
        repo.add_version(
            portage_atom::Cpv::parse(cpv).unwrap(),
            Some(Interned::intern(slot)),
            None,
            empty_deps(),
        );
    }

    // B has a single slot — installed
    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );

    let consumer = portage_atom::Cpv::parse("app-misc/consumer-1.0").unwrap();
    repo.add_version(
        consumer,
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            // slot-star deps trigger push_unslotted_or_choice → slot_* nesting
            depend: (DepEntry::parse("|| ( dev-libs/a:* dev-libs/b:* )").unwrap()).into(),
            rdepend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };

    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(Cpn::parse("dev-libs/b").unwrap(), Interned::intern("0")),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let consumer_pkg = PortagePackage::slotted(
        Cpn::parse("app-misc/consumer").unwrap(),
        Interned::intern("0"),
    );
    let solution = provider
        .resolve_targets(vec![(consumer_pkg, PortageVersionSet::any())])
        .unwrap();

    let b_in_sol = solution
        .get(&PortagePackage::slotted(
            Cpn::parse("dev-libs/b").unwrap(),
            Interned::intern("0"),
        ))
        .is_some();
    let a_in_sol = solution
        .iter()
        .any(|(p, _)| p.cpn().package.as_str() == "a");

    assert!(
        b_in_sol,
        "installed B should be chosen over non-installed A"
    );
    assert!(!a_in_sol, "non-installed A should not appear in solution");
}

#[test]
fn or_group_prefers_branch_satisfying_use_deps() {
    // Mirrors the librsvg BDEPEND case:
    //   || ( ( python:3.14  docutils[python_targets_python3_14(-)] )
    //        ( python:3.13  docutils[python_targets_python3_13(-)] ) )
    // Both python slots are installed.  docutils has python_targets_python3_13
    // enabled but python_targets_python3_14 disabled.
    // Expected: solver picks branch 2 (python:3.13) since its USE dep is satisfied.
    let mut repo = InMemoryRepository::new();

    // python:3.14 — installed
    repo.add_version(
        portage_atom::Cpv::parse("dev-lang/python-3.14.0").unwrap(),
        Some(Interned::intern("3.14")),
        None,
        empty_deps(),
    );
    // python:3.13 — installed
    repo.add_version(
        portage_atom::Cpv::parse("dev-lang/python-3.13.0").unwrap(),
        Some(Interned::intern("3.13")),
        None,
        empty_deps(),
    );
    // docutils — has both python_targets flags in IUSE, only p3.13 enabled
    repo.add_version_with_iuse(
        portage_atom::Cpv::parse("dev-python/docutils-0.21.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        vec![
            Interned::intern("python_targets_python3_13"),
            Interned::intern("python_targets_python3_14"),
        ],
        empty_deps(),
    );

    // consumer has the OR group via an AllOf pair (simplified encoding)
    repo.add_version(
        portage_atom::Cpv::parse("media-libs/librsvg-2.60.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            bdepend: DepEntry::parse(
                "|| ( \
                   ( dev-lang/python:3.14 dev-python/docutils[python_targets_python3_14(-)] ) \
                   ( dev-lang/python:3.13 dev-python/docutils[python_targets_python3_13(-)] ) \
                 )",
            )
            .unwrap()
            .into(),
            depend: (vec![]).into(),
            rdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        let mut p = PortageDependencyProvider::new(repo);
        p.set_with_bdeps(true);
        p
    };

    // Install python:3.14, python:3.13, and docutils with p3.13 active
    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(
            Cpn::parse("dev-lang/python").unwrap(),
            Interned::intern("3.14"),
        ),
        version: Version::parse("3.14.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });
    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(
            Cpn::parse("dev-lang/python").unwrap(),
            Interned::intern("3.13"),
        ),
        version: Version::parse("3.13.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });
    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(
            Cpn::parse("dev-python/docutils").unwrap(),
            Interned::intern("0"),
        ),
        version: Version::parse("0.21.0").unwrap(),
        policy: InstalledPolicy::Favor,
        // Only python3_13 is enabled; python3_14 is in IUSE but disabled
        active_use: vec![Interned::intern("python_targets_python3_13")],
        iuse: vec![
            Interned::intern("python_targets_python3_13"),
            Interned::intern("python_targets_python3_14"),
        ],
    });

    let librsvg = PortagePackage::slotted(
        Cpn::parse("media-libs/librsvg").unwrap(),
        Interned::intern("0"),
    );
    let solution = provider
        .resolve_targets(vec![(librsvg, PortageVersionSet::any())])
        .unwrap();

    let has = |pkg: &str, slot: &str| {
        solution
            .get(&PortagePackage::slotted(
                Cpn::parse(pkg).unwrap(),
                Interned::intern(slot),
            ))
            .is_some()
    };

    assert!(
        has("dev-lang/python", "3.13"),
        "branch 2 (python:3.13) should be chosen since docutils p3.13 USE dep is satisfied"
    );
    assert!(
        !has("dev-lang/python", "3.14"),
        "branch 1 (python:3.14) should not be chosen — docutils p3.14 USE dep is NOT satisfied"
    );
}

#[test]
fn reinstall_deps_detected_for_direct_use_dep_violation() {
    // Package A (newly installed) has a direct RDEPEND on B[flag].
    // B is already installed but with flag disabled → B must be rebuilt (R).
    let mut repo = InMemoryRepository::new();

    repo.add_version_with_iuse(
        portage_atom::Cpv::parse("dev-python/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        vec![Interned::intern("flag")],
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("dev-python/b[flag]").unwrap()).into(),
            depend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };

    // B is installed but flag is disabled
    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(
            Cpn::parse("dev-python/b").unwrap(),
            Interned::intern("0"),
        ),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![], // flag NOT active
        iuse: vec![Interned::intern("flag")],
    });

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    let reinstall = provider.reinstall_deps();
    assert_eq!(reinstall.len(), 1, "B must be flagged for reinstall");
    assert_eq!(reinstall[0].package.cpn().package.as_str(), "b");
}

#[test]
fn reinstall_deps_empty_when_use_dep_satisfied() {
    // Same setup as above, but B is installed with flag enabled → no reinstall.
    let mut repo = InMemoryRepository::new();

    repo.add_version_with_iuse(
        portage_atom::Cpv::parse("dev-python/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        vec![Interned::intern("flag")],
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("dev-python/b[flag]").unwrap()).into(),
            depend: (vec![]).into(),
            bdepend: (vec![]).into(),
            pdepend: (vec![]).into(),
            idepend: (vec![]).into(),
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };

    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(
            Cpn::parse("dev-python/b").unwrap(),
            Interned::intern("0"),
        ),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![Interned::intern("flag")], // flag IS active
        iuse: vec![Interned::intern("flag")],
    });

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    assert!(
        provider.reinstall_deps().is_empty(),
        "no reinstall needed when USE dep is already satisfied"
    );
}

#[test]
fn upgrade_to_resolves_new_versions_deps() {
    // Regression for the "post-solve remap does not re-solve" gap:
    // when a forced rebuild of an installed package is favoured up to a
    //
    // See issue #4.
    // newer repo version, that newer version's dependency closure must be
    // part of the plan.
    //
    // Setup:
    //   - b-1.0 installed (flag off) — the installed version has NO deps.
    //   - b-2.0 in the tree RDEPENDs a brand-new package c (which b-1.0
    //     lacks).
    //   - a-1.0 RDEPENDs b[flag] → b must rebuild → upgrade to b-2.0.
    // Before the fix, the solve used b-1.0's (empty) deps and c never
    // appeared; after the fix the re-solve pins b-2.0 and pulls in c.
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/c-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    // Installed version: no deps.
    repo.add_version_with_iuse(
        portage_atom::Cpv::parse("dev-python/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        vec![Interned::intern("flag")],
        empty_deps(),
    );
    // Newer version: gains an RDEPEND on c.
    repo.add_version_with_iuse(
        portage_atom::Cpv::parse("dev-python/b-2.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        vec![Interned::intern("flag")],
        PackageDeps {
            rdepend: (DepEntry::parse("dev-libs/c").unwrap()).into(),
            ..empty_deps()
        },
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("dev-python/b[flag]").unwrap()).into(),
            ..empty_deps()
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };

    // b is installed at 1.0 with flag disabled → rebuild forced by a.
    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(
            Cpn::parse("dev-python/b").unwrap(),
            Interned::intern("0"),
        ),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![], // flag NOT active
        iuse: vec![Interned::intern("flag")],
    });

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    // b must be upgraded to 2.0 ...
    let b_ver = solution
        .iter()
        .find(|(p, _)| p.cpn().package.as_str() == "b")
        .map(|(_, v)| v.clone());
    assert_eq!(
        b_ver,
        Some(Version::parse("2.0").unwrap()),
        "b should be upgraded to 2.0"
    );
    // ... and 2.0's new dependency c must be in the plan.
    assert!(
        solution
            .iter()
            .any(|(p, _)| p.cpn().package.as_str() == "c"),
        "c (a new dependency of b-2.0) must be pulled into the re-solved plan"
    );
}

#[test]
fn required_use_of_fixed_flags_never_constrains_the_solve() {
    // With no flags ceded, the encoder partially evaluates REQUIRED_USE
    // against the fixed config and emits no constraints — violations are
    // Level A's domain (docs/required-use-level-c.md). Proven two ways:
    // (1) a package whose REQUIRED_USE is unsatisfiable still resolves (no
    //     NoSolution, same version), and
    // (2) the solution is byte-identical to the same repo without the fact.
    use crate::required_use::RequiredUse;

    let build = |with_ru: bool| {
        let mut repo = InMemoryRepository::new();
        let deps = PackageDeps {
            rdepend: (DepEntry::parse("dev-libs/b").unwrap()).into(),
            ..empty_deps()
        };
        // ^^ ( x y ) with both flags off by default → Level-A violation.
        let ru = RequiredUse::ExactlyOne(vec![
            RequiredUse::Flag {
                name: Interned::intern("x"),
                negated: false,
            },
            RequiredUse::Flag {
                name: Interned::intern("y"),
                negated: false,
            },
        ]);
        if with_ru {
            repo.add_version_with_required_use(
                portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
                Some(Interned::intern("0")),
                vec![Interned::intern("x"), Interned::intern("y")],
                deps,
                ru,
            );
        } else {
            repo.add_version_with_iuse(
                portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
                Some(Interned::intern("0")),
                None,
                vec![Interned::intern("x"), Interned::intern("y")],
                deps,
            );
        }
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            empty_deps(),
        );
        let mut provider = {
            repo.set_use_config(UseConfig::new());
            PortageDependencyProvider::new(repo)
        };
        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .expect("unsatisfiable REQUIRED_USE of fixed flags must not break the solve")
    };

    let with_ru: std::collections::BTreeSet<String> = build(true)
        .iter()
        .map(|(p, v)| format!("{p}@{v}"))
        .collect();
    let without_ru: std::collections::BTreeSet<String> = build(false)
        .iter()
        .map(|(p, v)| format!("{p}@{v}"))
        .collect();

    assert!(
        with_ru.iter().any(|s| s.contains("app-misc/a")),
        "a must still be selected despite its unsatisfiable REQUIRED_USE"
    );
    assert_eq!(
        with_ru, without_ru,
        "REQUIRED_USE of fixed flags must not change the solution"
    );
}

#[test]
fn ceded_flag_follows_preference() {
    // A SolverDecided flag with no constraint forcing it should take the
    // caller's preferred value: choose_version biases its UseDecision node.
    // Observable via a conditional dep gated on the flag.
    let build = |prefer: bool| {
        let mut repo = InMemoryRepository::new();
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            empty_deps(),
        );
        repo.add_version_with_iuse(
            portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            vec![Interned::intern("flag")],
            PackageDeps {
                rdepend: (DepEntry::parse("flag? ( dev-libs/b )").unwrap()).into(),
                ..empty_deps()
            },
        );
        let mut cfg = UseConfig::new();
        cfg.solver_decide(Interned::intern("flag"), prefer);
        let mut provider = {
            repo.set_use_config(cfg);
            PortageDependencyProvider::new(repo)
        };
        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap()
    };
    let has_b = |sol: &SelectedDependencies<PortagePackage, Version>| {
        sol.iter().any(|(p, _)| p.cpn().package.as_str() == "b")
    };
    assert!(has_b(&build(true)), "prefer=on must enable flag → pull b");
    assert!(
        !has_b(&build(false)),
        "prefer=off must leave flag off → no b"
    );
}

// ---- Level-C REQUIRED_USE encoding (Phase 1b) ----

// Build `app-misc/a` with the given REQUIRED_USE, ceding x/y/z (preferences
// from `prefer`), where each flag pulls a marker dep `dev-libs/p{flag}` when
// on. Returns the set of marker package names present in the solution.
fn solve_required_use(
    ru: crate::required_use::RequiredUse,
    prefer: &[(&str, bool)],
    fixed: &[(&str, bool)],
) -> std::collections::BTreeSet<String> {
    let mut repo = InMemoryRepository::new();
    for f in ["w", "x", "y", "z"] {
        repo.add_version(
            portage_atom::Cpv::parse(&format!("dev-libs/p{f}-1.0")).unwrap(),
            Some(Interned::intern("0")),
            None,
            empty_deps(),
        );
    }
    repo.add_version_with_required_use(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        vec![
            Interned::intern("w"),
            Interned::intern("x"),
            Interned::intern("y"),
            Interned::intern("z"),
        ],
        PackageDeps {
            rdepend: DepEntry::parse(
                "w? ( dev-libs/pw ) x? ( dev-libs/px ) y? ( dev-libs/py ) z? ( dev-libs/pz )",
            )
            .unwrap()
            .into(),
            ..empty_deps()
        },
        ru,
    );
    let mut cfg = UseConfig::new();
    for (f, p) in prefer {
        cfg.solver_decide(Interned::intern(f), *p);
    }
    for (f, on) in fixed {
        if *on {
            cfg.enable(Interned::intern(f))
        } else {
            cfg.disable(Interned::intern(f))
        }
    }
    let mut provider = {
        repo.set_use_config(cfg);
        PortageDependencyProvider::new(repo)
    };
    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let sol = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();
    sol.iter()
        .filter(|(p, _)| !p.is_virtual() && p.cpn().category.as_str() == "dev-libs")
        .map(|(p, _)| p.cpn().package.as_str().to_string())
        .filter(|n| n.starts_with('p'))
        .collect()
}

fn flag(name: &str, negated: bool) -> crate::required_use::RequiredUse {
    crate::required_use::RequiredUse::Flag {
        name: Interned::intern(name),
        negated,
    }
}

fn cond(
    name: &str,
    negated: bool,
    entries: Vec<crate::required_use::RequiredUse>,
) -> crate::required_use::RequiredUse {
    crate::required_use::RequiredUse::UseConditional {
        flag: Interned::intern(name),
        negated,
        entries,
    }
}

#[test]
fn required_use_exactly_one_picks_one() {
    use crate::required_use::RequiredUse::ExactlyOne;
    // ^^ ( x y ), both ceded off → solver must enable exactly one.
    let got = solve_required_use(
        ExactlyOne(vec![flag("x", false), flag("y", false)]),
        &[("x", false), ("y", false)],
        &[],
    );
    assert_eq!(got.len(), 1, "exactly one marker expected, got {got:?}");
}

#[test]
fn required_use_exactly_one_with_installed_alternative_does_not_overflow() {
    use crate::required_use::RequiredUse::ExactlyOne;
    // Reproduces a 2026-07-12 stack overflow found via `em stages --stage1
    // --root <dir>` on a root with a real, already-built toolchain:
    // app-alternatives/tar's `^^ ( gnu libarchive )` crashed only when
    // app-arch/libarchive was already installed in that root under
    // --autosolve-use; the identical atom against a fresh/empty root
    // resolved fine. Minimal shape: `^^ ( w x )`, both ceded off, where
    // "w"'s own dependency is already installed.
    let mut repo = InMemoryRepository::new();
    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/pw-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-libs/px-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version_with_required_use(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        vec![Interned::intern("w"), Interned::intern("x")],
        PackageDeps {
            rdepend: DepEntry::parse("w? ( dev-libs/pw ) x? ( dev-libs/px )")
                .unwrap()
                .into(),
            ..empty_deps()
        },
        ExactlyOne(vec![flag("w", false), flag("x", false)]),
    );
    let mut cfg = UseConfig::new();
    cfg.solver_decide(Interned::intern("w"), false);
    cfg.solver_decide(Interned::intern("x"), false);
    repo.set_use_config(cfg);
    let mut provider = PortageDependencyProvider::new(repo);
    provider.add_installed(InstalledPackage {
        package: PortagePackage::unslotted(Cpn::parse("dev-libs/pw").unwrap()),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });
    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let _sol = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();
}

#[test]
fn required_use_any_of_enables_at_least_one() {
    use crate::required_use::RequiredUse::AnyOf;
    // || ( x y ), both ceded off → at least one on.
    let got = solve_required_use(
        AnyOf(vec![flag("x", false), flag("y", false)]),
        &[("x", false), ("y", false)],
        &[],
    );
    assert!(!got.is_empty(), "at least one marker expected");
}

#[test]
fn required_use_at_most_one_caps_preferences() {
    use crate::required_use::RequiredUse::AtMostOne;
    // ?? ( x y ), both ceded ON → at most one may stay on.
    let got = solve_required_use(
        AtMostOne(vec![flag("x", false), flag("y", false)]),
        &[("x", true), ("y", true)],
        &[],
    );
    assert!(got.len() <= 1, "at most one marker allowed, got {got:?}");
}

#[test]
fn required_use_conditional_forces_consequent() {
    use crate::required_use::RequiredUse::UseConditional;
    // x? ( y ): x ceded ON (pref) ⇒ y must be on; y prefers OFF but is forced.
    let got = solve_required_use(
        UseConditional {
            flag: Interned::intern("x"),
            negated: false,
            entries: vec![flag("y", false)],
        },
        &[("x", true), ("y", false)],
        &[],
    );
    assert!(got.contains("px"), "x on");
    assert!(got.contains("py"), "y forced on by x? ( y )");
}

#[test]
fn required_use_exactly_one_with_fixed_on_disables_rest() {
    use crate::required_use::RequiredUse::ExactlyOne;
    // ^^ ( x y ): x fixed ON, y ceded (prefers on) → y must be off.
    let got = solve_required_use(
        ExactlyOne(vec![flag("x", false), flag("y", false)]),
        &[("y", true)],
        &[("x", true)],
    );
    assert!(got.contains("px"), "x is the fixed-on choice");
    assert!(
        !got.contains("py"),
        "y must be disabled by ^^ with x fixed on"
    );
}

#[test]
fn required_use_preference_kept_when_unconstrained() {
    use crate::required_use::RequiredUse::AnyOf;
    // || ( x y ) with x preferring ON: the at-least-one is already met by x,
    // y stays at its preferred OFF (no gratuitous flip).
    let got = solve_required_use(
        AnyOf(vec![flag("x", false), flag("y", false)]),
        &[("x", true), ("y", false)],
        &[],
    );
    assert!(got.contains("px"));
    assert!(!got.contains("py"), "y should keep its preferred off");
}

#[test]
fn required_use_exactly_one_keeps_preferred_not_first() {
    use crate::required_use::RequiredUse::ExactlyOne;
    // ^^ ( x y ) with the *second*-listed flag (y) preferred on and already
    // satisfying the group: the solver must keep y, not gratuitously flip to
    // the first-listed x. Guards against choice branches ignoring preference.
    let got = solve_required_use(
        ExactlyOne(vec![flag("x", false), flag("y", false)]),
        &[("x", false), ("y", true)],
        &[],
    );
    assert!(got.contains("py"), "preferred y kept, got {got:?}");
    assert!(
        !got.contains("px"),
        "x not gratuitously enabled, got {got:?}"
    );
}

#[test]
fn required_use_any_of_keeps_preferred_no_extra() {
    use crate::required_use::RequiredUse::AnyOf;
    // || ( x y z ) with only z (last) preferred on: the at-least-one is met,
    // no other flag should be flipped on (the python_targets blowup case).
    let got = solve_required_use(
        AnyOf(vec![flag("x", false), flag("y", false), flag("z", false)]),
        &[("x", false), ("y", false), ("z", true)],
        &[],
    );
    assert!(got.contains("pz"), "preferred z kept");
    assert!(
        !got.contains("px") && !got.contains("py"),
        "no extra flips, got {got:?}"
    );
}

#[test]
fn required_use_nested_exactly_one_under_guard() {
    use crate::required_use::RequiredUse::{ExactlyOne, UseConditional};
    // x? ( ^^ ( y z ) ): x ceded ON, y/z ceded OFF → x stays on and exactly
    // one of y/z is enabled by the nested group.
    let got = solve_required_use(
        UseConditional {
            flag: Interned::intern("x"),
            negated: false,
            entries: vec![ExactlyOne(vec![flag("y", false), flag("z", false)])],
        },
        &[("x", true), ("y", false), ("z", false)],
        &[],
    );
    assert!(got.contains("px"), "x kept on");
    let yz = got.iter().filter(|n| *n == "py" || *n == "pz").count();
    assert_eq!(yz, 1, "exactly one of y/z under the guard, got {got:?}");
}

#[test]
fn required_use_nested_group_inert_when_guard_off() {
    use crate::required_use::RequiredUse::{ExactlyOne, UseConditional};
    // x? ( ^^ ( y z ) ): x ceded OFF (preferred) → the nested ^^ never fires,
    // so y/z keep their preferred off (no gratuitous enable).
    let got = solve_required_use(
        UseConditional {
            flag: Interned::intern("x"),
            negated: false,
            entries: vec![ExactlyOne(vec![flag("y", false), flag("z", false)])],
        },
        &[("x", false), ("y", false), ("z", false)],
        &[],
    );
    assert!(got.is_empty(), "guard off ⇒ nothing forced, got {got:?}");
}

#[test]
fn required_use_nested_conditional_fixed_inner_guard() {
    use crate::required_use::RequiredUse::UseConditional;
    // x? ( y? ( z ) ): x ceded ON, y *fixed* ON (not ceded), z prefers OFF →
    // the inner guard collapses to a constant and z is forced on.
    let got = solve_required_use(
        UseConditional {
            flag: Interned::intern("x"),
            negated: false,
            entries: vec![UseConditional {
                flag: Interned::intern("y"),
                negated: false,
                entries: vec![flag("z", false)],
            }],
        },
        &[("x", true), ("z", false)],
        &[("y", true)],
    );
    assert!(got.contains("px") && got.contains("py"), "x,y on");
    assert!(got.contains("pz"), "z forced on by x? ( y(fixed)? ( z ) )");
}

#[test]
fn required_use_doubly_ceded_chain_forces_consequent() {
    // x? ( y? ( z ) ) with BOTH x and y ceded ON: the clause encoding
    // (¬x ∨ ¬y ∨ z) must fire, and the body-first branch order prefers
    // enabling the consequent over flipping a user-configured guard.
    let got = solve_required_use(
        cond("x", false, vec![cond("y", false, vec![flag("z", false)])]),
        &[("x", true), ("y", true), ("z", false)],
        &[],
    );
    assert!(got.contains("px") && got.contains("py"), "guards kept on");
    assert!(got.contains("pz"), "z forced on by x? ( y? ( z ) )");
}

#[test]
fn required_use_doubly_ceded_chain_inactive_guard_no_flip() {
    // x? ( y? ( z ) ) with y preferring OFF: the clause is already met by
    // the ¬y escape, so nothing is flipped (z stays off).
    let got = solve_required_use(
        cond("x", false, vec![cond("y", false, vec![flag("z", false)])]),
        &[("x", true), ("y", false), ("z", false)],
        &[],
    );
    assert!(got.contains("px"), "x kept on");
    assert!(
        !got.contains("py") && !got.contains("pz"),
        "no flips: {got:?}"
    );
}

#[test]
fn required_use_chain_negated_inner_guard() {
    // x? ( !y? ( z ) ) with x on, y OFF (so the inner guard is active):
    // clause ¬x ∨ y ∨ z; body-first ⇒ z forced on, y stays off.
    let got = solve_required_use(
        cond("x", false, vec![cond("y", true, vec![flag("z", false)])]),
        &[("x", true), ("y", false), ("z", false)],
        &[],
    );
    assert!(got.contains("px"), "x kept on");
    assert!(!got.contains("py"), "y not gratuitously enabled");
    assert!(got.contains("pz"), "z forced on by x? ( !y? ( z ) )");
}

#[test]
fn required_use_triple_ceded_chain() {
    // w? ( x? ( y? ( z ) ) ), all guards ceded ON: depth-3 chain is one
    // 4-literal clause; z is forced on.
    let got = solve_required_use(
        cond(
            "w",
            false,
            vec![cond(
                "x",
                false,
                vec![cond("y", false, vec![flag("z", false)])],
            )],
        ),
        &[("w", true), ("x", true), ("y", true), ("z", false)],
        &[],
    );
    assert!(
        got.contains("pw") && got.contains("px") && got.contains("py"),
        "guards kept on: {got:?}"
    );
    assert!(got.contains("pz"), "z forced on by the depth-3 chain");
}

#[test]
fn required_use_chain_fixed_false_body_escapes_guard() {
    // x? ( y? ( z ) ) with z FIXED off: unsatisfiable body ⇒ one guard
    // must flip off (the escape clause ¬x ∨ ¬y), the other stays on.
    let got = solve_required_use(
        cond("x", false, vec![cond("y", false, vec![flag("z", false)])]),
        &[("x", true), ("y", true)],
        &[("z", false)],
    );
    assert!(!got.contains("pz"), "z is fixed off");
    let guards = got.iter().filter(|n| *n == "px" || *n == "py").count();
    assert_eq!(guards, 1, "exactly one guard escapes, got {got:?}");
}

#[test]
fn required_use_any_of_under_ceded_chain() {
    // x? ( y? ( || ( w z ) ) ), guards ceded ON, w/z OFF: one clause
    // ¬x ∨ ¬y ∨ w ∨ z; at least one of w/z comes on, guards stay on.
    let got = solve_required_use(
        cond(
            "x",
            false,
            vec![cond(
                "y",
                false,
                vec![crate::required_use::RequiredUse::AnyOf(vec![
                    flag("w", false),
                    flag("z", false),
                ])],
            )],
        ),
        &[("w", false), ("x", true), ("y", true), ("z", false)],
        &[],
    );
    assert!(got.contains("px") && got.contains("py"), "guards kept on");
    let wz = got.iter().filter(|n| *n == "pw" || *n == "pz").count();
    assert!(wz >= 1, "at least one of w/z under the chain, got {got:?}");
}

#[test]
fn required_use_at_most_one_under_ceded_chain() {
    // x? ( y? ( ?? ( w z ) ) ), guards ON, w/z both ON: pairwise clause
    // ¬x ∨ ¬y ∨ ¬w ∨ ¬z; at most one of w/z survives, guards stay on.
    let got = solve_required_use(
        cond(
            "x",
            false,
            vec![cond(
                "y",
                false,
                vec![crate::required_use::RequiredUse::AtMostOne(vec![
                    flag("w", false),
                    flag("z", false),
                ])],
            )],
        ),
        &[("w", true), ("x", true), ("y", true), ("z", true)],
        &[],
    );
    assert!(got.contains("px") && got.contains("py"), "guards kept on");
    let wz = got.iter().filter(|n| *n == "pw" || *n == "pz").count();
    assert!(wz <= 1, "at most one of w/z under the chain, got {got:?}");
}

#[test]
fn required_use_nested_at_most_one_under_guard() {
    use crate::required_use::RequiredUse::{AtMostOne, UseConditional};
    // x? ( ?? ( y z ) ): x ceded ON, y/z both ceded ON → at most one of y/z
    // may stay on while the guard is active.
    let got = solve_required_use(
        UseConditional {
            flag: Interned::intern("x"),
            negated: false,
            entries: vec![AtMostOne(vec![flag("y", false), flag("z", false)])],
        },
        &[("x", true), ("y", true), ("z", true)],
        &[],
    );
    assert!(got.contains("px"), "x kept on");
    let yz = got.iter().filter(|n| *n == "py" || *n == "pz").count();
    assert!(yz <= 1, "at most one of y/z under the guard, got {got:?}");
}

// ---- Characterization: autounmask "needed" USE detection ----
//
// These pin the observable behaviour that the `desired_use` concern
// extraction (step 3) must preserve: a flag is reported as needed only when
// it is NOT already provided — whether "provided" comes from the ebuild's
// IUSE default or from the global USE config.  When step 3 moves policy
// resolution behind `PackageRepository::desired_use`, the *setup* here will
// change (the caller will fold IUSE defaults / config into the desired set),
// but the assertions — needed vs not-needed — must stay identical.

// `a` RDEPENDs `b[flag]`; `flag` is off everywhere → `b` needs it enabled.
#[test]
fn use_flag_needed_when_flag_off() {
    let mut repo = InMemoryRepository::new();
    repo.add_version_with_iuse(
        portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        vec![Interned::intern("flag")],
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("dev-libs/b[flag]").unwrap()).into(),
            ..empty_deps()
        },
    );
    let mut provider = PortageDependencyProvider::new(repo);
    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    let b = provider
        .use_flag_requirements()
        .iter()
        .find(|r| r.package.cpn().package.as_str() == "b")
        .expect("b should have a USE requirement");
    assert!(b.required_enabled.contains(&Interned::intern("flag")));
}

// Same, but `b` carries `+flag` as an IUSE default → already on, none needed.
#[test]
fn use_flag_not_needed_when_iuse_default_on() {
    let mut repo = InMemoryRepository::new();
    let mut defaults = HashMap::new();
    defaults.insert(Interned::intern("flag"), IUseDefault::Enabled);
    repo.add_package_versions(
        portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
        PackageVersions {
            slot: Some(Interned::intern("0")),
            subslot: None,
            repo: None,
            iuse: vec![Interned::intern("flag")],
            iuse_defaults: defaults,
            deps: empty_deps(),
            required_use: None,
            empty_any_of_matches: true,
            needs_unmask: false,
            live: false,
        },
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("dev-libs/b[flag]").unwrap()).into(),
            ..empty_deps()
        },
    );
    let mut provider = PortageDependencyProvider::new(repo);
    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    assert!(
        provider
            .use_flag_requirements()
            .iter()
            .all(|r| r.required_enabled.is_empty()),
        "IUSE +flag default already satisfies b[flag]; no autounmask needed"
    );
}

// Same, but the global config already enables `flag` → none needed.
#[test]
fn use_flag_not_needed_when_config_enables() {
    let mut repo = InMemoryRepository::new();
    repo.add_version_with_iuse(
        portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        vec![Interned::intern("flag")],
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("dev-libs/b[flag]").unwrap()).into(),
            ..empty_deps()
        },
    );
    let mut config = UseConfig::new();
    config.enable(Interned::intern("flag"));
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };
    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    assert!(
        provider
            .use_flag_requirements()
            .iter()
            .all(|r| r.required_enabled.is_empty()),
        "global config already enables flag; no autounmask needed"
    );
}

#[test]
fn packages_for_cpn_excludes_virtual_choice_nodes() {
    // Multi-slot packages cause register_virtual_choices to insert Choice
    // nodes into self.packages. packages_for_cpn must skip those nodes
    // rather than calling cpn() on them (which panics).
    let mut repo = InMemoryRepository::new();
    repo.add_version(
        portage_atom::Cpv::parse("dev-lang/python-3.11.9").unwrap(),
        Some(Interned::intern("3.11")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-lang/python-3.12.4").unwrap(),
        Some(Interned::intern("3.12")),
        None,
        empty_deps(),
    );

    let provider = PortageDependencyProvider::new(repo);
    let cpn = Cpn::parse("dev-lang/python").unwrap();
    let pkgs = provider.packages_for_cpn(&cpn);

    assert_eq!(pkgs.len(), 2, "expected one entry per slot");
    assert!(pkgs.iter().all(|p| !p.is_virtual()), "no virtual nodes");
    assert!(
        pkgs.iter()
            .any(|p| p.slot() == Some(Interned::intern("3.11"))),
        "slot 3.11 present"
    );
    assert!(
        pkgs.iter()
            .any(|p| p.slot() == Some(Interned::intern("3.12"))),
        "slot 3.12 present"
    );
}
#[test]
fn use_dep_from_new_parent_on_installed_target_built_without_flag() {
    // The distlib case: a NEW parent version BDEPENDs `b[flag]`; b is
    // installed at a version whose BUILD lacked `flag`, but the global
    // config has `flag` on (so a naive desired-config check looks
    // satisfied). The requirement must still be raised (rebuild b).
    let mut repo = InMemoryRepository::new();
    repo.add_version_with_iuse(
        portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        vec![Interned::intern("flag")],
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version_with_iuse(
        portage_atom::Cpv::parse("app-misc/a-2.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        vec![Interned::intern("flag")],
        PackageDeps {
            bdepend: (DepEntry::parse("dev-libs/b[flag(-)?]").unwrap()).into(),
            ..empty_deps()
        },
    );
    let mut cfg = UseConfig::new();
    cfg.enable(Interned::intern("flag"));
    let mut provider = {
        repo.set_use_config(cfg);
        PortageDependencyProvider::new(repo)
    };
    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let b = PortagePackage::slotted(Cpn::parse("dev-libs/b").unwrap(), Interned::intern("0"));
    provider.add_installed(InstalledPackage {
        package: a.clone(),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });
    provider.add_installed(InstalledPackage {
        package: b.clone(),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![], // built WITHOUT `flag`
        iuse: vec![Interned::intern("flag")],
    });
    let sol = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();
    assert!(
        sol.iter()
            .any(|(p, v)| p.cpn().package.as_str() == "a" && v == &Version::parse("2.0").unwrap())
    );
    let req = provider
        .use_flag_requirements()
        .iter()
        .find(|r| r.package.cpn().package.as_str() == "b")
        .cloned();
    let req = req.expect("b[flag] from the new parent must raise a requirement");
    assert!(req.required_enabled.contains(&Interned::intern("flag")));
}

// Same-slot update where the installed version was *removed from the
// repo* and a newer version in the same slot is available, with no USE
// violation to trigger the upgrade path. Mirrors `dev-lang/python:3.13`
// installed at an old 3.13.x that's been dropped from the tree, with a
// newer 3.13.y present. The resolver must select the newer version.
#[test]
fn installed_version_removed_from_repo_upgrades_in_slot() {
    let mut repo = InMemoryRepository::new();

    // Only the newer version exists in the tree; the installed version
    // (1.0) is deliberately NOT registered here.
    repo.add_version(
        portage_atom::Cpv::parse("dev-python/b-2.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };

    // Installed at 1.0 (absent from the repo above), same slot 0.
    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(
            Cpn::parse("dev-python/b").unwrap(),
            Interned::intern("0"),
        ),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let b = PortagePackage::slotted(Cpn::parse("dev-python/b").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(b, PortageVersionSet::any())])
        .unwrap();

    let b_ver = solution
        .iter()
        .find(|(p, _)| p.cpn().package.as_str() == "b")
        .map(|(_, v)| v.clone());
    assert_eq!(
        b_ver,
        Some(Version::parse("2.0").unwrap()),
        "an installed version removed from the repo must upgrade to the newer in-slot version"
    );
}

// Same scenario as above, but `b` is reached *transitively* (not a root
// target). Under `Favor` (no `--update`/`--deep`) emerge keeps the
// installed version even when its exact cpv was pruned from the tree (e.g.
// a revbump `4.3.3` -> `4.3.3-r1` superseding the installed build): it
// satisfies the plain dep, and a revbump is not pulled without `--update`.
// The empty-deps installed stub is fine since the package is satisfying a
// dep, not being rebuilt.
#[test]
fn installed_version_removed_from_repo_kept_when_satisfying() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("dev-python/b").unwrap()).into(),
            ..empty_deps()
        },
    );
    // Only the newer version exists in the tree; the installed version
    // (1.0) is deliberately NOT registered here.
    repo.add_version(
        portage_atom::Cpv::parse("dev-python/b-2.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };

    // b installed at 1.0 (absent from the repo), reached only via a.
    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(
            Cpn::parse("dev-python/b").unwrap(),
            Interned::intern("0"),
        ),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    let b_ver = solution
        .iter()
        .find(|(p, _)| p.cpn().package.as_str() == "b")
        .map(|(_, v)| v.clone());
    assert_eq!(
        b_ver,
        Some(Version::parse("1.0").unwrap()),
        "transitive installed dep whose version was removed from the repo \
         must be kept under Favor when it satisfies the dep (no --update)"
    );
}

// `host_installed` (BROOT) satisfies BDEPEND: a package being built whose
// BDEPEND is already present on the host must not pull that build tool into
// the plan. Mirrors portage — `em --root <empty> a` doesn't build host-gcc.
// Per-edge: a package that is *also* an RDEPEND is still pulled (next test).
#[test]
fn host_installed_satisfies_bdepend() {
    let mut repo = InMemoryRepository::new();
    // b is a pure build tool (BDEPEND of a), present on the host.
    repo.add_version(
        portage_atom::Cpv::parse("dev-build/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            bdepend: (DepEntry::parse("dev-build/b").unwrap()).into(),
            ..empty_deps()
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        let mut p = PortageDependencyProvider::new(repo);
        p.set_with_bdeps(true);
        p
    };
    // b-1.0 is present on BROOT (the host).
    provider.add_host_installed(
        PortagePackage::slotted(Cpn::parse("dev-build/b").unwrap(), Interned::intern("0")),
        Version::parse("1.0").unwrap(),
        vec![],
        vec![],
    );

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    assert!(
        solution
            .iter()
            .all(|(p, _)| p.cpn().package.as_str() != "b"),
        "b is satisfied by host BROOT and must not be built into the plan"
    );
    assert!(
        solution
            .iter()
            .any(|(p, _)| p.cpn().package.as_str() == "a")
    );
}

// `-uD` / `prefer_update`: host-satisfied BDEPEND still enters the graph so
// an in-slot newer version can be selected (emerge deep-update of build tools).
#[test]
fn prefer_update_upgrades_host_satisfied_bdepend() {
    let mut repo = InMemoryRepository::new();
    repo.add_version(
        portage_atom::Cpv::parse("dev-build/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-build/b-2.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            bdepend: (DepEntry::parse("dev-build/b").unwrap()).into(),
            ..empty_deps()
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        let mut p = PortageDependencyProvider::new(repo);
        p.set_with_bdeps(true);
        p.set_prefer_update(true);
        p
    };
    let b_pkg = PortagePackage::slotted(Cpn::parse("dev-build/b").unwrap(), Interned::intern("0"));
    // Host has 1.0; target VDB also has 1.0 as Favor (typical native).
    provider.add_host_installed(
        b_pkg.clone(),
        Version::parse("1.0").unwrap(),
        vec![],
        vec![],
    );
    provider.add_installed(InstalledPackage {
        package: b_pkg.clone(),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    assert_eq!(
        solution.get(&b_pkg),
        Some(&Version::parse("2.0").unwrap()),
        "prefer_update must upgrade host-satisfied BDEPEND in-slot"
    );
}

// Per-edge BDEPEND filtering: when `b` is *both* a's BDEPEND (host-provided)
// and c's RDEPEND, the host satisfies the build edge but c still needs b at
// runtime — so b must be built. Confirms filtering is edge-class-scoped.
#[test]
fn bdepend_filtering_is_per_edge_not_per_package() {
    let mut repo = InMemoryRepository::new();
    repo.add_version(
        portage_atom::Cpv::parse("dev-build/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    // a BDEPENDs b; c RDEPENDs b.
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            bdepend: (DepEntry::parse("dev-build/b").unwrap()).into(),
            ..empty_deps()
        },
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/c-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("dev-build/b").unwrap()).into(),
            ..empty_deps()
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };
    provider.add_host_installed(
        PortagePackage::slotted(Cpn::parse("dev-build/b").unwrap(), Interned::intern("0")),
        Version::parse("1.0").unwrap(),
        vec![],
        vec![],
    );

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let c = PortagePackage::slotted(Cpn::parse("app-misc/c").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![
            (a, PortageVersionSet::any()),
            (c, PortageVersionSet::any()),
        ])
        .unwrap();

    // c's runtime need pulls b even though a's build edge was host-satisfied.
    assert!(
        solution
            .iter()
            .any(|(p, _)| p.cpn().package.as_str() == "b"),
        "b is c's RDEPEND, so it must be built despite a's BDEPEND being host-satisfied"
    );
}

// Native offset / host: host-satisfied `IDEPEND` (BROOT) must not enter the plan.
#[test]
fn host_installed_satisfies_native_idepend() {
    let mut repo = InMemoryRepository::new();
    repo.add_version(
        portage_atom::Cpv::parse("sys-apps/locale-gen-1.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("sys-libs/glibc-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            idepend: (DepEntry::parse("sys-apps/locale-gen").unwrap()).into(),
            ..empty_deps()
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };
    provider.add_host_installed(
        PortagePackage::unslotted(Cpn::parse("sys-apps/locale-gen").unwrap()),
        Version::parse("1.0").unwrap(),
        vec![],
        vec![],
    );

    let glibc =
        PortagePackage::slotted(Cpn::parse("sys-libs/glibc").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(glibc, PortageVersionSet::any())])
        .unwrap();

    assert!(
        solution
            .iter()
            .all(|(p, _)| p.cpn().package.as_str() != "locale-gen"),
        "locale-gen is satisfied on BROOT and must not be built into the native plan"
    );
}

// Cross target build: host-satisfied `IDEPEND` (BROOT) must not enter the plan.
// Mirrors glibc `!compile-locales? ( sys-apps/locale-gen )` when locale-gen
// is already installed on the build host.
#[test]
fn host_installed_satisfies_cross_idepend() {
    let mut repo = InMemoryRepository::new();
    repo.add_version(
        portage_atom::Cpv::parse("sys-apps/locale-gen-1.0").unwrap(),
        None,
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("sys-libs/glibc-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            idepend: (DepEntry::parse("sys-apps/locale-gen").unwrap()).into(),
            ..empty_deps()
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        PortageDependencyProvider::new(repo)
    };
    provider.set_cross_active(true);
    provider.set_is_cross_arch(true);
    provider.add_host_installed(
        PortagePackage::unslotted(Cpn::parse("sys-apps/locale-gen").unwrap()),
        Version::parse("1.0").unwrap(),
        vec![],
        vec![],
    );

    let glibc =
        PortagePackage::slotted(Cpn::parse("sys-libs/glibc").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(glibc, PortageVersionSet::any())])
        .unwrap();

    assert!(
        solution
            .iter()
            .all(|(p, _)| p.cpn().package.as_str() != "locale-gen"),
        "locale-gen is satisfied on BROOT and must not be built into the cross plan"
    );
    assert!(
        solution
            .iter()
            .any(|(p, _)| p.cpn().package.as_str() == "glibc")
    );
}

// `--root-deps=rdeps` (crossdev cross builds): a target package's `DEPEND`
// (build-only) is discarded from the sysroot graph, while `RDEPEND` still
// installs into the sysroot. Mirrors crossdev's `<CTARGET>-emerge
// --root-deps=rdeps`, where build deps resolve on the host toolchain and
// only runtime libraries land in the target ROOT.
#[test]
fn root_deps_rdeps_drops_target_depend() {
    let slot0 = Interned::intern("0");
    let mut repo = InMemoryRepository::new();
    // A build-only dependency (DEPEND, absent from RDEPEND).
    repo.add_version(
        portage_atom::Cpv::parse("dev-build/buildtool-1.0").unwrap(),
        Some(slot0),
        None,
        empty_deps(),
    );
    // A runtime library (RDEPEND).
    repo.add_version(
        portage_atom::Cpv::parse("sys-libs/runlib-1.0").unwrap(),
        Some(slot0),
        None,
        empty_deps(),
    );
    // The target leaf: DEPEND on the build tool, RDEPEND on the runtime lib.
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/leaf-1.0").unwrap(),
        Some(slot0),
        None,
        PackageDeps {
            depend: (DepEntry::parse("dev-build/buildtool").unwrap()).into(),
            rdepend: (DepEntry::parse("sys-libs/runlib").unwrap()).into(),
            ..empty_deps()
        },
    );

    // Cross solve at the two `--root-deps` policies.
    let solve = |rdeps: bool| {
        let mut repo = repo.clone();
        repo.set_use_config(UseConfig::new());
        let mut provider = PortageDependencyProvider::new(repo);
        provider.set_cross_active(true);
        provider.set_is_cross_arch(true);
        provider.set_root_deps_rdeps(rdeps);
        let leaf = PortagePackage::slotted(Cpn::parse("app-misc/leaf").unwrap(), slot0);
        provider
            .resolve_targets(vec![(leaf, PortageVersionSet::any())])
            .unwrap()
    };
    let names = |sol: &SelectedDependencies<PortagePackage, Version>| {
        sol.iter()
            .map(|(p, _)| p.cpn().package.as_str().to_owned())
            .collect::<Vec<_>>()
    };

    // rdeps on: the leaf and its RDEPEND install into the sysroot; the
    // build-only DEPEND is discarded (resolved on the host toolchain).
    let on = names(&solve(true));
    assert!(on.iter().any(|p| p == "leaf"), "leaf itself must resolve");
    assert!(
        on.iter().any(|p| p == "runlib"),
        "rdeps keeps RDEPEND in the sysroot: {on:?}"
    );
    assert!(
        !on.iter().any(|p| p == "buildtool"),
        "rdeps must discard the target DEPEND (build tool): {on:?}"
    );

    // rdeps off (the default / same-arch offset build): DEPEND still
    // installs into the target ROOT.
    let off = names(&solve(false));
    assert!(
        off.iter().any(|p| p == "buildtool"),
        "without rdeps the target DEPEND stays in the target-root graph: {off:?}"
    );
}

// A host-satisfied BDEPEND edge whose atom USE-dep is **not** met by the
// host instance's active USE is rebuilt rather than pruned: `b[text(+)]`
// with the host `b` built `text`-off fails the USE-dep, so `b` enters the
// plan (and its `text?` conditional would re-expand on rebuild). Mirrors
// portage's USE-change rebuild — e.g. `app-text/xmlto[text(+)]` pulling
// `virtual/w3m` when the host xmlto lacks `text`.
#[test]
fn host_installed_bdepend_with_unmet_use_dep_is_rebuilt() {
    let text = Interned::intern("text");
    let slot0 = Interned::intern("0");

    let mut repo = InMemoryRepository::new();
    // b is a build tool with IUSE=text and a text-gated runtime dep on c.
    repo.add_version_with_iuse(
        portage_atom::Cpv::parse("dev-build/b-1.0").unwrap(),
        Some(slot0),
        None,
        vec![text],
        PackageDeps {
            rdepend: (DepEntry::parse("text? ( dev-build/c )").unwrap()).into(),
            ..empty_deps()
        },
    );
    repo.add_version(
        portage_atom::Cpv::parse("dev-build/c-1.0").unwrap(),
        Some(slot0),
        None,
        empty_deps(),
    );
    // a BDEPENDs b with a [text(+)] USE-dep the host lacks.
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(slot0),
        None,
        PackageDeps {
            bdepend: (DepEntry::parse("dev-build/b[text(+)]").unwrap()).into(),
            ..empty_deps()
        },
    );

    let mut provider = {
        let mut cfg = UseConfig::new();
        // b is rebuilt with text on (the USE-dep's demand), so its text?
        // conditional expands.
        cfg.set(text, UseFlagState::Enabled);
        repo.set_use_config(cfg);
        let mut p = PortageDependencyProvider::new(repo);
        p.set_with_bdeps(true);
        p
    };
    // Host b has text OFF (iuse=text, active=[]) → [text(+)] unmet.
    provider.add_host_installed(
        PortagePackage::slotted(Cpn::parse("dev-build/b").unwrap(), slot0),
        Version::parse("1.0").unwrap(),
        vec![],
        vec![text],
    );

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), slot0);
    let solution = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    assert!(
        solution
            .iter()
            .any(|(p, _)| p.cpn().package.as_str() == "b"),
        "a BDEPEND edge whose [flag] USE-dep the host lacks must keep b in \
         the plan (rebuild), not prune it as host-satisfied"
    );
    assert!(
        solution
            .iter()
            .any(|(p, _)| p.cpn().package.as_str() == "c"),
        "b rebuilt with text on must pull its text? runtime dep c"
    );
}

// The satisfied counterpart: when the host instance *does* meet the
// `[flag]` USE-dep (text active), the edge is pruned as before and b/c are
// not pulled.
#[test]
fn host_installed_bdepend_with_met_use_dep_is_pruned() {
    let text = Interned::intern("text");
    let slot0 = Interned::intern("0");

    let mut repo = InMemoryRepository::new();
    repo.add_version_with_iuse(
        portage_atom::Cpv::parse("dev-build/b-1.0").unwrap(),
        Some(slot0),
        None,
        vec![text],
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(slot0),
        None,
        PackageDeps {
            bdepend: (DepEntry::parse("dev-build/b[text(+)]").unwrap()).into(),
            ..empty_deps()
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        let mut p = PortageDependencyProvider::new(repo);
        p.set_with_bdeps(true);
        p
    };
    // Host b has text ON → [text(+)] met → edge pruned.
    provider.add_host_installed(
        PortagePackage::slotted(Cpn::parse("dev-build/b").unwrap(), slot0),
        Version::parse("1.0").unwrap(),
        vec![text],
        vec![text],
    );

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), slot0);
    let solution = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    assert!(
        solution
            .iter()
            .all(|(p, _)| p.cpn().package.as_str() != "b"),
        "a BDEPEND edge whose [flag] USE-dep the host meets is pruned as \
         host-satisfied (no rebuild)"
    );
}

// Cross target build with `--with-bdeps`: host-satisfied BDEPEND must not
// enter the plan (same closure as without the flag; mirrors emerge cross `-p`).
#[test]
fn host_installed_satisfies_cross_bdepend_with_bdeps() {
    let mut repo = InMemoryRepository::new();
    repo.add_version(
        portage_atom::Cpv::parse("dev-build/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            bdepend: (DepEntry::parse("dev-build/b").unwrap()).into(),
            ..empty_deps()
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        let mut p = PortageDependencyProvider::new(repo);
        p.set_cross_active(true);
        p.set_is_cross_arch(true);
        p.set_with_bdeps(true);
        p
    };
    provider.add_host_installed(
        PortagePackage::slotted(Cpn::parse("dev-build/b").unwrap(), Interned::intern("0")),
        Version::parse("1.0").unwrap(),
        vec![],
        vec![],
    );

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    assert!(
        solution
            .iter()
            .all(|(p, _)| p.cpn().package.as_str() != "b"),
        "b is satisfied on BROOT; cross target build must not pull it even with --with-bdeps"
    );
    assert!(
        solution
            .iter()
            .any(|(p, _)| p.cpn().package.as_str() == "a")
    );
}

// Regression test for the `sys-apps/systemd-utils` stage3 failure:
// `cross_target_runtime_deps` (the dependency function for a `--cross`
// Target-root package actually being built) called
// `append_unsatisfied_broot` for IDEPEND but never for BDEPEND at all —
// an unsatisfied BDEPEND (the host lacks `b` entirely here, standing in
// for `dev-python/jinja2` built for the wrong python target) never
// scheduled a Host-root rebuild, so `em` silently omitted it from the
// plan and the target package's own build later failed for a "missing"
// dependency `em` itself dropped.
#[test]
fn cross_target_build_pulls_unsatisfied_bdepend() {
    let mut repo = InMemoryRepository::new();
    repo.add_version(
        portage_atom::Cpv::parse("dev-build/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            bdepend: (DepEntry::parse("dev-build/b").unwrap()).into(),
            ..empty_deps()
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        let mut p = PortageDependencyProvider::new(repo);
        p.set_cross_active(true);
        p.set_is_cross_arch(true);
        p.set_with_bdeps(true);
        p
    };
    // No `add_host_installed` call: the host genuinely lacks `b`.

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    let b_entry = solution
        .iter()
        .find(|(p, _)| p.cpn().package.as_str() == "b");
    assert!(
        b_entry.is_some(),
        "b is genuinely missing on BROOT; the cross target build must \
         schedule it rather than silently drop the BDEPEND edge"
    );
    assert_eq!(
        b_entry.unwrap().0.merge_root(),
        MergeRoot::Host,
        "an unsatisfied BDEPEND resolves onto BROOT (the host), never the \
         target sysroot"
    );
}

// Regression test for the riscv64 stage3 shakeout: same scenario as
// `cross_target_build_pulls_unsatisfied_bdepend`, but `b` is *also*
//
// See issue #33.
// already installed at the **Target** (the `--cross` sysroot) — standing
// in for `dev-lang/perl` being genuinely present in a real, already-built
// target `@system` while `base_roots()` (BROOT) still lacks it. A
// Host-root `b` must still be scheduled: BDEPEND always resolves on
// BROOT, and a same-named Target-side package can never satisfy it.
#[test]
fn cross_target_build_pulls_unsatisfied_bdepend_even_if_target_already_has_it() {
    let mut repo = InMemoryRepository::new();
    repo.add_version(
        portage_atom::Cpv::parse("dev-build/b-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            bdepend: (DepEntry::parse("dev-build/b").unwrap()).into(),
            ..empty_deps()
        },
    );

    let config = UseConfig::new();
    let mut provider = {
        repo.set_use_config(config);
        let mut p = PortageDependencyProvider::new(repo);
        p.set_cross_active(true);
        p.set_is_cross_arch(true);
        p.set_with_bdeps(true);
        p
    };
    // `b` is already installed at the Target (sysroot) — NOT at the host.
    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(Cpn::parse("dev-build/b").unwrap(), Interned::intern("0")),
        version: Version::parse("1.0").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: Vec::new(),
        iuse: Vec::new(),
    });

    let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(a, PortageVersionSet::any())])
        .unwrap();

    let host_b = solution
        .iter()
        .find(|(p, _)| p.cpn().package.as_str() == "b" && p.merge_root() == MergeRoot::Host);
    assert!(
        host_b.is_some(),
        "b is missing on BROOT even though a same-named package is \
         installed at the Target sysroot; the cross target build must \
         still schedule a Host-root b rather than treating the Target \
         instance as satisfying the BDEPEND"
    );
}

//
// `virtual/libcrypt` completed while `sys-libs/libxcrypt` never got
// `Emerging`, so pam's configure failed to find libcrypt in the *sysroot*.
//
// Host often has `libxcrypt` (and even `virtual/libcrypt`) installed. Those
// must satisfy BDEPEND on BROOT only — they must **not** suppress a Target
// RDEPEND of the virtual. The provider must select Target `libxcrypt`,
// order it before the virtual, and emit a Target→Target RDEPEND edge so
// `build_blockers` can wait on the provider.
#[test]
fn cross_target_virtual_rdepend_provider_is_target_not_host() {
    let mut repo = InMemoryRepository::new();
    // Real provider (like sys-libs/libxcrypt).
    repo.add_version(
        portage_atom::Cpv::parse("sys-libs/libxcrypt-4.5.2").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    // Also an older host-like version so Favor/host can't "upgrade away".
    repo.add_version(
        portage_atom::Cpv::parse("sys-libs/libxcrypt-4.4.38").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    // Gentoo-style virtual: RDEPEND only (no DEPEND).
    repo.add_version(
        portage_atom::Cpv::parse("virtual/libcrypt-2").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("sys-libs/libxcrypt").unwrap()).into(),
            ..empty_deps()
        },
    );
    // Consumer like pam.
    repo.add_version(
        portage_atom::Cpv::parse("sys-libs/pam-1.7.2").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            depend: (DepEntry::parse("virtual/libcrypt").unwrap()).into(),
            rdepend: (DepEntry::parse("virtual/libcrypt").unwrap()).into(),
            ..empty_deps()
        },
    );

    let mut provider = {
        repo.set_use_config(UseConfig::new());
        let mut p = PortageDependencyProvider::new(repo);
        p.set_cross_active(true);
        p.set_is_cross_arch(true);
        p.set_with_bdeps(true);
        p
    };

    // Host already has the provider (and could have the virtual) — BROOT only.
    provider.add_host_installed(
        PortagePackage::slotted(
            Cpn::parse("sys-libs/libxcrypt").unwrap(),
            Interned::intern("0"),
        ),
        Version::parse("4.4.38").unwrap(),
        Vec::new(),
        Vec::new(),
    );
    provider.add_host_installed(
        PortagePackage::slotted(
            Cpn::parse("virtual/libcrypt").unwrap(),
            Interned::intern("0"),
        ),
        Version::parse("2").unwrap(),
        Vec::new(),
        Vec::new(),
    );

    let pam = PortagePackage::slotted(Cpn::parse("sys-libs/pam").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(pam, PortageVersionSet::any())])
        .expect("solve pam");

    let cpn_eq = |p: &PortagePackage, cat: &str, pkg: &str| {
        p.cpn().category.as_str() == cat && p.cpn().package.as_str() == pkg
    };
    let target_xcrypt = solution
        .iter()
        .find(|(p, _)| cpn_eq(p, "sys-libs", "libxcrypt") && p.merge_root() == MergeRoot::Target);
    assert!(
        target_xcrypt.is_some(),
        "Target sysroot must still get libxcrypt even when host has it; \
         host-only satisfaction leaves pam without crypt.h in the sysroot. \
         solution={:?}",
        solution
            .iter()
            .map(|(p, v)| format!("{}@{:?}={v}", p.cpn(), p.merge_root()))
            .collect::<Vec<_>>()
    );

    // Virtual must also be a Target merge (installs into sysroot VDB).
    assert!(
        solution
            .iter()
            .any(|(p, _)| cpn_eq(p, "virtual", "libcrypt") && p.merge_root() == MergeRoot::Target),
        "virtual/libcrypt must merge as Target"
    );

    let order = provider.install_order(&solution);
    let idx = |cat: &str, pkg: &str| {
        order
            .iter()
            .position(|(p, _)| cpn_eq(p, cat, pkg) && p.merge_root() == MergeRoot::Target)
    };
    let i_xcrypt = idx("sys-libs", "libxcrypt").expect("libxcrypt in order");
    let i_virt = idx("virtual", "libcrypt").expect("virtual in order");
    let i_pam = idx("sys-libs", "pam").expect("pam in order");
    assert!(
        i_xcrypt < i_virt,
        "libxcrypt must install before virtual/libcrypt (RDEPEND provider); \
         order indices xcrypt={i_xcrypt} virt={i_virt}"
    );
    assert!(
        i_virt < i_pam,
        "virtual/libcrypt before pam; virt={i_virt} pam={i_pam}"
    );

    // Edge graph must carry Target virtual → Target libxcrypt RDEPEND for
    // build_blockers (same keys as plan entries).
    let edges = provider.dependency_graph(&solution);
    let has_edge = edges.iter().any(|e| {
        e.class == crate::graph::DepClass::Rdepend
            && cpn_eq(&e.from.0, "virtual", "libcrypt")
            && e.from.0.merge_root() == MergeRoot::Target
            && cpn_eq(&e.to.0, "sys-libs", "libxcrypt")
            && e.to.0.merge_root() == MergeRoot::Target
    });
    assert!(
        has_edge,
        "need Target RDEPEND edge virtual/libcrypt → libxcrypt for \
         build_blockers; edges={:?}",
        edges
            .iter()
            .map(|e| format!(
                "{:?}: {}@{:?} -> {}@{:?}",
                e.class,
                e.from.0.cpn(),
                e.from.0.merge_root(),
                e.to.0.cpn(),
                e.to.0.merge_root()
            ))
            .collect::<Vec<_>>()
    );
}

// The `sys-apps/coreutils` shape: RDEPEND nests one USE-conditional group
// inside another, `!static? ( acl? ( sys-apps/acl ) )` (LIB_DEPEND folded
// into RDEPEND at the ebuild level — PMS doesn't special-case it, it's just
// ordinary nested group syntax). `static` is off and `acl` is on, so the
// inner atom should convert to a real requirement and pull `sys-apps/acl`
// into the solution.
#[test]
fn nested_use_conditional_groups_still_pull_the_inner_atom() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("sys-apps/coreutils-9.11").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("!static? ( acl? ( sys-apps/acl ) )").unwrap()).into(),
            ..empty_deps()
        },
    );
    repo.add_version(
        portage_atom::Cpv::parse("sys-apps/acl-2.4.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );

    let mut config = UseConfig::new();
    config.enable(Interned::intern("acl"));
    repo.set_use_config(config);

    let mut provider = PortageDependencyProvider::new(repo);
    let coreutils = PortagePackage::slotted(
        Cpn::parse("sys-apps/coreutils").unwrap(),
        Interned::intern("0"),
    );
    let solution = provider
        .resolve_targets(vec![(coreutils, PortageVersionSet::any())])
        .unwrap();

    let acl = Cpn::parse("sys-apps/acl").unwrap();
    assert!(
        solution.iter().any(|(p, _)| p.cpn() == &acl),
        "sys-apps/acl must be pulled in via the nested !static?( acl?( ... ) ) \
         group; solution={:?}",
        solution
            .iter()
            .map(|(p, v)| format!("{p}-{v}"))
            .collect::<Vec<_>>()
    );
}

// The `net-misc/rsync` shape: RDEPEND has a *versioned* atom inside a single
// USE-conditional group, `acl? ( >=virtual/acl-2.4.0 )`. The installed
// `virtual/acl-0-r2` (Favor policy) does not satisfy `>=2.4.0`, so this
// should force a real upgrade requirement, not a silently-dropped edge.
#[test]
fn versioned_atom_inside_use_conditional_forces_the_upgrade() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("net-misc/rsync-3.5.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse("acl? ( >=virtual/acl-2.4.0 )").unwrap()).into(),
            ..empty_deps()
        },
    );
    repo.add_version(
        portage_atom::Cpv::parse("virtual/acl-0-r2").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );
    repo.add_version(
        portage_atom::Cpv::parse("virtual/acl-2.4.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );

    let mut config = UseConfig::new();
    config.enable(Interned::intern("acl"));
    repo.set_use_config(config);

    let mut provider = PortageDependencyProvider::new(repo);
    provider.add_installed(InstalledPackage {
        package: PortagePackage::slotted(Cpn::parse("virtual/acl").unwrap(), Interned::intern("0")),
        version: Version::parse("0-r2").unwrap(),
        policy: InstalledPolicy::Favor,
        active_use: vec![],
        iuse: vec![],
    });

    let rsync =
        PortagePackage::slotted(Cpn::parse("net-misc/rsync").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(rsync, PortageVersionSet::any())])
        .unwrap();

    let virtual_acl = Cpn::parse("virtual/acl").unwrap();
    let got = solution.iter().find(|(p, _)| p.cpn() == &virtual_acl);
    assert_eq!(
        got.map(|(_, v)| v.clone()),
        Some(Version::parse("2.4.0").unwrap()),
        "virtual/acl must be upgraded to satisfy >=2.4.0, not silently \
         dropped or left at the unsatisfying installed 0-r2; solution={:?}",
        solution
            .iter()
            .map(|(p, v)| format!("{p}-{v}"))
            .collect::<Vec<_>>()
    );
}

// The real `virtual/acl-2.4.0` shape: `kernel_linux? ( >=sys-apps/acl-2.4.0[
// static-libs?,abi_x86_32(-)?,abi_x86_64(-)?,abi_x86_x32(-)?,abi_mips_n32(-)?,
// abi_mips_n64(-)?,abi_mips_o32(-)?,abi_s390_32(-)?,abi_s390_64(-)?] )` — a
// versioned atom carrying eight simultaneous conditional USE-deps (the
// multilib-build ABI flags), none of which this arm64 fixture's `sys-apps/acl`
// declares in IUSE. `kernel_linux` is on (every Linux profile sets it).
#[test]
fn many_conditional_use_deps_on_one_atom_still_pull_it_in() {
    let mut repo = InMemoryRepository::new();

    repo.add_version(
        portage_atom::Cpv::parse("virtual/acl-2.4.0").unwrap(),
        Some(Interned::intern("0")),
        None,
        PackageDeps {
            rdepend: (DepEntry::parse(
                "kernel_linux? ( >=sys-apps/acl-2.4.0[static-libs?,abi_x86_32(-)?,\
                 abi_x86_64(-)?,abi_x86_x32(-)?,abi_mips_n32(-)?,abi_mips_n64(-)?,\
                 abi_mips_o32(-)?,abi_s390_32(-)?,abi_s390_64(-)?] )",
            )
            .unwrap())
            .into(),
            ..empty_deps()
        },
    );
    repo.add_version(
        portage_atom::Cpv::parse("sys-apps/acl-2.4.0-r2").unwrap(),
        Some(Interned::intern("0")),
        None,
        empty_deps(),
    );

    let mut config = UseConfig::new();
    config.enable(Interned::intern("kernel_linux"));
    repo.set_use_config(config);

    let mut provider = PortageDependencyProvider::new(repo);
    let virtual_acl =
        PortagePackage::slotted(Cpn::parse("virtual/acl").unwrap(), Interned::intern("0"));
    let solution = provider
        .resolve_targets(vec![(virtual_acl, PortageVersionSet::any())])
        .unwrap();

    let acl = Cpn::parse("sys-apps/acl").unwrap();
    assert!(
        solution.iter().any(|(p, _)| p.cpn() == &acl),
        "sys-apps/acl must be pulled in despite the pile of ABI use-deps on \
         the atom; solution={:?}",
        solution
            .iter()
            .map(|(p, v)| format!("{p}-{v}"))
            .collect::<Vec<_>>()
    );
}

// ---- Widened candidate supply (`needs_unmask` tiering) ----

fn tagged_version(cpv: &str) -> (portage_atom::Cpv, PackageVersions) {
    let mut pv = PackageVersions {
        slot: Some(Interned::intern("0")),
        subslot: None,
        repo: None,
        iuse: vec![],
        iuse_defaults: Default::default(),
        deps: empty_deps(),
        required_use: None,
        empty_any_of_matches: true,
        needs_unmask: true,
        live: false,
    };
    // Mirror what the real adapter computes from version shape + PROPERTIES.
    pv.live = portage_atom::Cpv::parse(cpv).unwrap().version.is_live();
    (portage_atom::Cpv::parse(cpv).unwrap(), pv)
}

fn accepted_version(cpv: &str) -> (portage_atom::Cpv, PackageVersions) {
    let (cpv, mut pv) = tagged_version(cpv);
    pv.needs_unmask = false;
    (cpv, pv)
}

fn add_all(repo: &mut InMemoryRepository, entries: Vec<(portage_atom::Cpv, PackageVersions)>) {
    for (cpv, pv) in entries {
        repo.add_package_versions(cpv, pv);
    }
}

#[test]
fn widened_candidate_loses_to_accepted_in_range() {
    let mut repo = InMemoryRepository::new();
    // Accepted 1.0 must beat the masked newer 2.0 even under widening.
    add_all(
        &mut repo,
        vec![
            accepted_version("dev-libs/pkg-1.0"),
            tagged_version("dev-libs/pkg-2.0"),
        ],
    );
    repo.set_use_config(UseConfig::new());
    let provider = PortageDependencyProvider::new(repo);

    let pkg = PortagePackage::slotted(Cpn::parse("dev-libs/pkg").unwrap(), Interned::intern("0"));
    assert_eq!(
        provider
            .choose_version(&pkg, &PortageVersionSet::any())
            .unwrap(),
        Some(Version::parse("1.0").unwrap())
    );
}

#[test]
fn tagged_only_range_resolves_and_reports() {
    let mut repo = InMemoryRepository::new();
    add_all(&mut repo, vec![tagged_version("dev-libs/pkg-2.0")]);
    repo.set_use_config(UseConfig::new());
    let provider = PortageDependencyProvider::new(repo);

    let pkg = PortagePackage::slotted(Cpn::parse("dev-libs/pkg").unwrap(), Interned::intern("0"));
    // Strict filtering would make this range a hard `NoVersions` failure.
    let chosen = provider
        .choose_version(&pkg, &PortageVersionSet::any())
        .unwrap();
    assert_eq!(chosen, Some(Version::parse("2.0").unwrap()));
    assert!(provider.selection_needs_unmask(&pkg, chosen.as_ref().unwrap()));
}

#[test]
fn tagged_release_beats_tagged_live() {
    let mut repo = InMemoryRepository::new();
    add_all(
        &mut repo,
        vec![
            tagged_version("dev-libs/pkg-2.0"),
            tagged_version("dev-libs/pkg-2.0.9999"),
        ],
    );
    repo.set_use_config(UseConfig::new());
    let provider = PortageDependencyProvider::new(repo);

    let pkg = PortagePackage::slotted(Cpn::parse("dev-libs/pkg").unwrap(), Interned::intern("0"));
    // Both are tagged; among them the release ebuild beats the live one
    // (deliberate divergence from portage's `**` behaviour).
    assert_eq!(
        provider
            .choose_version(&pkg, &PortageVersionSet::any())
            .unwrap(),
        Some(Version::parse("2.0").unwrap())
    );
}

#[test]
fn choice_branch_needing_unmask_loses_to_untagged_branch() {
    let mut repo = InMemoryRepository::new();
    // First-listed `||` branch resolves only via a masked version; the
    // second branch has an accepted version. Without branch demotion the
    // first-listed wins `max()` and silently pulls the masked pick.
    add_all(
        &mut repo,
        vec![(
            portage_atom::Cpv::parse("app-parent/parent-1.0").unwrap(),
            PackageVersions {
                slot: Some(Interned::intern("0")),
                subslot: None,
                repo: None,
                iuse: vec![],
                iuse_defaults: Default::default(),
                deps: PackageDeps {
                    depend: DepList::new(
                        DepEntry::parse("|| ( dev-libs/bad dev-libs/good )").unwrap(),
                    ),
                    rdepend: (vec![]).into(),
                    bdepend: (vec![]).into(),
                    pdepend: (vec![]).into(),
                    idepend: (vec![]).into(),
                },
                required_use: None,
                empty_any_of_matches: true,
                needs_unmask: false,
                live: false,
            },
        )],
    );
    add_all(&mut repo, vec![tagged_version("dev-libs/bad-1.0")]);
    add_all(&mut repo, vec![accepted_version("dev-libs/good-1.0")]);
    repo.set_use_config(UseConfig::new());
    let mut provider = PortageDependencyProvider::new(repo);

    let parent = PortagePackage::slotted(
        Cpn::parse("app-parent/parent").unwrap(),
        Interned::intern("0"),
    );
    let solution = provider
        .resolve_targets(vec![(parent, PortageVersionSet::any())])
        .unwrap();

    let good = Cpn::parse("dev-libs/good").unwrap();
    let bad = Cpn::parse("dev-libs/bad").unwrap();
    assert!(
        solution.iter().any(|(p, _)| p.cpn() == &good),
        "untagged branch must win"
    );
    assert!(
        !solution.iter().any(|(p, _)| p.cpn() == &bad),
        "tagged-only branch must be demoted"
    );
}

fn live_tagged_version(cpv: &str) -> (portage_atom::Cpv, PackageVersions) {
    let (cpv, mut pv) = tagged_version(cpv);
    pv.live = true;
    (cpv, pv)
}

#[test]
fn tagged_live_is_never_selected_transitively() {
    let mut repo = InMemoryRepository::new();
    // Parent's dep range admits a release and a live; the solver must not
    // land on the live ebuild even though it is the newest candidate.
    add_all(
        &mut repo,
        vec![
            (
                portage_atom::Cpv::parse("app-parent/parent-1.0").unwrap(),
                PackageVersions {
                    slot: Some(Interned::intern("0")),
                    subslot: None,
                    repo: None,
                    iuse: vec![],
                    iuse_defaults: Default::default(),
                    deps: PackageDeps {
                        depend: DepList::new(DepEntry::parse("dev-libs/pkg").unwrap()),
                        rdepend: (vec![]).into(),
                        bdepend: (vec![]).into(),
                        pdepend: (vec![]).into(),
                        idepend: (vec![]).into(),
                    },
                    required_use: None,
                    empty_any_of_matches: true,
                    needs_unmask: false,
                    live: false,
                },
            ),
            accepted_version("dev-libs/pkg-1.0"),
            live_tagged_version("dev-libs/pkg-2.0.9999"),
        ],
    );
    repo.set_use_config(UseConfig::new());
    let mut provider = PortageDependencyProvider::new(repo);

    let parent = PortagePackage::slotted(
        Cpn::parse("app-parent/parent").unwrap(),
        Interned::intern("0"),
    );
    let solution = provider
        .resolve_targets(vec![(parent, PortageVersionSet::any())])
        .unwrap();

    let pkg = Cpn::parse("dev-libs/pkg").unwrap();
    let selected = solution
        .iter()
        .find(|(p, _)| p.cpn() == &pkg)
        .map(|(_, v)| v.clone());
    assert_eq!(selected, Some(Version::parse("1.0").unwrap()));
}

#[test]
fn root_target_may_select_live_explicitly() {
    let mut repo = InMemoryRepository::new();
    add_all(
        &mut repo,
        vec![
            accepted_version("dev-libs/pkg-1.0"),
            live_tagged_version("dev-libs/pkg-2.0.9999"),
        ],
    );
    repo.set_use_config(UseConfig::new());
    let mut provider = PortageDependencyProvider::new(repo);

    let pkg = PortagePackage::slotted(Cpn::parse("dev-libs/pkg").unwrap(), Interned::intern("0"));
    // Naming the live version explicitly as a target is the "asked for"
    // case: root-target registration is what marks it allowed.
    let vs = PortageVersionSet::from_operator(
        portage_atom::Operator::Equal,
        false,
        Version::parse("2.0.9999").unwrap(),
    );
    let solution = provider.resolve_targets(vec![(pkg.clone(), vs)]).unwrap();
    assert_eq!(
        solution.get(&pkg),
        Some(&Version::parse("2.0.9999").unwrap())
    );
}
