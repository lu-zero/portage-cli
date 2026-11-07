//! `sync-type = git-redb`: the repository lives in redb databases next to its location, written by
//! em from git objects; no checkout is updated
//!
//! A repository with no database yet is imported from the git objects of a checkout at its location,
//! or fetched whole without one; after that each sync fetches a thin pack against the stored commit
//! and applies the tree diff.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use camino::Utf8Path;
use portage_repo::tree::redb::{
    ObjectKind, ObjectReader, Oid, PathChange, TreeItem, TreeUpdate, TreeWriter,
};

use super::info_line;
use super::verify::CommitVerifier;

mod fetch;
mod pack;

/// Bring the databases of the repository at `location` to the tip of `uri`
pub(super) fn sync(
    location: &Utf8Path,
    uri: &str,
    quiet: bool,
    verify: Option<&CommitVerifier>,
) -> Result<bool> {
    if let Some(mut writer) = TreeWriter::open(location)? {
        let from = writer
            .head()?
            .with_context(|| format!("{location}.redb holds no commit"))?;
        let stored = writer.objects()?;
        let Some(update) = fetch_update(uri, Some((from, &stored)), quiet, verify)? else {
            if !quiet {
                info_line(&format!("Already at {}", hex::encode(from.0)));
            }
            return Ok(false);
        };
        drop(stored);
        let (head, tip) = (hex::encode(from.0), hex::encode(update.commit));
        let summary = writer
            .apply(update)
            .with_context(|| format!("updating {location}.redb"))?;
        if !quiet {
            info_line(&format!(
                "Updated {}..{}: {} ebuilds added, {} changed, {} removed",
                &head[..12],
                &tip[..12],
                summary.versions_added.len(),
                summary.versions_modified.len(),
                summary.versions_removed.len()
            ));
        }
        return Ok(true);
    }
    let git_dir = location.join(".git");
    let (update, source) = if git_dir.is_dir() {
        (
            import_from_git(&git_dir, uri, quiet, verify)?,
            git_dir.to_string(),
        )
    } else {
        let update = fetch_update(uri, None, quiet, verify)?
            .with_context(|| format!("{uri} sent nothing"))?;
        (update, uri.to_string())
    };
    let commit = hex::encode(update.commit);
    let summary = TreeWriter::import(location, update)
        .with_context(|| format!("importing {source} into {location}.redb"))?;
    if !quiet {
        info_line(&format!(
            "Imported {commit} from {source}: {} files, {} ebuilds, {} md5-cache entries",
            summary.files_added,
            summary.versions_added.len(),
            summary.cache_written
        ));
    }
    Ok(true)
}

/// The update from the stored commit and tree in `from` (none: an empty database) to the tip of
/// `uri`; `None` when the database is already there
fn fetch_update(
    uri: &str,
    from: Option<((Oid, Oid), &ObjectReader)>,
    quiet: bool,
    verify: Option<&CommitVerifier>,
) -> Result<Option<TreeUpdate>> {
    let fetched = fetch::fetch(uri, from.map(|((head, _), _)| gix::ObjectId::from(head)))?;
    if fetched.pack.is_empty() {
        // An unchanged remote is not evidence that its stored tip satisfies today's policy:
        // verification may have been enabled, the keyring replaced, or a key expired since import.
        if let Some(verifier) = verify {
            let ((head, _), stored) = from.context("an empty pack without a stored tip")?;
            let Some((ObjectKind::Commit, commit)) = stored.get(&head)? else {
                bail!(
                    "the database does not hold its tip commit {}",
                    hex::encode(head)
                );
            };
            verifier.check(&commit, &hex::encode(head), quiet)?;
        }
        return Ok(None);
    }
    let tip = oid(&fetched.tip)?;
    let stored = from.map(|(_, stored)| stored);
    let received = pack::resolve(
        &fetched.pack,
        Overlay {
            received: None,
            stored,
        },
    )?;
    drop(fetched);
    let (signer, tree, commit_time) = {
        let Some((ObjectKind::Commit, commit)) = received.get(&tip) else {
            bail!(
                "the pack from {uri} does not hold its tip {}",
                hex::encode(tip)
            );
        };
        let signer = verify
            .map(|v| v.check(commit, &hex::encode(tip), quiet))
            .transpose()?;
        let parsed = gix::objs::CommitRef::from_bytes(commit, gix::hash::Kind::Sha1)
            .map_err(|e| anyhow::anyhow!("parsing commit {}: {e}", hex::encode(tip)))?;
        let time = parsed
            .committer()
            .map(|c| c.time.to_string())
            .map_err(|e| anyhow::anyhow!("reading the committer of {}: {e}", hex::encode(tip)))?;
        (signer, oid(&parsed.tree())?, time)
    };
    let overlay = Overlay {
        received: Some(&received),
        stored,
    };
    let changes = diff(&overlay, from.map(|((_, tree), _)| tree).as_ref(), &tree)?;
    Ok(Some(TreeUpdate {
        from: from.map(|((head, _), _)| head),
        commit: tip,
        tree,
        commit_time,
        remote: uri.to_string(),
        signer,
        changes,
        objects: received,
    }))
}

