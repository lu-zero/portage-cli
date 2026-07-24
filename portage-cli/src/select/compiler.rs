//! `em select compiler` — `gcc-config`/`eselect gcc` workalike.
//!
//! Manages compiler profile selection for gcc. Reads/writes env.d files and
//! creates symlinks similar to gcc-config. Supports grouping profiles by target
//! architecture and showing which is active per architecture.

use std::collections::BTreeMap;

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};

use super::{Cli, env_d};
use crate::cli::CompilerAction;
use portage_resolve::Roots;

/// GCC-specific profile type.
pub struct GccProfileType;

impl env_d::EnvDProfile for GccProfileType {
    fn module_name() -> &'static str {
        "compiler"
    }

    fn env_d_subdir() -> &'static str {
        "gcc"
    }

    fn global_env_file() -> &'static str {
        "04gcc-{target}"
    }

    fn global_env_uses_target() -> bool {
        true
    }

    fn target_var_name() -> &'static str {
        "CTARGET="
    }

    fn install_wrappers(
        roots: &Roots,
        target: &str,
        vars: &BTreeMap<String, String>,
    ) -> Result<()> {
        let Some(gcc_path) = vars.get("GCC_PATH").filter(|p| !p.is_empty()) else {
            return Ok(());
        };
        install_gcc_wrappers(&env_d::eprefix(roots), target, gcc_path)
    }
}

/// Replicate `gcc-config`'s `usr/bin/<T>-<tool>` → `<GCC_PATH>/<T>-<tool>` symlinks
/// (the gcc-bin binaries are already `<T>-`prefixed), plus the `<T>-cc` alias.
/// `gcc_path` is the env.d `GCC_PATH` (`/usr/<CBUILD>/<T>/gcc-bin/<ver>`); it is
/// always resolved under `eprefix` so a `--local`/`--prefix` install links its own
/// binaries, not a same-pathed host copy. No-op until the compiler is merged.
fn install_gcc_wrappers(eprefix: &Utf8Path, target: &str, gcc_path: &str) -> Result<()> {
    // GCC_PATH may or may not already carry the EPREFIX; strip it then re-root so
    // the symlink content stays inside the prefix either way.
    let rel = gcc_path.strip_prefix(eprefix.as_str()).unwrap_or(gcc_path);
    let bindir = eprefix.join(rel.trim_start_matches('/'));
    if !bindir.is_dir() {
        return Ok(());
    }
    let usr_bin = eprefix.join("usr/bin");
    let mut have_gcc = false;
    for entry in std::fs::read_dir(&bindir)? {
        let Ok(path) = Utf8PathBuf::from_path_buf(entry?.path()) else {
            continue;
        };
        let name = path.file_name().unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        env_d::symlink_force(&bindir.join(name), &usr_bin.join(name))?;
        have_gcc |= name == format!("{target}-gcc");
    }
    if have_gcc {
        env_d::symlink_force(
            Utf8Path::new(&format!("{target}-gcc")),
            &usr_bin.join(format!("{target}-cc")),
        )?;
    }
    Ok(())
}

/// Activate the newest gcc profile built into this root for `target`
/// (`crossdev --setup`). EPREFIX-aware; no-op until a gcc step merges. Run after
/// [`super::binutils::activate_latest`] so the gcc wrappers can reach cross as/ld.
pub fn activate_latest(roots: &Roots, target: &str) -> Result<bool> {
    env_d::activate_latest::<GccProfileType>(roots, target)
}

/// The `SLOT` `gcc-config` currently has active for `target` (e.g. `"15"`),
/// or `None` if no toolchain has been activated there yet. The profile
/// string stored in `env.d` is `<target>-<slot>`; strip the target prefix
/// to recover just the slot.
pub fn current_slot(roots: &Roots, target: &str) -> Option<String> {
    let current = env_d::get_current_profile::<GccProfileType>(roots, target)?;
    current
        .strip_prefix(target)?
        .strip_prefix('-')
        .map(str::to_owned)
}

