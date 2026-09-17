//! `em query check` — verify checksums and mtimes of installed package files

use anyhow::{Context, Result};
use portage_vdb::{ContentsKind, InstalledPackage, Vdb};

use crate::cli::OutputFormat;
use crate::vdb::find_packages;

/// One failing file from [`check_package`] — enumerated in both the
/// pretty (as a `!!!` line) and JSON (`failures[]`) output.
struct Failure {
    kind: &'static str,
    reason: &'static str,
    path: String,
    detail: Option<String>,
}

pub fn run(vdb: &Vdb, atoms: &[String], format: OutputFormat) -> Result<()> {
    let mut results: Vec<serde_json::Value> = Vec::new();

    for raw in atoms {
        let matched = find_packages(vdb, raw);
        if matched.is_empty() {
            crate::style::warn_line!("no installed package matches '{raw}'");
            continue;
        }
        for pkg in matched {
            let (ok, failures) = check_package(&pkg)?;
            match format {
                OutputFormat::Pretty => {
                    for f in &failures {
                        match &f.detail {
                            Some(d) => eprintln!("  !!! {}  {}  {}: {d}", f.kind, f.reason, f.path),
                            None => eprintln!("  !!! {}  {}  {}", f.kind, f.reason, f.path),
                        }
                    }
                    if failures.is_empty() {
                        println!("{pkg}: {ok} files OK");
                    } else {
                        println!("{pkg}: {ok} files OK, {} failures", failures.len());
                    }
                }
                OutputFormat::Json => {
                    let failures: Vec<serde_json::Value> = failures
                        .iter()
                        .map(|f| {
                            serde_json::json!({
                                "path": f.path,
                                "kind": f.kind,
                                "reason": f.reason,
                                "detail": f.detail,
                            })
                        })
                        .collect();
                    results.push(serde_json::json!({
                        "atom": pkg.to_string(),
                        "ok": ok,
                        "fail": failures.len(),
                        "failures": failures,
                    }));
                }
            }
        }
    }

    if matches!(format, OutputFormat::Json) {
        let json = serde_json::to_string_pretty(&results)
            .map_err(|e| anyhow::anyhow!("failed to serialize JSON output: {e}"))?;
        println!("{json}");
    }
    Ok(())
}

fn check_package(pkg: &InstalledPackage) -> Result<(u32, Vec<Failure>)> {
    let entries = pkg.contents().with_context(|| format!("{pkg}"))?;

    let mut ok: u32 = 0;
    let mut failures: Vec<Failure> = Vec::new();

    for entry in &entries {
        match &entry.kind {
            ContentsKind::Obj => {
                let path = &entry.path;
                match std::fs::read(path.as_std_path()) {
                    Ok(data) => {
                        let digest = format!("{:x}", md5::compute(&data));
                        if entry.md5.as_deref() == Some(digest.as_str()) {
                            ok += 1;
                        } else {
                            failures.push(Failure {
                                kind: "md5",
                                reason: "FAIL",
                                path: path.to_string(),
                                detail: None,
                            });
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        failures.push(Failure {
                            kind: "obj",
                            reason: "MISS",
                            path: path.to_string(),
                            detail: None,
                        });
                    }
                    Err(e) => {
                        failures.push(Failure {
                            kind: "obj",
                            reason: "ERR",
                            path: path.to_string(),
                            detail: Some(e.to_string()),
                        });
                    }
                }
            }
            ContentsKind::Sym => {
                let path = &entry.path;
                match path.as_std_path().symlink_metadata() {
                    Ok(meta) => {
                        if let Some(expected) = entry.mtime {
                            let actual = meta
                                .modified()
                                .ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_secs());
                            if actual != Some(expected) {
                                failures.push(Failure {
                                    kind: "sym",
                                    reason: "MTIME",
                                    path: path.to_string(),
                                    detail: None,
                                });
                            } else {
                                ok += 1;
                            }
                        } else {
                            ok += 1;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        failures.push(Failure {
                            kind: "sym",
                            reason: "MISS",
                            path: path.to_string(),
                            detail: None,
                        });
                    }
                    Err(e) => {
                        failures.push(Failure {
                            kind: "sym",
                            reason: "ERR",
                            path: path.to_string(),
                            detail: Some(e.to_string()),
                        });
                    }
                }
            }
            ContentsKind::Dir | ContentsKind::Fifo | ContentsKind::Dev => {}
        }
    }

    Ok((ok, failures))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn make_vdb_pkg(
        dir: &std::path::Path,
        cat: &str,
        pf: &str,
        fields: &[(&str, &str)],
    ) -> portage_vdb::Vdb {
        let pkg_dir = dir.join(cat).join(pf);
        fs::create_dir_all(&pkg_dir).unwrap();
        for (name, val) in fields {
            fs::write(pkg_dir.join(name), val).unwrap();
        }
        let root: camino::Utf8PathBuf = dir.to_path_buf().try_into().unwrap();
        portage_vdb::Vdb::open(root).unwrap()
    }

    #[test]
    fn check_passes_for_correct_obj() {
        let tmp = tempdir().unwrap();

        let file_dir = tmp.path().join("actual");
        fs::create_dir_all(&file_dir).unwrap();
        let file_path = file_dir.join("hello.txt");
        fs::write(&file_path, b"hello\n").unwrap();

        let digest = format!("{:x}", md5::compute(b"hello\n"));
        let mtime = file_path
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let contents = format!("obj {} {} {}\n", file_path.display(), digest, mtime);
        let vdb = make_vdb_pkg(
            tmp.path(),
            "app-shells",
            "bash-5.3",
            &[("CONTENTS", &contents)],
        );

        let result = run(
            &vdb,
            &["app-shells/bash-5.3".to_string()],
            OutputFormat::Pretty,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn check_json_still_succeeds_with_failures_present() {
        let tmp = tempdir().unwrap();
        // CONTENTS references a file that was never created — a MISS.
        let contents = "obj /nonexistent/path deadbeef 0\n";
        let vdb = make_vdb_pkg(
            tmp.path(),
            "app-shells",
            "bash-5.3",
            &[("CONTENTS", contents)],
        );

        // Reporting failures is not itself an error — `run` still succeeds;
        // the failures are represented in the (JSON) output, not the Result.
        let result = run(
            &vdb,
            &["app-shells/bash-5.3".to_string()],
            OutputFormat::Json,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn check_package_reports_missing_file_as_a_failure() {
        let tmp = tempdir().unwrap();
        let contents = "obj /nonexistent/path deadbeef 0\n";
        let vdb = make_vdb_pkg(
            tmp.path(),
            "app-shells",
            "bash-5.3",
            &[("CONTENTS", contents)],
        );
        // find_packages's bare-name convenience form matches on the plain
        // package name or full PF, not "cat/pn-ver" — use the PF here.
        let pkg = find_packages(&vdb, "bash-5.3").into_iter().next().unwrap();

        let (ok, failures) = check_package(&pkg).unwrap();
        assert_eq!(ok, 0);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, "obj");
        assert_eq!(failures[0].reason, "MISS");
        assert_eq!(failures[0].path, "/nonexistent/path");
    }
}
