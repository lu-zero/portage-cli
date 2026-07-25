use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;
use portage_atom::{Dep, DepEntry};
use portage_vdb::Vdb;

pub fn run(
    repo_path: &Path,
    vdb: Option<&Vdb>,
    mode: super::ResolveMode,
    atoms: &[String],
) -> Result<()> {
    let repo = crate::repo_open::open(repo_path)?;

    for raw in atoms {
        let target = super::resolve_atom(&repo, vdb, mode, raw)?;

        let mut matches: BTreeSet<String> = BTreeSet::new();

        for ebuild in repo.ebuilds()? {
            let cpv = ebuild.cpv();
            let Ok(Some(entry)) = repo.cache_entry(cpv) else {
                continue;
            };
            let m = &entry.metadata;
            let dep_trees = [&m.depend, &m.rdepend, &m.bdepend, &m.pdepend, &m.idepend];

            if dep_trees.iter().any(|tree| tree_contains(&target, tree)) {
                matches.insert(cpv.cpn.to_string());
            }
        }

        if atoms.len() > 1 {
            println!("[{raw}]");
        }
        for cpn in &matches {
            println!("{cpn}");
        }
    }
    Ok(())
}

fn tree_contains(target: &Dep, entries: &[DepEntry]) -> bool {
    entries.iter().any(|e| entry_matches(target, e))
}

fn entry_matches(target: &Dep, entry: &DepEntry) -> bool {
    match entry {
        DepEntry::Atom(dep) => dep.blocker.is_none() && dep.cpn == target.cpn,
        DepEntry::UseConditional { children, .. }
        | DepEntry::AllOf(children)
        | DepEntry::AnyOf(children)
        | DepEntry::ExactlyOneOf(children)
        | DepEntry::AtMostOneOf(children) => tree_contains(target, children),
    }
}
