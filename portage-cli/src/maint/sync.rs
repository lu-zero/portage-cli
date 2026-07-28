//! `em sync` / `em maint sync` — update ebuild repositories from `repos.conf`.
//!
//! **Git only (MVP).** `sync-type = git` repos are cloned/fetched with
//! [`gix`]. After a fetch, the worktree is applied with a pure-gix hard-reset
//! composition ([`crate::gix_ext`]) — the orchestration gitoxide still lists
//! as unfinished porcelain. Other `sync-type`s (rsync, …) are skipped with a
//! clear message for now.
//!
//! Selection (Portage / emaint):
//! - named repos: those names, regardless of `auto-sync`
//! - no names: every repo with `auto-sync = yes|true` (default yes) that is
//!   syncable (`sync-type` + `sync-uri` + on-disk `location`)

use std::path::Path;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::{RepoEntry, ReposConf};

use crate::cli::Cli;
use crate::gix_ext;

/// Run repository sync for `em sync` and `em maint sync`.
///
/// `repos` empty → auto-sync-enabled repos only. Non-empty → those names
/// (error if unknown / not syncable as git).
pub async fn run(repos: &[String], globals: &Cli) -> Result<()> {
    let conf = globals.roots().repos_conf().context("reading repos.conf")?;
    let selected = select_repos(&conf, repos)?;
    if selected.is_empty() {
        if globals.quiet {
            return Ok(());
        }
        println!(">>> Nothing to sync.");
        return Ok(());
    }

    let mut failed = 0usize;
    for entry in &selected {
        match sync_one(entry, globals).await {
            Ok(SyncOutcome::Skipped(why)) => {
                if !globals.quiet {
                    println!(">>> Skipping {}: {why}", entry.name);
                }
            }
            Ok(SyncOutcome::Pretend(msg)) => {
                println!(">>> Would sync {}: {msg}", entry.name);
            }
            Ok(SyncOutcome::Synced { changed }) => {
                if !globals.quiet {
                    if changed {
                        println!(">>> Synced {} (updated)", entry.name);
                    } else {
                        println!(">>> Synced {} (already up to date)", entry.name);
                    }
                }
            }
            Err(e) => {
                eprintln!("!!! Failed to sync {}: {e:#}", entry.name);
                failed += 1;
            }
        }
    }

    if failed > 0 {
        bail!("{failed} of {} repo(s) failed to sync", selected.len());
    }
    Ok(())
}

/// Choose which conf entries to sync.
fn select_repos<'a>(conf: &'a ReposConf, names: &[String]) -> Result<Vec<&'a RepoEntry>> {
    if names.is_empty() {
        return Ok(conf
            .repos()
            .iter()
            .filter(|e| e.auto_sync && e.is_syncable())
            .collect());
    }

    let mut out = Vec::with_capacity(names.len());
    let mut missing = Vec::new();
    for name in names {
        match conf.find(name) {
            Some(e) => out.push(e),
            None => missing.push(name.clone()),
        }
    }
    if !missing.is_empty() {
        bail!(
            "unknown repo(s): {} (not in repos.conf)",
            missing.join(", ")
        );
    }
    Ok(out)
}

#[derive(Debug)]
enum SyncOutcome {
    Synced { changed: bool },
    Pretend(String),
    Skipped(String),
}

async fn sync_one(entry: &RepoEntry, globals: &Cli) -> Result<SyncOutcome> {
    let Some(path) = entry.location.as_path() else {
        return Ok(SyncOutcome::Skipped("virtual/alias repo".into()));
    };
    let path = Utf8PathBuf::try_from(path.to_path_buf())
        .map_err(|_| anyhow::anyhow!("repo path is not valid UTF-8: {}", path.display()))?;

    let sync_type = entry
        .sync_type
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let sync_uri = entry
        .sync_uri
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());

    let (Some(sync_type), Some(sync_uri)) = (sync_type, sync_uri) else {
        if entry.sync_type.is_none() && entry.sync_uri.is_none() {
            return Ok(SyncOutcome::Skipped(
                "no sync-type/sync-uri in repos.conf".into(),
            ));
        }
        bail!("missing sync-type or sync-uri in repos.conf");
    };

    if !sync_type.eq_ignore_ascii_case("git") {
        return Ok(SyncOutcome::Skipped(format!(
            "sync-type={sync_type} not supported yet (git only)"
        )));
    }

    if globals.pretend {
        let volatile = resolve_volatile(entry, path.as_std_path());
        let action = if path.join(".git").is_dir() {
            if volatile {
                format!("gix fetch + ff-only hard-reset in {path} from {sync_uri}")
            } else {
                format!("gix fetch + hard-reset in {path} from {sync_uri}")
            }
        } else {
            format!("gix clone {sync_uri} → {path}")
        };
        return Ok(SyncOutcome::Pretend(action));
    }

    if !globals.quiet {
        println!(">>> Syncing {} ({sync_uri})…", entry.name);
    }

    let volatile = resolve_volatile(entry, path.as_std_path());
    let changed = tokio::task::spawn_blocking({
        let path = path.clone();
        let sync_uri = sync_uri.to_string();
        let quiet = globals.quiet;
        move || git_sync(&path, &sync_uri, volatile, quiet)
    })
    .await
    .context("sync task join")??;

    Ok(SyncOutcome::Synced { changed })
}

