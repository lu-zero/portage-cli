//! Pure-gix hard-reset composition.
//!
//! # Algorithm (mirrors `git reset --hard <target>`)
//!
//! 1. Peel `target` to a commit / tree.
//! 2. Move the current branch (or detached `HEAD`) to that commit.
//! 3. Build a fresh index from the target tree.
//! 4. Delete worktree paths that were in the old index but not the new one
//!    (tracked deletions — `git reset --hard` removes them; untracked files
//!    are left alone, same as Git).
//! 5. Force-checkout the new index into the worktree (`overwrite_existing`).
//! 6. Write the new index to disk.
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

use std::path::{Path, PathBuf};

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
    #[error("not a fast-forward (HEAD is not an ancestor of target)")]
    NotFastForward,
    #[error("{0}")]
    Other(String),
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

/// True if `ancestor` is an ancestor of `descendant` (or equal).
///
/// Used for the volatile / `--ff-only` path: only move HEAD when the update
/// is a fast-forward.
pub fn is_ancestor(
    repo: &gix::Repository,
    ancestor: gix::ObjectId,
    descendant: gix::ObjectId,
) -> Result<bool, HardResetError> {
    if ancestor == descendant {
        return Ok(true);
    }
    match repo.merge_base(ancestor, descendant) {
        Ok(base) => Ok(base.detach() == ancestor),
        Err(_) => Ok(false),
    }
}

/// Hard-reset `HEAD`, the index, and the worktree to `target` (a commit-ish).
///
/// When `require_fast_forward` is true, fails with [`HardResetError::NotFastForward`]
/// unless `HEAD` is an ancestor of `target` (volatile / ff-only semantics).
///
/// `progress` receives the force-checkout counters (files / bytes). Pass
/// [`gix::progress::Discard`] when the caller does not care.
pub fn hard_reset_to<P>(
    repo: &gix::Repository,
    target: gix::ObjectId,
    require_fast_forward: bool,
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

    if require_fast_forward {
        let head = repo
            .head_id()
            .map_err(|e| HardResetError::Other(e.to_string()))?
            .detach();
        if !is_ancestor(repo, head, target)? {
            return Err(HardResetError::NotFastForward);
        }
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
    let old_paths: Vec<PathBuf> = match repo.open_index() {
        Ok(idx) => idx
            .entries()
            .iter()
            .filter(|e| e.stage_raw() == 0)
            .map(|e| {
                let p = e.path_in(idx.path_backing());
                workdir.join(gix_path_to_os(p))
            })
            .collect(),
        Err(_) => Vec::new(),
    };

    // 1) Move the current branch (or detached HEAD) to `target`.
    update_head_to(repo, target)?;

    // 2) New index from the target tree.
    let mut index = repo
        .index_from_tree(&tree_id)
        .map_err(|e| HardResetError::IndexFromTree {
            id: tree_id,
            source: Box::new(e),
        })?;

    // 3) Remove worktree files that were tracked before and are gone now.
    let new_paths: std::collections::HashSet<PathBuf> = index
        .entries()
        .iter()
        .filter(|e| e.stage_raw() == 0)
        .map(|e| {
            let p = e.path_in(index.path_backing());
            workdir.join(gix_path_to_os(p))
        })
        .collect();
    for path in old_paths {
        if !new_paths.contains(&path) {
            let _ = remove_path(&path);
        }
    }

    // 4) Force-checkout the new index into the worktree.
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

    // 5) Persist the index.
    index
        .write(Default::default())
        .map_err(|e| HardResetError::IndexWrite(e.to_string()))?;

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

fn gix_path_to_os(path: &BStr) -> PathBuf {
    let s = path.to_str().unwrap_or("");
    PathBuf::from(s)
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

    #[test]
    fn hard_reset_moves_branch_index_and_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let work = tmp.path().join("work");
        git(tmp.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(
            tmp.path(),
            &["clone", remote.to_str().unwrap(), work.to_str().unwrap()],
        );

        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-m", "one"]);
        git(&work, &["push", "origin", "HEAD:master"]);

        let work2 = tmp.path().join("work2");
        git(
            tmp.path(),
            &["clone", remote.to_str().unwrap(), work2.to_str().unwrap()],
        );
        std::fs::write(work2.join("a.txt"), "two\n").unwrap();
        std::fs::write(work2.join("b.txt"), "new\n").unwrap();
        // also remove a path that existed only before? keep a updated
        git(&work2, &["add", "-A"]);
        git(&work2, &["commit", "-m", "two"]);
        git(&work2, &["push", "origin", "HEAD:master"]);

        // Delete b from third commit to test tracked-file removal
        std::fs::remove_file(work2.join("b.txt")).unwrap();
        git(&work2, &["add", "-A"]);
        git(&work2, &["commit", "-m", "three"]);
        git(&work2, &["push", "origin", "HEAD:master"]);

        let repo = gix::open(&work).unwrap();
        let mut remote_gix = repo
            .find_default_remote(gix::remote::Direction::Fetch)
            .unwrap()
            .unwrap();
        remote_gix = remote_gix.with_fetch_tags(gix::remote::fetch::Tags::None);
        let conn = remote_gix.connect(gix::remote::Direction::Fetch).unwrap();
        conn.prepare_fetch(gix::progress::Discard, Default::default())
            .unwrap()
            .receive(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
            .unwrap();

        // After first push work has a; after fetch, tip has a=two, no b.
        // Intermediate push had b then deleted — tip has no b.
        let tip = resolve_upstream_tip(&repo).unwrap();
        hard_reset_to(&repo, tip, false, gix::progress::Discard).unwrap();

        assert_eq!(
            std::fs::read_to_string(work.join("a.txt")).unwrap(),
            "two\n"
        );
        assert!(!work.join("b.txt").exists(), "deleted tracked file removed");
        let head = gix::open(&work).unwrap().head_id().unwrap().detach();
        assert_eq!(head, tip);
    }

    #[test]
    fn ff_only_rejects_diverged_history() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        git(
            tmp.path(),
            &["init", "-b", "master", work.to_str().unwrap()],
        );
        std::fs::write(work.join("a.txt"), "base\n").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-m", "base"]);

        git(&work, &["checkout", "-b", "side"]);
        std::fs::write(work.join("a.txt"), "side\n").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-m", "side"]);
        let side = gix::open(&work).unwrap().head_id().unwrap().detach();

        git(&work, &["checkout", "master"]);
        std::fs::write(work.join("a.txt"), "main\n").unwrap();
        git(&work, &["add", "a.txt"]);
        git(&work, &["commit", "-m", "main"]);

        let repo = gix::open(&work).unwrap();
        let err = hard_reset_to(&repo, side, true, gix::progress::Discard).unwrap_err();
        assert!(matches!(err, HardResetError::NotFastForward));
    }
}
