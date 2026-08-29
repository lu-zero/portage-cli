use super::*;
use tempfile::tempdir;

#[tokio::test]
async fn test_use_flags() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().to_path_buf();

    // Create a minimal repository structure
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::create_dir_all(repo_path.join("eclass")).unwrap();

    // Write minimal layout.conf
    std::fs::write(
        repo_path.join("metadata").join("layout.conf"),
        "masters = \ncache-formats = md5-dict\n",
    )
    .unwrap();

    // Write repo_name
    std::fs::write(repo_path.join("profiles").join("repo_name"), "test-repo\n").unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    // Test setting USE flags
    shell.set_use_flags(&["ssl", "gtk", "-doc"]).unwrap();
    assert_eq!(shell.use_flags_string(), "gtk ssl");

    // Test that USE environment variable is set
    let use_env = shell.get_var("USE").unwrap_or_default();
    assert!(use_env.contains("ssl"));
    assert!(use_env.contains("gtk"));
    assert!(!use_env.contains("doc"));
}

#[tokio::test]
async fn eclass_search_path_prefers_own_repo_over_masters() {
    // Build an overlay with two masters (m1, m2). The search path must put
    // the overlay's own eclass/ first, then masters in reverse order, so
    // first-hit-wins matches portage's last-writer-wins (own > m2 > m1).
    fn mk_repo(base: &std::path::Path, name: &str) -> std::path::PathBuf {
        let p = base.join(name);
        std::fs::create_dir_all(p.join("metadata")).unwrap();
        std::fs::create_dir_all(p.join("profiles")).unwrap();
        std::fs::create_dir_all(p.join("eclass")).unwrap();
        std::fs::write(p.join("metadata/layout.conf"), "masters = \n").unwrap();
        std::fs::write(p.join("profiles/repo_name"), format!("{name}\n")).unwrap();
        p
    }

    let dir = tempdir().unwrap();
    let base = dir.path();
    let m1 = mk_repo(base, "m1");
    let m2 = mk_repo(base, "m2");
    let own = mk_repo(base, "own");

    let m1_repo = Repository::builder().in_memory_cache().open(&m1).unwrap();
    let m2_repo = Repository::builder().in_memory_cache().open(&m2).unwrap();
    let own_repo = Repository::builder().in_memory_cache().open(&own).unwrap();

    let shell = own_repo
        .shell_with_masters(&[&m1_repo, &m2_repo])
        .await
        .unwrap();

    let dirs = shell.get_var("__PORTAGE_ECLASS_DIRS").unwrap_or_default();
    let expected = format!(
        "{}:{}:{}",
        own.join("eclass").display(),
        m2.join("eclass").display(),
        m1.join("eclass").display(),
    );
    assert_eq!(dirs, expected);
}

#[tokio::test]
async fn reused_shell_does_not_leak_metadata_between_ebuilds() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().to_path_buf();
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::create_dir_all(repo_path.join("dev-libs/foo")).unwrap();
    std::fs::write(
        repo_path.join("metadata/layout.conf"),
        "masters = 
cache-formats = md5-dict
",
    )
    .unwrap();
    std::fs::write(
        repo_path.join("profiles/repo_name"),
        "test-repo
",
    )
    .unwrap();
    // First ebuild sets KEYWORDS; the second (a live-style ebuild)
    // deliberately leaves it unset — it must not inherit the first's.
    std::fs::write(
        repo_path.join("dev-libs/foo/foo-1.0.ebuild"),
        concat!(
            "EAPI=8\n",
            "DESCRIPTION=\"release\"\n",
            "SLOT=\"0\"\n",
            "LICENSE=\"MIT\"\n",
            "KEYWORDS=\"~amd64 ~arm64\"\n",
        ),
    )
    .unwrap();
    std::fs::write(
        repo_path.join("dev-libs/foo/foo-9999.ebuild"),
        concat!(
            "EAPI=8\n",
            "DESCRIPTION=\"live\"\n",
            "SLOT=\"0\"\n",
            "LICENSE=\"MIT\"\n",
        ),
    )
    .unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let release = Ebuild::from_path(
        camino::Utf8Path::from_path(&repo_path.join("dev-libs/foo/foo-1.0.ebuild")).unwrap(),
    )
    .unwrap();
    let live = Ebuild::from_path(
        camino::Utf8Path::from_path(&repo_path.join("dev-libs/foo/foo-9999.ebuild")).unwrap(),
    )
    .unwrap();

    let first = shell.source_ebuild(&release).await.unwrap();
    assert_eq!(first.metadata.keywords.len(), 2);
    let second = shell.source_ebuild(&live).await.unwrap();
    assert!(
        second.metadata.keywords.is_empty(),
        "live ebuild must not inherit the previous sourcing's KEYWORDS: {:?}",
        second.metadata.keywords
    );
}
// `has_version`/`best_version` builtins query the VDB under the root the
// -b/-d/-r flag names; phase shells unset the metadata-sourcing stubs so
// the builtins take over (the stub shadowed them and made
// autotools.eclass's autoconf probe die in every build).
#[tokio::test]
async fn version_query_builtins_query_the_flagged_root() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();

    // Synthetic BROOT with one installed package.
    let broot = dir.path().join("broot");
    let pkgdir = broot.join("var/db/pkg/dev-build/autoconf-2.73-r1");
    std::fs::create_dir_all(&pkgdir).unwrap();
    std::fs::write(pkgdir.join("SLOT"), "2.73\n").unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    super::commands::set_tool_mode(&mut shell.shell, super::commands::ToolMode::Build);
    shell
        .run_string(&format!(
            "BROOT={}; \
             has_version -b '=dev-build/autoconf-2.73*' && HV=yes || HV=no; \
             BV=$(best_version -b '=dev-build/autoconf-2.73*'); \
             has_version -b 'dev-build/automake' && HV2=yes || HV2=no",
            broot.display()
        ))
        .await
        .unwrap();
    assert_eq!(shell.get_var("HV").as_deref(), Some("yes"));
    assert_eq!(
        shell.get_var("BV").as_deref(),
        Some("dev-build/autoconf-2.73-r1")
    );
    assert_eq!(shell.get_var("HV2").as_deref(), Some("no"));
}

#[tokio::test]
async fn bashrc_files_are_sourced_during_a_phase() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();
    let ebdir = repo_path.join("cat/pkg");
    std::fs::create_dir_all(&ebdir).unwrap();
    std::fs::write(
        ebdir.join("pkg-1.ebuild"),
        "EAPI=8\nDESCRIPTION=\"t\"\nSLOT=\"0\"\nLICENSE=\"MIT\"\nS=\"${WORKDIR}\"\npkg_setup() { :; }\n",
    )
    .unwrap();

    // A bashrc hook that records that it ran with the phase env available.
    let bashrc = dir.path().join("bashrc");
    std::fs::write(&bashrc, "export EM_BASHRC_MARKER=\"hit:${EBUILD_PHASE}\"\n").unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    shell.set_bashrc_files(vec![Utf8PathBuf::from_path_buf(bashrc).unwrap()]);

    let ebuild =
        Ebuild::from_path(camino::Utf8Path::from_path(&ebdir.join("pkg-1.ebuild")).unwrap())
            .unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    assert_eq!(
        shell.get_var("EM_BASHRC_MARKER").as_deref(),
        Some("hit:setup")
    );
}

// Profile/user bashrc `die` must abort the phase (regression: die_flag was
// cleared after bashrc, so merged-usr checks were no-ops — 2026-08-07).
#[tokio::test]
async fn bashrc_die_aborts_the_phase() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();
    let ebdir = repo_path.join("cat/pkg");
    std::fs::create_dir_all(&ebdir).unwrap();
    std::fs::write(
        ebdir.join("pkg-1.ebuild"),
        "EAPI=8\nDESCRIPTION=\"t\"\nSLOT=\"0\"\nLICENSE=\"MIT\"\nS=\"${WORKDIR}\"\n\
         pkg_setup() { export EM_PHASE_RAN=1; }\n",
    )
    .unwrap();

    let bashrc = dir.path().join("profile.bashrc");
    std::fs::write(
        &bashrc,
        "die \"merged-usr profile, but disk is split-usr\"\n",
    )
    .unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    shell.set_bashrc_files(vec![Utf8PathBuf::from_path_buf(bashrc).unwrap()]);

    let ebuild =
        Ebuild::from_path(camino::Utf8Path::from_path(&ebdir.join("pkg-1.ebuild")).unwrap())
            .unwrap();
    let work = dir.path().join("work");
    let err = shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .expect_err("bashrc die must fail the phase");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("merged-usr") || msg.contains("die"),
        "unexpected error: {msg}"
    );
    assert!(
        shell.get_var("EM_PHASE_RAN").is_none(),
        "pkg_setup body must not run after bashrc die"
    );
}

