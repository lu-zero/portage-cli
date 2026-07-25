use anyhow::{Context, Result};
use camino::Utf8Path;

pub fn run(repo_path: &Utf8Path, output: Option<&str>) -> Result<()> {
    let repo = crate::repo_open::open(repo_path)
        .with_context(|| format!("opening repo at {repo_path}"))?;
    let use_db =
        portage_repo::UseDb::build_local_from_repo(&repo).context("building use.local.desc")?;
    let pkg_count = use_db.packages_with_local_flags().count();
    let flag_count: usize = use_db
        .packages_with_local_flags()
        .filter_map(|cpn| use_db.local_flags(cpn))
        .map(|m| m.len())
        .sum();
    match output {
        Some("-") => {
            use_db
                .write_use_local_desc_to(std::io::stdout().lock())
                .context("writing to stdout")?;
        }
        Some(path) => {
            let out_path = Utf8Path::new(path);
            use_db
                .write_use_local_desc(out_path)
                .with_context(|| format!("writing {path}"))?;
            eprintln!("Wrote {flag_count} use flags ({pkg_count} packages) to {path}.");
        }
        None => {
            let out_path = repo.path().join("profiles/use.local.desc");
            use_db
                .write_use_local_desc(&out_path)
                .with_context(|| format!("writing {out_path}"))?;
            eprintln!("Wrote {flag_count} use flags ({pkg_count} packages) to {out_path}.");
        }
    }
    Ok(())
}
