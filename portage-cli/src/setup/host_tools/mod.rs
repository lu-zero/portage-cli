//! Host tools a fresh `--local` prefix borrows until it has built its own
//!
//! The prefix starts with an empty VDB, so the host's own binaries run every
//! early phase, and two of them must be GNU: `sys-apps/baselayout`'s
//! `src_install` and `prefix.eclass`'s `hprefixify` use `sed -i`/`sed -r`
//! (BSD sed spells both differently), glibc's `src_install` uses `grep -Z`.
//!
//! Detection is behavioural — a candidate is trusted only when its own
//! `--version` banner says so, never because of where it lives. Linux hosts
//! pass on their own `PATH` without any of the fallback machinery below.
//!
//! Private to `setup`: the system-specific search in [`system`] is reachable
//! from nowhere else, and only `em setup --local` ever relaxes the phase
//! `PATH` this way.

#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(not(target_os = "macos"), path = "generic.rs")]
mod system;

use anyhow::{Result, bail};
use camino::{Utf8Path, Utf8PathBuf};

/// A tool the first `--local` merges cannot proceed without
struct Hard {
    /// Accepted command names, in search order
    names: &'static [&'static str],
    /// `--version` banner substring proving the behaviour ebuilds rely on
    /// `None` when merely being present is enough.
    banner: Option<&'static str>,
    /// Homebrew formula shipping a GNU build of this tool
    brew: Option<&'static str>,
    /// Completes "… needs <why>", for the failure message
    why: &'static str,
}

const HARD: &[Hard] = &[
    Hard {
        names: &["sed"],
        banner: Some("(GNU sed)"),
        brew: Some("gnu-sed"),
        why: "GNU sed's `-i`/`-r` (baselayout's src_install, prefix.eclass's hprefixify)",
    },
    Hard {
        names: &["grep"],
        banner: Some("(GNU grep)"),
        // Homebrew's `grep` formula is GNU grep; there is no `gnu-` prefix.
        brew: Some("grep"),
        why: "GNU grep's `-Z` (glibc's src_install)",
    },
    Hard {
        names: &["cc", "gcc", "clang"],
        banner: None,
        brew: None,
        why: "a C compiler to build the prefix's own toolchain with",
    },
    Hard {
        names: &["c++", "g++", "clang++"],
        banner: None,
        brew: None,
        why: "a C++ compiler (gcc's own build is C++)",
    },
    Hard {
        names: &["make"],
        banner: Some("GNU Make"),
        // Homebrew's `make` formula is GNU Make; there is no `gnu-` prefix.
        brew: Some("make"),
        why: "GNU Make, which `emake`/every early package's build system drives \
              (BSD make's option syntax is incompatible)",
    },
    Hard {
        names: &["python3"],
        banner: None,
        brew: None,
        why: "a host python for the python-any-r1 and meson eclasses",
    },
    Hard {
        names: &["tar"],
        banner: None,
        brew: None,
        why: "tar to unpack distfiles",
    },
    Hard {
        names: &["bzip2"],
        banner: None,
        brew: None,
        why: "bzip2, which `tar xjf` execs to unpack .bz2/.tar.bz2 distfiles",
    },
    Hard {
        names: &["xz"],
        banner: None,
        brew: None,
        why: "xz, which `tar xJf` execs to unpack .xz/.tar.xz distfiles",
    },
    Hard {
        names: &["patch"],
        banner: None,
        brew: None,
        why: "patch, which `eapply` runs",
    },
    Hard {
        names: &["awk", "gawk"],
        banner: None,
        brew: None,
        why: "awk, which configure scripts run",
    },
];

/// Where the tools resolved outside the phase `PATH` are exposed under the names ebuilds
/// actually invoke
///
/// Outside the prefix: a `--local` prefix owns only what it built itself, and a host
/// symlink under `${EPREFIX}/usr/bin` would masquerade as prefix-owned content.
fn farm_dir() -> Utf8PathBuf {
    crate::xdg::em_state_dir().join("bootstrap-bin")
}

/// Verify every hard prerequisite resolves, and return the directories to put
/// ahead of the phase `PATH` — `extra_path` as given, plus the farm when some
/// tool was only reachable outside it.
pub(super) fn check(extra_path: &[Utf8PathBuf]) -> Result<Vec<Utf8PathBuf>> {
    let mut dirs = extra_path.to_vec();
    let mut farmed = false;
    for tool in HARD {
        if resolve(tool, &dirs).is_some() {
            continue;
        }
        let found = match tool.banner {
            Some(banner) => fallback(tool, banner),
            None => resolve_raw_path(tool),
        };
        let Some(found) = found else {
            bail!("{}", missing(tool));
        };
        let name = tool.names[0];
        link_into_farm(name, &found)?;
        tracing::info!("using {found} as {name} for this prefix's first merges");
        farmed = true;
    }
    if farmed {
        dirs.push(farm_dir());
    }
    Ok(dirs)
}

/// The binary a phase would run for `name`, or `None` when the host has none
/// — resolved through exactly the `PATH` a phase gets, so nothing claims a
/// tool the build cannot reach.
pub(super) fn which(name: &str, extra_path: &[Utf8PathBuf]) -> Option<Utf8PathBuf> {
    let raw = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let sanitised = portage_repo::phase_path_dirs(&raw, &home);
    let dirs = extra_path.iter().map(|d| d.as_str()).chain(sanitised);
    find_in(dirs, name)
}

