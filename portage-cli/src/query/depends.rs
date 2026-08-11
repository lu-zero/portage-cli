use std::collections::BTreeSet;

use anyhow::Result;
use portage_atom::{Dep, DepEntry};
use portage_repo::RepoSet;
use portage_vdb::Vdb;

pub fn run(
    set: &RepoSet,
    vdb: Option<&Vdb>,
    mode: super::ResolveMode,
    atoms: &[String],
) -> Result<()> {
    // No overlay search here: `set.main()` only — resolving an atom against
    // set as a whole (via resolve_atom below) can already succeed for an
    // overlay-only package, but scanning overlay ebuilds for reverse deps
    // is a separate gap, not fixed here.
    let repo = set.main();

    for raw in atoms {
        let target = super::resolve_atom(set, vdb, mode, raw)?;

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
