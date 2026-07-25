use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;

pub fn run(repo_path: &Path, flags: &[String]) -> Result<()> {
    let repo = crate::repo_open::open(repo_path)?;

    for flag in flags {
        let mut seen: BTreeSet<String> = BTreeSet::new();

        for ebuild in repo.ebuilds()? {
            let cpv = ebuild.cpv();
            let cpn = cpv.cpn.to_string();
            if seen.contains(&cpn) {
                continue;
            }
            if let Ok(Some(entry)) = repo.cache_entry(cpv)
                && entry
                    .metadata
                    .iuse
                    .iter()
                    .any(|u| u.name() == flag.as_str())
            {
                seen.insert(cpn);
            }
        }

        if flags.len() > 1 {
            println!("[{flag}]");
        }
        for cpn in &seen {
            println!("{cpn}");
        }
    }
    Ok(())
}
