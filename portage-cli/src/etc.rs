//! `em etc` — reconcile the `._cfgNNNN_` files a protected merge leaves behind
//!
//! One command for the job real Gentoo splits between `etc-update` and
//! `dispatch-conf`: they differ in UX, not in what they do. `dispatch-conf`'s
//! auto-merge of files that differ only in comments or whitespace is a flag
//! here (`--auto`), not a second front-end.
//!
//! The reason this cannot just be the host's `etc-update`: `em` writes those
//! sidecars under whatever root it merged into, and a host tool only ever
//! looks at `/`. Under `--root`/`--prefix`/`--local` there is otherwise no way
//! to review them at all.
//!
//! `diff`/`sdiff` are shelled out to rather than reimplemented, which is what
//! the real tools do — it also means a user's own `diff` options and merge
//! habits keep working.

use std::io::Write;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use crate::cli::{Cli, EtcCommand, EtcOpts};

/// One pending sidecar and how it relates to its target
struct Pending {
    /// The `._cfgNNNN_<name>` file
    sidecar: Utf8PathBuf,
    /// The file it would replace
    target: Utf8PathBuf,
    /// `NNNN`, so the oldest is offered first
    index: u32,
    kind: Kind,
    bytes: u64,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Kind {
    /// The target does not exist — nothing to reconcile, the sidecar *is* the file
    New,
    /// Byte-identical to the target: safe to drop with no question asked
    Identical,
    /// Differs only in comments and blank lines — `dispatch-conf`'s auto case
    Trivial,
    /// A real content change needing a decision
    Differs,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::New => "new file",
            Self::Identical => "identical",
            Self::Trivial => "comments/whitespace only",
            Self::Differs => "modified",
        }
    }
}

pub async fn run(globals: &Cli, command: Option<&EtcCommand>, opts: &EtcOpts) -> Result<()> {
    let roots = globals.roots();
    let protect = crate::ebuild::ConfigProtect::from_roots(&roots).await;
    let mut pending = scan(roots.merge_root(), &protect);
    // Oldest first: accepting the newest sidecar first would silently discard
    // the intermediate versions behind it.
    pending.sort_by(|a, b| (&a.target, a.index).cmp(&(&b.target, b.index)));

    match command {
        None => list(&pending, opts, globals),
        Some(EtcCommand::Diff { path }) => diff(&pending, path.as_deref(), globals),
        Some(EtcCommand::Merge) => merge_interactive(&pending, globals),
    }
}

/// Every `._cfgNNNN_<name>` under a protected directory of `merge_root`
fn scan(merge_root: &Utf8Path, protect: &crate::ebuild::ConfigProtect) -> Vec<Pending> {
    let mut out = Vec::new();
    for dir in protect.protected_dirs() {
        // A protected path is absolute in config; resolve it inside the root
        // being reconciled, which is the whole point of doing this in `em`.
        let base = merge_root.join(dir.trim_start_matches('/'));
        walk(&base, merge_root, protect, &mut out);
    }
    out
}

fn walk(
    dir: &Utf8Path,
    merge_root: &Utf8Path,
    protect: &crate::ebuild::ConfigProtect,
    out: &mut Vec<Pending>,
) {
    let Ok(entries) = std::fs::read_dir(dir.as_std_path()) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            walk(&path, merge_root, protect, out);
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        let Some((index, real_name)) = parse_cfg_name(name) else {
            continue;
        };
        let target = path.with_file_name(real_name);
        // `CONFIG_PROTECT_MASK` is expressed against the real root, so test the
        // target with the leading merge root stripped back off.
        let rel = target
            .strip_prefix(merge_root)
            .map(|r| Utf8PathBuf::from("/").join(r))
            .unwrap_or_else(|_| target.clone());
        if !protect.is_protected(&rel) {
            continue;
        }
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(Pending {
            kind: classify(&path, &target),
            sidecar: path,
            target,
            index,
            bytes,
        });
    }
}

/// `._cfg0007_make.conf` → `(7, "make.conf")`
///
/// Mirrors the shape `ebuild::scan_cfg` writes: exactly four digits, then `_`,
/// then the real name.
fn parse_cfg_name(name: &str) -> Option<(u32, &str)> {
    let rest = name.strip_prefix("._cfg")?;
    if rest.len() <= 5 || rest.as_bytes()[4] != b'_' {
        return None;
    }
    let index = rest[..4].parse::<u32>().ok()?;
    Some((index, &rest[5..]))
}

