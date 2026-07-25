//! List and inspect profiles from a repository.
//!
//! When a profile path (containing `/`) is given as the filter, the full
//! inheritance stack is resolved and `make.defaults` is sourced so the
//! fully-expanded USE flag list (after force/mask) is shown.

use std::process;

use clap::Parser;
use portage_repo::{Repository, UseExpand};

#[derive(Parser)]
#[command(about = "List and inspect profiles from a repository")]
struct Args {
    /// Path to the repository
    repo: String,
    /// Architecture keyword or profile path to filter/inspect
    #[arg(name = "arch-or-profile")]
    filter: Option<String>,
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

    let all_profiles = match repo.profiles_desc() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error reading profiles.desc: {e}");
            process::exit(1);
        }
    };

    let filter = args.filter.as_deref();

    // If the filter contains '/' it is a profile path — run full inspect.
    if let Some(path) = filter.filter(|s| s.contains('/')) {
        inspect_profile(&repo, path).await;
        return;
    }

    // Validate the arch filter against the known arch list.
    if let Some(arch) = filter
        && !arch.contains('/')
    {
        let known = repo.arch_list();
        if !known.is_empty() && !known.iter().any(|a| repo.arch_keyword(a) == arch) {
            let keywords: Vec<&str> = known.iter().map(|a| repo.arch_keyword(a)).collect();
            eprintln!(
                "Unknown arch {arch:?}. Known arches: {}",
                keywords.join(", ")
            );
            process::exit(1);
        }
    }

    // List profiles, optionally filtered by arch.
    let profiles: Vec<_> = all_profiles
        .iter()
        .filter(|p| filter.is_none_or(|arch| p.arch() == arch))
        .collect();

    if profiles.is_empty() {
        eprintln!("No profiles found for arch {:?}", filter.unwrap_or("(any)"));
        process::exit(1);
    }

    let mut current_arch = String::new();
    for desc in &profiles {
        let arch_str = desc.arch().to_string();
        if arch_str != current_arch {
            println!("\n[{}]", arch_str);
            current_arch = arch_str;
        }

        let status = desc.status().to_string();

        match repo.profile_stack(desc.path()) {
            Ok(stack) => {
                let depth = stack.profiles().len();
                let deprecated = if stack.is_deprecated() {
                    " [DEPRECATED]"
                } else {
                    ""
                };
                let force = stack.use_force().map(|v| v.len()).unwrap_or(0);
                let mask = stack.use_mask().map(|v| v.len()).unwrap_or(0);
                let pkg_mask = stack.package_mask().map(|v| v.len()).unwrap_or(0);
                let sys_pkgs = stack
                    .packages()
                    .map(|v| v.iter().filter(|(sys, _)| *sys).count())
                    .unwrap_or(0);
                println!(
                    "  {:<45} {:6}  depth={depth}  force={force}  mask={mask}  \
                     pkg_mask={pkg_mask}  sys={sys_pkgs}{deprecated}",
                    desc.path(),
                    status,
                );
            }
            Err(e) => {
                println!("  {:<45} {:6}  (stack error: {e})", desc.path(), status);
            }
        }
    }
    println!();
}

/// Show full detail for a single profile, including resolved USE flags.
async fn inspect_profile(repo: &Repository, profile_path: &str) {
    let stack = match repo.profile_stack(profile_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error building profile stack: {e}");
            process::exit(1);
        }
    };

    let expand = repo.use_expand().unwrap_or_default();

    println!("Profile:    {profile_path}");
    println!("Deprecated: {}", stack.is_deprecated());
    println!();

    println!(
        "=== Inheritance chain ({} profiles) ===",
        stack.profiles().len()
    );
    for (i, p) in stack.profiles().iter().enumerate() {
        println!("  [{i}] {}", p.path().display());
    }
    println!();

    print_use_set("use.force", stack.use_force(), &expand);
    print_use_set("use.mask", stack.use_mask(), &expand);
    print_use_set("use.stable.force", stack.use_stable_force(), &expand);
    print_use_set("use.stable.mask", stack.use_stable_mask(), &expand);

    if let Ok(pkgs) = stack.packages() {
        let sys: Vec<_> = pkgs.iter().filter(|(s, _)| *s).map(|(_, d)| d).collect();
        if !sys.is_empty() {
            println!("=== System packages ({}) ===", sys.len());
            for dep in &sys {
                println!("  {dep}");
            }
            println!();
        }
    }

    if let Ok(masks) = stack.package_mask()
        && !masks.is_empty()
    {
        println!("=== package.mask ({} atoms) ===", masks.len());
        for dep in masks.iter().take(20) {
            println!("  {dep}");
        }
        if masks.len() > 20 {
            println!("  ... ({} more)", masks.len() - 20);
        }
        println!();
    }

    println!("=== Resolved USE flags (after make.defaults + force/mask) ===");
    let mut shell = match repo.shell().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error creating shell: {e}");
            process::exit(1);
        }
    };
    match stack.configure_shell(&mut shell, &[]).await {
        Ok(()) => {
            let flags: Vec<String> = shell
                .use_flags_string()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let shell_expand =
                UseExpand::from_var(&shell.get_var("USE_EXPAND").unwrap_or_default());
            let groups = shell_expand.group(flags.iter().map(String::as_str));
            println!("  ({} flags across {} groups)", flags.len(), groups.len());
            println!();
            print_grouped(&groups);
        }
        Err(e) => eprintln!("  Error resolving USE flags: {e}"),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn print_use_set(name: &str, result: portage_repo::Result<Vec<String>>, expand: &UseExpand) {
    let Ok(flags) = result else { return };
    if flags.is_empty() {
        return;
    }
    let groups = expand.group(flags.iter().map(String::as_str));
    println!("=== {name} ({} flags) ===", flags.len());
    print_grouped(&groups);
}

fn print_grouped<K, S: AsRef<str>>(groups: &std::collections::BTreeMap<K, Vec<S>>)
where
    K: AsRef<str> + std::fmt::Display,
{
    const MAX_WIDTH: usize = 100;
    for (group, values) in groups {
        let mut values: Vec<&str> = values.iter().map(S::as_ref).collect();
        values.sort();
        let header = format!("  [{group}]");
        let indent = " ".repeat(header.len());
        print!("{header}");
        let mut col = header.len();
        for value in &values {
            if col > header.len() && col + 1 + value.len() > MAX_WIDTH {
                println!();
                print!("{indent}");
                col = indent.len();
            }
            print!(" {value}");
            col += 1 + value.len();
        }
        println!();
    }
    println!();
}