pub fn run(action: &CompilerAction, globals: &Cli) -> Result<()> {
    let target = match action {
        CompilerAction::List { target, .. } | CompilerAction::Show { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
        CompilerAction::Set { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
    };

    // outer_roots(), not roots() -- see env_d::run_list's doc comment (the
    // --target flag collision between this subcommand's own field and the
    // global one).
    let base_dir = env_d::env_d_dir::<GccProfileType>(&globals.outer_roots());

    match action {
        CompilerAction::List { .. } => env_d::run_list::<GccProfileType>(globals),
        CompilerAction::Show { .. } => {
            env_d::run_show::<GccProfileType>(globals, &target);
            Ok(())
        }
        CompilerAction::Set { profile, .. } => {
            env_d::run_set::<GccProfileType>(globals, &target, profile, &base_dir)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    /// `em select`'s config-root resolution deliberately does NOT infer a
    /// config root from bare `--root` (matching real eselect, which only
    /// ever honours an explicit `PORTAGE_CONFIGROOT`/`EROOT` — see
    /// `select/mod.rs::config_portage_dir_for`'s doc comment) — so reading
    /// a self-contained root's own gcc slot requires `--config-root`
    /// alongside `--root`, exactly like a user would need to point real
    /// eselect at it explicitly.
    #[test]
    fn current_slot_reads_the_active_gcc_config_profile() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let target = "riscv64-unknown-linux-gnu";
        let cli = Cli::parse_from(["em", "--root", root, "--config-root", root]);

        // No toolchain activated yet.
        assert_eq!(current_slot(&cli.roots(), target), None);

        let config_dir = dir.path().join("etc/env.d/gcc");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join(format!("config-{target}")),
            format!("CURRENT={target}-15\n"),
        )
        .unwrap();

        assert_eq!(current_slot(&cli.roots(), target), Some("15".to_string()));
    }

    /// Without `--config-root`, a bare `--root` must NOT silently pick up
    /// the offset's own env.d — `em select` only follows an explicit
    /// config root, never `--root` alone (see the test above's doc
    /// comment). Verified via the `is_self_contained_root`-aware internal
    /// override instead of the real host's `/etc/env.d/gcc`, so this stays
    /// deterministic in CI regardless of what's activated on the machine
    /// running the test.
    #[test]
    fn current_slot_ignores_bare_root_without_explicit_config_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let target = "riscv64-unknown-linux-gnu";
        let cli = Cli::parse_from(["em", "--root", root]);

        let config_dir = dir.path().join("etc/env.d/gcc");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join(format!("config-{target}")),
            format!("CURRENT={target}-15\n"),
        )
        .unwrap();

        // The offset's own env.d has a slot recorded, but without
        // --config-root, current_slot must not find it there.
        assert_ne!(current_slot(&cli.roots(), target), Some("15".to_string()));
        // The internal orchestration path (crossdev's own activation),
        // by contrast, does pick it up automatically.
        let internal_roots = cli.roots().with_own_config_root_if_self_contained();
        assert_eq!(
            current_slot(&internal_roots, target),
            Some("15".to_string())
        );
    }

    #[test]
    fn cross_wrappers_link_gcc_bin_directly() {
        let td = tempfile::TempDir::new().unwrap();
        let eprefix = Utf8Path::from_path(td.path()).unwrap().to_path_buf();
        let target = "riscv64-unknown-linux-gnu";
        let gcc_path = format!("/usr/aarch64-unknown-linux-gnu/{target}/gcc-bin/15");

        let bindir = eprefix.join(gcc_path.trim_start_matches('/'));
        std::fs::create_dir_all(&bindir).unwrap();
        for tool in ["gcc", "g++", "cpp"] {
            std::fs::write(bindir.join(format!("{target}-{tool}")), b"#!/bin/true\n").unwrap();
        }

        install_gcc_wrappers(&eprefix, target, &gcc_path).unwrap();

        let bin_gcc = eprefix.join("usr/bin").join(format!("{target}-gcc"));
        assert_eq!(
            std::fs::read_link(&bin_gcc).unwrap(),
            bindir.join(format!("{target}-gcc")).as_std_path()
        );
        // <T>-cc aliases <T>-gcc (relative content).
        let bin_cc = eprefix.join("usr/bin").join(format!("{target}-cc"));
        assert_eq!(
            std::fs::read_link(&bin_cc).unwrap(),
            std::path::Path::new(&format!("{target}-gcc"))
        );
    }
}