/// Objects for gix: those received first, then the database's
struct Overlay<'a> {
    received: Option<&'a pack::Objects>,
    stored: Option<&'a ObjectReader>,
}

fn gix_kind(kind: ObjectKind) -> gix::objs::Kind {
    match kind {
        ObjectKind::Commit => gix::objs::Kind::Commit,
        ObjectKind::Tree => gix::objs::Kind::Tree,
        ObjectKind::Blob => gix::objs::Kind::Blob,
        ObjectKind::Tag => gix::objs::Kind::Tag,
    }
}

impl gix::objs::Find for Overlay<'_> {
    fn try_find<'b>(
        &self,
        id: &gix::oid,
        buffer: &'b mut Vec<u8>,
    ) -> gix::error::Result<Option<gix::objs::Data<'b>>> {
        let Ok(key) = Oid::try_from(id.as_bytes()) else {
            return Ok(None);
        };
        let found = match self.received.and_then(|r| r.get(&key)) {
            Some((kind, body)) => Some((*kind, body.clone())),
            None => match self.stored {
                Some(stored) => stored.get(&key).map_err(gix::error::Error::from_error)?,
                None => None,
            },
        };
        Ok(found.map(|(kind, body)| {
            *buffer = body;
            gix::objs::Data {
                kind: gix_kind(kind),
                object_hash: gix::hash::Kind::Sha1,
                data: buffer.as_slice(),
            }
        }))
    }
}

