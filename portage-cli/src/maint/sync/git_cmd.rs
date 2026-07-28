//! Default git sync backend: shell out to `git` (Portage parity).
//!
//! Mirrors emerge's git module:
//! - clone: `git clone --depth 1 <uri> <path>`
//! - update: `git fetch origin --depth 1` then hard-reset (or ff-only if volatile)
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
                format!("git fetch --depth 1 + merge --ff-only in {path} from {uri}")
            } else {
                format!("git fetch --depth 1 + reset --hard in {path} from {uri}")
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
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating parent of {path}"))?;
    }
    if !quiet {
        info_line(&format!("git clone --depth 1 {uri} → {path}"));
    }
    let status = git_stdio(quiet)
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

    let before = rev_parse(path_s, "HEAD").ok();

    if !volatile {
        // Align origin URL when Portage would rewrite it (non-volatile).
        let set = git_stdio(true)
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
    let fetch = git_stdio(quiet)
        .args(["-C", path_s, "fetch", "origin", "--depth", "1"])
        .status()
        .context("spawning git fetch")?;
    if !fetch.success() {
        bail!("git fetch failed with {fetch}");
    }

    let tip = rev_parse(path_s, "@{upstream}")
        .or_else(|_| rev_parse(path_s, "FETCH_HEAD"))
        .context("resolving upstream tip after fetch")?;
    let head = rev_parse(path_s, "HEAD").context("resolving HEAD after fetch")?;

    if head == tip {
        if !quiet {
            info_line("already up to date (skip reset)");
        }
        return Ok(false);
    }

    if volatile {
        if !quiet {
            info_line(&format!("git merge --ff-only {tip}…"));
        }
        let st = git_stdio(quiet)
            .args(["-C", path_s, "merge", "--ff-only", &tip])
            .status()
            .context("spawning git merge --ff-only")?;
        if !st.success() {
            bail!("git merge --ff-only failed with {st} (volatile / not a fast-forward)");
        }
    } else {
        if !quiet {
            info_line(&format!("git reset --hard {tip}…"));
        }
        let st = git_stdio(quiet)
            .args(["-C", path_s, "reset", "--hard", &tip])
            .status()
            .context("spawning git reset --hard")?;
        if !st.success() {
            bail!("git reset --hard failed with {st}");
        }
    }

    let after = rev_parse(path_s, "HEAD").ok();
    Ok(before != after)
}

fn rev_parse(path: &str, rev: &str) -> Result<String> {
    let out = Command::new("git")
        .args(["-C", path, "rev-parse", rev])
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

fn git_stdio(quiet: bool) -> Command {
    let mut c = Command::new("git");
    if quiet {
        c.stdout(Stdio::null()).stderr(Stdio::null());
    } else {
        c.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    }
    c
}
