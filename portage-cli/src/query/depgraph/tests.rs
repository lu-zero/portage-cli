//! Fixture solves through [`super::depgraph`]. The live `capture-plan.sh`
//! oracle only snapshots `--emptytree` on a real tree and never sets
//! `autounmask_widen`.

use std::collections::HashSet;

use camino::Utf8PathBuf;
use gentoo_core::Arch;
use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{MergeRoot, PortagePackage, UseFlagRequirement};
use portage_repo::RepoSet;

use super::targets::TargetAtom;
use super::{AutounmaskPersist, DepgraphOpts, apply_order_filters, depgraph};
use crate::cli::DepgraphFormat;

struct Pkg {
    cpv: &'static str,
    keywords: &'static str,
    depend: &'static str,
    iuse: &'static str,
    slot: &'static str,
}

struct Fix {
    _dir: tempfile::TempDir,
    set: RepoSet,
    root: Utf8PathBuf,
    roots: portage_resolve::Roots,
}

struct Solve<'a> {
    atoms: &'a [TargetAtom],
    empty: bool,
    noreplace: bool,
    widen: bool,
    onlydeps: bool,
    update: bool,
    deep: bool,
    complete_graph: bool,
    exclude: &'a [String],
    resume: HashSet<(MergeRoot, String)>,
}

impl<'a> Solve<'a> {
    fn new(atoms: &'a [TargetAtom]) -> Self {
        Self {
            atoms,
            empty: false,
            noreplace: false,
            widen: false,
            onlydeps: false,
            update: false,
            deep: false,
            complete_graph: false,
            exclude: &[],
            resume: HashSet::new(),
        }
    }
}

fn cache_text(pkg: &Pkg) -> String {
    let mut text = format!(
        "EAPI=8\nDESCRIPTION=t\nSLOT={}\nKEYWORDS={}\nDEPEND={}\n",
        pkg.slot, pkg.keywords, pkg.depend
    );
    if !pkg.iuse.is_empty() {
        text.push_str(&format!("IUSE={}\n", pkg.iuse));
    }
    text
}

fn plant_vdb(root: &Utf8PathBuf, cpv: &str) {
    plant_vdb_rdepend(root, cpv, None);
}

fn plant_installed(root: &Utf8PathBuf, cpv: &str, slot: &str, use_flags: &str, iuse: &str) {
    let cpv = Cpv::parse(cpv).unwrap();
    let dir = root
        .join("var/db/pkg")
        .join(cpv.cpn.category.as_str())
        .join(format!("{}-{}", cpv.cpn.package, cpv.version));
    std::fs::create_dir_all(dir.as_std_path()).unwrap();
    std::fs::write(dir.join("SLOT").as_std_path(), format!("{slot}\n")).unwrap();
    std::fs::write(dir.join("EAPI").as_std_path(), "8\n").unwrap();
    if !use_flags.is_empty() {
        std::fs::write(dir.join("USE").as_std_path(), format!("{use_flags}\n")).unwrap();
    }
    if !iuse.is_empty() {
        std::fs::write(dir.join("IUSE").as_std_path(), format!("{iuse}\n")).unwrap();
    }
}

fn plant_vdb_rdepend(root: &Utf8PathBuf, cpv: &str, rdepend: Option<&str>) {
    let cpv = Cpv::parse(cpv).unwrap();
    let dir = root
        .join("var/db/pkg")
        .join(cpv.cpn.category.as_str())
        .join(format!("{}-{}", cpv.cpn.package, cpv.version));
    std::fs::create_dir_all(dir.as_std_path()).unwrap();
    std::fs::write(dir.join("SLOT").as_std_path(), "0\n").unwrap();
    std::fs::write(dir.join("EAPI").as_std_path(), "8\n").unwrap();
    if let Some(rdepend) = rdepend {
        std::fs::write(dir.join("RDEPEND").as_std_path(), format!("{rdepend}\n")).unwrap();
    }
}

