//! Scan an installed image for ELF dynamic-link metadata, producing portage's
//! `NEEDED`, `NEEDED.ELF.2`, `REQUIRES` and `PROVIDES` VDB fields.
//!
//! Reads each ELF's dynamic section with the [`object`] crate (no `scanelf`):
//! `DT_NEEDED` (the comma list), `DT_SONAME` (what a library provides) and
//! `DT_RPATH`/`DT_RUNPATH`. `REQUIRES` is the needed sonames minus the ones the
//! package itself provides, grouped by portage's multilib category.
//!
//! [`scan_image`] walks the image once, then parses candidate files in parallel
//! (flume worker pool sized to `available_parallelism`), matching the pattern
//! used for ebuild sourcing / cache regen.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use camino::Utf8Path;
use object::elf;
use object::read::elf::{Dyn, FileHeader, ProgramHeader, SectionHeader};
use object::{Endianness, FileKind};

/// The four ELF metadata fields, ready to write into the VDB.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
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

/// Per-file dynamic-link metadata (internal + benches).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElfInfo {
    /// ELF machine name for `NEEDED.ELF.2` (e.g. `X86_64`).
    pub machine: &'static str,
    /// Portage multilib category (e.g. `x86_64`).
    pub category: String,
    /// `DT_SONAME`, if any.
    pub soname: Option<String>,
    /// `DT_RPATH` / `DT_RUNPATH`, if any.
    pub rpath: Option<String>,
    /// `DT_NEEDED` sonames.
    pub needed: Vec<String>,
}

/// Parse one on-disk file's ELF dynamic-link metadata, or `None` if it isn't
/// a file, isn't readable, or isn't a dynamic ELF. Used both by
/// [`scan_image`] (per entry, while walking a build image) and by
/// `preserve_libs` (to re-derive the link metadata of a registry-carried
/// preserved lib, which — having outlived its owning VDB entry — is no
/// longer listed in any package's `NEEDED.ELF.2`).
pub(crate) fn scan_file(path: &Path) -> Option<ElfInfo> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    // Cheap reject before a full read: ELF magic only.
    {
        use std::io::Read;
        let mut f = std::fs::File::open(path).ok()?;
        let mut magic = [0u8; 4];
        f.read_exact(&mut magic).ok()?;
        if magic != *b"\x7fELF" {
            return None;
        }
    }
    let data = std::fs::read(path).ok()?;
    parse_elf(&data)
}

/// Walk `image_dir` and collect ELF link metadata for every dynamic ELF object.
///
/// Worker count defaults to [`std::thread::available_parallelism`]. Use
/// [`scan_image_with_jobs`] for serial (`Some(1)`) microbenchmarks.
pub fn scan_image(image_dir: &Utf8Path) -> ElfScan {
    scan_image_with_jobs(image_dir, None)
}

/// Like [`scan_image`], but with an explicit worker count (`None` = available
/// parallelism, `Some(1)` = fully serial — useful for A/B benches).
pub fn scan_image_with_jobs(image_dir: &Utf8Path, jobs: Option<usize>) -> ElfScan {
    let image_root = image_dir.as_std_path();
    let paths = collect_regular_files(image_root);
    let entries = scan_paths_parallel(image_root, paths, jobs);
    assemble_scan(entries)
}

/// Scan an arbitrary list of absolute paths under `image_root` (for benches).
pub fn scan_paths_parallel(
    image_root: &Path,
    paths: Vec<PathBuf>,
    jobs: Option<usize>,
) -> Vec<(String, ElfInfo)> {
    let jobs = jobs
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        })
        .max(1);

    if jobs == 1 || paths.len() < 32 {
        return scan_paths_serial(image_root, paths);
    }

    let image_root = Arc::new(image_root.to_path_buf());
    // Bounded work queue; each worker accumulates locally so a full result
    // channel cannot stall producers (classic bounded in+out deadlock).
    let (tx, rx) = flume::bounded::<PathBuf>(jobs * 4);

    let mut handles = Vec::with_capacity(jobs);
    for _ in 0..jobs {
        let rx = rx.clone();
        let root = Arc::clone(&image_root);
        handles.push(std::thread::spawn(move || {
            let mut local = Vec::new();
            while let Ok(entry) = rx.recv() {
                let Some(info) = scan_file(&entry) else {
                    continue;
                };
                let rel = entry
                    .strip_prefix(root.as_path())
                    .unwrap_or(&entry)
                    .to_string_lossy();
                let install = format!("/{}", rel.trim_start_matches('/'));
                local.push((install, info));
            }
            local
        }));
    }
    drop(rx);

    for p in paths {
        let _ = tx.send(p);
    }
    drop(tx);

    let mut entries: Vec<(String, ElfInfo)> = Vec::new();
    for h in handles {
        if let Ok(local) = h.join() {
            entries.extend(local);
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

fn scan_paths_serial(image_root: &Path, paths: Vec<PathBuf>) -> Vec<(String, ElfInfo)> {
    let mut entries = Vec::new();
    for entry in paths {
        let Some(info) = scan_file(&entry) else {
            continue;
        };
        let rel = entry
            .strip_prefix(image_root)
            .unwrap_or(&entry)
            .to_string_lossy();
        let install = format!("/{}", rel.trim_start_matches('/'));
        entries.push((install, info));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

/// Build the four VDB field lists from sorted `(install_path, info)` pairs.
pub fn assemble_scan(entries: Vec<(String, ElfInfo)>) -> ElfScan {
    let mut scan = ElfScan::default();
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

/// Collect regular files under `root` (skips dirs and unreadable entries;
/// follows the same non-symlink-dir recursion as the old serial walk).
pub fn collect_regular_files(root: &Path) -> Vec<PathBuf> {
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
                Ok(m) if m.is_file() => out.push(p),
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn scan_image_parallel_matches_serial_on_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let a = scan_image_with_jobs(root, Some(1));
        let b = scan_image_with_jobs(root, Some(4));
        assert_eq!(a, b);
        assert!(a.needed_elf2.is_empty());
    }

    #[test]
    fn scan_image_skips_non_elf() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("readme.txt"), b"hello").unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let scan = scan_image(root);
        assert!(scan.needed_elf2.is_empty());
    }

    #[test]
    fn parallel_and_serial_agree_on_shared_objects() {
        // Use a real system lib dir if present; otherwise skip.
        let lib = Path::new("/usr/lib64");
        if !lib.is_dir() {
            return;
        }
        // Cap work: only a handful of files for unit test speed.
        let mut paths = collect_regular_files(lib);
        paths.truncate(64);
        if paths.is_empty() {
            return;
        }
        let serial = scan_paths_serial(lib, paths.clone());
        let parallel = scan_paths_parallel(lib, paths, Some(4));
        assert_eq!(serial, parallel);
    }

    #[test]
    fn magic_reject_does_not_require_full_read() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("not-elf.bin");
        // Large non-ELF — would be expensive to fully parse as ELF.
        let mut data = vec![0u8; 1 << 20];
        data[0] = b'N';
        std::fs::write(&p, &data).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(scan_file(&p).is_none());
    }
}
