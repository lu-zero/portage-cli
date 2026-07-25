//! Demo of the PMS 12.3.14 version functions: `ver_cut`, `ver_rs`, `ver_test`.
//!
//! These are Rust builtins registered by [`EbuildShell`] — no real repository
//! or ebuild is needed to exercise them.

use std::process;

use clap::Parser;
use portage_repo::{EbuildShell, Repository};

#[derive(Parser)]
#[command(about = "Demo the PMS version functions: ver_cut, ver_rs, ver_test")]
struct Args {
    /// Version string to exercise
    #[arg(default_value = "7.1.3_rc2-r4")]
    version: String,
}

/// Run `expr` in the shell via `$()` substitution; return the captured output.
async fn capture(sh: &mut EbuildShell, expr: &str) -> String {
    let _ = sh.run_string(&format!("_r=$({expr})")).await;
    sh.get_var("_r").unwrap_or_default()
}

/// Run `ver_test <args>`; return whether the comparison is true (exit 0).
async fn ver_test(sh: &mut EbuildShell, args: &str) -> bool {
    let _ = sh
        .run_string(&format!("ver_test {args} && _vt=y || _vt=n"))
        .await;
    sh.get_var("_vt").is_some_and(|v| v == "y")
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = Args::parse();
    let ver = args.version;

    // Build a throwaway shell — ver_* builtins need no real repository content.
    let tmp = tempfile::tempdir().expect("create tempdir");
    std::fs::create_dir_all(tmp.path().join("metadata")).unwrap();
    std::fs::write(tmp.path().join("metadata").join("layout.conf"), "").unwrap();
    std::fs::create_dir_all(tmp.path().join("profiles")).unwrap();

    let repo = Repository::builder()
        .in_memory_cache()
        .open(tmp.path())
        .unwrap_or_else(|e| {
            eprintln!("repo: {e}");
            process::exit(1);
        });
    let mut sh = repo.shell().await.unwrap_or_else(|e| {
        eprintln!("shell: {e}");
        process::exit(1);
    });

    // ── ver_cut ───────────────────────────────────────────────────────────────
    println!("ver_cut  (version = {ver:?})");
    println!("{:-<50}", "");
    for range in ["1", "2", "3", "4", "1-2", "2-3", "1-3", "2-", "1-"] {
        let out = capture(&mut sh, &format!("ver_cut {range} '{ver}'")).await;
        println!("  ver_cut {range:<5}  =>  {out:?}");
    }

    // ── ver_rs ────────────────────────────────────────────────────────────────
    println!();
    println!("ver_rs   (version = {ver:?})");
    println!("{:-<50}", "");
    for (idx, sep) in [("1", "-"), ("2", "_"), ("3", ".")] {
        let out = capture(&mut sh, &format!("ver_rs {idx} '{sep}' '{ver}'")).await;
        println!("  ver_rs {idx} {sep:?}     =>  {out:?}");
    }

    // ── ver_test ──────────────────────────────────────────────────────────────
    println!();
    println!("ver_test");
    println!("{:-<50}", "");
    let cases: &[(&str, &str, &str)] = &[
        ("3.12", "-gt", "3.11"),
        ("3.12", "-ge", "3.12"),
        ("3.12", "-eq", "3.12"),
        ("3.11", "-ne", "3.12"),
        ("3.11", "-lt", "3.12"),
        ("3.11", "-le", "3.12"),
        ("3.12", "-gt", "3.12"), // false
        ("1.0_alpha1", "-lt", "1.0"),
        ("1.0_rc1", "-lt", "1.0"),
        ("1.0", "-lt", "1.0_p1"),
    ];
    for (a, op, b) in cases {
        let result = ver_test(&mut sh, &format!("{a} {op} {b}")).await;
        println!("  ver_test {a:<12} {op}  {b:<12}  =>  {result}");
    }
}