fn fixture(pkgs: &[Pkg], installed: &[&str]) -> Fix {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let root = dir.path().join("root");
    std::fs::create_dir_all(repo.join("metadata")).unwrap();
    std::fs::create_dir_all(repo.join("profiles/base")).unwrap();
    std::fs::write(repo.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo.join("profiles/repo_name"), "fixture\n").unwrap();
    std::fs::write(
        repo.join("profiles/base/make.defaults"),
        "ARCH=\"amd64\"\nACCEPT_KEYWORDS=\"amd64\"\n",
    )
    .unwrap();
    for pkg in pkgs {
        let cpv = Cpv::parse(pkg.cpv).unwrap();
        let cache = repo
            .join("metadata/md5-cache")
            .join(cpv.cpn.category.as_str());
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join(format!("{}-{}", cpv.cpn.package, cpv.version)),
            cache_text(pkg),
        )
        .unwrap();
    }

    let root_path = Utf8PathBuf::try_from(root.clone()).unwrap();
    std::fs::create_dir_all(root.join("etc/portage")).unwrap();
    std::fs::write(
        root.join("etc/portage/make.conf"),
        "ARCH=\"amd64\"\nACCEPT_KEYWORDS=\"amd64\"\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        repo.join("profiles/base"),
        root.join("etc/portage/make.profile"),
    )
    .unwrap();
    for cpv in installed {
        plant_vdb(&root_path, cpv);
    }

    let opened = portage_repo::Repository::builder()
        .in_memory_cache()
        .open(&repo)
        .unwrap();
    let roots = portage_resolve::Roots::default()
        .with_config(Some(root_path.clone()))
        .with_base(Some(root_path.clone()))
        .with_target(Some(root_path.clone()))
        .with_broot(Some(root_path.clone()));
    Fix {
        _dir: dir,
        set: RepoSet::single(opened),
        root: root_path,
        roots,
    }
}

fn solve(fix: &Fix, opts: Solve<'_>) -> super::DepgraphOutcome {
    let arch = Arch::intern("amd64");
    let world: &[Dep] = &[];
    let extra: &[(Dep, Vec<portage_atom_pubgrub::UseOverride>)] = &[];
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(depgraph(DepgraphOpts {
        set: fix.set.clone(),
        atoms: opts.atoms,
        world_additions: world,
        arch: &arch,
        format: DepgraphFormat::Pretty,
        verbose: 0,
        empty: opts.empty,
        autounmask_write: false,
        autounmask_persist: AutounmaskPersist::Never,
        ask: false,
        autosolve_use: false,
        autounmask_widen: opts.widen,
        roots: &fix.roots,
        host_merge_root: &fix.root,
        onlydeps: opts.onlydeps,
        with_bdeps: false,
        root_deps_rdeps: false,
        deep: opts.deep,
        update: opts.update,
        newuse: false,
        changed_use: false,
        noreplace: opts.noreplace,
        nodeps: false,
        extra_use_override: None,
        extra_package_use: extra,
        sysroot_override: None,
        binpkg_index: None,
        exclude: opts.exclude,
        resume_completed: opts.resume,
        complete_graph: opts.complete_graph,
        quiet: true,
    }))
    .unwrap()
}

fn names(outcome: &super::DepgraphOutcome) -> Vec<String> {
    outcome.plan.iter().map(|p| p.cpv.to_string()).collect()
}

fn pair() -> Vec<Pkg> {
    vec![
        Pkg {
            cpv: "app-misc/foo-1",
            keywords: "amd64",
            depend: "app-misc/bar",
            iuse: "",
            slot: "0",
        },
        Pkg {
            cpv: "app-misc/bar-1",
            keywords: "amd64",
            depend: "",
            iuse: "",
            slot: "0",
        },
    ]
}

/// `bar-1` is installed. `consumer-1` is installed with `RDEPEND="<app-misc/bar-2"`.
/// `consumer-2` accepts `bar-2`, so a repair round can upgrade the consumer
/// instead of leaving the pin broken.
fn pinned_consumer() -> Vec<Pkg> {
    vec![
        Pkg {
            cpv: "app-misc/bar-1",
            keywords: "amd64",
            depend: "",
            iuse: "",
            slot: "0",
        },
        Pkg {
            cpv: "app-misc/bar-2",
            keywords: "amd64",
            depend: "",
            iuse: "",
            slot: "0",
        },
        Pkg {
            cpv: "app-misc/consumer-1",
            keywords: "amd64",
            depend: "<app-misc/bar-2",
            iuse: "",
            slot: "0",
        },
        Pkg {
            cpv: "app-misc/consumer-2",
            keywords: "amd64",
            depend: ">=app-misc/bar-2",
            iuse: "",
            slot: "0",
        },
    ]
}

