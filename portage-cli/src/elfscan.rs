//! Scan an installed image for ELF dynamic-link metadata, producing portage's
//! `NEEDED`, `NEEDED.ELF.2`, `REQUIRES` and `PROVIDES` VDB fields.
//!
//! Reads each ELF's dynamic section with the [`object`] crate (no `scanelf`):
//! `DT_NEEDED` (the comma list), `DT_SONAME` (what a library provides) and
//! `DT_RPATH`/`DT_RUNPATH`. `REQUIRES` is the needed sonames minus the ones the
//! package itself provides, grouped by portage's multilib category.

use std::collections::{BTreeMap, BTreeSet};

use camino::Utf8Path;
use object::elf;
use object::read::elf::{Dyn, FileHeader, ProgramHeader, SectionHeader};
use object::{Endianness, FileKind};

/// The four ELF metadata fields, ready to write into the VDB.
#[derive(Debug, Default)]
pub struct ElfScan {
    /// Legacy `NEEDED`: `<path> <needed,comma>` per ELF.
    pub needed: Vec<String>,
    /// `NEEDED.ELF.2`: `<MACHINE>;<path>;<SONAME>;<RPATH>;<needed,comma>;<cat>`.
    pub needed_elf2: Vec<String>,
    /// `REQUIRES`: `<cat>: <sonames>` (needed minus provided).
    pub requires: Vec<String>,
    /// `PROVIDES`: `<cat>: <sonames>` (the package's own DT_SONAMEs).
    pub provides: Vec<String>,
}

pub(crate) struct ElfInfo {
    pub machine: &'static str,
    pub category: String,
    pub soname: Option<String>,
    pub rpath: Option<String>,
    pub needed: Vec<String>,
}

/// Parse one on-disk file's ELF dynamic-link metadata, or `None` if it isn't
/// a file, isn't readable, or isn't a dynamic ELF. Used both by
/// [`scan_image`] (per entry, while walking a build image) and by
/// `preserve_libs` (to re-derive the link metadata of a registry-carried
/// preserved lib, which — having outlived its owning VDB entry — is no
/// longer listed in any package's `NEEDED.ELF.2`).
pub(crate) fn scan_file(path: &std::path::Path) -> Option<ElfInfo> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    parse_elf(&data)
}

