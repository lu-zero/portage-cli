//! `em use` — enable/disable/query USE flags in make.conf
//!
//! Mirrors `em pkg use`'s add/subtract/drop trichotomy (`subtract` writes an explicit
//! `-flag`, `drop` removes the token entirely) so the two applets never drift onto
//! different semantics for the same three verbs. `-a`/`-s`/`-d` also accept euse's own
//! short letters as aliases — `-E`/`-D`/`-R`/`-P` (enable/disable/remove/prune).
//!
//! `--expand VAR`/`-e VAR` retargets the edit onto a USE_EXPAND variable
//! (e.g. `VIDEO_CARDS`) instead of `USE` — same trichotomy, same file, just
//! a different assignment. `--list-expand`/`-L` lists every USE_EXPAND
//! variable the active profile knows about. `--info`/`-i FLAG` shows flag
//! descriptions from `profiles/use.desc` and `use.local.desc` — the
//! counterpart to euse's `-i`; `-g`/`-l-desc` restrict the search to just
//! one of the two, matching euse's `-g`/`-l` scope suboptions (euse's `-p`
//! per-package equivalent is `em pkg use`; a single package's flags are
//! `em query uses <atom>`).

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::MakeConf;

use crate::cli;

mod output;

/// Options for [`run`], one field per CLI flag on `Applet::Use`
pub struct UseOpts<'a> {
    pub add: &'a [String],
    pub subtract: &'a [String],
    pub drop: &'a [String],
    pub dry_run: bool,
    pub expand: Option<&'a str>,
    pub list_expand: bool,
    pub info: &'a [String],
    pub global: bool,
    pub local_desc: bool,
    pub make_conf: Option<&'a Utf8Path>,
}

pub async fn run(cli: &cli::Cli, opts: &UseOpts<'_>) -> Result<()> {
    let path = resolve_path(cli, opts.make_conf)?;

    if !opts.info.is_empty() || opts.global || opts.local_desc {
        return output::show_info(cli, opts.info, opts.global, opts.local_desc);
    }
    if opts.list_expand {
        return output::list_expand(cli, &path).await;
    }
    let var = opts
        .expand
        .map_or_else(|| "USE".to_string(), str::to_uppercase);

    if opts.add.is_empty() && opts.subtract.is_empty() && opts.drop.is_empty() {
        // Bare `em use` (no --expand either): show USE plus every set
        // USE_EXPAND variable, not just USE alone.
        return match opts.expand {
            Some(_) => output::show(&path, &var),
            None => output::show_summary(cli, &path).await,
        };
    }

    let (old_value, preview_value) =
        MakeConf::preview_use_changes_at(&path, &var, opts.add, opts.subtract, opts.drop)
            .with_context(|| format!("previewing {var} change in {path}"))?;

    if opts.dry_run {
        output::print_diff(&var, &old_value, &preview_value);
        return Ok(());
    }

    // Directory-form make.conf: patch a single fragment (see MakeConf::apply_use_changes_at).
    let new_value = MakeConf::apply_use_changes_at(&path, &var, opts.add, opts.subtract, opts.drop)
        .with_context(|| format!("updating {var} in {path}"))?;

    output::print_diff(&var, &old_value, &new_value);
    Ok(())
}

