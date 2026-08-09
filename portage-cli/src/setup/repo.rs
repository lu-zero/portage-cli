//! `em --local setup`'s repo resolution: piggy-back the host's `::gentoo` if
//! one is usable, else write an own-tree entry and sync it — step 2 of the
//! config-root ladder (`todo/local-bootstrap-provided.md`).

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::ReposConf;

use crate::cli::Cli;
use crate::config_plan::{self, ConfigEntry};

/// Default `::gentoo` mirror for a fresh own-tree clone. Matches the URI
/// real `emerge-webrsync`/the Gentoo handbook point new installs at.
const DEFAULT_SYNC_URI: &str = "https://github.com/gentoo-mirror/gentoo.git";

/// A repo tree is usable once it has a `profiles/repo_name` — the same
/// on-disk marker Portage itself requires for a valid repository.
fn repo_has_profiles(path: &Utf8Path) -> bool {
    path.join("profiles/repo_name").is_file()
}

/// A host `::gentoo` this prefix can piggy-back on without cloning anything:
/// `host_root`'s own `repos.conf` main/`gentoo` entry if it resolves to a
/// real tree, else the conventional `<host_root>/var/db/repos/gentoo` path.
/// `host_root` is `/` in production; tests pass a fixture dir so the piggy-
/// back-vs-own-tree decision is exercised without touching the real host.
fn detect_host_gentoo(host_root: &Utf8Path) -> Option<Utf8PathBuf> {
    if let Ok(conf) = ReposConf::load_rooted(host_root, &[])
        && let Some(entry) = conf.main_repo().or_else(|| conf.find("gentoo"))
        && let Some(path) = entry.location.as_path()
        && let Some(path) = Utf8Path::from_path(path)
        && repo_has_profiles(path)
    {
        return Some(path.to_path_buf());
    }
    let fallback = host_root.join("var/db/repos/gentoo");
    repo_has_profiles(&fallback).then_some(fallback)
}

/// Ensure `prefix` has a resolvable `::gentoo` — piggy-back a usable host
/// tree, else write an own-tree entry pointing at
/// `<prefix>/var/db/repos/gentoo` and sync it (shallow clone; the git sync
/// backend already defaults to `--depth 1`). Idempotent: an existing
/// `gentoo.conf` (this prefix's own earlier run, or a hand edit) is trusted
/// and only re-synced if its tree turns out to still be empty. Returns the
/// resolved on-disk repo path.
pub(super) async fn ensure_repo(cli: &Cli, prefix: &Utf8Path) -> Result<Utf8PathBuf> {
    ensure_repo_from(cli, prefix, Utf8Path::new("/")).await
}

/// [`ensure_repo`], but with the "host" root to piggy-back detection against
/// pulled out as a parameter — production always passes `/`; tests pass a
/// fixture dir standing in for it.
async fn ensure_repo_from(
    cli: &Cli,
    prefix: &Utf8Path,
    host_root: &Utf8Path,
) -> Result<Utf8PathBuf> {
    let path = resolve_repo_path(prefix, host_root)?;
    // One trigger for all three sources (existing conf, piggy-back, fresh
    // own-tree): sync whenever the resolved location doesn't have a tree
    // yet, not just for the own-tree branch — an existing `gentoo.conf`
    // whose sync was previously interrupted needs the same retry.
    if !repo_has_profiles(&path) {
        sync_gentoo(cli).await?;
    }
    Ok(path)
}

/// The synchronous half of [`ensure_repo_from`]: decide (and, unless already
/// decided by an earlier run, write) which `::gentoo` this prefix uses, with
/// no network I/O — the part [`ensure_repo_from`]'s tests exercise directly,
/// since sync is inherently not something a unit test should depend on.
fn resolve_repo_path(prefix: &Utf8Path, host_root: &Utf8Path) -> Result<Utf8PathBuf> {
    let conf_path = prefix.join("etc/portage/repos.conf/gentoo.conf");

    if conf_path.exists() {
        let conf = ReposConf::load_from(&[conf_path.as_std_path()])
            .with_context(|| format!("reading {conf_path}"))?;
        let entry = conf
            .find("gentoo")
            .with_context(|| format!("{conf_path} has no [gentoo] section"))?;
        return entry
            .location
            .as_path()
            .and_then(Utf8Path::from_path)
            .map(Utf8Path::to_path_buf)
            .with_context(|| format!("{conf_path}'s [gentoo] location is not a real path"));
    }

    if let Some(host_path) = detect_host_gentoo(host_root) {
        tracing::info!(location = %host_path, "::gentoo resolved (piggy-backing the host's repo)");
        config_plan::apply_now(&[ConfigEntry::CreateOnly {
            path: conf_path,
            desired: format!("[gentoo]\nlocation = {host_path}\n"),
        }])?;
        return Ok(host_path);
    }

    let own_path = prefix.join("var/db/repos/gentoo");
    tracing::info!(location = %own_path, sync_uri = DEFAULT_SYNC_URI, "::gentoo resolved (own tree, syncing)");
    config_plan::apply_now(&[ConfigEntry::CreateOnly {
        path: conf_path,
        desired: format!(
            "[DEFAULT]\nmain-repo = gentoo\n\n\
             [gentoo]\nlocation = {own_path}\nsync-type = git\nsync-uri = {DEFAULT_SYNC_URI}\n"
        ),
    }])?;
    Ok(own_path)
}