/// Walk `image_dir` and collect ELF link metadata for every dynamic ELF object.
pub fn scan_image(image_dir: &Utf8Path) -> ElfScan {
    let mut entries: Vec<(String, ElfInfo)> = Vec::new();
    for entry in walkdir(image_dir.as_std_path()) {
        let Some(info) = scan_file(&entry) else {
            continue;
        };
        // Install path = path under the image, with a leading `/`.
        let rel = entry
            .strip_prefix(image_dir.as_std_path())
            .unwrap_or(&entry)
            .to_string_lossy();
        let install = format!("/{}", rel.trim_start_matches('/'));
        entries.push((install, info));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut scan = ElfScan::default();
    // provided sonames per category (for the REQUIRES subtraction).
    let mut provides: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut requires: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for (path, info) in &entries {
        let needed = info.needed.join(",");
        scan.needed.push(format!("{path} {needed}"));
        scan.needed_elf2.push(format!(
            "{};{};{};{};{};{}",
            info.machine,
            path,
            info.soname.as_deref().unwrap_or(""),
            info.rpath.as_deref().unwrap_or(""),
            needed,
            info.category,
        ));
        let cat = info.category.clone();
        if let Some(so) = &info.soname {
            provides.entry(cat.clone()).or_default().insert(so.clone());
        }
        let r = requires.entry(cat).or_default();
        for n in &info.needed {
            r.insert(n.clone());
        }
    }
    // REQUIRES = needed − provided, per category.
    for (cat, mut req) in requires {
        if let Some(prov) = provides.get(&cat) {
            for p in prov {
                req.remove(p);
            }
        }
        if !req.is_empty() {
            scan.requires.push(format!(
                "{cat}: {}",
                req.into_iter().collect::<Vec<_>>().join(" ")
            ));
        }
    }
    for (cat, prov) in &provides {
        scan.provides.push(format!(
            "{cat}: {}",
            prov.iter().cloned().collect::<Vec<_>>().join(" ")
        ));
    }
    scan
}

/// Parse one file as an ELF, returning its link metadata, or `None` when it
/// isn't a dynamic ELF.
fn parse_elf(data: &[u8]) -> Option<ElfInfo> {
    match FileKind::parse(data).ok()? {
        FileKind::Elf32 => extract::<elf::FileHeader32<Endianness>>(data),
        FileKind::Elf64 => extract::<elf::FileHeader64<Endianness>>(data),
        _ => None,
    }
}

fn extract<Elf: FileHeader<Endian = Endianness>>(data: &[u8]) -> Option<ElfInfo> {
    let header = Elf::parse(data).ok()?;
    let endian = header.endian().ok()?;
    let machine = header.e_machine(endian);
    let is_64 = header.is_type_64();
    let (machine_name, category) = arch(machine, is_64);

    // The dynamic string table backs the DT_NEEDED/SONAME/RPATH name offsets.
    let sections = header.sections(endian, data).ok()?;
    let (dynamic, dynamic_index) = header
        .program_headers(endian, data)
        .ok()?
        .iter()
        .find_map(|ph| ph.dynamic(endian, data).ok().flatten().map(|d| (d, ph)))?;
    let _ = dynamic_index;
    let strings = sections
        .section_by_name(endian, b".dynstr")
        .and_then(|(_, s)| s.data(endian, data).ok())
        .map(|d| object::StringTable::new(d, 0, d.len() as u64))?;

    let mut needed = Vec::new();
    let mut soname = None;
    let mut rpath = None;
    for d in dynamic {
        let Some(tag) = d.tag32(endian) else { continue };
        let val = match d.string(endian, strings) {
            Ok(s) => String::from_utf8_lossy(s).into_owned(),
            Err(_) => continue,
        };
        match tag {
            elf::DT_NEEDED => needed.push(val),
            elf::DT_SONAME => soname = Some(val),
            elf::DT_RPATH | elf::DT_RUNPATH => rpath = Some(val),
            _ => {}
        }
    }
    Some(ElfInfo {
        machine: machine_name,
        category,
        soname,
        rpath,
        needed,
    })
}

/// `(MACHINE name, portage multilib category)` for an ELF machine + class,
/// matching portage's `NEEDED.ELF.2` arch field and soname-dep categories.
fn arch(e_machine: u16, is_64: bool) -> (&'static str, String) {
    let (name, cat) = match e_machine {
        elf::EM_AARCH64 => ("AARCH64", "arm_64"),
        elf::EM_ARM => ("ARM", "arm_32"),
        elf::EM_X86_64 if is_64 => ("X86_64", "x86_64"),
        elf::EM_X86_64 => ("X86_64", "x86_32"),
        elf::EM_386 => ("386", "x86_32"),
        elf::EM_PPC64 => ("PPC64", "ppc_64"),
        elf::EM_PPC => ("PPC", "ppc_32"),
        elf::EM_RISCV if is_64 => ("RISCV", "riscv_lp64d"),
        elf::EM_RISCV => ("RISCV", "riscv_ilp32d"),
        elf::EM_S390 if is_64 => ("S390", "s390_64"),
        elf::EM_S390 => ("S390", "s390_32"),
        elf::EM_SPARCV9 => ("SPARC", "sparc_64"),
        elf::EM_SPARC | elf::EM_SPARC32PLUS => ("SPARC", "sparc_32"),
        elf::EM_LOONGARCH => ("LOONGARCH", "loong_64"),
        elf::EM_MIPS if is_64 => ("MIPS", "mips_n64"),
        elf::EM_MIPS => ("MIPS", "mips_o32"),
        _ => ("UNKNOWN", "unknown"),
    };
    (name, cat.to_string())
}

/// Iterative directory walk yielding every entry path under `root`.
fn walkdir(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            match std::fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => stack.push(p),
                Ok(_) => out.push(p),
                Err(_) => {}
            }
        }
    }
    out
}