/// `--make-conf FILE` short-circuits; otherwise resolves against
/// `--config-root`/`--local`/`--prefix` like every other config-reading
/// command (`crate::select::config_portage_dir`), not the bare host
/// `/etc/portage/make.conf` — a `--local`/`--prefix` invocation must edit
/// the prefix's own make.conf, not the host's.
fn resolve_path(cli: &cli::Cli, override_path: Option<&Utf8Path>) -> Result<Utf8PathBuf> {
    if let Some(p) = override_path {
        return Ok(p.to_owned());
    }
    let portage_dir = crate::select::config_portage_dir(cli);
    let candidates = [
        portage_dir.join("make.conf"),
        // Legacy pre-etc/portage convention, same root.
        portage_dir
            .parent()
            .map(|etc| etc.join("make.conf"))
            .unwrap_or_else(|| Utf8PathBuf::from("/etc/make.conf")),
    ];
    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.clone());
        }
    }
    bail!(
        "no make.conf found at {} or {}",
        candidates[0],
        candidates[1]
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    fn opts() -> UseOpts<'static> {
        UseOpts {
            add: &[],
            subtract: &[],
            drop: &[],
            dry_run: false,
            expand: None,
            list_expand: false,
            info: &[],
            global: false,
            local_desc: false,
            make_conf: None,
        }
    }

    // The bug this module used to have: `resolve_path` ignored `--local`
    // entirely and always resolved the host's `/etc/portage/make.conf`.
    #[test]
    fn resolve_path_follows_local_prefix_not_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = Utf8Path::from_path(dir.path()).unwrap();
        let portage_dir = prefix.join("etc/portage");
        std::fs::create_dir_all(portage_dir.as_std_path()).unwrap();
        std::fs::write(portage_dir.join("make.conf").as_std_path(), "USE=\"x\"\n").unwrap();

        let cli = cli::Cli::parse_from(["em", "--local", prefix.as_str(), "use"]);
        let resolved = resolve_path(&cli, None).unwrap();
        assert_eq!(resolved, portage_dir.join("make.conf"));
    }

    #[test]
    fn resolve_path_explicit_override_wins() {
        let dir = tempfile::tempdir().unwrap();
        let explicit = Utf8Path::from_path(dir.path()).unwrap().join("custom.conf");
        let cli = cli::Cli::parse_from(["em", "use"]);
        let resolved = resolve_path(&cli, Some(&explicit)).unwrap();
        assert_eq!(resolved, explicit);
    }

    // `--dry-run` must compute the same result `run()` would otherwise
    // write, but the file itself must stay untouched.
    #[tokio::test]
    async fn dry_run_previews_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().join("make.conf");
        std::fs::write(path.as_std_path(), "USE=\"ssl\"\n").unwrap();

        let cli = cli::Cli::parse_from(["em", "use"]);
        run(
            &cli,
            &UseOpts {
                add: &["nls".to_string()],
                dry_run: true,
                make_conf: Some(path.as_path()),
                ..opts()
            },
        )
        .await
        .unwrap();

        let untouched = std::fs::read_to_string(path.as_std_path()).unwrap();
        assert_eq!(untouched, "USE=\"ssl\"\n");
    }

    // `add`/`subtract`/`drop` follow the same trichotomy as `em pkg use`:
    // subtract writes an explicit `-flag`, drop removes the token entirely.
    #[tokio::test]
    async fn add_subtract_drop_match_pkg_use_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().join("make.conf");
        std::fs::write(path.as_std_path(), "USE=\"ssl themes bar\"\n").unwrap();

        let cli = cli::Cli::parse_from(["em", "use"]);
        run(
            &cli,
            &UseOpts {
                add: &["nls".to_string()],
                subtract: &["themes".to_string()],
                drop: &["bar".to_string()],
                make_conf: Some(path.as_path()),
                ..opts()
            },
        )
        .await
        .unwrap();

        let written = std::fs::read_to_string(path.as_std_path()).unwrap();
        assert!(written.contains("ssl nls -themes"), "{written}");
        assert!(!written.contains("bar"), "{written}");
    }

    // `--expand VAR` retargets add/subtract/drop onto that variable instead
    // of USE, lowercase input included (profile `.desc` names are lowercase).
    #[tokio::test]
    async fn expand_targets_the_named_variable() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8Path::from_path(dir.path()).unwrap().join("make.conf");
        std::fs::write(path.as_std_path(), "USE=\"ssl\"\nVIDEO_CARDS=\"intel\"\n").unwrap();

        let cli = cli::Cli::parse_from(["em", "use"]);
        run(
            &cli,
            &UseOpts {
                add: &["nouveau".to_string()],
                expand: Some("video_cards"),
                make_conf: Some(path.as_path()),
                ..opts()
            },
        )
        .await
        .unwrap();

        let written = std::fs::read_to_string(path.as_std_path()).unwrap();
        assert!(
            written.contains("VIDEO_CARDS=\"intel nouveau\""),
            "{written}"
        );
        assert!(written.contains("USE=\"ssl\""), "{written}");
    }
}
