//! `REQUIRED_USE` as a *fact* in the solver's own vocabulary.
//!
//! The type lives in `portage-solver` (the layer this crate encodes for) and is
//! re-exported here so pubgrub consumers keep a single `RequiredUse` type across
//! both crates — the interned-flag mirror of `portage_metadata::RequiredUseExpr`.
//!
//! The Level-C encoder (`convert::encode_required_use`, see
//! `docs/required-use-level-c.md`) consumes it at ingestion: flags the caller
//! ceded become constrained `UseDecision` nodes, fixed flags are partially
//! evaluated away. With nothing ceded (the default) the fact never constrains
//! the solve — `REQUIRED_USE` stays the cli's post-solve Level-A check.

pub use portage_solver::RequiredUse;
