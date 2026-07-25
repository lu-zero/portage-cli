//! Integration tests for ver_cut, ver_rs, and ver_test.
//!
//! These tests are ported from Gentoo's `eclass/tests/version-funcs.sh`
//! and exercise the actual bash implementation registered by
//! [`portage_repo::EbuildShell`].

use portage_repo::{EbuildShell, Repository};
use tempfile::TempDir;

/// Raise the process soft fd limit to the hard limit once per test binary.
/// The default macOS soft limit (256) is too low for 124 concurrent shells,
/// each of which clones stdin/stdout/stderr during brush initialisation.
fn raise_fd_limit() {
    let max = rustix::process::getrlimit(rustix::process::Resource::Nofile).maximum;
    if let Some(max) = max {
        let _ = rustix::process::setrlimit(
            rustix::process::Resource::Nofile,
            rustix::process::Rlimit {
                current: Some(max),
                maximum: Some(max),
            },
        );
    }
}

/// Minimal repo dir shared across all tests — created once, never deleted.
/// The ver_* tests only need a shell with builtins loaded; no repo files
/// are accessed after shell construction.
static TEST_REPO: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();

fn test_repo_dir() -> &'static std::path::Path {
    TEST_REPO
        .get_or_init(|| {
            raise_fd_limit();
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            std::fs::create_dir_all(root.join("metadata")).unwrap();
            std::fs::write(root.join("metadata/layout.conf"), "masters =\n").unwrap();
            std::fs::create_dir_all(root.join("profiles")).unwrap();
            std::fs::write(root.join("profiles/repo_name"), "test-repo\n").unwrap();
            tmp
        })
        .path()
}

/// Create a shell for testing ver_* functions.
async fn test_shell() -> EbuildShell {
    let repo = Repository::builder()
        .in_memory_cache()
        .open(test_repo_dir())
        .unwrap();
    repo.shell().await.unwrap()
}

/// Source a bash script string in the shell.
async fn run_script(shell: &mut EbuildShell, script: &str) {
    let _ = shell.run_string(script).await;
}

/// Run a bash expression and return its stdout (trimmed).
async fn eval(shell: &mut EbuildShell, script: &str) -> String {
    let wrapper = format!("__test_out=$({script})\n");
    run_script(shell, &wrapper).await;
    shell.get_var("__test_out").unwrap_or_default()
}

/// Run a bash command and return its exit code.
async fn exit_code(shell: &mut EbuildShell, script: &str) -> i32 {
    let wrapper = format!("{script}\n__test_rc=$?\n");
    run_script(shell, &wrapper).await;
    shell
        .get_var("__test_rc")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(-1)
}

// ─── ver_cut tests ──────────────────────────────────────────────────

macro_rules! ver_cut_test {
    ($name:ident, $expected:expr, $range:expr, $version:expr) => {
        #[tokio::test]
        async fn $name() {
            let mut shell = test_shell().await;
            let got = eval(&mut shell, &format!("ver_cut {} {}", $range, $version)).await;
            assert_eq!(
                got, $expected,
                "ver_cut {} {} -> expected {:?}",
                $range, $version, $expected
            );
        }
    };
}

ver_cut_test!(ver_cut_1, "1", "1", "1.2.3");
ver_cut_test!(ver_cut_1_1, "1", "1-1", "1.2.3");
ver_cut_test!(ver_cut_1_2, "1.2", "1-2", "1.2.3");
ver_cut_test!(ver_cut_2_end, "2.3", "2-", "1.2.3");
ver_cut_test!(ver_cut_1_end, "1.2.3", "1-", "1.2.3");
ver_cut_test!(ver_cut_3_4, "3b", "3-4", "1.2.3b_alpha4");
ver_cut_test!(ver_cut_5, "alpha", "5", "1.2.3b_alpha4");
ver_cut_test!(ver_cut_1_2_dot_prefix, "1.2", "1-2", ".1.2.3");
ver_cut_test!(ver_cut_0_2_dot_prefix, ".1.2", "0-2", ".1.2.3");
ver_cut_test!(ver_cut_2_3_dot_suffix, "2.3", "2-3", "1.2.3.");
ver_cut_test!(ver_cut_2_end_dot_suffix, "2.3.", "2-", "1.2.3.");
ver_cut_test!(ver_cut_2_4_dot_suffix, "2.3.", "2-4", "1.2.3.");

