use std::path::{Path, PathBuf};

use cargo_lock::Lockfile;
use thiserror::Error;

const CRATE_REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageMetadata {
    pub name: String,
    pub version: String,
    pub license: Option<String>,
    pub license_file: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub features: std::collections::BTreeMap<String, bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileCrate {
    pub name: String,
    pub version: String,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GitHost {
    Github,
    Gitlab,
    GitlabSelfHosted,
    Gitea,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitCrate {
    pub name: String,
    pub version: String,
    pub repository: String,
    pub commit: String,
    pub host: GitHost,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Crate {
    File(FileCrate),
    Git(GitCrate),
}

impl Crate {
    pub fn name(&self) -> &str {
        match self {
            Self::File(c) => &c.name,
            Self::Git(c) => &c.name,
        }
    }
    pub fn version(&self) -> &str {
        match self {
            Self::File(c) => &c.version,
            Self::Git(c) => &c.version,
        }
    }
    pub fn filename(&self) -> String {
        match self {
            Self::File(c) => format!("{}-{}.crate", c.name, c.version),
            Self::Git(c) => {
                let repo = c.repository.rsplit('/').next().unwrap_or(&c.repository);
                let ext = match c.host {
                    GitHost::Github => ".gh",
                    GitHost::Gitlab => ".gl",
                    GitHost::Gitea => ".gt",
                    GitHost::GitlabSelfHosted => "",
                };
                format!("{repo}-{}{ext}.tar.gz", c.commit)
            }
        }
    }
    pub fn download_url(&self) -> String {
        match self {
            Self::File(c) => {
                format!(
                    "https://static.crates.io/crates/{}/{}/download",
                    c.name, c.version
                )
            }
            Self::Git(c) => match c.host {
                GitHost::Github | GitHost::Gitea => {
                    format!("{}/archive/{}.tar.gz", c.repository, c.commit)
                }
                GitHost::Gitlab | GitHost::GitlabSelfHosted => {
                    let repo = c.repository.rsplit('/').next().unwrap_or(&c.repository);
                    format!(
                        "{}/-/archive/{}/{repo}-{}.tar.gz",
                        c.repository, c.commit, c.commit
                    )
                }
            },
        }
    }
    /// Cargo.eclass `GIT_CRATES` entry value — mirrors `pycargoebuild/cargo.py:get_git_crate_entry`
    pub fn git_crate_entry(&self, subdir: &str) -> Option<String> {
        let GitCrate {
            repository,
            commit,
            host,
            ..
        } = match self {
            Self::Git(g) => g,
            _ => return None,
        };
        let subdir = subdir.replace(commit, "%commit%");
        match host {
            GitHost::Github | GitHost::Gitlab => Some(format!("{repository};{commit};{subdir}")),
            GitHost::Gitea => Some(format!("{repository};{commit};{subdir};gitea")),
            GitHost::GitlabSelfHosted => {
                let uri = format!("{repository}/-/archive/%commit%/x-%commit%.tar.gz")
                    .replace(commit, "%commit%");
                // use real download_url with placeholder
                let crate_uri = self.download_url().replace(commit, "%commit%");
                let _ = uri;
                Some(format!("{crate_uri};{commit};{subdir}"))
            }
        }
    }
    pub fn crate_entry(&self) -> Option<String> {
        match self {
            Self::File(c) => Some(format!("{}@{}", c.name, c.version)),
            Self::Git(_) => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum CargoError {
    #[error("unsupported Cargo.lock version")]
    UnsupportedLockVersion,
    #[error("cargo-lock parse: {0}")]
    LockParse(#[from] cargo_lock::Error),
    #[error("invalid git source {0:?}")]
    InvalidGitSource(String),
    #[error("unsupported host {0:?}")]
    UnsupportedHost(String),
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Classify git host — same set as `pycargoebuild/cargo.py:GitHost`
pub fn classify_host(repo: &str) -> Result<GitHost, CargoError> {
    if repo.starts_with("https://github.com/") {
        Ok(GitHost::Github)
    } else if repo.starts_with("https://gitlab.com/") {
        Ok(GitHost::Gitlab)
    } else if repo.starts_with("https://gitlab.") {
        Ok(GitHost::GitlabSelfHosted)
    } else if repo.starts_with("https://codeberg.org/") {
        Ok(GitHost::Gitea)
    } else {
        Err(CargoError::UnsupportedHost(repo.to_string()))
    }
}

/// Load crates from a `Cargo.lock` file — offline, no network.
pub fn crates_from_lockfile(path: &Path) -> Result<Vec<Crate>, CargoError> {
    let lock = Lockfile::load(path)?;
    let mut out = Vec::new();
    for pkg in &lock.packages {
        let src = match &pkg.source {
            None => continue, // local path crate
            Some(s) => s,
        };
        if src.is_registry() {
            // pycargoebuild only handles crates.io registry (CRATE_REGISTRY), cargo-lock normalizes sparse as well
            let checksum = pkg
                .checksum
                .as_ref()
                .map(|c| c.to_string())
                .unwrap_or_default();
            out.push(Crate::File(FileCrate {
                name: pkg.name.to_string(),
                version: pkg.version.to_string(),
                checksum,
            }));
        } else if src.is_git() {
            // `cargo-lock` gives url without `git+` prefix; precise holds commit
            let url_str = src.url().to_string();
            let commit = src
                .precise()
                .map(|p| p.to_string())
                .or_else(|| pkg.checksum.as_ref().map(|c| c.to_string()))
                .unwrap_or_default();
            let repo = url_str
                .trim_start_matches("git+")
                .split(['?', '#'])
                .next()
                .unwrap_or(&url_str)
                .trim_end_matches(".git")
                .to_string();
            let host = classify_host(&repo)?;
            out.push(Crate::Git(GitCrate {
                name: pkg.name.to_string(),
                version: pkg.version.to_string(),
                host,
                repository: repo,
                commit,
            }));
        } else {
            return Err(CargoError::InvalidGitSource(format!("{src:?}")));
        }
    }
    let _ = CRATE_REGISTRY;
    Ok(out)
}

/// Minimal `Cargo.toml` package read — mirrors `pycargoebuild/cargo.py:get_package_metadata`
pub fn package_from_toml(path: &Path) -> Result<PackageMetadata, CargoError> {
    let s = std::fs::read_to_string(path)?;
    let manifest = cargo_toml::Manifest::from_str(&s)
        .map_err(|e| CargoError::Io(std::io::Error::other(e.to_string())))?;
    let pkg = manifest
        .package
        .ok_or_else(|| CargoError::Io(std::io::Error::other("missing [package]")))?;
    let name = pkg.name.clone();
    let version = pkg
        .version
        .get()
        .map(|s| s.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    let license = pkg
        .license
        .as_ref()
        .and_then(|l| l.get().ok())
        .map(|s| s.replace('/', " OR "));
    let license_file = pkg
        .license_file
        .as_ref()
        .and_then(|l| l.get().ok())
        .map(|s| s.display().to_string());
    let description = pkg
        .description
        .as_ref()
        .and_then(|d| d.get().ok())
        .map(|s| s.to_string());
    let homepage = pkg
        .homepage
        .as_ref()
        .and_then(|h| h.get().ok())
        .map(|s| s.to_string());
    let features = manifest
        .features
        .keys()
        .map(|k| {
            let is_default = manifest
                .features
                .get("default")
                .map(|defs| defs.iter().any(|x| x == k))
                .unwrap_or(false);
            (k.clone(), is_default)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    // filter default itself out, keep others
    let features: std::collections::BTreeMap<String, bool> = features
        .into_iter()
        .filter(|(k, _)| k != "default")
        .collect();
    Ok(PackageMetadata {
        name,
        version,
        license,
        license_file,
        description,
        homepage,
        features,
    })
}

/// Open the fetched crate archive at `distdir/<filename>` for scanning.
fn open_crate_archive(
    krate: &Crate,
    distdir: &Path,
) -> Option<tar::Archive<flate2::read::GzDecoder<std::fs::File>>> {
    let file = std::fs::File::open(distdir.join(krate.filename())).ok()?;
    Some(tar::Archive::new(flate2::read::GzDecoder::new(file)))
}

/// Read license from a crate's `Cargo.toml` inside the archive fetched to
/// `DISTDIR` — mirrors `pycargoebuild/ebuild.py:get_license_from_crate`.
/// The crate is expected to have already been fetched there (see
/// `fetch::fetch_crates`); this does no fetching of its own.
pub fn license_from_crate(krate: &Crate, distdir: &Path) -> Option<String> {
    let mut ar = open_crate_archive(krate, distdir)?;
    for entry in ar.entries().ok()? {
        let mut entry = entry.ok()?;
        let entry_path = entry.path().ok()?.to_path_buf();
        if entry_path.file_name().and_then(|n| n.to_str()) != Some("Cargo.toml") {
            continue;
        }
        let mut s = String::new();
        use std::io::Read;
        entry.read_to_string(&mut s).ok()?;
        let Ok(manifest) = cargo_toml::Manifest::from_str(&s) else {
            continue;
        };
        let Some(pkg) = manifest.package else {
            continue;
        };
        if pkg.name != krate.name() {
            continue;
        }
        if let Some(lic) = pkg.license.and_then(|l| l.get().ok().cloned()) {
            return Some(lic.replace('/', " OR "));
        }
        // pycargoebuild warns and skips crates that only set `license-file`
        // — the license text needs manual review, it can't be auto-mapped.
        return None;
    }
    None
}

/// Find the directory (relative to the archive root) containing this
/// crate's `Cargo.toml`, matched by `name`+`version` — ported independently
/// from `pycargoebuild/cargo.py:get_package_directory` (`cargo.py:140-165`).
///
/// A `FileCrate` from crates.io is always `name-version/`; a `GitCrate`
/// archive needs this because the real in-repo subdir can be nested (a
/// workspace member) or differ from any name we could otherwise guess —
/// this is the actual fix for the `GIT_CRATES` subdir cargo.eclass expects,
/// previously fabricated as a placeholder. Returns `None` for "archive
/// root" or "not found".
pub fn package_directory_in_archive(krate: &Crate, distdir: &Path) -> Option<String> {
    let mut ar = open_crate_archive(krate, distdir)?;
    let mut name_only_fallback: Option<String> = None;
    for entry in ar.entries().ok()?.flatten() {
        let mut entry = entry;
        let Ok(entry_path) = entry.path().map(|p| p.to_path_buf()) else {
            continue;
        };
        if entry_path.file_name().and_then(|n| n.to_str()) != Some("Cargo.toml") {
            continue;
        }
        let mut s = String::new();
        use std::io::Read;
        if entry.read_to_string(&mut s).is_err() {
            continue;
        }
        let Ok(manifest) = cargo_toml::Manifest::from_str(&s) else {
            continue;
        };
        let Some(pkg) = manifest.package else {
            continue;
        };
        if pkg.name != krate.name() {
            continue;
        }
        let subdir = entry_path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|p| !p.is_empty());
        let version = pkg
            .version
            .get()
            .map(|s| s.as_str())
            .unwrap_or("0.0.0")
            .to_string();
        if version == krate.version() {
            return subdir;
        }
        // Workspace-inherited `version.workspace = true` can't always be
        // resolved without the workspace root's Cargo.toml — keep the
        // first name match as a fallback rather than giving up entirely.
        name_only_fallback.get_or_insert(subdir.unwrap_or_default());
    }
    name_only_fallback.filter(|s| !s.is_empty())
}

/// Workspace-aware crate discovery — walk parents for `Cargo.lock` like `pycargoebuild/__main__.py:get_workspace_root`
pub fn find_lock(start: &Path) -> Option<PathBuf> {
    let mut cur = start.canonicalize().ok()?;
    loop {
        let cand = cur.join("Cargo.lock");
        if cand.is_file() {
            return Some(cand);
        }
        if !cur.pop() {
            break;
        }
    }
    None
}
