//! Shared terminal styles for `em` output — the single source of truth, so
//! colours stay consistent across applets. Plain `anstyle::Style` values that
//! interpolate in format strings with anstream writers
//! (`"{C_PKG}foo{C_PKG:#}"`); anstream strips them when output is not a TTY.
//!
//! Add palette entries here (and a mapping helper if a domain enum drives the
//! choice, e.g. [`profile_status`]) rather than constructing styles inline.

use anstyle::{AnsiColor, Color, Effects, Style};

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
