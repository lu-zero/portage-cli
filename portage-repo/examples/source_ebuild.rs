//! Source a single ebuild through the embedded bash shell and print the
//! extracted PMS metadata variables.

use std::process;

use clap::Parser;
use portage_repo::Repository;

#[derive(Parser)]
#[command(about = "Source an ebuild and print its extracted PMS metadata")]
struct Args {
    /// Path to the repository
    repo: String,
    /// Package atom, e.g. dev-lang/rust-1.75.0
    cpv: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = Args::parse();

    let repo = match Repository::builder().in_memory_cache().open(&args.repo) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error opening repository: {e}");
            process::exit(1);
        }
    };

    let cpv = match portage_atom::Cpv::parse(&args.cpv) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Invalid atom {}: {e}", args.cpv);
            process::exit(1);
        }
    };

    let category = match repo.category(&cpv.cpn.category) {
        Some(c) => c,
        None => {
            eprintln!("Category {} not found", cpv.cpn.category);
            process::exit(1);
        }
    };

    let package = match category.package(&cpv.cpn.package) {
        Some(p) => p,
        None => {
            eprintln!(
                "Package {} not found in {}",
                cpv.cpn.package, cpv.cpn.category
            );
            process::exit(1);
        }
    };

    let version_str = cpv.version.to_string();
    let ebuild = match package.ebuild(&version_str) {
        Ok(Some(e)) => e,
        Ok(None) => {
            eprintln!("Ebuild {} not found", args.cpv);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error looking up ebuild: {e}");
            process::exit(1);
        }
    };

    println!("Sourcing {}", ebuild.path());
    println!();

    let mut shell = match repo.shell().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error creating shell: {e}");
            process::exit(1);
        }
    };

    let metadata = match shell.source_ebuild(&ebuild).await {
        Ok(s) => s.metadata,
        Err(e) => {
            eprintln!("Error sourcing ebuild: {e}");
            process::exit(1);
        }
    };

    println!("EAPI:         {}", metadata.eapi);
    println!("DESCRIPTION:  {}", metadata.description);
    println!("SLOT:         {}", metadata.slot);
    println!("HOMEPAGE:     {:?}", metadata.homepage);
    println!("KEYWORDS:     {:?}", metadata.keywords);
    println!("IUSE:         {:?}", metadata.iuse);
    println!("LICENSE:      {:?}", metadata.license);

    if !metadata.depend.is_empty_raw() {
        println!("DEPEND:       {:?}", metadata.depend.list());
    }
    if !metadata.rdepend.is_empty_raw() {
        println!("RDEPEND:      {:?}", metadata.rdepend.list());
    }
    if !metadata.bdepend.is_empty_raw() {
        println!("BDEPEND:      {:?}", metadata.bdepend.list());
    }
    if !metadata.pdepend.is_empty_raw() {
        println!("PDEPEND:      {:?}", metadata.pdepend.list());
    }
    if !metadata.idepend.is_empty_raw() {
        println!("IDEPEND:      {:?}", metadata.idepend.list());
    }
    if !metadata.restrict.is_empty() {
        println!("RESTRICT:     {:?}", metadata.restrict);
    }
    if !metadata.properties.is_empty() {
        println!("PROPERTIES:   {:?}", metadata.properties);
    }
    if metadata.required_use.is_some() {
        println!("REQUIRED_USE: {:?}", metadata.required_use);
    }
    if !metadata.inherited.is_empty() {
        println!("INHERITED:    {:?}", metadata.inherited);
    }
    if !metadata.defined_phases.is_empty() {
        println!("PHASES:       {:?}", metadata.defined_phases);
    }
}
