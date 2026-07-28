//! Optional pure-gix git sync backend (`--features sync-gix`).
//!
//! Not the default: heavy to compile and not yet proven faster than `git` for
//! Portage sync. Kept for dogfooding / upstream hard-reset contribution.

use std::num::NonZeroU32;

use anyhow::{Context, Result};
use camino::Utf8Path;

use crate::gix_ext;
use crate::gix_ext::ProgressSession;

use super::{GitBackend, info_line, warn_line};

pub(super) struct GixBackend;

impl GitBackend for GixBackend {
    fn pretend(&self, path: &Utf8Path, uri: &str, volatile: bool) -> String {
        if path.join(".git").is_dir() {
            if volatile {
                format!("gix fetch --depth 1 + ff-only hard-reset in {path} from {uri}")
            } else {
                format!("gix fetch --depth 1 + hard-reset in {path} from {uri}")
            }
        } else {
            format!("gix clone --depth 1 {uri} → {path}")
        }
    }

    fn sync(&self, path: &Utf8Path, uri: &str, volatile: bool, quiet: bool) -> Result<bool> {
        git_sync(path, uri, volatile, quiet)
    }
}

fn sync_shallow() -> gix::remote::fetch::Shallow {
    gix::remote::fetch::Shallow::DepthAtRemote(NonZeroU32::new(1).expect("1"))
}

fn git_sync(path: &Utf8Path, uri: &str, volatile: bool, quiet: bool) -> Result<bool> {
    let url =
        gix::url::parse(uri.into()).map_err(|e| anyhow::anyhow!("invalid sync-uri {uri}: {e}"))?;

    if path.join(".git").is_dir() || gix::open(path.as_std_path()).is_ok() {
        git_update(path, uri, volatile, quiet)
    } else {
        git_clone(path, url, quiet)?;
        Ok(true)
    }
}

fn git_clone(path: &Utf8Path, url: gix::Url, quiet: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating parent of {path}"))?;
    }
    if !quiet {
        info_line(&format!("gix clone --depth 1 {url} → {path}"));
    }
    let progress = ProgressSession::new(quiet);
    let mut prepare = gix::prepare_clone(url, path.as_std_path())
        .map_err(|e| anyhow::anyhow!("gix clone prepare: {e}"))?
        .with_shallow(sync_shallow());
    let (mut checkout, _outcome) = prepare
        .fetch_then_checkout(progress.child("clone"), &gix::interrupt::IS_INTERRUPTED)
        .map_err(|e| anyhow::anyhow!("gix clone fetch: {e}"))?;
    let (_repo, _) = checkout
        .main_worktree(progress.child("checkout"), &gix::interrupt::IS_INTERRUPTED)
        .map_err(|e| anyhow::anyhow!("gix clone checkout: {e}"))?;
    Ok(())
}

fn git_update(path: &Utf8Path, uri: &str, volatile: bool, quiet: bool) -> Result<bool> {
    let repo = gix::open(path.as_std_path())
        .map_err(|e| anyhow::anyhow!("opening git repo {path}: {e}"))?;

    let before = head_id(&repo);

    if !volatile
        && let Err(e) = align_remote_url(&repo, uri)
        && !quiet
    {
        warn_line(&format!("could not set remote URL to {uri}: {e:#}"));
    }

    let mut remote = repo
        .find_default_remote(gix::remote::Direction::Fetch)
        .transpose()
        .map_err(|e| anyhow::anyhow!("default remote: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("no default remote configured in {path}"))?;
    remote = remote.with_fetch_tags(gix::remote::fetch::Tags::None);

    if !quiet {
        info_line(&format!("gix fetch origin --depth 1 in {path}"));
    }
    let progress = ProgressSession::new(quiet);
    let fetch_status = {
        let connection = remote
            .connect(gix::remote::Direction::Fetch)
            .map_err(|e| anyhow::anyhow!("connect remote: {e}"))?;
        let prepare = connection
            .prepare_fetch(progress.child("negotiate"), Default::default())
            .map_err(|e| anyhow::anyhow!("prepare fetch: {e}"))?
            .with_shallow(sync_shallow());
        let outcome = prepare
            .receive(progress.child("receive"), &gix::interrupt::IS_INTERRUPTED)
            .map_err(|e| anyhow::anyhow!("fetch: {e}"))?;
        outcome.status
    };
    drop(progress);

    let repo =
        gix::open(path.as_std_path()).map_err(|e| anyhow::anyhow!("re-open after fetch: {e}"))?;
    let tip = gix_ext::resolve_upstream_tip(&repo).map_err(|e| anyhow::anyhow!("{e}"))?;
    let head = head_id(&repo);

    if head == Some(tip) {
        if !quiet {
            let why = match &fetch_status {
                gix::remote::fetch::Status::NoPackReceived { .. } => "no pack received",
                gix::remote::fetch::Status::Change { .. } => "HEAD already at tip",
            };
            info_line(&format!("already up to date ({why}; skip hard-reset)"));
        }
        return Ok(false);
    }

    if !quiet {
        info_line(&format!(
            "gix hard-reset{} to {}…",
            if volatile { " (ff-only)" } else { "" },
            tip.to_hex_with_len(12)
        ));
    }
    let progress = ProgressSession::new(quiet);
    gix_ext::hard_reset_to(&repo, tip, volatile, progress.child("hard-reset"))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    drop(progress);

    let after = head_id(&repo);
    Ok(before != after)
}

fn head_id(repo: &gix::Repository) -> Option<gix::ObjectId> {
    repo.head_id().ok().map(|id| id.detach())
}

fn align_remote_url(repo: &gix::Repository, uri: &str) -> Result<()> {
    let name = repo
        .remote_default_name(gix::remote::Direction::Fetch)
        .ok_or_else(|| anyhow::anyhow!("no default remote name"))?
        .to_string();
    gix_ext::set_remote_url(repo, &name, uri).map_err(|e| anyhow::anyhow!("{e}"))
}
