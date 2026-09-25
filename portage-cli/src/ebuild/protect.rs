//! `CONFIG_PROTECT` / `CONFIG_PROTECT_MASK` resolution and `._cfg` diversion
//!
//! The lists come from the layers the `*_protect_mask` helpers below read:
//! `make.globals`, the live ebuild shell, the profile stack, and the merge
//! root's `etc/env.d`.

use std::collections::HashSet;

use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::MakeConf;

use super::{MAKE_GLOBALS, apply_profile_env};

/// CONFIG_PROTECT / CONFIG_PROTECT_MASK resolution (portage's `ConfigProtect`)
///
/// A path is protected when the longest matching `CONFIG_PROTECT` prefix is
/// longer than the longest matching `CONFIG_PROTECT_MASK` prefix. Protected
/// files that already exist and differ are diverted to `._cfgNNNN_<name>`
/// for `dispatch-conf`/`etc-update` instead of being overwritten.
///
/// Deliberately real-Portage semantics, not the PMS 13.3.3 letter (which
/// gives an ancestor `CONFIG_PROTECT_MASK` unconditional priority regardless
/// of how deep the matching `CONFIG_PROTECT` entry is). Matching what real
/// `emerge` actually does on a system is the intended behavior here.
pub(crate) struct ConfigProtect {
    protect: Vec<String>,
    mask: Vec<String>,
}

