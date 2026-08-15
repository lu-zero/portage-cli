pub(crate) mod commands;
pub mod env;
pub(crate) mod profile;
pub(crate) mod pty;
pub mod shell;
pub(crate) mod stubs;
pub mod terminal;
pub(crate) mod ver_funcs;

pub use commands::inherit;
pub use env::EbuildEnv;
pub use shell::{EbuildShell, phase_path_dirs, run_helper};
pub use terminal::{PortageColors, TerminalConfig};

/// `tracing` target for an `INFO` event that already carries its own
/// portage-style `">>> "` action-announcement prefix (`">>> Unpacking …"`,
/// matching real portage's own action lines) — as opposed to an ordinary
/// informational note (real portage's `einfo`). The console formatter
/// (`portage-cli`'s `diag::CompactFormatter`) renders an event on this
/// target bare, and everything else gets `einfo`'s `" * "` marker.
///
/// A dedicated `target` rather than sniffing the rendered message text for a
/// `">>> "` prefix: the message is free-form call-site text, not a stable
/// contract, and tracing already gives every event a structured axis for
/// exactly this kind of routing decision.
pub const ACTION_TARGET: &str = "portage_repo::action";
