//! Pure-gix hard-reset composition.
//!
//! # Algorithm (mirrors `git reset --hard <target>`)
//!
//! 1. Check the [guard](ResetGuard): [`ResetGuard::RefuseIfDirty`] bails out
//!    up front if the worktree has uncommitted modifications.
//! 2. Peel `target` to a commit / tree.
//! 3. Build a fresh index from the target tree.
//! 4. Delete worktree paths that were in the old index but not the new one
//!    (tracked deletions — `git reset --hard` removes them; untracked files
//!    are left alone, same as Git; use [`clean_worktree`] for `git clean`).
//! 5. Force-checkout the new index into the worktree (`overwrite_existing`).
//! 6. Write the new index to disk.
//! 7. **Last**, move the current branch (or detached `HEAD`) to the target.
//!
//! Step 7 is deliberately last, exactly as real `git reset --hard` orders it.
//! Moving the ref first would mean a checkout that fails halfway (ENOSPC,
//! EACCES, an interrupt) leaves `HEAD` at the new tip over a stale/partial
//! worktree — and `git_gix`'s "HEAD already at tip → nothing to do" fast path
//! would then skip the repair on every subsequent sync, silently freezing the
//! corruption in place. With the ref moved last, a failed reset leaves `HEAD`
//! where it was, so the next sync retries and heals the tree.
//!
//! This is the orchestration gap listed as unfinished in gitoxide's
//! `crate-status.md` (*checkout, switch, restore and reset*). Plumbing used:
//! [`gix::Repository::index_from_tree`], [`gix_worktree_state::checkout`],
//! [`gix::Reference::set_target_id`] / [`gix::Repository::reference`].
//!
//! ## Upstream proposal sketch
//!
//! ```text
//! // proposed for gix (crate-status.md: "reset orchestration")
//! impl Repository {
//!     pub fn reset(
//!         &self,
//!         target: impl Into<ObjectId>,
//!         mode: reset::Mode, // Soft | Mixed | Hard
//!     ) -> Result<(), reset::Error>;
//! }
//! ```
//!
//! Landed in `em` first so we can dogfood Portage sync and offer the module
//! (or a cleaned-up PR) to gitoxide with a real-world caller.

use std::path::{Component, Path, PathBuf};

use gix::Progress;
use gix::bstr::{BStr, ByteSlice};
use gix::refs::transaction::PreviousValue;
use gix::worktree::stack::state::attributes;

/// Errors from [`hard_reset_to`] and friends.
#[derive(Debug, thiserror::Error)]
pub enum HardResetError {
    #[error("repository has no worktree (bare)")]
    BareRepository,
    #[error("failed to resolve object {id}: {source}")]
    Resolve {
        id: gix::ObjectId,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("failed to update ref: {0}")]
    RefEdit(String),
    #[error("failed to build index from tree {id}: {source}")]
    IndexFromTree {
        id: gix::ObjectId,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("checkout failed: {0}")]
    Checkout(String),
    #[error("failed to write index: {0}")]
    IndexWrite(String),
    #[error("failed to resolve upstream tip: {0}")]
    Upstream(String),
    #[error(
        "worktree has uncommitted changes; refusing to reset (commit, stash or discard them, \
         or set volatile = no in repos.conf to let em clobber this repo)"
    )]
    DirtyWorktree,
    #[error("failed to inspect worktree status: {0}")]
    Status(String),
    #[error("failed to walk the worktree: {0}")]
    Dirwalk(String),
    #[error("{0}")]
    Other(String),
}