#[tokio::test]
async fn phase_aborts_on_die_not_on_trailing_exit() {
    // Portage aborts a phase only via `die` (helpers self-die; `eapply` /
    // explicit `die` raise it), NOT from the phase function's trailing exit
    // status. `run_phase` must match: a phase ending on a benign non-zero
    // command (e.g. binutils' `find … -exec rmdir {} +`) must NOT abort,
    // while an explicit `die` must. Regression for the cross-toolchain
    // binutils `src_install` that ends on a non-zero `find … rmdir`.
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();
    let ebdir = repo_path.join("cat/pkg");
    std::fs::create_dir_all(&ebdir).unwrap();
    std::fs::write(
        ebdir.join("pkg-1.ebuild"),
        "EAPI=8\nDESCRIPTION=\"t\"\nSLOT=\"0\"\nLICENSE=\"MIT\"\nS=\"${WORKDIR}\"\n\
         pkg_setup() { :; }\n",
    )
    .unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let ebuild =
        Ebuild::from_path(camino::Utf8Path::from_path(&ebdir.join("pkg-1.ebuild")).unwrap())
            .unwrap();
    let work = dir.path().join("work");

    // A first, succeeding phase sources the ebuild and captures the
    // baseline, so the phases below run the function body only.
    shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    // A phase ending on a non-zero command (no `die`) is tolerated — it
    // must NOT abort the build.
    shell
        .run_string("src_compile() { true; false; }")
        .await
        .unwrap();
    shell
        .run_phase(&ebuild, "compile", &work, std::path::Path::new("/"))
        .await
        .expect("a benign trailing non-zero must not abort the phase");

    // An explicit `die` (as the helpers raise on failure) must abort.
    shell
        .run_string("src_test() { die \"boom\"; }")
        .await
        .unwrap();
    let err = shell
        .run_phase(&ebuild, "test", &work, std::path::Path::new("/"))
        .await
        .expect_err("an explicit die must abort the build");
    let msg = format!("{err}");
    assert!(
        msg.contains("die") && msg.contains("src_test"),
        "expected the die/phase name in the error, got: {msg}"
    );
}

// Regression: `stubs.rs` used to define a bash function `nonfatal() { "$@";
// return 0; }`. A bash function always shadows a same-named builtin in this
// shell (same class of bug as eapply's, todo/eapply-stub-shadows-real-
// builtin.md), so the real `NonfatalCommand` builtin -- whose whole job is
// scoping `PORTAGE_NONFATAL=1` around its argument -- never ran, and `die -n`
// inside `nonfatal` always took the fatal path since `PORTAGE_NONFATAL` was
// never set. `nonfatal die -n ...` must instead report failure without
// aborting the phase (like a real `-n`-honouring die), and execution must
// continue afterward.
#[tokio::test]
async fn nonfatal_die_dash_n_does_not_abort_the_phase() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();
    let ebdir = repo_path.join("cat/pkg");
    std::fs::create_dir_all(&ebdir).unwrap();
    std::fs::write(
        ebdir.join("pkg-1.ebuild"),
        "EAPI=8\nDESCRIPTION=\"t\"\nSLOT=\"0\"\nLICENSE=\"MIT\"\nS=\"${WORKDIR}\"\n\
         pkg_setup() { nonfatal die -n \"boom\"; export EM_PHASE_RAN=1; }\n",
    )
    .unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let ebuild =
        Ebuild::from_path(camino::Utf8Path::from_path(&ebdir.join("pkg-1.ebuild")).unwrap())
            .unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .expect("`nonfatal die -n` must not abort the phase");
    assert!(
        shell.get_var("EM_PHASE_RAN").is_some(),
        "pkg_setup must continue past the nonfatal die"
    );
}

// Companion case from the same regression: `nonfatal false` must likewise
// not abort the phase (a plain non-zero exit was never fatal on its own —
// see `phase_aborts_on_die_not_on_trailing_exit` above — so this mainly
// guards that the real builtin's argument dispatch doesn't itself error out).
#[tokio::test]
async fn nonfatal_false_does_not_abort_the_phase() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();
    let ebdir = repo_path.join("cat/pkg");
    std::fs::create_dir_all(&ebdir).unwrap();
    std::fs::write(
        ebdir.join("pkg-1.ebuild"),
        "EAPI=8\nDESCRIPTION=\"t\"\nSLOT=\"0\"\nLICENSE=\"MIT\"\nS=\"${WORKDIR}\"\n\
         pkg_setup() { nonfatal false; export EM_PHASE_RAN=1; }\n",
    )
    .unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let ebuild =
        Ebuild::from_path(camino::Utf8Path::from_path(&ebdir.join("pkg-1.ebuild")).unwrap())
            .unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .expect("`nonfatal false` must not abort the phase");
    assert!(
        shell.get_var("EM_PHASE_RAN").is_some(),
        "pkg_setup must continue past nonfatal false"
    );
}

#[tokio::test]
async fn einstall_enforces_eapi_ban_and_requires_a_makefile() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    shell
        .run_string(&format!(
            "cd {}; \
             EAPI=6; einstall 2>/dev/null && BAN=ok || BAN=died; \
             EAPI=5; einstall 2>/dev/null && NOMK=ok || NOMK=died",
            empty.display()
        ))
        .await
        .unwrap();
    // Banned in EAPI 6+, and dies on a missing Makefile in EAPI 5.
    assert_eq!(shell.get_var("BAN").as_deref(), Some("died"));
    assert_eq!(shell.get_var("NOMK").as_deref(), Some("died"));
}

// `use_with`/`use_enable`'s explicit-empty second argument
// (`use_with brotli '' link`, as `net-libs/gnutls` calls it) must fall
// back to the flag name, matching bash's `${2:-$1}` in real portage's
// `use_with()` — not just an omitted argument. An empty `Option<String>`
// still satisfies `Option::unwrap_or`'s `Some` case, so a naive
// translation silently drops the feature name entirely, producing
// `--without-` instead of `--without-brotli` (which `./configure` then
// warns is unrecognized and ignores, leaving the feature auto-detected
// regardless of the requested USE flag).
#[tokio::test]
async fn use_with_and_use_enable_treat_empty_feature_arg_as_omitted() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    shell.set_use_flags(&["-brotli", "cxx"]).unwrap();

    shell
        .run_string(
            "WITH_OUT=$(use_with brotli '' link); \
             ENABLE_OUT=$(use_enable cxx '')",
        )
        .await
        .unwrap();
    assert_eq!(
        shell.get_var("WITH_OUT").as_deref(),
        Some("--without-brotli")
    );
    assert_eq!(shell.get_var("ENABLE_OUT").as_deref(), Some("--enable-cxx"));
}

// A profile/make.conf-sourced variable must reach a *real* subprocess an
// ebuild/eclass spawns directly — not just brush's in-process variable
// table (which is all `get_var`/em's Rust builtins need). `MULTILIB_ABIS`
// stands in for any such variable em doesn't specifically know about
// (this is the exact shape of the CHOST bug: invisible to a real child
// process, even though brush itself sees it fine).
#[tokio::test]
async fn export_sourced_env_reaches_a_real_subprocess() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    // Plain (non-exported) assignment — exactly what `source`ing a
    // make.conf-style file produces.
    shell.run_string("MULTILIB_ABIS=lp64d").await.unwrap();
    shell.export_sourced_env().unwrap();

    // A real external command, not a brush builtin: only sees inherited
    // (exported) process environment variables.
    shell
        .run_string("OUT=$(/bin/sh -c 'printf %s \"$MULTILIB_ABIS\"')")
        .await
        .unwrap();
    assert_eq!(
        shell.get_var("OUT").as_deref(),
        Some("lp64d"),
        "a real subprocess must inherit a profile/make.conf-sourced var after export_sourced_env"
    );
}

#[tokio::test]
async fn install_helpers_are_self_contained() {
    // The do*/new* helpers must place files purely from INSTALL_HELPERS,
    // with no portage ebuild-helpers on PATH. Verifies the into->DESTTREE
    // mirror and the env.d/conf.d/init.d (do*/new*) helpers.
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();

    let d = dir.path().join("image");
    let t = dir.path().join("temp");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("myprog"), "#!/bin/sh\n:\n").unwrap();
    std::fs::write(src.join("foo.conf"), "X=1\n").unwrap();
    std::fs::write(src.join("foo.envd"), "PATH=/opt/foo/bin\n").unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    // init_build_env no longer prepends portage's ebuild-helpers to PATH,
    // so these helpers must resolve entirely from INSTALL_HELPERS (they
    // still use coreutils like install/cp, which stay on the system PATH).
    shell
        .run_string(&format!(
            "{INSTALL_HELPERS}\n\
             export D={d} ED={d} T={t} CATEGORY=cat PN=pkg SLOT=0 PF=pkg-1; \
             into /usr/local; dobin {src}/myprog; \
             [[ ${{DESTTREE}} == /usr/local ]] || die 'into did not set DESTTREE'; \
             newconfd {src}/foo.conf renamed.conf; \
             doenvd {src}/foo.envd; \
             newinitd {src}/myprog svc",
            d = d.display(),
            t = t.display(),
            src = src.display(),
        ))
        .await
        .unwrap();

    assert!(
        d.join("usr/local/bin/myprog").exists(),
        "dobin into /usr/local"
    );
    assert!(d.join("etc/conf.d/renamed.conf").exists(), "newconfd");
    assert!(d.join("etc/env.d/foo.envd").exists(), "doenvd");
    assert!(d.join("etc/init.d/svc").exists(), "newinitd");
}

