//! `-r`/`--resume` persisted state + `em maint cleanresume`
//!
//! Mirrors real portage's own mechanism (`portage/util/mtimedb.py`,
//! `_emerge/actions.py`, `_emerge/Scheduler.py::_save_resume_list`), read
//! directly rather than guessed, but simplified for `em`'s architecture:
//! portage persists a fully-resolved `mergelist` and prunes it entry-by-entry.
//! `em` instead persists just enough to **replay the original invocation** —
//! the raw atoms (pre-`@set`-expansion) and the effective merge/depgraph flags
//! — and lets a fresh dependency resolution run again on `-r`.
//!
//! ## Layout
//!
//! | Path | Role |
//! |------|------|
//! | `<root>/var/cache/edb/em-resume.json` | Job *shape* (atoms + flags + `job_id`) for the live and backup slots |
//! | `<root>/var/cache/edb/em-resume.done/<job_id>/…` | Completion **markers** — one empty file per finished package |
//!
//! Completions are **not** rewritten into the JSON. Each successful package
//! creates a single marker file under its job id (`create_new`, idempotent).
//! Parallel `--jobs N` merges never contend on a shared read-modify-write of
//! one file; they only create distinct paths. The JSON is touched only at
//! job start / supersede / promote / clear — single-threaded emerge paths.
//!
//! Marker layout for a finished target package `sys-libs/glibc-2.39-r1`:
//! `…/em-resume.done/<job_id>/target/sys-libs/glibc-2.39-r1`.
//!
//! That is what makes resume work under `--emptytree`: VDB presence alone
//! cannot mean "done for this job", because an emptytree rebuild starts with
//! those CPVs already installed. On `-r`, marker keys are dropped from the
//! re-resolved plan (preview and merge agree).
//!
//! One level of backup, matching portage's `resume`/`resume_backup` pair:
//! starting a *new* (non-`-r`) top-level merge while a `resume` entry is
//! still pending backs it up first. The backed-up entry keeps its own
//! `job_id`, so its markers stay on disk until `cleanresume --fix` or the
//! job is eventually cleared. See [`take_for_resume`].

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::cli::{DepgraphFlags, MergeFlags};
use crate::query::depgraph::MergeRoot;
use crate::util::write_atomic;

/// Legacy JSON-embedded completion entry (pre-marker layout)
///
/// Still deserialised so an interrupted job from an older build can resume once; never
/// written back.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
struct LegacyCompletedPkg {
    merge_root: String,
    cpv: String,
}

fn merge_root_key(merge_root: MergeRoot) -> &'static str {
    match merge_root {
        MergeRoot::Host => "host",
        MergeRoot::Base => "base",
        MergeRoot::Target => "target",
    }
}

fn parse_merge_root(s: &str) -> Option<MergeRoot> {
    match s {
        "host" => Some(MergeRoot::Host),
        "base" => Some(MergeRoot::Base),
        "target" => Some(MergeRoot::Target),
        _ => None,
    }
}

/// Everything needed to replay a top-level `em <atoms>` invocation
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
    /// Directory name under `em-resume.done/` for this job's markers
    /// Assigned on first [`save`] of a fresh job; preserved across `-r`.
    #[serde(default)]
    pub job_id: String,
    /// Pre-marker builds stored completions here
    ///
    /// Read for migration only; after migration the vec is empty so this field is omitted on
    /// write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    completed: Vec<LegacyCompletedPkg>,
}