/// How much a [`hard_reset_to`] is allowed to clobber.
///
/// The two variants mirror the two shapes Portage's git sync module uses (see
/// `portage.sync.modules.git.git.GitSync.update`):
///
/// | Portage | `volatile` | here |
/// |---------|------------|------|
/// | `git clean -d -x` + `git reset --hard` | `no` | [`ResetGuard::Force`] |
/// | `git reset --merge` | `yes` | [`ResetGuard::RefuseIfDirty`] |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetGuard {
    /// Reset unconditionally — the repository is Portage's to manage
    /// (`volatile = no`) and local modifications are expendable.
    Force,
    /// Reset only when the worktree has no uncommitted modifications,
    /// otherwise fail with [`HardResetError::DirtyWorktree`].
    ///
    /// This is our stand-in for `git reset --merge`, which is what Portage
    /// runs on a volatile repo. Two properties matter and both hold here:
    ///
    /// - it does **not** require `HEAD` to be an ancestor of the target. A
    ///   `--depth 1` fetch's new tip routinely shares no ancestry at all with
    ///   the previous shallow history, which is exactly why Portage cannot
    ///   use `git merge --ff-only` (bug #887025 / bug #824782). An ancestry
    ///   check here would break every volatile re-sync the same way.
    /// - it refuses rather than destroying uncommitted local work.
    ///
    /// ### Deviation from `git reset --merge`
    ///
    /// Real `reset --merge` is finer-grained: it keeps local modifications to
    /// files whose content does not differ between `HEAD` and the target, and
    /// only aborts when a *conflicting* path would be lost. Reproducing that
    /// needs a full three-way index/worktree merge, which gix has no porcelain
    /// for. We refuse on *any* dirty worktree instead. That is strictly more
    /// conservative — it never loses data `reset --merge` would have kept, it
    /// only declines some resets `reset --merge` would have performed. For an
    /// ebuild repository (which nobody is expected to hand-edit) declining
    /// with an actionable message is the right trade; the default `git_cmd`
    /// backend still gets the exact `reset --merge` behaviour.
    RefuseIfDirty,
}

/// Resolve the commit `HEAD` tracks on its upstream remote (`@{upstream}`).
pub fn resolve_upstream_tip(repo: &gix::Repository) -> Result<gix::ObjectId, HardResetError> {
    let head = repo
        .head()
        .map_err(|e| HardResetError::Upstream(e.to_string()))?;
    let branch = head
        .try_into_referent()
        .ok_or_else(|| HardResetError::Upstream("HEAD is detached; no upstream".into()))?;
    let tracking = branch
        .remote_tracking_ref_name(gix::remote::Direction::Fetch)
        .ok_or_else(|| {
            HardResetError::Upstream(format!(
                "branch {} has no remote-tracking name configured",
                branch.name().as_bstr()
            ))
        })?
        .map_err(|e| HardResetError::Upstream(e.to_string()))?;
    let mut tracking_ref = repo
        .find_reference(tracking.as_ref())
        .map_err(|e| HardResetError::Upstream(e.to_string()))?;
    let id = tracking_ref
        .peel_to_id()
        .map_err(|e| HardResetError::Upstream(e.to_string()))?
        .detach();
    Ok(id)
}