fn masked_dep() -> Vec<Pkg> {
    vec![
        Pkg {
            cpv: "app-misc/qux-1",
            keywords: "amd64",
            depend: "app-misc/masked",
            iuse: "",
            slot: "0",
        },
        Pkg {
            cpv: "app-misc/masked-1",
            keywords: "~amd64",
            depend: "",
            iuse: "",
            slot: "0",
        },
    ]
}

#[test]
fn emptytree_orders_the_dependency_before_its_parent() {
    let fix = fixture(&pair(), &[]);
    let atoms = [TargetAtom::explicit("app-misc/foo")];
    let mut opts = Solve::new(&atoms);
    opts.empty = true;
    let outcome = solve(&fix, opts);
    let planned = names(&outcome);
    let bar = planned.iter().position(|n| n == "app-misc/bar-1").unwrap();
    let foo = planned.iter().position(|n| n == "app-misc/foo-1").unwrap();
    assert!(bar < foo);
    assert!(outcome.build_blockers[foo].contains(&bar));
    assert_eq!(outcome.exit_code, 0);
}

#[test]
fn emptytree_still_plans_a_package_that_is_already_installed() {
    let fix = fixture(&pair(), &["app-misc/bar-1"]);
    let atoms = [TargetAtom::explicit("app-misc/foo")];
    let mut opts = Solve::new(&atoms);
    opts.empty = true;
    let planned = names(&solve(&fix, opts));
    assert!(planned.iter().any(|n| n == "app-misc/bar-1"));
    assert!(planned.iter().any(|n| n == "app-misc/foo-1"));
}

#[test]
fn exclude_removes_the_dependency_after_the_solve() {
    let fix = fixture(&pair(), &[]);
    let atoms = [TargetAtom::explicit("app-misc/foo")];
    let excluded = ["app-misc/bar".to_string()];
    let mut opts = Solve::new(&atoms);
    opts.empty = true;
    opts.exclude = &excluded;
    let planned = names(&solve(&fix, opts));
    assert_eq!(planned, ["app-misc/foo-1"]);
}

#[test]
fn resume_omits_a_package_already_completed() {
    let fix = fixture(&pair(), &[]);
    let atoms = [TargetAtom::explicit("app-misc/foo")];
    let mut opts = Solve::new(&atoms);
    opts.empty = true;
    opts.resume
        .insert((MergeRoot::Target, "app-misc/bar-1".to_string()));
    let planned = names(&solve(&fix, opts));
    assert_eq!(planned, ["app-misc/foo-1"]);
}

#[test]
fn noreplace_leaves_an_installed_target_out_of_the_plan() {
    let fix = fixture(&pair(), &["app-misc/foo-1", "app-misc/bar-1"]);
    let atoms = [TargetAtom::explicit("app-misc/foo")];
    let mut opts = Solve::new(&atoms);
    opts.noreplace = true;
    assert!(names(&solve(&fix, opts)).is_empty());
}

#[test]
fn an_installed_target_is_reinstalled_when_the_resolve_is_not_selective() {
    let fix = fixture(&pair(), &["app-misc/foo-1", "app-misc/bar-1"]);
    let atoms = [TargetAtom::explicit("app-misc/foo")];
    let outcome = solve(&fix, Solve::new(&atoms));
    assert_eq!(names(&outcome), ["app-misc/foo-1"]);
    assert!(outcome.plan[0].reinstall);
}

#[test]
fn onlydeps_plans_the_dependency_and_omits_the_target() {
    let fix = fixture(&pair(), &[]);
    let atoms = [TargetAtom::explicit("app-misc/foo")];
    let mut opts = Solve::new(&atoms);
    opts.empty = true;
    opts.onlydeps = true;
    assert_eq!(names(&solve(&fix, opts)), ["app-misc/bar-1"]);
}

