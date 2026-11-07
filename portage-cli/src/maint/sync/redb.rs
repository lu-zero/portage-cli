//! `sync-type = git-redb`: the repository lives in redb databases next to its location, written by
//! em from git objects; no checkout is updated
//!
//! A repository with no database yet is imported from the git objects of a checkout at its location.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use camino::Utf8Path;
use portage_repo::tree::redb::{ObjectKind, Oid, PathChange, TreeItem, TreeUpdate, TreeWriter};

use super::info_line;
use super::verify::CommitVerifier;

/// Bring the databases of the repository at `location` to the tip of `uri`
pub(super) fn sync(
    location: &Utf8Path,
    uri: &str,
    quiet: bool,
    verify: Option<&CommitVerifier>,
) -> Result<bool> {
    if TreeWriter::open(location)?.is_some() {
        bail!("{location}.redb exists: updating it from {uri} is not supported yet");
    }
    let git_dir = location.join(".git");
    if !git_dir.is_dir() {
        bail!("{location} has no database and no git checkout to import one from");
    }
    let update = import_from_git(&git_dir, uri, quiet, verify)?;
    let commit = hex::encode(update.commit);
    let summary = TreeWriter::import(location, update)
        .with_context(|| format!("importing {git_dir} into {location}.redb"))?;
    if !quiet {
        info_line(&format!(
            "Imported {commit} from {git_dir}: {} files, {} ebuilds, {} md5-cache entries",
            summary.files_added,
            summary.versions_added.len(),
            summary.cache_written
        ));
    }
    Ok(true)
}

fn oid(id: &gix::oid) -> Result<Oid> {
    Oid::try_from(id.as_bytes()).context("not a SHA-1 object id")
}

/// The commit `HEAD` of `git_dir` as an update into an empty database: every entry of its tree
/// added, with the objects they name
fn import_from_git(
    git_dir: &Utf8Path,
    remote: &str,
    quiet: bool,
    verify: Option<&CommitVerifier>,
) -> Result<TreeUpdate> {
    let repo = gix::open(git_dir.as_std_path()).with_context(|| format!("opening {git_dir}"))?;
    let (commit_id, commit_data, commit_time, root) = {
        let commit = repo
            .head_commit()
            .with_context(|| format!("resolving HEAD of {git_dir}"))?;
        let time = commit
            .committer()
            .map(|c| c.time.to_string())
            .context("reading the committer")?;
        let root = commit.tree_id().context("reading the root tree")?.detach();
        (oid(&commit.id)?, commit.data.clone(), time, root)
    };
    let signer = verify
        .map(|v| v.check(&commit_data, &hex::encode(commit_id), quiet))
        .transpose()?;

    let mut update = TreeUpdate {
        from: None,
        commit: commit_id,
        tree: oid(&root)?,
        commit_time,
        remote: remote.to_string(),
        signer,
        ..TreeUpdate::default()
    };
    update
        .objects
        .insert(commit_id, (ObjectKind::Commit, commit_data));

    // A subtree can appear under several paths; each path is an entry of its own.
    let mut trees: HashMap<gix::ObjectId, Vec<u8>> = HashMap::new();
    let mut blobs = Vec::new();
    let mut stack = vec![(String::new(), root)];
    while let Some((prefix, id)) = stack.pop() {
        if let std::collections::hash_map::Entry::Vacant(slot) = trees.entry(id) {
            let tree = repo
                .find_tree(id)
                .with_context(|| format!("reading tree {id}"))?;
            slot.insert(tree.data.clone());
        }
        let body = &trees[&id];
        for entry in gix::objs::TreeRefIter::from_bytes(body, gix::hash::Kind::Sha1) {
            let entry = entry.map_err(|e| anyhow::anyhow!("parsing tree {id}: {e}"))?;
            let name = std::str::from_utf8(entry.filename)
                .with_context(|| format!("non-UTF-8 name in tree {id}"))?;
            let path = format!("{prefix}{name}");
            let item = TreeItem {
                mode: entry.mode.value(),
                oid: oid(entry.oid)?,
            };
            if entry.mode.is_tree() {
                stack.push((format!("{path}/"), entry.oid.to_owned()));
            } else if entry.mode.is_blob_or_symlink() {
                blobs.push(entry.oid.to_owned());
            }
            update.changes.push(PathChange {
                path,
                old: None,
                new: Some(item),
            });
        }
    }
    for (id, body) in trees {
        update.objects.insert(oid(&id)?, (ObjectKind::Tree, body));
    }
    blobs.sort_unstable();
    blobs.dedup();
    let bodies = read_blobs(repo, &blobs)?;
    update.objects.extend(
        bodies
            .into_iter()
            .map(|(id, body)| (id, (ObjectKind::Blob, body))),
    );
    Ok(update)
}

