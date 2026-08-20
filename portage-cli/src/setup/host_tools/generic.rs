//! Hosts with GNU tools on the plain `PATH`
//!
//! These hosts install GNU tools through the ordinary system package
//! manager, so the tools are already on `PATH` under their plain names and
//! nothing but `PATH` needs searching.

use camino::Utf8PathBuf;

pub(super) fn candidates(_brew: Option<&str>, _name: &str) -> Vec<Utf8PathBuf> {
    Vec::new()
}

pub(super) fn install_hint(_brew: Option<&str>, name: &str) -> String {
    format!("Install {name} with this system's package manager, then re-run em setup.")
}