/// Remove untracked **and** ignored worktree entries — `git clean --force -d -x`.
///
/// Portage runs this before a non-volatile `reset --hard` because a shallow
/// fetch can leave orphaned files behind that block the next sync (see the
/// long comment block in `portage.sync.modules.git.git.GitSync.update`, bug
/// #887025). Returns the number of entries removed.
///
/// Only reachable via a [`ResetGuard::Force`]-style caller: it deletes files
/// git does not track, so it must never run against a volatile repository.
pub fn clean_worktree(repo: &gix::Repository) -> Result<usize, HardResetError> {
    use gix::dir::entry::Status;
    use gix::dir::walk::{EmissionMode, ForDeletionMode};

    let workdir = repo
        .workdir()
        .ok_or(HardResetError::BareRepository)?
        .to_owned();
    let index = repo
        .index_or_empty()
        .map_err(|e| HardResetError::Dirwalk(e.to_string()))?;

    let options = repo
        .dirwalk_options()
        .map_err(|e| HardResetError::Dirwalk(e.to_string()))?
        .emit_tracked(false)
        .emit_untracked(EmissionMode::CollapseDirectory)
        // `-x`: ignored files too, which is what makes this a *clean* slate.
        .emit_ignored(Some(EmissionMode::CollapseDirectory))
        // `-d`: leaf directories that hold nothing else.
        .emit_empty_directories(true)
        // Required for a deletion walk so ignored directories cannot hide a
        // nested repository we would otherwise remove wholesale.
        .for_deletion(Some(
            ForDeletionMode::FindNonBareRepositoriesInIgnoredDirectories,
        ))
        .recurse_repositories(false);

    let iter = repo
        .dirwalk_iter(
            index,
            Vec::<gix::bstr::BString>::new(),
            (&gix::interrupt::IS_INTERRUPTED).into(),
            options,
        )
        .map_err(|e| HardResetError::Dirwalk(e.to_string()))?;

    let mut removed = 0usize;
    for item in iter {
        let item = item.map_err(|e| HardResetError::Dirwalk(e.to_string()))?;
        if !matches!(item.entry.status, Status::Untracked | Status::Ignored(_)) {
            continue;
        }
        let rela = item.entry.rela_path.as_bstr();
        // The walk classifies `.git` as `Pruned` (never emitted here), but a
        // deletion loop is not the place to rely on that alone.
        if rela == ".git" || rela.starts_with(b".git/") {
            continue;
        }
        let Some(path) = worktree_path(&workdir, rela) else {
            continue;
        };
        if remove_path(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Hard-reset `HEAD`, the index, and the worktree to `target` (a commit-ish).
///
/// `guard` decides whether uncommitted worktree changes are clobbered
/// ([`ResetGuard::Force`]) or make the reset fail with
/// [`HardResetError::DirtyWorktree`] ([`ResetGuard::RefuseIfDirty`]). Note
/// that neither variant requires `HEAD` to be an ancestor of `target` — see
/// [`ResetGuard::RefuseIfDirty`] for why an ancestry check would be wrong.
///
/// `progress` receives the force-checkout counters (files / bytes). Pass
/// [`gix::progress::Discard`] when the caller does not care.
pub fn hard_reset_to<P>(
    repo: &gix::Repository,
    target: gix::ObjectId,
    guard: ResetGuard,
    mut progress: P,
) -> Result<(), HardResetError>
where
    P: gix::NestedProgress,
    P::SubProgress: gix::NestedProgress + 'static,
{
    let workdir = repo
        .workdir()
        .ok_or(HardResetError::BareRepository)?
        .to_owned();

    if guard == ResetGuard::RefuseIfDirty
        && repo
            .is_dirty()
            .map_err(|e| HardResetError::Status(e.to_string()))?
    {
        return Err(HardResetError::DirtyWorktree);
    }

    let tree_id = {
        let obj = repo
            .find_object(target)
            .map_err(|e| HardResetError::Resolve {
                id: target,
                source: Box::new(e),
            })?;
        obj.peel_to_tree()
            .map_err(|e| HardResetError::Resolve {
                id: target,
                source: Box::new(e),
            })?
            .id
    };

    // Snapshot old index paths so we can delete tracked files that vanish.
    // `skipped` counts entries whose recorded path could not be turned into a
    // worktree path we are willing to delete (see `worktree_path`).
    let mut skipped = 0usize;
    let old_paths: Vec<PathBuf> = match repo.open_index() {
        Ok(idx) => idx
            .entries()
            .iter()
            .filter(|e| e.stage_raw() == 0)
            .filter_map(|e| {
                let p = e.path_in(idx.path_backing());
                let joined = worktree_path(&workdir, p);
                if joined.is_none() {
                    skipped += 1;
                }
                joined
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    if skipped > 0 {
        progress.info(format!(
            "{skipped} index entr{} had a path that cannot be safely mapped into the worktree \
             and will not be removed",
            if skipped == 1 { "y" } else { "ies" }
        ));
    }

    // 1) New index from the target tree.
    let mut index = repo
        .index_from_tree(&tree_id)
        .map_err(|e| HardResetError::IndexFromTree {
            id: tree_id,
            source: Box::new(e),
        })?;

    // 2) Remove worktree files that were tracked before and are gone now.
    // Done before the checkout so a path that changes shape (file `foo` →
    // directory `foo/`, or the reverse) is out of the way first.
    let new_paths: std::collections::HashSet<PathBuf> = index
        .entries()
        .iter()
        .filter(|e| e.stage_raw() == 0)
        .filter_map(|e| {
            let p = e.path_in(index.path_backing());
            worktree_path(&workdir, p)
        })
        .collect();
    for path in old_paths {
        if !new_paths.contains(&path) {
            let _ = remove_path(&path);
        }
    }

    // 3) Force-checkout the new index into the worktree.
    let mut opts = repo
        .checkout_options(attributes::Source::IdMapping)
        .map_err(|e| HardResetError::Checkout(e.to_string()))?;
    opts.overwrite_existing = true;
    opts.destination_is_initially_empty = false;

    let objects = repo
        .objects
        .clone()
        .into_arc()
        .map_err(|e| HardResetError::Checkout(e.to_string()))?;

    let mut files = progress.add_child("checkout");
    let mut bytes = progress.add_child("writing");
    files.init(Some(index.entries().len()), gix::progress::count("files"));
    bytes.init(None, gix::progress::bytes());

    gix::worktree::state::checkout(
        &mut index,
        &workdir,
        objects,
        &files,
        &bytes,
        &gix::interrupt::IS_INTERRUPTED,
        opts,
    )
    .map_err(|e| HardResetError::Checkout(e.to_string()))?;

    // 4) Persist the index.
    index
        .write(Default::default())
        .map_err(|e| HardResetError::IndexWrite(e.to_string()))?;

    // 5) Only now move the branch / detached HEAD — see the module docs: a
    // ref moved ahead of a failed checkout is invisible corruption.
    update_head_to(repo, target)?;

    Ok(())
}

/// Set `remote.<name>.url` by rewriting the on-disk git config with gix's
/// remote serializer (no `git` binary).
pub fn set_remote_url(
    repo: &gix::Repository,
    remote_name: &str,
    url: &str,
) -> Result<(), HardResetError> {
    let fetch_specs: Vec<String> = {
        let old = repo
            .find_remote(remote_name)
            .map_err(|e| HardResetError::Other(e.to_string()))?;
        old.refspecs(gix::remote::Direction::Fetch)
            .iter()
            .map(|s| s.to_ref().to_bstring().to_string())
            .collect()
    };

    let mut remote = repo
        .remote_at(url)
        .map_err(|e| HardResetError::Other(e.to_string()))?;
    if !fetch_specs.is_empty() {
        remote
            .replace_refspecs(
                fetch_specs.iter().map(|s| s.as_str()),
                gix::remote::Direction::Fetch,
            )
            .map_err(|e| HardResetError::Other(e.to_string()))?;
    }

    // Open the repo config file independently so we don't fight `Remote`'s
    // borrow of `repo` (see `Remote::save_as_to`).
    let config_path = repo.git_dir().join("config");
    let mut file =
        gix::config::File::from_path_no_includes(config_path.clone(), gix::config::Source::Local)
            .map_err(|e| HardResetError::Other(e.to_string()))?;
    remote
        .save_as_to(remote_name, &mut file)
        .map_err(|e| HardResetError::Other(e.to_string()))?;
    let bytes = file.to_bstring();
    std::fs::write(&config_path, bytes.as_slice())
        .map_err(|e| HardResetError::Other(e.to_string()))?;
    Ok(())
}

fn update_head_to(repo: &gix::Repository, target: gix::ObjectId) -> Result<(), HardResetError> {
    let head = repo
        .head()
        .map_err(|e| HardResetError::RefEdit(e.to_string()))?;

    if let Some(mut branch) = head.try_into_referent() {
        // Prefer set_target_id on a direct branch ref; fall back to force-update.
        if branch.set_target_id(target, "em sync: hard reset").is_err() {
            repo.reference(
                branch.name().to_owned(),
                target,
                PreviousValue::Any,
                "em sync: hard reset",
            )
            .map_err(|e| HardResetError::RefEdit(e.to_string()))?;
        }
        Ok(())
    } else {
        repo.reference("HEAD", target, PreviousValue::Any, "em sync: hard reset")
            .map_err(|e| HardResetError::RefEdit(e.to_string()))?;
        Ok(())
    }
}

/// Join a repo-relative git path onto `workdir`, or `None` if it must not be
/// used as a deletion target.
///
/// Git paths are arbitrary bytes on Unix, not UTF-8, so
/// [`gix::path::try_from_bstr`] is used instead of a lossy `to_str()`
/// (which is infallible on Unix and only rejects ill-formed UTF-16 on
/// Windows). The component check then rules out anything that could escape
/// or collide with the worktree root:
///
/// - an empty path — `workdir.join("")` compares **equal to `workdir`**, so
///   the "tracked file vanished" loop would have handed the whole repository,
///   `.git` and all, to `remove_dir_all`. This is not hypothetical: the
///   previous `to_str().unwrap_or("")` produced exactly that for any
///   non-UTF-8 entry.
/// - an absolute path, or one containing `..`/`.`/a root or prefix
///   component — a real index never holds these, so a path that does is
///   either corrupt or hostile, and either way not something to delete.
///
/// Anything surviving the filter has at least one `Normal` component, so the
/// result is always strictly below `workdir`.
fn worktree_path(workdir: &Path, rela_path: &BStr) -> Option<PathBuf> {
    if rela_path.is_empty() {
        return None;
    }
    let rel = gix::path::try_from_bstr(rela_path).ok()?;
    if rel.is_absolute() {
        return None;
    }
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(workdir.join(rel))
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// A repo with one commit on `master`, plus a second commit available on
    /// `other` that changes `a.txt` and drops `b.txt`.
    fn repo_with_two_commits(tmp: &Path) -> (std::path::PathBuf, gix::ObjectId) {
        let work = tmp.join("work");
        git(tmp, &["init", "-q", "-b", "master", work.to_str().unwrap()]);
        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        std::fs::write(work.join("b.txt"), "gone-later\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "one"]);

        git(&work, &["checkout", "-q", "-b", "other"]);
        std::fs::write(work.join("a.txt"), "two\n").unwrap();
        std::fs::remove_file(work.join("b.txt")).unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "two"]);
        let target = gix::open(&work).unwrap().head_id().unwrap().detach();
        git(&work, &["checkout", "-q", "master"]);
        (work, target)
    }

    #[test]
    fn hard_reset_moves_branch_index_and_worktree() {
        let _path = crate::test_support::path_lock();
        // gix::open/reset read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        let tmp = tempfile::tempdir().unwrap();
        let (work, target) = repo_with_two_commits(tmp.path());

        let repo = gix::open(&work).unwrap();
        hard_reset_to(&repo, target, ResetGuard::Force, gix::progress::Discard).unwrap();

        assert_eq!(
            std::fs::read_to_string(work.join("a.txt")).unwrap(),
            "two\n"
        );
        assert!(!work.join("b.txt").exists(), "deleted tracked file removed");
        assert_eq!(
            gix::open(&work).unwrap().head_id().unwrap().detach(),
            target
        );
        // The index must agree with the worktree, or `git status` (and our own
        // `is_dirty` guard on the next sync) would report phantom changes.
        assert!(!gix::open(&work).unwrap().is_dirty().unwrap());
    }

    /// `ResetGuard::RefuseIfDirty` must *not* imply the old fast-forward
    /// check: `master` and `other` here have a common ancestor, but a shallow
    /// re-sync's tip routinely does not, and refusing on that basis broke
    /// every volatile update (bug #887025). Only worktree cleanliness matters.
    #[test]
    fn refuse_if_dirty_allows_a_non_fast_forward_when_the_tree_is_clean() {
        let _path = crate::test_support::path_lock();
        // gix::open/reset read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        git(
            tmp.path(),
            &["init", "-q", "-b", "master", work.to_str().unwrap()],
        );
        std::fs::write(work.join("a.txt"), "base\n").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-q", "-m", "base"]);

        // A completely unrelated root commit — no merge base at all, exactly
        // what a `--depth 1` fetch of a moved shallow boundary looks like.
        git(&work, &["checkout", "-q", "--orphan", "unrelated"]);
        git(&work, &["rm", "-q", "-rf", "."]);
        std::fs::write(work.join("c.txt"), "unrelated\n").unwrap();
        git(&work, &["add", "c.txt"]);
        git(&work, &["commit", "-q", "-m", "unrelated"]);
        let unrelated = gix::open(&work).unwrap().head_id().unwrap().detach();
        git(&work, &["checkout", "-q", "-f", "master"]);

        let repo = gix::open(&work).unwrap();
        assert!(
            repo.merge_base(repo.head_id().unwrap().detach(), unrelated)
                .is_err(),
            "fixture must have no common ancestor"
        );

        hard_reset_to(
            &repo,
            unrelated,
            ResetGuard::RefuseIfDirty,
            gix::progress::Discard,
        )
        .expect("unrelated history must not block a clean-worktree reset");
        assert_eq!(
            std::fs::read_to_string(work.join("c.txt")).unwrap(),
            "unrelated\n"
        );
        assert!(!work.join("a.txt").exists());
    }

    #[test]
    fn refuse_if_dirty_rejects_uncommitted_changes_and_changes_nothing() {
        let _path = crate::test_support::path_lock();
        // gix::open/reset read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        let tmp = tempfile::tempdir().unwrap();
        let (work, target) = repo_with_two_commits(tmp.path());
        let head_before = gix::open(&work).unwrap().head_id().unwrap().detach();
        std::fs::write(work.join("a.txt"), "local edit\n").unwrap();

        let repo = gix::open(&work).unwrap();
        let err = hard_reset_to(
            &repo,
            target,
            ResetGuard::RefuseIfDirty,
            gix::progress::Discard,
        )
        .unwrap_err();
        assert!(matches!(err, HardResetError::DirtyWorktree));
        assert_eq!(
            std::fs::read_to_string(work.join("a.txt")).unwrap(),
            "local edit\n"
        );
        assert_eq!(
            gix::open(&work).unwrap().head_id().unwrap().detach(),
            head_before
        );
    }

    #[test]
    fn force_clobbers_uncommitted_changes() {
        let _path = crate::test_support::path_lock();
        // gix::open/reset read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        let tmp = tempfile::tempdir().unwrap();
        let (work, target) = repo_with_two_commits(tmp.path());
        std::fs::write(work.join("a.txt"), "local edit\n").unwrap();

        let repo = gix::open(&work).unwrap();
        hard_reset_to(&repo, target, ResetGuard::Force, gix::progress::Discard).unwrap();
        assert_eq!(
            std::fs::read_to_string(work.join("a.txt")).unwrap(),
            "two\n"
        );
    }

    /// The confirmed `remove_dir_all(workdir)` bug: any index path that does
    /// not map to a real, strictly-nested worktree path — an empty one above
    /// all, which is what the old `to_str().unwrap_or("")` produced for every
    /// non-UTF-8 entry — must be skipped, never joined onto `workdir`.
    #[test]
    fn worktree_path_never_resolves_to_the_workdir_or_outside_it() {
        let workdir = Path::new("/var/db/repos/gentoo");
        for bad in [
            &b""[..],
            b".",
            b"..",
            b"../../etc/passwd",
            b"/etc/passwd",
            b"sub/../..",
        ] {
            let got = worktree_path(workdir, bad.as_bstr());
            assert!(
                got.is_none(),
                "{:?} must not map to a deletion target, got {got:?}",
                bad.as_bstr()
            );
        }

        // Non-UTF-8 is legal on Unix and must map through byte-for-byte
        // rather than collapsing to the empty path.
        #[cfg(unix)]
        {
            let raw = b"metadata/\xff\xfeinvalid";
            let got = worktree_path(workdir, raw.as_bstr()).expect("valid Unix path");
            assert!(got.starts_with(workdir));
            assert_ne!(got, workdir);
            use std::os::unix::ffi::OsStrExt as _;
            assert_eq!(got.file_name().unwrap().as_bytes(), b"\xff\xfeinvalid");
        }

        let good = worktree_path(workdir, b"sys-apps/portage/Manifest".as_bstr()).unwrap();
        assert_eq!(good, workdir.join("sys-apps/portage/Manifest"));
    }

    /// Guards the ordering fix: HEAD must only move once the checkout and the
    /// index write have both succeeded. A directory the checkout cannot create
    /// a new entry in stands in for ENOSPC/EACCES/an interrupt. (The target
    /// must *add* a file, not just modify one — modifying an existing file
    /// needs no write permission on its parent directory.)
    #[test]
    #[cfg(unix)]
    fn head_does_not_move_when_the_checkout_fails() {
        use std::os::unix::fs::PermissionsExt as _;

        let _path = crate::test_support::path_lock();
        // gix::open/reset read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        // A test running as root would write through the mode bits and
        // falsify the setup rather than the fix.
        if rustix::process::geteuid().is_root() {
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        git(
            tmp.path(),
            &["init", "-q", "-b", "master", work.to_str().unwrap()],
        );
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::write(work.join("sub/keep.txt"), "keep\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "one"]);

        git(&work, &["checkout", "-q", "-b", "other"]);
        std::fs::write(work.join("sub/added.txt"), "added\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "two"]);
        let target = gix::open(&work).unwrap().head_id().unwrap().detach();
        git(&work, &["checkout", "-q", "-f", "master"]);
        assert!(!work.join("sub/added.txt").exists());

        let head_before = gix::open(&work).unwrap().head_id().unwrap().detach();
        let sub = work.join("sub");
        let saved = std::fs::metadata(&sub).unwrap().permissions();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o500)).unwrap();

        let repo = gix::open(&work).unwrap();
        let result = hard_reset_to(&repo, target, ResetGuard::Force, gix::progress::Discard);

        std::fs::set_permissions(&sub, saved).unwrap();

        assert!(result.is_err(), "checkout into a read-only dir must fail");
        assert_eq!(
            gix::open(&work).unwrap().head_id().unwrap().detach(),
            head_before,
            "HEAD must not move ahead of a failed checkout — the gix backend's \
             'already at tip' fast path would otherwise never retry the repair"
        );
    }

    #[test]
    fn clean_worktree_removes_untracked_and_ignored_but_keeps_git_and_tracked() {
        let _path = crate::test_support::path_lock();
        // gix::open/reset read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        git(
            tmp.path(),
            &["init", "-q", "-b", "master", work.to_str().unwrap()],
        );
        std::fs::write(work.join(".gitignore"), "ignored/\n*.log\n").unwrap();
        std::fs::write(work.join("tracked.txt"), "keep\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "one"]);

        std::fs::write(work.join("orphan.txt"), "stale\n").unwrap();
        std::fs::write(work.join("build.log"), "ignored\n").unwrap();
        std::fs::create_dir_all(work.join("ignored/deep")).unwrap();
        std::fs::write(work.join("ignored/deep/x"), "x\n").unwrap();
        std::fs::create_dir_all(work.join("untracked-dir")).unwrap();
        std::fs::write(work.join("untracked-dir/y"), "y\n").unwrap();

        let repo = gix::open(&work).unwrap();
        let removed = clean_worktree(&repo).unwrap();
        assert!(removed >= 4, "expected 4+ removals, got {removed}");

        assert!(!work.join("orphan.txt").exists());
        assert!(
            !work.join("build.log").exists(),
            "-x must drop ignored files"
        );
        assert!(!work.join("ignored").exists());
        assert!(!work.join("untracked-dir").exists(), "-d must drop dirs");
        assert!(work.join("tracked.txt").exists());
        assert!(work.join(".gitignore").exists());
        assert!(work.join(".git").is_dir(), ".git must never be cleaned");
    }

    #[test]
    fn set_remote_url_rewrites_the_url_and_keeps_the_refspec() {
        let _path = crate::test_support::path_lock();
        // gix::open/reset read the ambient $HOME (~/.gitconfig config
        // layering) even though this test never touches HOME itself --
        // path_lock alone doesn't exclude a concurrently-running test that
        // does (active.rs/cli.rs's home_lock-protected `set_var("HOME", ..)`
        // tests are a *different* mutex). See test_support::home_lock's doc.
        let _home = crate::test_support::home_lock();
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        git(
            tmp.path(),
            &["init", "-q", "-b", "master", work.to_str().unwrap()],
        );
        git(&work, &["remote", "add", "origin", "https://example/a.git"]);

        let repo = gix::open(&work).unwrap();
        set_remote_url(&repo, "origin", "https://example/b.git").unwrap();

        let repo = gix::open(&work).unwrap();
        let remote = repo.find_remote("origin").unwrap();
        assert_eq!(
            remote
                .url(gix::remote::Direction::Fetch)
                .unwrap()
                .to_bstring()
                .to_string(),
            "https://example/b.git"
        );
        assert!(
            !remote.refspecs(gix::remote::Direction::Fetch).is_empty(),
            "the fetch refspec must survive the rewrite"
        );
    }
}
