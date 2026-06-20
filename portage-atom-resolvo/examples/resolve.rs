//! Example: resolve a set of real-world Portage dependencies using resolvo.
//!
//! Models a small slice of the Gentoo tree with real package atoms, including
//! transitive deps, `|| ()` any-of (openssl vs libressl), multi-slot Python,
//! versioned constraints, and USE-conditional dependencies.
//!
//! Runs the solver twice — once with `ssl` enabled, once without — to show
//! how USE flags affect the dependency graph.

use std::collections::HashSet;

use portage_atom::{Cpv, Dep};
use portage_atom_resolvo::{
    DepEntry, InMemoryRepository, PackageDeps, PackageMetadata, PortageDependencyProvider,
    UseConfig, interner,
};
use resolvo::{DenseIndex, Problem, Solver, VersionSetId};

/// Shorthand to build a PackageMetadata from a CPV string.
fn pkg(cpv: &str, slot: &str, deps: Vec<DepEntry>) -> PackageMetadata {
    PackageMetadata {
        cpv: Cpv::parse(cpv).unwrap(),
        slot: Some(slot.into()),
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

/// Build a PackageMetadata with a sub-slot (e.g. openssl:0/3.2).
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

/// Build the repository — shared between both solver runs.
fn build_repo() -> InMemoryRepository {
    let mut repo = InMemoryRepository::new();

    // -- sys-libs/zlib: two versions, single slot --
    repo.add(pkg("sys-libs/zlib-1.2.13", "0", vec![]));
    repo.add(pkg("sys-libs/zlib-1.3.1", "0", vec![]));

    // -- app-arch/bzip2 --
    repo.add(pkg("app-arch/bzip2-1.0.8-r4", "0", vec![]));

    // -- dev-libs/expat --
    repo.add(pkg("dev-libs/expat-2.6.2", "0", vec![]));

    // -- dev-libs/openssl: depends on zlib, weak-blocks libressl --
    //    Uses sub-slots :0/3.1 and :0/3.2 for ABI tracking.
    repo.add(pkg_subslot(
        "dev-libs/openssl-3.1.7",
        "0",
        "3.1",
        vec![
            DepEntry::Atom(Dep::parse(">=sys-libs/zlib-1.2.13").unwrap()),
            DepEntry::Atom(Dep::parse("!dev-libs/libressl").unwrap()),
        ],
    ));
    repo.add(pkg_subslot(
        "dev-libs/openssl-3.2.1",
        "0",
        "3.2",
        vec![
            DepEntry::Atom(Dep::parse(">=sys-libs/zlib-1.2.13").unwrap()),
            DepEntry::Atom(Dep::parse("!dev-libs/libressl").unwrap()),
        ],
    ));

    // -- dev-libs/libressl: alternative TLS, needs zlib, strong-blocks openssl --
    repo.add(pkg(
        "dev-libs/libressl-3.9.2",
        "0",
        vec![
            DepEntry::Atom(Dep::parse("sys-libs/zlib").unwrap()),
            DepEntry::Atom(Dep::parse("!!dev-libs/openssl").unwrap()),
        ],
    ));

    // -- media-libs/libpng: needs >=zlib-1.2.13 --
    repo.add(pkg(
        "media-libs/libpng-1.6.43",
        "0",
        vec![DepEntry::Atom(
            Dep::parse(">=sys-libs/zlib-1.2.13").unwrap(),
        )],
    ));

    // -- dev-lang/python: multi-slot, depends on zlib, bzip2, expat --
    //    xml USE flag pulls in dev-libs/expat
    let python_base = vec![
        DepEntry::Atom(Dep::parse(">=sys-libs/zlib-1.2.13").unwrap()),
        DepEntry::Atom(Dep::parse("app-arch/bzip2").unwrap()),
        DepEntry::UseConditional {
            flag: "xml".into(),
            negate: false,
            children: vec![DepEntry::Atom(Dep::parse("dev-libs/expat").unwrap())],
        },
    ];
    repo.add(pkg("dev-lang/python-3.11.9", "3.11", python_base.clone()));
    repo.add(pkg("dev-lang/python-3.12.4", "3.12", python_base));

    // -- dev-python/certifi: needs any python slot (:*) --
    repo.add(pkg(
        "dev-python/certifi-2024.2.2",
        "0",
        vec![DepEntry::Atom(Dep::parse("dev-lang/python:*").unwrap())],
    ));

    // -- net-misc/curl --
    //   always: >=sys-libs/zlib-1.2.13
    //   always: || ( dev-libs/openssl dev-libs/libressl )
    //   ssl?  : dev-python/certifi  (CA cert bundle)
    repo.add(pkg(
        "net-misc/curl-8.7.1",
        "0",
        vec![
            DepEntry::Atom(Dep::parse(">=sys-libs/zlib-1.2.13").unwrap()),
            DepEntry::AnyOf(vec![
                DepEntry::Atom(Dep::parse("dev-libs/openssl").unwrap()),
                DepEntry::Atom(Dep::parse("dev-libs/libressl").unwrap()),
            ]),
            DepEntry::UseConditional {
                flag: "ssl".into(),
                negate: false,
                children: vec![DepEntry::Atom(Dep::parse("dev-python/certifi").unwrap())],
            },
        ],
    ));

    // -- app-portage/gentoolkit: needs python:3.12, certifi --
    repo.add(pkg(
        "app-portage/gentoolkit-0.6.3",
        "0",
        vec![
            DepEntry::Atom(Dep::parse("dev-lang/python:3.12").unwrap()),
            DepEntry::Atom(Dep::parse("dev-python/certifi").unwrap()),
        ],
    ));

    // -- www-client/firefox: both python slots, libpng, >=openssl-3.2:0= (rebuild trigger) --
    repo.add(pkg(
        "www-client/firefox-125.0.3",
        "0",
        vec![
            DepEntry::Atom(Dep::parse("dev-lang/python:3.11").unwrap()),
            DepEntry::Atom(Dep::parse("dev-lang/python:3.12").unwrap()),
            DepEntry::Atom(Dep::parse("media-libs/libpng").unwrap()),
            DepEntry::Atom(Dep::parse(">=dev-libs/openssl-3.2.0:0=").unwrap()),
        ],
    ));

    // -- dev-python/sphinx: documentation builder, needs python --
    repo.add(pkg(
        "dev-python/sphinx-7.0.0",
        "0",
        vec![DepEntry::Atom(Dep::parse("dev-lang/python:*").unwrap())],
    ));

    // -- app-doc/pygDoc: uses python_gen_any_dep pattern --
    //   || ( ( python:3.12 sphinx ) ( python:3.11 sphinx ) )
    //   Both packages in a group must be installed together.
    repo.add(pkg(
        "app-doc/pygdoc-1.0",
        "0",
        vec![DepEntry::AnyOf(vec![
            DepEntry::AllOf(vec![
                DepEntry::Atom(Dep::parse("dev-lang/python:3.12").unwrap()),
                DepEntry::Atom(Dep::parse("dev-python/sphinx").unwrap()),
            ]),
            DepEntry::AllOf(vec![
                DepEntry::Atom(Dep::parse("dev-lang/python:3.11").unwrap()),
                DepEntry::Atom(Dep::parse("dev-python/sphinx").unwrap()),
            ]),
        ])],
    ));

    repo
}

/// Pretty-print the declared dependency tree for a package.
fn print_deps(indent: usize, entries: &[DepEntry]) {
    let pad = " ".repeat(indent);
    for entry in entries {
        match entry {
            DepEntry::Atom(dep) => println!("{pad}{dep}"),
            DepEntry::AnyOf(children) => {
                println!("{pad}|| (");
                print_deps(indent + 4, children);
                println!("{pad})");
            }
            DepEntry::UseConditional {
                flag,
                negate,
                children,
            } => {
                let prefix = if *negate { "!" } else { "" };
                println!("{pad}{prefix}{flag}? (");
                print_deps(indent + 4, children);
                println!("{pad})");
            }
            DepEntry::ExactlyOneOf(children) => {
                println!("{pad}^^ (");
                print_deps(indent + 4, children);
                println!("{pad})");
            }
            DepEntry::AtMostOneOf(children) => {
                println!("{pad}?? (");
                print_deps(indent + 4, children);
                println!("{pad})");
            }
            DepEntry::AllOf(children) => {
                println!("{pad}(");
                print_deps(indent + 4, children);
                println!("{pad})");
            }
        }
    }
}

/// Run the solver with a given USE config and print the result.
fn solve_and_print(repo: &InMemoryRepository, use_config: &UseConfig) {
    let mut provider = PortageDependencyProvider::new(repo, use_config);

    let root_atoms = [
        "net-misc/curl",
        "app-portage/gentoolkit",
        "www-client/firefox",
        "app-doc/pygdoc",
    ];
    let reqs: Vec<_> = root_atoms
        .iter()
        .map(|s| provider.intern_requirement(&Dep::parse(s).unwrap()))
        .collect();
    let problem = Problem::new().requirements(reqs);

    let mut solver = Solver::new(provider);
    match solver.solve(problem) {
        Ok(solution) => {
            let provider = solver.provider();
            let mut pkgs: Vec<_> = solution
                .iter()
                .map(|&sid| {
                    let meta = provider.package_metadata(sid);
                    let slot = meta.slot.as_deref().unwrap_or("0");
                    match &meta.subslot {
                        Some(sub) => format!("{:<45} :{}/{}", meta.cpv, slot, sub),
                        None => format!("{:<45} :{}", meta.cpv, slot),
                    }
                })
                .collect();
            pkgs.sort();
            println!("  Solution ({} packages):", pkgs.len());
            for p in &pkgs {
                println!("    {p}");
            }

            // Show recorded blocker types.
            let pool = provider.pool();
            let mut blockers = Vec::new();
            for i in 0..pool.version_set_count() {
                let vs_id = VersionSetId::from_index(i);
                if let Some(blocker) = provider.blocker_type(vs_id) {
                    let constraint = pool.resolve_version_set(vs_id);
                    let kind = match blocker {
                        portage_atom::Blocker::Weak => "weak (!)",
                        portage_atom::Blocker::Strong => "strong (!!)",
                    };
                    blockers.push(format!("    {kind:<14} {constraint}"));
                }
            }
            if !blockers.is_empty() {
                blockers.sort();
                blockers.dedup();
                println!("  Active blockers:");
                for b in &blockers {
                    println!("{b}");
                }
            }

            // Show rebuild triggers (:=).
            let mut triggers = Vec::new();
            for i in 0..pool.version_set_count() {
                let vs_id = VersionSetId::from_index(i);
                if provider.is_rebuild_trigger(vs_id) {
                    let constraint = pool.resolve_version_set(vs_id);
                    triggers.push(format!("    {constraint}"));
                }
            }
            if !triggers.is_empty() {
                triggers.sort();
                triggers.dedup();
                println!("  Rebuild triggers (:=):");
                for t in &triggers {
                    println!("{t}");
                }
            }
        }
        Err(e) => {
            eprintln!("  Resolution failed: {e:?}");
        }
    }
}

fn main() {
    let repo = build_repo();

    // ── Show the repository ─────────────────────────────────────────
    println!("Repository:\n");
    println!("  sys-libs/zlib          1.2.13, 1.3.1           :0");
    println!("  app-arch/bzip2         1.0.8-r4                :0");
    println!("  dev-libs/expat         2.6.2                   :0");
    println!("  dev-libs/openssl       3.1.7 :0/3.1, 3.2.1 :0/3.2");
    println!("  dev-libs/libressl      3.9.2                   :0");
    println!("  media-libs/libpng      1.6.43                  :0");
    println!("  dev-lang/python        3.11.9 :3.11, 3.12.4 :3.12");
    println!("  dev-python/certifi     2024.2.2                :0");
    println!("  net-misc/curl          8.7.1                   :0");
    println!("  app-portage/gentoolkit 0.6.3                   :0");
    println!("  www-client/firefox     125.0.3                 :0");
    println!("  dev-python/sphinx      7.0.0                   :0");
    println!("  app-doc/pygdoc         1.0                     :0");

    // ── Show declared dependencies ──────────────────────────────────
    println!("\nDeclared dependencies:\n");

    let pkgs_with_deps: Vec<(&str, Vec<DepEntry>)> = vec![
        (
            "dev-libs/openssl-3.2.1",
            vec![
                DepEntry::Atom(Dep::parse(">=sys-libs/zlib-1.2.13").unwrap()),
                DepEntry::Atom(Dep::parse("!dev-libs/libressl").unwrap()),
            ],
        ),
        (
            "dev-libs/libressl-3.9.2",
            vec![
                DepEntry::Atom(Dep::parse("sys-libs/zlib").unwrap()),
                DepEntry::Atom(Dep::parse("!!dev-libs/openssl").unwrap()),
            ],
        ),
        (
            "media-libs/libpng-1.6.43",
            vec![DepEntry::Atom(
                Dep::parse(">=sys-libs/zlib-1.2.13").unwrap(),
            )],
        ),
        (
            "dev-lang/python-3.{11,12}",
            vec![
                DepEntry::Atom(Dep::parse(">=sys-libs/zlib-1.2.13").unwrap()),
                DepEntry::Atom(Dep::parse("app-arch/bzip2").unwrap()),
                DepEntry::UseConditional {
                    flag: "xml".into(),
                    negate: false,
                    children: vec![DepEntry::Atom(Dep::parse("dev-libs/expat").unwrap())],
                },
            ],
        ),
        (
            "dev-python/certifi-2024.2.2",
            vec![DepEntry::Atom(Dep::parse("dev-lang/python:*").unwrap())],
        ),
        (
            "net-misc/curl-8.7.1",
            vec![
                DepEntry::Atom(Dep::parse(">=sys-libs/zlib-1.2.13").unwrap()),
                DepEntry::AnyOf(vec![
                    DepEntry::Atom(Dep::parse("dev-libs/openssl").unwrap()),
                    DepEntry::Atom(Dep::parse("dev-libs/libressl").unwrap()),
                ]),
                DepEntry::UseConditional {
                    flag: "ssl".into(),
                    negate: false,
                    children: vec![DepEntry::Atom(Dep::parse("dev-python/certifi").unwrap())],
                },
            ],
        ),
        (
            "app-portage/gentoolkit-0.6.3",
            vec![
                DepEntry::Atom(Dep::parse("dev-lang/python:3.12").unwrap()),
                DepEntry::Atom(Dep::parse("dev-python/certifi").unwrap()),
            ],
        ),
        (
            "www-client/firefox-125.0.3",
            vec![
                DepEntry::Atom(Dep::parse("dev-lang/python:3.11").unwrap()),
                DepEntry::Atom(Dep::parse("dev-lang/python:3.12").unwrap()),
                DepEntry::Atom(Dep::parse("media-libs/libpng").unwrap()),
                DepEntry::Atom(Dep::parse(">=dev-libs/openssl-3.2.0:0=").unwrap()),
            ],
        ),
        (
            "app-doc/pygdoc-1.0",
            vec![DepEntry::AnyOf(vec![
                DepEntry::AllOf(vec![
                    DepEntry::Atom(Dep::parse("dev-lang/python:3.12").unwrap()),
                    DepEntry::Atom(Dep::parse("dev-python/sphinx").unwrap()),
                ]),
                DepEntry::AllOf(vec![
                    DepEntry::Atom(Dep::parse("dev-lang/python:3.11").unwrap()),
                    DepEntry::Atom(Dep::parse("dev-python/sphinx").unwrap()),
                ]),
            ])],
        ),
    ];

    for (name, deps) in &pkgs_with_deps {
        println!("  {name}:");
        print_deps(4, deps);
    }

    // ── Root requirements ───────────────────────────────────────────
    println!("\nRoot requirements:");
    println!("  net-misc/curl");
    println!("  app-portage/gentoolkit");
    println!("  www-client/firefox");
    println!("  app-doc/pygdoc");

    // ── Solve with USE="ssl xml" ────────────────────────────────────
    let flags_on = UseConfig::from(
        ["ssl", "xml"]
            .iter()
            .map(|s| interner::Interned::intern(s))
            .collect::<HashSet<_>>(),
    );
    println!(
        "\n{}\nUSE=\"ssl xml\"  (eager)\n{}",
        "=".repeat(60),
        "=".repeat(60),
    );
    solve_and_print(&repo, &flags_on);

    // ── Solve with USE="-ssl xml" ───────────────────────────────────
    let flags_no_ssl = UseConfig::from(
        ["xml"]
            .iter()
            .map(|s| interner::Interned::intern(s))
            .collect::<HashSet<_>>(),
    );
    println!(
        "\n{}\nUSE=\"xml -ssl\"  (eager)\n{}",
        "=".repeat(60),
        "=".repeat(60),
    );
    solve_and_print(&repo, &flags_no_ssl);

    // ── Solve with USE="-ssl -xml" ──────────────────────────────────
    println!(
        "\n{}\nUSE=\"-ssl -xml\"  (minimal)\n{}",
        "=".repeat(60),
        "=".repeat(60),
    );
    solve_and_print(&repo, &UseConfig::default());

    // ── Solve with xml enabled, ssl solver-decided ──────────────────
    let flags_solver = UseConfig {
        enabled: ["xml"]
            .iter()
            .map(|s| interner::Interned::intern(s))
            .collect(),
        solver_decided: ["ssl"]
            .iter()
            .map(|s| interner::Interned::intern(s))
            .collect(),
        ..UseConfig::default()
    };
    println!(
        "\n{}\nUSE=\"xml\" + ssl=solver-decided\n{}",
        "=".repeat(60),
        "=".repeat(60),
    );
    solve_and_print(&repo, &flags_solver);
}
