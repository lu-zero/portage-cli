//! `em sync` / `em maint sync` — update ebuild repositories from `repos.conf`.
//!
//! ## Backends
//!
//! | `sync-type` | Default backend | Optional |
//! |-------------|-----------------|----------|
//! | `git` | shell `git` ([`git_cmd`]) | pure gix via feature **`sync-gix`** |
//! | `rsync` | shell `rsync` ([`rsync_cmd`]) | — |
//! | other | skipped with a message | — |
//!
//! gix is **not** the default: it pulls a large dep graph (build time) and we
//! have not yet proven it faster than `/usr/bin/git` for Portage sync. Enable
//! with `--features sync-gix` to dogfood the pure-Rust path.
//!
//! Selection (Portage / emaint):
//! - named repos: those names, regardless of `auto-sync`
//! - no names: every repo with `auto-sync = yes|true` (default yes) that is
//!   syncable (`sync-type` + `sync-uri` + on-disk `location`)

#[cfg(not(feature = "sync-gix"))]
mod git_cmd;
#[cfg(feature = "sync-gix")]
mod git_gix;
mod rsync_cmd;

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::{RepoEntry, ReposConf};

use crate::cli::Cli;
use crate::style::{C_BOLD, C_ERROR, C_WARN};

/// Run repository sync for `em sync` and `em maint sync`.
pub async fn run(repos: &[String], globals: &Cli) -> Result<()> {
    let conf = globals.roots().repos_conf().context("reading repos.conf")?;
    let selected = select_repos(&conf, repos)?;
    if selected.is_empty() {
        if globals.quiet {
            return Ok(());
        }
        info_line("Nothing to sync.");
        return Ok(());
    }

    let mut failed = 0usize;
    for entry in &selected {
        match sync_one(entry, globals).await {
            Ok(SyncOutcome::Skipped(why)) => {
                if !globals.quiet {
                    info_repo(&entry.name, &format!("Skipping — {why}"));
                }
            }
            Ok(SyncOutcome::Pretend(msg)) => {
                info_repo(&entry.name, &format!("Would sync — {msg}"));
            }
            Ok(SyncOutcome::Synced { changed }) => {
                if !globals.quiet {
                    if changed {
                        info_repo(&entry.name, "Synced (updated)");
                    } else {
                        info_repo(&entry.name, "Synced (already up to date)");
                    }
                }
            }
            Err(e) => {
                error_repo(&entry.name, &format!("{e:#}"));
                failed += 1;
            }
        }
    }

    if failed > 0 {
        bail!("{failed} of {} repo(s) failed to sync", selected.len());
    }
    Ok(())
}

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

#[derive(Debug, Clone, Copy)]
enum SyncKind {
    Git,
    Rsync,
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

    let kind = match sync_type.to_ascii_lowercase().as_str() {
        "git" => SyncKind::Git,
        "rsync" => SyncKind::Rsync,
        other => {
            return Ok(SyncOutcome::Skipped(format!(
                "sync-type={other} not supported (git, rsync)"
            )));
        }
    };

    let volatile = resolve_volatile(entry, path.as_std_path());

    if globals.pretend {
        let action = match kind {
            SyncKind::Git => git_pretend(&path, sync_uri, volatile),
            SyncKind::Rsync => rsync_cmd::pretend(&path, sync_uri),
        };
        return Ok(SyncOutcome::Pretend(action));
    }

    if !globals.quiet {
        let mut out = anstream::stdout();
        let _ = writeln!(
            out,
            ">>> Syncing repository '{C_BOLD}{}{C_BOLD:#}' into '{path}'...",
            entry.name
        );
        if globals.verbose > 0 {
            let _ = writeln!(
                out,
                ">>> sync-type={sync_type} sync-uri={sync_uri} backend={}",
                backend_label(kind)
            );
        }
        let _ = out.flush();
    }

