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
/// The `[oldversion]` column in `-p` output — the version(s) being replaced
/// (real emerge's `convert_myoldbest`, which paints it bold blue: `\x1b[34;01m`).
pub const C_OLDVERSION: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Blue)))
    .effects(Effects::BOLD);
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

// ── Banner / diagnostic palette (Portage `>>>` / `!!!`) ───────────────────
// The `>>>` info tag is deliberately plain (uncoloured) — matching both real
// portage's own interactive banners and the ~50 existing `>>>` call sites
// across this codebase (the primary `>>> Installing`/`>>> Completed` merge
// banners included); `!!!`, bold + coloured, is what's meant to stand out.
/// Soft failure / warning prefix (`!!!`) — real portage's own `PORTAGE_COLOR_WARN`
/// (`isolated-functions.sh`) is `\e[33;01m`, i.e. plain yellow, bold — not the
/// xterm-256 orange this used to be.
pub const C_WARN: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Yellow)))
    .effects(Effects::BOLD);
/// Hard failure prefix (`!!!`) and error text — matches real portage's
/// `PORTAGE_COLOR_BAD`/`PORTAGE_COLOR_ERR` (`\e[31;01m`, red, bold).
pub const C_ERROR: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Red)))
    .effects(Effects::BOLD);

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

/// Print a warning line to stderr: a colored `!!!` banner ([`C_WARN`],
/// orange) followed by `msg` — the uniform replacement for a bare,
/// uncoloured `eprintln!("warning: …")`. For a non-fatal problem where the
/// caller carries on regardless (an optional side effect failed, an item was
/// skipped in a batch, …).
///
/// Goes through [`anstream::stderr`] like the rest of this module's output,
/// so the color strips itself when stderr is not a terminal — including any
/// [`C_PKG`]/[`C_BOLD`]/… styling already baked into `msg` (e.g. from an
/// [`anyhow::Error`]'s `Display`): `anstream` strips ANSI escapes from the
/// byte stream itself, regardless of where in the pipeline they were added.
pub fn warn_line(msg: &str) {
    use std::io::Write;
    let mut out = anstream::stderr();
    let _ = writeln!(out, "{C_WARN}!!!{C_WARN:#} {msg}");
    let _ = out.flush();
}

/// Print an error line to stderr: a colored `!!!` banner ([`C_ERROR`], red)
/// followed by `msg` — the uniform replacement for a bare, uncoloured
/// `eprintln!("error: …")` or a plain `eprintln!("!!! …")`. For an item's
/// requested operation that definitively failed, whether or not the overall
/// command carries on to the next item.
pub fn error_line(msg: &str) {
    use std::io::Write;
    let mut out = anstream::stderr();
    let _ = writeln!(out, "{C_ERROR}!!!{C_ERROR:#} {msg}");
    let _ = out.flush();
}

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