// ─── ver_rs tests ───────────────────────────────────────────────────

macro_rules! ver_rs_test {
    ($name:ident, $expected:expr, $($args:expr),+) => {
        #[tokio::test]
        async fn $name() {
            let mut shell = test_shell().await;
            let args = vec![$($args),+];
            let cmd = format!("ver_rs {}", args.join(" "));
            let got = eval(&mut shell, &cmd).await;
            let expected: &str = $expected;
            assert_eq!(got, expected, "{} -> expected {:?}", cmd, expected);
        }
    };
}

ver_rs_test!(ver_rs_1_dash, "1-2.3", "1", "-", "1.2.3");
ver_rs_test!(ver_rs_2_dash, "1.2-3", "2", "-", "1.2.3");
ver_rs_test!(ver_rs_1_2_dash, "1-2-3.4", "1-2", "-", "1.2.3.4");
ver_rs_test!(ver_rs_2_end_dash, "1.2-3-4", "2-", "-", "1.2.3.4");
ver_rs_test!(ver_rs_2_dot, "1.2.3", "2", ".", "1.2-3");
ver_rs_test!(ver_rs_3_dot, "1.2.3.a", "3", ".", "1.2.3a");
ver_rs_test!(ver_rs_2_3_dash, "1.2-alpha-4", "2-3", "-", "1.2_alpha4");
ver_rs_test!(
    ver_rs_multi_pair,
    "1.23-b_alpha4",
    "3",
    "-",
    "2",
    "\"\"",
    "1.2.3b_alpha4"
);

ver_rs_test!(
    ver_rs_multi_pair2,
    "a1b_2-c-3-d4e5",
    "3-5",
    "_",
    "4-6",
    "-",
    "a1b2c3d4e5"
);
ver_rs_test!(ver_rs_dot_prefix_1, ".1-2.3", "1", "-", ".1.2.3");
ver_rs_test!(ver_rs_dot_prefix_0, "-1.2.3", "0", "-", ".1.2.3");

// ─── Truncating range tests ─────────────────────────────────────────

ver_cut_test!(ver_cut_trunc_0_2, "1.2", "0-2", "1.2.3");
ver_cut_test!(ver_cut_trunc_2_5, "2.3", "2-5", "1.2.3");

#[tokio::test]
async fn ver_cut_trunc_4() {
    let mut shell = test_shell().await;
    let got = eval(&mut shell, "ver_cut 4 1.2.3").await;
    assert_eq!(got, "");
}

#[tokio::test]
async fn ver_cut_trunc_0() {
    let mut shell = test_shell().await;
    let got = eval(&mut shell, "ver_cut 0 1.2.3").await;
    assert_eq!(got, "");
}

#[tokio::test]
async fn ver_cut_trunc_4_end() {
    let mut shell = test_shell().await;
    let got = eval(&mut shell, "ver_cut 4- 1.2.3").await;
    assert_eq!(got, "");
}

// ver_rs truncating ranges (no change expected)
ver_rs_test!(ver_rs_trunc_0, "1.2.3", "0", "-", "1.2.3");
ver_rs_test!(ver_rs_trunc_3, "1.2.3", "3", ".", "1.2.3");
ver_rs_test!(ver_rs_trunc_3_end, "1.2.3", "3-", ".", "1.2.3");
ver_rs_test!(ver_rs_trunc_3_5, "1.2.3", "3-5", ".", "1.2.3");

// ─── ver_cut/ver_rs die tests ───────────────────────────────────────

#[tokio::test]
async fn ver_cut_die_foo() {
    let mut shell = test_shell().await;
    let got = eval(&mut shell, "ver_cut foo 1.2.3 2>&1").await;
    assert!(got.starts_with("die:"), "expected die, got: {got}");
}