#[test]
fn a_keyword_dropped_dependency_is_not_installable() {
    let fix = fixture(&masked_dep(), &[]);
    let atoms = [TargetAtom::explicit("app-misc/qux")];
    let outcome = solve(&fix, Solve::new(&atoms));
    let planned = names(&outcome);
    assert!(
        planned.iter().any(|n| n == "app-misc/qux-1"),
        "plan was {planned:?}"
    );
    assert!(
        !planned.iter().any(|n| n == "app-misc/masked-1"),
        "plan was {planned:?}"
    );
    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn complete_graph_pulls_the_installed_package_whose_pin_the_update_breaks() {
    let fix = fixture(&pinned_consumer(), &[]);
    plant_vdb(&fix.root, "app-misc/bar-1");
    plant_vdb_rdepend(&fix.root, "app-misc/consumer-1", Some("<app-misc/bar-2"));
    let atoms = [TargetAtom::explicit("app-misc/bar")];

    let mut update = Solve::new(&atoms);
    update.update = true;
    update.deep = true;
    assert_eq!(names(&solve(&fix, update)), ["app-misc/bar-2"]);

    let mut repair = Solve::new(&atoms);
    repair.update = true;
    repair.deep = true;
    repair.complete_graph = true;
    let planned = names(&solve(&fix, repair));
    let bar = planned
        .iter()
        .position(|n| n == "app-misc/bar-2")
        .unwrap_or_else(|| panic!("plan was {planned:?}"));
    let consumer = planned
        .iter()
        .position(|n| n == "app-misc/consumer-2")
        .unwrap_or_else(|| panic!("plan was {planned:?}"));
    assert!(bar < consumer, "plan was {planned:?}");
}

#[test]
fn widen_schedules_a_testing_keyword_dependency_the_strict_solve_drops() {
    let fix = fixture(&masked_dep(), &[]);
    let atoms = [TargetAtom::explicit("app-misc/qux")];
    let strict = names(&solve(&fix, Solve::new(&atoms)));
    assert!(
        !strict.iter().any(|n| n == "app-misc/masked-1"),
        "strict plan was {strict:?}"
    );

    let mut opts = Solve::new(&atoms);
    opts.widen = true;
    let widened = names(&solve(&fix, opts));
    assert!(
        widened.iter().any(|n| n == "app-misc/masked-1"),
        "widened plan was {widened:?}"
    );
    let masked = widened
        .iter()
        .position(|n| n == "app-misc/masked-1")
        .unwrap();
    let qux = widened.iter().position(|n| n == "app-misc/qux-1").unwrap();
    assert!(masked < qux);
}

#[test]
fn a_missing_explicit_atom_is_fatal() {
    let fix = fixture(&pair(), &[]);
    let atoms = [TargetAtom::explicit("app-misc/missing")];
    let arch = Arch::intern("amd64");
    let world: &[Dep] = &[];
    let extra: &[(Dep, Vec<portage_atom_pubgrub::UseOverride>)] = &[];
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(depgraph(DepgraphOpts {
            set: fix.set.clone(),
            atoms: &atoms,
            world_additions: world,
            arch: &arch,
            format: DepgraphFormat::Pretty,
            verbose: 0,
            empty: false,
            autounmask_write: false,
            autounmask_persist: AutounmaskPersist::Never,
            ask: false,
            autosolve_use: false,
            autounmask_widen: false,
            roots: &fix.roots,
            host_merge_root: &fix.root,
            onlydeps: false,
            with_bdeps: false,
            root_deps_rdeps: false,
            deep: false,
            update: false,
            newuse: false,
            changed_use: false,
            noreplace: false,
            nodeps: false,
            extra_use_override: None,
            extra_package_use: extra,
            sysroot_override: None,
            binpkg_index: None,
            exclude: &[],
            resume_completed: HashSet::new(),
            complete_graph: false,
            quiet: true,
        }));
    let Err(err) = result else {
        panic!("a missing explicit atom must abort the resolve");
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("app-misc/missing"), "{msg}");
}

#[test]
fn a_missing_world_member_does_not_abort() {
    let fix = fixture(&pair(), &[]);
    let atoms = [TargetAtom {
        atom: "app-misc/missing".to_string(),
        origin: super::targets::TargetOrigin::Set("world".to_string()),
    }];
    let outcome = solve(&fix, Solve::new(&atoms));
    assert!(outcome.plan.is_empty());
    assert_eq!(outcome.exit_code, 0);
}

#[test]
fn apply_order_filters_reappends_a_reinstall_missing_from_the_order() {
    let cpn = Cpn::parse("app-misc/re").unwrap();
    let pkg = PortagePackage::slotted(cpn, Interned::intern("0"));
    let ver = Version::parse("1").unwrap();
    let req = UseFlagRequirement {
        package: pkg.clone(),
        version: ver.clone(),
        upgrade_to: None,
        required_enabled: Vec::new(),
        required_disabled: Vec::new(),
        required_by: Vec::new(),
    };
    let (order, excluded, resumed) =
        apply_order_filters(Vec::new(), &[&req], &[], &HashSet::new(), false);
    assert_eq!(order, vec![(pkg, ver)]);
    assert_eq!((excluded, resumed), (0, 0));
}

#[test]
fn apply_order_filters_counts_exclude_and_resume_and_drops_host_rows() {
    let target =
        PortagePackage::slotted(Cpn::parse("app-misc/foo").unwrap(), Interned::intern("0"));
    let host = PortagePackage::slotted_at(
        Cpn::parse("app-misc/tool").unwrap(),
        Interned::intern("0"),
        MergeRoot::Host,
    );
    let ver = Version::parse("1").unwrap();
    let order = vec![(target.clone(), ver.clone()), (host, ver.clone())];
    let exclude = [Dep::parse("app-misc/foo").unwrap()];
    let mut resume = HashSet::new();
    resume.insert((MergeRoot::Host, "app-misc/tool-1".to_string()));
    let (kept, excluded, resumed) = apply_order_filters(order, &[], &exclude, &resume, true);
    assert!(kept.is_empty());
    assert_eq!((excluded, resumed), (1, 1));
}

fn masked_names(outcome: &super::DepgraphOutcome) -> Vec<String> {
    outcome
        .masked_installed
        .iter()
        .map(|(cpv, _)| cpv.to_string())
        .collect()
}

#[test]
fn a_masked_installed_package_is_reported_when_nothing_replaces_it() {
    let pkgs = vec![
        Pkg {
            cpv: "app-misc/consumer-1",
            keywords: "amd64",
            depend: "app-misc/bar",
            iuse: "",
            slot: "0",
        },
        Pkg {
            cpv: "app-misc/bar-1",
            keywords: "~amd64",
            depend: "",
            iuse: "",
            slot: "0",
        },
    ];
    let fix = fixture(&pkgs, &[]);
    plant_installed(&fix.root, "app-misc/bar-1", "0", "", "");
    let atoms = [TargetAtom::explicit("app-misc/consumer")];
    let outcome = solve(&fix, Solve::new(&atoms));
    assert_eq!(names(&outcome), ["app-misc/consumer-1"]);
    assert_eq!(masked_names(&outcome), ["app-misc/bar-1"]);
    // The kept package is still a dropped keyword dep, so autounmask fails
    // the plan. The warning itself is not what sets the exit code.
    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn a_masked_installed_package_is_not_reported_when_its_slot_is_upgraded() {
    let pkgs = vec![
        Pkg {
            cpv: "app-misc/other-1",
            keywords: "amd64",
            depend: ">=app-misc/bar-2",
            iuse: "",
            slot: "0",
        },
        Pkg {
            cpv: "app-misc/bar-1",
            keywords: "~amd64",
            depend: "",
            iuse: "",
            slot: "0",
        },
        Pkg {
            cpv: "app-misc/bar-2",
            keywords: "amd64",
            depend: "",
            iuse: "",
            slot: "0",
        },
    ];
    let fix = fixture(&pkgs, &[]);
    plant_installed(&fix.root, "app-misc/bar-1", "0", "", "");
    // The first atom matches only the masked installed version and is dropped.
    let atoms = [
        TargetAtom::explicit("<app-misc/bar-2"),
        TargetAtom::explicit("app-misc/other"),
    ];
    let mut opts = Solve::new(&atoms);
    opts.noreplace = true;
    let outcome = solve(&fix, opts);
    assert_eq!(names(&outcome), ["app-misc/bar-2", "app-misc/other-1"]);
    assert!(
        masked_names(&outcome).is_empty(),
        "masked was {:?}",
        masked_names(&outcome)
    );
}

#[test]
fn a_masked_installed_package_in_another_slot_is_not_reported() {
    let pkgs = vec![
        Pkg {
            cpv: "app-misc/consumer-1",
            keywords: "amd64",
            depend: "app-misc/bar:0",
            iuse: "",
            slot: "0",
        },
        Pkg {
            cpv: "app-misc/bar-1",
            keywords: "~amd64",
            depend: "",
            iuse: "",
            slot: "1",
        },
    ];
    let fix = fixture(&pkgs, &[]);
    plant_installed(&fix.root, "app-misc/bar-1", "0", "", "");
    let atoms = [TargetAtom::explicit("app-misc/consumer")];
    let outcome = solve(&fix, Solve::new(&atoms));
    assert_eq!(names(&outcome), ["app-misc/consumer-1"]);
    assert!(
        masked_names(&outcome).is_empty(),
        "masked was {:?}",
        masked_names(&outcome)
    );
    assert_eq!(outcome.exit_code, 0);
}
