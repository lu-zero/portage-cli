use brush_core::builtins;
use clap::Parser;

// ── P4 unpack ─────────────────────────────────────────────────────────────────

/// `unpack <archive>...`  (PMS 12.3.11)
///
/// Extracts one or more archives into the current directory (`$S`).
/// Archives named without a path separator are looked up in `$DISTDIR`.
/// Dispatches to the appropriate extraction tool based on file extension.
/// Extension matching is case-insensitive for EAPI ≤ 5, case-sensitive for EAPI ≥ 6.
///
/// See [PMS 12.3.11](https://projects.gentoo.org/pms/9/pms.html#unpack).
#[derive(Parser)]
pub(crate) struct UnpackCommand {
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    archives: Vec<String>,
}

impl builtins::Command for UnpackCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        // unpack self-dies on failure so a bare `unpack` aborts the
        // build without the phase driver having to treat the phase's exit status
        // as fatal. The per-archive "die:" diagnostics are printed in the
        // blocking task below; this only raises the shared flag.
        let die_flag = context.shared::<super::die::DieFlag>().ok().cloned();
        let shell = context.shell;

        let get = |var: &str| {
            shell
                .env_str(var)
                .map(|s| s.into_owned())
                .unwrap_or_default()
        };
        let distdir = {
            let s = get("DISTDIR");
            if s.is_empty() {
                "/var/cache/distfiles".to_string()
            } else {
                s
            }
        };
        let ro_distdirs: Vec<String> = get("PORTAGE_RO_DISTDIRS")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let eapi: u32 = get("EAPI").parse().unwrap_or(0);
        let cwd = shell.working_dir().to_path_buf();
        let archives = self.archives.clone();

        let exit = tokio::task::spawn_blocking(move || -> u8 {
            for archive in &archives {
                let src_path = match resolve_src_path(archive, &cwd, &distdir, &ro_distdirs, eapi) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!("die: unpack: {e}");
                        return 1;
                    }
                };

                if !src_path.exists() {
                    tracing::error!("die: unpack: {} not found", src_path.display());
                    return 1;
                }

                tracing::info!(">>> Unpacking {} to {}", archive, cwd.display());

                match unpack_archive(&src_path, &cwd, eapi) {
                    Ok(0) => {}
                    Ok(code) => {
                        tracing::error!("die: unpack: {} failed with exit code {}", archive, code);
                        return 1;
                    }
                    Err(e) => {
                        tracing::error!("die: unpack: {e}");
                        return 1;
                    }
                }
            }
            0
        })
        .await
        .unwrap_or(1);

        if exit != 0
            && let Some(flag) = &die_flag
        {
            flag.raise("unpack failed");
        }

        Ok(brush_core::ExecutionResult::new(exit))
    }
}

/// Resolve an `unpack` argument to the on-disk distfile it names, per PMS
/// 12.3.11: a bare filename (no path separator) is looked up in `$DISTDIR`
/// (or its read-only fallbacks); anything containing a path separator is
/// used as given — absolute as-is, `./`-relative resolved against `cwd`
/// (the file the ebuild itself just created there, e.g. a `cp foo.whl
/// foo.whl.zip` before re-unpacking it as a zip).
fn resolve_src_path(
    archive: &str,
    cwd: &std::path::Path,
    distdir: &str,
    ro_distdirs: &[String],
    eapi: u32,
) -> Result<std::path::PathBuf, String> {
    if archive.starts_with('/') {
        if eapi < 6 {
            return Err(format!("absolute paths not supported in EAPI {eapi}"));
        }
        Ok(std::path::PathBuf::from(archive))
    } else if archive.starts_with("./") {
        // A `./`-relative archive refers to a file the ebuild itself just
        // created in the work directory (e.g. `dev-python/installer`'s
        // `cp foo.whl foo.whl.zip && unpack "./foo.whl.zip"`, to route a
        // wheel through the generic zip unpacker — the same pattern
        // `eclass/rpm.eclass` independently uses via `unpack "./${a}"`).
        // Must resolve against the shell's *tracked* working directory
        // (`cwd`), not whatever the Rust process's own OS-level CWD happens
        // to be — those can diverge, since brush tracks `$PWD` independently
        // of calling `std::env::set_current_dir`. A bare relative-path
        // existence check silently looked in the wrong place and always
        // reported "not found".
        Ok(cwd.join(archive))
    } else {
        // The writable DISTDIR first, then the read-only fallbacks
        // (PORTAGE_RO_DISTDIRS — e.g. the system distfiles dir when
        // running unprivileged).
        Ok(std::iter::once(&distdir.to_string())
            .chain(ro_distdirs.iter())
            .map(|d| std::path::PathBuf::from(d).join(archive))
            .find(|p| p.exists())
            .unwrap_or_else(|| std::path::PathBuf::from(distdir).join(archive)))
    }
}