fn classify(sidecar: &Utf8Path, target: &Utf8Path) -> Kind {
    let Ok(new) = std::fs::read(sidecar.as_std_path()) else {
        return Kind::Differs;
    };
    let Ok(old) = std::fs::read(target.as_std_path()) else {
        return Kind::New;
    };
    if new == old {
        return Kind::Identical;
    }
    if significant_lines(&new) == significant_lines(&old) {
        return Kind::Trivial;
    }
    Kind::Differs
}

/// The lines that carry meaning, for the "comments/whitespace only" test
///
/// Follows real `dispatch-conf`'s `replace-wscomments` rule
/// (`bin/dispatch-conf`): a differing line counts as insignificant only when
/// it is *entirely* blank or a **whole-line** comment — `^\s*#`. Whitespace
/// *amount* within a kept line is normalised, which is what portage's own
/// `diff -Bbua` does with `-b`.
///
/// The earlier version truncated every line at its first `#`, which silently
/// classified `psk="secret#1"` → `psk="secret#2"` as a comment change and let
/// `--auto` overwrite it. Truncation is the bug; whole-line filtering is not.
///
/// Note this shares one property with portage: `#!/bin/sh` and sudoers'
/// `#includedir` *are* whole-line comments, so a change confined to them
/// counts as insignificant here too. That is why `--auto` is opt-in — as
/// `replace-wscomments=no` is portage's shipped default.
fn significant_lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.is_empty() && !t.starts_with('#')
        })
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

fn list(pending: &[Pending], opts: &EtcOpts, globals: &Cli) -> Result<()> {
    if pending.is_empty() {
        println!(">>> No pending configuration files.");
        return Ok(());
    }

    // `--use-new`/`--use-old`/`--auto` are batch resolutions, not listings.
    if opts.use_new || opts.use_old || opts.auto {
        return resolve_batch(pending, opts, globals);
    }

    let mut current = Utf8PathBuf::new();
    for p in pending {
        if p.target != current {
            println!("{}", p.target);
            current = p.target.clone();
        }
        println!(
            "    {}  ({}, {})",
            p.sidecar.file_name().unwrap_or("?"),
            p.kind.label(),
            crate::clean::human_bytes(p.bytes)
        );
    }
    let droppable = pending
        .iter()
        .filter(|p| p.kind == Kind::Identical || p.kind == Kind::Trivial)
        .count();
    println!(
        "\n>>> {} pending file(s) across {} target(s).",
        pending.len(),
        pending
            .iter()
            .map(|p| &p.target)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );
    if droppable > 0 {
        println!(">>> {droppable} need no decision — `em etc --auto` clears them.");
    }
    println!(">>> `em etc diff` to inspect, `em etc merge` to resolve interactively.");
    Ok(())
}

/// Apply one resolution to every pending file `opts` selects
fn resolve_batch(pending: &[Pending], opts: &EtcOpts, globals: &Cli) -> Result<()> {
    let selected: Vec<&Pending> = pending
        .iter()
        .filter(|p| {
            if opts.use_new || opts.use_old {
                true
            } else {
                // `--auto` alone: only what needs no human decision.
                p.kind == Kind::Identical || p.kind == Kind::Trivial
            }
        })
        .collect();
    if selected.is_empty() {
        println!(">>> Nothing to do.");
        return Ok(());
    }
    let mut done = 0usize;

    // `--auto` keeps the *new* file for a trivial difference (it is the
    // package's current comments) and simply drops an identical one.
    let keep_new = opts.use_new || opts.auto;
    for p in &selected {
        let verb = if opts.use_old || p.kind == Kind::Identical {
            "discard"
        } else if keep_new {
            "install"
        } else {
            "discard"
        };
        println!("    {verb}: {}", p.sidecar);
        if globals.pretend {
            continue;
        }
        // Best-effort per file, like `em clean`'s sweep: one sidecar whose
        // target cannot be replaced must not cost the user every other
        // resolution in the batch.
        let outcome = if verb == "install" {
            install(p)
        } else {
            std::fs::remove_file(p.sidecar.as_std_path())
                .with_context(|| format!("removing {}", p.sidecar))
        };
        match outcome {
            Ok(()) => done += 1,
            Err(e) => crate::style::warn_line!("{e:#}"),
        }
    }
    if globals.pretend {
        println!(">>> Would resolve {} file(s).", selected.len());
    } else {
        println!(">>> Resolved {done} of {} file(s).", selected.len());
    }
    Ok(())
}

