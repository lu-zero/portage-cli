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

/// Portage's `PORTAGE_COLOR_*` palette for the `e*` builtins a build phase
/// calls, mapped onto this module's own constants — every code verified against
/// `portage/output.py`'s `_styles` table (`INFO`=`darkgreen`→`32m`,
/// `LOG`/`GOOD`=`green`→`32;01m`, `WARN`=`yellow`→`33;01m`,
/// `QAWARN`=`brown`→`33m`, `ERR`/`BAD`=`red`→`31;01m`,
/// `BRACKET`=`blue`→`34;01m`).
pub const PORTAGE_COLORS: portage_repo::PortageColors = portage_repo::PortageColors {
    info: C_MARKER_INFO,
    log: C_GOOD,
    warn: C_WARN,
    qawarn: C_QAWARN,
    err: C_ERROR,
    bracket: C_BRACKET,
    good: C_GOOD,
    bad: C_ERROR,
};

/// How a build phase should see the console: this process's width, and
/// portage's palette unless colour is off.
///
/// `portage-repo` never asks the terminal anything itself — this is the one
/// place the answer is resolved, mirroring the split real portage keeps between
/// its Python and bash halves.
pub fn terminal_config() -> portage_repo::TerminalConfig {
    portage_repo::TerminalConfig {
        columns: term_width(),
        colors: if crate::diag::stdout_wants_color() {
            PORTAGE_COLORS
        } else {
            portage_repo::PortageColors::default()
        },
    }
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
/// A `-t`/`--tree` row shown only for dependency context, not actually being
/// (re)built — real emerge's `PKG_NOMERGE` (`portage/output.py`: `"teal"` ==
/// `0x00AAAA`, plain — not bold — ANSI cyan). Distinct from [`C_PKG`]/
/// [`C_PKG_BINARY`], which are for rows that *are* merging.
pub const C_PKG_NOMERGE: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan)));
/// `_SELECTED`: a package explicitly named on the command line (`em`'s own
/// simplification of real emerge's `PKG_MERGE_WORLD`/`PKG_NOMERGE_WORLD`/
/// `PKG_BINARY_MERGE_WORLD` — real portage bolds a row only when it's
/// actually tracked in `@world`, which needs a live `ProfileStack`/
/// `SetResolver`; `em` bolds every explicit target unconditionally instead,
/// installed or not, so a fresh `em mesa` still highlights `mesa` — see
/// `query::depgraph::output::PrettyCtx::selected`'s doc). Same three hues
/// real portage's own `_WORLD` variants use (`portage/output.py`: `"green"`
/// = `0x55FF55` bold, `"fuchsia"` = `0xFF55FF` bold, `"blue"` = `0x5555FF`
/// bold) — note `_NOMERGE_SELECTED` is bold **blue**, not a bolded teal.
pub const C_PKG_SELECTED: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
pub const C_PKG_BINARY_SELECTED: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Magenta)))
    .effects(Effects::BOLD);
pub const C_PKG_NOMERGE_SELECTED: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Blue)))
    .effects(Effects::BOLD);
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

/// `einfo`'s `" * "` marker colour — real portage's `EOutput._styles`
/// `INFO`=`"darkgreen"`, which `portage/output.py`'s `codes` dict maps to
/// `0x00AA00` → plain ANSI 32: green, *not* bold.
///
/// There is deliberately no `C_MARKER_WARN`/`C_MARKER_ERROR` counterpart:
/// `ewarn`'s `"yellow"` (`0xFFFF55` → `33;01m`) and `eerror`'s `"red"`
/// (`0xFF5555` → `31;01m`) are the same bold codes as [`C_WARN`]/[`C_ERROR`]
/// above, so the marker reuses those.
pub const C_MARKER_INFO: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green)));
/// `eend`'s bracket color (`portage/output.py`'s `BRACKET` = `"blue"` →
/// `0x5555FF` → `34;01m`, bold blue) — encloses the `"ok"`/`"!!"` text in
/// `"[ ok ]"`/`"[ !! ]"`.
pub const C_BRACKET: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Blue)))
    .effects(Effects::BOLD);
/// `eend`'s success text color (`portage/output.py`'s `GOOD` = `"green"` →
/// `0x55FF55` → `32;01m`, bold green — distinct from [`C_MARKER_INFO`]'s
/// plain `darkgreen`; portage uses two different greens here).
pub const C_GOOD: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
/// `eqawarn`'s marker colour (`portage/output.py`'s `QAWARN` = `"brown"` →
/// `33m`, plain yellow) — deliberately one shade quieter than [`C_WARN`]'s
/// bold, since a QA notice is aimed at the ebuild's author, not its user.
pub const C_QAWARN: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow)));
/// The default answer in a `[y/N]`-style confirm prompt — real portage's
/// `PROMPT_CHOICE_DEFAULT` (plain green, not bold; `UserQuery.query`'s
/// default `colours=[PROMPT_CHOICE_DEFAULT, PROMPT_CHOICE_OTHER]`).
pub const C_CHOICE_DEFAULT: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green)));
/// The non-default answer in a `[y/N]`-style confirm prompt — real portage's
/// `PROMPT_CHOICE_OTHER` (plain red, not bold).
pub const C_CHOICE_OTHER: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red)));

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