/// Run `em sync gentoo` against `cli`'s own topology. `cli.roots().repos_conf()`
/// always merges the `--local` overlay's `repos.conf` as an extra source
/// (`ReposConf::load_rooted`'s doc comment) regardless of whether the merge
/// config root has flipped to the prefix yet, so the `gentoo.conf` entry
/// [`ensure_repo`] just wrote is already visible here.
async fn sync_gentoo(cli: &Cli) -> Result<()> {
    crate::maint::sync::run(&["gentoo".to_string()], cli).await
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn repo_has_profiles_requires_the_repo_name_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap();
        assert!(!repo_has_profiles(path));
        std::fs::create_dir_all(path.join("profiles")).unwrap();
        std::fs::write(path.join("profiles/repo_name"), "gentoo\n").unwrap();
        assert!(repo_has_profiles(path));
    }

    fn utf8_tempdir() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().to_path_buf();
        (dir, path)
    }

    fn fixture_repo() -> (tempfile::TempDir, Utf8PathBuf) {
        let (dir, path) = utf8_tempdir();
        std::fs::create_dir_all(path.join("profiles")).unwrap();
        std::fs::write(path.join("profiles/repo_name"), "gentoo\n").unwrap();
        (dir, path)
    }

    #[test]
    fn piggy_backs_a_usable_host_repo() {
        let (_repo_dir, repo_path) = fixture_repo();
        let (_host_dir, host_root) = utf8_tempdir();
        let host_conf_dir = host_root.join("etc/portage/repos.conf");
        std::fs::create_dir_all(&host_conf_dir).unwrap();
        std::fs::write(
            host_conf_dir.join("gentoo.conf"),
            format!("[gentoo]\nlocation = {repo_path}\n"),
        )
        .unwrap();
        let (_prefix_dir, prefix) = utf8_tempdir();

        let resolved = resolve_repo_path(&prefix, &host_root).unwrap();
        assert_eq!(resolved, repo_path);
        let written =
            std::fs::read_to_string(prefix.join("etc/portage/repos.conf/gentoo.conf")).unwrap();
        assert!(written.contains(repo_path.as_str()));
    }

    #[test]
    fn falls_back_to_an_own_tree_when_the_host_has_no_repo() {
        let (_host_dir, host_root) = utf8_tempdir();
        let (_prefix_dir, prefix) = utf8_tempdir();

        let resolved = resolve_repo_path(&prefix, &host_root).unwrap();
        assert_eq!(resolved, prefix.join("var/db/repos/gentoo"));
        let written =
            std::fs::read_to_string(prefix.join("etc/portage/repos.conf/gentoo.conf")).unwrap();
        assert!(written.contains("main-repo = gentoo"));
        assert!(written.contains(DEFAULT_SYNC_URI));
    }

    #[test]
    fn an_existing_conf_is_trusted_and_never_rewritten() {
        let (_host_dir, host_root) = utf8_tempdir();
        let (_prefix_dir, prefix) = utf8_tempdir();
        let conf_dir = prefix.join("etc/portage/repos.conf");
        std::fs::create_dir_all(&conf_dir).unwrap();
        let hand_written = "[gentoo]\nlocation = /custom/path\n";
        std::fs::write(conf_dir.join("gentoo.conf"), hand_written).unwrap();

        let resolved = resolve_repo_path(&prefix, &host_root).unwrap();
        assert_eq!(resolved, Utf8PathBuf::from("/custom/path"));
        assert_eq!(
            std::fs::read_to_string(conf_dir.join("gentoo.conf")).unwrap(),
            hand_written
        );
    }

    #[tokio::test]
    async fn ensure_repo_from_skips_sync_once_the_resolved_tree_already_has_profiles() {
        // Own-tree branch, but the target dir is pre-populated — proves the
        // unified `!repo_has_profiles` sync trigger skips the network call
        // (this test would hang/fail on a real clone otherwise).
        let (_host_dir, host_root) = utf8_tempdir();
        let (_prefix_dir, prefix) = utf8_tempdir();
        let own_path = prefix.join("var/db/repos/gentoo");
        std::fs::create_dir_all(own_path.join("profiles")).unwrap();
        std::fs::write(own_path.join("profiles/repo_name"), "gentoo\n").unwrap();

        let cli = crate::cli::Cli::parse_from(["em", "sync"]);
        let resolved = ensure_repo_from(&cli, &prefix, &host_root).await.unwrap();
        assert_eq!(resolved, own_path);
    }
}