#[tokio::test]
async fn ver_rs_die_negative() {
    let mut shell = test_shell().await;
    let got = eval(&mut shell, "ver_rs -3 _ a1b2c3d4e5 2>&1").await;
    assert!(got.starts_with("die:"), "expected die, got: {got}");
}

#[tokio::test]
async fn ver_rs_die_reversed() {
    let mut shell = test_shell().await;
    let got = eval(&mut shell, "ver_rs 5-3 _ a1b2c3d4e5 2>&1").await;
    assert!(got.starts_with("die:"), "expected die, got: {got}");
}

// ─── ver_test comparison tests ──────────────────────────────────────

macro_rules! ver_test_case {
    ($name:ident, $expected:expr, $v1:expr, $op:expr, $v2:expr) => {
        #[tokio::test]
        async fn $name() {
            let mut shell = test_shell().await;
            let code = exit_code(&mut shell, &format!("ver_test {} {} {}", $v1, $op, $v2)).await;
            assert_eq!(
                code, $expected,
                "ver_test {} {} {} -> expected exit {}",
                $v1, $op, $v2, $expected
            );
        }
    };
}

// Tests from Portage's test_vercmp.py
ver_test_case!(vt_gt_6_5, 0, "6.0", "-gt", "5.0");
ver_test_case!(vt_gt_5_5, 0, "5.0", "-gt", "5");
ver_test_case!(vt_gt_r1_r0, 0, "1.0-r1", "-gt", "1.0-r0");
ver_test_case!(
    vt_gt_big_int,
    0,
    "999999999999999999",
    "-gt",
    "999999999999999998"
);
ver_test_case!(vt_gt_1_0_0_vs_1_0, 0, "1.0.0", "-gt", "1.0");
ver_test_case!(vt_gt_1_0_0_vs_1_0b, 0, "1.0.0", "-gt", "1.0b");
ver_test_case!(vt_gt_1b_vs_1, 0, "1b", "-gt", "1");
ver_test_case!(vt_gt_1b_p1_vs_1_p1, 0, "1b_p1", "-gt", "1_p1");
ver_test_case!(vt_gt_1_1b_vs_1_1, 0, "1.1b", "-gt", "1.1");
ver_test_case!(vt_gt_12_2_5_vs_12_2b, 0, "12.2.5", "-gt", "12.2b");
ver_test_case!(vt_lt_4_5, 0, "4.0", "-lt", "5.0");
ver_test_case!(vt_lt_5_5_0, 0, "5", "-lt", "5.0");
ver_test_case!(vt_lt_pre2_p2, 0, "1.0_pre2", "-lt", "1.0_p2");
ver_test_case!(vt_lt_alpha2_p2, 0, "1.0_alpha2", "-lt", "1.0_p2");
ver_test_case!(vt_lt_alpha1_beta1, 0, "1.0_alpha1", "-lt", "1.0_beta1");
ver_test_case!(vt_lt_beta3_rc3, 0, "1.0_beta3", "-lt", "1.0_rc3");
ver_test_case!(
    vt_lt_leading_zero_long,
    0,
    "1.001000000000000001",
    "-lt",
    "1.001000000000000002"
);
ver_test_case!(
    vt_lt_leading_zero_short_long,
    0,
    "1.00100000000",
    "-lt",
    "1.001000000000000001"
);
ver_test_case!(
    vt_lt_big_int,
    0,
    "999999999999999998",
    "-lt",
    "999999999999999999"
);
ver_test_case!(vt_lt_01_1, 0, "1.01", "-lt", "1.1");
ver_test_case!(vt_lt_r0_r1, 0, "1.0-r0", "-lt", "1.0-r1");
ver_test_case!(vt_lt_no_rev_r1, 0, "1.0", "-lt", "1.0-r1");
ver_test_case!(vt_lt_1_0_vs_1_0_0, 0, "1.0", "-lt", "1.0.0");
ver_test_case!(vt_lt_1_0b_vs_1_0_0, 0, "1.0b", "-lt", "1.0.0");
ver_test_case!(vt_lt_1_p1_vs_1b_p1, 0, "1_p1", "-lt", "1b_p1");
ver_test_case!(vt_lt_1_vs_1b, 0, "1", "-lt", "1b");
ver_test_case!(vt_lt_1_1_vs_1_1b, 0, "1.1", "-lt", "1.1b");
ver_test_case!(vt_lt_12_2b_vs_12_2_5, 0, "12.2b", "-lt", "12.2.5");
ver_test_case!(vt_eq_4_4, 0, "4.0", "-eq", "4.0");
ver_test_case!(vt_eq_1_1, 0, "1.0", "-eq", "1.0");
ver_test_case!(vt_eq_r0_none, 0, "1.0-r0", "-eq", "1.0");
ver_test_case!(vt_eq_none_r0, 0, "1.0", "-eq", "1.0-r0");
ver_test_case!(vt_eq_r0_r0, 0, "1.0-r0", "-eq", "1.0-r0");
ver_test_case!(vt_eq_r1_r1, 0, "1.0-r1", "-eq", "1.0-r1");
ver_test_case!(vt_ne_1_2, 1, "1", "-eq", "2");
ver_test_case!(vt_ne_alpha_pre, 1, "1.0_alpha", "-eq", "1.0_pre");
ver_test_case!(vt_ne_beta_alpha, 1, "1.0_beta", "-eq", "1.0_alpha");
ver_test_case!(vt_ne_1_0_0, 1, "1", "-eq", "0.0");
ver_test_case!(vt_ne_r0_r1, 1, "1.0-r0", "-eq", "1.0-r1");
ver_test_case!(vt_ne_r1_r0, 1, "1.0-r1", "-eq", "1.0-r0");
ver_test_case!(vt_ne_none_r1, 1, "1.0", "-eq", "1.0-r1");
ver_test_case!(vt_ne_r1_none, 1, "1.0-r1", "-eq", "1.0");
ver_test_case!(vt_ne_1_0_vs_1_0_0, 1, "1.0", "-eq", "1.0.0");
ver_test_case!(vt_ne_1_p1_vs_1b_p1, 1, "1_p1", "-eq", "1b_p1");
ver_test_case!(vt_ne_1b_vs_1, 1, "1b", "-eq", "1");
ver_test_case!(vt_ne_1_1b_vs_1_1, 1, "1.1b", "-eq", "1.1");
ver_test_case!(vt_ne_12_2b_vs_12_2, 1, "12.2b", "-eq", "12.2");

