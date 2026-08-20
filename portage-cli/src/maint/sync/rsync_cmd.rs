//! rsync sync backend: shell out to `rsync` (Portage parity MVP)
//!
//! Uses a conservative flag set close to Portage's git-overlay sibling
//! defaults: archive mode, delete extras, human-readable progress when not
//! quiet. Fine-tuning (`sync-rsync-*` repos.conf keys) can land later.

use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use camino::Utf8Path;

use super::info_line;

pub(super) fn pretend(path: &Utf8Path, uri: &str) -> String {
    format!("rsync -a --delete {uri}/ → {path}/")
}

/// Returns whether the tree may have changed (rsync exit 0; we cannot cheaply
/// detect no-op without parsing itemize-changes, so `true` on success).
pub(super) fn sync(path: &Utf8Path, uri: &str, quiet: bool) -> Result<bool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating parent of {path}"))?;
    }
    std::fs::create_dir_all(path).with_context(|| format!("creating {path}"))?;

    // Trailing slashes: copy *contents* of uri into path (Portage style).
    let src = ensure_trailing_slash(uri);
    let dst = ensure_trailing_slash(path.as_str());

    if !quiet {
        info_line(&format!("rsync -a --delete {src} → {dst}"));
    }

    let mut cmd = Command::new("rsync");
    cmd.args(["-a", "--delete"]);
    if quiet {
        cmd.arg("-q");
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
    } else {
        // Progress without drowning the terminal on huge trees.
        cmd.args(["--info=progress2"]);
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    }
    cmd.arg(&src).arg(&dst);

    let status = cmd.status().context("spawning rsync")?;
    if !status.success() {
        bail!("rsync failed with {status}");
    }
    // Without --itemize-changes we cannot know no-op; treat success as maybe-changed.
    Ok(true)
}

fn ensure_trailing_slash(s: &str) -> String {
    if s.ends_with('/') {
        s.to_string()
    } else {
        format!("{s}/")
    }
}
