//! The package being generated for, as `cargo metadata` resolves it, and its
//! locked dependencies.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use cargo_lock::Lockfile;
use serde_json::Value;

/// The package `cargo eb` generates an ebuild for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageMetadata {
    pub name: String,
    /// Cargo (semver) version.
    pub version: String,
    pub license: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub features: Features,
    /// `false` when `publish = false` (or an empty registry list).
    pub publish: bool,
}

/// Cargo features split into what the ebuild can toggle and what it cannot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Features {
    /// USE-flag-valid feature names; `true` for members of `default`.
    pub iuse: BTreeMap<String, bool>,
    /// `default` entries that are not flags (`dep/feat`, invalid USE names), always enabled.
    pub always: Vec<String>,
    /// Whether to pass `--no-default-features`; false when `default` holds a
    /// `dep:` entry, which `--features` cannot re-enable.
    pub no_default: bool,
}

impl Features {
    fn from_metadata(features: &serde_json::Map<String, Value>) -> Self {
        let default: Vec<&str> = features
            .get("default")
            .and_then(Value::as_array)
            .map(|d| d.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut iuse = BTreeMap::new();
        for name in features.keys().filter(|k| *k != "default") {
            if is_use_flag(name) {
                iuse.insert(name.clone(), default.contains(&name.as_str()));
            } else {
                tracing::warn!(
                    "feature {name:?} is not a valid USE flag name, not exposed in IUSE"
                );
            }
        }
        let mut always = Vec::new();
        let mut no_default = !iuse.is_empty();
        for entry in default {
            if entry.starts_with("dep:") {
                tracing::warn!(
                    "default feature {entry:?} cannot be passed to --features, keeping defaults on"
                );
                no_default = false;
            } else if !iuse.contains_key(entry) {
                always.push(entry.to_string());
            }
        }
        Self {
            iuse,
            always,
            no_default,
        }
    }
}

/// PMS 3.1.4 USE flag name.
fn is_use_flag(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '_' | '@' | '-'))
}

impl PackageMetadata {
    fn from_metadata(pkg: &Value) -> Result<Self> {
        let str_field = |k: &str| pkg[k].as_str().map(str::to_string);
        let publish = match &pkg["publish"] {
            Value::Array(regs) => !regs.is_empty(),
            _ => true,
        };
        Ok(Self {
            name: str_field("name").context("package without a name")?,
            version: str_field("version").context("package without a version")?,
            license: str_field("license").map(|l| l.replace('/', " OR ")),
            description: str_field("description"),
            homepage: str_field("homepage"),
            features: pkg["features"]
                .as_object()
                .map(Features::from_metadata)
                .unwrap_or_default(),
            publish,
        })
    }

    /// Crates.io-style release: publishable and no semver pre-release suffix.
    pub fn is_release(&self) -> bool {
        self.publish && !self.version.contains('-')
    }

    /// `${P}` for the ebuild: the crate name and [`gentoo_version`] of its version.
    pub fn gentoo_p(&self) -> Result<String> {
        let p = format!("{}-{}", self.name, gentoo_version(&self.version)?);
        portage_atom::Cpv::parse(&format!("dev-rust/{p}"))
            .with_context(|| format!("{p} is not a valid Gentoo package name and version"))?;
        Ok(p)
    }
}

