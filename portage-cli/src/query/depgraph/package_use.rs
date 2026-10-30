use std::io::Write as _;

use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::{Cpv, Dep};
use portage_atom_pubgrub::UseDemand;
use portage_repo::ConfigFilesMode;
pub(super) use portage_resolve::package_use::{
    CosolveOutcome, FlagPin, MatchedFlag, PackageUseEntry, UseConflict, build_entries,
    cosolve_use_deps, use_dep_spelling, write,
};

use super::output::{C_DIM, C_OFF, C_ON, C_PKG};

/// Print the required USE changes to stderr in portage style
pub(super) fn report(entries: &[PackageUseEntry]) {
    if entries.is_empty() {
        return;
    }
    let mut out = anstream::stderr();
    writeln!(
        out,
        "\nThe following {C_ON}USE changes{C_ON:#} are necessary to proceed:"
    )
    .ok();
    writeln!(
        out,
        " (see \"package.use\" in the portage(5) man page for more details)"
    )
    .ok();
    for entry in entries {
        for line in &entry.lines {
            for comment in &line.comments {
                writeln!(out, "{C_DIM}{comment}{C_DIM:#}").ok();
            }
            let flag_str: String = line
                .flags
                .iter()
                .map(|f| {
                    if f.starts_with('-') {
                        format!("{C_OFF}{f}{C_OFF:#}")
                    } else {
                        format!("{C_ON}{f}{C_ON:#}")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            writeln!(out, "{C_PKG}{}{C_PKG:#} {flag_str}", line.atom).ok();
        }
    }
}

/// Print the flags demanded both ways that no USE change settles
pub(super) fn report_conflicts(conflicts: &[UseConflict], package_use_path: &Utf8Path) {
    if conflicts.is_empty() {
        return;
    }
    let mut out = anstream::stderr();
    writeln!(
        out,
        "\n{C_OFF}!!!{C_OFF:#} USE flag conflict(s) detected:\n"
    )
    .ok();
    for c in conflicts {
        let slot = c
            .package
            .slot()
            .map(|s| format!(":{}", s.as_str()))
            .unwrap_or_default();
        writeln!(
            out,
            "  {C_PKG}{}-{}{slot}{C_PKG:#}  {}",
            c.package.cpn(),
            c.version,
            c.flag.as_str()
        )
        .ok();
        let parents: Vec<String> = c
            .demands
            .iter()
            .map(|(d, _)| format!("{}-{}", d.parent.cpn(), d.parent_version))
            .collect();
        let width = parents.iter().map(String::len).max().unwrap_or(0);
        for ((d, pin), parent) in c.demands.iter().zip(&parents) {
            let value = if d.enable { "on" } else { "off" };
            let label = if pin.is_some() {
                format!("forced {value} by:")
            } else {
                format!("needed {value} by:")
            };
            let mut why = format!("{}[{}]", c.package.cpn(), use_dep_spelling(d));
            if let Some(pin) = pin {
                why.push_str("; ");
                why.push_str(&pin_reason(*pin, d, package_use_path));
            }
            writeln!(
                out,
                "    {label:<16}{C_PKG}{parent:<width$}{C_PKG:#} ({why})"
            )
            .ok();
        }
    }
}

/// What holds the parent flag behind `d` at the value that makes it lose
fn pin_reason(pin: FlagPin, d: &UseDemand, package_use_path: &Utf8Path) -> String {
    let flag = d.flag.as_str();
    let held = if d.parent_value_to_agree() == Some(true) {
        format!("-{flag}")
    } else {
        flag.to_string()
    };
    match pin {
        FlagPin::Config => match setting_file(d, package_use_path) {
            Some(file) => format!("{held} set in {file}"),
            None => format!("{held} set in package.use"),
        },
        FlagPin::Masked => format!("{flag} masked by the profile"),
        FlagPin::Forced => format!("{flag} forced by the profile"),
        FlagPin::Decided => format!("{held} needed by another dependency"),
    }
}

/// The last `package.use` file with a line setting the parent's flag behind `d`
fn setting_file(d: &UseDemand, package_use_path: &Utf8Path) -> Option<Utf8PathBuf> {
    let cpv = Cpv::new(*d.parent.cpn(), d.parent_version.clone());
    let slot = d.parent.slot().map(portage_atom::Slot::from_name);
    let flag = d.flag.as_str();
    let files = portage_repo::list_config_files(package_use_path, ConfigFilesMode::Flat).ok()?;
    files.into_iter().rev().find_map(|file| {
        let lines = portage_repo::read_config_lines(&file).ok()?;
        let sets = lines.iter().any(|line| {
            let mut parts = line.split_whitespace();
            parts
                .next()
                .and_then(|atom| Dep::parse(atom).ok())
                .is_some_and(|dep| dep.matches_cpv(&cpv, slot.as_ref()))
                && parts.any(|f| f.strip_prefix('-').unwrap_or(f) == flag)
        });
        sets.then(|| Utf8PathBuf::from_path_buf(file).ok())
            .flatten()
    })
}
