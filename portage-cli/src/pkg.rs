use std::io::Write as _;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Dep;
use portage_metadata::IUseDefault;
use portage_repo::{PackageConf, UseExpand};

use crate::cli::{Cli, PkgCommand};
use crate::query::depgraph::output::colorize_use_flag;
use crate::style::{C_DISABLED, C_LABEL, C_PKG, C_STABLE, C_TESTING};

/// The add/subtract/drop trichotomy, bundled so `edit_valued`/
/// `update_valued_entry`/`apply_flags` don't each carry three separate slice
/// parameters.
struct FlagOps<'a> {
    add: &'a [String],
    subtract: &'a [String],
    drop: &'a [String],
}

impl FlagOps<'_> {
    fn is_empty(&self) -> bool {
        self.add.is_empty() && self.subtract.is_empty() && self.drop.is_empty()
    }
}

pub async fn run(command: &PkgCommand, globals: &Cli) -> Result<()> {
    // Topology-aware: --config-root, then --prefix/--local overlay, then the
    // host's /etc/portage — same resolution `em select` uses.
    let confdir = crate::select::config_portage_dir(globals);
    match command {
        PkgCommand::Use {
            atom,
            add,
            subtract,
            drop,
            dry_run,
            info,
            path,
        } => {
            if !info.is_empty() {
                return show_flag_info(globals, atom, info);
            }
            let ops = FlagOps {
                add,
                subtract,
                drop,
            };
            let no_edit = ops.is_empty();
            edit_valued(
                &confdir,
                atom,
                &ops,
                path.as_deref(),
                "package.use",
                *dry_run,
                ValueStyle::UseFlag,
            )?;
            // Best-effort addendum, not core to package.use editing — a
            // repo/profile that fails to resolve must not break the plain
            // package.use show/edit this command has always supported.
            if no_edit
                && !*dry_run
                && let Err(e) = show_active_use(globals, &confdir, atom).await
            {
                crate::style::warn_line!("could not resolve active USE for {atom}: {e}");
            }
            Ok(())
        }
        PkgCommand::Keyword {
            atom,
            add,
            subtract,
            drop,
            path,
        } => edit_valued(
            &confdir,
            atom,
            &FlagOps {
                add,
                subtract,
                drop,
            },
            path.as_deref(),
            "package.accept_keywords",
            false,
            ValueStyle::Keyword,
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
            &FlagOps {
                add,
                subtract: &[],
                drop,
            },
            path.as_deref(),
            "package.env",
            false,
            ValueStyle::Plain,
        ),
    }
}

/// How `edit_valued`'s callers colorize the value tokens they show/write.
/// `Use`'s tokens are USE flags (bold red/blue on/off, same palette `em use`
/// uses); `Keyword`'s are KEYWORDS-shaped arch tokens (stable/testing/masked,
/// [`portage_metadata::Keyword::parse`]); `Env`'s are just filenames (plain).
#[derive(Clone, Copy)]
enum ValueStyle {
    UseFlag,
    Keyword,
    Plain,
}

impl ValueStyle {
    fn colorize(self, tok: &str) -> String {
        match self {
            ValueStyle::UseFlag => colorize_use_flag(tok),
            ValueStyle::Keyword => match portage_metadata::Keyword::<
                portage_atom::interner::DefaultInterner,
            >::parse(tok)
            {
                Ok(kw) => {
                    let style = match kw.stability {
                        portage_metadata::Stability::Stable => C_STABLE,
                        portage_metadata::Stability::Testing => C_TESTING,
                        portage_metadata::Stability::Disabled
                        | portage_metadata::Stability::DisabledAll => C_DISABLED,
                    };
                    format!("{style}{tok}{style:#}")
                }
                // Not a standard `~arch`/`arch`/`-arch` token (e.g. `**`,
                // `-*`) — shown plain rather than guessing a color.
                Err(_) => tok.to_string(),
            },
            ValueStyle::Plain => tok.to_string(),
        }
    }

