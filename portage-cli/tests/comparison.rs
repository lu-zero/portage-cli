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
        // Explicit real-host root, trailing (clap wants it after the
        // applet name): these tests compare against the host's own
        // `qfile`/`qlist`/VDB, so an `em active` prefix/local registered
        // on this machine must not redirect em's own view of things.
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
