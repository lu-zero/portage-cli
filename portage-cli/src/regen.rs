//! Metadata-cache regeneration (`em regen`).
//!
//! Library work ([`portage_repo::regen_cache`]) sends one
//! [`portage_repo::RegenItem`] per finished ebuild on a channel. This applet
//! owns the activity bus and the terminal: progress and short failures are
//! [`ActivityEvent`]s rendered by [`crate::activity::HumanStdoutSink`]; rich
//! miette frames for parse failures are printed here after the corresponding
//! bus event (structured payload stays in-process — the wire form only
//! carries a short string).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use portage_repo::{RegenItem, RegenOpts, RegenWriteTarget, ReposConf, SourceOpts, regen_cache};

use crate::activity::{
    ACTIVITY_EVENT_VERSION, ActivityBus, ActivityEvent, ActivityMergeRoot, ActivityMode, PkgKind,
    SessionFlags,
};
use crate::cli::Cli;

/// Regenerate metadata caches.
///
/// With no repos named, every `repos.conf` repo *except the main one* is
/// regenerated — overlays are the repos that usually ship no `md5-cache`,
/// while the main repo's rsync/git cache is maintained upstream (name it
/// explicitly to regenerate it anyway). Each argument may be a repos.conf
/// name or a path.
///
/// Default write path: in-tree primary if writable, else the repo's user
/// secondary (`$XDG_CACHE_HOME/em/md5-cache/<name>`), configured at open.
/// `--output DIR` forces a single directory instead.
pub async fn run(
    cli: &Cli,
    repos_args: &[String],
    main_repo_path: &str,
    repos_dir: Option<&str>,
    output: Option<PathBuf>,
    jobs: Option<usize>,
    dedup: bool,
) -> Result<()> {
    let conf = ReposConf::load().ok();

    // Resolve targets: explicit names/paths, or every non-main conf repo.
    let mut targets: Vec<(PathBuf, Option<Vec<String>>)> = Vec::new();
    if repos_args.is_empty() {
        let Some(conf) = &conf else {
            bail!("no repos named and no repos.conf found");
        };
        let main = conf
            .main_repo()
            .and_then(|m| m.location.as_path().map(PathBuf::from));
        for entry in conf.repos() {
            let Some(loc) = entry.location.as_path().map(PathBuf::from) else {
                continue; // virtual/alias repo — no cache to regenerate
            };
            if Some(&loc) != main.as_ref() {
                targets.push((loc, entry.masters.clone()));
            }
        }
        if targets.is_empty() {
            eprintln!("em regen: no overlays in repos.conf — nothing to do");
            return Ok(());
        }
    } else {
        for arg in repos_args {
            let path = Path::new(arg);
            if path.is_dir() {
                let masters = conf.as_ref().and_then(|c| {
                    c.repos()
                        .iter()
                        .find(|e| e.location.as_path() == Some(path))
                        .and_then(|e| e.masters.clone())
                });
                targets.push((path.to_path_buf(), masters));
            } else if let Some(entry) = conf.as_ref().and_then(|c| c.find(arg)) {
                if let Some(p) = entry.location.as_path() {
                    targets.push((p.to_path_buf(), entry.masters.clone()));
                } else {
                    bail!("'{arg}' is a virtual repo (no on-disk path to regenerate)");
                }
            } else {
                bail!("'{arg}' is neither a directory nor a repos.conf repo name");
            }
        }
    }
    if output.is_some() && targets.len() > 1 {
        bail!("--output only makes sense with a single repository");
    }

    // Masters resolve against the main repo's parent (e.g. an overlay's
    // `masters = gentoo` → /var/db/repos/gentoo).
    let default_repos_dir = Path::new(main_repo_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let repos_dir = repos_dir.map_or(default_repos_dir, PathBuf::from);

    let roots = cli.roots();
    // Not `roots.merge_root()`: regen must stay usable unprivileged (see
    // `xdg::regen_activity_root`'s doc), unlike a real merge's activity bus.
    let activity_root = crate::xdg::regen_activity_root();
    let activity = crate::activity::regen_activity_bus(&activity_root);
    crate::activity::attach_human_stdout(&activity, cli.quiet, cli.verbose);
    let activity_args = cli.effective_activity();
    crate::activity::attach_jsonl_outputs(
        &activity,
        activity_args.activity_fd,
        activity_args.activity_jsonl.as_deref(),
    )?;
    if activity_args.emergelog {
        crate::activity::attach_emergelog(&activity, roots.merge_root());
    }

    let mut total_errors = 0usize;
    for (target, masters_override) in &targets {
        let repo = crate::repo_open::open_with_masters(
            target.clone(),
            &repos_dir,
            masters_override.as_deref(),
        )
        .context("open repo")?;

        let ebuilds: Vec<_> = repo
            .ebuilds()
            .context("list ebuilds")?
            .into_iter()
            .collect();

        let write = match &output {
            Some(o) => RegenWriteTarget::Dir(o.clone()),
            // Prefer primary, else secondary already attached at open.
            None => RegenWriteTarget::Repository,
        };

        let plan_total = ebuilds.len() as u32;
        let job_id = crate::activity::resolve_job_id(&Default::default(), None);
        let session_started = ActivityEvent::now();
        let banner = format!("regen ::{} ({} ebuilds)", repo.name(), plan_total);
        // HumanStdoutSink prints argv[1] as the regen banner.
        let argv = vec!["em".into(), banner, "regen".into()];

        activity.emit(ActivityEvent::SessionStart {
            v: ACTIVITY_EVENT_VERSION,
            job_id: job_id.clone(),
            parent_job_id: None,
            pid: std::process::id(),
            started_at: session_started,
            argv,
            merge_root: roots.merge_root().to_string(),
            host_root: cli.host_roots().merge_root().to_string(),
            mode: ActivityMode::Regen,
            plan_total,
            flags: SessionFlags {
                jobs: jobs.map(|j| j as u32),
                ..Default::default()
            },
            plan: vec![],
            blockers: vec![],
        });

        let opts = RegenOpts {
            source: SourceOpts { jobs, dedup },
            write,
        };

        // Concurrent UI drain: pool runs on this task (feed+join); items land
        // on an unbounded channel so the consumer never parks workers.
        let (tx, rx) = flume::unbounded();
        let bus = activity.clone();
        let job_id_ui = job_id.clone();
        let ui = tokio::spawn(async move {
            while let Ok(item) = rx.recv_async().await {
                emit_item(&bus, &job_id_ui, item);
            }
        });

        let stats = regen_cache(&repo, ebuilds, &opts, tx)
            .await
            .context("regen")?;
        // `tx` dropped inside regen_cache → rx disconnects → ui finishes.
        ui.await.context("regen UI task")?;

        let completed = plan_total.saturating_sub(stats.errors as u32);
        activity.emit(ActivityEvent::SessionEnd {
            v: ACTIVITY_EVENT_VERSION,
            job_id,
            parent_job_id: None,
            at: ActivityEvent::now(),
            ok: stats.errors == 0,
            completed,
            failed: stats.errors as u32,
            seconds: ActivityEvent::now() - session_started,
        });

        // HumanStdoutSink's SessionEnd handler already printed a per-repo
        // "regenerated N ebuild(s) ..., M failed" summary; just tally here.
        total_errors += stats.errors;
    }

    if total_errors > 0 {
        bail!("{total_errors} sourcing errors");
    }
    Ok(())
}

/// Map one finished ebuild onto the bus (and rich UI for parse failures).
fn emit_item(bus: &ActivityBus, job_id: &str, item: RegenItem) {
    let cpv = item.ebuild.cpv().to_string();
    let cpn = item.ebuild.cpv().cpn.to_string();
    let at = ActivityEvent::now();
    let (ok, error) = match &item.result {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };

    // Progress / short failure line — HumanStdoutSink owns the terminal form.
    bus.emit(ActivityEvent::PkgEnd {
        v: ACTIVITY_EVENT_VERSION,
        job_id: job_id.to_string(),
        parent_job_id: None,
        cpv,
        cpn,
        merge_root: ActivityMergeRoot::Target,
        kind: PkgKind::Source,
        ok,
        at,
        seconds: 0.0,
        phases: vec![],
        error,
    });

    // Rich code frame stays off the wire (JSONL only has the short `error`
    // string above). Color/theme decided here at the UI boundary.
    if let Err(e) = &item.result
        && let Some(diag) = e.parse_diagnostic()
    {
        crate::diag::print_diagnostic(diag);
    }
}