/// The first executable `name` across `dirs`, in order
fn find_in<'a>(dirs: impl Iterator<Item = &'a str>, name: &str) -> Option<Utf8PathBuf> {
    dirs.map(|d| Utf8Path::new(d).join(name))
        .find(|p| is_executable(p))
}

fn is_executable(path: &Utf8Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Whether `path --version` identifies as the implementation `banner` names
///
/// A candidate that cannot even be run is simply not a match.
fn behaves_like(path: &Utf8Path, banner: &str) -> bool {
    let Ok(out) = std::process::Command::new(path).arg("--version").output() else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout).contains(banner)
}

/// This tool as a phase would get it today, if that is usable at all
fn resolve(tool: &Hard, extra_path: &[Utf8PathBuf]) -> Option<Utf8PathBuf> {
    tool.names.iter().find_map(|name| {
        let path = which(name, extra_path)?;
        match tool.banner {
            Some(banner) => behaves_like(&path, banner).then_some(path),
            None => Some(path),
        }
    })
}

/// Plain presence check against the user's raw `PATH`, no banner — rung 3 of
/// the ladder for a tool whose mere existence is enough (`banner: None`).
/// `$HOME` and `/usr/local` are sanitised out of the phase `PATH` on the
/// (Linux/macOS) assumption that anything there is a hand-installed shadow
/// copy — wrong on FreeBSD, where `/usr/local` is `pkg`'s own, canonical
/// install prefix for everything, not a suspect override.
fn resolve_raw_path(tool: &Hard) -> Option<Utf8PathBuf> {
    let raw = std::env::var("PATH").unwrap_or_default();
    tool.names
        .iter()
        .find_map(|name| find_in(raw.split(':'), name))
}

/// Search outside the phase `PATH`: the user's own raw `PATH` (`$HOME` and
/// `/usr/local` are sanitised away for builds, but a GNU tool installed there
/// is still a perfectly good answer for this one question), then whatever
/// this system installs GNU tools through. Both under the plain name and the
/// `g`-prefixed one distributions use to avoid clobbering a BSD original.
fn fallback(tool: &Hard, banner: &str) -> Option<Utf8PathBuf> {
    let raw = std::env::var("PATH").unwrap_or_default();
    tool.names
        .iter()
        .flat_map(|name| [name.to_string(), format!("g{name}")])
        .find_map(|name| {
            let on_path = find_in(raw.split(':'), &name);
            on_path
                .into_iter()
                .chain(system::candidates(tool.brew, &name))
                .find(|p| behaves_like(p, banner))
        })
}

/// Expose `found` as `name` in the farm, replacing any earlier link — the
/// host's own tools move between runs.
fn link_into_farm(name: &str, found: &Utf8Path) -> Result<()> {
    let dir = farm_dir();
    std::fs::create_dir_all(&dir)?;
    let link = dir.join(name);
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(found, &link)?;
    Ok(())
}

fn missing(tool: &Hard) -> String {
    let name = tool.names[0];
    let install = system::install_hint(tool.brew, name);
    format!(
        "no usable {name} on this host — a fresh --local prefix has to borrow one until it \
         builds its own, and needs {}.\n{install}\n\
         Already have one somewhere em does not look? Point at it with \
         `em setup --local --extra-path DIR`.",
        tool.why
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // An executable `dir/name` printing `banner` for `--version`
    fn fake_tool(dir: &Utf8Path, name: &str, banner: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho '{banner}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    const SED: &Hard = &HARD[0];

    #[test]
    fn a_gnu_banner_is_accepted_and_anything_else_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let bin = Utf8Path::from_path(dir.path()).unwrap();
        fake_tool(bin, "sed", "sed (GNU sed) 4.9");
        fake_tool(bin, "bsd", "sed: illegal option -- -version");

        assert!(behaves_like(&bin.join("sed"), "(GNU sed)"));
        assert!(!behaves_like(&bin.join("bsd"), "(GNU sed)"));
        assert!(!behaves_like(&bin.join("absent"), "(GNU sed)"));
    }

    #[test]
    fn resolve_takes_the_first_extra_path_hit_that_behaves() {
        let dir = tempfile::tempdir().unwrap();
        let base = Utf8Path::from_path(dir.path()).unwrap();
        let bsd = base.join("bsd");
        let gnu = base.join("gnu");
        fake_tool(&bsd, "sed", "sed: illegal option");
        fake_tool(&gnu, "sed", "sed (GNU sed) 4.9");

        assert_eq!(
            resolve(SED, &[gnu.clone(), bsd.clone()]),
            Some(gnu.join("sed"))
        );
        // A non-GNU first hit does not disqualify the host: `which` answers
        // per name, and `resolve` keeps looking only across `names`. The
        // shadowed GNU copy behind it is exactly what `fallback` is for.
        assert_eq!(resolve(SED, &[bsd]), None);
    }

    #[test]
    fn find_in_skips_a_non_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        let bin = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::write(bin.join("sed"), "not executable").unwrap();
        assert_eq!(find_in(std::iter::once(bin.as_str()), "sed"), None);
    }

    #[test]
    fn the_missing_message_names_the_tool_and_the_escape_hatch() {
        let msg = missing(SED);
        assert!(msg.contains("no usable sed"), "{msg}");
        assert!(msg.contains("--extra-path"), "{msg}");
    }
}