// Paludis subset
ver_test_case!(
    vt_paludis_alpha_gt_1_alpha,
    0,
    "1.0_alpha",
    "-gt",
    "1_alpha"
);
ver_test_case!(vt_paludis_alpha_gt_1, 0, "1.0_alpha", "-gt", "1");
ver_test_case!(vt_paludis_alpha_lt_1_0, 0, "1.0_alpha", "-lt", "1.0");
ver_test_case!(
    vt_paludis_complex_gt,
    0,
    "1.2.0.0_alpha7-r4",
    "-gt",
    "1.2_alpha7-r4"
);
ver_test_case!(vt_paludis_leading_zero_eq, 0, "0001", "-eq", "1");
ver_test_case!(vt_paludis_leading_zero_eq2, 0, "01", "-eq", "001");
ver_test_case!(vt_paludis_leading_zero_dot, 0, "0001.1", "-eq", "1.1");
ver_test_case!(vt_paludis_01_01, 0, "01.01", "-eq", "1.01");
ver_test_case!(vt_paludis_1_010, 0, "1.010", "-eq", "1.01");
ver_test_case!(vt_paludis_1_00, 0, "1.00", "-eq", "1.0");
ver_test_case!(vt_paludis_1_0100, 0, "1.0100", "-eq", "1.010");
ver_test_case!(vt_paludis_r00, 0, "1-r00", "-eq", "1-r0");