impl ResumeState {
    /// Fresh job shape for [`save`] — `job_id` is assigned there
    pub fn new(
        atoms: Vec<String>,
        merge_flags: MergeFlags,
        depgraph_flags: DepgraphFlags,
        nodeps: bool,
    ) -> Self {
        Self {
            atoms,
            merge_flags,
            depgraph_flags,
            nodeps,
            job_id: String::new(),
            completed: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct ResumeFile {
    resume: Option<ResumeState>,
    resume_backup: Option<ResumeState>,
}

fn resume_path(root: &Utf8Path) -> Utf8PathBuf {
    root.join("var/cache/edb/em-resume.json")
}

fn done_root(root: &Utf8Path) -> Utf8PathBuf {
    root.join("var/cache/edb/em-resume.done")
}

fn job_done_dir(root: &Utf8Path, job_id: &str) -> Utf8PathBuf {
    done_root(root).join(job_id)
}

/// Marker file for one finished package
///
/// CPV `cat/pf` becomes `…/<job_id>/<host|target>/<cat>/<pf>`.
fn marker_path(root: &Utf8Path, job_id: &str, merge_root: MergeRoot, cpv: &str) -> Utf8PathBuf {
    let (cat, pf) = cpv.split_once('/').unwrap_or(("_", cpv));
    job_done_dir(root, job_id)
        .join(merge_root_key(merge_root))
        .join(cat)
        .join(pf)
}

fn new_job_id() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{t:x}-{}", std::process::id())
}

/// Read the resume file, treating anything missing or unparseable as empty
/// — matches portage's own `MtimeDB._load`, which discards bad content
/// rather than hard-failing (a corrupt resume file shouldn't block an
/// ordinary merge from proceeding).
fn read_file(root: &Utf8Path) -> ResumeFile {
    std::fs::read_to_string(resume_path(root))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_file(root: &Utf8Path, file: &ResumeFile) -> Result<()> {
    let path = resume_path(root);
    let json = serde_json::to_string_pretty(file).context("serializing resume state")?;
    write_atomic(&path, json)
}

fn rm_job_markers(root: &Utf8Path, job_id: &str) {
    if job_id.is_empty() {
        return;
    }
    let dir = job_done_dir(root, job_id);
    let _ = std::fs::remove_dir_all(dir.as_std_path());
}

fn rm_all_markers(root: &Utf8Path) {
    let _ = std::fs::remove_dir_all(done_root(root).as_std_path());
}

/// Ensure `state.job_id` is set; migrate any legacy JSON `completed` list
/// into marker files for that id.
fn ensure_job_id(root: &Utf8Path, state: &mut ResumeState) {
    if state.job_id.is_empty() {
        state.job_id = new_job_id();
    }
    if !state.completed.is_empty() {
        for c in std::mem::take(&mut state.completed) {
            let Some(mr) = parse_merge_root(&c.merge_root) else {
                continue;
            };
            // Best-effort migration — don't fail the whole save if one marker
            // cannot be written (e.g. bad legacy path chars).
            let _ = mark_completed(root, &state.job_id, mr, &c.cpv);
        }
    }
}

/// Strip UI-only flags that must never be restored from a saved job
///
/// `ask` / `tree` / `json` describe how *this* invocation should present
/// itself, not the job's merge shape. Persisting them made `-r` re-prompt
/// or re-emit tree/json purely because the original run used `-a`/`--tree`.
fn strip_ephemeral_ui_flags(flags: &mut MergeFlags) {
    flags.ask = false;
    flags.tree = false;
    flags.json = false;
}

/// Persist `state` as the current pending run, right before a real merge
/// starts (after `--pretend`/`--ask` have already been resolved).
///
/// Returns the job id under which completion markers will be written —
/// pass it to [`mark_completed`] / the merge loop.
///
/// `is_resume`: this call is itself replaying a previous `-r` — same
/// logical job, so `resume_backup` is left untouched and the existing
/// `job_id` (and its markers) are preserved. When `false` (a fresh
/// top-level invocation), any existing `resume` entry is preserved as
/// `resume_backup` first, matching portage's supersede rule, and a new
/// `job_id` is assigned (empty marker set).
pub fn save(root: &Utf8Path, mut state: ResumeState, is_resume: bool) -> Result<String> {
    strip_ephemeral_ui_flags(&mut state.merge_flags);
    let mut file = read_file(root);
    if is_resume {
        // Keep the live job's id (and therefore its markers). Caller usually
        // rebuilds atoms/flags from the overlay without a job_id.
        if let Some(prev) = &file.resume {
            if state.job_id.is_empty() {
                state.job_id = prev.job_id.clone();
            }
            // Migrate once if the previous build only had JSON completions.
            if state.completed.is_empty() && !prev.completed.is_empty() {
                state.completed = prev.completed.clone();
            }
        }
        ensure_job_id(root, &mut state);
    } else {
        if let Some(prev) = file.resume.take() {
            file.resume_backup = Some(prev);
            // Markers for the superseded job stay under its own job_id so a
            // later backup promotion can still see them; the new job gets a
            // fresh id.
        }
        state.job_id = new_job_id();
        state.completed.clear();
    }
    let job_id = state.job_id.clone();
    file.resume = Some(state);
    write_file(root, &file)?;
    Ok(job_id)
}

/// Record that one plan entry finished successfully by creating its marker file
///
/// Concurrent-safe for distinct packages under `--jobs N` (each path is independent).
/// Idempotent if the marker already exists.
///
/// `job_id` comes from [`save`]'s return value — do not re-read the JSON on
/// every package.
pub fn mark_completed(
    root: &Utf8Path,
    job_id: &str,
    merge_root: MergeRoot,
    cpv: &str,
) -> Result<()> {
    if job_id.is_empty() {
        return Ok(());
    }
    let path = marker_path(root, job_id, merge_root, cpv);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent.as_std_path())
            .with_context(|| format!("creating resume marker dir {parent}"))?;
    }
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.as_std_path())
    {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).with_context(|| format!("creating resume marker {path}")),
    }
}

