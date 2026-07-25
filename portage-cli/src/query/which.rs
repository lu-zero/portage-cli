use std::cmp::Ordering;
use std::path::Path;

use anyhow::Result;
use portage_atom::{Cpv, Dep, Operator};
use portage_vdb::Vdb;

pub fn run(
    repo_path: &Path,
    vdb: Option<&Vdb>,
    mode: super::ResolveMode,
    atoms: &[String],
) -> Result<()> {
    let repo = crate::repo_open::open(repo_path)?;

    let ebuilds: Vec<_> = repo.ebuilds()?.into_iter().collect();

    for raw in atoms {
        let dep = super::resolve_atom(&repo, vdb, mode, raw)?;

        let best = ebuilds
            .iter()
            .filter(|e| dep_matches_cpv(&dep, e.cpv()))
            .max_by(|a, b| a.cpv().version.cmp(&b.cpv().version));

        match best {
            Some(e) => println!("{}", e.path()),
            None => eprintln!("em: no ebuild found for '{raw}'"),
        }
    }
    Ok(())
}

pub fn dep_matches_cpv(dep: &Dep, cpv: &Cpv) -> bool {
    if dep.cpn != cpv.cpn {
        return false;
    }
    match (&dep.version, &dep.op) {
        (None, _) => true,
        (Some(v), Some(Operator::Equal)) if dep.glob => cpv.version.glob_matches(v),
        (Some(v), Some(op)) => {
            let ord = cpv.version.cmp(v);
            match op {
                Operator::Less => ord == Ordering::Less,
                Operator::LessOrEqual => ord != Ordering::Greater,
                Operator::Equal => ord == Ordering::Equal,
                Operator::Approximate => {
                    let mut base = v.clone();
                    base.revision = Default::default();
                    let mut cv = cpv.version.clone();
                    cv.revision = Default::default();
                    cv == base
                }
                Operator::GreaterOrEqual => ord != Ordering::Less,
                Operator::Greater => ord == Ordering::Greater,
            }
        }
        _ => true,
    }
}
