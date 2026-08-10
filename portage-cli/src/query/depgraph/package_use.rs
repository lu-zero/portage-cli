use std::io::Write as _;

pub(super) use portage_resolve::package_use::{
    PackageUseEntry, build_entries, cosolve_use_deps, write,
};

use super::output::{C_DIM, C_OFF, C_ON, C_PKG};

/// Print the required USE changes to stderr in portage style.
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
