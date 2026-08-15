//! `em --local setup`'s bootstrap `package.provided` — step 4 of the
//! config-root ladder (`todo/local-bootstrap-provided.md`). There is no host
//! VDB to read on a non-Gentoo host (or a Gentoo host used only through
//! `--local`, which never weaves the host VDB into BDEPEND satisfaction —
//! see that doc's "why provided is stronger than break cycles"), so the
//! version for each Tier-1 cycle-fuel CPN is picked by probing the host's
//! own tool (`gcc --version`, `python3 -V`, …) and mapping that to the
//! closest tree-present version.
//!
//! A tool the host does not have is left out entirely rather than claimed at
//! some floor version: the entry exists to say "the system already supplies
//! this", and inventing one turns a missing tool into a `command not found`
//! deep inside an unrelated package's phase instead of a package the prefix
//! plans and builds for itself.

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Version;
use portage_repo::Repository;

/// How to check whether the host already has a Tier-1 package's real-world
/// equivalent.
enum Probe {
    /// Run `bin args...` and parse a PMS-shaped version out of the banner
    /// (`gcc --version`, `python3 -V`, …) — [`pick_version`] then maps that
    /// to the closest tree-present version.
    Command(&'static str, &'static [&'static str]),
    /// Run `bin args...` and check only the exit status — no version
    /// banner to parse (`sys-kernel/linux-headers` has none). Used for
    /// `linux-headers` to compile-check `#include <linux/version.h>`
    /// through the actual host C preprocessor — the same header real
    /// portage's own `virtual/os-headers` checks for, but through the
    /// compiler that will actually consume it rather than a bare
    /// `stat()`, so a compiler with a narrower search path than the
    /// filesystem check would expect still gets caught. When it succeeds,
    /// claim the tree's newest version: the declared CPV is otherwise
    /// inert for a provided entry — nothing reads it back, and whatever
    /// actually gets included at build time is the host's real
    /// `/usr/include`, not this string.
    CommandSucceeds(&'static str, &'static [&'static str]),
}

/// One Tier-1 "cycle fuel" package: the bootstrap closure's build-tool
/// dependencies (never the stage products themselves — `sys-apps/baselayout`,
/// binutils, headers, libc, gcc must still be *built* into the prefix, see
/// the design doc's "explicitly out of provided" list). `probe` is how to
/// check the host already has this; `None` when there's no real host
/// equivalent to probe at all (`elt-patches` is a Gentoo-only eclass-patches
/// snapshot — unlike [`Probe::CommandSucceeds`], there's nothing to check,
/// it's simply always claimed).
struct Tier1Pkg {
    category: &'static str,
    package: &'static str,
    probe: Option<Probe>,
}

const TIER1: &[Tier1Pkg] = &[
    Tier1Pkg {
        category: "dev-lang",
        package: "perl",
        probe: Some(Probe::Command("perl", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "meson",
        probe: Some(Probe::Command("meson", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "ninja",
        probe: Some(Probe::Command("ninja", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "cmake",
        probe: Some(Probe::Command("cmake", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "make",
        probe: Some(Probe::Command("make", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "autoconf",
        probe: Some(Probe::Command("autoconf", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "automake",
        probe: Some(Probe::Command("automake", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-devel",
        package: "m4",
        probe: Some(Probe::Command("m4", &["--version"])),
    },
    Tier1Pkg {
        category: "dev-build",
        package: "libtool",
        probe: Some(Probe::Command("libtool", &["--version"])),
    },
    // `virtual/os-headers`'s own RDEPEND (`!prefix-guest? ( kernel_linux? (
    // sys-kernel/linux-headers:0 ) )`) already skips this under
    // `prefix-guest`, but that only covers packages that reach it through
    // the virtual — anything that BDEPENDs on the concrete CPN directly (or
    // hits it while the depgraph explores candidates, independent of which
    // branch the final plan lands on) still needs a real answer. `cc -E
    // -include linux/version.h` is the actual consumer, so check through it
    // rather than a bare `stat()` on the marker file.
    Tier1Pkg {
        category: "sys-kernel",
        package: "linux-headers",
        probe: Some(Probe::CommandSucceeds(
            "cc",
            &[
                "-E",
                "-x",
                "c",
                "-include",
                "linux/version.h",
                "-",
                "-o",
                "/dev/null",
            ],
        )),
    },
    // `sys-devel/gcc`'s own RDEPEND is `elibc_glibc? ( sys-libs/glibc[...] )`
    // — gated on `ELIBC` (which libc family this Linux system uses at all),
    // not on `prefix-guest` (which only means "don't build one from source
    // in the prefix"). Skipping the dedicated libc step under prefix-guest
    // (`toolchain_plan`) does nothing to satisfy this edge by itself; found
    // live — the "gcc" step still pulled a full from-scratch glibc, hitting
    // the real bootstrap cycle (glibc BDEPENDs on an existing gcc that
    // doesn't exist yet in an empty root) that prefix-guest exists to avoid.
    Tier1Pkg {
        category: "sys-libs",
        package: "glibc",
        probe: Some(Probe::Command("ldd", &["--version"])),
    },
    Tier1Pkg {
        category: "app-portage",
        package: "elt-patches",
        probe: None,
    },
    Tier1Pkg {
        category: "app-arch",
        package: "xz-utils",
        probe: Some(Probe::Command("xz", &["--version"])),
    },
    Tier1Pkg {
        category: "app-arch",
        package: "zstd",
        probe: Some(Probe::Command("zstd", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-devel",
        package: "gettext",
        probe: Some(Probe::Command("gettext", &["--version"])),
    },
    // No single `coreutils`/`findutils` binary reports its own package
    // version; `ls`/`find` do (`ls (GNU coreutils) 9.4`).
    Tier1Pkg {
        category: "sys-apps",
        package: "coreutils",
        probe: Some(Probe::Command("ls", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-apps",
        package: "findutils",
        probe: Some(Probe::Command("find", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-apps",
        package: "gawk",
        probe: Some(Probe::Command("gawk", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-apps",
        package: "grep",
        probe: Some(Probe::Command("grep", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-apps",
        package: "sed",
        probe: Some(Probe::Command("sed", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-apps",
        package: "file",
        probe: Some(Probe::Command("file", &["--version"])),
    },
    Tier1Pkg {
        category: "sys-devel",
        package: "patch",
        probe: Some(Probe::Command("patch", &["--version"])),
    },
    // bzip2 historically doesn't reliably support `--version`; `-h` always
    // prints the same version banner.
    Tier1Pkg {
        category: "app-arch",
        package: "bzip2",
        probe: Some(Probe::Command("bzip2", &["-h"])),
    },
    Tier1Pkg {
        category: "app-arch",
        package: "gzip",
        probe: Some(Probe::Command("gzip", &["--version"])),
    },
    Tier1Pkg {
        category: "app-arch",
        package: "tar",
        probe: Some(Probe::Command("tar", &["--version"])),
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

/// Run `bin args...` and extract a best-guess host version. `None` when the
/// run fails or nothing version-shaped comes out — an unusual banner is not
/// an error, [`pick_version`] just falls back to the tree's own floor.
fn probe_version(bin: &Utf8Path, args: &[&str]) -> Option<Version> {
    let output = std::process::Command::new(bin).args(args).output().ok()?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push('\n');
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Version::parse(&first_version_token(&combined)?).ok()
}

/// Run `bin args...` with stdin closed and report only whether it exited
/// zero — for [`Probe::CommandSucceeds`], where the check *is* the
/// version's absence (no banner to parse).
fn command_succeeds(bin: &Utf8Path, args: &[&str]) -> bool {
    std::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success())
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

/// The tree version that best represents the host's probed one, else the
/// oldest tree version — never an invented version absent from the tree.
///
/// Multi-SLOT packages (`dev-lang/python`'s `SLOT="${PYVER}"`, one SLOT per
/// `major.minor`) break a plain "closest tree version `<= host`" compare:
/// the tree's ebuild for the *host's own* `major.minor` line almost always
/// has a higher micro/patch than whatever shipped with the host years ago
/// (e.g. host `python3 -V` → `3.11.2`, tree only has `3.11.15`+), so the
/// naive compare skips straight past the right SLOT into an older, wrong
/// one (`3.10.9999`) — which then fails to satisfy a `dev-lang/python:3.11`
/// dependency at all. Prefer a same-`major.minor` tree version (any patch
/// level) over a strictly-older one from a different line.
fn pick_version(versions: &[Version], host: Option<&Version>) -> Option<Version> {
    if let Some(host) = host {
        let n = host.numbers.len().min(2);
        let same_line = Version::new(&host.numbers[..n]);
        if let Some(best) = versions.iter().filter(|v| v.glob_matches(&same_line)).max() {
            return Some(best.clone());
        }
        if let Some(best) = versions.iter().filter(|v| *v <= host).max() {
            return Some(best.clone());
        }
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
pub(super) fn ensure_provided(
    eroot: &Utf8Path,
    repo: &Repository,
    extra_path: &[Utf8PathBuf],
) -> Result<()> {
    let path = eroot.join("etc/portage/profile/package.provided");

    let mut lines = Vec::new();
    for pkg in TIER1 {
        let versions = tree_versions(repo, pkg.category, pkg.package);
        if versions.is_empty() {
            continue;
        }
        let mut host_version = None;
        let mut newest_if_present = false;
        match &pkg.probe {
            Some(Probe::Command(bin, args)) => {
                let Some(found) = super::host_tools::which(bin, extra_path) else {
                    tracing::info!(
                        "no host {bin}: the prefix will build {}/{} itself",
                        pkg.category,
                        pkg.package
                    );
                    continue;
                };
                host_version = probe_version(&found, args);
            }
            Some(Probe::CommandSucceeds(bin, args)) => {
                let Some(found) = super::host_tools::which(bin, extra_path) else {
                    tracing::info!(
                        "no host {bin}: the prefix will build {}/{} itself",
                        pkg.category,
                        pkg.package
                    );
                    continue;
                };
                if !command_succeeds(&found, args) {
                    tracing::info!(
                        "host {bin} {args:?} failed: the prefix will build {}/{} itself",
                        pkg.category,
                        pkg.package
                    );
                    continue;
                }
                newest_if_present = true;
            }
            None => {}
        }
        let picked = if newest_if_present {
            versions.iter().max().cloned()
        } else {
            pick_version(&versions, host_version.as_ref())
        };
        if let Some(v) = picked {
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
    fn pick_version_prefers_same_major_minor_line_over_an_older_slot() {
        // Real dev-lang/python shape: tree's 3.11 line has moved past the
        // host's old 3.11.2 (patch-level newer), while an unrelated older
        // SLOT (3.10) happens to sit just below host by raw compare —
        // picking 3.10.9999 would fail a `dev-lang/python:3.11` dependency.
        let versions = vec![
            Version::parse("3.10.9999").unwrap(),
            Version::parse("3.11.15").unwrap(),
            Version::parse("3.11.9999").unwrap(),
            Version::parse("3.14.7").unwrap(),
        ];
        let host = Version::parse("3.11.2").unwrap();
        assert_eq!(
            pick_version(&versions, Some(&host)),
            Some(Version::parse("3.11.9999").unwrap())
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
