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
/// `has_version`/`best_version` builtins query the VDB under the root the
/// -b/-d/-r flag names; phase shells unset the metadata-sourcing stubs so
/// the builtins take over (the stub shadowed them and made
/// autotools.eclass's autoconf probe die in every build).
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
    shell
        .run_string(&format!(
            "unset -f has_version best_version; BROOT={}; \
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
            "unset -f einstall; cd {}; \
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

/// `use_with`/`use_enable`'s explicit-empty second argument
/// (`use_with brotli '' link`, as `net-libs/gnutls` calls it) must fall
/// back to the flag name, matching bash's `${2:-$1}` in real portage's
/// `use_with()` — not just an omitted argument. An empty `Option<String>`
/// still satisfies `Option::unwrap_or`'s `Some` case, so a naive
/// translation silently drops the feature name entirely, producing
/// `--without-` instead of `--without-brotli` (which `./configure` then
/// warns is unrecognized and ignores, leaving the feature auto-detected
/// regardless of the requested USE flag).
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

/// A profile/make.conf-sourced variable must reach a *real* subprocess an
/// ebuild/eclass spawns directly — not just brush's in-process variable
/// table (which is all `get_var`/em's Rust builtins need). `MULTILIB_ABIS`
/// stands in for any such variable em doesn't specifically know about
/// (this is the exact shape of the CHOST bug: invisible to a real child
/// process, even though brush itself sees it fine).
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
             unset -f dodir keepdir doins doexe dobin dosbin dodoc doheader \
                      doinfo doman domo dolib dolib.a dolib.so dosym fperms fowners \
                      newbin newsbin newins newexe newdoc newman newheader newlib.a newlib.so newinitd newconfd newenvd; \
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
             unset -f newbin newsbin newins newexe newdoc newman newheader \
                      newlib.a newlib.so newinitd newconfd newenvd; \
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
    // The metadata stubs shadow the Rust builtins until init_build_env
    // unsets them; do the same here so the builtins run.
    shell
        .run_string(
            "unset -f docompress dostrip; \
             docompress /opt/data /usr/share/extra; \
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

/// Regression test for the 2026-07-16 fix: the cross-toolchain PATH/CC
/// selection used to derive its bin dir from `build_config_root`
/// (`PORTAGE_CONFIGROOT`) — a proxy that only coincidentally matched the
/// crossdev sysroot layout. It must instead come from `build_broot`
/// (`Cli::broot()`'s merge root — the real host for a privileged `--root`,
/// the prefix itself for an unprivileged `--prefix` overlay), so a `${CHOST}-
/// gcc` built into the prefix (not the host) is still found.
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
    shell.set_build_roots(Some(&decoy_config_root), None, None, Some(&broot_utf8));

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

/// Without a `${CHOST}-gcc` reachable at all (no `build_broot`, and a bogus
/// tuple that can't be on the real test-runner's `$PATH`), the cross-
/// toolchain block must leave `CC` untouched rather than setting a bare,
/// unreachable `${CHOST}-gcc`.
#[tokio::test]
async fn cross_toolchain_selection_no_op_when_tool_unreachable() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    shell.set_var("CHOST", "bogus-tuple-that-does-not-exist");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.init_build_env().await.unwrap();

    assert!(shell.get_var("CC").unwrap_or_default().is_empty());
}

/// Regression test for a bug found live 2026-07-17 building `sys-libs/readline`
/// in a from-scratch riscv64 cross build: the cross-toolchain block's own
/// gate only checks that `${chost}-gcc` exists, then used to set all 12
/// tool vars (including `PKG_CONFIG`) unconditionally from that same
/// assumption. Unlike the other 11 (built by crossdev's own toolchain steps
/// alongside gcc), nothing em builds ever creates a `${chost}-pkg-config`
/// wrapper, so `PKG_CONFIG` ended up pointing at a file that doesn't exist —
/// worse than leaving it unset, since `tc-getPKG_CONFIG`'s own "already set"
/// fast path then skips its real PATH-search/bare-name fallback entirely,
/// turning a normally-recoverable "no pkg-config" case into a hard failure.
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
    shell.set_build_roots(None, None, None, Some(&broot_utf8));

    shell.set_var("CHOST", "riscv64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.init_build_env().await.unwrap();

    assert!(shell.get_var("CC").unwrap_or_default().ends_with("-gcc"));
    assert!(
        shell.get_var("PKG_CONFIG").unwrap_or_default().is_empty(),
        "PKG_CONFIG must stay unset when the wrapper doesn't exist, not point at a dead path"
    );
}

/// Phase 2 of the `--prefix`/`--local` toolchain-awareness work
/// (`todo/select-toolchain.md`): a **native** build (CHOST == CBUILD) under
/// `--prefix`/`--local` used to never enter the toolchain-selection block at
/// all (it was gated on `chost != cbuild`), so it silently fell through to
/// the host's `gcc` on `$PATH` even after the prefix had built and activated
/// its own compiler. `build_eprefix.is_some()` now also opens the gate;
/// `build_broot` (already topology-correct — see the gate's own doc comment)
/// is still the bin-dir source, unchanged from the cross case.
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
    // itself (see the gate's own doc comment on `Cli::broot()`).
    shell.set_build_roots(None, None, Some(&prefix_utf8), Some(&prefix_utf8));

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

/// Regression test for a bug found live 2026-08-04 bootstrapping riscv64
/// glibc under `--prefix`: a genuine cross-*target* package (`CTARGET` set,
/// no `TARGET_ABI` — matches `crossdev --setup`'s "libc"/"kernel headers"
/// steps' own `package.env`, unlike the host-arch toolchain-*tool* packages)
/// must get `CC`/etc. as `${CTARGET}-<tool>`, not `${CHOST}-<tool>`, even
/// though `CHOST` here is the *ambient* value (e.g. `crossdev --setup`'s
/// `bypass_cross_root` routes these steps through host config for unrelated
/// reasons) — real glibc's own `sanity_prechecks` runs a `tc-getCPP
/// ${CTARGET}` probe that can only self-correct to the right cross tool when
/// `$CC` starts out unset; a premature `${CHOST}-gcc` export defeats it.
/// Also checks the new `BUILD_CC` export: real toolchain-funcs.eclass's
/// `tc-getBUILD_CC` checks `BUILD_CC`/`CC_FOR_BUILD`/`HOSTCC` (never plain
/// `CC`) once genuinely cross-compiling, so a host-side sub-probe still
/// needs a real ambient/CBUILD-side compiler available under that name.
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
    shell.set_build_roots(None, None, Some(&prefix_utf8), Some(&prefix_utf8));

    // Ambient CHOST/CBUILD both aarch64 (matching bypass_cross_root's
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

/// The host-arch toolchain-*tool* package class (`binutils`/`gcc`/`gdb` —
/// `package.env` marks these with `TARGET_ABI`, unlike genuine target
/// packages) must keep using `${CHOST}-<tool>`, exactly as before this fix —
/// their own compile identity genuinely is the host's, `CTARGET` there only
/// describes what the *resulting* cross compiler will target, not this
/// package's own build.
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
    shell.set_build_roots(None, None, Some(&prefix_utf8), Some(&prefix_utf8));

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

/// A plain `--root`/bare build (no `--prefix`/`--local`, so `build_eprefix`
/// stays `None`) must NOT be affected by the Phase 2 gate change — real
/// `--root` defaulting to the host's `gcc` on `PATH` is correct as-is
/// (catalyst seed-compiler model, confirmed not a bug).
#[tokio::test]
async fn native_toolchain_selection_is_a_no_op_without_eprefix() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    shell.set_var("CHOST", "aarch64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.init_build_env().await.unwrap();

    assert!(shell.get_var("CC").unwrap_or_default().is_empty());
}

/// Same `PKG_CONFIG`-must-not-point-at-a-dead-wrapper guard as
/// `cross_toolchain_selection_skips_pkg_config_when_wrapper_missing`, for the
/// native-prefix path opened by Phase 2.
#[tokio::test]
async fn native_toolchain_selection_skips_pkg_config_when_wrapper_missing() {
    let dir = tempdir().unwrap();
    let mut shell = minimal_shell(dir.path()).await;

    let prefix = dir.path().join("prefix");
    let bin = prefix.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("aarch64-unknown-linux-gnu-gcc"), "#!/bin/sh\n:\n").unwrap();

    let prefix_utf8 = Utf8PathBuf::from_path_buf(prefix).unwrap();
    shell.set_build_roots(None, None, Some(&prefix_utf8), Some(&prefix_utf8));

    shell.set_var("CHOST", "aarch64-unknown-linux-gnu");
    shell.set_var("CBUILD", "aarch64-unknown-linux-gnu");
    shell.init_build_env().await.unwrap();

    assert!(shell.get_var("CC").unwrap_or_default().ends_with("-gcc"));
    assert!(
        shell.get_var("PKG_CONFIG").unwrap_or_default().is_empty(),
        "PKG_CONFIG must stay unset when the wrapper doesn't exist, not point at a dead path"
    );
}

/// Build a minimal ebuild fixture at `<category>/<pn>` and return its path,
/// for tests that need `run_phase` (category/pn only exist per-ebuild,
/// unlike the `init_build_env`-only tests above).
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

/// Regression test for a bug found live 2026-08-04 bootstrapping a riscv64
/// cross toolchain under `--prefix`: `cross-<tuple>/binutils` compiles with
/// the prefix's own native compiler, whose default system header search is
/// confined to `<prefix>/usr/include` — it has no knowledge of the real
/// host's `/usr/include`, where a BDEPEND like dev-libs/elfutils
/// (`USE=debuginfod`) that isn't itself installed as a native prefix package
/// actually lives. `binutils/dwarf.c` hit `elfutils/debuginfod.h: No such
/// file or directory` despite pkg-config correctly finding
/// `libdebuginfod >= 0.188` on the host. CPPFLAGS must gain a `-idirafter
/// /usr/include` fallback for exactly this package class, under a
/// `--prefix`-style overlay (`build_sysroot` set — see `build_sysroot`'s own
/// doc comment: `Some` only for the host-borrowing overlay case, `None` for
/// `--local`'s standalone closure).
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
    shell.set_build_roots(None, Some(&sysroot), None, None);

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

/// Same package class, but no `build_sysroot` (`--local`'s standalone
/// closure, not a `--prefix` overlay — `build_sysroot` stays `None`): must
/// NOT get the host include fallback. `--local` is meant to own everything
/// itself, not reach for the host's headers.
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

/// An ordinary (non-`cross-*`) package must never get the host include
/// fallback, even under a `--prefix` overlay — it is specific to the
/// host-arch crossdev toolchain-tool package class.
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
    shell.set_build_roots(None, Some(&sysroot), None, None);

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
