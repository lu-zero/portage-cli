//! USE-flag evaluation vocabulary, shared from `portage-solver`.
//!
//! `UseConfig`/`UseFlagState`/`UseOverride` and the caller-side
//! `resolve_effective_use` resolver are the single source of truth in
//! [`portage_solver`]; this module just re-exports them so existing
//! `portage_atom_pubgrub::` import paths keep working. `portage_solver::UseConfig`
//! already implements `portage_atom::UseFlagLookup`, so the solver's dependency
//! conversion sees flag state through the same trait.

pub use portage_solver::{UseConfig, UseFlagState, UseLayer, UseOverride, resolve_effective_use};