impl ConfigProtect {
    /// Read the lists from the configured shell
    ///
    /// `/etc` is always protected (portage's make.globals guarantees it).
    pub(crate) fn from_shell(shell: &portage_repo::EbuildShell) -> Self {
        let read = |name: &str| -> Vec<String> {
            shell
                .get_var(name)
                .unwrap_or_default()
                .split_whitespace()
                .map(|s| s.trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let mut protect = read("CONFIG_PROTECT");
        if !protect.iter().any(|p| p == "/etc") {
            protect.push("/etc".to_string());
        }
        Self {
            protect,
            mask: read("CONFIG_PROTECT_MASK"),
        }
    }

    /// Stacked CONFIG_PROTECT / MASK for callers with no ebuild shell (`em etc`)
    ///
    /// Same layers the merge sees, minus per-ebuild `pkg_preinst` mutations:
    /// make.globals, then the profile + make.conf incrementals, then the
    /// merge root's `etc/env.d`. `/etc` is always protected, the same
    /// guarantee `from_shell` relies on portage's `make.globals` for.
    pub(crate) async fn from_roots(roots: &portage_resolve::Roots) -> Self {
        let mut protect_layers: Vec<String> = Vec::new();
        let mut mask_layers: Vec<String> = Vec::new();
        let push = |layers: &mut Vec<String>, v: String| {
            if !v.trim().is_empty() {
                layers.push(v);
            }
        };

        if let Some((p, m)) = make_globals_protect_mask() {
            push(&mut protect_layers, p);
            push(&mut mask_layers, m);
        }

        if let Some((p, m)) = stacked_protect_mask(roots).await {
            push(&mut protect_layers, p);
            push(&mut mask_layers, m);
        } else {
            if let Some(v) =
                crate::binpkg::read_make_conf_var_for_roots(roots, "CONFIG_PROTECT").await
            {
                push(&mut protect_layers, v);
            }
            if let Some(v) =
                crate::binpkg::read_make_conf_var_for_roots(roots, "CONFIG_PROTECT_MASK").await
            {
                push(&mut mask_layers, v);
            }
        }

        let (env_p, env_m) = env_d_protect_mask(roots.merge_root());
        push(&mut protect_layers, env_p);
        push(&mut mask_layers, env_m);

        let mut protect = merge_protect_layers(protect_layers.iter().map(String::as_str));
        if !protect.iter().any(|p| p == "/etc") {
            protect.push("/etc".to_string());
        }
        Self {
            protect,
            mask: merge_protect_layers(mask_layers.iter().map(String::as_str)),
        }
    }

    /// The protected directories themselves, for a caller that has to walk
    /// them rather than test a single path.
    pub(crate) fn protected_dirs(&self) -> &[String] {
        &self.protect
    }

    /// Length of the longest entry in `list` that prefix-matches `obj` on
    /// whole components (`obj == p` or `obj` under `p/`); 0 if none.
    fn longest_match(list: &[String], obj: &str) -> usize {
        list.iter()
            .filter(|p| obj == p.as_str() || obj.starts_with(&format!("{p}/")))
            .map(String::len)
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn is_protected(&self, obj: &Utf8Path) -> bool {
        let obj = obj.as_str();
        Self::longest_match(&self.protect, obj) > Self::longest_match(&self.mask, obj)
    }

    /// Explicit lists, for a test that needs a specific protect/mask pair
    /// without a shell or a `make.conf`.
    #[cfg(test)]
    pub(crate) fn for_test(protect: &[&str], mask: &[&str]) -> Self {
        Self {
            protect: protect.iter().map(|s| (*s).to_string()).collect(),
            mask: mask.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// A config-protection set that protects nothing (for tests / contexts
    /// where protection does not apply).
    #[cfg(test)]
    pub(crate) fn none() -> Self {
        Self {
            protect: vec![],
            mask: vec![],
        }
    }
}

/// make.globals' CONFIG_PROTECT / MASK. ProfileStack never sources that file.
fn make_globals_protect_mask() -> Option<(String, String)> {
    let mg = Utf8Path::new(MAKE_GLOBALS);
    if !mg.exists() {
        return None;
    }
    let mc = MakeConf::load(mg).ok()?;
    Some((
        mc.get("CONFIG_PROTECT").unwrap_or("").to_string(),
        mc.get("CONFIG_PROTECT_MASK").unwrap_or("").to_string(),
    ))
}

/// Profile + make.conf incrementals, the same source merge's `from_shell` uses
async fn stacked_protect_mask(roots: &portage_resolve::Roots) -> Option<(String, String)> {
    let conf = roots.repos_conf().ok()?;
    let entry = conf.main_repo().or_else(|| conf.find("gentoo"))?;
    let repo = crate::repo_open::open(entry.location.as_path()?).ok()?;
    let mut shell = repo.shell().await.ok()?;
    if !apply_profile_env(&mut shell, roots.config(), roots.config_overlay())
        .await
        .ok()?
    {
        return None;
    }
    Some((
        shell.get_var("CONFIG_PROTECT").unwrap_or_default(),
        shell.get_var("CONFIG_PROTECT_MASK").unwrap_or_default(),
    ))
}

/// Space-separated CONFIG_PROTECT / MASK accumulated from `root/etc/env.d`
pub(crate) fn env_d_protect_mask(root: &Utf8Path) -> (String, String) {
    let env_d = root.join("etc/env.d");
    let mut entries: Vec<_> = match std::fs::read_dir(env_d.as_std_path()) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| !n.starts_with('.') && !n.ends_with('~'))
            })
            .collect(),
        Err(_) => return (String::new(), String::new()),
    };
    entries.sort();

    let mut protect = String::new();
    let mut mask = String::new();
    for path in &entries {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, raw)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim_start_matches("export ").trim();
            let value = raw.trim().trim_matches('"').trim_matches('\'');
            if value.is_empty() {
                continue;
            }
            let dest = match key {
                "CONFIG_PROTECT" => &mut protect,
                "CONFIG_PROTECT_MASK" => &mut mask,
                _ => continue,
            };
            if !dest.is_empty() {
                dest.push(' ');
            }
            dest.push_str(value);
        }
    }
    (protect, mask)
}

