use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Dep;
use portage_repo::PackageConf;

use crate::cli::{Cli, PkgCommand};

pub fn run(command: &PkgCommand, globals: &Cli) -> Result<()> {
    // Topology-aware: --config-root, then --prefix/--local overlay, then the
    // host's /etc/portage — same resolution `em select` uses.
    let confdir = crate::select::config_portage_dir(globals);
    match command {
        PkgCommand::Use {
            atom,
            add,
            subtract,
            drop,
            path,
        } => edit_valued(
            &confdir,
            atom,
            add,
            subtract,
            drop,
            path.as_deref(),
            "package.use",
        ),
        PkgCommand::Keyword {
            atom,
            add,
            subtract,
            drop,
            path,
        } => edit_valued(
            &confdir,
            atom,
            add,
            subtract,
            drop,
            path.as_deref(),
            "package.accept_keywords",
        ),
        PkgCommand::Mask {
            atom,
            add,
            drop,
            path,
        } => edit_mask(&confdir, atom, *add, *drop, path.as_deref()),
        PkgCommand::Env {
            atom,
            add,
            drop,
            path,
        } => edit_valued(
            &confdir,
            atom,
            add,
            &[],
            drop,
            path.as_deref(),
            "package.env",
        ),
    }
}

fn edit_valued(
    confdir: &Utf8Path,
    atom_str: &str,
    add: &[String],
    subtract: &[String],
    drop: &[String],
    path_override: Option<&Utf8Path>,
    conf_name: &str,
) -> Result<()> {
    let atom = Dep::parse(atom_str).with_context(|| format!("invalid atom {atom_str:?}"))?;

    let base = confdir.join(conf_name);
    let no_edit = add.is_empty() && subtract.is_empty() && drop.is_empty();

    if base.is_dir() {
        let mut all = PackageConf::load_dir(&base).with_context(|| format!("reading {base}"))?;

        let matches: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, (_, pc))| pc.find(&atom).is_some())
            .map(|(i, _)| i)
            .collect();

        if no_edit {
            show_valued_dir(&all, &matches, &atom, conf_name);
            return Ok(());
        }

        match matches.len() {
            0 => {
                let target = resolve_new_path(&base, &atom, path_override);
                let mut pc = if target.exists() {
                    PackageConf::load_file(&target).with_context(|| format!("reading {target}"))?
                } else {
                    PackageConf::parse(String::new())?
                };
                let current: Vec<String> = vec![];
                let new_values = apply_flags(current, add, subtract, drop);
                if new_values.is_empty() {
                    println!("{conf_name}: no entry for {atom}");
                } else {
                    let refs: Vec<&str> = new_values.iter().map(String::as_str).collect();
                    pc.set(&atom, &refs);
                    pc.save(&target)
                        .with_context(|| format!("writing {target}"))?;
                    println!("{atom} {}", new_values.join(" "));
                }
            }
            1 => {
                let idx = matches[0];
                let (ref file, ref mut pc) = all[idx];
                if let Some(path_override) = path_override {
                    let target = base.join(path_override);
                    if &target != file {
                        crate::style::warn_line!(
                            "entry found in {}, ignoring --path",
                            file.file_name().unwrap_or("?")
                        );
                    }
                }
                update_valued_entry(pc, file, &atom, add, subtract, drop, conf_name)?;
            }
            _ => {
                crate::style::error_line!("atom found in multiple files:");
                for &i in &matches {
                    eprintln!("  {}", all[i].0);
                }
                eprintln!("Specify --path to edit one explicitly.");
                bail!("ambiguous entries for {atom}");
            }
        }
    } else {
        let mut pc = if base.exists() {
            PackageConf::load_file(&base).with_context(|| format!("reading {base}"))?
        } else {
            PackageConf::parse(String::new())?
        };

        if no_edit {
            show_valued_single(&pc, &atom, &base, conf_name);
            return Ok(());
        }

        update_valued_entry(&mut pc, &base, &atom, add, subtract, drop, conf_name)?;
    }

    Ok(())
}