// Regression (live 2026-08-28, dev-build/make-4.4.1-r102's own
// `DOCS="AUTHORS NEWS README*"`): a scalar DOCS containing a glob pattern
// must still expand it, matching real bash's unquoted `dodoc -r ${DOCS}` —
// quoting each split word (the first attempt at this einstalldocs port)
// passed the literal string "README*" straight to dodoc as a filename.
#[tokio::test]
async fn einstalldocs_expands_a_glob_pattern_in_a_scalar_docs() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();

    let d = dir.path().join("image");
    let t = dir.path().join("temp");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("AUTHORS"), "me\n").unwrap();
    std::fs::write(src.join("README.rst"), "hi\n").unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    shell
        .run_string(&format!(
            "export D={d} ED={d} T={t} CATEGORY=cat PN=pkg SLOT=0 PF=pkg-1; \
             DOCS=\"AUTHORS README*\"; \
             cd {src}; \
             einstalldocs",
            d = d.display(),
            t = t.display(),
            src = src.display(),
        ))
        .await
        .unwrap();

    assert!(
        d.join("usr/share/doc/pkg-1/AUTHORS").exists(),
        "exact-name scalar DOCS entry"
    );
    assert!(
        d.join("usr/share/doc/pkg-1/README.rst").exists(),
        "glob-pattern scalar DOCS entry must expand, not install a literal 'README*'"
    );
}

// PMS Algorithm 12.4: the README*/AUTHORS/… fallback list only applies when
// DOCS is *unset*. A declared-but-empty DOCS must install nothing, even
// though a real README sits right there for the fallback to find.
#[tokio::test]
async fn einstalldocs_empty_docs_installs_nothing() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();

    let d = dir.path().join("image");
    let t = dir.path().join("temp");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("README"), "hi\n").unwrap();
    std::fs::write(src.join("AUTHORS"), "me\n").unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    shell
        .run_string(&format!(
            "export D={d} ED={d} T={t} CATEGORY=cat PN=pkg SLOT=0 PF=pkg-1; \
             DOCS=\"\"; \
             cd {src}; \
             einstalldocs",
            d = d.display(),
            t = t.display(),
            src = src.display(),
        ))
        .await
        .unwrap();

    assert!(
        !d.join("usr/share/doc/pkg-1").exists(),
        "DOCS=\"\" is declared-but-empty, not unset -- fallback list must not run"
    );
}

// Companion to the above: with DOCS genuinely unset, the fallback list does
// run and installs whatever of it is present.
#[tokio::test]
async fn einstalldocs_unset_docs_uses_fallback_list() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();

    let d = dir.path().join("image");
    let t = dir.path().join("temp");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::create_dir_all(&t).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("README"), "hi\n").unwrap();
    std::fs::write(src.join("AUTHORS"), "me\n").unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    shell
        .run_string(&format!(
            "export D={d} ED={d} T={t} CATEGORY=cat PN=pkg SLOT=0 PF=pkg-1; \
             cd {src}; \
             einstalldocs",
            d = d.display(),
            t = t.display(),
            src = src.display(),
        ))
        .await
        .unwrap();

    assert!(
        d.join("usr/share/doc/pkg-1/README").exists(),
        "DOCS unset -- fallback list must install README"
    );
    assert!(
        d.join("usr/share/doc/pkg-1/AUTHORS").exists(),
        "DOCS unset -- fallback list must install AUTHORS"
    );
}

#[tokio::test]
async fn new_helpers_read_stdin_for_dash_source() {
    // `newins - <name>` (and every new* with `-`) reads the file body from
    // stdin — e.g. acct-group.eclass's `newins - foo.conf < <(…)`. Here a
    // here-string feeds the builtin's stdin; the content must land under the
    // requested name. newman additionally derives the section from the name.
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();

    let d = dir.path().join("image");
    let t = dir.path().join("temp");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::create_dir_all(&t).unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    shell
        .run_string(&format!(
            "{INSTALL_HELPERS}\n\
             export D={d} ED={d} T={t} CATEGORY=cat PN=pkg SLOT=0 PF=pkg-1; \
             newins - etc.conf <<< 'KEY=value'; \
             newman - app.1 <<< '.TH app 1'",
            d = d.display(),
            t = t.display(),
        ))
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(d.join("etc.conf")).unwrap(),
        "KEY=value\n",
        "newins - reads stdin into the named file"
    );
    assert!(
        d.join("usr/share/man/man1/app.1").exists(),
        "newman - derives the section from the name"
    );
}

#[tokio::test]
async fn docompress_dostrip_builtins_accumulate_shared_lists() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    shell
        .run_string(
            "docompress /opt/data /usr/share/extra; \
             docompress -x /usr/share/doc/foo/html; \
             dostrip /usr/lib/debug-me; \
             dostrip -x /usr/lib/keep.so",
        )
        .await
        .unwrap();

    let paths = shell.install_paths();
    assert_eq!(paths.compress, ["/opt/data", "/usr/share/extra"]);
    assert_eq!(paths.compress_exclude, ["/usr/share/doc/foo/html"]);
    assert_eq!(paths.strip, ["/usr/lib/debug-me"]);
    assert_eq!(paths.strip_exclude, ["/usr/lib/keep.so"]);
}

async fn minimal_shell(dir: &std::path::Path) -> EbuildShell {
    let repo_path = dir.join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();
    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    repo.shell().await.unwrap()
}

// Regression test for the 2026-07-16 fix: the cross-toolchain PATH/CC
// selection used to derive its bin dir from `build_config_root`
// (`PORTAGE_CONFIGROOT`) — a proxy that only coincidentally matched the
// crossdev sysroot layout. It must instead come from `build_broot`
// (`Cli::host_roots()`'s merge root — the real host for a privileged `--root`,
// the prefix itself for an unprivileged `--prefix` overlay), so a `${CHOST}-
// gcc` built into the prefix (not the host) is still found.
#[tokio::test]
async fn cross_toolchain_selection_uses_broot_not_config_root() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    let broot = dir.path().join("broot");
    let bin = broot.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gcc = bin.join("riscv64-unknown-linux-gnu-gcc");
    std::fs::write(&gcc, "#!/bin/sh\n:\n").unwrap();

    let broot_utf8 = Utf8PathBuf::from_path_buf(broot.clone()).unwrap();
    // config_root deliberately left as a decoy under a different directory,
    // with no `usr/bin` of its own — proves the bin dir comes from broot,
    // not from build_config_root the way it used to.
    let decoy_config_root =
        Utf8PathBuf::from_path_buf(dir.path().join("decoy/usr/riscv64-unknown-linux-gnu")).unwrap();
    shell.set_build_roots(
        Some(&decoy_config_root),
        None,
        None,
        Some(&broot_utf8),
        None,
    );

    shell.set_var("CHOST", "riscv64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.init_build_env().await.unwrap();

    let expected_cc = gcc.to_str().unwrap().to_string();
    assert_eq!(shell.get_var("CC").as_deref(), Some(expected_cc.as_str()));
    let path = shell.get_var("PATH").unwrap_or_default();
    assert!(
        path.split(':').any(|p| p == bin.to_str().unwrap()),
        "broot's usr/bin must be on PATH: {path}"
    );
}

// Without a `${CHOST}-gcc` reachable at all (no `build_broot`, and a bogus
// tuple that can't be on the real test-runner's `$PATH`), the cross-
// toolchain block must leave `CC` untouched rather than setting a bare,
// unreachable `${CHOST}-gcc`.
#[tokio::test]
async fn cross_toolchain_selection_no_op_when_tool_unreachable() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    shell.set_var("CHOST", "bogus-tuple-that-does-not-exist");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.init_build_env().await.unwrap();

    assert!(shell.get_var("CC").unwrap_or_default().is_empty());
}

