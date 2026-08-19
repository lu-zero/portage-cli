//! Portage's P1 output helpers, following `isolated-functions.sh`
//!
//! The palette and the width come from [`TerminalState`] rather than from
//! `PORTAGE_COLOR_*`/`COLS` shell variables: portage exports those only because
//! its own helpers are bash, and nothing in the ebuild tree reads them (checked
//! against every eclass and ebuild in `::gentoo`). `COLUMNS` and `NOCOLOR` are
//! still exported — those *do* have real consumers outside this file.

use std::io::Write;

use anstyle::Style;
use brush_core::builtins;
use clap::Parser;

use crate::build::terminal::{PortageColors, TerminalState};

/// The current palette, or a plain one if the shell has no terminal state
fn colors<SE: brush_core::ShellExtensions>(
    context: &brush_core::ExecutionContext<'_, SE>,
) -> PortageColors {
    context
        .shared::<TerminalState>()
        .map(|t| t.get().colors)
        .unwrap_or_default()
}

/// Portage's message prefix: a space, the coloured `*`, a space
///
/// Only the `*` is painted — the surrounding spaces are not, matching
/// `echo " ${PORTAGE_COLOR_INFO}*${PORTAGE_COLOR_NORMAL} ${REPLY}"`.
fn marker(style: Style) -> String {
    format!(" {style}*{style:#} ")
}

/// Expand backslash escapes the way `echo -e` does
///
/// Every one of portage's `e*` helpers renders its message through
/// `echo -e "$@"` (and records it through the same), so `ewarn "a\nb"` is two
/// lines to a user of real portage. Applied once, before the message is either
/// printed or recorded, so the two never disagree.
///
/// `\c` truncates the rest, as in `echo -e`. Unknown escapes are left alone,
/// backslash and all — again matching `echo -e`.
fn expand_escapes(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut chars = msg.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('a') => out.push('\x07'),
            Some('b') => out.push('\x08'),
            Some('e') => out.push('\x1b'),
            Some('f') => out.push('\x0c'),
            Some('v') => out.push('\x0b'),
            Some('\\') => out.push('\\'),
            Some('c') => break,
            // Not an escape `echo -e` knows: keep it verbatim.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Portage's `__elog_base`: append `<CLASS> <line>` to
/// `${T}/logging/${EBUILD_PHASE}`, the file the elog system replays once the
/// package is merged.
///
/// Capture is switched on by that directory *existing* — `run_phase` creates it
/// per package, so metadata sourcing (which has no build dir) records nothing,
/// exactly as `[[ -z "${1}" || -z "${T}" || ! -d "${T}/logging" ]] && return 1`
/// arranges. `T` and `EBUILD_PHASE` are read from the shell rather than held in
/// Rust state because both are per-phase values the shell already owns.
///
/// A file, not an in-memory buffer, because `pkg_preinst`/`pkg_postinst` run in
/// the `em __worker` subprocess: their messages have no other way back.
fn elog_base<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    class: &str,
    msg: &str,
) {
    if msg.is_empty() {
        return;
    }
    let Some(t) = shell.env_str("T").filter(|t| !t.is_empty()) else {
        return;
    };
    let dir = std::path::Path::new(t.as_ref()).join("logging");
    if !dir.is_dir() {
        return;
    }
    let phase = shell
        .env_str("EBUILD_PHASE")
        .filter(|p| !p.is_empty())
        .map_or_else(|| "other".to_string(), |p| p.into_owned());

    let mut body = String::new();
    for line in msg.split('\n') {
        body.push_str(class);
        body.push(' ');
        body.push_str(line);
        body.push('\n');
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(phase))
        .and_then(|mut f| f.write_all(body.as_bytes()));
}

/// `einfo/einfon/elog/ewarn/eerror/eqawarn <message>`
///
/// Prints ` * <message>` to stderr. All six share this implementation and
/// differ only in which colour paints the `*` — plus `einfon`, which omits the
/// trailing newline (that is what the `n` is for) and, like portage's, does not
/// split its message across lines.
#[derive(Parser)]
pub(crate) struct EchoMessageCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for EchoMessageCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let palette = colors(&context);
        // Colour and elog class travel together — one match so the two can't
        // drift apart.
        let (style, class) = match context.command_name.as_str() {
            "ewarn" => (palette.warn, "WARN"),
            "eerror" => (palette.err, "ERROR"),
            "eqawarn" => (palette.qawarn, "QA"),
            "elog" => (palette.log, "LOG"),
            _ => (palette.info, "INFO"),
        };
        let einfon = context.command_name == "einfon";
        let prefix = marker(style);
        let msg = expand_escapes(&self.message.join(" "));
        elog_base(context.shell, class, &msg);

        let shell = context.shell;
        let mut out = context.params.stderr(shell);
        if einfon {
            let _ = write!(out, "{prefix}{msg}");
        } else {
            // A multi-line message gets the marker on every line, as portage's
            // `while read -r` loop does.
            for line in msg.split('\n') {
                let _ = writeln!(out, "{prefix}{line}");
            }
        }
        Ok(brush_core::ExecutionResult::success())
    }
}

