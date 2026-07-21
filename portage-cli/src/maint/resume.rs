//! `-r`/`--resume` persisted state + `em maint cleanresume`.
//!
//! Mirrors real portage's own mechanism (`portage/util/mtimedb.py`,
//! `_emerge/actions.py`, `_emerge/Scheduler.py::_save_resume_list`), read
//! directly rather than guessed, but simplified for `em`'s architecture:
//! portage persists a fully-resolved `mergelist` (`[pkg_type, pkg_root, cpv,
//! action]` tuples) and prunes it entry-by-entry as packages complete. `em`
//! instead persists just enough to **replay the original invocation** — the
//! raw atoms (pre-`@set`-expansion) and the effective merge/depgraph flags —
//! and lets a fresh dependency resolution run again on `-r`. Already-merged
//! packages are skipped for free by the existing VDB-presence check in
//! `merge::merge_sequential`/`merge_parallel`, so there is no need to track
//! per-package completion separately, and the replay is self-healing if the
//! repo changed between runs (no stale pinned list to invalidate).
//!
//! One level of backup, matching portage's `resume`/`resume_backup` pair:
//! starting a *new* (non-`-r`) top-level merge while a `resume` entry is
//! still pending backs it up first, so an unrelated command doesn't
//! silently discard an interrupted job. `-r` consults `resume` first,
//! falling back to (and promoting) `resume_backup` if `resume` is absent —
//! see [`take_for_resume`].
//!
//! Stored at `<root>/var/cache/edb/em-resume.json` — the same directory
//! real portage's own `mtimedb` lives in, but a distinct filename and JSON
//! shape (own format, never reads/writes portage's actual `mtimedb`, so a
//! shared ROOT can't have one corrupt the other's state). Unlike portage's
//! single central file (which can span multiple ROOTs in one `mergelist`,
//! needing its own stale-root filtering), this file's *path* is already
//! root-scoped, so there's no cross-root ambiguity to store or validate —
//! the same convention `maint::world`'s `world_path` already uses.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::cli::{DepgraphFlags, MergeFlags};
use crate::util::write_atomic;

/// Everything needed to replay a top-level `em <atoms>` invocation.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ResumeState {
    /// The original atoms/`@set` refs, before expansion — so a replay
    /// re-expands `@world`/`@system` fresh rather than reusing a
    /// potentially-stale expansion.
    pub atoms: Vec<String>,
    #[serde(default)]
    pub merge_flags: MergeFlags,
    #[serde(default)]
    pub depgraph_flags: DepgraphFlags,
    #[serde(default)]
    pub nodeps: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct ResumeFile {
    resume: Option<ResumeState>,
    resume_backup: Option<ResumeState>,
}

fn resume_path(root: &Utf8Path) -> Utf8PathBuf {
    root.join("var/cache/edb/em-resume.json")
}