// Additional tests
ver_test_case!(vt_add_rc99_lt_0, 0, "0_rc99", "-lt", "0");
ver_test_case!(vt_add_011_eq_11, 0, "011", "-eq", "11");
ver_test_case!(vt_add_019_eq_19, 0, "019", "-eq", "19");
ver_test_case!(vt_add_1_2_eq_001_2, 0, "1.2", "-eq", "001.2");
ver_test_case!(vt_add_1_2_gt_1_02, 0, "1.2", "-gt", "1.02");
ver_test_case!(vt_add_1_2a_lt_1_2b, 0, "1.2a", "-lt", "1.2b");
ver_test_case!(
    vt_add_pre1_gt_pre1_beta2,
    0,
    "1.2_pre1",
    "-gt",
    "1.2_pre1_beta2"
);
ver_test_case!(vt_add_pre1_lt_pre1_p2, 0, "1.2_pre1", "-lt", "1.2_pre1_p2");
ver_test_case!(vt_add_1_00_lt_1_0_0, 0, "1.00", "-lt", "1.0.0");
ver_test_case!(vt_add_1_010_eq_1_01, 0, "1.010", "-eq", "1.01");
ver_test_case!(vt_add_1_01_lt_1_1, 0, "1.01", "-lt", "1.1");
ver_test_case!(vt_add_pre08_r09, 0, "1.2_pre08-r09", "-eq", "1.2_pre8-r9");
ver_test_case!(vt_add_0_lt_2pow59, 0, "0", "-lt", "576460752303423488");
ver_test_case!(vt_add_0_lt_2pow63, 0, "0", "-lt", "9223372036854775808");

// ─── ver_test bad arguments die tests ───────────────────────────────

#[tokio::test]
async fn ver_test_die_1_arg() {
    let mut shell = test_shell().await;
    let got = eval(&mut shell, "ver_test 1 2>&1").await;
    assert!(got.starts_with("die:"), "expected die, got: {got}");
}

#[tokio::test]
async fn ver_test_die_4_args() {
    let mut shell = test_shell().await;
    let got = eval(&mut shell, "ver_test 1 -lt 2 3 2>&1").await;
    assert!(got.starts_with("die:"), "expected die, got: {got}");
}

#[tokio::test]
async fn ver_test_die_op_first() {
    let mut shell = test_shell().await;
    let got = eval(&mut shell, "ver_test -lt 1 2 2>&1").await;
    assert!(got.starts_with("die:"), "expected die, got: {got}");
}

// ─── ver_test bad operator die tests ────────────────────────────────

macro_rules! ver_test_die_op {
    ($name:ident, $op:expr) => {
        #[tokio::test]
        async fn $name() {
            let mut shell = test_shell().await;
            let got = eval(&mut shell, &format!("ver_test 1 {} 2 2>&1", $op)).await;
            assert!(
                got.starts_with("die:"),
                "expected die for op {:?}, got: {got}",
                $op
            );
        }
    };
}

ver_test_die_op!(ver_test_die_op_lt_sym, "'<'");
ver_test_die_op!(ver_test_die_op_lt_word, "lt");
ver_test_die_op!(ver_test_die_op_foo, "-foo");

// ─── ver_test malformed version die tests ───────────────────────────

macro_rules! ver_test_die_version {
    ($name:ident, $bad_ver:expr) => {
        #[tokio::test]
        async fn $name() {
            let mut shell = test_shell().await;
            let got = eval(&mut shell, &format!("ver_test {} -ne 1 2>&1", $bad_ver)).await;
            assert!(
                got.starts_with("die:"),
                "expected die for version {:?}, got: {got}",
                $bad_ver
            );
        }
    };
}

ver_test_die_version!(ver_test_die_empty, "\"\"");
ver_test_die_version!(ver_test_die_trailing_dot, "1.");
ver_test_die_version!(ver_test_die_1ab, "1ab");
ver_test_die_version!(ver_test_die_b, "b");
ver_test_die_version!(ver_test_die_1_r1_pre, "1-r1_pre");
ver_test_die_version!(ver_test_die_1_pre1_nodash, "1-pre1");
ver_test_die_version!(ver_test_die_1_foo, "1_foo");
ver_test_die_version!(ver_test_die_1_pre1_dot, "1_pre1.1");
ver_test_die_version!(ver_test_die_1_r1_dot, "1-r1.0");
ver_test_die_version!(ver_test_die_cvs, "cvs.9999");