// ── Console line emitters ─────────────────────────────────────────────────
//
// Each form below has a `*_impl` function (the writer) and a same-named
// `macro_rules!` (the public interface, mirroring std's `println!`): callers
// always go through the macro form (`style::warn_line!("…", args)`), never
// the bare function. The macro forwards its args via `format_args!` (zero-
// allocation `fmt::Arguments`), NOT `format!` (which would build a temporary
// `String` only for the function's own `writeln!` to re-format — a double
// format pass for every line). The `_impl` function is what the macro
// expands to, kept `pub(crate)` (not hidden) so it can be named via
// `$crate::style::…` regardless of the caller's `use` site; do not call it
// directly.

/// Underlying writer for [`warn_line!`] — print a warning line to stderr:
/// a colored `!!!` banner ([`C_WARN`], yellow) followed by `args`. The
/// uniform replacement for a bare, uncoloured `eprintln!("warning: …")`.
/// For a non-fatal problem where the caller carries on regardless (an
/// optional side effect failed, an item was skipped in a batch, …).
///
/// Goes through [`anstream::stderr`] like the rest of this module's output,
/// so the color strips itself when stderr is not a terminal — including any
/// [`C_PKG`]/[`C_BOLD`]/… styling already baked into `args` (e.g. from an
/// [`anyhow::Error`]'s `Display`): `anstream` strips ANSI escapes from the
/// byte stream itself, regardless of where in the pipeline they were added.
pub(crate) fn warn_line_impl(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let mut out = anstream::stderr();
    let _ = writeln!(out, "{C_WARN}!!!{C_WARN:#} {args}");
    let _ = out.flush();
}

/// Underlying writer for [`error_line!`] — print an error line to stderr:
/// a colored `!!!` banner ([`C_ERROR`], red) followed by `args`. The uniform
/// replacement for a bare, uncoloured `eprintln!("error: …")` or a plain
/// `eprintln!("!!! …")`. For an item's requested operation that definitively
/// failed, whether or not the overall command carries on to the next item.
pub(crate) fn error_line_impl(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let mut out = anstream::stderr();
    let _ = writeln!(out, "{C_ERROR}!!!{C_ERROR:#} {args}");
    let _ = out.flush();
}

/// Underlying writer for [`einfo_line!`] — print an informational line to
/// **stdout** in real portage's `einfo` shape: `" * {args}"`, the marker in
/// [`C_MARKER_INFO`]. The stdout-bound mirror of the ` * ` marker
/// [`crate::diag::CompactFormatter`] renders for an `INFO`-level `tracing`
/// event on stderr.
///
/// Prefer `tracing::info!` when the message belongs on stderr with the rest
/// of the build status flow; this helper is only for structured stdout
/// reports whose stream contract is part of the command's primary output
/// (e.g. `revdep`/`preserve_libs`'s `>>> package: <cpv>` / ` * ...` shape,
/// where a pipe consumer may want the report separate from diagnostics).
/// `args` is the text after the marker — the `" * "` prefix is added here.
pub(crate) fn einfo_line_impl(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let mut out = anstream::stdout();
    let _ = writeln!(out, "{C_MARKER_INFO} * {C_MARKER_INFO:#}{args}");
    let _ = out.flush();
}

/// Underlying writer for [`ewarn_sub_bullet!`] — print an indented `einfo`
/// sub-bullet to **stderr** with the marker in [`C_WARN`] (`ewarn`'s own
/// colour): `"  * {args}"`, plain when stderr is not a terminal. Used for
/// structured sub-item listings under a `>>>`-banner that itself carries the
/// overall warning header (e.g. the per-`cpv` failure list under `>>> N
/// package(s) failed to merge:` in the merge summary) — a single
/// [`tracing::warn!`] per item would lose the visual hierarchy and prefix
/// every continuation line, so this keeps the listing shape while still
/// painting the marker to match every other WARN-flavoured ` * `.
pub(crate) fn ewarn_sub_bullet_impl(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let mut out = anstream::stderr();
    let _ = writeln!(out, "  {C_WARN}*{C_WARN:#} {args}");
    let _ = out.flush();
}

// ── Public macro forms (println!-style format-arg ergonomics) ─────────────
//
// The macro IS the public interface (mirrors std's `println!`): call sites
// write `style::warn_line!("{} failed: {e}", path)` instead of the older
// `style::warn_line(&format!("{} failed: {e}", path))` boilerplate. A bare
// string still works (`warn_line!("literal")`), and any `Display` value can
// be inlined directly (`warn_line!("{}", e)` replaces the older
// `warn_line(&e.to_string())`).
//
// Args are forwarded to the writer via `format_args!` (zero-allocation
// `fmt::Arguments`), not `format!` (which would build a temporary `String`
// only for the writer's own `writeln!` to re-format — a double format pass
// for every line; `writeln!(… "{}", fmt::Arguments)` is a single pass).

