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

/// Lines with comments and blank lines dropped and whitespace collapsed —
/// what `dispatch-conf`'s auto-merge treats as "no real change"
fn significant_lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|l| {
            let l = match l.find('#') {
                Some(i) => &l[..i],
                None => l,
            };
            l.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .filter(|l| !l.is_empty())
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
        if verb == "install" {
            install(p)?;
        } else {
            std::fs::remove_file(p.sidecar.as_std_path())
                .with_context(|| format!("removing {}", p.sidecar))?;
        }
    }
    if globals.pretend {
        println!(">>> Would resolve {} file(s).", selected.len());
    } else {
        println!(">>> Resolved {} file(s).", selected.len());
    }
    Ok(())
}

/// Move `sidecar` over `target`, keeping the target's permissions
///
/// The target's mode wins because a package's `._cfg` copy carries whatever
/// the image had, while the live file may have been deliberately tightened
/// (a credentials file left `0600`, say).
fn install(p: &Pending) -> Result<()> {
    if let Ok(meta) = std::fs::metadata(p.target.as_std_path()) {
        let _ = std::fs::set_permissions(p.sidecar.as_std_path(), meta.permissions());
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
/// Returns whether the merge completed — a user who quits `sdiff` leaves the
/// sidecar in place so the file can be revisited.
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
    match status {
        Ok(s) if s.success() => {
            std::fs::rename(tmp.as_std_path(), p.target.as_std_path())
                .with_context(|| format!("installing the merge result over {}", p.target))?;
            let _ = std::fs::remove_file(p.sidecar.as_std_path());
            Ok(true)
        }
        Ok(_) => {
            let _ = std::fs::remove_file(tmp.as_std_path());
            println!("    merge abandoned; {} left in place", p.sidecar);
            Ok(false)
        }
        Err(e) => {
            let _ = std::fs::remove_file(tmp.as_std_path());
            crate::style::warn_line!("cannot run sdiff: {e}");
            Ok(false)
        }
    }
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

    // The distinction `--auto` rests on: a change confined to comments or
    // blank lines is not a change a human needs to adjudicate.
    #[test]
    fn comment_and_whitespace_only_edits_are_trivial() {
        let a = b"# old comment\nFOO=\"1\"\n\nBAR=2\n";
        let b = b"# NEW comment\n\n\nFOO=\"1\"\nBAR=2   # trailing\n";
        assert_eq!(significant_lines(a), significant_lines(b));

        let c = b"FOO=\"2\"\nBAR=2\n";
        assert_ne!(significant_lines(a), significant_lines(c));
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

    #[test]
    fn install_keeps_the_targets_permissions() {
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
            std::fs::Permissions::from_mode(0o644),
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
        assert_eq!(mode, 0o600, "a tightened live file must not be loosened");
    }
}