/// Every entry that differs between the trees `old` (none: the empty tree) and `new`, directories
/// included
fn diff(objects: &Overlay<'_>, old: Option<&Oid>, new: &Oid) -> Result<Vec<PathChange>> {
    use gix::diff::tree::recorder::Change;
    use gix::objs::Find as _;

    let body = |id: &Oid| -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        let found = objects
            .try_find(&gix::ObjectId::from(*id), &mut buf)
            .map_err(|e| anyhow::anyhow!("reading tree {}: {e}", hex::encode(id)))?;
        found
            .with_context(|| format!("tree {} is neither received nor stored", hex::encode(id)))?;
        Ok(buf)
    };
    let old_body = old.map(body).transpose()?.unwrap_or_default();
    let new_body = body(new)?;
    let mut recorder = gix::diff::tree::Recorder::default();
    gix::diff::tree(
        gix::objs::TreeRefIter::from_bytes(&old_body, gix::hash::Kind::Sha1),
        gix::objs::TreeRefIter::from_bytes(&new_body, gix::hash::Kind::Sha1),
        gix::diff::tree::State::default(),
        objects,
        &mut recorder,
    )
    .map_err(|e| anyhow::anyhow!("diffing the trees: {e}"))?;
    let item = |mode: gix::objs::tree::EntryMode, id: &gix::oid| -> Result<TreeItem> {
        Ok(TreeItem {
            mode: mode.value(),
            oid: oid(id)?,
        })
    };
    recorder
        .records
        .into_iter()
        .map(|change| {
            let (path, old, new) = match change {
                Change::Addition {
                    entry_mode,
                    oid,
                    path,
                    ..
                } => (path, None, Some(item(entry_mode, &oid)?)),
                Change::Deletion {
                    entry_mode,
                    oid,
                    path,
                    ..
                } => (path, Some(item(entry_mode, &oid)?), None),
                Change::Modification {
                    previous_entry_mode,
                    previous_oid,
                    entry_mode,
                    oid,
                    path,
                } => (
                    path,
                    Some(item(previous_entry_mode, &previous_oid)?),
                    Some(item(entry_mode, &oid)?),
                ),
            };
            let path = String::from_utf8(path.into()).context("a non-UTF-8 path in the tree")?;
            Ok(PathChange { path, old, new })
        })
        .collect()
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

    fn write_files(root: &Utf8Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, text).unwrap();
        }
    }

    /// Every file of the database at `location`, by path
    fn files(location: &Utf8Path) -> Vec<(String, String)> {
        fn walk(tree: &RedbTree, dir: &str, out: &mut Vec<(String, String)>) {
            for (name, kind) in tree.list(Utf8Path::new(dir)).unwrap().unwrap() {
                let path = if dir.is_empty() {
                    name
                } else {
                    format!("{dir}/{name}")
                };
                match kind {
                    EntryKind::Dir => walk(tree, &path, out),
                    EntryKind::File => {
                        let body = tree.read(Utf8Path::new(&path)).unwrap().unwrap();
                        out.push((path, String::from_utf8(body).unwrap()));
                    }
                }
            }
        }
        let tree = RedbTree::open(format!("{location}.redb")).unwrap();
        let mut out = Vec::new();
        walk(&tree, "", &mut out);
        out.sort();
        out
    }

    #[test]
    fn a_sync_brings_the_database_to_the_remote_tip() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let origin = root.join("origin");
        write_files(
            &origin,
            &[
                ("app-misc/foo/foo-1.ebuild", "foo 1"),
                ("app-misc/bar/bar-1.ebuild", "bar 1"),
                ("eclass/e.eclass", "e 1"),
                ("metadata/md5-cache/app-misc/foo-1", "foo entry"),
                ("profiles/categories", "app-misc\n"),
            ],
        );
        git(&origin, &["init", "-q"]);
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-q", "-m", "1"]);
        let url = format!("file://{origin}");
        let location = root.join("repo");
        git(&root, &["clone", "-q", &url, location.as_str()]);
        assert!(sync(&location, &url, true, None).unwrap(), "imported");
        assert!(
            !sync(&location, &url, true, None).unwrap(),
            "already at the tip"
        );

        write_files(
            &origin,
            &[
                ("app-misc/foo/foo-1.ebuild", "foo 1 edited"),
                ("app-misc/foo/foo-2.ebuild", "foo 2"),
                ("dev-libs/new/new-1.ebuild", "new 1"),
                ("metadata/md5-cache/app-misc/foo-2", "foo 2 entry"),
            ],
        );
        git(&origin, &["rm", "-q", "-r", "app-misc/bar"]);
        git(&origin, &["mv", "eclass/e.eclass", "eclass/moved.eclass"]);
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-q", "-m", "2"]);
        assert!(sync(&location, &url, true, None).unwrap(), "updated");

        // No checkout: the whole tip is fetched instead.
        let fresh = root.join("fresh");
        assert!(sync(&fresh, &url, true, None).unwrap());
        assert!(!fresh.exists());
        let synced = files(&location);
        assert_eq!(synced, files(&fresh));
        assert!(synced.contains(&("eclass/moved.eclass".into(), "e 1".into())));
        assert!(
            !synced
                .iter()
                .any(|(path, _)| path.starts_with("app-misc/bar"))
        );
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

    #[test]
    fn an_unchanged_tip_still_has_to_pass_the_current_verifier() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let origin = root.join("origin");
        write_files(&origin, &[("profiles/repo_name", "test\n")]);
        git(&origin, &["init", "-q"]);
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-q", "-m", "unsigned"]);
        let url = format!("file://{origin}");
        let location = root.join("repo");
        assert!(sync(&location, &url, true, None).unwrap());
        let verifier = CommitVerifier::gentoo_for_tests();
        let result = sync(&location, &url, true, Some(&verifier));
        assert!(
            result.is_err(),
            "enabling verification must reject the unsigned tip: {result:?}"
        );
        assert!(!sync(&location, &url, true, None).unwrap());
    }
}
