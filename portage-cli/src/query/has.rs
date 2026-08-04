//! `em query has` — list installed packages where a VDB field matches a value.
//!
//! Usage: `em query has <FIELD> [VALUE]`
//!
//! Searches the installed package database for packages whose metadata field
//! `FIELD` contains (or equals) `VALUE`.  When no `VALUE` is given, lists all
//! packages for which the field is non-empty.
//!
//! Examples:
//!   em query has SLOT 0          — packages installed in slot 0
//!   em query has repository      — packages with any repository set
//!   em query has USE lto         — packages built with the `lto` USE flag

use anyhow::{Result, bail};
use portage_vdb::Vdb;

pub fn run(vdb: &Vdb, args: &[String]) -> Result<()> {
    let (field, value) = match args {
        [] => bail!("em query has: expected <FIELD> [VALUE]"),
        [field] => (field.as_str(), None),
        [field, value, ..] => (field.as_str(), Some(value.as_str())),
    };

    for pkg in vdb.packages() {
        let raw = match pkg.field(field) {
            Ok(Some(v)) if !v.is_empty() => v,
            Ok(_) => continue,
            Err(e) => {
                crate::style::warn_line!("{pkg}: {e}");
                continue;
            }
        };

        let matched = match value {
            None => true,
            Some(want) => field_matches(&raw, want),
        };

        if matched {
            println!("{pkg}");
        }
    }
    Ok(())
}

fn field_matches(raw: &str, want: &str) -> bool {
    if raw.contains(' ') {
        raw.split_whitespace()
            .any(|token| token.trim_start_matches(['+', '-']) == want)
    } else {
        raw.trim() == want
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_matches_list() {
        assert!(field_matches("net nls readline", "nls"));
        assert!(!field_matches("net nls readline", "nl"));
        assert!(field_matches("+net +nls", "nls"));
    }

    #[test]
    fn field_matches_single() {
        assert!(field_matches("0", "0"));
        assert!(!field_matches("0/5.1", "0"));
        assert!(field_matches("gentoo", "gentoo"));
    }
}
