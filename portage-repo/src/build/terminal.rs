//! The terminal description `em` hands down to a build phase
//!
//! Portage's `isolated-functions.sh` decides nothing about the terminal on its
//! own: the Python side resolves the palette and the width and pushes both into
//! the ebuild environment (`PORTAGE_COLORMAP`, `COLUMNS`), and the bash side
//! just obeys. `em` keeps the same split — policy (a `--color` flag, `NO_COLOR`,
//! whether stderr is a real TTY) belongs to `portage-cli`, which resolves it
//! once and pushes the answer through [`EbuildShell::set_terminal`].
//!
//! [`EbuildShell::set_terminal`]: crate::EbuildShell::set_terminal

use std::sync::{Arc, Mutex};

use anstyle::Style;

/// Portage's `PORTAGE_COLOR_*` palette (`isolated-functions.sh`'s `__set_colors`)
///
/// Carried as [`Style`] values rather than pre-rendered escape strings so
/// the caller's palette stays the single definition of each colour.
///
/// [`Default`] is portage's `__unset_colors`: every style empty, which anstyle
/// renders as the empty string at both ends — the same "leave the variables
/// unset" no-colour mode portage falls back to.
///
/// No `PORTAGE_COLOR_HILITE` here: portage defines it, but none of the `e*`
/// output functions use it.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct PortageColors {
    /// `einfo`'s marker
    pub info: Style,
    /// `elog`'s marker
    pub log: Style,
    /// `ewarn`'s marker
    pub warn: Style,
    /// `eqawarn`'s marker
    pub qawarn: Style,
    /// `eerror`'s marker
    pub err: Style,
    /// `eend`'s `[ … ]` frame
    pub bracket: Style,
    /// `eend`'s `ok` indicator
    pub good: Style,
    /// `eend`'s `!!` indicator
    ///
    /// Portage paints this the same as [`err`], but keeps the two
    /// separately overridable; so do we.
    ///
    /// [`err`]: Self::err
    pub bad: Style,
}

impl PortageColors {
    /// Whether this palette paints nothing at all
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

/// The terminal a build phase should believe it is writing to
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalConfig {
    /// Usable width, exported as `COLUMNS`
    pub columns: usize,
    /// Palette for the `e*` output builtins
    pub colors: PortageColors,
}

impl Default for TerminalConfig {
    /// 80 columns and no colour — what portage assumes when it can measure
    /// nothing.
    fn default() -> Self {
        Self {
            columns: 80,
            colors: PortageColors::default(),
        }
    }
}

/// Cross-subshell handle to the live [`TerminalConfig`]
///
/// Shared (`Arc`) with every clone of the inner shell, like
/// [`DieFlag`](crate::build::commands::die::DieFlag): an `ebegin` that runs
/// inside `$(...)` and the `eend` that closes it outside must agree on the
/// width, which a plain shell variable would not survive.
#[derive(Clone, Default)]
pub(crate) struct TerminalState(Arc<Mutex<TerminalConfig>>);

impl TerminalState {
    pub(crate) fn get(&self) -> TerminalConfig {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn set(&self, config: TerminalConfig) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = config;
    }
}