// Do not set `PKG_CONFIG` to a missing `${chost}-pkg-config` just because
// `${chost}-gcc` exists — leave it unset so `tc-getPKG_CONFIG` can fall back.
#[tokio::test]
async fn cross_toolchain_selection_skips_pkg_config_when_wrapper_missing() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    let broot = dir.path().join("broot");
    let bin = broot.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    // Only gcc exists in this toolchain bin dir — no pkg-config wrapper,
    // matching em's real crossdev bootstrap (which never creates one).
    std::fs::write(bin.join("riscv64-unknown-linux-gnu-gcc"), "#!/bin/sh\n:\n").unwrap();

    let broot_utf8 = Utf8PathBuf::from_path_buf(broot).unwrap();
    shell.set_build_roots(None, None, None, Some(&broot_utf8), None);

    shell.set_var("CHOST", "riscv64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.init_build_env().await.unwrap();

    assert!(shell.get_var("CC").unwrap_or_default().ends_with("-gcc"));
    assert!(
        shell.get_var("PKG_CONFIG").unwrap_or_default().is_empty(),
        "PKG_CONFIG must stay unset when the wrapper doesn't exist, not point at a dead path"
    );
}

// Phase 2 of the `--prefix`/`--local` toolchain-awareness work: a
// **native** build (CHOST == CBUILD) under `--prefix`/`--local` used to
// never enter the toolchain-selection block at
// all (it was gated on `chost != cbuild`), so it silently fell through to
// the host's `gcc` on `$PATH` even after the prefix had built and activated
// its own compiler. `build_eprefix.is_some()` now also opens the gate;
// `build_broot` (already topology-correct — see the gate's own doc comment)
// is still the bin-dir source, unchanged from the cross case.
#[tokio::test]
async fn native_toolchain_selection_prefers_prefix_gcc_when_eprefix_set() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    let prefix = dir.path().join("prefix");
    let bin = prefix.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gcc = bin.join("aarch64-unknown-linux-gnu-gcc");
    std::fs::write(&gcc, "#!/bin/sh\n:\n").unwrap();

    let prefix_utf8 = Utf8PathBuf::from_path_buf(prefix).unwrap();
    // `--prefix`/`--local`: broot and eprefix both resolve to the prefix
    // itself (see the gate's own doc comment on `Cli::host_roots()`).
    shell.set_build_roots(None, None, Some(&prefix_utf8), Some(&prefix_utf8), None);

    shell.set_var("CHOST", "aarch64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.init_build_env().await.unwrap();

    let expected_cc = gcc.to_str().unwrap().to_string();
    assert_eq!(shell.get_var("CC").as_deref(), Some(expected_cc.as_str()));
    let path = shell.get_var("PATH").unwrap_or_default();
    assert!(
        path.split(':').any(|p| p == bin.to_str().unwrap()),
        "prefix's usr/bin must be on PATH: {path}"
    );
}

// Target packages use `${CTARGET}-*` tools (not ambient CHOST) and export
// `BUILD_CC` for host-side sub-probes.
#[tokio::test]
async fn cross_target_package_toolchain_uses_ctarget_not_ambient_chost() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    let prefix = dir.path().join("prefix");
    let bin = prefix.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let host_gcc = bin.join("aarch64-unknown-linux-gnu-gcc");
    std::fs::write(&host_gcc, "#!/bin/sh\n:\n").unwrap();
    let target_gcc = bin.join("riscv64-unknown-linux-gnu-gcc");
    std::fs::write(&target_gcc, "#!/bin/sh\n:\n").unwrap();

    let prefix_utf8 = Utf8PathBuf::from_path_buf(prefix).unwrap();
    shell.set_build_roots(None, None, Some(&prefix_utf8), Some(&prefix_utf8), None);

    // Ambient CHOST/CBUILD both aarch64 (matching use_outer_eroot's
    // host-config resolution for this step) — package.env's own CTARGET
    // names the real target, with no TARGET_ABI (unlike binutils/gcc).
    shell.set_var("CHOST", "aarch64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.set_var("CTARGET", "riscv64-unknown-linux-gnu");
    shell.init_build_env().await.unwrap();

    assert_eq!(
        shell.get_var("CC").as_deref(),
        Some(target_gcc.to_str().unwrap()),
        "a genuine cross-target package must get CTARGET's own compiler, not the ambient CHOST's"
    );
    assert_eq!(
        shell.get_var("BUILD_CC").as_deref(),
        Some(host_gcc.to_str().unwrap()),
        "BUILD_CC must still resolve to the ambient/CBUILD-side compiler for host-side sub-probes"
    );
}

// The host-arch toolchain-*tool* package class (`binutils`/`gcc`/`gdb` —
// `package.env` marks these with `TARGET_ABI`, unlike genuine target
// packages) must keep using `${CHOST}-<tool>`, exactly as before this fix —
// their own compile identity genuinely is the host's, `CTARGET` there only
// describes what the *resulting* cross compiler will target, not this
// package's own build.
#[tokio::test]
async fn cross_host_tool_package_still_uses_chost_when_target_abi_set() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    let prefix = dir.path().join("prefix");
    let bin = prefix.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let host_gcc = bin.join("aarch64-unknown-linux-gnu-gcc");
    std::fs::write(&host_gcc, "#!/bin/sh\n:\n").unwrap();
    let target_gcc = bin.join("riscv64-unknown-linux-gnu-gcc");
    std::fs::write(&target_gcc, "#!/bin/sh\n:\n").unwrap();

    let prefix_utf8 = Utf8PathBuf::from_path_buf(prefix).unwrap();
    shell.set_build_roots(None, None, Some(&prefix_utf8), Some(&prefix_utf8), None);

    shell.set_var("CHOST", "aarch64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.set_var("CTARGET", "riscv64-unknown-linux-gnu");
    shell.set_var("TARGET_ABI", "lp64d");
    shell.init_build_env().await.unwrap();

    assert_eq!(
        shell.get_var("CC").as_deref(),
        Some(host_gcc.to_str().unwrap()),
        "binutils/gcc/gdb (TARGET_ABI set) must keep the host CHOST compiler"
    );
    assert!(
        shell.get_var("BUILD_CC").unwrap_or_default().is_empty(),
        "BUILD_CC is only for the genuine-target-package case"
    );
}

// Host-env package.env marker (`TARGET_ABI` set, bash-crossdev `*`) keeps
// the host CHOST compiler even when CTARGET is present for the target ABI.
#[tokio::test]
async fn package_env_host_marker_uses_chost_tools() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    let prefix = dir.path().join("prefix");
    let bin = prefix.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let host_gcc = bin.join("aarch64-unknown-linux-gnu-gcc");
    std::fs::write(&host_gcc, "#!/bin/sh\n:\n").unwrap();
    let target_gcc = bin.join("riscv64-unknown-linux-gnu-gcc");
    std::fs::write(&target_gcc, "#!/bin/sh\n:\n").unwrap();

    let prefix_utf8 = Utf8PathBuf::from_path_buf(prefix).unwrap();
    shell.set_build_roots(None, None, Some(&prefix_utf8), Some(&prefix_utf8), None);

    shell.set_var("CHOST", "aarch64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.set_var("CTARGET", "riscv64-unknown-linux-gnu");
    // Host-env package (binutils/gcc): TARGET_ABI set → host tools.
    shell.set_var("TARGET_ABI", "lp64d");
    shell.init_build_env().await.unwrap();

    assert_eq!(
        shell.get_var("CC").as_deref(),
        Some(host_gcc.to_str().unwrap()),
        "TARGET_ABI set → host CHOST compiler"
    );
    assert!(
        shell.get_var("BUILD_CC").unwrap_or_default().is_empty(),
        "host-env package does not set BUILD_*"
    );
}

// Target-env package.env (K|L): CTARGET set, no TARGET_ABI → CTARGET tools
// and host BUILD_*.
#[tokio::test]
async fn package_env_target_marker_uses_ctarget_tools() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    let prefix = dir.path().join("prefix");
    let bin = prefix.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let host_gcc = bin.join("aarch64-unknown-linux-gnu-gcc");
    std::fs::write(&host_gcc, "#!/bin/sh\n:\n").unwrap();
    let target_gcc = bin.join("riscv64-unknown-linux-gnu-gcc");
    std::fs::write(&target_gcc, "#!/bin/sh\n:\n").unwrap();

    let prefix_utf8 = Utf8PathBuf::from_path_buf(prefix).unwrap();
    shell.set_build_roots(None, None, Some(&prefix_utf8), Some(&prefix_utf8), None);

    shell.set_var("CHOST", "aarch64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.set_var("CTARGET", "riscv64-unknown-linux-gnu");
    // No TARGET_ABI — K|L target package.
    shell.init_build_env().await.unwrap();

    assert_eq!(
        shell.get_var("CC").as_deref(),
        Some(target_gcc.to_str().unwrap()),
        "CTARGET without TARGET_ABI → target tools"
    );
    assert_eq!(
        shell.get_var("BUILD_CC").as_deref(),
        Some(host_gcc.to_str().unwrap()),
        "target package still gets host BUILD_CC"
    );
}

