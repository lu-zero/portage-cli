use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

const REPO_REVISIONS: &str = "var/lib/portage/repo_revisions";

/// Purge the repo_revisions file (emaint revisions behaviour).
///
/// Without `repos`, the entire file is deleted.  With `repos`, only those
/// specific repo entries are removed from the JSON object.
pub fn run(repos: &[String], root: Option<&Utf8Path>) -> Result<()> {
    let path = revisions_path(root);

    if !path.exists() {
        println!("No repo_revisions file found at {path}.");
        return Ok(());
    }

    if repos.is_empty() {
        std::fs::remove_file(&path).with_context(|| format!("removing {path}"))?;
        println!("Purged {path}.");
    } else {
        purge_repos(&path, repos)?;
    }

    Ok(())
}

fn purge_repos(path: &Utf8Path, repos: &[String]) -> Result<()> {
    let content = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;

    let mut map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&content).with_context(|| format!("parsing {path}"))?;

    let mut removed = Vec::new();
    for repo in repos {
        if map.remove(repo).is_some() {
            removed.push(repo.as_str());
        } else {
            crate::style::warn_line(&format!("repo '{repo}' not found in {path}"));
        }
    }

    if removed.is_empty() {
        return Ok(());
    }

    if map.is_empty() {
        std::fs::remove_file(path).with_context(|| format!("removing {path}"))?;
        println!(
            "Purged {path} (empty after removing {}).",
            removed.join(", ")
        );
    } else {
        let out = serde_json::to_string(&map).context("serialising")?;
        std::fs::write(path, out).with_context(|| format!("writing {path}"))?;
        println!("Removed {} from {path}.", removed.join(", "));
    }

    Ok(())
}

fn revisions_path(root: Option<&Utf8Path>) -> Utf8PathBuf {
    match root {
        Some(r) => r.join(REPO_REVISIONS),
        None => Utf8PathBuf::from("/").join(REPO_REVISIONS),
    }
}
