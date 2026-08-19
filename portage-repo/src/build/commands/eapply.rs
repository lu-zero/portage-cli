use std::io::Write as _;

use brush_core::builtins;
use clap::Parser;

use super::die::DieFlag;

// ── P6 eapply ────────────────────────────────────────────────────────────────

/// `eapply [options] [--] patch-or-dir...`  (PMS 11.3.3, EAPI ≥ 6)
///
/// Applies one or more patches (or every `*.diff`/`*.patch` in a directory,
/// non-recursively, in name order) via `patch -p1 -f -g0
/// --no-backup-if-mismatch`, plus any extra `options`.
///
/// Option/operand split: a literal `--` splits the argument list positionally
/// (left = options, right = operands, no further interpretation); without one,
/// the first non-`-`-prefixed argument starts the operands and any `-`-prefixed
/// argument after that is an error ("options must precede non-option arguments").
#[derive(Parser)]
pub(crate) struct EapplyCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for EapplyCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let die_flag = context.shared::<DieFlag>().ok().cloned();
        let env_vars = super::context_env(&context);
        let shell = context.shell;
        let cwd = shell.working_dir().to_path_buf();

        let die = |msg: String| {
            if let Some(flag) = &die_flag {
                flag.raise(&msg);
            }
            msg
        };

        let (options, operands) = match split_options_operands(&self.args) {
            Ok(v) => v,
            Err(msg) => {
                let msg = die(msg);
                let _ = writeln!(context.params.stderr(shell), "die: {msg}");
                return Ok(brush_core::ExecutionResult::new(1));
            }
        };
        if operands.is_empty() {
            let msg = die("eapply: no operands were specified".to_string());
            let _ = writeln!(context.params.stderr(shell), "die: {msg}");
            return Ok(brush_core::ExecutionResult::new(1));
        }

        // Patch's blocking I/O runs off-thread and returns a sequence of
        // events in application order; the "Applying patches from DIR..."
        // header goes through the real `einfo` builtin (exact ANSI styling,
        // not hand-rolled) on the async side below, and patch output is
        // written straight to stdout — neither can happen from inside
        // `spawn_blocking`, which has no access to the shell/context.
        let (events, result) = tokio::task::spawn_blocking(move || {
            let mut events = Vec::new();
            let result = apply_all(&operands, &options, &cwd, &env_vars, &mut events);
            (events, result)
        })
        .await
        .unwrap_or_else(|e| (Vec::new(), Err(format!("eapply: task panicked: {e}"))));

        let source_info = brush_core::SourceInfo::from("eapply");
        let params = shell.default_exec_params();
        for event in events {
            match event {
                Event::ApplyingFrom(dir) => {
                    let script = format!(
                        "einfo {}",
                        shell_quote(&format!("Applying patches from {dir} ..."))
                    );
                    let _ = shell.run_string(&script, &source_info, &params).await;
                }
                Event::PatchOutput(text) => {
                    let _ = write!(context.params.stdout(shell), "{text}");
                }
            }
        }

        if let Err(msg) = result {
            let msg = die(msg);
            let _ = writeln!(context.params.stderr(shell), "die: {msg}");
            return Ok(brush_core::ExecutionResult::new(1));
        }

        Ok(brush_core::ExecutionResult::new(0))
    }
}

/// Single-quote `s` for a literal bash argument (matches the pattern already
/// established at the call sites that build small `shell.run_string`
/// snippets elsewhere in this module set).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// PMS 11.3.3's option/operand split — see the type doc comment
fn split_options_operands(args: &[String]) -> Result<(Vec<String>, Vec<String>), String> {
    if let Some(sep) = args.iter().position(|a| a == "--") {
        Ok((args[..sep].to_vec(), args[sep + 1..].to_vec()))
    } else {
        let (mut options, mut operands) = (Vec::new(), Vec::new());
        for a in args {
            if a.starts_with('-') {
                if !operands.is_empty() {
                    return Err("eapply: options must precede non-option arguments".to_string());
                }
                options.push(a.clone());
            } else {
                operands.push(a.clone());
            }
        }
        Ok((options, operands))
    }
}

