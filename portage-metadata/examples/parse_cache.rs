use portage_metadata::CacheEntry;
use std::env;
use std::fs;

const EXAMPLE: &str = "\
DEFINED_PHASES=install test unpack
DEPEND=>=sys-devel/clang-10.0.0_rc1:* dev-python/setuptools
DESCRIPTION=Python bindings for sys-devel/clang
EAPI=7
HOMEPAGE=https://llvm.org/
IUSE=test python_targets_python3_6 python_targets_python3_7
KEYWORDS=~amd64 ~x86
LICENSE=Apache-2.0-with-LLVM-exceptions UoI-NCSA
RDEPEND=>=sys-devel/clang-10.0.0_rc1:*
REQUIRED_USE=|| ( python_targets_python3_6 python_targets_python3_7 )
RESTRICT=!test? ( test )
SLOT=0
SRC_URI=https://github.com/llvm/llvm-project/archive/llvmorg-10.0.0-rc1.tar.gz
_eclasses_=llvm.org\t4e92abcd4e92abcd4e92abcd4e92abcd\tmultibuild\t40fe123440fe123440fe123440fe1234
_md5_=4539d849d3cea8ac84debad9b3154143
";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        // No arguments provided, use the built-in example
        let entry = CacheEntry::parse(EXAMPLE).expect("failed to parse cache entry");
        print_entry(&entry);
    } else {
        // Process each file argument
        for file_path in args {
            let content = fs::read_to_string(&file_path).expect("failed to read file");
            let entry = CacheEntry::parse(&content).expect("failed to parse cache entry");
            println!("=== File: {} ===", file_path);
            print_entry(&entry);
        }
    }
}

fn print_entry(entry: &CacheEntry) {
    let m = &entry.metadata;

    println!("=== Parsed Cache Entry ===");
    println!("EAPI:         {}", m.eapi);
    println!("Description:  {}", m.description);
    println!("Slot:         {}", m.slot);
    println!("Homepage:     {}", m.homepage.join(" "));
    println!(
        "Keywords:     {}",
        m.keywords
            .iter()
            .map(|k| k.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "IUSE:         {}",
        m.iuse
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    if let Some(ref license) = m.license {
        println!("License:      {}", license);
    }
    if let Some(ref ru) = m.required_use {
        println!("Required USE: {}", ru);
    }
    if !m.restrict.is_empty() {
        println!(
            "Restrict:     {}",
            m.restrict
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    println!(
        "Phases:       {}",
        m.defined_phases
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "DEPEND:       {}",
        m.depend
            .list()
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "RDEPEND:      {}",
        m.rdepend
            .list()
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "SRC_URI:      {}",
        m.src_uri
            .list()
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );

    if let Some(ref md5) = entry.md5 {
        println!("MD5:          {}", md5);
    }
    if !entry.eclasses.is_empty() {
        println!("Eclasses:");
        for (name, checksum) in &entry.eclasses {
            println!("  {name} -> {checksum:x}");
        }
    }

    println!("\n=== Serialized Back ===");
    print!("{}", entry.serialize());
}
