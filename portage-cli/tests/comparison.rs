//! Comparison tests: `em query` vs `qfile`/`qlist`/`qsize`/`equery`.
//!
//! These tests only run on a live Gentoo system with `portage-utils` installed.
//! They are ignored by default; run with `cargo test -- --ignored`.

use std::process::Command;

use camino::Utf8Path;

/// The release `em` binary Cargo already built for this test run — not a
/// nested `cargo run` (debug, holds the target lock, swallows exit status).
fn em(args: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_em"))
        .args(args.split_whitespace())
        // `--root` is not global, so it sits after the applet. These tests
        // compare against the host VDB; an `em active` prefix/local on this
        // machine must not redirect that.
        .args(["--root", "/"])
        .output()
        .expect("failed to run em");
    assert!(
        output.status.success(),
        "em {args} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Like [`q`], but doesn't assert exit status: `qgrep` exits 1 whenever it
/// is given atom targets, matches or not (portage-utils 0.97.1: `qgrep -l
/// EAPI=8 dev-python/setuptools` prints hits and exits 1; without targets it
/// exits 0), so target-restricted comparisons must only compare stdout.
fn q_output_only(args: &str) -> String {
    let output = Command::new(args.split_whitespace().next().unwrap())
        .args(args.split_whitespace().skip(1))
        .output()
        .expect("failed to run comparison tool");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn q(args: &str) -> String {
    let output = Command::new(args.split_whitespace().next().unwrap())
        .args(args.split_whitespace().skip(1))
        .output()
        .expect("failed to run comparison tool");
    assert!(
        output.status.success(),
        "{args} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The category/package (without version) from a full atom string, e.g.
/// "app-shells/bash-5.3_p9-r2" -> "app-shells/bash". Falls back to the atom
/// unchanged if it doesn't parse as a `Cpv` (PMS version-boundary parsing
/// already lives in portage-atom; no reason to hand-roll it here too).
fn strip_version(atom: &str) -> String {
    portage_atom::Cpv::parse(atom)
        .map(|cpv| cpv.cpn.to_string())
        .unwrap_or_else(|_| atom.to_string())
}

#[test]
#[ignore]
fn query_belongs_matches_qfile() {
    let files = ["/bin/bash", "/bin/ls", "/usr/bin/qfile", "/usr/bin/make"];

    for file in files {
        let em_out = em(&format!("query belongs {}", file));
        let q_out = q(&format!("qfile {}", file));

        // qfile outputs "category/package: /path", em outputs "category/package-version"
        // Compare the category/package part
        let em_pkg = strip_version(&em_out);
        let q_line = q_out.lines().next().unwrap_or("");
        let q_pkg = q_line.split(':').next().unwrap_or("").trim();

        assert_eq!(em_pkg, q_pkg, "mismatch for {file}: em={em_out} q={q_out}");
    }
}

#[test]
#[ignore]
fn query_files_matches_qlist() {
    // Use category-qualified atoms for exact match (qlist does substring match on PN)
    let atoms = ["app-shells/bash"];

    for atom in atoms {
        let em_out = em(&format!("query files {}", atom));
        let q_out = q(&format!("qlist -e {}", atom));

        // em prepends "category/pkg-version\t", strip it
        let em_clean: Vec<String> = em_out
            .lines()
            .filter_map(|l| l.split('\t').nth(1).map(|s| s.to_string()))
            // qlist suppresses debug files by default, skip them for comparison
            .filter(|p| !p.starts_with("/usr/lib/debug/"))
            .collect();

        let q_lines: Vec<String> = q_out.lines().map(|s| s.to_string()).collect();

        assert_eq!(
            em_clean.len(),
            q_lines.len(),
            "file count mismatch for {atom}: em={} q={}\nextra: {:?}\nmissing: {:?}",
            em_clean.len(),
            q_lines.len(),
            em_clean
                .iter()
                .filter(|p| !q_lines.contains(p))
                .collect::<Vec<_>>(),
            q_lines
                .iter()
                .filter(|p| !em_clean.contains(p))
                .collect::<Vec<_>>(),
        );
    }
}

#[test]
#[ignore]
fn installed_count_matches_qlist() {
    let q_count = q("qlist -I").lines().count();
    let vdb = portage_vdb::Vdb::open(Utf8Path::new("/var/db/pkg")).unwrap();
    let em_count = vdb.packages().into_iter().count();

    assert_eq!(
        em_count, q_count,
        "installed package count mismatch: em={em_count} qlist={q_count}"
    );
}

#[test]
#[ignore]
fn grep_default_matches_qgrep() {
    let em_out = em("grep PYTHON_COMPAT dev-python/setuptools");
    let q_out = q_output_only("qgrep PYTHON_COMPAT dev-python/setuptools");
    assert_eq!(em_out, q_out);
}

#[test]
#[ignore]
fn grep_atom_name_matches_qgrep() {
    let em_out = em("grep -N EAPI=8 dev-python/setuptools");
    let q_out = q_output_only("qgrep -N EAPI=8 dev-python/setuptools");
    assert_eq!(em_out, q_out);
}

#[test]
#[ignore]
fn grep_list_matches_qgrep() {
    let em_out = em("grep -l PYTHON_COMPAT dev-python/setuptools");
    let q_out = q_output_only("qgrep -l PYTHON_COMPAT dev-python/setuptools");
    assert_eq!(em_out, q_out);
}

#[test]
#[ignore]
fn grep_count_matches_qgrep() {
    let em_out = em("grep -c PYTHON_COMPAT dev-python/setuptools");
    let q_out = q_output_only("qgrep -c PYTHON_COMPAT dev-python/setuptools");
    assert_eq!(em_out, q_out);
}

#[test]
#[ignore]
fn grep_eclass_matches_qgrep() {
    // Same line set, different order: `em grep -E` walks eclasses sorted by
    // name (deterministic); qgrep's own order is directory-iteration order.
    // Sort both before comparing.
    let em_out = em("grep -E EXPORT_FUNCTIONS");
    let q_out = q("qgrep -E EXPORT_FUNCTIONS");
    let mut em_lines: Vec<&str> = em_out.lines().collect();
    let mut q_lines: Vec<&str> = q_out.lines().collect();
    em_lines.sort_unstable();
    q_lines.sort_unstable();
    assert_eq!(em_lines, q_lines);
}

/// Raw `em portageq` run: no `--root` (this applet carries no topology)
/// and no success assertion, since miss cases exit 1 by design.
fn em_pq(args: &[&str]) -> (i32, String, String) {
    em_pq_env(args, &[])
}

/// [`em_pq`] with extra variables in the child's environment only.
fn em_pq_env(args: &[&str], vars: &[(&str, &str)]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_em"))
        .args(["portageq"])
        .args(args)
        .envs(vars.iter().copied())
        .output()
        .expect("failed to run em");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Raw real `portageq` run with the same no-success-assertion shape.
fn real_pq(args: &[&str]) -> (i32, String, String) {
    real_pq_env(args, &[])
}

/// [`real_pq`] with extra variables in the child's environment only.
fn real_pq_env(args: &[&str], vars: &[(&str, &str)]) -> (i32, String, String) {
    let output = Command::new("/usr/bin/portageq")
        .args(args)
        .envs(vars.iter().copied())
        .output()
        .expect("failed to run comparison tool");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn have_portageq() -> bool {
    std::path::Path::new("/usr/bin/portageq").exists()
}

/// Cases below assume host state: `app-shells/bash` installed, a `gentoo`
/// repo configured, `CFLAGS` set. A failure here means the host differs.
fn check_pq_parity(cases: &[&[&str]]) {
    if !have_portageq() {
        eprintln!("skipping: /usr/bin/portageq not present");
        return;
    }
    for args in cases {
        assert_eq!(em_pq(args), real_pq(args), "mismatch for {args:?}");
    }
}

#[test]
#[ignore]
fn portageq_vdb_group_matches_real() {
    check_pq_parity(&[
        &["has_version", "/", "app-shells/bash"],
        &["has_version", "/", "app-shells/no-such-pkg-xyz"],
        &["best_version", "/", "app-shells/bash"],
        &["best_version", "/", "app-shells/no-such-pkg-xyz"],
        &["match", "/", "app-shells/bash"],
        &[
            "mass_best_version",
            "/",
            "app-shells/bash",
            "app-shells/no-such-pkg-xyz",
        ],
    ]);
}

#[test]
#[ignore]
fn portageq_envvar_matches_real() {
    check_pq_parity(&[
        &["envvar", "CFLAGS"],
        &["envvar", "-v", "CFLAGS"],
        &["envvar", "CFLAGS", "EM_NO_SUCH_VAR_XYZ"],
        &["envvar"],
    ]);
}

#[test]
#[ignore]
fn portageq_settings_match_real() {
    check_pq_parity(&[
        &["vdb_path"],
        &["pkgdir"],
        &["distdir"],
        &["gentoo_mirrors"],
        &["config_protect"],
        &["config_protect_mask"],
        &["portdir"],
        &["portdir_overlay"],
    ]);
}

#[test]
#[ignore]
fn portageq_repos_match_real() {
    check_pq_parity(&[
        &["get_repos", "/"],
        &["get_repos", "/no/such/eroot"],
        &["get_repo_path", "/", "gentoo"],
        &["get_repo_path", "/", "no-such-repo-xyz"],
        &["get_repo_path", "/", "bad/name"],
        &["get_repo_path", "/"],
    ]);
}

#[test]
#[ignore]
fn portageq_env_from_process_environment_matches_real() {
    if !have_portageq() {
        eprintln!("skipping: /usr/bin/portageq not present");
        return;
    }
    let vars = [("EM_PQ_QUOTED_XYZ", "a'b"), ("EM_PQ_EMPTY_XYZ", "")];
    for args in [
        &["envvar", "-v", "EM_PQ_QUOTED_XYZ"][..],
        &["envvar", "EM_PQ_EMPTY_XYZ"][..],
    ] {
        assert_eq!(
            em_pq_env(args, &vars),
            real_pq_env(args, &vars),
            "mismatch for {args:?}"
        );
    }
}