/// Map a semver version to a PMS one: `1.0.0-rc.2` → `1.0.0_rc2`.
///
/// `alpha`/`beta`/`rc` keep their suffix, any other pre-release tag becomes `_pre`,
/// and build metadata is dropped.
pub fn gentoo_version(version: &str) -> Result<String> {
    let v = cargo_lock::Version::parse(version)
        .with_context(|| format!("parsing version {version}"))?;
    let mut out = format!("{}.{}.{}", v.major, v.minor, v.patch);
    if !v.pre.is_empty() {
        let mut idents = v.pre.as_str().split(['.', '-']);
        let first = idents.next().unwrap_or_default().to_ascii_lowercase();
        let tag = first.trim_end_matches(|c: char| c.is_ascii_digit());
        let mut num = first[tag.len()..].to_string();
        if num.is_empty()
            && let Some(n) = idents
                .next()
                .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
        {
            num = n.to_string();
        }
        let suffix = match tag {
            "alpha" | "a" => "alpha",
            "beta" | "b" => "beta",
            "rc" => "rc",
            _ => "pre",
        };
        out.push_str(&format!("_{suffix}{num}"));
    }
    portage_atom::Version::parse(&out)
        .with_context(|| format!("{version} maps to invalid {out}"))?;
    Ok(out)
}

/// A crates.io crate from `Cargo.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileCrate {
    pub name: String,
    pub version: String,
    /// sha256 from `Cargo.lock`; empty for old lockfiles without one.
    pub checksum: String,
}

/// Forge hosting a git dependency; decides its archive URL and `GIT_CRATES` form.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GitHost {
    Github,
    Gitlab,
    GitlabSelfHosted,
    Gitea,
}

/// A git dependency pinned to a commit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitCrate {
    pub name: String,
    pub version: String,
    pub repository: String,
    pub commit: String,
    pub host: GitHost,
}

/// A locked dependency that `--no-tarball` mode lists in `CRATES`/`GIT_CRATES`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Crate {
    File(FileCrate),
    Git(GitCrate),
}

impl Crate {
    /// Crate name.
    pub fn name(&self) -> &str {
        match self {
            Self::File(c) => &c.name,
            Self::Git(c) => &c.name,
        }
    }

    /// Crate version.
    pub fn version(&self) -> &str {
        match self {
            Self::File(c) => &c.version,
            Self::Git(c) => &c.version,
        }
    }

    /// DISTDIR file name, matching what cargo.eclass renames the download to.
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