/// A user-visible thing that happened while applying patches, in order —
/// kept separate from the actual writes so the blocking task never needs
/// shell/context access (see `execute`'s doc comment).
enum Event {
    /// A directory operand had matching patches — needs a real `einfo` call
    ApplyingFrom(String),
    /// Raw `patch` output (a fuzz warning on success, or the failure text)
    PatchOutput(String),
}

fn apply_all(
    operands: &[String],
    options: &[String],
    cwd: &std::path::Path,
    env_vars: &[(String, String)],
    events: &mut Vec<Event>,
) -> Result<(), String> {
    for operand in operands {
        let path = cwd.join(operand);
        if path.is_dir() {
            let mut patches: Vec<std::path::PathBuf> = std::fs::read_dir(&path)
                .map_err(|e| format!("eapply: {operand}: {e}"))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    let s = p.to_string_lossy();
                    s.ends_with(".diff") || s.ends_with(".patch")
                })
                .collect();
            patches.sort();

            if patches.is_empty() {
                return Err(format!("No *.{{patch,diff}} files in directory {operand}"));
            }
            events.push(Event::ApplyingFrom(operand.clone()));
            for patch in &patches {
                // Display label matches bash's `$f` (`"${path}"/*`, relative
                // to cwd, never absolutized) — not the absolute path used
                // for the actual file I/O below.
                let name = patch.file_name().unwrap_or_default().to_string_lossy();
                let label = format!("{operand}/{name}");
                apply_one(patch, &label, options, env_vars, cwd, events)?;
            }
        } else {
            apply_one(&path, operand, options, env_vars, cwd, events)?;
        }
    }
    Ok(())
}

/// Whether `output` contains patch's "N with fuzz N" fuzz-warning marker
/// anywhere (PMS: surfaced even on an otherwise-successful apply)
///
/// Matches bash's `*[0-9]" with fuzz "[0-9]*` glob: a digit, the literal
/// text, a digit.
fn has_fuzz_marker(output: &str) -> bool {
    let marker = " with fuzz ";
    let bytes = output.as_bytes();
    output.match_indices(marker).any(|(i, _)| {
        i > 0
            && bytes[i - 1].is_ascii_digit()
            && bytes.get(i + marker.len()).is_some_and(u8::is_ascii_digit)
    })
}

