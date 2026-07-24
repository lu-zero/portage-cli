//! `em select linker` — linker profile selection.
//!
//! Manages linker profile selection. Similar to binutils-config but focused
//! specifically on the linker (ld, lld, mold, etc.). Supports grouping profiles
//! by target architecture and showing which is active per architecture.

use anyhow::Result;

use super::{Cli, env_d};
use crate::cli::LinkerAction;

/// Linker-specific profile type.
pub struct LinkerProfileType;

impl env_d::EnvDProfile for LinkerProfileType {
    fn module_name() -> &'static str {
        "linker"
    }

    fn env_d_subdir() -> &'static str {
        "linker"
    }

    fn global_env_file() -> &'static str {
        "06linker"
    }

    fn target_var_name() -> &'static str {
        "CTARGET="
    }

    fn extra_env_vars() -> &'static [&'static str] {
        &["LD="]
    }
}

pub fn run(action: &LinkerAction, globals: &Cli) -> Result<()> {
    let target = match action {
        LinkerAction::List { target, .. } | LinkerAction::Show { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
        LinkerAction::Set { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
    };

    // outer_roots(), not roots() -- see env_d::run_list's doc comment (the
    // --target flag collision between this subcommand's own field and the
    // global one).
    let base_dir = env_d::env_d_dir::<LinkerProfileType>(&globals.outer_roots());

    match action {
        LinkerAction::List { .. } => env_d::run_list::<LinkerProfileType>(globals),
        LinkerAction::Show { .. } => {
            env_d::run_show::<LinkerProfileType>(globals, &target);
            Ok(())
        }
        LinkerAction::Set { profile, .. } => {
            env_d::run_set::<LinkerProfileType>(globals, &target, profile, &base_dir)
        }
    }
}
