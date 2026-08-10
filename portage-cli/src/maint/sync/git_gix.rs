//! Optional pure-gix git sync backend (`--features sync-gix`).
//!
//! Not the default: heavy to compile and not yet proven faster than `git` for
//! Portage sync. Kept for dogfooding / upstream hard-reset contribution.
//!
//! Behaviour tracks [`super::git_cmd`], which in turn mirrors
//! `portage.sync.modules.git.git.GitSync.update`:
//!
//! - clone: shallow (`Shallow::DepthAtRemote(1)`)
//! - update: fetch, shallow **unless** the repo is volatile and was not
//!   already shallow ([`gix::Repository::is_shallow`]) — Portage never
//!   shallows a repo the user cloned in full
//!   - non-volatile: [`gix_ext::clean_worktree`] (`git clean -d -x`) then a
//!     [`ResetGuard::Force`] hard reset
//!   - volatile: a [`ResetGuard::RefuseIfDirty`] hard reset, our stand-in for
//!     `git reset --merge` (no ancestry requirement, still refuses to destroy
//!     uncommitted work — see [`ResetGuard::RefuseIfDirty`] for the exact
//!     deviation from git's finer-grained behaviour)
//! - the remote URL is only rewritten when it actually differs from
//!   `sync-uri`, and only for non-volatile repos
//! - skip the reset entirely when `HEAD` already matches the upstream tip
//!
//! ## Two `git_cmd` steps with no gix counterpart
//!
//! **`git gc --auto` after a shallow fetch (bug #599008).** Deliberately not
//! ported: gitoxide 0.83 ships no repacking, pruning or maintenance API at all
//! (`gix::Repository` has no `gc`/`maintenance`/`prune` entry point), so there
//! is nothing to call. Shelling out to `git gc` here would defeat the point of
//! a pure-gix backend, and gix's own fetch already avoids the specific hazard
//! that made Portage's `gc` call *mandatory* rather than merely tidy: it
//! writes a `.keep` file alongside each received pack until the corresponding
//! refs are updated, so a concurrent collector cannot drop objects out from
//! under an in-flight fetch. What remains is unbounded growth of unreachable
//! objects across many shallow fetches — a disk-usage issue, not a
//! correctness one, and unavoidable until gitoxide grows maintenance support.
//! A user who cares can still run `git gc` in the repo by hand; the default
//! `git_cmd` backend does it automatically.
//!
//! **`GIT_CEILING_DIRECTORIES`.** Genuinely a non-issue here, not an
//! oversight. That variable stops `git`'s *upward* search for a `.git` when a
//! command is run inside a directory that is not itself a repository. gix has
//! no such search on this path: [`gix::open`] resolves exactly `<path>/.git`
//! (falling back to `<path>` itself if that is the git dir) and errors out
//! otherwise — upward discovery is a separate, explicitly-named API
//! (`gix::discover`) that this backend never calls.

use std::num::NonZeroU32;

use anyhow::{Context, Result};
use camino::Utf8Path;

use crate::gix_ext;
use crate::gix_ext::{ProgressSession, ResetGuard};

use super::{GitBackend, info_line, warn_line};

pub(super) struct GixBackend;

