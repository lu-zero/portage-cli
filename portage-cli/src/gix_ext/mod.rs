//! Small pure-gix compositions that gitoxide does not ship as porcelain yet.
//!
//! gitoxide's own status ([`crate-status.md`] in the gitoxide tree) still marks
//! *checkout / switch / restore / reset orchestration* as unfinished. The
//! building blocks exist (`index_from_tree`, `gix_worktree_state::checkout` with
//! `overwrite_existing`, ref edits). This module wires them into a
//! Portage-shaped **hard reset** and is written so it can be proposed upstream
//! as something like `Repository::reset(target, Hard)`.
//!
//! ## Intended upstream shape
//!
//! ```ignore
//! // gix (proposed)
//! impl Repository {
//!     pub fn reset_hard(&self, target: impl Into<ObjectId>) -> Result<(), reset::Error>;
//! }
//! ```
//!
//! Once that lands in `gix`, delete this module and call the upstream API.

mod reset;

pub use reset::{hard_reset_to, resolve_upstream_tip, set_remote_url};