fn apply_one(
    patch_path: &std::path::Path,
    label: &str,
    options: &[String],
    env_vars: &[(String, String)],
    cwd: &std::path::Path,
    events: &mut Vec<Event>,
) -> Result<(), String> {
    let stdin = std::fs::File::open(patch_path)
        .map_err(|e| format!("eapply: invalid patch: {label}: {e}"))?;

    let output = std::process::Command::new("patch")
        .current_dir(cwd)
        .args(["-p1", "-f", "-g0", "--no-backup-if-mismatch"])
        .args(options)
        .stdin(stdin)
        .envs(env_vars.iter().cloned())
        // Force C-locale diagnostics so the fuzz-marker check below is
        // reliable regardless of the invoking user's own locale.
        .env("LC_ALL", "")
        .env("LC_MESSAGES", "C")
        .output()
        .map_err(|e| format!("eapply: patch failed to run: {e}"))?;

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    if output.status.success() {
        if has_fuzz_marker(&combined) {
            events.push(Event::PatchOutput(combined));
        }
        Ok(())
    } else {
        events.push(Event::PatchOutput(combined));
        Err(format!("eapply: patch failed: {label}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch_outputs(events: &[Event]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::PatchOutput(s) => Some(s.as_str()),
                Event::ApplyingFrom(_) => None,
            })
            .collect()
    }

    fn headers(events: &[Event]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::ApplyingFrom(s) => Some(s.as_str()),
                Event::PatchOutput(_) => None,
            })
            .collect()
    }

    #[test]
    fn split_double_dash_is_positional_only() {
        let args: Vec<String> = ["-p1", "--", "file.patch"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let (options, operands) = split_options_operands(&args).unwrap();
        assert_eq!(options, vec!["-p1"]);
        assert_eq!(operands, vec!["file.patch"]);
    }

    #[test]
    fn split_no_dash_dash_requires_options_first() {
        let args: Vec<String> = ["-p1", "file.patch"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let (options, operands) = split_options_operands(&args).unwrap();
        assert_eq!(options, vec!["-p1"]);
        assert_eq!(operands, vec!["file.patch"]);
    }

    #[test]
    fn split_option_after_operand_without_dash_dash_errors() {
        let args: Vec<String> = ["file.patch", "-p1"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let err = split_options_operands(&args).unwrap_err();
        assert_eq!(err, "eapply: options must precede non-option arguments");
    }

    #[test]
    fn split_trailing_dash_dash_yields_no_operands() {
        let args: Vec<String> = ["file.patch", "--"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let (options, operands) = split_options_operands(&args).unwrap();
        assert_eq!(options, vec!["file.patch"]);
        assert!(operands.is_empty());
    }

    #[test]
    fn fuzz_marker_detected() {
        assert!(has_fuzz_marker(
            "Hunk #1 succeeded at 12 with fuzz 2 (offset 1 line)."
        ));
        assert!(!has_fuzz_marker("Hunk #1 succeeded at 12."));
        assert!(!has_fuzz_marker("a with fuzz b"));
    }

    #[test]
    fn apply_single_file_patch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "0\n").unwrap();
        std::fs::write(
            dir.path().join("file.patch"),
            "--- a/file.txt\n+++ a/file.txt\n@@ -1 +1 @@\n-0\n+1\n",
        )
        .unwrap();

        let mut events = Vec::new();
        apply_all(
            &["file.patch".to_string()],
            &[],
            dir.path(),
            &[],
            &mut events,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "1\n"
        );
        // A clean single-file apply produces no output and no header.
        assert!(patch_outputs(&events).is_empty());
        assert!(headers(&events).is_empty());
    }

    #[test]
    fn apply_directory_in_sorted_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "0\n").unwrap();
        let patches = dir.path().join("patches");
        std::fs::create_dir(&patches).unwrap();
        std::fs::write(
            patches.join("0.patch"),
            "--- a/file.txt\n+++ a/file.txt\n@@ -1 +1 @@\n-0\n+1\n",
        )
        .unwrap();
        std::fs::write(
            patches.join("1.patch"),
            "--- a/file.txt\n+++ a/file.txt\n@@ -1 +1 @@\n-1\n+2\n",
        )
        .unwrap();
        // Non-matching file must be ignored, not picked up as a patch.
        std::fs::write(patches.join("readme.txt"), "not a patch").unwrap();

        let mut events = Vec::new();
        apply_all(&["patches".to_string()], &[], dir.path(), &[], &mut events).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "2\n"
        );
        assert_eq!(headers(&events), vec!["patches"]);
    }

    #[test]
    fn empty_directory_dies() {
        let dir = tempfile::tempdir().unwrap();
        let patches = dir.path().join("patches");
        std::fs::create_dir(&patches).unwrap();

        let mut events = Vec::new();
        let err =
            apply_all(&["patches".to_string()], &[], dir.path(), &[], &mut events).unwrap_err();
        assert_eq!(err, "No *.{patch,diff} files in directory patches");
    }

    #[test]
    fn failed_patch_dies_with_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "9\n").unwrap();
        std::fs::write(
            dir.path().join("file.patch"),
            "--- a/file.txt\n+++ a/file.txt\n@@ -1 +1 @@\n-0\n+1\n",
        )
        .unwrap();

        let mut events = Vec::new();
        let err = apply_all(
            &["file.patch".to_string()],
            &[],
            dir.path(),
            &[],
            &mut events,
        )
        .unwrap_err();
        assert_eq!(err, "eapply: patch failed: file.patch");
        assert!(!patch_outputs(&events).is_empty());
    }
}