/// Read the resume file, treating anything missing or unparseable as empty
/// — matches portage's own `MtimeDB._load`, which discards bad content
/// rather than hard-failing (a corrupt resume file shouldn't block an
/// ordinary merge from proceeding).
fn read(root: &Utf8Path) -> ResumeFile {
    std::fs::read_to_string(resume_path(root))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write(root: &Utf8Path, file: &ResumeFile) -> Result<()> {
    let path = resume_path(root);
    let json = serde_json::to_string_pretty(file).context("serializing resume state")?;
    write_atomic(&path, json)
}

/// Persist `state` as the current pending run, right before a real merge
/// starts (after `--pretend`/`--ask` have already been resolved).
///
/// `is_resume`: this call is itself replaying a previous `-r` — same
/// logical job, so `resume_backup` is left untouched. When `false` (an
/// ordinary fresh top-level invocation), any existing `resume` entry is
/// preserved as `resume_backup` first, matching portage's supersede rule.
pub fn save(root: &Utf8Path, state: ResumeState, is_resume: bool) -> Result<()> {
    let mut file = read(root);
    if !is_resume && let Some(prev) = file.resume.take() {
        file.resume_backup = Some(prev);
    }
    file.resume = Some(state);
    write(root, &file)
}

/// Clear the current `resume` entry after a fully successful run.
/// `resume_backup` (if any) is left untouched — matches portage, which only
/// prunes `resume`'s own mergelist on success.
pub fn clear(root: &Utf8Path) {
    let mut file = read(root);
    if file.resume.take().is_some() {
        let _ = write(root, &file);
    }
}

/// Load the state to replay for `-r`/`--resume`: `resume` if present, else
/// `resume_backup` — promoted (removed from backup, persisted as the live
/// `resume`) to match portage's own promotion-on-`--resume` behavior.
/// `Ok(None)` if neither slot is occupied.
pub fn take_for_resume(root: &Utf8Path) -> Result<Option<ResumeState>> {
    let mut file = read(root);
    if let Some(state) = file.resume.clone() {
        return Ok(Some(state));
    }
    let Some(backup) = file.resume_backup.take() else {
        return Ok(None);
    };
    file.resume = Some(backup.clone());
    write(root, &file)?;
    Ok(Some(backup))
}

/// `em maint cleanresume`: report which slots are occupied (and how many
/// atoms each holds); with `fix`, delete both. Backs the CLI stub that
/// already existed for this (`MaintCommand::Cleanresume`).
pub fn cleanresume(root: &Utf8Path, fix: bool) -> Result<Vec<String>> {
    let mut file = read(root);
    let mut messages = Vec::new();
    for (name, slot) in [
        ("resume", file.resume.as_ref()),
        ("resume_backup", file.resume_backup.as_ref()),
    ] {
        if let Some(s) = slot {
            let n = s.atoms.len();
            messages.push(format!(
                "{name} list contains {n} atom{}",
                if n == 1 { "" } else { "s" }
            ));
        }
    }
    if fix && (file.resume.take().is_some() | file.resume_backup.take().is_some()) {
        write(root, &file)?;
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(atoms: &[&str]) -> ResumeState {
        ResumeState {
            atoms: atoms.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn save_and_take_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        save(&root, state(&["app-misc/foo"]), false).unwrap();
        let loaded = take_for_resume(&root).unwrap().unwrap();

        assert_eq!(loaded.atoms, vec!["app-misc/foo".to_string()]);
    }

    #[test]
    fn take_for_resume_is_none_when_nothing_saved() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        assert!(take_for_resume(&root).unwrap().is_none());
    }

    #[test]
    fn fresh_save_backs_up_a_still_pending_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        save(&root, state(&["app-misc/old"]), false).unwrap();
        // A second, unrelated fresh (non-resume) invocation supersedes it.
        save(&root, state(&["app-misc/new"]), false).unwrap();

        let file = read(&root);
        assert_eq!(file.resume.unwrap().atoms, vec!["app-misc/new".to_string()]);
        assert_eq!(
            file.resume_backup.unwrap().atoms,
            vec!["app-misc/old".to_string()]
        );
    }

    #[test]
    fn resume_replay_save_does_not_touch_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        save(&root, state(&["app-misc/old"]), false).unwrap();
        save(&root, state(&["app-misc/orig"]), false).unwrap(); // old -> backup
        // Replaying that same job (is_resume=true) must not disturb backup.
        save(&root, state(&["app-misc/orig"]), true).unwrap();

        let file = read(&root);
        assert_eq!(
            file.resume_backup.unwrap().atoms,
            vec!["app-misc/old".to_string()]
        );
    }

    #[test]
    fn take_for_resume_promotes_backup_when_resume_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        save(&root, state(&["app-misc/old"]), false).unwrap();
        save(&root, state(&["app-misc/new"]), false).unwrap();
        clear(&root); // "new" finished successfully; "old" remains backed up.

        let loaded = take_for_resume(&root).unwrap().unwrap();
        assert_eq!(loaded.atoms, vec!["app-misc/old".to_string()]);

        // Promotion must have cleared the backup slot.
        let file = read(&root);
        assert!(file.resume_backup.is_none());
        assert_eq!(file.resume.unwrap().atoms, vec!["app-misc/old".to_string()]);
    }

    #[test]
    fn clear_leaves_backup_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        save(&root, state(&["app-misc/old"]), false).unwrap();
        save(&root, state(&["app-misc/new"]), false).unwrap();
        clear(&root);

        let file = read(&root);
        assert!(file.resume.is_none());
        assert_eq!(
            file.resume_backup.unwrap().atoms,
            vec!["app-misc/old".to_string()]
        );
    }

    #[test]
    fn cleanresume_reports_occupied_slots_and_fix_clears_both() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        save(&root, state(&["app-misc/old"]), false).unwrap();
        save(&root, state(&["app-misc/new", "app-misc/other"]), false).unwrap();

        let report = cleanresume(&root, false).unwrap();
        assert_eq!(report.len(), 2);
        assert!(report.iter().any(|m| m.contains("resume list contains 2")));
        assert!(
            report
                .iter()
                .any(|m| m.contains("resume_backup list contains 1"))
        );

        // check-only must not have cleared anything.
        assert!(take_for_resume(&root).unwrap().is_some());
        save(&root, state(&["app-misc/new", "app-misc/other"]), true).unwrap();

        cleanresume(&root, true).unwrap();
        assert!(take_for_resume(&root).unwrap().is_none());
    }

    #[test]
    fn missing_file_behaves_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        assert!(cleanresume(&root, false).unwrap().is_empty());
    }

    #[test]
    fn corrupt_file_behaves_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        std::fs::create_dir_all(root.join("var/cache/edb")).unwrap();
        std::fs::write(resume_path(&root), "not valid json").unwrap();

        assert!(take_for_resume(&root).unwrap().is_none());
    }
}