/// Completed `(MergeRoot, cpv)` keys for filtering a re-resolved plan —
/// markers for the live job, plus any still-unmigrated legacy JSON list.
pub fn completed_keys(root: &Utf8Path) -> HashSet<(MergeRoot, String)> {
    let file = read_file(root);
    let Some(state) = file.resume.as_ref() else {
        return HashSet::new();
    };
    let mut keys = read_markers(root, &state.job_id);
    for c in &state.completed {
        if let Some(mr) = parse_merge_root(&c.merge_root) {
            keys.insert((mr, c.cpv.clone()));
        }
    }
    keys
}

fn read_markers(root: &Utf8Path, job_id: &str) -> HashSet<(MergeRoot, String)> {
    let mut keys = HashSet::new();
    if job_id.is_empty() {
        return keys;
    }
    let base = job_done_dir(root, job_id);
    for side in [MergeRoot::Host, MergeRoot::Base, MergeRoot::Target] {
        let side_dir = base.join(merge_root_key(side));
        let Ok(cats) = std::fs::read_dir(side_dir.as_std_path()) else {
            continue;
        };
        for cat_ent in cats.flatten() {
            let cat_path = cat_ent.path();
            if !cat_path.is_dir() {
                continue;
            }
            let Some(cat) = cat_ent.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(pfs) = std::fs::read_dir(&cat_path) else {
                continue;
            };
            for pf_ent in pfs.flatten() {
                if !pf_ent.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let Some(pf) = pf_ent.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                keys.insert((side, format!("{cat}/{pf}")));
            }
        }
    }
    keys
}

fn completed_count(root: &Utf8Path, state: &ResumeState) -> usize {
    let n = read_markers(root, &state.job_id).len();
    if n > 0 { n } else { state.completed.len() }
}

/// Clear the current `resume` entry after a fully successful run
///
/// Removes that job's marker directory. `resume_backup` (if any) is left
/// untouched — matches portage, which only prunes `resume` on success.
pub fn clear(root: &Utf8Path) {
    let mut file = read_file(root);
    if let Some(prev) = file.resume.take() {
        rm_job_markers(root, &prev.job_id);
        let _ = write_file(root, &file);
    }
}

/// Load the state to replay for `-r`/`--resume`: `resume` if present, else
/// `resume_backup` — promoted (removed from backup, persisted as the live
/// `resume`) to match portage's own promotion-on-`--resume` behavior.
/// `Ok(None)` if neither slot is occupied.
pub fn take_for_resume(root: &Utf8Path) -> Result<Option<ResumeState>> {
    let mut file = read_file(root);
    if let Some(state) = file.resume.clone() {
        return Ok(Some(state));
    }
    let Some(backup) = file.resume_backup.take() else {
        return Ok(None);
    };
    file.resume = Some(backup.clone());
    write_file(root, &file)?;
    Ok(Some(backup))
}

