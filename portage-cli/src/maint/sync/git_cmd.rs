//! Default git sync backend: shell out to `git` (Portage parity).
//!
//! Mirrors `portage.sync.modules.git.git.GitSync.update` (see
//! `sync/modules/git/git.py`, comment block above `sync_depth`, bug #887025 /
//! bug #824782): a `--depth 1` fetch's new tip commonly shares no ancestry
//! with the previous shallow history, so a plain `git merge`/`--ff-only`
//! refuses with "unrelated histories". Portage avoids that entirely:
//! - clone: `git clone --depth 1 <uri> <path>`
//! - update: `git fetch origin --depth 1`, then
//!   - non-volatile: `git clean --force -d -x` (drop orphaned files a prior
//!     shallow sync left behind — the actual failure mode these bugs are
//!     about) followed by `git reset --hard` (always succeeds regardless of
//!     ancestry)
//!   - volatile: `git reset --merge <tip>` — unlike `git merge`, `reset
//!     --merge` does not require a common ancestor, so it survives the
//!     shallow-history case while still refusing to clobber uncommitted
//!     worktree changes
//! - skip reset when HEAD already matches `@{upstream}`

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use camino::Utf8Path;

use super::{GitBackend, info_line, warn_line};

pub(super) struct GitCmdBackend;

impl GitBackend for GitCmdBackend {
    fn pretend(&self, path: &Utf8Path, uri: &str, volatile: bool) -> String {
        if path.join(".git").is_dir() {
            if volatile {
                format!("git fetch --depth 1 + reset --merge in {path} from {uri}")
            } else {
                format!("git fetch --depth 1 + clean -d -x + reset --hard in {path} from {uri}")
            }
        } else {
            format!("git clone --depth 1 {uri} → {path}")
        }
    }

    fn sync(&self, path: &Utf8Path, uri: &str, volatile: bool, quiet: bool) -> Result<bool> {
        if path.join(".git").is_dir() {
            git_update(path.as_std_path(), uri, volatile, quiet)
        } else {
            git_clone(path, uri, quiet)?;
            Ok(true)
        }
    }
}

fn git_clone(path: &Utf8Path, uri: &str, quiet: bool) -> Result<()> {
    let parent = path.parent().map(Utf8Path::as_std_path);
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent).with_context(|| format!("creating parent of {path}"))?;
    }
    if !quiet {
        info_line(&format!("git clone --depth 1 {uri} → {path}"));
    }
    // cwd pinned to a directory we just ensured exists — spawning must not
    // depend on the calling process's own (possibly stale) working
    // directory, which `-C`/`git`'s own args do not cover.
    let cwd = parent.unwrap_or_else(|| Path::new("/"));
    let status = git_stdio(quiet, cwd)
        .args(["clone", "--depth", "1", uri, path.as_str()])
        .status()
        .context("spawning git clone")?;
    if !status.success() {
        bail!("git clone failed with {status}");
    }
    Ok(())
}

fn git_update(path: &Path, uri: &str, volatile: bool, quiet: bool) -> Result<bool> {
    let path_s = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("repo path is not valid UTF-8"))?;

    let before = rev_parse(path, "HEAD").ok();

    if !volatile {
        // Align origin URL when Portage would rewrite it (non-volatile).
        let set = git_stdio(true, path)
            .args(["-C", path_s, "remote", "set-url", "origin", uri])
            .status();
        match set {
            Ok(st) if st.success() => {}
            Ok(st) if !quiet => {
                warn_line(&format!("git remote set-url origin failed ({st})"));
            }
            Err(e) if !quiet => {
                warn_line(&format!("git remote set-url origin: {e}"));
            }
            _ => {}
        }
    }

    if !quiet {
        info_line(&format!("git fetch origin --depth 1 in {path_s}"));
    }
    let fetch = git_stdio(quiet, path)
        .args(["-C", path_s, "fetch", "origin", "--depth", "1"])
        .status()
        .context("spawning git fetch")?;
    if !fetch.success() {
        bail!("git fetch failed with {fetch}");
    }

    let tip = rev_parse(path, "@{upstream}")
        .or_else(|_| rev_parse(path, "FETCH_HEAD"))
        .context("resolving upstream tip after fetch")?;
    let head = rev_parse(path, "HEAD").context("resolving HEAD after fetch")?;

    if head == tip {
        if !quiet {
            info_line("already up to date (skip reset)");
        }
        return Ok(false);
    }

    if volatile {
        // `reset --merge`, not `merge`/`--ff-only`: a `--depth 1` fetch's tip
        // commonly has no common ancestor with the previous shallow history,
        // so a real merge refuses with "unrelated histories" (reproduced
        // locally against a genuine shallow clone). `reset --merge` moves
        // HEAD/index to `tip` without requiring ancestry, while still
        // refusing (non-zero exit) if uncommitted worktree changes would be
        // clobbered — this is Portage's own fix for the identical failure
        // (bug #887025 / bug #824782).
        if !quiet {
            info_line(&format!("git reset --merge {tip}…"));
        }
        let st = git_stdio(quiet, path)
            .args(["-C", path_s, "reset", "--merge", &tip])
            .status()
            .context("spawning git reset --merge")?;
        if !st.success() {
            bail!("git reset --merge failed with {st} (volatile / local changes conflict)");
        }
    } else {
        // `git clean` first: a shallow fetch can leave orphaned files from a
        // prior sync that block the next one (the exact failure mode
        // Portage's git module comments describe) — safe to clobber here
        // since non-volatile means only we ever touch this tree.
        if !quiet {
            info_line("git clean --force -d -x…");
        }
        let clean = git_stdio(quiet, path)
            .args(["-C", path_s, "clean", "--force", "-d", "-x"])
            .status()
            .context("spawning git clean")?;
        if !clean.success() {
            bail!("git clean failed with {clean}");
        }
        if !quiet {
            info_line(&format!("git reset --hard {tip}…"));
        }
        let st = git_stdio(quiet, path)
            .args(["-C", path_s, "reset", "--hard", &tip])
            .status()
            .context("spawning git reset --hard")?;
        if !st.success() {
            bail!("git reset --hard failed with {st}");
        }
    }

    let after = rev_parse(path, "HEAD").ok();
    Ok(before != after)
}