    fn colorize_list(self, values: &[String]) -> String {
        values
            .iter()
            .map(|v| self.colorize(v))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn edit_valued(
    confdir: &Utf8Path,
    atom_str: &str,
    ops: &FlagOps,
    path_override: Option<&Utf8Path>,
    conf_name: &str,
    dry_run: bool,
    style: ValueStyle,
) -> Result<()> {
    let atom = Dep::parse(atom_str).with_context(|| format!("invalid atom {atom_str:?}"))?;

    let base = confdir.join(conf_name);
    let no_edit = ops.is_empty();

    if base.is_dir() {
        let mut all = PackageConf::load_dir(&base).with_context(|| format!("reading {base}"))?;

        let matches: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, (_, pc))| pc.find(&atom).is_some())
            .map(|(i, _)| i)
            .collect();

        if no_edit {
            show_valued_dir(&all, &matches, &atom, conf_name, style);
            return Ok(());
        }

        match matches.len() {
            0 => {
                let new_values = apply_flags(Vec::new(), ops);
                if !dry_run && !new_values.is_empty() {
                    let target = resolve_new_path(&base, &atom, path_override);
                    let mut pc = if target.exists() {
                        PackageConf::load_file(&target)
                            .with_context(|| format!("reading {target}"))?
                    } else {
                        PackageConf::parse(String::new())?
                    };
                    let refs: Vec<&str> = new_values.iter().map(String::as_str).collect();
                    pc.set(&atom, &refs);
                    pc.save(&target)
                        .with_context(|| format!("writing {target}"))?;
                }
                print_value_diff(&atom, &[], &new_values, style);
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
                update_valued_entry(pc, file, &atom, ops, dry_run, style)?;
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
            show_valued_single(&pc, &atom, &base, conf_name, style);
            return Ok(());
        }

        update_valued_entry(&mut pc, &base, &atom, ops, dry_run, style)?;
    }

    Ok(())
}

fn update_valued_entry(
    pc: &mut PackageConf,
    file: &Utf8Path,
    atom: &Dep,
    ops: &FlagOps,
    dry_run: bool,
    style: ValueStyle,
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

    let new_values = apply_flags(current.clone(), ops);

    if !dry_run {
        if new_values.is_empty() {
            pc.remove(atom);
        } else {
            let refs: Vec<&str> = new_values.iter().map(String::as_str).collect();
            pc.set(atom, &refs);
        }
        pc.save(file).with_context(|| format!("writing {file}"))?;
    }

    print_value_diff(atom, &current, &new_values, style);
    Ok(())
}

/// Print `atom old -> new`, colorized values, matching `em use`'s own
/// old→new diff shape — `old`/`new` empty renders as `(none)`.
fn print_value_diff(atom: &Dep, old: &[String], new: &[String], style: ValueStyle) {
    let render = |values: &[String]| {
        if values.is_empty() {
            "(none)".to_string()
        } else {
            style.colorize_list(values)
        }
    };

    let mut out = anstream::stdout();
    if old == new {
        writeln!(out, "{C_PKG}{atom}{C_PKG:#} {} (unchanged)", render(old)).ok();
        return;
    }
    writeln!(out, "{C_PKG}{atom}{C_PKG:#} {}", render(old)).ok();
    writeln!(out, "  -> {}", render(new)).ok();
}

fn show_valued_dir(
    all: &[(Utf8PathBuf, PackageConf)],
    matches: &[usize],
    atom: &Dep,
    conf_name: &str,
    style: ValueStyle,
) {
    let mut out = anstream::stdout();
    if matches.is_empty() {
        writeln!(out, "{conf_name}: no entry for {C_PKG}{atom}{C_PKG:#}").ok();
        return;
    }
    for &i in matches.iter() {
        let (ref file, ref pc) = all[i];
        let fname = file.file_name().unwrap_or("?");
        for entry in pc.find_all(atom) {
            let values: Vec<String> = entry.values().map(str::to_owned).collect();
            if values.is_empty() {
                writeln!(out, "[{fname}] {C_PKG}{}{C_PKG:#}", entry.atom_raw()).ok();
            } else {
                writeln!(
                    out,
                    "[{fname}] {C_PKG}{}{C_PKG:#} {}",
                    entry.atom_raw(),
                    style.colorize_list(&values)
                )
                .ok();
            }
        }
    }
}

fn show_valued_single(
    pc: &PackageConf,
    atom: &Dep,
    file: &Utf8Path,
    conf_name: &str,
    style: ValueStyle,
) {
    let fname = file.file_name().unwrap_or("?");
    let mut out = anstream::stdout();
    let mut found = false;
    for entry in pc.find_all(atom) {
        found = true;
        let values: Vec<String> = entry.values().map(str::to_owned).collect();
        if values.is_empty() {
            writeln!(out, "[{fname}] {C_PKG}{}{C_PKG:#}", entry.atom_raw()).ok();
        } else {
            writeln!(
                out,
                "[{fname}] {C_PKG}{}{C_PKG:#} {}",
                entry.atom_raw(),
                style.colorize_list(&values)
            )
            .ok();
        }
    }
    if !found {
        writeln!(out, "{conf_name}: no entry for {C_PKG}{atom}{C_PKG:#}").ok();
    }
}

/// `-i/--info FLAG`: show `flag`'s description for `atom`'s package —
/// [`portage_repo::UseDb::describe`] (this package's local
/// `use.local.desc`/metadata.xml-derived description first, falling back to
/// the repo-wide `profiles/use.desc`).
fn show_flag_info(cli: &Cli, atom_str: &str, flags: &[String]) -> Result<()> {
    let atom = Dep::parse(atom_str).with_context(|| format!("invalid atom {atom_str:?}"))?;
    let cpn = atom.cpn.to_string();

    let repo = crate::crossdev::main_repo(cli).context("opening main repo")?;
    let use_db = repo.use_db().unwrap_or_default();

    let mut out = anstream::stdout();
    for flag in flags {
        match use_db.describe(&cpn, flag) {
            Some(desc) => writeln!(out, "{C_LABEL}{flag}{C_LABEL:#}  {desc}").ok(),
            None => writeln!(out, "{C_LABEL}{flag}{C_LABEL:#}  (no description)").ok(),
        };
    }
    Ok(())
}

/// Bare `em pkg use <atom>` addendum (printed after the package.use entries
/// themselves): the best-effort resolved USE for `atom`'s best-matching
/// candidate ebuild — IUSE defaults folded with the profile/make.conf global
/// USE (the same `EbuildShell` resolution `em use`'s bare summary and
/// `em --info` use), then this atom's own package.use entries on top.
///
/// Approximate: unlike the real solver this applies no use.force/use.mask or
/// REQUIRED_USE, so a force/masked flag may show the "wrong" state here —
/// good enough for a quick glance, not a substitute for a real `-p` resolve.
async fn show_active_use(cli: &Cli, confdir: &Utf8Path, atom_str: &str) -> Result<()> {
    let atom = Dep::parse(atom_str).with_context(|| format!("invalid atom {atom_str:?}"))?;

    let repo = crate::crossdev::main_repo(cli).context("opening main repo")?;
    let ebuilds: Vec<_> = repo.ebuilds()?.into_iter().collect();
    let set = portage_repo::RepoSet::single(repo);
    let repo = set.main();

    let matches = crate::query::matching_ebuilds(
        &set,
        None,
        crate::query::ResolveMode::Error,
        &ebuilds,
        atom_str,
    )?;
    let Some(best) = matches.last() else {
        return Ok(());
    };
    let cpv = best.cpv();
    let Some(entry) = repo.cache_entry(cpv)? else {
        return Ok(());
    };

    let roots = cli.roots();
    let mut shell = repo.shell().await.context("creating shell")?;
    let has_profile =
        crate::ebuild::apply_profile_env(&mut shell, roots.config(), roots.config_overlay())
            .await
            .context("resolving active profile")?;
    if !has_profile {
        return Ok(());
    }

    let global_use: Vec<String> = shell
        .get_var("USE")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let expand_names: Vec<String> = shell
        .get_var("USE_EXPAND")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();

    let package_use = package_use_values(confdir, &atom);

    let active: Vec<String> = entry
        .metadata
        .iuse
        .iter()
        .map(|flag| {
            let name = flag.name();
            if use_state(name, &package_use, &global_use, flag.default) {
                name.to_string()
            } else {
                format!("-{name}")
            }
        })
        .collect();

    let expand = UseExpand::new(&expand_names);
    let mut groups = expand.group(active.iter().map(String::as_str));
    let mut base: Vec<&str> = groups.remove("global").unwrap_or_default();
    base.sort_unstable();

    let mut out = anstream::stdout();
    writeln!(out, "Active USE for {C_PKG}{cpv}{C_PKG:#}:").ok();
    if !base.is_empty() {
        let rendered: Vec<String> = base.iter().copied().map(colorize_use_flag).collect();
        writeln!(out, "  {C_LABEL}USE{C_LABEL:#}=\"{}\"", rendered.join(" ")).ok();
    }
    for (group, mut values) in groups {
        if values.is_empty() {
            continue;
        }
        values.sort_unstable();
        let var = group.to_uppercase();
        let rendered: Vec<String> = values.iter().copied().map(colorize_use_flag).collect();
        writeln!(
            out,
            "  {C_LABEL}{var}{C_LABEL:#}=\"{}\"",
            rendered.join(" ")
        )
        .ok();
    }
    Ok(())
}

/// Fold order for [`show_active_use`]: this atom's own package.use entries
/// win, then global (profile+make.conf) USE, then the ebuild's own IUSE
/// default.
fn use_state(
    flag: &str,
    package_use: &[String],
    global_use: &[String],
    default: Option<IUseDefault>,
) -> bool {
    for tok in package_use {
        if tok == flag {
            return true;
        }
        if tok.strip_prefix('-') == Some(flag) {
            return false;
        }
    }
    for tok in global_use {
        if tok == flag {
            return true;
        }
        if tok.strip_prefix('-') == Some(flag) {
            return false;
        }
    }
    matches!(default, Some(IUseDefault::Enabled))
}

/// This atom's current package.use values, unioned across every matching
/// entry — ambiguity is tolerated here (unlike `update_valued_entry`'s
/// strict single-entry requirement) since this is a read-only best-effort
/// fold for [`show_active_use`], not an edit.
fn package_use_values(confdir: &Utf8Path, atom: &Dep) -> Vec<String> {
    let base = confdir.join("package.use");
    let entries: Vec<(Utf8PathBuf, PackageConf)> = if base.is_dir() {
        PackageConf::load_dir(&base).unwrap_or_default()
    } else if base.exists() {
        PackageConf::load_file(&base)
            .map(|pc| vec![(base.clone(), pc)])
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    entries
        .iter()
        .flat_map(|(_, pc)| pc.find_all(atom))
        .flat_map(|e| e.values().map(str::to_owned).collect::<Vec<_>>())
        .collect()
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

fn apply_flags(mut values: Vec<String>, ops: &FlagOps) -> Vec<String> {
    for op in ops.add.iter().chain(ops.subtract).chain(ops.drop) {
        let base = op.trim_start_matches('-');
        values.retain(|v| {
            let vbase = v.trim_start_matches('-');
            vbase != base
        });
    }
    for flag in ops.add {
        let base = flag.trim_start_matches('-');
        values.push(base.to_owned());
    }
    for flag in ops.subtract {
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
            &FlagOps {
                add: &["static-libs".into()],
                subtract: &[],
                drop: &[],
            },
            None,
            "package.use",
            false,
            ValueStyle::UseFlag,
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
            &FlagOps {
                add: &["static-libs".into()],
                subtract: &[],
                drop: &[],
            },
            None,
            "package.use",
            false,
            ValueStyle::UseFlag,
        )
        .expect("edit");

        let written = std::fs::read_to_string(conf.join("package.use/sys-libs-zlib"))
            .expect("per-package file written");
        assert!(written.contains("sys-libs/zlib static-libs"), "{written}");
    }

    /// `--dry-run` must compute the same result the write path would
    /// otherwise produce, but the file itself must stay untouched.
    #[test]
    fn edit_valued_dry_run_does_not_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let conf = confdir(&tmp);

        edit_valued(
            &conf,
            "sys-libs/zlib",
            &FlagOps {
                add: &["static-libs".into()],
                subtract: &[],
                drop: &[],
            },
            None,
            "package.use",
            true,
            ValueStyle::UseFlag,
        )
        .expect("dry-run edit");

        assert!(!conf.join("package.use").exists());
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