/// `ebegin <message>`
///
/// Prints ` * <message> ...` to stderr (beginning of a timed action); `eend`
/// closes it.
#[derive(Parser)]
pub(crate) struct EbeginCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for EbeginCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let prefix = marker(colors(&context).info);
        // Portage's `ebegin` renders through `einfon "${msg} ..."`, so the
        // recorded entry is INFO and carries the trailing dots too — the
        // message elog files is the one the user saw.
        let msg = format!("{} ...", expand_escapes(&self.message.join(" ")));
        elog_base(context.shell, "INFO", &msg);
        let shell = context.shell;
        let _ = writeln!(context.params.stderr(shell), "{prefix}{msg}");
        Ok(brush_core::ExecutionResult::success())
    }
}

/// Portage's `ENDCOL`: move the cursor up one line, then `columns - 8` to the
/// right, so `eend`'s indicator lands at the end of the line `ebegin` wrote.
///
/// anstyle models SGR (colour, bold) and nothing else, so cursor motion has no
/// shared constant to reuse; this is the one place that emits it. `columns` is
/// portage's `COLS`, and `saturating_sub` keeps a terminal narrower than the
/// indicator from producing a negative column (portage emits `\e[-7C` there).
fn endcol(columns: usize) -> String {
    format!("\x1b[A\x1b[{}C", columns.saturating_sub(8))
}

/// `eend [exit_code] [message]`
///
/// Closes an `ebegin` with `[ ok ]` (exit_code 0) or `[ !! ]` (non-zero,
/// preceded by `message` rendered as an `eerror`), right-aligned onto the line
/// `ebegin` wrote. Returns `exit_code`.
///
/// Portage hardcodes `RC_ENDCOL="yes"`, so the indicator is always placed by
/// cursor motion rather than by padding — and with colour off `ENDCOL` is empty,
/// which degrades to the indicator on its own line. Both branches are here.
#[derive(Parser)]
pub(crate) struct EendCommand {
    exit_code: Option<u8>,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for EendCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let terminal = context
            .shared::<TerminalState>()
            .map(TerminalState::get)
            .unwrap_or_default();
        let palette = terminal.colors;
        let code = self.exit_code.unwrap_or(0);

        // `__eend` emits its failure diagnostic through `eerror`, so it is
        // captured as an ERROR entry exactly like a direct `eerror` call.
        let failure_msg = (code != 0)
            .then(|| expand_escapes(&self.message.join(" ")))
            .filter(|m| !m.is_empty());
        if let Some(msg) = &failure_msg {
            elog_base(context.shell, "ERROR", msg);
        }

        let shell = context.shell;
        let mut out = context.params.stderr(shell);

        // Switch colour between segments and reset once at the end, exactly as
        // portage's `"${BRACKET}[ ${GOOD}ok${BRACKET} ]${NORMAL}"` does —
        // closing each segment instead would triple the escapes for no visible
        // difference. The trailing `{br:#}` is the `NORMAL`, and renders empty
        // on a plain palette, so this stays escape-free with colour off.
        let indicator = if code == 0 {
            let good = palette.good;
            let br = palette.bracket;
            format!("{br}[ {good}ok{br} ]{br:#}")
        } else {
            // The diagnostic goes out first, as its own `eerror` line, so
            // `ENDCOL`'s single cursor-up puts the indicator at the end of
            // *that* line rather than the `ebegin` one — as in portage.
            if let Some(msg) = &failure_msg {
                let prefix = marker(palette.err);
                for line in msg.split('\n') {
                    let _ = writeln!(out, "{prefix}{line}");
                }
            }
            let bad = palette.bad;
            let br = palette.bracket;
            format!("{br}[ {bad}!!{br} ]{br:#}")
        };

        let endcol = if palette.is_plain() {
            String::new()
        } else {
            endcol(terminal.columns)
        };
        let _ = writeln!(out, "{endcol} {indicator}");
        Ok(brush_core::ExecutionResult::new(code))
    }
}