pub(crate) fn merge_protect_layers<'a>(layers: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut acc = Vec::new();
    for layer in layers {
        for raw in layer.split_whitespace() {
            let token = raw.trim_end_matches('/');
            if token.is_empty() {
                continue;
            }
            if token == "-*" {
                seen.clear();
                acc.clear();
                continue;
            }
            if let Some(name) = token.strip_prefix('-') {
                if seen.remove(name) {
                    acc.retain(|p| p != name);
                }
            } else if seen.insert(token.to_string()) {
                acc.push(token.to_string());
            }
        }
    }
    acc
}

/// portage's `new_protect_filename`: the next `._cfgNNNN_<name>` beside
/// `dest` (highest existing index + 1), plus the most recent existing one
/// so the caller can reuse it when the content already matches.
pub(crate) fn scan_cfg(dest: &Utf8Path) -> (Utf8PathBuf, Option<Utf8PathBuf>) {
    let dir = dest.parent().unwrap_or_else(|| Utf8Path::new("/"));
    let name = dest.file_name().unwrap_or_default();
    let mut highest: i32 = -1;
    let mut latest: Option<Utf8PathBuf> = None;
    if let Ok(rd) = std::fs::read_dir(dir.as_std_path()) {
        for entry in rd.flatten() {
            let Ok(f) = entry.file_name().into_string() else {
                continue;
            };
            // ._cfg<4 digits>_<name>
            let Some(rest) = f.strip_prefix("._cfg") else {
                continue;
            };
            if rest.len() > 5
                && rest.as_bytes()[4] == b'_'
                && &rest[5..] == name
                && let Ok(n) = rest[..4].parse::<i32>()
                && n > highest
            {
                highest = n;
                latest = Some(dir.join(&f));
            }
        }
    }
    (dir.join(format!("._cfg{:04}_{name}", highest + 1)), latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_protect_layers_unions_and_honours_minus() {
        assert_eq!(
            merge_protect_layers(["/etc", "/etc /usr/share/config"]),
            vec!["/etc", "/usr/share/config"]
        );
        assert_eq!(
            merge_protect_layers(["/etc /usr/share/config", "-/usr/share/config /opt/x"]),
            vec!["/etc", "/opt/x"]
        );
        assert_eq!(merge_protect_layers(["/etc /foo", "-* /bar"]), vec!["/bar"]);
    }

    #[test]
    fn env_d_protect_mask_accumulates_space_separated() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let env_d = root.join("etc/env.d");
        std::fs::create_dir_all(env_d.as_std_path()).unwrap();
        std::fs::write(
            env_d.join("50foo").as_std_path(),
            "CONFIG_PROTECT=\"/usr/share/config\"\n",
        )
        .unwrap();
        std::fs::write(
            env_d.join("60gpg").as_std_path(),
            "CONFIG_PROTECT=\"/usr/share/gnupg/templates\"\nCONFIG_PROTECT_MASK=\"/etc/ca-certificates.conf\"\n",
        )
        .unwrap();

        let (p, m) = env_d_protect_mask(root);
        assert!(p.contains("/usr/share/config"), "{p}");
        assert!(p.contains("/usr/share/gnupg/templates"), "{p}");
        assert_eq!(m, "/etc/ca-certificates.conf");
    }

    #[tokio::test]
    async fn from_roots_sees_env_d_paths_not_just_etc() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let env_d = root.join("etc/env.d");
        std::fs::create_dir_all(env_d.as_std_path()).unwrap();
        std::fs::write(
            env_d.join("50kde").as_std_path(),
            "CONFIG_PROTECT=\"/usr/share/config\"\n",
        )
        .unwrap();

        let roots =
            portage_resolve::Roots::for_test(root.as_str()).with_config(Some(root.to_owned()));
        let cp = ConfigProtect::from_roots(&roots).await;
        assert!(cp.is_protected(Utf8Path::new("/etc/hosts")));
        assert!(
            cp.is_protected(Utf8Path::new("/usr/share/config/kdeglobals")),
            "env.d CONFIG_PROTECT must be scanned: {:?}",
            cp.protected_dirs()
        );
    }
}
