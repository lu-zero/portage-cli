//! Shared terminal styles for `em` output — the single source of truth, so
//! colours stay consistent across applets. Plain `anstyle::Style` values that
//! interpolate in format strings with anstream writers
//! (`"{C_PKG}foo{C_PKG:#}"`); anstream strips them when output is not a TTY.
//!
//! Add palette entries here (and a mapping helper if a domain enum drives the
//! choice, e.g. [`profile_status`]) rather than constructing styles inline.

use anstyle::{AnsiColor, Color, Effects, Style};

/// Usable output width: the real terminal's, or 80 when it has none (a pipe,
/// a log file, a CI runner).
pub fn term_width() -> usize {
    terminal_size::terminal_size().map_or(80, |(terminal_size::Width(w), _)| w as usize)
}

/// Wrap `items` into space-separated lines that fit `width`, each line after
/// the first indented by `indent`. Never splits an item, so a single item
/// longer than the width simply overflows its line.
pub fn wrap_items(items: &[String], indent: usize, width: usize) -> Vec<String> {
    let budget = width.saturating_sub(indent).max(1);
    let mut lines: Vec<String> = Vec::new();
    for item in items {
        match lines.last_mut() {
            Some(line) if line.len() + 1 + item.len() <= budget => {
                line.push(' ');
                line.push_str(item);
            }
            _ => lines.push(item.clone()),
        }
    }
    lines
}

// ── Package / label palette ────────────────────────────────────────────────
/// Package atoms and general "primary" text.
pub const C_PKG: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green)));
/// Field labels and list indices.
pub const C_LABEL: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green)));
/// Emphasis without colour.
pub const C_BOLD: Style = Style::new().effects(Effects::BOLD);
/// "Current selection" marker (`*`).
pub const C_STAR: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
/// Masked / error emphasis.
pub const C_MASKED: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Red)))
    .effects(Effects::BOLD);
/// Category half of a `cat/pkg` (subdued).
pub const C_CAT: Style = Style::new().effects(Effects::DIMMED);
/// Package-name half of a `cat/pkg`.
pub const C_PKGNAME: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightGreen)));
/// Version strings.
pub const C_VERSION: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green)));
/// Prefix profile source label.
pub const C_PREFIX: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan)));
/// Host profile source label.
pub const C_HOST: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow)));
/// Binary-package cpv in merge banners (real emerge's `PKG_BINARY_MERGE`).
pub const C_PKG_BINARY: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Magenta)));
/// `(N of M)` progress counters in merge banners (real emerge's
/// `MERGE_LIST_PROGRESS`, which is yellow — not to be confused with the
/// testing-keyword yellow below, same colour, different meaning).
pub const C_COUNT: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow)));

// ── Stability / status palette (stable=green, testing/dev=yellow, …=red) ────
/// Stable keyword / stable profile.
pub const C_STABLE: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
/// Testing keyword / dev profile.
pub const C_TESTING: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Yellow)))
    .effects(Effects::BOLD);
/// Disabled keyword / experimental profile.
pub const C_DISABLED: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Red)))
    .effects(Effects::BOLD);

/// Style for a profile's stability status (same palette as keyword stability).
pub fn profile_status(status: &portage_repo::ProfileStatus) -> Style {
    use portage_repo::ProfileStatus::*;
    match status {
        Stable => C_STABLE,
        Dev => C_TESTING,
        Exp => C_DISABLED,
        Other(_) => Style::new().effects(Effects::DIMMED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Budget is `width - indent`, so 10 here: `aaa bbb` fits, `aaa bbb ccc`
    /// (11) does not.
    #[test]
    fn wraps_to_the_budget_left_after_the_indent() {
        let got = wrap_items(&items(&["aaa", "bbb", "ccc", "ddd"]), 2, 12);
        assert_eq!(got, vec!["aaa bbb", "ccc ddd"]);
    }

    #[test]
    fn keeps_everything_on_one_line_when_it_fits() {
        let got = wrap_items(&items(&["aaa", "bbb"]), 0, 80);
        assert_eq!(got, vec!["aaa bbb"]);
    }

    #[test]
    fn never_splits_an_item_that_cannot_fit() {
        let got = wrap_items(&items(&["a-very-long-single-item", "x"]), 4, 10);
        assert_eq!(got, vec!["a-very-long-single-item", "x"]);
    }

    #[test]
    fn an_indent_wider_than_the_terminal_still_makes_progress() {
        let got = wrap_items(&items(&["a", "b"]), 200, 80);
        assert_eq!(got, vec!["a", "b"]);
    }

    #[test]
    fn no_items_means_no_lines() {
        assert!(wrap_items(&[], 4, 80).is_empty());
    }
}