fn rev_parse(cwd: &Path, rev: &str) -> Result<String> {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", rev])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("git rev-parse {rev}"))?;
    if !out.status.success() {
        bail!(
            "git rev-parse {rev} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `cwd` is always pinned explicitly (never left to the inherited process
/// cwd): the OS must resolve *some* working directory to fork/exec `git`
/// even though `-C <path>` is what actually tells git where the repo is, and
/// the calling process's ambient cwd is out of our control (another thread
/// could have `set_current_dir`'d away and dropped it, or a caller could
/// invoke `em sync` from a directory removed out from under it).
fn git_stdio(quiet: bool, cwd: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(cwd);
    if quiet {
        c.stdout(Stdio::null()).stderr(Stdio::null());
    } else {
        c.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn git(cwd: &std::path::Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?} failed");
    }

    /// A real `file://` bare remote (not a plain local-path clone, which
    /// silently ignores `--depth` — the bug this whole module works around
    /// only reproduces against a genuinely shallow clone).
    struct Fixture {
        _tmp: tempfile::TempDir,
        remote_uri: String,
        overlay: Utf8PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        git(
            tmp.path(),
            &["init", "--bare", "-q", remote.to_str().unwrap()],
        );

        let seed = tmp.path().join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "-q"]);
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        std::fs::write(seed.join("a.txt"), "one\n").unwrap();
        git(&seed, &["add", "a.txt"]);
        git(&seed, &["commit", "-q", "-m", "one"]);
        git(&seed, &["push", "-q", "origin", "HEAD:master"]);

        Fixture {
            remote_uri: format!("file://{}", remote.display()),
            overlay: Utf8PathBuf::try_from(tmp.path().join("overlay")).unwrap(),
            _tmp: tmp,
        }
    }

    fn advance_remote(fixture: &Fixture, content: &str) {
        let tmp = fixture._tmp.path();
        let work = tmp.join("work2");
        git(
            tmp,
            &[
                "clone",
                "-q",
                fixture.remote_uri.trim_start_matches("file://"),
                work.to_str().unwrap(),
            ],
        );
        std::fs::write(work.join("a.txt"), content).unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-q", "-m", "advance"]);
        git(&work, &["push", "-q", "origin", "HEAD:master"]);
    }

    #[test]
    fn clone_creates_a_shallow_checkout() {
        // Every git spawn here resolves `git` via `PATH` — serialize against
        // any test elsewhere that temporarily mutates it (`crate::test_support`).
        let _path = crate::test_support::path_lock();
        let f = fixture();
        let changed = GitCmdBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();
        assert!(changed);
        assert_eq!(
            std::fs::read_to_string(f.overlay.join("a.txt")).unwrap(),
            "one\n"
        );
    }

    /// Regression test for the confirmed bug: a `--depth 1` fetch's new tip
    /// shares no ancestry with the previous shallow history, so `git merge
    /// --ff-only` used to fail with "refusing to merge unrelated histories"
    /// on every volatile-repo sync after the first. `reset --merge` must not
    /// have that problem.
    #[test]
    fn volatile_update_survives_unrelated_shallow_history() {
        let _path = crate::test_support::path_lock();
        let f = fixture();
        GitCmdBackend
            .sync(&f.overlay, &f.remote_uri, true, true)
            .unwrap();
        advance_remote(&f, "two\n");

        let changed = GitCmdBackend
            .sync(&f.overlay, &f.remote_uri, true, true)
            .expect("volatile re-sync over a shallow history must not fail");
        assert!(changed);
        assert_eq!(
            std::fs::read_to_string(f.overlay.join("a.txt")).unwrap(),
            "two\n"
        );
    }

    #[test]
    fn nonvolatile_update_cleans_orphaned_files_before_reset() {
        let _path = crate::test_support::path_lock();
        let f = fixture();
        GitCmdBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();
        advance_remote(&f, "two\n");
        // A file a prior shallow sync could plausibly have left behind,
        // untracked in the new tip — `git clean -d -x` must clear it so it
        // cannot block a future `reset --hard`.
        std::fs::write(f.overlay.join("orphan.txt"), "stale\n").unwrap();

        let changed = GitCmdBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();
        assert!(changed);
        assert_eq!(
            std::fs::read_to_string(f.overlay.join("a.txt")).unwrap(),
            "two\n"
        );
        assert!(!f.overlay.join("orphan.txt").exists());
    }

    #[test]
    fn update_is_a_no_op_when_already_at_the_upstream_tip() {
        let _path = crate::test_support::path_lock();
        let f = fixture();
        GitCmdBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();
        let changed = GitCmdBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();
        assert!(!changed, "no upstream change: sync must report unchanged");
    }
}