/// Move `sidecar` over `target`
///
/// A plain rename, which keeps the *sidecar's* mode and ownership — and the
/// sidecar carries the package's own, because `walk_image` applies the image's
/// permissions when it writes the divert. That is what real `dispatch-conf`
/// does for use-new (`os.rename(newconf, curconf)`, no `chmod`): the package's
/// intent for its config file wins over whatever the live copy had.
fn install(p: &Pending) -> Result<()> {
    // A `._cfg0000_..`-shaped name resolves its target to a directory, which
    // `rename` refuses with EBUSY/ENOTEMPTY anyway — reject it up front so the
    // message names the real problem.
    if p.target.is_dir() {
        anyhow::bail!("{} names a directory, not a config file", p.target);
    }
    std::fs::rename(p.sidecar.as_std_path(), p.target.as_std_path())
        .with_context(|| format!("installing {} over {}", p.sidecar, p.target))
}

fn diff(pending: &[Pending], only: Option<&str>, globals: &Cli) -> Result<()> {
    let selected: Vec<&Pending> = pending
        .iter()
        .filter(|p| only.is_none_or(|f| p.target.as_str().contains(f)))
        .collect();
    if selected.is_empty() {
        println!(">>> No pending configuration files match.");
        return Ok(());
    }
    for p in selected {
        println!("--- {} ({})", p.target, p.kind.label());
        if p.kind == Kind::New {
            println!("    (target does not exist yet)");
            continue;
        }
        run_diff(&p.target, &p.sidecar, globals)?;
    }
    Ok(())
}

/// `diff -u <target> <sidecar>`
///
/// Shelled out rather than reimplemented, as the real tools do: a user's
/// `diff` already honours their own options, and there is no value in a
/// second, worse renderer.
fn run_diff(target: &Utf8Path, sidecar: &Utf8Path, globals: &Cli) -> Result<()> {
    let mut cmd = std::process::Command::new("diff");
    cmd.arg("-u");
    if crate::diag::stderr_wants_color() && !globals.quiet {
        cmd.arg("--color=always");
    }
    cmd.arg(target.as_std_path()).arg(sidecar.as_std_path());
    // diff exits 1 for "files differ", which is the expected case here.
    match cmd.status() {
        Ok(_) => Ok(()),
        Err(e) => {
            crate::style::warn_line!("cannot run diff: {e}");
            Ok(())
        }
    }
}

fn merge_interactive(pending: &[Pending], globals: &Cli) -> Result<()> {
    if pending.is_empty() {
        println!(">>> No pending configuration files.");
        return Ok(());
    }
    // `-p` is "show what would be done without performing any actions".
    // The prompt itself is the action; honouring it by mutating is the bug.
    if globals.pretend {
        for p in pending {
            println!("    would resolve: {} ({})", p.target, p.kind.label());
        }
        println!(">>> Would resolve {} file(s).", pending.len());
        return Ok(());
    }
    require_tty()?;

    let mut resolved = 0usize;
    for p in pending {
        // Re-check: an earlier iteration may have installed a newer sidecar
        // over this target, or the user may have resolved it in another shell.
        if !p.sidecar.exists() {
            continue;
        }
        println!("\n=== {} ({})", p.target, p.kind.label());
        loop {
            print!("[n]ew  [o]ld  [d]iff  [e]dit  [m]erge  [s]kip  [q]uit ? ");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line)? == 0 {
                println!();
                return done(resolved);
            }
            match line.trim() {
                "n" | "new" => {
                    install(p)?;
                    resolved += 1;
                    break;
                }
                "o" | "old" => {
                    std::fs::remove_file(p.sidecar.as_std_path())
                        .with_context(|| format!("removing {}", p.sidecar))?;
                    resolved += 1;
                    break;
                }
                "d" | "diff" => run_diff(&p.target, &p.sidecar, globals)?,
                "e" | "edit" => spawn_editor(&p.sidecar)?,
                "m" | "merge" => {
                    if sdiff_merge(p)? {
                        resolved += 1;
                        break;
                    }
                }
                "s" | "skip" | "" => break,
                "q" | "quit" => return done(resolved),
                other => println!("    unrecognised: {other:?}"),
            }
        }
    }
    done(resolved)
}

/// `merge` prompts per file, so refuse up front outside a terminal rather
/// than reading EOF as an answer for every one of them.
///
/// Deliberately not `merge::require_ask_tty`: that carries real portage's
/// exact `--ask` wording, and `em etc merge` has no `--ask` to mention.
fn require_tty() -> Result<()> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        Ok(())
    } else {
        anyhow::bail!(
            "em etc merge is interactive and needs a terminal — \
             use --auto, --use-new or --use-old for a batch resolution"
        )
    }
}

fn done(resolved: usize) -> Result<()> {
    println!("\n>>> Resolved {resolved} file(s).");
    Ok(())
}

