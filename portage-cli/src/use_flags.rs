use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::MakeConf;

use crate::cli;

pub fn run(
    cli: &cli::Cli,
    add: &[String],
    remove: &[String],
    make_conf: Option<&Utf8Path>,
) -> Result<()> {
    let path = resolve_path(cli, make_conf)?;

    if add.is_empty() && remove.is_empty() {
        return show(&path);
    }

    // Directory-form make.conf: patch a single fragment (see MakeConf::apply_use_changes_at).
    let new_use = MakeConf::apply_use_changes_at(&path, add, remove)
        .with_context(|| format!("updating USE in {path}"))?;

    println!("USE=\"{}\"", new_use);
    Ok(())
}

fn show(path: &Utf8Path) -> Result<()> {
    let mc = MakeConf::load(path).with_context(|| format!("reading {path}"))?;

    match mc.get("USE") {
        Some(val) => println!("USE=\"{}\"", val),
        None => println!("USE not set in {}", path),
    }
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

    /// The bug this module used to have: `resolve_path` ignored `--local`
    /// entirely and always resolved the host's `/etc/portage/make.conf`.
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
}