// A plain `--root`/bare build (no `--prefix`/`--local`, so `build_eprefix`
// stays `None`) must NOT be affected by the Phase 2 gate change — real
// `--root` defaulting to the host's `gcc` on `PATH` is correct as-is
// (catalyst seed-compiler model, confirmed not a bug).
#[tokio::test]
async fn native_toolchain_selection_is_a_no_op_without_eprefix() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    shell.set_var("CHOST", "aarch64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.init_build_env().await.unwrap();

    assert!(shell.get_var("CC").unwrap_or_default().is_empty());
}

// Same `PKG_CONFIG`-must-not-point-at-a-dead-wrapper guard as
// `cross_toolchain_selection_skips_pkg_config_when_wrapper_missing`, for the
// native-prefix path opened by Phase 2.
#[tokio::test]
async fn native_toolchain_selection_skips_pkg_config_when_wrapper_missing() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    let prefix = dir.path().join("prefix");
    let bin = prefix.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("aarch64-unknown-linux-gnu-gcc"), "#!/bin/sh\n:\n").unwrap();

    let prefix_utf8 = Utf8PathBuf::from_path_buf(prefix).unwrap();
    shell.set_build_roots(None, None, Some(&prefix_utf8), Some(&prefix_utf8), None);

    shell.set_var("CHOST", "aarch64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.init_build_env().await.unwrap();

    assert!(shell.get_var("CC").unwrap_or_default().ends_with("-gcc"));
    assert!(
        shell.get_var("PKG_CONFIG").unwrap_or_default().is_empty(),
        "PKG_CONFIG must stay unset when the wrapper doesn't exist, not point at a dead path"
    );
}

// Build a minimal ebuild fixture at `<category>/<pn>` and return its path,
// for tests that need `run_phase` (category/pn only exist per-ebuild,
// unlike the `init_build_env`-only tests above).
fn write_minimal_ebuild(repo_root: &std::path::Path, category: &str, pn: &str) -> Utf8PathBuf {
    std::fs::create_dir_all(repo_root.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_root.join("profiles")).unwrap();
    std::fs::write(repo_root.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_root.join("profiles/repo_name"), "t\n").unwrap();
    let ebdir = repo_root.join(category).join(pn);
    std::fs::create_dir_all(&ebdir).unwrap();
    let path = ebdir.join(format!("{pn}-1.ebuild"));
    std::fs::write(
        &path,
        "EAPI=8\nDESCRIPTION=\"t\"\nSLOT=\"0\"\nLICENSE=\"MIT\"\nS=\"${WORKDIR}\"\npkg_setup() { :; }\n",
    )
    .unwrap();
    Utf8PathBuf::from_path_buf(path).unwrap()
}

// Build a minimal ebuild that `inherit`s an eclass which plain-assigns
// `IUSE="foo"` (matching real `verify-sig.eclass`'s own `IUSE="verify-sig"`
// — a plain assignment, not `+=`), for the `already_phase_sourced` tests
// below.
fn write_ebuild_with_plain_iuse_eclass(repo_root: &std::path::Path) -> Utf8PathBuf {
    std::fs::create_dir_all(repo_root.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_root.join("profiles")).unwrap();
    std::fs::create_dir_all(repo_root.join("eclass")).unwrap();
    std::fs::write(repo_root.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_root.join("profiles/repo_name"), "t\n").unwrap();
    std::fs::write(repo_root.join("eclass/plainiuse.eclass"), "IUSE=\"foo\"\n").unwrap();
    let ebdir = repo_root.join("cat/pkg");
    std::fs::create_dir_all(&ebdir).unwrap();
    let path = ebdir.join("pkg-1.ebuild");
    std::fs::write(
        &path,
        "EAPI=8\ninherit plainiuse\nDESCRIPTION=\"t\"\nSLOT=\"0\"\nLICENSE=\"MIT\"\n\
         S=\"${WORKDIR}\"\npkg_setup() { :; }\n",
    )
    .unwrap();
    Utf8PathBuf::from_path_buf(path).unwrap()
}

// Verifies the invariant `portage-cli`'s `run_merge` fix (2026-08-04) relies
// on: once an ebuild has been sourced by an earlier phase in this same shell
// (`run_phase`'s own `need_source`-gated sourcing — `unpack`/`prepare`/etc.
// in a real merge), the resulting `IUSE` already correctly folds in an
// eclass's own plain-assignment contribution (matching real
// `verify-sig.eclass`'s `IUSE="verify-sig"`) via the PMS 10.2 `E_IUSE`
// combine — so a caller can read it via `collect_env()` directly, guarded by
// `is_phase_sourced`, instead of calling `source_ebuild` again.
//
// `run_merge` calling `source_ebuild` unconditionally (the pre-fix bug) was
// `verify-sig`, even though the pre-merge dependency plan showed it
// correctly and the md5-cache (via `em regen`) also has it. The exact
// bash/eclass-level reason the *specific* live sequence of phases dropped it
// wasn't isolated in a minimal repro here (a synthetic multi-pass
// `source_ebuild`/`run_phase` sequence modeled on the real merge's phase
// order did not reproduce the loss) — this test instead locks down the
// precondition the fix depends on, and the fix itself was verified directly
// against the real `::gentoo` binutils ebuild (VDB `IUSE` now matches the
// cache).
#[tokio::test]
async fn already_phase_sourced_iuse_includes_eclass_contribution() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let ebuild_path = write_ebuild_with_plain_iuse_eclass(&repo_path);

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    let ebuild = Ebuild::from_path(&ebuild_path).unwrap();
    let work = dir.path().join("work");

    shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    assert!(shell.is_phase_sourced(&ebuild));
    assert!(
        shell.collect_env().iuse.iter().any(|f| f == "foo"),
        "already-phase-sourced IUSE must include the eclass's own contribution: {:?}",
        shell.collect_env().iuse
    );
}

// Regression (live 2026-08-28, a resumed board `--root` stage1 reinstalling
// `app-alternatives/awk`): `run_merge` runs a slot occupant's pkg_prerm/
// pkg_postrm (a different `Ebuild` path) on this same shell, between the new
// package's preinst and postinst. A shared eclass with its own bash include
// guard (as real `app-alternatives.eclass` has) looks already-sourced on the
// way back: both `inherit`'s dedup list *and* the guard variable survive in
// the live shell, so clearing only the former (an earlier, reverted fix
// attempt) can't help. `save_session`/`restore_session` instead returns to
// the exact pre-unmerge shell instead of re-sourcing over it.
#[tokio::test]
async fn session_save_restore_survives_a_slot_replaces_foreign_ebuild() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::create_dir_all(repo_path.join("eclass")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();
    std::fs::write(
        repo_path.join("eclass/guardiuse.eclass"),
        "if [[ -z ${_GUARDIUSE_ECLASS} ]]; then\n_GUARDIUSE_ECLASS=1\nIUSE=\"foo\"\nfi\n",
    )
    .unwrap();
    let ebdir = repo_path.join("cat/pkg");
    std::fs::create_dir_all(&ebdir).unwrap();
    let new_path = Utf8PathBuf::from_path_buf(ebdir.join("pkg-2.ebuild")).unwrap();
    std::fs::write(
        &new_path,
        "EAPI=8\ninherit guardiuse\nIUSE=\"new_only\"\nDESCRIPTION=\"t\"\n\
         SLOT=\"0\"\nLICENSE=\"MIT\"\nS=\"${WORKDIR}\"\n\
         pkg_preinst() { :; }\npkg_postinst() { :; }\n",
    )
    .unwrap();
    let old_path = Utf8PathBuf::from_path_buf(ebdir.join("pkg-1.ebuild")).unwrap();
    std::fs::write(
        &old_path,
        "EAPI=8\ninherit guardiuse\nIUSE=\"old_only\"\nDESCRIPTION=\"t\"\n\
         SLOT=\"0\"\nLICENSE=\"MIT\"\nS=\"${WORKDIR}\"\n\
         pkg_prerm() { :; }\npkg_postrm() { :; }\n",
    )
    .unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    let work = dir.path().join("work");
    let old_work = dir.path().join("old_work");

    let new_ebuild = Ebuild::from_path(&new_path).unwrap();
    shell
        .run_phase(&new_ebuild, "preinst", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    let session = shell.save_session();

    let old_ebuild = Ebuild::from_path(&old_path).unwrap();
    shell
        .run_phase(&old_ebuild, "prerm", &old_work, std::path::Path::new("/"))
        .await
        .unwrap();
    shell
        .run_phase(&old_ebuild, "postrm", &old_work, std::path::Path::new("/"))
        .await
        .unwrap();

    shell.restore_session(session);

    shell
        .run_phase(&new_ebuild, "postinst", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    let effective = shell.get_var("IUSE_EFFECTIVE").unwrap_or_default();
    assert!(
        effective.split_whitespace().any(|f| f == "foo"),
        "postinst's IUSE_EFFECTIVE must still include the shared eclass's \
         guarded contribution after the foreign old-package sourcing: {effective:?}"
    );
}

// Host cross tools under `--prefix` get `-idirafter /usr/include` so
// host-only BDEPEND headers resolve.
#[tokio::test]
async fn cross_host_tool_package_gets_a_host_include_fallback_under_prefix() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let ebuild_path =
        write_minimal_ebuild(&repo_path, "cross-riscv64-unknown-linux-gnu", "binutils");

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let sysroot = Utf8PathBuf::from("/");
    shell.set_build_roots(None, Some(&sysroot), None, None, None);

    let ebuild = Ebuild::from_path(&ebuild_path).unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    let cppflags = shell.get_var("CPPFLAGS").unwrap_or_default();
    assert!(
        cppflags.contains("-idirafter /usr/include"),
        "cross-<tuple>/binutils under a --prefix overlay must get the host include fallback: {cppflags}"
    );
}

// Same package class, but no `build_sysroot` (`--local`'s standalone
// closure, not a `--prefix` overlay — `build_sysroot` stays `None`): must
// NOT get the host include fallback. `--local` is meant to own everything
// itself, not reach for the host's headers.
#[tokio::test]
async fn cross_host_tool_package_no_host_fallback_without_overlay() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let ebuild_path =
        write_minimal_ebuild(&repo_path, "cross-riscv64-unknown-linux-gnu", "binutils");

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let ebuild = Ebuild::from_path(&ebuild_path).unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    let cppflags = shell.get_var("CPPFLAGS").unwrap_or_default();
    assert!(
        !cppflags.contains("/usr/include"),
        "no --prefix overlay in play: CPPFLAGS must stay untouched: {cppflags}"
    );
}

