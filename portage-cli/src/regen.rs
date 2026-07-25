use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use portage_repo::{RegenOpts, RegenWriteTarget, ReposConf, SourceOpts, regen_cache};

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
    repos_args: &[String],
    main_repo_path: &str,
    repos_dir: Option<&str>,
    output: Option<PathBuf>,
    jobs: Option<usize>,
    dedup: bool,
) -> Result<()> {
    let conf = ReposConf::load().ok();

    // Resolve targets: explicit names/paths, or every non-main conf repo.
    let mut targets: Vec<PathBuf> = Vec::new();
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
                targets.push(loc);
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
                targets.push(path.to_path_buf());
            } else if let Some(entry) = conf.as_ref().and_then(|c| c.find(arg)) {
                if let Some(p) = entry.location.as_path() {
                    targets.push(p.to_path_buf());
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

    let mut total_errors = 0usize;
    for target in &targets {
        let (repo, masters) =
            crate::repo_open::open_with_masters(target.clone(), &repos_dir).context("open repo")?;

        let ebuilds: Vec<_> = repo
            .ebuilds_with_masters(&masters)
            .context("list ebuilds")?
            .into_iter()
            .collect();

        let write = match &output {
            Some(o) => RegenWriteTarget::Dir(o.clone()),
            // Prefer primary, else secondary already attached at open.
            None => RegenWriteTarget::Repository,
        };

        eprintln!("regen ::{} ({} ebuilds)", repo.name(), ebuilds.len());
        let opts = RegenOpts {
            source: SourceOpts { jobs, dedup },
            write,
        };
        let stats = regen_cache(&repo, &masters, ebuilds, &opts, |done, total| {
            eprint!("\r[{done}/{total}]");
        })
        .await
        .context("regen")?;
        eprintln!();
        if stats.errors > 0 {
            eprintln!("::{}: {} sourcing errors", repo.name(), stats.errors);
            total_errors += stats.errors;
        }
    }

    if total_errors > 0 {
        bail!("{total_errors} sourcing errors");
    }
    Ok(())
}