/// `warn_line!("…", args…)` — see [`warn_line_impl`] (the writer).
macro_rules! warn_line {
    ($($arg:tt)*) => {
        $crate::style::warn_line_impl(::std::format_args!($($arg)*))
    };
}

/// `error_line!("…", args…)` — see [`error_line_impl`] (the writer).
macro_rules! error_line {
    ($($arg:tt)*) => {
        $crate::style::error_line_impl(::std::format_args!($($arg)*))
    };
}

/// `einfo_line!("…", args…)` — see [`einfo_line_impl`] (the writer).
macro_rules! einfo_line {
    ($($arg:tt)*) => {
        $crate::style::einfo_line_impl(::std::format_args!($($arg)*))
    };
}

/// `ewarn_sub_bullet!("…", args…)` — see [`ewarn_sub_bullet_impl`] (the writer).
macro_rules! ewarn_sub_bullet {
    ($($arg:tt)*) => {
        $crate::style::ewarn_sub_bullet_impl(::std::format_args!($($arg)*))
    };
}

pub(crate) use {einfo_line, error_line, ewarn_sub_bullet, warn_line};

/// Render one line in real portage's `ebegin`/`eend` shape (`portage/
/// output.py`'s `EOutput`): `" * {msg} ..."` immediately followed by a
/// right-aligned `"[ ok ]"`/`"[ !! ]"` bracket — e.g. what real portage shows
/// for a fetch's already-verified-distfile check
/// (`eout.ebegin(f"{file} size ;-)"); eout.eend(0)`).
///
/// Unlike real portage's own `ebegin`(prints immediately, no newline)/`eend`
/// (completes the same line later), this renders the whole line in one call:
/// every caller here already has the outcome in hand by the time it prints
/// anything (e.g. `Fetcher::fetch_all` gathers every distfile's result
/// before any of them get reported), so there's no live gap to bridge and,
/// just as importantly, no risk of two concurrent checks' half-written
/// "began" lines interleaving on the terminal.
///
/// `width` is the terminal width ([`term_width`]); `ansi` whether to color
/// the bracket (real portage's `BRACKET`=blue, `GOOD`=green, `BAD`=red).
pub fn estatus_line(msg: &str, ok: bool, width: usize, ansi: bool) -> String {
    let plain_head = format!(" * {msg} ...");
    // Matches EOutput.__eend's own "%*s%s" % (term_columns - last_e_len - 7, "", brackets):
    // last_e_len is this line's own length so far; -7 is "[ ok ]"/"[ !! ]"'s
    // 6 characters plus a 1-column margin so an exact-width terminal doesn't
    // wrap. Measured on the plain text: ANSI escapes occupy no columns on
    // screen and must never count toward the width.
    let pad = width.saturating_sub(plain_head.chars().count() + 7).max(1);
    let padding = " ".repeat(pad);
    if ansi {
        let result = if ok { C_GOOD } else { C_ERROR };
        let result_text = if ok { "ok" } else { "!!" };
        format!(
            "{C_MARKER_INFO} * {C_MARKER_INFO:#}{msg} ...{padding}\
             {C_BRACKET}[ {C_BRACKET:#}{result}{result_text}{result:#}{C_BRACKET} ]{C_BRACKET:#}"
        )
    } else {
        let bracket = if ok { "[ ok ]" } else { "[ !! ]" };
        format!("{plain_head}{padding}{bracket}")
    }
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

    /// Right edge lands one column short of `width` (real portage's own
    /// `EOutput.__eend` deliberately leaves a 1-column margin so an
    /// exact-width terminal doesn't wrap) — checked plain (no ANSI padding
    /// characters to throw off a byte-length check).
    #[test]
    fn estatus_line_ok_right_aligns_one_short_of_width() {
        let line = estatus_line("fetch: binutils-2.46.1.tar.xz", true, 80, false);
        assert_eq!(line.len(), 79);
        assert!(line.starts_with(" * fetch: binutils-2.46.1.tar.xz ..."));
        assert!(line.ends_with("[ ok ]"));
    }

    #[test]
    fn estatus_line_failure_uses_the_failure_bracket() {
        let line = estatus_line("fetch: foo.tar.gz", false, 80, false);
        assert!(line.ends_with("[ !! ]"));
    }

    #[test]
    fn estatus_line_colors_only_under_ansi() {
        let plain = estatus_line("x", true, 80, false);
        assert!(!plain.contains('\x1b'));
        let colored = estatus_line("x", true, 80, true);
        assert!(colored.contains("\x1b[32mok"), "{colored:?}");
    }

    /// A message longer than the terminal must not underflow the padding
    /// arithmetic (`saturating_sub` + a `.max(1)` floor) — must still
    /// terminate with the bracket, not panic.
    #[test]
    fn estatus_line_survives_a_narrow_terminal() {
        let line = estatus_line("a very long message indeed", true, 10, false);
        assert!(line.ends_with("[ ok ]"));
    }
}