// An ordinary (non-`cross-*`) package must never get the host include
// fallback, even under a `--prefix` overlay — it is specific to the
// host-arch crossdev toolchain-tool package class.
#[tokio::test]
async fn ordinary_package_no_host_fallback_even_under_prefix() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let ebuild_path = write_minimal_ebuild(&repo_path, "sys-libs", "zlib");

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let sysroot = Utf8PathBuf::from("/");
    shell.set_build_roots(None, Some(&sysroot), None, None, None);

    let ebuild = Ebuild::from_path(&ebuild_path).unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    let cppflags = shell.get_var("CPPFLAGS").unwrap_or_default();
    assert!(
        !cppflags.contains("/usr/include"),
        "an ordinary package must not get the cross-host-tool fallback: {cppflags}"
    );
}

// Ordinary target packages: ESYSROOT is the substituted sysroot alone,
// never sysroot+outer-eprefix.
#[tokio::test]
async fn esysroot_is_not_doubled_for_an_ordinary_target_package_under_prefix() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let ebuild_path = write_minimal_ebuild(&repo_path, "sys-libs", "zlib");

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let sysroot_dir = dir.path().join("prefix/usr/riscv64-unknown-linux-gnu");
    std::fs::create_dir_all(&sysroot_dir).unwrap();
    let sysroot_path = Utf8PathBuf::from_path_buf(sysroot_dir.clone()).unwrap();
    let outer_prefix = Utf8PathBuf::from_path_buf(dir.path().join("prefix")).unwrap();

    // `build_sysroot: None` reproduces `Roots::build_sysroot()` returning
    // `None` (base == target == the already-substituted sysroot); `eprefix`
    // still carries the outer `--prefix` path, exactly as `Cli::roots()`'s
    // `--target` branch leaves it.
    shell.set_build_roots(None, None, Some(&outer_prefix), None, None);

    let ebuild = Ebuild::from_path(&ebuild_path).unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, sysroot_dir.as_path())
        .await
        .unwrap();

    let esysroot = shell.get_var("ESYSROOT").unwrap_or_default();
    assert_eq!(
        esysroot.trim_end_matches('/'),
        sysroot_path.as_str().trim_end_matches('/'),
        "ESYSROOT must equal the already-substituted sysroot alone, not sysroot+outer-eprefix doubled: {esysroot}"
    );
}

// A board-destined package under `--target` correctly gets an empty
// EPREFIX (see the ESYSROOT test above), so autoconf's own
// `${--prefix}/share/config.site` discovery can never reach crossdev's
// cache-answer library — which lives under `build_broot`
// (`Cli::host_roots()`'s merge root), not under this package's own prefix.
#[tokio::test]
async fn config_site_points_at_build_broot_for_a_board_destined_package() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let ebuild_path = write_minimal_ebuild(&repo_path, "sys-apps", "diffutils");

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let broot_dir = dir.path().join("prefix");
    let board_dir = dir.path().join("board");
    std::fs::create_dir_all(&board_dir).unwrap();
    let broot = Utf8PathBuf::from_path_buf(broot_dir.clone()).unwrap();

    // eprefix: None — the board's own packages are correctly not prefixed.
    shell.set_build_roots(None, None, None, Some(&broot), None);

    let ebuild = Ebuild::from_path(&ebuild_path).unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, board_dir.as_path())
        .await
        .unwrap();

    assert_eq!(
        shell.get_var("CONFIG_SITE").as_deref(),
        Some(broot.join("usr/share/config.site").as_str()),
        "CONFIG_SITE must point at build_broot, not this package's own (empty) EPREFIX"
    );

    // Exported (not just set): `set_var` alone doesn't reach a real
    // subprocess like `configure` — only names in run_phase's explicit
    // `export` list do (caught this exact gap: the value above was already
    // correct while the subprocess still never saw it).
    shell
        .run_string("_test_exported=$(export -p | grep -cE '^declare -x CONFIG_SITE=')")
        .await
        .unwrap();
    assert_eq!(shell.get_var("_test_exported").as_deref(), Some("1"));
}

// No `--target`/toolchain context at all (bare `em ebuild` debug path,
// `build_broot: None`): CONFIG_SITE must stay unset so autoconf's own
// default `${--prefix}/share/config.site` search is unaffected.
#[tokio::test]
async fn config_site_unset_without_a_build_broot() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let ebuild_path = write_minimal_ebuild(&repo_path, "sys-apps", "diffutils");

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    shell.set_build_roots(None, None, None, None, None);

    let ebuild = Ebuild::from_path(&ebuild_path).unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, dir.path())
        .await
        .unwrap();

    assert_eq!(shell.get_var("CONFIG_SITE"), None);
}

// `set_build_roots`'s `ld_library_path` is exported as-is, no filesystem
// read here (the caller resolves it — see todo/for-sonnet.md 2026-08-08).
#[tokio::test]
async fn prefix_build_exports_the_given_ld_library_path() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let ebuild_path = write_minimal_ebuild(&repo_path, "sys-libs", "zlib");
    let prefix_dir = dir.path().join("prefix");
    let eprefix = Utf8PathBuf::from_path_buf(prefix_dir.clone()).unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    shell.set_build_roots(None, None, Some(&eprefix), None, Some("/prefix/usr/lib64"));

    let ebuild = Ebuild::from_path(&ebuild_path).unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, prefix_dir.as_path())
        .await
        .unwrap();

    assert_eq!(
        shell.get_var("LD_LIBRARY_PATH").unwrap_or_default(),
        "/prefix/usr/lib64"
    );
}

// No `ld_library_path` given (a bare host/`--root` build) must not touch
// `LD_LIBRARY_PATH` at all.
#[tokio::test]
async fn no_ld_library_path_given_leaves_it_unset() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let ebuild_path = write_minimal_ebuild(&repo_path, "sys-libs", "zlib");

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let ebuild = Ebuild::from_path(&ebuild_path).unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    assert!(shell.get_var("LD_LIBRARY_PATH").is_none());
}

// `set_terminal` must export COLUMNS/NOCOLOR/NO_COLOR for external
// subprocesses (gentoo-functions, eclasses), not only brush-internal state.
#[tokio::test]
async fn set_terminal_is_exported_to_phase_subprocesses() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let ebuild_path = write_minimal_ebuild(&repo_path, "sys-devel", "binutils");

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    shell.set_terminal(crate::TerminalConfig {
        columns: 123,
        colors: crate::PortageColors::default(),
    });

    let ebuild = Ebuild::from_path(&ebuild_path).unwrap();
    let work = dir.path().join("work");
    shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    assert_eq!(shell.get_var("COLUMNS").as_deref(), Some("123"));

    // Exported (not just set): a real subprocess must see it as a real OS
    // environment variable, not just brush's own internal variable table.
    // Captured via command substitution into a var (run_string's own exit
    // code doesn't reflect the script's internal commands' exit statuses,
    // so a plain `assert!(result.is_ok())` on the grep itself would prove
    // nothing).
    shell
        .run_string(
            "_test_exported=$(export -p | grep -cE '^declare -x (COLUMNS|NOCOLOR|NO_COLOR)=')",
        )
        .await
        .unwrap();
    assert_eq!(
        shell.get_var("_test_exported").as_deref(),
        Some("3"),
        "COLUMNS/NOCOLOR/NO_COLOR must be in the exported-variable list, not just brush's \
         internal table"
    );

    // A plain palette is portage's `__unset_colors`, which both halves of the
    // convention must agree on: `NOCOLOR=true` for the eclasses that read
    // portage's spelling, a *non-empty* `NO_COLOR` for everyone else.
    assert_eq!(shell.get_var("NOCOLOR").as_deref(), Some("true"));
    assert_eq!(shell.get_var("NO_COLOR").as_deref(), Some("1"));
}