/// Merge a *saved* job's flags with the current `-r` invocation
///
/// Semantics (intentionally different from
/// [`crate::crossdev::merge_merge_flags_fields`]'s subcommand-vs-global OR):
///
/// - **Job shape** bools start from `saved`; a `true` on `cli` turns them
///   on (clap cannot express "explicitly false" for these flags, so `-r`
///   can only *add* shape flags — e.g. `-r --keep-going`). To change the
///   job shape in a way that needs a flag *off*, clear the list and
///   re-invoke (`em maint cleanresume --fix`).
///
/// - **Ephemeral UI** (`ask` / `tree` / `json`) comes only from `cli` —
///   never restored from the saved job.
/// - **`jobs` / `load_average`**: current `Some` wins, else saved.
/// - **`exclude`**: current `-X` values are *unioned* into the saved list
///   (so `-r -X stuck/atom` adds a skip without dropping prior excludes).
pub fn merge_resume_flags(saved: &MergeFlags, cli: &MergeFlags) -> MergeFlags {
    let mut exclude = saved.exclude.clone();
    for atom in &cli.exclude {
        if !exclude.iter().any(|e| e == atom) {
            exclude.push(atom.clone());
        }
    }
    MergeFlags {
        // Ephemeral — current invocation only.
        ask: cli.ask,
        eta: cli.eta,
        tree: cli.tree,
        json: cli.json,
        // Job shape — saved base, CLI can only add.
        update: saved.update || cli.update,
        autounmask_write: saved.autounmask_write || cli.autounmask_write,
        oneshot: saved.oneshot || cli.oneshot,
        fetchonly: saved.fetchonly || cli.fetchonly,
        fetch_all_uri: saved.fetch_all_uri || cli.fetch_all_uri,
        buildpkg: saved.buildpkg || cli.buildpkg,
        buildpkgonly: saved.buildpkgonly || cli.buildpkgonly,
        usepkg: saved.usepkg || cli.usepkg,
        usepkgonly: saved.usepkgonly || cli.usepkgonly,
        getbinpkg: saved.getbinpkg || cli.getbinpkg,
        getbinpkgonly: saved.getbinpkgonly || cli.getbinpkgonly,
        emptytree: saved.emptytree || cli.emptytree,
        onlydeps: saved.onlydeps || cli.onlydeps,
        noreplace: saved.noreplace || cli.noreplace,
        keep_going: saved.keep_going || cli.keep_going,
        autounmask: saved.autounmask || cli.autounmask,
        autosolve_use: saved.autosolve_use || cli.autosolve_use,
        complete_graph: saved.complete_graph || cli.complete_graph,
        with_bdeps: saved.with_bdeps || cli.with_bdeps,
        root_deps: saved.root_deps || cli.root_deps,
        jobs: cli.jobs.or(saved.jobs),
        load_average: cli.load_average.or(saved.load_average),
        exclude,
    }
}

/// `em maint cleanresume`: report which slots are occupied (atoms, flags
/// summary, completion progress); with `fix`, delete both slots and all
/// marker trees. Backs the CLI stub that already existed for this
/// (`MaintCommand::Cleanresume`).
pub fn cleanresume(root: &Utf8Path, fix: bool) -> Result<Vec<String>> {
    let mut file = read_file(root);
    let mut messages = Vec::new();

    for (name, slot) in [
        ("resume", file.resume.as_ref()),
        ("resume_backup", file.resume_backup.as_ref()),
    ] {
        if let Some(s) = slot {
            messages.push(format_slot_report(root, name, s));
        }
    }

    let had_any = file.resume.is_some() || file.resume_backup.is_some();
    if fix && had_any {
        file.resume = None;
        file.resume_backup = None;
        write_file(root, &file)?;
        // Drop every job's markers — both live and anything left from older
        // supersedes that never got a clean promotion path.
        rm_all_markers(root);
        messages.push("Cleared saved resume list(s).".to_string());
    }
    Ok(messages)
}