fn update_valued_entry(
    pc: &mut PackageConf,
    file: &Utf8Path,
    atom: &Dep,
    add: &[String],
    subtract: &[String],
    drop: &[String],
    conf_name: &str,
) -> Result<()> {
    let all_entries: Vec<_> = pc.find_all(atom).collect();
    if all_entries.len() > 1 && atom.version.is_none() {
        crate::style::error_line!(
            "multiple entries for {atom} in {}:",
            file.file_name().unwrap_or("?")
        );
        for e in &all_entries {
            let values: Vec<&str> = e.values().collect();
            if values.is_empty() {
                eprintln!("  {}", e.atom_raw());
            } else {
                eprintln!("  {} {}", e.atom_raw(), values.join(" "));
            }
        }
        eprintln!("Use a versioned atom to edit a specific entry.");
        bail!("ambiguous CPN for {atom}");
    }

    let current: Vec<String> = all_entries
        .into_iter()
        .next()
        .map(|e| e.values().map(str::to_owned).collect())
        .unwrap_or_default();

    let new_values = apply_flags(current, add, subtract, drop);

    if new_values.is_empty() {
        pc.remove(atom);
        println!("{conf_name}: removed entry for {atom}");
    } else {
        let refs: Vec<&str> = new_values.iter().map(String::as_str).collect();
        pc.set(atom, &refs);
        println!("{atom} {}", new_values.join(" "));
    }

    pc.save(file).with_context(|| format!("writing {file}"))
}

fn show_valued_dir(
    all: &[(Utf8PathBuf, PackageConf)],
    matches: &[usize],
    atom: &Dep,
    conf_name: &str,
) {
    if matches.is_empty() {
        println!("{conf_name}: no entry for {atom}");
        return;
    }
    for &i in matches.iter() {
        let (ref file, ref pc) = all[i];
        let fname = file.file_name().unwrap_or("?");
        for entry in pc.find_all(atom) {
            let values: Vec<&str> = entry.values().collect();
            if values.is_empty() {
                println!("[{fname}] {}", entry.atom_raw());
            } else {
                println!("[{fname}] {} {}", entry.atom_raw(), values.join(" "));
            }
        }
    }
}

fn show_valued_single(pc: &PackageConf, atom: &Dep, file: &Utf8Path, conf_name: &str) {
    let fname = file.file_name().unwrap_or("?");
    let mut found = false;
    for entry in pc.find_all(atom) {
        found = true;
        let values: Vec<&str> = entry.values().collect();
        if values.is_empty() {
            println!("[{fname}] {}", entry.atom_raw());
        } else {
            println!("[{fname}] {} {}", entry.atom_raw(), values.join(" "));
        }
    }
    if !found {
        println!("{conf_name}: no entry for {atom}");
    }
}

fn edit_mask(
    confdir: &Utf8Path,
    atom_str: &str,
    add: bool,
    drop: bool,
    path_override: Option<&Utf8Path>,
) -> Result<()> {
    let atom = Dep::parse(atom_str).with_context(|| format!("invalid atom {atom_str:?}"))?;

    let base = confdir.join("package.mask");

    if base.is_dir() {
        let mut all = PackageConf::load_dir(&base).with_context(|| format!("reading {base}"))?;

        let matches: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, (_, pc))| pc.find(&atom).is_some())
            .map(|(i, _)| i)
            .collect();

        if !add && !drop {
            if matches.is_empty() {
                println!("package.mask: {atom} is not masked");
            } else {
                for &i in &matches {
                    let fname = all[i].0.file_name().unwrap_or("?");
                    println!("masked in [{fname}]: {atom}");
                }
            }
            return Ok(());
        }

        if drop {
            match matches.len() {
                0 => println!("package.mask: {atom} not found"),
                1 => {
                    let (ref file, ref mut pc) = all[matches[0]];
                    pc.remove(&atom);
                    pc.save(file).with_context(|| format!("writing {file}"))?;
                    println!("removed {atom} from {}", file.file_name().unwrap_or("?"));
                }
                _ => {
                    crate::style::error_line!("atom found in multiple files:");
                    for &i in &matches {
                        eprintln!("  {}", all[i].0);
                    }
                    eprintln!("Specify --path to edit one explicitly.");
                    bail!("ambiguous mask entries for {atom}");
                }
            }
        } else {
            let target = resolve_new_path(&base, &atom, path_override);
            let mut pc = if target.exists() {
                PackageConf::load_file(&target).with_context(|| format!("reading {target}"))?
            } else {
                PackageConf::parse(String::new())?
            };
            if pc.find(&atom).is_some() {
                println!(
                    "package.mask: {atom} already masked in {}",
                    target.file_name().unwrap_or("?")
                );
            } else {
                pc.set(&atom, &[]);
                pc.save(&target)
                    .with_context(|| format!("writing {target}"))?;
                println!("masked {atom} in {}", target.file_name().unwrap_or("?"));
            }
        }
    } else {
        let mut pc = if base.exists() {
            PackageConf::load_file(&base).with_context(|| format!("reading {base}"))?
        } else {
            PackageConf::parse(String::new())?
        };

        if !add && !drop {
            if pc.find(&atom).is_some() {
                println!("package.mask: {atom} is masked");
            } else {
                println!("package.mask: {atom} is not masked");
            }
            return Ok(());
        }

        if drop {
            if pc.remove(&atom) {
                pc.save(&base).with_context(|| format!("writing {base}"))?;
                println!("removed {atom} from package.mask");
            } else {
                println!("package.mask: {atom} not found");
            }
        } else {
            pc.set(&atom, &[]);
            pc.save(&base).with_context(|| format!("writing {base}"))?;
            println!("masked {atom}");
        }
    }

    Ok(())
}