    let quiet = globals.quiet;
    let path_c = path.clone();
    let uri = sync_uri.to_string();
    let changed = tokio::task::spawn_blocking(move || match kind {
        SyncKind::Git => git_sync(&path_c, &uri, volatile, quiet),
        SyncKind::Rsync => rsync_cmd::sync(&path_c, &uri, quiet),
    })
    .await
    .context("sync task join")??;

    Ok(SyncOutcome::Synced { changed })
}

fn backend_label(kind: SyncKind) -> &'static str {
    match kind {
        SyncKind::Git => {
            #[cfg(feature = "sync-gix")]
            {
                "gix (pure Rust)"
            }
            #[cfg(not(feature = "sync-gix"))]
            {
                "git (command)"
            }
        }
        SyncKind::Rsync => "rsync (command)",
    }
}

fn git_pretend(path: &Utf8Path, uri: &str, volatile: bool) -> String {
    #[cfg(feature = "sync-gix")]
    {
        git_gix::GixBackend.pretend(path, uri, volatile)
    }
    #[cfg(not(feature = "sync-gix"))]
    {
        git_cmd::GitCmdBackend.pretend(path, uri, volatile)
    }
}

fn git_sync(path: &Utf8Path, uri: &str, volatile: bool, quiet: bool) -> Result<bool> {
    #[cfg(feature = "sync-gix")]
    {
        git_gix::GixBackend.sync(path, uri, volatile, quiet)
    }
    #[cfg(not(feature = "sync-gix"))]
    {
        git_cmd::GitCmdBackend.sync(path, uri, volatile, quiet)
    }
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

/// Resolve the `portage` user's uid from the host's `/etc/passwd` by name.
/// Falls back to the conventional Gentoo uid (250) when there is no entry
/// (e.g. a minimal chroot with no passwd db yet) — same fallback shape as
/// `portage-repo`'s `resolve_owner`.
fn portage_uid() -> Option<u32> {
    let content = std::fs::read_to_string("/etc/passwd").ok()?;
    parse_portage_uid(&content)
}

fn parse_portage_uid(passwd: &str) -> Option<u32> {
    // `passwd` and `group` rows both carry the id in field 2, so one parser
    // serves both — see [`crate::util::id_by_name_in`].
    crate::util::id_by_name_in(passwd, "portage")
}

fn is_portage_uid(uid: u32) -> bool {
    portage_uid().unwrap_or(250) == uid
}

// ── colour helpers (shared by backends) ──────────────────────────────────

pub(super) fn info_line(msg: &str) {
    let mut out = anstream::stdout();
    let _ = writeln!(out, ">>> {msg}");
    let _ = out.flush();
}

fn info_repo(name: &str, msg: &str) {
    let mut out = anstream::stdout();
    let _ = writeln!(out, ">>> {msg}: '{C_BOLD}{name}{C_BOLD:#}'");
    let _ = out.flush();
}

pub(super) fn warn_line(msg: &str) {
    let mut out = anstream::stderr();
    let _ = writeln!(out, "{C_WARN}!!!{C_WARN:#} {msg}");
    let _ = out.flush();
}

fn error_repo(name: &str, msg: &str) {
    let mut out = anstream::stderr();
    let _ = writeln!(
        out,
        "{C_ERROR}!!!{C_ERROR:#} Failed to sync '{C_BOLD}{name}{C_BOLD:#}': {msg}"
    );
    let _ = out.flush();
}

/// Shared trait for git backends (command vs gix).
pub(super) trait GitBackend {
    fn pretend(&self, path: &Utf8Path, uri: &str, volatile: bool) -> String;
    fn sync(&self, path: &Utf8Path, uri: &str, volatile: bool, quiet: bool) -> Result<bool>;
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
    fn portage_uid_resolves_by_name_not_hardcoded_id() {
        assert_eq!(
            parse_portage_uid(
                "root:x:0:0:root:/root:/bin/bash\nportage:x:999:999:portage:/var/tmp/portage:/sbin/nologin\n"
            ),
            Some(999)
        );
    }

    #[test]
    fn portage_uid_is_none_without_a_matching_entry() {
        assert_eq!(parse_portage_uid("root:x:0:0:root:/root:/bin/bash\n"), None);
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
