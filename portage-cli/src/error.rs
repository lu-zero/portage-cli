pub use anyhow::Result;

/// A `--pretend` resolve that surfaced required USE/mask changes.
///
/// The detailed change block was already printed (by the depgraph), so this
/// is carried as an error purely to drive a non-zero exit through the
/// normal `Result` flow. The `em` binary entry point recognises it and
/// exits `1` *quietly* — no `error:` prefix — matching `emerge -p`, where
/// the printed block is the message. When the staged-build driver adds
/// step context, that context is still shown.
#[derive(Debug, thiserror::Error)]
#[error("USE/mask changes are required to proceed (see above)")]
pub struct ConfigChangesNeeded;

/// No atom on the command line resolved to anything mergeable/queryable.
///
/// Each failure already printed its own `!!!` warning (unresolvable atom,
/// ambiguous name with its "pass -u" hint, etc.), so — same pattern as
/// [`ConfigChangesNeeded`] — this is carried purely to drive a non-zero exit
/// through the normal `Result` flow, without a final generic "no valid
/// atoms" line that adds nothing beyond what the warnings above it already
/// said.
#[derive(Debug, thiserror::Error)]
#[error("no valid atoms (see warnings above)")]
pub struct NoValidAtoms;