/// The bodies of `ids`, read on every core
fn read_blobs(repo: gix::Repository, ids: &[gix::ObjectId]) -> Result<Vec<(Oid, Vec<u8>)>> {
    let repo = repo.into_sync();
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = ids.len().div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = ids
            .chunks(chunk)
            .map(|part| {
                let repo = &repo;
                scope.spawn(move || -> Result<Vec<_>> {
                    let repo = repo.to_thread_local();
                    part.iter()
                        .map(|id| {
                            let mut blob = repo
                                .find_blob(*id)
                                .with_context(|| format!("reading blob {id}"))?;
                            Ok((oid(id)?, blob.take_data()))
                        })
                        .collect()
                })
            })
            .collect();
        let mut out = Vec::with_capacity(ids.len());
        for handle in handles {
            match handle.join() {
                Ok(part) => out.extend(part?),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use camino::Utf8PathBuf;
    use portage_repo::tree::redb::RedbTree;
    use portage_repo::tree::{EntryKind, TreeSource};

    use super::*;

    fn git(cwd: &Utf8Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn a_checkout_imports_every_path_of_its_head() {
        let dir = tempfile::tempdir().unwrap();
        let location = Utf8PathBuf::from_path_buf(dir.path().join("repo")).unwrap();
        for (path, text) in [
            ("app-misc/foo/foo-1.ebuild", "EAPI=8\n"),
            ("metadata/md5-cache/app-misc/foo-1", "EAPI=8\n_md5_=x\n"),
            ("profiles/categories", "app-misc\n"),
            ("a/run", "#!/bin/sh\n"),
            ("b/run", "#!/bin/sh\n"),
        ] {
            let file = location.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, text).unwrap();
        }
        use std::os::unix::fs::PermissionsExt as _;
        for dir in ["a", "b"] {
            let run = location.join(dir).join("run");
            std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::os::unix::fs::symlink("run", location.join(dir).join("link")).unwrap();
        }
        git(&location, &["init", "-q"]);
        git(&location, &["add", "."]);
        git(&location, &["commit", "-q", "-m", "c"]);

        assert!(sync(&location, "file:///nowhere", true, None).unwrap());
        let tree = RedbTree::open(format!("{location}.redb")).unwrap();
        let read =
            |p: &str| String::from_utf8(tree.read(Utf8Path::new(p)).unwrap().unwrap()).unwrap();
        // `a` and `b` are one tree object under two paths: both are walked.
        assert_eq!(read("b/run"), "#!/bin/sh\n");
        assert_eq!(read("b/link"), "run");
        let mut names = tree.list(Utf8Path::new("a")).unwrap().unwrap();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            names,
            [
                ("link".into(), EntryKind::File),
                ("run".into(), EntryKind::File)
            ]
        );
        assert_eq!(
            tree.ebuild_index().unwrap().unwrap(),
            [Utf8PathBuf::from("app-misc/foo/foo-1.ebuild")]
        );
        let copies = dir.path().join("copies");
        let root = tree
            .checkout(
                &[Utf8Path::new("b")],
                Some(Utf8Path::from_path(&copies).unwrap()),
            )
            .unwrap();
        let mode = std::fs::metadata(root.join("b/run"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "the executable bit survives the import"
        );
    }
}
