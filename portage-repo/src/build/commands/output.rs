//! Portage's P1 output helpers, following `isolated-functions.sh`.
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

/// The current palette, or a plain one if the shell has no terminal state.
fn colors<SE: brush_core::ShellExtensions>(
    context: &brush_core::ExecutionContext<'_, SE>,
) -> PortageColors {
    context
        .shared::<TerminalState>()
        .map(|t| t.get().colors)
        .unwrap_or_default()
}

/// Portage's message prefix: a space, the coloured `*`, a space. Only the `*`
/// is painted — the surrounding spaces are not, matching
/// `echo " ${PORTAGE_COLOR_INFO}*${PORTAGE_COLOR_NORMAL} ${REPLY}"`.
fn marker(style: Style) -> String {
    format!(" {style}*{style:#} ")
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
        let style = match context.command_name.as_str() {
            "ewarn" => palette.warn,
            "eerror" => palette.err,
            "eqawarn" => palette.qawarn,
            "elog" => palette.log,
            _ => palette.info,
        };
        let einfon = context.command_name == "einfon";
        let prefix = marker(style);
        let msg = self.message.join(" ");

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
        let msg = self.message.join(" ");
        let shell = context.shell;
        let _ = writeln!(context.params.stderr(shell), "{prefix}{msg} ...");
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
            let msg = self.message.join(" ");
            if !msg.is_empty() {
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