/// `$EDITOR` on the sidecar, so a user can hand-edit before accepting it
fn spawn_editor(path: &Utf8Path) -> Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".to_string());
    let status = std::process::Command::new(&editor)
        .arg(path.as_std_path())
        .status()
        .with_context(|| format!("running {editor}"))?;
    if !status.success() {
        crate::style::warn_line!("{editor} exited {status}");
    }
    Ok(())
}

/// Interactive side-by-side merge into the target, `sdiff --output`
///
/// **`sdiff` exits 1 whenever the two inputs differed**, which is every real
/// merge — only byte-identical inputs give 0. Treating non-zero as failure
/// therefore discarded every completed merge. Real `dispatch-conf` maps
/// `status < 2` to success (`bin/dispatch-conf`), and 2-or-more to "Failure
/// running 'merge' command"; this follows it.
///
/// Returns whether the merge completed — on failure the sidecar is left in
/// place so the file can be revisited.
fn sdiff_merge(p: &Pending) -> Result<bool> {
    let tmp = p.target.with_file_name(format!(
        ".em-merge-{}",
        p.target.file_name().unwrap_or("cfg")
    ));
    let status = std::process::Command::new("sdiff")
        .arg("--output")
        .arg(tmp.as_std_path())
        .arg(p.target.as_std_path())
        .arg(p.sidecar.as_std_path())
        .status();
    let code = match status {
        Ok(s) => s.code().unwrap_or(2),
        Err(e) => {
            let _ = std::fs::remove_file(tmp.as_std_path());
            crate::style::warn_line!("cannot run sdiff: {e}");
            return Ok(false);
        }
    };
    if code >= 2 {
        let _ = std::fs::remove_file(tmp.as_std_path());
        crate::style::warn_line!("sdiff failed ({code}); {} left in place", p.sidecar);
        return Ok(false);
    }

    // `sdiff` created `tmp` with the invoking umask, so carry the sidecar's
    // mode *and* ownership onto it before it becomes the live file — the same
    // `chmod`/`chown` from `lstat(conf["new"])` dispatch-conf applies to its
    // own merge result. Without this a `0600` root-owned config comes back
    // world-readable.
    if let Ok(meta) = std::fs::metadata(p.sidecar.as_std_path()) {
        use std::os::unix::fs::MetadataExt;
        let _ = std::fs::set_permissions(tmp.as_std_path(), meta.permissions());
        let _ = std::os::unix::fs::chown(tmp.as_std_path(), Some(meta.uid()), Some(meta.gid()));
    }
    std::fs::rename(tmp.as_std_path(), p.target.as_std_path())
        .with_context(|| format!("installing the merge result over {}", p.target))?;
    let _ = std::fs::remove_file(p.sidecar.as_std_path());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfg_names_need_exactly_four_digits_and_an_underscore() {
        assert_eq!(
            parse_cfg_name("._cfg0000_make.conf"),
            Some((0, "make.conf"))
        );
        assert_eq!(parse_cfg_name("._cfg0042_hosts"), Some((42, "hosts")));
        // Not the shape `ebuild::scan_cfg` writes.
        assert_eq!(parse_cfg_name("._cfg42_hosts"), None);
        assert_eq!(parse_cfg_name("._cfg0000_"), None);
        assert_eq!(parse_cfg_name("make.conf"), None);
        assert_eq!(parse_cfg_name(".cfg0000_hosts"), None);
    }

    // The distinction `--auto` rests on. A whole-line comment or a blank line
    // is insignificant; a `#` *inside* a value is not, and truncating there
    // silently classified a changed secret as a comment edit.
    #[test]
    fn only_whole_line_comments_and_blank_lines_are_insignificant() {
        // Comment text, blank lines and whitespace amount: insignificant.
        let a = b"# old comment\nFOO=\"1\"\n\nBAR=2\n";
        let b = b"# NEW comment\n\n\nFOO=\"1\"\nBAR=2\n";
        assert_eq!(significant_lines(a), significant_lines(b));
        // An indented comment is still a whole-line comment.
        assert_eq!(
            significant_lines(b"   # x\nFOO=1\n"),
            significant_lines(b"FOO=1\n")
        );

        // A `#` inside a quoted value is not a comment — the bug this fixes.
        assert_ne!(
            significant_lines(b"psk=\"secret#1\"\n"),
            significant_lines(b"psk=\"secret#2\"\n")
        );
        // A real value change stays significant.
        assert_ne!(
            significant_lines(a),
            significant_lines(b"FOO=\"2\"\nBAR=2\n")
        );
        // A dropped non-comment line stays significant.
        assert_ne!(
            significant_lines(b"FOO=1\nBAR=2\n"),
            significant_lines(b"FOO=1\n")
        );
    }

    #[test]
    fn classify_distinguishes_the_four_cases() {
        let dir = tempfile::tempdir().unwrap();
        let base = camino::Utf8Path::from_path(dir.path()).unwrap();
        let target = base.join("make.conf");
        std::fs::write(target.as_std_path(), "FOO=1\n").unwrap();

        let same = base.join("._cfg0000_make.conf");
        std::fs::write(same.as_std_path(), "FOO=1\n").unwrap();
        assert!(classify(&same, &target) == Kind::Identical);

        let trivial = base.join("._cfg0001_make.conf");
        std::fs::write(trivial.as_std_path(), "# hi\nFOO=1\n").unwrap();
        assert!(classify(&trivial, &target) == Kind::Trivial);

        let differs = base.join("._cfg0002_make.conf");
        std::fs::write(differs.as_std_path(), "FOO=2\n").unwrap();
        assert!(classify(&differs, &target) == Kind::Differs);

        let orphan = base.join("._cfg0000_absent");
        std::fs::write(orphan.as_std_path(), "x\n").unwrap();
        assert!(classify(&orphan, &base.join("absent")) == Kind::New);
    }

    // A sidecar is only ours if its *target* is protected — the scan walks
    // CONFIG_PROTECT directories, and CONFIG_PROTECT_MASK carves back out of
    // them.
    #[test]
    fn scan_finds_sidecars_under_the_root_and_honours_the_mask() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        for rel in ["etc", "etc/skipme"] {
            std::fs::create_dir_all(root.join(rel).as_std_path()).unwrap();
        }
        std::fs::write(root.join("etc/hosts").as_std_path(), "old\n").unwrap();
        std::fs::write(root.join("etc/._cfg0000_hosts").as_std_path(), "new\n").unwrap();
        std::fs::write(root.join("etc/skipme/x").as_std_path(), "old\n").unwrap();
        std::fs::write(root.join("etc/skipme/._cfg0000_x").as_std_path(), "new\n").unwrap();

        let protect = crate::ebuild::ConfigProtect::for_test(&["/etc"], &["/etc/skipme"]);
        let found = scan(root, &protect);
        let names: Vec<&str> = found
            .iter()
            .map(|p| p.target.file_name().unwrap())
            .collect();
        assert_eq!(names, vec!["hosts"], "masked subdirectory must be skipped");
    }

    // Parity with dispatch-conf's use-new (`os.rename`, no chmod): the
    // *sidecar's* mode survives, because it is the package's own — walk_image
    // applies the image permissions when it writes the divert. An earlier
    // version copied the target's mode instead, which was invented, not
    // portage behaviour.
    #[test]
    fn install_keeps_the_packages_permissions_not_the_targets() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let base = camino::Utf8Path::from_path(dir.path()).unwrap();
        let target = base.join("secret.conf");
        std::fs::write(target.as_std_path(), "old\n").unwrap();
        std::fs::set_permissions(target.as_std_path(), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let sidecar = base.join("._cfg0000_secret.conf");
        std::fs::write(sidecar.as_std_path(), "new\n").unwrap();
        std::fs::set_permissions(
            sidecar.as_std_path(),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();

        install(&Pending {
            sidecar: sidecar.clone(),
            target: target.clone(),
            index: 0,
            kind: Kind::Differs,
            bytes: 4,
        })
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(target.as_std_path()).unwrap(),
            "new\n"
        );
        assert!(!sidecar.exists());
        let mode = std::fs::metadata(target.as_std_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o640,
            "the package's mode must survive, as in dispatch-conf"
        );
    }

    #[test]
    fn merge_interactive_pretend_does_not_mutate() {
        let dir = tempfile::tempdir().unwrap();
        let base = camino::Utf8Path::from_path(dir.path()).unwrap();
        let target = base.join("hosts");
        std::fs::write(target.as_std_path(), "old\n").unwrap();
        let sidecar = base.join("._cfg0000_hosts");
        std::fs::write(sidecar.as_std_path(), "new\n").unwrap();

        let pending = [Pending {
            sidecar: sidecar.clone(),
            target: target.clone(),
            index: 0,
            kind: Kind::Differs,
            bytes: 4,
        }];
        let globals = crate::cli::parse_cli(&["em", "-p", "etc", "merge"]);
        merge_interactive(&pending, &globals).unwrap();

        assert_eq!(
            std::fs::read_to_string(target.as_std_path()).unwrap(),
            "old\n"
        );
        assert!(sidecar.exists(), "pretend must leave the sidecar in place");
    }
}