/// Dispatch extraction of `src` into `cwd` based on file extension.
fn unpack_archive(src: &std::path::Path, cwd: &std::path::Path, eapi: u32) -> Result<u8, String> {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let case_sensitive = eapi >= 6;
    let ext = |suffix: &str| -> bool {
        if case_sensitive {
            name.ends_with(suffix)
        } else {
            name.to_lowercase().ends_with(&suffix.to_lowercase())
        }
    };

    let src_s = src.to_string_lossy().into_owned();
    let stem = src.file_stem().unwrap_or_default();

    // Check .tar.* compound extensions before single-extension forms.
    // `--no-same-owner` must come *after* the old-style combined mode
    // letters (`xzf`, `xjf`, …): GNU tar only recognizes a bare, undashed
    // option cluster as its *first* argument — once a long option precedes
    // it, tar stops treating it as old-style short options at all and reads
    // it as a plain (non-option) filename, dying with "You must specify one
    // of the '-Acdtrux' ... options" (found live testing this exact fix).
    if ext(".tar.gz") || ext(".tgz") || ext(".tar.Z") {
        unpack_cmd("tar", &["xzf", &src_s, "--no-same-owner"], cwd)
    } else if ext(".tar.bz2") || ext(".tbz2") || ext(".tbz") {
        unpack_cmd("tar", &["xjf", &src_s, "--no-same-owner"], cwd)
    } else if ext(".tar.xz") || ext(".txz") {
        if eapi < 3 {
            return Err(format!(".tar.xz not supported in EAPI {eapi}"));
        }
        unpack_cmd("tar", &["xJf", &src_s, "--no-same-owner"], cwd)
    } else if ext(".tar.zst") {
        unpack_cmd("tar", &["--zstd", "-xf", &src_s, "--no-same-owner"], cwd)
    } else if ext(".tar.lz") {
        unpack_cmd("tar", &["--lzip", "-xf", &src_s, "--no-same-owner"], cwd)
    } else if ext(".tar.lzma") {
        unpack_cmd("tar", &["--lzma", "-xf", &src_s, "--no-same-owner"], cwd)
    } else if ext(".zip") {
        unpack_cmd("unzip", &["-qo", &src_s], cwd)
    } else if ext(".gz") || ext(".Z") {
        unpack_piped("gzip", &["-d", "-c", &src_s], &cwd.join(stem), cwd)
    } else if ext(".bz2") {
        unpack_piped("bzip2", &["-d", "-c", &src_s], &cwd.join(stem), cwd)
    } else if ext(".xz") {
        if eapi < 3 {
            return Err(format!(".xz not supported in EAPI {eapi}"));
        }
        unpack_piped("xz", &["-d", "-c", &src_s], &cwd.join(stem), cwd)
    } else if ext(".lzma") {
        unpack_piped(
            "xz",
            &["--format=lzma", "-d", "-c", &src_s],
            &cwd.join(stem),
            cwd,
        )
    } else if ext(".zst") {
        unpack_piped("zstd", &["-d", "-c", &src_s], &cwd.join(stem), cwd)
    } else if ext(".7z") {
        if eapi > 7 {
            return Err(format!(".7z not supported in EAPI {eapi}"));
        }
        unpack_cmd("7z", &["x", &src_s], cwd)
    } else if ext(".rar") {
        if eapi > 7 {
            return Err(format!(".rar not supported in EAPI {eapi}"));
        }
        unpack_cmd("unrar", &["x", &src_s], cwd)
    } else if ext(".lha") || ext(".lzh") {
        if eapi > 7 {
            return Err(format!(".lha/.lzh not supported in EAPI {eapi}"));
        }
        unpack_cmd("lha", &["xfq", &src_s], cwd)
    } else {
        // PMS 12.3.11 only lists behavior for recognized suffixes; real
        // Portage's `unpack` helper leaves a file with an unrecognized
        // suffix untouched rather than failing the phase. Ebuilds routinely
        // list non-archive files in SRC_URI (patches, man pages, data files)
        // alongside real archives and rely on `default` src_unpack (which
        // calls `unpack ${A}` unconditionally on every distfile) not dying
        // on them — e.g. dev-build/meson's `meson-reference.3` man page,
        // installed straight from `${DISTDIR}` in src_install, never needs
        // to be extracted at all.
        Ok(0)
    }
}

