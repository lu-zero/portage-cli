//! `PORTAGE_INST_UID` / `PORTAGE_INST_GID` resolution for install helpers
//!
//! Portage defaults these to `0`/`0` for privileged merges and to the owner of
//! the merge root (`EROOT`) in [unprivileged mode](https://github.com/gentoo/portage/blob/master/lib/portage/data.py).
//! Values from `make.conf` override the defaults. The resolved ids are stored in
//! brush shared state (like [`DieFlag`](super::die::DieFlag)) so `dobin`/`dosbin`
//! and PATH shims (`em __helper`) agree, and synced into the shell environment
//! so exported vars reach `make`/`find` child processes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use brush_core::ShellExtensions;

/// Cross-subshell install ownership defaults (`dobin`/`dosbin`/`newbin`/`newsbin`)
#[derive(Clone, Default)]
pub(crate) struct InstOwnerDefaults(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    uid: Option<String>,
    gid: Option<String>,
    resolved: bool,
}

impl InstOwnerDefaults {
    /// Return the stored uid/gid pair, falling back to the current process ids
    pub(crate) fn resolved_pair(&self) -> (String, String) {
        if let Some(pair) = self.stored() {
            return pair;
        }
        (process_uid(), process_gid())
    }

    /// Seed shared state from the helper subprocess environment (PATH shims)
    ///
    /// Uses `PORTAGE_INST_*` when exported by the active phase; otherwise falls
    /// back to the current process ids so `install -o <self>` succeeds for
    /// unprivileged builds instead of defaulting to root (`0`).
    pub(crate) fn seed_from_process_env(&self) {
        let uid = std::env::var("PORTAGE_INST_UID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| rustix::process::getuid().as_raw().to_string());
        let gid = std::env::var("PORTAGE_INST_GID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| rustix::process::getgid().as_raw().to_string());
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        guard.uid = Some(uid);
        guard.gid = Some(gid);
        guard.resolved = true;
    }

    /// `-o <uid> -g <gid>` arguments for coreutils `install`, matching
    /// portage's `dobin`/`dosbin`
    ///
    /// Shell overrides win, then shared defaults, then the current
    /// process ids.
    pub(crate) fn install_opts<SE: ShellExtensions>(
        &self,
        shell: &brush_core::Shell<SE>,
    ) -> Vec<String> {
        let (uid, gid) = self.ids_for_install(shell);
        vec!["-o".into(), uid, "-g".into(), gid]
    }

    fn ids_for_install<SE: ShellExtensions>(
        &self,
        shell: &brush_core::Shell<SE>,
    ) -> (String, String) {
        let shell_uid = nonempty_env(shell, "PORTAGE_INST_UID");
        let shell_gid = nonempty_env(shell, "PORTAGE_INST_GID");
        if shell_uid.is_some() || shell_gid.is_some() {
            let uid = shell_uid.unwrap_or_else(process_uid);
            let gid = shell_gid.unwrap_or_else(process_gid);
            self.store(&uid, &gid);
            return (uid, gid);
        }
        if let Some((uid, gid)) = self.stored() {
            return (uid, gid);
        }
        (process_uid(), process_gid())
    }

    fn store(&self, uid: &str, gid: &str) {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        guard.uid = Some(uid.to_string());
        guard.gid = Some(gid.to_string());
    }

    fn stored(&self) -> Option<(String, String)> {
        let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match (guard.uid.clone(), guard.gid.clone()) {
            (Some(uid), Some(gid)) => Some((uid, gid)),
            _ => None,
        }
    }
}

/// Resolve install ownership for a phase and return the canonical uid/gid pair
///
/// Non-empty `PORTAGE_INST_*` shell variables (e.g. from `make.conf`) override
/// computed defaults. The result is stored in `defaults` for builtins and PATH
/// shims that share the same shell clone tree.
pub(crate) fn resolve_inst_owner<SE: ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    defaults: &InstOwnerDefaults,
    merge_root: &Path,
) -> (String, String) {
    let shell_uid = nonempty_env(shell, "PORTAGE_INST_UID");
    let shell_gid = nonempty_env(shell, "PORTAGE_INST_GID");

    let mut guard = defaults.0.lock().unwrap_or_else(|e| e.into_inner());
    let (default_uid, default_gid) = if guard.resolved {
        (
            guard.uid.clone().unwrap_or_else(|| "0".into()),
            guard.gid.clone().unwrap_or_else(|| "0".into()),
        )
    } else {
        let ids = portage_default_inst_ids(merge_root);
        guard.resolved = true;
        ids
    };

    let uid = shell_uid.unwrap_or(default_uid);
    let gid = shell_gid.unwrap_or(default_gid);
    guard.uid = Some(uid.clone());
    guard.gid = Some(gid.clone());
    (uid, gid)
}

/// Portage `config.reset()` defaults for `PORTAGE_INST_UID`/`PORTAGE_INST_GID`
fn portage_default_inst_ids(merge_root: &Path) -> (String, String) {
    let mut uid = "0".to_string();
    let mut gid = "0".to_string();
    let eroot = eroot_or_parent(merge_root);
    if unprivileged_mode(&eroot)
        && let Ok(meta) = std::fs::metadata(&eroot)
    {
        use std::os::unix::fs::MetadataExt;
        uid = meta.uid().to_string();
        gid = meta.gid().to_string();
    }
    (uid, gid)
}

/// Mirrors portage `data._unprivileged_mode`: non-root with write access to a
/// non-world-writable merge root.
fn unprivileged_mode(eroot: &Path) -> bool {
    if rustix::process::getuid().is_root() {
        return false;
    }
    let Ok(meta) = std::fs::metadata(eroot) else {
        return false;
    };
    use std::os::unix::fs::PermissionsExt;
    if meta.permissions().mode() & 0o0002 != 0 {
        return false;
    }
    rustix::fs::access(eroot, rustix::fs::Access::WRITE_OK).is_ok()
}

/// First existing path along `merge_root` (portage `first_existing(eroot)`)
fn eroot_or_parent(merge_root: &Path) -> PathBuf {
    if merge_root.exists() {
        return merge_root.to_path_buf();
    }
    if let Some(parent) = merge_root.parent()
        && parent.exists()
    {
        return parent.to_path_buf();
    }
    merge_root.to_path_buf()
}

fn nonempty_env<SE: ShellExtensions>(shell: &brush_core::Shell<SE>, name: &str) -> Option<String> {
    shell
        .env_str(name)
        .map(|s| s.into_owned())
        .filter(|s| !s.is_empty())
}

fn process_uid() -> String {
    rustix::process::getuid().as_raw().to_string()
}

fn process_gid() -> String {
    rustix::process::getgid().as_raw().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn portage_defaults_root_for_missing_eroot() {
        let (uid, gid) = portage_default_inst_ids(Path::new("/nonexistent/eroot"));
        assert_eq!(uid, "0");
        assert_eq!(gid, "0");
    }

    #[test]
    fn unprivileged_mode_requires_non_world_writable() {
        let dir = tempfile::tempdir().unwrap();
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o1777);
        std::fs::set_permissions(dir.path(), perms).unwrap();
        if !rustix::process::getuid().is_root() {
            assert!(!unprivileged_mode(dir.path()));
        }
    }
}