// The host's `TERM` reaches the phase, as it does through portage's
// environ_whitelist.
//
// Leaving it unset is not the neutral choice it looks like: bash substitutes
// `dumb` for an unset `TERM`, and `dumb` is the first thing every capability
// probe tests for — real `gentoo-functions` throws away its entire palette on
// it (`rc.sh`'s `_has_color_terminal`), which is why `elibtoolize` (really the
// external `eltpatch` script) printed flat markers even in a capable terminal.
#[tokio::test]
async fn host_term_reaches_the_phase() {
    // SAFETY: single-threaded test, and the value is read during shell setup
    // below rather than by another thread.
    unsafe { std::env::set_var("TERM", "xterm-256color") };

    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let ebuild_path = write_minimal_ebuild(&repo_path, "sys-devel", "binutils");
    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let ebuild = Ebuild::from_path(&ebuild_path).unwrap();
    shell
        .run_phase(
            &ebuild,
            "setup",
            &dir.path().join("work"),
            std::path::Path::new("/"),
        )
        .await
        .unwrap();

    assert_eq!(shell.get_var("TERM").as_deref(), Some("xterm-256color"));
    shell
        .run_string("_test_exported=$(export -p | grep -c '^declare -x TERM=')")
        .await
        .unwrap();
    assert_eq!(
        shell.get_var("_test_exported").as_deref(),
        Some("1"),
        "TERM must be exported: only a real external subprocess like `eltpatch` reads it"
    );
}

// Run `script` and return what it wrote to stderr, with trailing newlines
// stripped by the command substitution as usual.
async fn captured_stderr(shell: &mut EbuildShell, script: &str) -> String {
    shell
        .run_string(&format!("_captured=$({{ {script} ; }} 2>&1)"))
        .await
        .unwrap();
    shell.get_var("_captured").unwrap_or_default()
}

// The `e*` builtins render exactly as portage's `isolated-functions.sh` does,
// in both of the two modes it has.
//
// Portage's bash half performs no terminal detection of its own: `RC_ENDCOL`
// is hardcoded to `"yes"`, and the only switch is `__set_colors` versus
// `__unset_colors`. With colours off `ENDCOL` is empty, so `eend`'s
// `echo -e "${ENDCOL} ${msg}"` degrades to the indicator on a line of its own;
// with colours on the same line becomes cursor-up plus cursor-forward, landing
// the indicator at the end of the line `ebegin` wrote.
#[tokio::test]
async fn e_output_builtins_render_like_isolated_functions() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    write_minimal_ebuild(&repo_path, "sys-devel", "binutils");
    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    // A fresh shell starts in metadata mode (no-op output builtins);
    // switch to build mode so they render for real (commands::dual_mode).
    super::commands::set_tool_mode(&mut shell.shell, super::commands::ToolMode::Build);

    // ── __unset_colors ────────────────────────────────────────────────────
    shell.set_terminal(crate::TerminalConfig {
        columns: 80,
        colors: crate::PortageColors::default(),
    });

    assert_eq!(captured_stderr(&mut shell, "einfo hello").await, " * hello");
    // Every line of a multi-line message carries the marker, as portage's
    // `while read -r` loop gives it.
    assert_eq!(
        captured_stderr(&mut shell, "einfo $'one\\ntwo'").await,
        " * one\n * two"
    );
    // einfon is the one that does not end the line — that is what the `n` is
    // for, and `ebegin` is built on it.
    assert_eq!(
        captured_stderr(&mut shell, "einfon bare; printf END").await,
        " * bareEND"
    );
    assert_eq!(
        captured_stderr(&mut shell, "ebegin Doing; eend 0").await,
        " * Doing ...\n [ ok ]"
    );
    // A failing eend reports through eerror first, so the indicator lands at
    // the end of *that* line rather than the ebegin one.
    assert_eq!(
        captured_stderr(&mut shell, "ebegin Doing; eend 1 nope").await,
        " * Doing ...\n * nope\n [ !! ]"
    );

    // eend's argument is the exit status it reports *and* returns.
    shell
        .run_string("eend 3 2>/dev/null; _rc=$?")
        .await
        .unwrap();
    assert_eq!(shell.get_var("_rc").as_deref(), Some("3"));

    // ── __set_colors ──────────────────────────────────────────────────────
    let ansi = |c| anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(c)));
    shell.set_terminal(crate::TerminalConfig {
        columns: 80,
        colors: crate::PortageColors {
            info: ansi(anstyle::AnsiColor::Green),
            bracket: ansi(anstyle::AnsiColor::Blue),
            good: ansi(anstyle::AnsiColor::Cyan),
            ..Default::default()
        },
    });

    // Only the `*` is painted; the spaces framing it are not
    // (`" ${PORTAGE_COLOR_INFO}*${PORTAGE_COLOR_NORMAL} ${REPLY}"`).
    assert_eq!(
        captured_stderr(&mut shell, "einfo hello").await,
        " \x1b[32m*\x1b[0m hello"
    );
    // ENDCOL (up one line, then `COLS - 8` right), then the indicator: colour
    // switches between segments with a single reset at the end, as portage's
    // `"${BRACKET}[ ${GOOD}ok${BRACKET} ]${NORMAL}"` does. Closing each segment
    // instead would render identically while emitting three extra escapes.
    assert_eq!(
        captured_stderr(&mut shell, "ebegin Doing; eend 0").await,
        " \x1b[32m*\x1b[0m Doing ...\n\x1b[A\x1b[72C \x1b[34m[ \x1b[36mok\x1b[34m ]\x1b[0m"
    );
}

// Every message the `e*` builtins print is also appended to
// `${T}/logging/${EBUILD_PHASE}`, portage's `__elog_base` — the file the elog
// system replays once the package is merged.
#[tokio::test]
async fn e_output_builtins_capture_elog_messages() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    write_minimal_ebuild(&repo_path, "sys-devel", "binutils");
    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    super::commands::set_tool_mode(&mut shell.shell, super::commands::ToolMode::Build);

    let t = dir.path().join("temp");
    let logging = t.join("logging");
    shell
        .run_string(&format!(
            "export T={} EBUILD_PHASE=postinst",
            t.to_str().unwrap()
        ))
        .await
        .unwrap();

    // Without the directory there is nowhere to capture to, and nothing is
    // created on demand: its existence is the switch.
    captured_stderr(&mut shell, "einfo before").await;
    assert!(!logging.exists());

    std::fs::create_dir_all(&logging).unwrap();
    captured_stderr(
        &mut shell,
        "einfo hi; elog logged; ewarn warned; eerror bad; eqawarn qa; einfon nolf",
    )
    .await;
    // Multi-line messages are recorded one entry per line, as the `while read`
    // loop feeding `>> ${T}/logging/...` gives; an empty message is skipped.
    captured_stderr(&mut shell, "einfo $'one\\ntwo'; einfo ''").await;
    // `ebegin` records an INFO entry — it renders through `einfon`, dots and
    // all (`isolated-functions.sh`'s `einfon "${msg}"`), so what is filed is
    // what the user saw. A failing `eend` reports through `eerror`, so its
    // diagnostic lands as an ERROR; `eend` itself records nothing else.
    captured_stderr(
        &mut shell,
        "ebegin Doing; eend 1 failed; ebegin Two; eend 0",
    )
    .await;

    assert_eq!(
        std::fs::read_to_string(logging.join("postinst")).unwrap(),
        "INFO hi\nLOG logged\nWARN warned\nERROR bad\nQA qa\nINFO nolf\n\
         INFO one\nINFO two\nINFO Doing ...\nERROR failed\nINFO Two ...\n"
    );

    // Messages raised outside a phase go to `other`, portage's
    // `${EBUILD_PHASE:-other}`.
    shell.run_string("export EBUILD_PHASE=").await.unwrap();
    captured_stderr(&mut shell, "elog stray").await;
    assert_eq!(
        std::fs::read_to_string(logging.join("other")).unwrap(),
        "LOG stray\n"
    );
}

