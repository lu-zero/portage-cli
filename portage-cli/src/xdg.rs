//! Minimal [XDG Base Directory](https://specifications.freedesktop.org/basedir-spec/latest/)
//! helpers for `em`.
//!
//! No third-party crate (`etcetera`, `directories`, …) — just env vars and
//! the standard defaults. This is CLI policy (app name `em`, dogfood state,
//! overlay user-cache layout); library crates take paths as arguments.

use camino::Utf8PathBuf;

/// Application directory name under cache/state homes
const APP: &str = "em";

/// `$HOME`, or `/root` when unset (matches unprivileged `~` resolution)
pub fn home() -> Utf8PathBuf {
    env_path("HOME").unwrap_or_else(|| Utf8PathBuf::from("/root"))
}

/// `$XDG_CACHE_HOME`, or `~/.cache`
///
/// When both `XDG_CACHE_HOME` and `HOME` are unset, falls back to `/tmp`
/// (writable without a home directory).
pub fn cache_home() -> Utf8PathBuf {
    if let Some(p) = env_path("XDG_CACHE_HOME") {
        return p;
    }
    match env_path("HOME") {
        Some(h) => h.join(".cache"),
        None => Utf8PathBuf::from("/tmp"),
    }
}

/// `$XDG_STATE_HOME`, or `~/.local/state`
pub fn state_home() -> Utf8PathBuf {
    env_path("XDG_STATE_HOME").unwrap_or_else(|| home().join(".local/state"))
}

/// `$XDG_CACHE_HOME/em` (or `~/.cache/em`)
pub fn em_cache_dir() -> Utf8PathBuf {
    cache_home().join(APP)
}

/// `$XDG_STATE_HOME/em` (or `~/.local/state/em`)
pub fn em_state_dir() -> Utf8PathBuf {
    state_home().join(APP)
}

/// Root of the user-side md5-cache tree:
/// `$XDG_CACHE_HOME/em/md5-cache` (per-repo dirs live underneath).
///
/// Written by `em regen` (when the tree is not writable) and by
/// `portage_repo::repo_entries` after a successful live source; read back on
/// the next resolve so cache-less overlays are not re-sourced every run.
pub fn md5_cache_root() -> Utf8PathBuf {
    em_cache_dir().join("md5-cache")
}

/// Per-repo user md5-cache: [`md5_cache_root`]/`/<repo>`
///
/// Prefer opening repos via [`crate::repo_open`] so this path is applied as
/// the repository secondary; call this only when you need the path itself.
#[allow(dead_code)] // public helper for tooling; open path goes through builder
pub fn md5_cache_dir(repo_name: &str) -> Utf8PathBuf {
    md5_cache_root().join(repo_name)
}

/// `$XDG_STATE_HOME/em/news` — `em news`'s unread/read/skip tracking when
/// operating on the bare host (no `--root`/`--prefix`/`--local`).
///
/// Real GLEP 42 news state lives at `<EROOT>/var/lib/gentoo/news/`,
/// root-owned and portage-group-writable. That's fine for a managed
/// `--root`/`--prefix`/`--local` target, but `em news list`/`count` is a
/// read-mostly, unprivileged operation an ordinary user should be able to
/// run against the real host without root — same reasoning as
/// [`regen_activity_root`].
///
/// Callers pick between this and the real `<eroot>/var/lib/gentoo/news`
/// based on whether `eroot` is `/`.
pub fn news_state_dir() -> Utf8PathBuf {
    em_state_dir().join("news")
}

/// `$XDG_STATE_HOME/em/glsa` — `em glsa`'s `glsa_injected` tracking on the bare host
///
/// Same reasoning as [`news_state_dir`]: real portage's `glsa_injected` lives at
/// `<EROOT>/var/lib/portage/glsa_injected` (`PRIVATE_PATH`), and `em glsa fix` marking a
/// GLSA as applied shouldn't need root just to remember what it already fixed.
pub fn glsa_state_dir() -> Utf8PathBuf {
    em_state_dir().join("glsa")
}

/// `$XDG_STATE_HOME/em/mirrordist` — `em mirrordist`'s own deletion-grace
/// state files (one JSON file per repo/distfiles-target pair; see
/// `mirrordist::DeletionDb`). Deliberately outside `--distfiles` so it's
/// neither rsynced to mirror clients nor mistaken for an orphan by
/// mirrordist's own scan.
pub fn mirrordist_state_dir() -> Utf8PathBuf {
    em_state_dir().join("mirrordist")
}

/// `$XDG_STATE_HOME/em/activity` — stand-in "merge root" for `em regen`'s activity bus
///
/// Unlike a real merge, `regen` runs unprivileged, but its activity bus used to be built
/// from the real `--root` unconditionally, so `LiveFsSink` tried (and failed) to write
/// under `<root>/var/cache/edb/em-activity/live`. This path keeps regen's live-session
/// tracking working without root.
pub fn regen_activity_root() -> Utf8PathBuf {
    em_state_dir().join("activity")
}

fn env_path(var: &str) -> Option<Utf8PathBuf> {
    std::env::var(var)
        .ok()
        .filter(|s| !s.is_empty())
        .map(Utf8PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        saved: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let saved = std::env::var(key).ok();
            // SAFETY: held under ENV_LOCK for the whole test.
            unsafe { std::env::set_var(key, val) };
            Self { key, saved }
        }

        fn clear(key: &'static str) -> Self {
            let saved = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.saved {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn cache_home_prefers_xdg() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _xdg = EnvGuard::set("XDG_CACHE_HOME", "/custom/cache");
        let _home = EnvGuard::set("HOME", "/home/u");
        assert_eq!(cache_home().as_str(), "/custom/cache");
        assert_eq!(em_cache_dir().as_str(), "/custom/cache/em");
        assert_eq!(
            md5_cache_dir("gentoo").as_str(),
            "/custom/cache/em/md5-cache/gentoo"
        );
    }

    #[test]
    fn cache_home_falls_back_to_home_dot_cache() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _xdg = EnvGuard::clear("XDG_CACHE_HOME");
        let _home = EnvGuard::set("HOME", "/home/u");
        assert_eq!(cache_home().as_str(), "/home/u/.cache");
    }

    #[test]
    fn cache_home_tmp_when_no_home() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _xdg = EnvGuard::clear("XDG_CACHE_HOME");
        let _home = EnvGuard::clear("HOME");
        assert_eq!(cache_home().as_str(), "/tmp");
    }

    #[test]
    fn state_home_prefers_xdg() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _xdg = EnvGuard::set("XDG_STATE_HOME", "/custom/state");
        let _home = EnvGuard::set("HOME", "/home/u");
        assert_eq!(state_home().as_str(), "/custom/state");
        assert_eq!(em_state_dir().as_str(), "/custom/state/em");
    }

    #[test]
    fn state_home_falls_back_to_home_local_state() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _xdg = EnvGuard::clear("XDG_STATE_HOME");
        let _home = EnvGuard::set("HOME", "/home/u");
        assert_eq!(state_home().as_str(), "/home/u/.local/state");
        assert_eq!(em_state_dir().as_str(), "/home/u/.local/state/em");
    }

    #[test]
    fn home_defaults_to_root() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = EnvGuard::clear("HOME");
        assert_eq!(home().as_str(), "/root");
    }
}