fn format_slot_report(root: &Utf8Path, name: &str, s: &ResumeState) -> String {
    let n = s.atoms.len();
    let atom_word = if n == 1 { "atom" } else { "atoms" };
    let preview = match n {
        0 => String::new(),
        1 => format!(" ({})", s.atoms[0]),
        2 => format!(" ({}, {})", s.atoms[0], s.atoms[1]),
        _ => format!(" ({}, {}, …)", s.atoms[0], s.atoms[1]),
    };
    let mut extras = Vec::new();
    if s.merge_flags.emptytree {
        extras.push("emptytree".to_string());
    }
    if s.merge_flags.keep_going {
        extras.push("keep-going".to_string());
    }
    if s.merge_flags.update {
        extras.push("update".to_string());
    }
    if s.depgraph_flags.deep {
        extras.push("deep".to_string());
    }
    if s.nodeps {
        extras.push("nodeps".to_string());
    }
    if let Some(j) = s.merge_flags.jobs {
        extras.push(format!("jobs={j}"));
    }
    if !s.merge_flags.exclude.is_empty() {
        extras.push(format!("exclude={}", s.merge_flags.exclude.len()));
    }
    let flags = if extras.is_empty() {
        String::new()
    } else {
        format!(" [{}]", extras.join(", "))
    };
    let done = completed_count(root, s);
    let progress = if done > 0 {
        format!("; {done} completed")
    } else {
        String::new()
    };
    format!("{name}: {n} {atom_word}{preview}{flags}{progress}")
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
        assert!(!loaded.job_id.is_empty());
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

        let file = read_file(&root);
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

        let file = read_file(&root);
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
        let file = read_file(&root);
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

        let file = read_file(&root);
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
        assert!(
            report
                .iter()
                .any(|m| m.starts_with("resume:") && m.contains("2 atoms"))
        );
        assert!(
            report
                .iter()
                .any(|m| m.starts_with("resume_backup:") && m.contains("1 atom"))
        );

        // check-only must not have cleared anything.
        assert!(take_for_resume(&root).unwrap().is_some());
        save(&root, state(&["app-misc/new", "app-misc/other"]), true).unwrap();

        let fixed = cleanresume(&root, true).unwrap();
        assert!(fixed.iter().any(|m| m.contains("Cleared")));
        assert!(take_for_resume(&root).unwrap().is_none());
        assert!(!done_root(&root).exists());
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

    #[test]
    fn mark_completed_uses_markers_not_json_and_survives_resave() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        let job_id = save(&root, state(&["@system"]), false).unwrap();
        mark_completed(&root, &job_id, MergeRoot::Target, "sys-libs/glibc-2.39-r1").unwrap();
        mark_completed(&root, &job_id, MergeRoot::Host, "dev-lang/python-3.12.0").unwrap();
        // Idempotent.
        mark_completed(&root, &job_id, MergeRoot::Target, "sys-libs/glibc-2.39-r1").unwrap();

        // Markers are on disk — not inside the JSON.
        assert!(marker_path(&root, &job_id, MergeRoot::Target, "sys-libs/glibc-2.39-r1").exists());
        let file = read_file(&root);
        assert!(file.resume.unwrap().completed.is_empty());

        let keys = completed_keys(&root);
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&(MergeRoot::Target, "sys-libs/glibc-2.39-r1".into())));
        assert!(keys.contains(&(MergeRoot::Host, "dev-lang/python-3.12.0".into())));

        // A resume re-save of atoms/flags must not wipe markers.
        let job_id2 = save(&root, state(&["@system"]), true).unwrap();
        assert_eq!(job_id, job_id2);
        assert_eq!(completed_keys(&root).len(), 2);
    }

    #[test]
    fn parallel_style_marks_do_not_need_a_lock() {
        // Sanity: many distinct markers under one job_id coexist without a
        // shared RMW of the JSON (this is the whole point of the layout).
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();
        let job_id = save(&root, state(&["@world"]), false).unwrap();
        for i in 0..32 {
            mark_completed(
                &root,
                &job_id,
                MergeRoot::Target,
                &format!("app-misc/pkg-{i}"),
            )
            .unwrap();
        }
        assert_eq!(completed_keys(&root).len(), 32);
        // JSON still has no completed list.
        assert!(read_file(&root).resume.unwrap().completed.is_empty());
    }

    #[test]
    fn clear_removes_live_markers_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        let old_id = save(&root, state(&["app-misc/old"]), false).unwrap();
        mark_completed(&root, &old_id, MergeRoot::Target, "app-misc/old-1").unwrap();
        let new_id = save(&root, state(&["app-misc/new"]), false).unwrap();
        mark_completed(&root, &new_id, MergeRoot::Target, "app-misc/new-1").unwrap();

        clear(&root);
        // Live job markers gone; superseded job's markers still on disk
        // (reachable if backup is promoted).
        assert!(!job_done_dir(&root, &new_id).exists());
        assert!(marker_path(&root, &old_id, MergeRoot::Target, "app-misc/old-1").exists());
    }

    #[test]
    fn save_strips_ephemeral_ui_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        let mut s = state(&["app-misc/foo"]);
        s.merge_flags.ask = true;
        s.merge_flags.tree = true;
        s.merge_flags.json = true;
        s.merge_flags.keep_going = true;
        save(&root, s, false).unwrap();

        let loaded = take_for_resume(&root).unwrap().unwrap();
        assert!(!loaded.merge_flags.ask);
        assert!(!loaded.merge_flags.tree);
        assert!(!loaded.merge_flags.json);
        assert!(loaded.merge_flags.keep_going);
    }

    #[test]
    fn merge_resume_flags_ui_from_cli_shape_additive() {
        let saved = MergeFlags {
            emptytree: true,
            jobs: Some(4),
            exclude: vec!["cat/a".into()],
            ..Default::default()
        };

        let cli = MergeFlags {
            ask: true,
            keep_going: true,
            jobs: Some(16),
            exclude: vec!["cat/b".into()],
            ..Default::default()
        };

        let m = merge_resume_flags(&saved, &cli);
        assert!(m.ask); // current UI
        assert!(m.emptytree); // preserved from saved
        assert!(m.keep_going); // added by cli
        assert_eq!(m.jobs, Some(16)); // cli Some wins
        assert_eq!(m.exclude, vec!["cat/a".to_string(), "cat/b".to_string()]);
    }

    #[test]
    fn cleanresume_report_includes_flags_and_progress() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        let mut s = state(&["@system", "app-misc/foo", "app-misc/bar"]);
        s.merge_flags.emptytree = true;
        s.merge_flags.jobs = Some(8);
        let job_id = save(&root, s, false).unwrap();
        mark_completed(&root, &job_id, MergeRoot::Target, "sys-apps/baselayout-2").unwrap();

        let report = cleanresume(&root, false).unwrap();
        assert_eq!(report.len(), 1);
        let line = &report[0];
        assert!(line.contains("3 atoms"));
        assert!(line.contains("@system"));
        assert!(line.contains("emptytree"));
        assert!(line.contains("jobs=8"));
        assert!(line.contains("1 completed"));
    }

    #[test]
    fn migrates_legacy_json_completed_into_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        // Simulate a pre-marker save file with completed embedded.
        let legacy = ResumeFile {
            resume: Some(ResumeState {
                atoms: vec!["@system".into()],
                job_id: String::new(),
                completed: vec![LegacyCompletedPkg {
                    merge_root: "target".into(),
                    cpv: "sys-libs/glibc-2.39-r1".into(),
                }],
                ..Default::default()
            }),
            resume_backup: None,
        };
        write_file(&root, &legacy).unwrap();

        // Resume re-save migrates.
        let job_id = save(&root, state(&["@system"]), true).unwrap();
        assert!(!job_id.is_empty());
        assert!(
            completed_keys(&root).contains(&(MergeRoot::Target, "sys-libs/glibc-2.39-r1".into()))
        );
        assert!(marker_path(&root, &job_id, MergeRoot::Target, "sys-libs/glibc-2.39-r1").exists());
    }
}