/// Portage `volatile`: explicit conf wins; else volatile if not root/portage-owned.
fn resolve_volatile(entry: &RepoEntry, path: &Path) -> bool {
    if let Some(v) = entry.volatile {
        return v;
    }
    match std::fs::metadata(path).ok().map(|m| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            m.uid()
        }
        #[cfg(not(unix))]
        {
            let _ = m;
            0u32
        }
    }) {
        Some(0) => false,
        Some(uid) if is_portage_uid(uid) => false,
        None => false,
        Some(_) => true,
    }
}

fn is_portage_uid(uid: u32) -> bool {
    uid == 250
}

/// Clone or fetch+hard-reset with pure gix.
///
/// Returns whether the worktree tip changed.
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
        eprintln!(">>> gix clone {url} → {path}");
    }
    let mut prepare = gix::prepare_clone(url, path.as_std_path())
        .map_err(|e| anyhow::anyhow!("gix clone prepare: {e}"))?
        .with_shallow(gix::remote::fetch::Shallow::DepthAtRemote(
            std::num::NonZeroU32::new(1).expect("1"),
        ));
    let (mut checkout, _outcome) = prepare
        .fetch_then_checkout(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
        .map_err(|e| anyhow::anyhow!("gix clone fetch: {e}"))?;
    let (_repo, _) = checkout
        .main_worktree(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
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
        eprintln!(">>> warning: could not set remote URL to {uri}: {e:#}");
    }

    let mut remote = repo
        .find_default_remote(gix::remote::Direction::Fetch)
        .transpose()
        .map_err(|e| anyhow::anyhow!("default remote: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("no default remote configured in {path}"))?;
    remote = remote.with_fetch_tags(gix::remote::fetch::Tags::None);

    if !quiet {
        eprintln!(">>> gix fetch in {path}");
    }
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|e| anyhow::anyhow!("connect remote: {e}"))?;
    let prepare = connection
        .prepare_fetch(gix::progress::Discard, Default::default())
        .map_err(|e| anyhow::anyhow!("prepare fetch: {e}"))?;
    prepare
        .receive(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
        .map_err(|e| anyhow::anyhow!("fetch: {e}"))?;

    // Re-open so we see updated remote-tracking refs.
    let repo =
        gix::open(path.as_std_path()).map_err(|e| anyhow::anyhow!("re-open after fetch: {e}"))?;
    let tip = gix_ext::resolve_upstream_tip(&repo).map_err(|e| anyhow::anyhow!("{e}"))?;

    if !quiet {
        eprintln!(
            ">>> gix hard-reset{} in {path}",
            if volatile { " (ff-only)" } else { "" }
        );
    }
    gix_ext::hard_reset_to(&repo, tip, volatile).map_err(|e| anyhow::anyhow!("{e}"))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    fn write(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn select_auto_sync_only_when_no_names() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("repos.conf");
        write(
            &conf,
            r#"
[gentoo]
location = /var/db/repos/gentoo
sync-type = git
sync-uri = https://example/gentoo.git

[local]
location = /var/db/repos/local
auto-sync = no
sync-type = git
sync-uri = https://example/local.git

[nosync]
location = /var/db/repos/nosync
"#,
        );
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        let selected = select_repos(&rc, &[]).unwrap();
        let names: Vec<_> = selected.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["gentoo"]);
    }

    #[test]
    fn select_named_ignores_auto_sync() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("repos.conf");
        write(
            &conf,
            r#"
[local]
location = /var/db/repos/local
auto-sync = no
sync-type = git
sync-uri = https://example/local.git
"#,
        );
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        let selected = select_repos(&rc, &["local".into()]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "local");
    }

    #[test]
    fn select_unknown_name_errors() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("repos.conf");
        write(&conf, "[gentoo]\nlocation = /a\n");
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        let err = select_repos(&rc, &["nope".into()]).unwrap_err();
        assert!(err.to_string().contains("unknown repo"));
    }
}