fn unpack_cmd(prog: &str, args: &[&str], cwd: &std::path::Path) -> Result<u8, String> {
    std::process::Command::new(prog)
        .args(args)
        .current_dir(cwd)
        .status()
        .map(|s| s.code().unwrap_or(1) as u8)
        .map_err(|e| format!("failed to run {prog}: {e}"))
}

fn unpack_piped(
    prog: &str,
    args: &[&str],
    out: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<u8, String> {
    let f = std::fs::File::create(out)
        .map_err(|e| format!("failed to create {}: {e}", out.display()))?;
    std::process::Command::new(prog)
        .args(args)
        .current_dir(cwd)
        .stdout(f)
        .status()
        .map(|s| s.code().unwrap_or(1) as u8)
        .map_err(|e| format!("failed to run {prog}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the riscv64 stage3 shakeout: `dev-build/meson`'s
    /// real ebuild lists `meson-reference.3` (a bare man page, fetched via
    /// SRC_URI's `->` rename) alongside its real `.tar.gz` source, and its
    /// `src_unpack` is just `default` — which calls `unpack ${A}`
    /// unconditionally on *every* distfile, including the man page. Real
    /// Portage's `unpack` leaves a file with an unrecognized suffix
    /// untouched rather than failing the phase; `em` was `die`-ing instead,
    /// breaking every ebuild with a non-archive SRC_URI entry.
    #[test]
    fn unrecognized_extension_is_a_no_op_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let result = unpack_archive(
            std::path::Path::new("meson-reference-1.11.1.3"),
            tmp.path(),
            8,
        );
        assert_eq!(result, Ok(0));
    }

    #[test]
    fn recognized_extension_still_dispatches() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("missing.tar.gz");
        // A genuinely recognized suffix still goes through the real
        // extractor (and fails here only because the file doesn't exist —
        // proving the no-op path above is specific to unknown suffixes).
        let result = unpack_archive(&archive, tmp.path(), 8);
        assert!(result.is_err() || result != Ok(0));
    }

    /// Regression test for the riscv64 stage3 shakeout: `dev-python/installer`'s
    /// `src_unpack` does `cp foo.whl foo.whl.zip && unpack "./foo.whl.zip"`
    /// (a real, PMS-legitimate pattern — `eclass/rpm.eclass` independently
    /// does the same `unpack "./${a}"` thing) to route a wheel through the
    /// generic zip unpacker. A `./`-relative archive must resolve against
    /// the shell's tracked working directory, not DISTDIR and not whatever
    /// the Rust process's own OS-level CWD happens to be at the time —
    /// those can diverge from brush's `$PWD` tracking. The bare
    /// `PathBuf::from(archive)` this replaces always reported "not found"
    /// for a file that demonstrably existed in `cwd`.
    #[test]
    fn relative_archive_resolves_against_tracked_cwd_not_distdir() {
        let cwd = tempfile::tempdir().unwrap();
        let distdir = tempfile::tempdir().unwrap();
        std::fs::write(cwd.path().join("foo.whl.zip"), b"stub").unwrap();

        let resolved = resolve_src_path(
            "./foo.whl.zip",
            cwd.path(),
            distdir.path().to_str().unwrap(),
            &[],
            8,
        )
        .unwrap();

        assert_eq!(resolved, cwd.path().join("foo.whl.zip"));
        assert!(resolved.exists());
    }

    #[test]
    fn bare_filename_still_resolves_via_distdir() {
        let cwd = tempfile::tempdir().unwrap();
        let distdir = tempfile::tempdir().unwrap();
        std::fs::write(distdir.path().join("real.tar.gz"), b"stub").unwrap();

        let resolved = resolve_src_path(
            "real.tar.gz",
            cwd.path(),
            distdir.path().to_str().unwrap(),
            &[],
            8,
        )
        .unwrap();

        assert_eq!(resolved, distdir.path().join("real.tar.gz"));
    }
}
