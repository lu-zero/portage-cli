//! Run a single ebuild phase — similar to `portage ebuild <file> <phase>`.
//!
//! Sources the ebuild through the embedded bash shell, then executes the
//! requested phase function.  Build directories are created under a temporary
//! location (or a path you specify with `--work-dir`).
//!
//! Phases: pretend, setup, unpack, prepare, configure, compile, test, install,
//! preinst, postinst, prerm, postrm, nofetch, info, config

use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;
use portage_atom::Cpv;
use portage_repo::Repository;

#[derive(Parser)]
#[command(about = "Run a single ebuild phase")]
struct Args {
    /// Path to the repository
    repo: String,
    /// Package atom, e.g. app-misc/hello-2.12.1
    cpv: String,
    /// Phase to execute (configure, compile, install, …)
    phase: String,
    /// Active USE flags
    #[arg(long, num_args = 1.., value_name = "FLAG")]
    r#use: Vec<String>,
    /// Directory to use for WORKDIR/T/D
    #[arg(long, value_name = "PATH")]
    work_dir: Option<PathBuf>,
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

    let cpv = match Cpv::parse(&args.cpv) {
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
            eprintln!("Package {}/{} not found", cpv.cpn.category, cpv.cpn.package);
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

    let work_root = args.work_dir.unwrap_or_else(|| {
        let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        let pf = format!("{}-{}", cpv.cpn.package, cpv.version);
        PathBuf::from(format!("{tmp}/portage/{}/{pf}", cpv.cpn.category))
    });

    eprintln!(
        ">>> Running phase '{}' for {} in {}",
        args.phase,
        args.cpv,
        work_root.display()
    );

    let mut shell = match repo.shell().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error creating shell: {e}");
            process::exit(1);
        }
    };

    if !args.r#use.is_empty() {
        let flags: Vec<&str> = args.r#use.iter().map(String::as_str).collect();
        if let Err(e) = shell.set_use_flags(&flags) {
            eprintln!("Error setting USE flags: {e}");
            process::exit(1);
        }
        eprintln!(">>> USE={}", args.r#use.join(" "));
    }

    match shell
        .run_phase(&ebuild, &args.phase, &work_root, Path::new("/"))
        .await
    {
        Ok(()) => {
            eprintln!(">>> Phase '{}' completed successfully", args.phase);
        }
        Err(e) => {
            eprintln!("!!! Phase '{}' failed: {e}", args.phase);
            process::exit(1);
        }
    }
}
