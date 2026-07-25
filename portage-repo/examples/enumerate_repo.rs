//! Print a summary of a repository's contents: category / package / ebuild
//! counts, plus eclass and license totals.

use clap::Parser;
use portage_repo::Repository;

#[derive(Parser)]
#[command(about = "Print a summary of a repository's contents")]
struct Args {
    /// Path to the repository
    #[arg(default_value = "/var/db/repos/gentoo")]
    repo: String,
}

fn main() {
    let args = Args::parse();

    let repo = match Repository::builder().in_memory_cache().open(&args.repo) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error opening repository at {}: {e}", args.repo);
            std::process::exit(1);
        }
    };

    println!("Repository: {}", repo.name());
    println!("Path: {}", repo.path());
    println!("Masters: {:?}", repo.layout().masters);
    println!();

    let categories = repo.categories().collect_vec();
    println!("Categories: {}", categories.len());

    let mut total_packages = 0;
    let mut total_ebuilds = 0;

    for cat in &categories {
        if !cat.exists() {
            continue;
        }
        for pkg in cat.packages() {
            total_packages += 1;
            let ebuilds = match pkg.ebuilds() {
                Ok(e) => e,
                Err(_) => continue,
            };
            total_ebuilds += ebuilds.len();
        }
    }

    println!("Packages: {total_packages}");
    println!("Ebuilds: {total_ebuilds}");

    if let Ok(eclasses) = repo.eclasses() {
        println!("Eclasses: {}", eclasses.len());
    }

    if let Ok(licenses) = repo.licenses() {
        println!("Licenses: {}", licenses.len());
    }

    let arches = repo.arch_list();
    if !arches.is_empty() {
        let keywords: Vec<&str> = arches.iter().map(|a| repo.arch_keyword(a)).collect();
        println!("Arches:   {} ({})", arches.len(), keywords.join(" "));
    }
}
