//! `em select binutils` — `binutils-config`/`eselect binutils` workalike
//!
//! Manages binutils profile selection. Supports grouping profiles by target
//! architecture and showing which is active per architecture.

use std::collections::BTreeMap;

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};

use super::{Cli, env_d};
use crate::cli::BinutilsAction;
use portage_resolve::Roots;

/// Binutils-specific profile type
pub struct BinutilsProfileType;

impl env_d::EnvDProfile for BinutilsProfileType {
    fn module_name() -> &'static str {
        "binutils"
    }

    fn env_d_subdir() -> &'static str {
        "binutils"
    }

    fn global_env_file() -> &'static str {
        "05binutils"
    }

    fn target_var_name() -> &'static str {
        "TARGET="
    }

    fn install_wrappers(
        roots: &Roots,
        target: &str,
        vars: &BTreeMap<String, String>,
    ) -> Result<()> {
        let Some(ver) = vars.get("VER").filter(|v| !v.is_empty()) else {
            return Ok(());
        };
        install_binutils_wrappers(&env_d::eprefix(roots), target, ver)
    }
}

/// Replicate `binutils-config`'s symlink layout for `target`/`ver` under `eprefix`:
/// `usr/libexec/gcc/<T>/<tool>` → the `binutils-bin` binary, and
/// `usr/bin/<T>-<tool>` → that libexec link. No-op if the binaries aren't merged
/// yet (env.d state was still written by the caller).
fn install_binutils_wrappers(eprefix: &Utf8Path, target: &str, ver: &str) -> Result<()> {
    let Some((binpath, links_dir)) = locate_binutils_bin(eprefix, target, ver) else {
        return Ok(());
    };
    let usr_bin = eprefix.join("usr/bin");
    for entry in std::fs::read_dir(&binpath)? {
        let Ok(path) = Utf8PathBuf::from_path_buf(entry?.path()) else {
            continue;
        };
        let tool = path.file_name().unwrap_or_default();
        if tool.is_empty() {
            continue;
        }
        let libexec_link = links_dir.join(tool);
        env_d::symlink_force(&binpath.join(tool), &libexec_link)?;
        env_d::symlink_force(&libexec_link, &usr_bin.join(format!("{target}-{tool}")))?;
    }
    Ok(())
}

/// Locate the `binutils-bin/<ver>` directory and its `BINPATH_LINKS` dir
///
/// Cross installs nest under `usr/<CBUILD>/<T>/binutils-bin` (links via
/// `usr/libexec/gcc/<T>`); a native install sits at `usr/<T>/binutils-bin` (links via
/// `usr/<T>/bin`).
fn locate_binutils_bin(
    eprefix: &Utf8Path,
    target: &str,
    ver: &str,
) -> Option<(Utf8PathBuf, Utf8PathBuf)> {
    let usr = eprefix.join("usr");
    if let Ok(rd) = std::fs::read_dir(&usr) {
        for entry in rd.flatten() {
            let Ok(host) = Utf8PathBuf::from_path_buf(entry.path()) else {
                continue;
            };
            let cand = host.join(target).join("binutils-bin").join(ver);
            if cand.is_dir() {
                return Some((cand, usr.join("libexec/gcc").join(target)));
            }
        }
    }
    let native = usr.join(target).join("binutils-bin").join(ver);
    if native.is_dir() {
        return Some((native, usr.join(target).join("bin")));
    }
    None
}

/// Activate the newest binutils profile built into this root for `target`
/// (`crossdev --setup`)
///
/// EPREFIX-aware; no-op until the binutils step merges.
pub fn activate_latest(roots: &Roots, target: &str) -> Result<bool> {
    env_d::activate_latest::<BinutilsProfileType>(roots, target)
}

pub fn run(action: &BinutilsAction, globals: &Cli) -> Result<()> {
    let target = match action {
        BinutilsAction::List { target, .. } | BinutilsAction::Show { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
        BinutilsAction::Set { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
    };

    // outer_roots(), not roots() -- see env_d::run_list's doc comment (the
    // --target flag collision between this subcommand's own field and the
    // global one).
    let base_dir = env_d::env_d_dir::<BinutilsProfileType>(&globals.outer_roots());

    match action {
        BinutilsAction::List { .. } => env_d::run_list::<BinutilsProfileType>(globals),
        BinutilsAction::Show { .. } => {
            env_d::run_show::<BinutilsProfileType>(globals, &target);
            Ok(())
        }
        BinutilsAction::Set { profile, .. } => {
            env_d::run_set::<BinutilsProfileType>(globals, &target, profile, &base_dir)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_wrappers_use_libexec_indirection() {
        let td = tempfile::TempDir::new().unwrap();
        let eprefix = Utf8Path::from_path(td.path()).unwrap().to_path_buf();
        let target = "riscv64-unknown-linux-gnu";
        let cbuild = "aarch64-unknown-linux-gnu";
        let ver = "2.46.0";

        let binpath = eprefix
            .join("usr")
            .join(cbuild)
            .join(target)
            .join("binutils-bin")
            .join(ver);
        std::fs::create_dir_all(&binpath).unwrap();
        for tool in ["as", "ld", "ar"] {
            std::fs::write(binpath.join(tool), b"#!/bin/true\n").unwrap();
        }

        install_binutils_wrappers(&eprefix, target, ver).unwrap();

        let libexec_as = eprefix.join("usr/libexec/gcc").join(target).join("as");
        assert_eq!(
            std::fs::read_link(&libexec_as).unwrap(),
            binpath.join("as").as_std_path()
        );
        let bin_as = eprefix.join("usr/bin").join(format!("{target}-as"));
        assert_eq!(
            std::fs::read_link(&bin_as).unwrap(),
            libexec_as.as_std_path()
        );
    }

    #[test]
    fn no_binaries_is_a_noop() {
        let td = tempfile::TempDir::new().unwrap();
        let eprefix = Utf8Path::from_path(td.path()).unwrap().to_path_buf();
        install_binutils_wrappers(&eprefix, "riscv64-unknown-linux-gnu", "2.46.0").unwrap();
        assert!(!eprefix.join("usr/bin").exists());
    }
}
