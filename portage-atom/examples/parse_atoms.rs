//! Example demonstrating parsing various Portage atoms

use portage_atom::{Cpn, Cpv, Dep};

fn main() {
    println!("Portage Atom Parser Examples\n");

    // Simple unversioned atom
    println!("1. Simple Cpn:");
    let cpn = Cpn::parse("dev-lang/rust").expect("Failed to parse cpn");
    println!("   Input: dev-lang/rust");
    println!("   Category: {}", cpn.category);
    println!("   Package: {}", cpn.package);
    println!("   Output: {}\n", cpn);

    // Versioned atom
    println!("2. Versioned Cpv:");
    let cpv = Cpv::parse("dev-lang/rust-1.75.0").expect("Failed to parse cpv");
    println!("   Input: dev-lang/rust-1.75.0");
    println!("   Category: {}", cpv.cpn.category);
    println!("   Package: {}", cpv.cpn.package);
    println!("   Version: {}", cpv.version);
    println!("   Output: {}\n", cpv);

    // Cpv with revision
    println!("3. Cpv with revision:");
    let cpv = Cpv::parse("dev-lang/rust-1.75.0-r1").expect("Failed to parse cpv with revision");
    println!("   Input: dev-lang/rust-1.75.0-r1");
    println!("   Revision: r{}", cpv.version.revision().0);
    println!("   Output: {}\n", cpv);

    // Full dependency with version operator
    println!("4. Dependency with version operator:");
    let dep = Dep::parse(">=dev-lang/rust-1.75.0").expect("Failed to parse dep");
    println!("   Input: >=dev-lang/rust-1.75.0");
    println!("   Operator: {:?}", dep.op);
    println!("   Output: {}\n", dep);

    // Dependency with slot
    println!("5. Dependency with slot:");
    let dep = Dep::parse("dev-lang/rust:0").expect("Failed to parse dep with slot");
    println!("   Input: dev-lang/rust:0");
    println!("   Has slot: {}", dep.slot_dep.is_some());
    println!("   Output: {}\n", dep);

    // Dependency with USE flags
    println!("6. Dependency with USE flags:");
    let dep = Dep::parse("dev-lang/rust[llvm_targets_AMDGPU,-debug]")
        .expect("Failed to parse dep with use flags");
    println!("   Input: dev-lang/rust[llvm_targets_AMDGPU,-debug]");
    if let Some(use_deps) = &dep.use_deps {
        println!("   USE flags: {} flags", use_deps.len());
        for flag in use_deps {
            println!("     - {}", flag);
        }
    }
    println!("   Output: {}\n", dep);

    // Blocker
    println!("7. Blocker:");
    let dep = Dep::parse("!!dev-lang/rust").expect("Failed to parse blocker");
    println!("   Input: !!dev-lang/rust");
    println!("   Blocker: {:?}", dep.blocker);
    println!("   Output: {}\n", dep);

    // Complex dependency
    println!("8. Complex dependency:");
    let dep = Dep::parse(">=dev-lang/rust-1.75.0:0/1.75[llvm_targets_AMDGPU]::gentoo")
        .expect("Failed to parse complex dep");
    println!("   Input: >=dev-lang/rust-1.75.0:0/1.75[llvm_targets_AMDGPU]::gentoo");
    println!("   Has version: {}", dep.version.is_some());
    println!("   Has slot: {}", dep.slot_dep.is_some());
    println!("   Has USE: {}", dep.use_deps.is_some());
    println!("   Repository: {:?}", dep.repo);
    println!("   Output: {}\n", dep);

    // Version with suffixes
    println!("9. Version with suffixes:");
    let cpv = Cpv::parse("dev-lang/python-3.11.0_rc2_p1-r1")
        .expect("Failed to parse version with suffixes");
    println!("   Input: dev-lang/python-3.11.0_rc2_p1-r1");
    println!("   Suffixes: {} suffixes", cpv.version.suffixes().count());
    for suffix in cpv.version.suffixes() {
        println!("     - {}", suffix);
    }
    println!("   Output: {}\n", cpv);
}