fn apply_flags(
    mut values: Vec<String>,
    add: &[String],
    subtract: &[String],
    drop: &[String],
) -> Vec<String> {
    for op in add.iter().chain(subtract).chain(drop) {
        let base = op.trim_start_matches('-');
        values.retain(|v| {
            let vbase = v.trim_start_matches('-');
            vbase != base
        });
    }
    for flag in add {
        let base = flag.trim_start_matches('-');
        values.push(base.to_owned());
    }
    for flag in subtract {
        let base = flag.trim_start_matches('-');
        values.push(format!("-{base}"));
    }
    values
}

fn resolve_new_path(
    base_dir: &Utf8Path,
    atom: &Dep,
    path_override: Option<&Utf8Path>,
) -> Utf8PathBuf {
    if let Some(p) = path_override {
        if p.is_absolute() {
            return p.to_owned();
        }
        return base_dir.join(p);
    }
    let stem = format!("{}-{}", atom.cpn.category, atom.cpn.package);
    base_dir.join(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confdir(tmp: &tempfile::TempDir) -> Utf8PathBuf {
        Utf8Path::from_path(tmp.path())
            .expect("utf8 tempdir")
            .to_owned()
    }

    /// The regression this module was root-blind for: edits must land under
    /// the passed config dir, never the host's /etc/portage.
    #[test]
    fn edit_valued_writes_under_the_given_confdir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let conf = confdir(&tmp);

        edit_valued(
            &conf,
            "sys-libs/zlib",
            &["static-libs".into()],
            &[],
            &[],
            None,
            "package.use",
        )
        .expect("edit");

        let written = std::fs::read_to_string(conf.join("package.use")).expect("file written");
        assert!(written.contains("sys-libs/zlib static-libs"), "{written}");
    }

    #[test]
    fn edit_valued_dir_mode_creates_a_per_package_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let conf = confdir(&tmp);
        std::fs::create_dir(conf.join("package.use")).expect("mkdir");

        edit_valued(
            &conf,
            "sys-libs/zlib",
            &["static-libs".into()],
            &[],
            &[],
            None,
            "package.use",
        )
        .expect("edit");

        let written = std::fs::read_to_string(conf.join("package.use/sys-libs-zlib"))
            .expect("per-package file written");
        assert!(written.contains("sys-libs/zlib static-libs"), "{written}");
    }

    #[test]
    fn edit_mask_writes_under_the_given_confdir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let conf = confdir(&tmp);

        edit_mask(&conf, "sys-libs/zlib", true, false, None).expect("mask");

        let written = std::fs::read_to_string(conf.join("package.mask")).expect("file written");
        assert!(written.contains("sys-libs/zlib"), "{written}");
    }
}