impl GitBackend for GixBackend {
    fn pretend(&self, path: &Utf8Path, uri: &str, volatile: bool) -> String {
        if path.join(".git").is_dir() {
            if volatile {
                format!("gix fetch --depth 1 + hard-reset (refuse if dirty) in {path} from {uri}")
            } else {
                format!("gix fetch --depth 1 + clean -d -x + hard-reset in {path} from {uri}")
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
    let url = gix::url::parse(uri).map_err(|e| anyhow::anyhow!("invalid sync-uri {uri}: {e}"))?;

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
    let mut repo = gix::open(path.as_std_path())
        .map_err(|e| anyhow::anyhow!("opening git repo {path}: {e}"))?;
    // The fetch below updates remote-tracking refs, which (like
    // gix_ext::hard_reset_to's own ref move) needs a resolvable committer
    // identity or gix hard-errors — see hard_reset_to's doc comment for why
    // this must not be skipped just because a host has no git identity
    // configured (a normal state for automated sync to run under).
    repo.committer_or_set_generic_fallback()
        .map_err(|e| anyhow::anyhow!("resolving committer identity: {e}"))?;

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

    // Portage never shallows a volatile repo the user cloned in full — its
    // `sync_depth` only drops to 0 (full fetch) in that one case; a
    // non-volatile repo always gets the shallow treatment.
    let shallow = if volatile { repo.is_shallow() } else { true };

    if !quiet {
        info_line(&format!(
            "gix fetch origin{} in {path}",
            if shallow { " --depth 1" } else { "" }
        ));
    }
    let progress = ProgressSession::new(quiet);
    let fetch_status = {
        let connection = remote
            .connect(gix::remote::Direction::Fetch)
            .map_err(|e| anyhow::anyhow!("connect remote: {e}"))?;
        let mut prepare = connection
            .prepare_fetch(progress.child("negotiate"), Default::default())
            .map_err(|e| anyhow::anyhow!("prepare fetch: {e}"))?;
        if shallow {
            prepare = prepare.with_shallow(sync_shallow());
        }
        let outcome = prepare
            .receive(progress.child("receive"), &gix::interrupt::IS_INTERRUPTED)
            .map_err(|e| anyhow::anyhow!("fetch: {e}"))?;
        outcome.status
    };
    drop(progress);

    let mut repo =
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

    // Non-volatile only: drop orphaned files a prior shallow sync can leave
    // behind, which is the actual failure mode bug #887025 describes. Safe to
    // clobber here because non-volatile means only Portage touches this tree.
    if !volatile {
        if !quiet {
            info_line("gix clean --force -d -x…");
        }
        let removed = gix_ext::clean_worktree(&repo).map_err(|e| anyhow::anyhow!("{e}"))?;
        if removed > 0 && !quiet {
            info_line(&format!(
                "removed {removed} untracked entr{}",
                plural(removed)
            ));
        }
    }

    let guard = if volatile {
        ResetGuard::RefuseIfDirty
    } else {
        ResetGuard::Force
    };
    if !quiet {
        info_line(&format!(
            "gix hard-reset{} to {}…",
            if volatile { " (refuse if dirty)" } else { "" },
            tip.to_hex_with_len(12)
        ));
    }
    let progress = ProgressSession::new(quiet);
    gix_ext::hard_reset_to(&mut repo, tip, guard, progress.child("hard-reset"))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    drop(progress);

    let after = head_id(&repo);
    Ok(before != after)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "y" } else { "ies" }
}

fn head_id(repo: &gix::Repository) -> Option<gix::ObjectId> {
    repo.head_id().ok().map(|id| id.detach())
}

/// Point the default fetch remote at `uri` — but only when it does not
/// already, so a plain sync never rewrites `.git/config` for nothing (matches
/// Portage's `git_remote_url != self.repo.sync_uri` guard).
///
/// The comparison is on parsed [`gix::Url`]s rather than raw strings: gix
/// normalises what it writes back (`file:///x` vs `file://localhost/x`, a
/// trailing slash, …), so a textual compare against `sync-uri` would report a
/// spurious difference and rewrite the config on every single sync.
fn align_remote_url(repo: &gix::Repository, uri: &str) -> Result<()> {
    let name = repo
        .remote_default_name(gix::remote::Direction::Fetch)
        .ok_or_else(|| anyhow::anyhow!("no default remote name"))?
        .to_string();

    let wanted =
        gix::url::parse(uri).map_err(|e| anyhow::anyhow!("invalid sync-uri {uri}: {e}"))?;
    let current = repo
        .find_remote(name.as_str())
        .ok()
        .and_then(|r| r.url(gix::remote::Direction::Fetch).cloned());
    if current.as_ref() == Some(&wanted) {
        return Ok(());
    }

    gix_ext::set_remote_url(repo, &name, uri).map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use std::path::Path;
    use std::process::Command;

    fn git(cwd: &Path, args: &[&str]) {
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
    /// silently ignores `--depth`): the shallow-history bug this backend has
    /// to survive only reproduces against a genuinely shallow clone.
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
        let work = tmp.join("work-advance");
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
        std::fs::remove_dir_all(&work).unwrap();
    }

    fn is_shallow(path: &Utf8Path) -> bool {
        gix::open(path.as_std_path()).unwrap().is_shallow()
    }

    #[test]
    fn clone_creates_a_shallow_checkout() {
        // The fixture spawns `git` via a PATH lookup — serialize against any
        // test elsewhere that temporarily mutates PATH (`crate::test_support`).
        let _path = crate::test_support::path_lock();
        // gix::open/fetch read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        crate::test_support::set_test_git_identity();
        let f = fixture();
        let changed = GixBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();
        assert!(changed);
        assert_eq!(
            std::fs::read_to_string(f.overlay.join("a.txt")).unwrap(),
            "one\n"
        );
        assert!(is_shallow(&f.overlay));
    }

    /// Regression test for the confirmed bug: `hard_reset_to` used to receive
    /// `volatile` as `require_fast_forward`, and a `--depth 1` fetch's new tip
    /// shares no ancestry with the previous shallow history — so the
    /// merge-base check rejected every volatile re-sync. Same failure the
    /// default backend had with `merge --ff-only` (bug #887025).
    #[test]
    fn volatile_update_survives_unrelated_shallow_history() {
        let _path = crate::test_support::path_lock();
        // gix::open/fetch read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        crate::test_support::set_test_git_identity();
        let f = fixture();
        GixBackend
            .sync(&f.overlay, &f.remote_uri, true, true)
            .unwrap();
        advance_remote(&f, "two\n");

        let changed = GixBackend
            .sync(&f.overlay, &f.remote_uri, true, true)
            .expect("volatile re-sync over a shallow history must not fail");
        assert!(changed);
        assert_eq!(
            std::fs::read_to_string(f.overlay.join("a.txt")).unwrap(),
            "two\n"
        );
    }

    /// The protective half of `reset --merge`: a volatile repo with real
    /// uncommitted work must be refused, not clobbered.
    #[test]
    fn volatile_update_refuses_a_dirty_worktree() {
        let _path = crate::test_support::path_lock();
        // gix::open/fetch read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        crate::test_support::set_test_git_identity();
        let f = fixture();
        GixBackend
            .sync(&f.overlay, &f.remote_uri, true, true)
            .unwrap();
        advance_remote(&f, "two\n");
        std::fs::write(f.overlay.join("a.txt"), "local edit\n").unwrap();

        let err = GixBackend
            .sync(&f.overlay, &f.remote_uri, true, true)
            .unwrap_err();
        assert!(
            err.to_string().contains("uncommitted changes"),
            "unexpected error: {err:#}"
        );
        assert_eq!(
            std::fs::read_to_string(f.overlay.join("a.txt")).unwrap(),
            "local edit\n",
            "the local edit must survive the refusal"
        );
    }

    /// Non-volatile means Portage owns the tree: local changes are clobbered
    /// and orphaned untracked files are cleaned before the reset.
    #[test]
    fn nonvolatile_update_cleans_orphans_and_clobbers_local_changes() {
        let _path = crate::test_support::path_lock();
        // gix::open/fetch read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        crate::test_support::set_test_git_identity();
        let f = fixture();
        GixBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();
        advance_remote(&f, "two\n");

        std::fs::write(f.overlay.join("a.txt"), "local edit\n").unwrap();
        std::fs::write(f.overlay.join("orphan.txt"), "stale\n").unwrap();
        std::fs::create_dir_all(f.overlay.join("stale-dir/nested")).unwrap();
        std::fs::write(f.overlay.join("stale-dir/nested/x"), "x\n").unwrap();

        let changed = GixBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();
        assert!(changed);
        assert_eq!(
            std::fs::read_to_string(f.overlay.join("a.txt")).unwrap(),
            "two\n"
        );
        assert!(!f.overlay.join("orphan.txt").exists());
        assert!(!f.overlay.join("stale-dir").exists());
        assert!(f.overlay.join(".git").is_dir(), ".git must survive a clean");
    }

    #[test]
    fn update_is_a_no_op_when_already_at_the_upstream_tip() {
        let _path = crate::test_support::path_lock();
        // gix::open/fetch read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        crate::test_support::set_test_git_identity();
        let f = fixture();
        GixBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();
        let changed = GixBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();
        assert!(!changed, "no upstream change: sync must report unchanged");
    }

    /// Portage never shallows a volatile repo the user cloned in full.
    #[test]
    fn volatile_update_does_not_shallow_a_full_clone() {
        let _path = crate::test_support::path_lock();
        // gix::open/fetch read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        crate::test_support::set_test_git_identity();
        let f = fixture();
        git(
            f._tmp.path(),
            &["clone", "-q", &f.remote_uri, f.overlay.as_str()],
        );
        assert!(!is_shallow(&f.overlay));

        advance_remote(&f, "two\n");
        GixBackend
            .sync(&f.overlay, &f.remote_uri, true, true)
            .unwrap();

        assert!(
            !is_shallow(&f.overlay),
            "a volatile update must not shallow a repo that started full"
        );
        assert_eq!(
            std::fs::read_to_string(f.overlay.join("a.txt")).unwrap(),
            "two\n"
        );
    }

    #[test]
    fn nonvolatile_sync_realigns_remote_url_when_sync_uri_changes() {
        let _path = crate::test_support::path_lock();
        // gix::open/fetch read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        crate::test_support::set_test_git_identity();
        let f = fixture();
        GixBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();

        let mirror = f._tmp.path().join("mirror.git");
        git(
            f._tmp.path(),
            &[
                "clone",
                "-q",
                "--bare",
                f.remote_uri.trim_start_matches("file://"),
                mirror.to_str().unwrap(),
            ],
        );
        let mirror_uri = format!("file://{}", mirror.display());

        GixBackend
            .sync(&f.overlay, &mirror_uri, false, true)
            .unwrap();

        let repo = gix::open(f.overlay.as_std_path()).unwrap();
        let url = repo
            .find_remote("origin")
            .unwrap()
            .url(gix::remote::Direction::Fetch)
            .unwrap()
            .to_bstring()
            .to_string();
        assert!(
            url.contains(mirror.to_str().unwrap()),
            "origin must be realigned to the new sync-uri, got {url}"
        );
    }

    /// The URL-compare guard: an unchanged `sync-uri` must leave
    /// `.git/config` byte-for-byte alone rather than round-tripping it
    /// through gix's remote serializer on every sync.
    #[test]
    fn matching_sync_uri_does_not_rewrite_the_config() {
        let _path = crate::test_support::path_lock();
        // gix::open/fetch read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        crate::test_support::set_test_git_identity();
        let f = fixture();
        GixBackend
            .sync(&f.overlay, &f.remote_uri, false, true)
            .unwrap();

        let config = f.overlay.join(".git/config");
        let before = std::fs::read(config.as_std_path()).unwrap();
        let repo = gix::open(f.overlay.as_std_path()).unwrap();
        align_remote_url(&repo, &f.remote_uri).unwrap();
        assert_eq!(
            std::fs::read(config.as_std_path()).unwrap(),
            before,
            "an unchanged sync-uri must not rewrite .git/config"
        );
    }
}