// Messages go through `echo -e`, so a `\n` in one is a line break in both the
// printed output and the recorded entry — portage's `e*` helpers all render
// (and record) via `echo -e "$@"`.
#[tokio::test]
async fn e_output_builtins_expand_escapes_like_echo_e() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    write_minimal_ebuild(&repo_path, "sys-devel", "binutils");
    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    super::commands::set_tool_mode(&mut shell.shell, super::commands::ToolMode::Build);
    shell.set_terminal(crate::TerminalConfig {
        columns: 80,
        colors: crate::PortageColors::default(),
    });

    let t = dir.path().join("temp");
    let logging = t.join("logging");
    std::fs::create_dir_all(&logging).unwrap();
    shell
        .run_string(&format!(
            "export T={} EBUILD_PHASE=postinst",
            t.to_str().unwrap()
        ))
        .await
        .unwrap();

    // A literal backslash-n in the argument, not a real newline.
    assert_eq!(
        captured_stderr(&mut shell, r#"einfo 'one\ntwo'"#).await,
        " * one\n * two"
    );
    // An escape `echo -e` does not know keeps its backslash.
    assert_eq!(
        captured_stderr(&mut shell, r#"einfo 'a\qb'"#).await,
        r" * a\qb"
    );
    // `\c` ends the message there.
    assert_eq!(
        captured_stderr(&mut shell, r#"einfo 'keep\cdrop'"#).await,
        " * keep"
    );

    // What was recorded is what was shown.
    assert_eq!(
        std::fs::read_to_string(logging.join("postinst")).unwrap(),
        "INFO one\nINFO two\nINFO a\\qb\nINFO keep\n"
    );
}

// A palette with no colour in it is portage's `__unset_colors`, which is what
// lets `eend` decide between its two renderings without a second flag to keep
// in sync. anstyle renders an empty style as the empty string at both ends,
// so this holds by construction — pin it, since `eend` depends on it.
#[test]
fn a_default_palette_is_plain() {
    assert!(crate::PortageColors::default().is_plain());
    let colors = crate::PortageColors {
        qawarn: anstyle::Style::new()
            .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow))),
        ..Default::default()
    };
    assert!(!colors.is_plain());
}

// What a build phase gets to see of the caller's `PATH`. `$HOME` and
// `/usr/local` come off it so a local install cannot shadow the Gentoo
// toolchain; everything else keeps its order.
#[test]
fn phase_path_drops_only_home_and_usr_local() {
    assert_eq!(
        crate::phase_path_dirs(
            "/home/u/.local/bin:/usr/bin:/usr/local/bin:/opt/x/bin:/home/u:/bin",
            "/home/u",
        ),
        vec!["/usr/bin", "/opt/x/bin", "/bin"]
    );
    // An unset HOME must not turn into a prefix that matches everything.
    assert_eq!(
        crate::phase_path_dirs("/usr/bin:/bin", ""),
        vec!["/usr/bin", "/bin"]
    );
    // `/usr/localstuff` is not under `/usr/local`.
    assert_eq!(
        crate::phase_path_dirs("/usr/localstuff/bin", ""),
        vec!["/usr/localstuff/bin"]
    );
}

// `set_extra_path` is the one way back in for a caller that resolved a tool
// the sanitising above would otherwise hide.
#[tokio::test]
async fn extra_path_dirs_lead_the_phase_path() {
    let dir = tempdir().unwrap();
    let repo_path = dir.path().to_path_buf();
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(
        repo_path.join("metadata").join("layout.conf"),
        "masters = \ncache-formats = md5-dict\n",
    )
    .unwrap();
    std::fs::write(repo_path.join("profiles").join("repo_name"), "test-repo\n").unwrap();
    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();

    let mut shell = repo.shell().await.unwrap();
    shell.init_build_env().await.unwrap();
    let plain = shell.get_var("PATH").unwrap_or_default();

    shell.set_extra_path(vec![Utf8PathBuf::from("/opt/bootstrap-bin")]);
    shell.init_build_env().await.unwrap();
    assert_eq!(
        shell.get_var("PATH").unwrap_or_default(),
        format!("/opt/bootstrap-bin:{plain}")
    );
}

#[tokio::test]
async fn default_src_prepare_applies_patches_set_during_an_earlier_phase() {
    // Regression: `eapply` used to have a metadata-mode bash stub that
    // never got unshadowed for real builds, so `default`'s PATCHES handling
    // silently applied nothing, with no error (fixed by the dual_mode
    // builtin registry — commands::dual_mode::set_tool_mode).
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();
    let ebdir = repo_path.join("cat/pkg");
    let filesdir = ebdir.join("files");
    std::fs::create_dir_all(&filesdir).unwrap();
    std::fs::write(
        filesdir.join("x.patch"),
        "--- a/f.txt\n+++ a/f.txt\n@@ -1 +1 @@\n-before\n+after\n",
    )
    .unwrap();
    std::fs::write(
        ebdir.join("pkg-1.ebuild"),
        "EAPI=8\nDESCRIPTION=\"t\"\nSLOT=\"0\"\nLICENSE=\"MIT\"\nS=\"${WORKDIR}\"\n\
         PATCHES=( \"${FILESDIR}/x.patch\" )\n\
         src_unpack() { mkdir -p \"${S}\"; echo before > \"${S}/f.txt\"; }\n",
    )
    .unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();

    let ebuild =
        Ebuild::from_path(camino::Utf8Path::from_path(&ebdir.join("pkg-1.ebuild")).unwrap())
            .unwrap();
    let work = dir.path().join("work");

    shell
        .run_phase(&ebuild, "unpack", &work, std::path::Path::new("/"))
        .await
        .unwrap();
    let s = shell.get_var("S").unwrap_or_default();

    // No src_prepare defined by the ebuild: the EAPI-8 default (`default() {
    // default_src_prepare; }` -> `__eapi8_src_prepare` -> `eapply -- "${PATCHES[@]}"`)
    // must run and actually apply the patch.
    shell
        .run_phase(&ebuild, "prepare", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    let content = std::fs::read_to_string(format!("{s}/f.txt")).unwrap();
    assert_eq!(
        content.trim(),
        "after",
        "default_src_prepare did not apply PATCHES"
    );
}

#[tokio::test]
async fn metadata_scan_after_a_real_build_gets_stubs_not_real_builtins() {
    // The reverse-direction case: a real phase run switches the
    // global-scope-reachable builtins (einfo/has_version/…) to build mode
    // (init_build_env); a *reused* shell that then does metadata-only work
    // for a different package must not inherit that — source_ebuild
    // re-asserts metadata mode on every call for exactly this reason.
    // Confirmed live before the fix: `eapply` (dual-mode at the time)
    // stayed real during a metadata-only scan that followed a real build on
    // the same shell. `einfo` is the current representative — its no-op
    // stub prints nothing, unlike the real builtin.
    let dir = tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
    std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
    std::fs::write(repo_path.join("metadata/layout.conf"), "masters =\n").unwrap();
    std::fs::write(repo_path.join("profiles/repo_name"), "t\n").unwrap();
    let ebdir = repo_path.join("cat/pkg");
    std::fs::create_dir_all(&ebdir).unwrap();
    std::fs::write(
        ebdir.join("pkg-1.ebuild"),
        "EAPI=8\nDESCRIPTION=\"t\"\nSLOT=\"0\"\nLICENSE=\"MIT\"\nS=\"${WORKDIR}\"\n\
         pkg_setup() { :; }\n",
    )
    .unwrap();
    let ebdir2 = repo_path.join("cat/pkg2");
    std::fs::create_dir_all(&ebdir2).unwrap();
    std::fs::write(
        ebdir2.join("pkg2-1.ebuild"),
        "EAPI=8\nDESCRIPTION=\"t2\"\nSLOT=\"0\"\nLICENSE=\"MIT\"\n",
    )
    .unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(&repo_path)
        .unwrap();
    let mut shell = repo.shell().await.unwrap();
    let ebuild =
        Ebuild::from_path(camino::Utf8Path::from_path(&ebdir.join("pkg-1.ebuild")).unwrap())
            .unwrap();
    let work = dir.path().join("work");

    // A real phase run switches eapply (and friends) to build mode.
    shell
        .run_phase(&ebuild, "setup", &work, std::path::Path::new("/"))
        .await
        .unwrap();

    // A metadata-only scan of a *different* package on the same shell.
    let ebuild2 =
        Ebuild::from_path(camino::Utf8Path::from_path(&ebdir2.join("pkg2-1.ebuild")).unwrap())
            .unwrap();
    shell.source_ebuild(&ebuild2).await.unwrap();

    // einfo must be back to its metadata-mode no-op: the real builtin
    // prints the message; the stub prints nothing.
    let out = captured_stderr(&mut shell, "einfo marker-message").await;
    assert!(
        !out.contains("marker-message"),
        "einfo dispatched the real builtin during metadata-only work: {out:?}"
    );
}
