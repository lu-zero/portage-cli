//! `em --local setup`'s bootstrap `package.provided` — step 4 of the
//! config-root ladder (`todo/local-bootstrap-provided.md`). There is no host
//! VDB to read on a non-Gentoo host (or a Gentoo host used only through
//! `--local`, which never weaves the host VDB into BDEPEND satisfaction —
//! see that doc's "why provided is stronger than break cycles"), so the
//! version for each Tier-1 cycle-fuel CPN is picked by probing the host's
//! own tool (`gcc --version`, `python3 -V`, …) and mapping that to the
//! closest tree-present version, falling back to the oldest tree version
//! when the host tool is missing or unparseable.

use anyhow::Result;
use camino::Utf8Path;
use portage_atom::Version;
use portage_repo::Repository;

/// One Tier-1 "cycle fuel" package: the bootstrap closure's build-tool
/// dependencies (never the stage products themselves — `sys-apps/baselayout`,
/// binutils, headers, libc, gcc must still be *built* into the prefix, see
/// the design doc's "explicitly out of provided" list). `probe` is the host
/// command (and args) whose output's first PMS-shaped version token
/// estimates what the host already has; `None` when there's no real host
/// equivalent to probe (`elt-patches` is a Gentoo-only eclass-patches
/// snapshot).
struct Tier1Pkg {
    category: &'static str,
    package: &'static str,
    probe: Option<(&'static str, &'static [&'static str])>,
}

const TIER1: &[Tier1Pkg] = &[
    Tier1Pkg {
        category: "dev-lang",
        package: "python",
        probe: Some(("python3", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-lang",
        package: "perl",
        probe: Some(("perl", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "meson",
        probe: Some(("meson", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "ninja",
        probe: Some(("ninja", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "cmake",
        probe: Some(("cmake", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "make",
        probe: Some(("make", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "autoconf",
        probe: Some(("autoconf", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "automake",
        probe: Some(("automake", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-devel",
        package: "m4",
        probe: Some(("m4", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "libtool",
        probe: Some(("libtool", &["--version"])),
    },
    Tier1Pkg {
        category: "app-portage",
        package: "elt-patches",
        probe: None,
    },
    Tier1Pkg {
        category: "app-arch",
        package: "xz-utils",
        probe: Some(("xz", &["--version"])),
    },
    Tier1Pkg {
        category: "app-arch",
        package: "zstd",
        probe: Some(("zstd", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-devel",
        package: "gettext",
        probe: Some(("gettext", &["--version"])),
    },
    // No single `coreutils`/`findutils` binary reports its own package
    // version; `ls`/`find` do (`ls (GNU coreutils) 9.4`).
    Tier1Pkg {
        category: "sys-apps",
        package: "coreutils",
        probe: Some(("ls", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-apps",
        package: "findutils",
        probe: Some(("find", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-apps",
        package: "gawk",
        probe: Some(("gawk", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-apps",
        package: "grep",
        probe: Some(("grep", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-apps",
        package: "sed",
        probe: Some(("sed", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-apps",
        package: "file",
        probe: Some(("file", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-devel",
        package: "patch",
        probe: Some(("patch", &["--version"])),
    },
    // bzip2 historically doesn't reliably support `--version`; `-h` always
    // prints the same version banner.
    Tier1Pkg {
        category: "app-arch",
        package: "bzip2",
        probe: Some(("bzip2", &["-h"])),
    },
    Tier1Pkg {
        category: "app-arch",
        package: "gzip",
        probe: Some(("gzip", &["--version"])),
    },
    Tier1Pkg {
        category: "app-arch",
        package: "tar",
        probe: Some(("tar", &["--version"])),
    },
];

const BEGIN_MARKER: &str = "# BEGIN em-bootstrap-provided";
const END_MARKER: &str = "# END em-bootstrap-provided";

/// The first whitespace-separated token that looks like a PMS version
/// (starts with a digit once outer punctuation is stripped, and contains a
/// `.`) — good enough to pull `3.11.2` out of `Python 3.11.2`, `1.5.5` out of
/// `*** Zstandard CLI (64-bit) v1.5.5, by ... ***`, `14.2.1_p20241221` out of
/// a Gentoo-patched `gcc --version` banner, etc. Best-effort: a tool with an
/// unusual banner just falls back to [`pick_version`]'s oldest-tree-version
/// case.
fn first_version_token(s: &str) -> Option<String> {
    s.split_whitespace().find_map(|tok| {
        let trimmed = tok.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
        (!trimmed.is_empty()
            && trimmed.contains('.')
            && trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .then(|| trimmed.to_string())
    })
}

/// Run `bin args...` and extract a best-guess host version. `None` if the
/// binary is missing, the run fails, or nothing version-shaped is found —
/// all treated the same by [`pick_version`] (fall back to oldest tree
/// version), not an error: a probe miss is expected on hosts missing one of
/// the Tier-1 tools.
fn probe_version(bin: &str, args: &[&str]) -> Option<Version> {
    let output = std::process::Command::new(bin).args(args).output().ok()?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push('\n');
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Version::parse(&first_version_token(&combined)?).ok()
}

/// Every version this CPN has an ebuild for in `repo`, in no particular
/// order — empty (not an error) if the CPN doesn't exist in this tree
/// edition, or the category/package lookup fails for any other reason.
fn tree_versions(repo: &Repository, category: &str, package: &str) -> Vec<Version> {
    repo.category(category)
        .and_then(|c| c.package(package))
        .and_then(|p| p.ebuilds().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.version().clone())
        .collect()
}

/// Policy from `todo/local-bootstrap-provided.md`'s "Floor versions when
/// host has no VDB": the closest tree version `<= host` when a host probe
/// succeeded and the tree has one that qualifies, else the oldest tree
/// version — never an invented version absent from the tree.
fn pick_version(versions: &[Version], host: Option<&Version>) -> Option<Version> {
    if let Some(host) = host
        && let Some(best) = versions.iter().filter(|v| *v <= host).max()
    {
        return Some(best.clone());
    }
    versions.iter().min().cloned()
}

fn rewrite_managed_block(existing: &str, block: &str) -> String {
    if let Some(start) = existing.find(BEGIN_MARKER)
        && let Some(end) = existing[start..].find(END_MARKER).map(|i| start + i)
    {
        let after = existing[end..]
            .find('\n')
            .map_or(existing.len(), |i| end + i + 1);
        return format!("{}{block}{}", &existing[..start], &existing[after..]);
    }
    if existing.is_empty() {
        block.to_string()
    } else {
        format!("{}\n{block}", existing.trim_end_matches('\n'))
    }
}

/// Write (or refresh) the managed `package.provided` block. Unlike
/// [`super::repo::ensure_repo`]/[`super::local_profile::ensure_profile`],
/// this re-derives and rewrites the block on every `em setup --local` run —
/// the host's tool versions can legitimately drift between runs, and the
/// doc's format spec calls for "rewrite only the `BEGIN`…`END` region on
/// setup re-run", preserving any hand-written lines outside the markers.
pub(super) fn ensure_provided(eroot: &Utf8Path, repo: &Repository) -> Result<()> {
    let path = eroot.join("etc/portage/profile/package.provided");

    let mut lines = Vec::new();
    for pkg in TIER1 {
        let versions = tree_versions(repo, pkg.category, pkg.package);
        if versions.is_empty() {
            continue;
        }
        let host_version = pkg.probe.and_then(|(bin, args)| probe_version(bin, args));
        if let Some(v) = pick_version(&versions, host_version.as_ref()) {
            lines.push(format!("{}/{}-{v}", pkg.category, pkg.package));
        }
    }

    let mut block = String::new();
    block.push_str(BEGIN_MARKER);
    block.push_str(
        "\n# generated-by: em setup\n# preset: any-linux\n\
         # regenerated: do not hand-edit inside this block\n",
    );
    for line in &lines {
        block.push_str(line);
        block.push('\n');
    }
    block.push_str(END_MARKER);
    block.push('\n');

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let desired = rewrite_managed_block(&existing, &block);
    if desired == existing {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tracing::info!(
        entries = lines.len(),
        "package.provided bootstrap block written"
    );
    std::fs::write(&path, desired)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_version_token_handles_common_banners() {
        assert_eq!(
            first_version_token("Python 3.11.2"),
            Some("3.11.2".to_string())
        );
        assert_eq!(
            first_version_token("*** Zstandard CLI (64-bit) v1.5.5, by Yann Collet ***"),
            Some("1.5.5".to_string())
        );
        assert_eq!(
            first_version_token("gcc (Gentoo 14.2.1_p20241221 p13) 14.2.1 20241221"),
            Some("14.2.1_p20241221".to_string())
        );
        assert_eq!(first_version_token("no digits here"), None);
    }

    #[test]
    fn pick_version_prefers_closest_at_or_below_host() {
        let versions = vec![
            Version::parse("1.0").unwrap(),
            Version::parse("2.0").unwrap(),
            Version::parse("3.0").unwrap(),
        ];
        let host = Version::parse("2.5").unwrap();
        assert_eq!(
            pick_version(&versions, Some(&host)),
            Some(Version::parse("2.0").unwrap())
        );
    }

    #[test]
    fn pick_version_falls_back_to_oldest_when_host_is_older_than_everything() {
        let versions = vec![
            Version::parse("2.0").unwrap(),
            Version::parse("3.0").unwrap(),
        ];
        let host = Version::parse("1.0").unwrap();
        assert_eq!(
            pick_version(&versions, Some(&host)),
            Some(Version::parse("2.0").unwrap())
        );
    }

    #[test]
    fn pick_version_falls_back_to_oldest_when_no_host_probe() {
        let versions = vec![
            Version::parse("2.0").unwrap(),
            Version::parse("1.0").unwrap(),
        ];
        assert_eq!(
            pick_version(&versions, None),
            Some(Version::parse("1.0").unwrap())
        );
    }

    #[test]
    fn rewrite_managed_block_replaces_only_the_marked_region() {
        let existing = "# user line before\n\
             # BEGIN em-bootstrap-provided\n\
             stale/entry-1.0\n\
             # END em-bootstrap-provided\n\
             # user line after\n";
        let block = "# BEGIN em-bootstrap-provided\nfresh/entry-2.0\n# END em-bootstrap-provided\n";
        let got = rewrite_managed_block(existing, block);
        assert_eq!(
            got,
            "# user line before\n\
             # BEGIN em-bootstrap-provided\nfresh/entry-2.0\n# END em-bootstrap-provided\n\
             # user line after\n"
        );
    }

    #[test]
    fn rewrite_managed_block_appends_when_no_prior_block() {
        let existing = "# hand-written line\n";
        let block = "# BEGIN em-bootstrap-provided\nfresh/entry-2.0\n# END em-bootstrap-provided\n";
        let got = rewrite_managed_block(existing, block);
        assert_eq!(
            got,
            "# hand-written line\n# BEGIN em-bootstrap-provided\nfresh/entry-2.0\n# END em-bootstrap-provided\n"
        );
    }
}