    /// Where the archive is downloaded from.
    pub fn download_url(&self) -> String {
        match self {
            Self::File(c) => format!(
                "https://static.crates.io/crates/{}/{}/download",
                c.name, c.version
            ),
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

    /// cargo.eclass `GIT_CRATES` value (`uri;commit;subdir[;host]`), `None` for crates.io crates.
    pub fn git_crate_entry(&self, subdir: &str) -> Option<String> {
        let Self::Git(GitCrate {
            repository,
            commit,
            host,
            ..
        }) = self
        else {
            return None;
        };
        let subdir = subdir.replace(commit, "%commit%");
        Some(match host {
            GitHost::Github | GitHost::Gitlab => format!("{repository};{commit};{subdir}"),
            GitHost::Gitea => format!("{repository};{commit};{subdir};gitea"),
            GitHost::GitlabSelfHosted => {
                let uri = self.download_url().replace(commit, "%commit%");
                format!("{uri};{commit};{subdir}")
            }
        })
    }

    /// `CRATES` entry (`name@version`), `None` for git crates.
    pub fn crate_entry(&self) -> Option<String> {
        match self {
            Self::File(c) => Some(format!("{}@{}", c.name, c.version)),
            Self::Git(_) => None,
        }
    }
}

fn classify_host(repo: &str) -> Result<GitHost> {
    Ok(if repo.starts_with("https://github.com/") {
        GitHost::Github
    } else if repo.starts_with("https://gitlab.com/") {
        GitHost::Gitlab
    } else if repo.starts_with("https://gitlab.") {
        GitHost::GitlabSelfHosted
    } else if repo.starts_with("https://codeberg.org/") {
        GitHost::Gitea
    } else {
        bail!("unsupported git host {repo:?}; use the vendor tarball instead of --no-tarball")
    })
}

/// Locked crates.io and git dependencies of `path`, for `CRATES`/`GIT_CRATES`.
pub fn crates_from_lockfile(path: &Path) -> Result<Vec<Crate>> {
    let lock = Lockfile::load(path)?;
    let mut out = Vec::new();
    for pkg in &lock.packages {
        let Some(src) = &pkg.source else { continue };
        let (name, version) = (pkg.name.to_string(), pkg.version.to_string());
        if src.is_default_registry() {
            let checksum = pkg
                .checksum
                .as_ref()
                .map(|c| c.to_string())
                .unwrap_or_default();
            out.push(Crate::File(FileCrate {
                name,
                version,
                checksum,
            }));
        } else if src.is_git() {
            let commit = src
                .precise()
                .with_context(|| format!("git dependency {name} is not pinned to a commit"))?
                .to_string();
            let url = src.url().as_str();
            let repository = url
                .split(['?', '#'])
                .next()
                .unwrap_or(url)
                .trim_end_matches(".git")
                .to_string();
            let host = classify_host(&repository)?;
            out.push(Crate::Git(GitCrate {
                name,
                version,
                repository,
                commit,
                host,
            }));
        } else {
            bail!(
                "{name}-{version} comes from {src}, which CRATES cannot express; use the vendor tarball"
            );
        }
    }
    Ok(out)
}

/// Each package manifest named `krate` inside its archive fetched to `distdir`,
/// with the directory it sits in.
fn manifests_in_archive(krate: &Crate, distdir: &Path) -> Vec<(PathBuf, cargo_toml::Package)> {
    let Ok(file) = std::fs::File::open(distdir.join(krate.filename())) else {
        return Vec::new();
    };
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let Ok(entries) = ar.entries() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for mut entry in entries.flatten() {
        let Ok(path) = entry.path().map(|p| p.into_owned()) else {
            continue;
        };
        if path.file_name() != Some(OsStr::new("Cargo.toml")) {
            continue;
        }
        let mut s = String::new();
        if entry.read_to_string(&mut s).is_err() {
            continue;
        }
        let Some(pkg) = cargo_toml::Manifest::from_str(&s)
            .ok()
            .and_then(|m| m.package)
        else {
            continue;
        };
        if pkg.name == krate.name() {
            let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            out.push((dir, pkg));
        }
    }
    out
}

/// SPDX license of a crate fetched to `distdir`; `None` when it only has `license-file`.
pub fn license_from_crate(krate: &Crate, distdir: &Path) -> Option<String> {
    manifests_in_archive(krate, distdir)
        .into_iter()
        .find_map(|(_, pkg)| pkg.license.and_then(|l| l.get().ok().cloned()))
        .map(|l| l.replace('/', " OR "))
}

/// Directory of `krate`'s `Cargo.toml` inside its fetched archive, preferring a
/// name+version match; `None` for the archive root or when not found.
pub fn package_directory_in_archive(krate: &Crate, distdir: &Path) -> Option<String> {
    let found = manifests_in_archive(krate, distdir);
    let exact = found
        .iter()
        .find(|(_, pkg)| pkg.version.get().is_ok_and(|v| v == krate.version()));
    // `version.workspace = true` can't be resolved from the member alone.
    exact
        .or(found.first())
        .map(|(dir, _)| dir.to_string_lossy().into_owned())
        .filter(|d| !d.is_empty())
}

/// SPDX license of a vendored dependency; `None` when it only has `license-file`.
pub fn crate_license(manifest: &Path) -> Option<String> {
    let s = std::fs::read_to_string(manifest).ok()?;
    let pkg = cargo_toml::Manifest::from_str(&s).ok()?.package?;
    pkg.license
        .and_then(|l| l.get().ok().cloned())
        .map(|l| l.replace('/', " OR "))
}

/// The package to generate for, with its lockfile and the tree to snapshot.
pub struct PreparedPackage {
    _scratch: Option<tempfile::TempDir>,
    pub package: PackageMetadata,
    pub manifest: PathBuf,
    pub lock: PathBuf,
    /// Directory to pack for a source snapshot (isolated workspace or the package).
    pub source_root: PathBuf,
    /// The package's directory under `source_root` when that is an isolated workspace.
    pub member_dir: Option<PathBuf>,
    /// Directory cargo runs from, so the user's `.cargo/config.toml` still applies.
    pub cargo_cwd: PathBuf,
}

/// Run cargo (`$CARGO` when invoked as an applet) in `cwd`, returning stdout.
pub(crate) fn run_cargo(cwd: &Path, args: &[&OsStr]) -> Result<Vec<u8>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .args(args)
        .current_dir(cwd)
        .output()
        .context("running cargo")?;
    if !out.status.success() {
        let sub = args
            .first()
            .map(|a| a.to_string_lossy())
            .unwrap_or_default();
        bail!(
            "cargo {sub} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out.stdout)
}

/// `cargo metadata`, which also writes (or trims) the workspace `Cargo.lock`.
fn cargo_metadata(manifest: &Path, cwd: &Path) -> Result<Value> {
    let args = ["metadata", "--format-version", "1", "--manifest-path"].map(OsStr::new);
    let out = run_cargo(cwd, &[&args[..], &[manifest.as_os_str()]].concat())?;
    serde_json::from_slice(&out).context("parsing cargo metadata")
}

fn manifest_dir(pkg: &Value) -> Option<&Path> {
    Path::new(pkg["manifest_path"].as_str()?).parent()
}

/// Directories (relative to the workspace root) of the members `target_id` needs,
/// or `None` when that is every member.
fn member_closure(meta: &Value, target_id: &str, ws_root: &Path) -> Result<Option<Vec<PathBuf>>> {
    let members: HashSet<&str> = meta["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let nodes: HashMap<&str, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();
    let mut needed = HashSet::new();
    let mut stack = vec![target_id];
    while let Some(id) = stack.pop() {
        if needed.insert(id) {
            let deps = nodes.get(id).and_then(|n| n["deps"].as_array());
            stack.extend(deps.into_iter().flatten().filter_map(|d| d["pkg"].as_str()));
        }
    }
    let kept: HashSet<&str> = members.intersection(&needed).copied().collect();
    if kept.len() == members.len() {
        return Ok(None);
    }
    let mut dirs = Vec::new();
    for pkg in meta["packages"].as_array().into_iter().flatten() {
        if !pkg["id"].as_str().is_some_and(|id| kept.contains(id)) {
            continue;
        }
        let dir = manifest_dir(pkg).context("package without manifest_path")?;
        let rel = dir.strip_prefix(ws_root).with_context(|| {
            format!("member {} is outside {}", dir.display(), ws_root.display())
        })?;
        dirs.push(rel.to_path_buf());
    }
    dirs.sort();
    Ok(Some(dirs))
}

/// A scratch workspace of `members`, symlinked at their original relative paths,
/// with the root manifest and lock of `ws_root`.
fn isolate_workspace(ws_root: &Path, members: &[PathBuf]) -> Result<tempfile::TempDir> {
    let scratch = tempfile::TempDir::new()?;
    for rel in members {
        let dest = scratch.path().join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(ws_root.join(rel), &dest)
            .with_context(|| format!("linking {}", dest.display()))?;
    }
    let root_manifest = ws_root.join("Cargo.toml");
    let orig = std::fs::read_to_string(&root_manifest)
        .with_context(|| format!("reading {}", root_manifest.display()))?;
    let mut doc: toml::Table = toml::from_str(&orig)?;
    if let Some(ws) = doc.get_mut("workspace").and_then(|v| v.as_table_mut()) {
        let names = members
            .iter()
            .map(|p| toml::Value::String(p.to_string_lossy().into_owned()));
        ws.insert("members".into(), toml::Value::Array(names.collect()));
        ws.remove("default-members");
        ws.remove("exclude");
    }
    // Relative `[patch]` paths would otherwise resolve against the scratch dir.
    let patches = doc.get_mut("patch").and_then(|p| p.as_table_mut());
    for (_, source) in patches.into_iter().flat_map(|p| p.iter_mut()) {
        for (_, dep) in source.as_table_mut().into_iter().flat_map(|s| s.iter_mut()) {
            if let Some(toml::Value::String(path)) = dep.get_mut("path") {
                *path = ws_root.join(path.as_str()).to_string_lossy().into_owned();
            }
        }
    }
    std::fs::write(scratch.path().join("Cargo.toml"), toml::to_string(&doc)?)?;
    let lock = ws_root.join("Cargo.lock");
    if lock.is_file() {
        std::fs::copy(&lock, scratch.path().join("Cargo.lock"))?;
    }
    Ok(scratch)
}

/// Drop the `[patch]` entries the lock lists as unused: `cargo vendor` skips them, but
/// an offline build still fails trying to load their source. Returns whether any went.
fn prune_unused_patches(root: &Path) -> Result<bool> {
    let lock = Lockfile::load(root.join("Cargo.lock"))?;
    if lock.patch.unused.is_empty() {
        return Ok(false);
    }
    let manifest = root.join("Cargo.toml");
    let mut doc: toml::Table = toml::from_str(&std::fs::read_to_string(&manifest)?)?;
    if let Some(patches) = doc.get_mut("patch").and_then(|p| p.as_table_mut()) {
        for (_, source) in patches.iter_mut() {
            if let Some(source) = source.as_table_mut() {
                for dep in &lock.patch.unused {
                    source.remove(dep.name.as_str());
                }
            }
        }
        patches.retain(|_, s| s.as_table().is_none_or(|t| !t.is_empty()));
        if patches.is_empty() {
            doc.remove("patch");
        }
    }
    std::fs::write(&manifest, toml::to_string(&doc)?)?;
    Ok(true)
}

/// Resolve the package in `dir`, isolating a workspace member from siblings it does
/// not depend on so the vendor tarball and snapshot only carry what it needs.
pub fn prepare_package(dir: &Path) -> Result<PreparedPackage> {
    let dir = dir
        .canonicalize()
        .with_context(|| format!("resolving {}", dir.display()))?;
    let manifest = dir.join("Cargo.toml");
    if !manifest.is_file() {
        bail!("no Cargo.toml under {}", dir.display());
    }
    let meta = cargo_metadata(&manifest, &dir)?;
    let ws_root = PathBuf::from(
        meta["workspace_root"]
            .as_str()
            .context("metadata without workspace_root")?,
    );
    let pkg = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| manifest_dir(p) == Some(dir.as_path()))
        .with_context(|| format!("{} is not a package", manifest.display()))?;
    let package = PackageMetadata::from_metadata(pkg)?;
    let target_id = pkg["id"].as_str().context("package without id")?;

    let Some(members) = member_closure(&meta, target_id, &ws_root)? else {
        return Ok(PreparedPackage {
            _scratch: None,
            package,
            manifest,
            lock: ws_root.join("Cargo.lock"),
            source_root: dir.clone(),
            member_dir: None,
            cargo_cwd: dir,
        });
    };
    let scratch = isolate_workspace(&ws_root, &members)?;
    let member_dir = dir.strip_prefix(&ws_root)?.to_path_buf();
    let manifest = scratch.path().join(&member_dir).join("Cargo.toml");
    // Trims the copied lock to the kept members instead of re-resolving.
    cargo_metadata(&manifest, &ws_root)?;
    if prune_unused_patches(scratch.path())? {
        cargo_metadata(&manifest, &ws_root)?;
    }
    Ok(PreparedPackage {
        package,
        manifest,
        lock: scratch.path().join("Cargo.lock"),
        source_root: scratch.path().to_path_buf(),
        member_dir: Some(member_dir),
        cargo_cwd: ws_root,
        _scratch: Some(scratch),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, s: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, s).unwrap();
    }

    /// Nested members, `a` inheriting from `[workspace.package]`; no external deps.
    fn workspace() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        write(
            &r.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/a\", \"crates/b\", \"c\"]\nresolver = \"2\"\n\
             [workspace.package]\nversion = \"1.2.3-rc.1\"\nlicense = \"MIT OR Apache-2.0\"\n\
             description = \"inherited\"\n[patch.crates-io]\nc = { path = \"c\" }\n",
        );
        write(
            &r.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion.workspace = true\nlicense.workspace = true\n\
             description.workspace = true\nedition = \"2021\"\n\
             [dependencies]\nb = { path = \"../b\" }\n\
             [features]\ndefault = [\"serde\", \"b/x\"]\nserde = []\njson = []\n",
        );
        write(
            &r.join("crates/b/Cargo.toml"),
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[features]\nx = []\n",
        );
        write(
            &r.join("c/Cargo.toml"),
            "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        for m in ["crates/a", "crates/b", "c"] {
            write(&r.join(m).join("src/lib.rs"), "");
        }
        tmp
    }

    #[test]
    fn member_metadata_resolves_workspace_inheritance() {
        let ws = workspace();
        let p = prepare_package(&ws.path().join("crates/a")).unwrap();
        assert_eq!(p.package.version, "1.2.3-rc.1");
        assert_eq!(p.package.license.as_deref(), Some("MIT OR Apache-2.0"));
        assert_eq!(p.package.description.as_deref(), Some("inherited"));
        assert_eq!(p.package.gentoo_p().unwrap(), "a-1.2.3_rc1");
        let f = &p.package.features;
        assert_eq!(
            f.iuse,
            BTreeMap::from([("json".into(), false), ("serde".into(), true)])
        );
        assert_eq!(f.always, ["b/x"]);
        assert!(f.no_default);
    }

    #[test]
    fn isolation_keeps_nested_paths_and_drops_unneeded_members() {
        let ws = workspace();
        let p = prepare_package(&ws.path().join("crates/a")).unwrap();
        assert_eq!(p.member_dir.as_deref(), Some(Path::new("crates/a")));
        let lock = std::fs::read_to_string(&p.lock).unwrap();
        assert!(lock.contains("name = \"b\""), "{lock}");
        assert!(!lock.contains("name = \"c\""), "{lock}");
        let root = std::fs::read_to_string(p.source_root.join("Cargo.toml")).unwrap();
        assert!(!root.contains("path"), "unused patch kept: {root}");
    }

    #[test]
    fn default_dep_entry_keeps_default_features() {
        let features = serde_json::json!({"default": ["dep:x", "a"], "a": [], "x": ["dep:x"]});
        let f = Features::from_metadata(features.as_object().unwrap());
        assert!(!f.no_default);
        assert_eq!(f.iuse.get("a"), Some(&true));
    }

    #[test]
    fn gentoo_version_maps_prereleases() {
        for (semver, pms) in [
            ("1.0.0", "1.0.0"),
            ("1.0.0-alpha.1", "1.0.0_alpha1"),
            ("1.0.0-beta2", "1.0.0_beta2"),
            ("1.0.0-rc.3", "1.0.0_rc3"),
            ("0.1.0-dev", "0.1.0_pre"),
            ("2.0.0-pre.4+build.7", "2.0.0_pre4"),
        ] {
            assert_eq!(gentoo_version(semver).unwrap(), pms, "{semver}");
        }
    }

    #[test]
    fn use_flag_names() {
        assert!(is_use_flag("tls-rustls"));
        assert!(!is_use_flag("_private"));
        assert!(!is_use_flag("v1.2"));
    }

    #[test]
    fn unpublished_is_not_a_release() {
        let mut p = PackageMetadata {
            name: "t".into(),
            version: "0.1.0".into(),
            license: None,
            description: None,
            homepage: None,
            features: Features::default(),
            publish: false,
        };
        assert!(!p.is_release());
        p.publish = true;
        assert!(p.is_release());
        p.version = "0.1.0-dev".into();
        assert!(!p.is_release());
    }
}
