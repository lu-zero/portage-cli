use std::collections::{HashMap, HashSet};

use blake2::{Blake2b512, Digest};
use portage_metadata::SrcUriEntry;
use portage_repo::Repository;

use crate::error::{Error, Result};

/// Candidate URLs for a distfile on a Gentoo mirror, in priority order: the
/// modern content-mirror **filename-hash** layout first, the legacy flat layout
/// as a fallback for non-conforming mirrors.
///
/// `distfiles.gentoo.org`'s `layout.conf` is `filename-hash BLAKE2B 8`: the file
/// lives under `distfiles/<xx>/<filename>`, where `<xx>` is the first 8 bits (two
/// hex chars) of `BLAKE2B-512(filename)` — the hash of the *filename string*, not
/// the file content (GLEP 75; matches portage's `FilenameHashLayout`). The old
/// flat `distfiles/<filename>` path now 404s on the official mirrors.
fn gentoo_distfile_urls(mirror: &str, filename: &str) -> Vec<String> {
    let mirror = mirror.trim_end_matches('/');
    let sub = format!("{:02x}", Blake2b512::digest(filename.as_bytes())[0]);
    vec![
        format!("{mirror}/distfiles/{sub}/{filename}"),
        format!("{mirror}/distfiles/{filename}"),
    ]
}

/// A fully resolved distfile: local filename + all candidate download URLs.
///
/// URLs are in priority order — try each in turn until one succeeds.
#[derive(Debug, Clone)]
pub struct Distfile {
    /// The local filename to store in DISTDIR.
    pub filename: String,
    /// Download URLs in priority order — GENTOO_MIRRORS first (mirrors-before-
    /// upstream, matching portage), then the expanded `mirror://`/upstream URLs.
    pub urls: Vec<String>,
    /// EAPI 8+ restriction: `Some("fetch")` or `Some("mirror")`.
    pub restriction: Option<String>,
}

/// Resolves `SRC_URI` entries into concrete [`Distfile`]s.
///
/// Expands `mirror://` URIs using the repository's `thirdpartymirrors` data
/// and appends GENTOO_MIRRORS as a fallback for every distfile.
pub struct DistfileResolver {
    /// Parsed `profiles/thirdpartymirrors`: mirror name → list of base URLs.
    thirdparty: HashMap<String, Vec<String>>,
    /// GENTOO_MIRRORS — appended as final fallback for every distfile.
    gentoo_mirrors: Vec<String>,
}

impl DistfileResolver {
    /// Build a resolver from explicit data (useful for testing).
    pub fn new(thirdparty: Vec<(String, Vec<String>)>, gentoo_mirrors: Vec<String>) -> Self {
        Self {
            thirdparty: thirdparty.into_iter().collect(),
            gentoo_mirrors,
        }
    }

    /// Build a resolver from a live repository + a GENTOO_MIRRORS list.
    ///
    /// `gentoo_mirrors` should come from the `GENTOO_MIRRORS` environment
    /// variable or `make.conf`, split on whitespace.
    pub fn from_repo(repo: &Repository, gentoo_mirrors: Vec<String>) -> Result<Self> {
        let thirdparty = repo
            .thirdpartymirrors()
            .map_err(|e| Error::Manifest(e.to_string()))?;
        Ok(Self::new(thirdparty, gentoo_mirrors))
    }

    /// Resolve `SRC_URI` entries into distfiles given the active USE flags.
    ///
    /// USE-conditional groups are evaluated; `mirror://` URIs are expanded.
    /// Resolve `SRC_URI` entries into distfiles given the active USE flags.
    ///
    /// USE-conditional groups are evaluated; `mirror://` URIs are expanded.
    /// GENTOO_MIRRORS are appended as a fallback for every distfile that is
    /// not `mirror`-restricted.
    pub fn resolve(&self, entries: &[SrcUriEntry], use_flags: &HashSet<String>) -> Vec<Distfile> {
        let mut raw: Vec<(String, String, Option<String>)> = Vec::new();
        collect_uri_pairs(entries, use_flags, &mut raw);
        self.build_distfiles(raw)
    }

    /// Resolve every `SRC_URI` entry regardless of USE conditionals —
    /// `-F`/`--fetch-all-uri`: fetch everything an ebuild could ever need,
    /// not just what the current USE selection asks for.
    pub fn resolve_all(&self, entries: &[SrcUriEntry]) -> Vec<Distfile> {
        let mut raw: Vec<(String, String, Option<String>)> = Vec::new();
        collect_uri_pairs_all(entries, &mut raw);
        self.build_distfiles(raw)
    }

    fn build_distfiles(&self, raw: Vec<(String, String, Option<String>)>) -> Vec<Distfile> {
        raw.into_iter()
            .map(|(url, filename, restriction)| {
                // Portage tries GENTOO_MIRRORS *before* the upstream SRC_URI URLs
                // (make.conf(5): "These locations are used to download files before
                // the ones listed in the ebuild scripts"). GENTOO_MIRRORS are
                // skipped for mirror-restricted files and for `mirror://gentoo/`
                // (which `expand_url` already routed through the gentoo mirrors).
                let mut urls = Vec::new();
                let use_gentoo_mirrors = restriction.as_deref() != Some("mirror")
                    && !url.starts_with("mirror://gentoo/");
                if use_gentoo_mirrors {
                    for mirror in &self.gentoo_mirrors {
                        for candidate in gentoo_distfile_urls(mirror, &filename) {
                            if !urls.contains(&candidate) {
                                urls.push(candidate);
                            }
                        }
                    }
                }
                for candidate in self.expand_url(&url, &filename) {
                    if !urls.contains(&candidate) {
                        urls.push(candidate);
                    }
                }
                Distfile {
                    filename,
                    urls,
                    restriction,
                }
            })
            .collect()
    }

    /// Expand a single URL to one or more concrete download URLs.
    ///
    /// `mirror://gentoo/path` uses GENTOO_MIRRORS with the full path suffix.
    /// `mirror://name/path` (other names) is expanded via `thirdpartymirrors`.
    /// Direct URLs are returned as-is; GENTOO_MIRRORS fallback is added by
    /// the caller (`resolve`) so it can be gated on the restriction flag.
    fn expand_url(&self, url: &str, filename: &str) -> Vec<String> {
        if let Some(rest) = url.strip_prefix("mirror://") {
            let (mirror_name, path) = rest.split_once('/').unwrap_or((rest, filename));
            if mirror_name == "gentoo" {
                // mirror://gentoo/[subdir/]file → GENTOO_MIRRORS in filename-hash
                // layout (hashed-first, flat fallback). The content-mirror layout
                // keys on the final filename component, not any historical subdir.
                let fname = path.rsplit('/').next().unwrap_or(path);
                return self
                    .gentoo_mirrors
                    .iter()
                    .flat_map(|m| gentoo_distfile_urls(m, fname))
                    .collect();
            }
            if let Some(bases) = self.thirdparty.get(mirror_name) {
                return bases
                    .iter()
                    .map(|base| format!("{}/{path}", base.trim_end_matches('/')))
                    .collect();
            }
            // Unknown mirror name — no direct URLs; caller will add GENTOO_MIRRORS
            // as a last-resort fallback (unless mirror-restricted).
            vec![]
        } else {
            vec![url.to_owned()]
        }
    }
}

/// Walk `SRC_URI` entries collecting `(url, filename, restriction)` tuples.
///
/// USE-conditional groups are evaluated against `use_flags`.
/// This is the public equivalent of the private `collect_src_filenames`
/// in `portage-repo`.
pub fn collect_filenames(entries: &[SrcUriEntry], use_flags: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    collect_filenames_inner(entries, use_flags, &mut out);
    out
}

fn collect_filenames_inner(
    entries: &[SrcUriEntry],
    use_flags: &HashSet<String>,
    out: &mut Vec<String>,
) {
    for entry in entries {
        match entry {
            SrcUriEntry::Uri { filename, .. } => out.push(filename.clone()),
            SrcUriEntry::Renamed { target, .. } => out.push(target.clone()),
            SrcUriEntry::UseConditional {
                flag,
                negated,
                entries,
            } => {
                let active = use_flags.contains(flag.as_str());
                if active != *negated {
                    collect_filenames_inner(entries, use_flags, out);
                }
            }
            SrcUriEntry::Group(entries) => {
                collect_filenames_inner(entries, use_flags, out);
            }
        }
    }
}

fn collect_uri_pairs(
    entries: &[SrcUriEntry],
    use_flags: &HashSet<String>,
    out: &mut Vec<(String, String, Option<String>)>,
) {
    for entry in entries {
        match entry {
            SrcUriEntry::Uri {
                url,
                filename,
                restriction,
            } => {
                out.push((url.clone(), filename.clone(), restriction.clone()));
            }
            SrcUriEntry::Renamed {
                url,
                target,
                restriction,
            } => {
                out.push((url.clone(), target.clone(), restriction.clone()));
            }
            SrcUriEntry::UseConditional {
                flag,
                negated,
                entries,
            } => {
                let active = use_flags.contains(flag.as_str());
                if active != *negated {
                    collect_uri_pairs(entries, use_flags, out);
                }
            }
            SrcUriEntry::Group(entries) => {
                collect_uri_pairs(entries, use_flags, out);
            }
        }
    }
}

/// Same as [`collect_uri_pairs`], but descends into every `UseConditional`
/// branch unconditionally instead of gating on `use_flags` — the union of
/// what every USE setting could ever request.
fn collect_uri_pairs_all(entries: &[SrcUriEntry], out: &mut Vec<(String, String, Option<String>)>) {
    for entry in entries {
        match entry {
            SrcUriEntry::Uri {
                url,
                filename,
                restriction,
            } => {
                out.push((url.clone(), filename.clone(), restriction.clone()));
            }
            SrcUriEntry::Renamed {
                url,
                target,
                restriction,
            } => {
                out.push((url.clone(), target.clone(), restriction.clone()));
            }
            SrcUriEntry::UseConditional { entries, .. } | SrcUriEntry::Group(entries) => {
                collect_uri_pairs_all(entries, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(gentoo_mirrors: &[&str]) -> DistfileResolver {
        DistfileResolver::new(
            vec![(
                "kde".to_owned(),
                vec!["https://mirrors.kde.org/".to_owned()],
            )],
            gentoo_mirrors.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn gentoo_mirrors_tried_before_upstream() {
        // make.conf(5): GENTOO_MIRRORS "are used to download files before the
        // ones listed in the ebuild scripts" — mirrors-first, upstream last.
        let r = resolver(&["https://mirror.gentoo.org"]);
        let entries = SrcUriEntry::parse("https://example.com/foo-1.0.tar.gz").unwrap();
        let dfs = r.resolve(&entries, &HashSet::new());
        let mut expected = gentoo_distfile_urls("https://mirror.gentoo.org", "foo-1.0.tar.gz");
        expected.push("https://example.com/foo-1.0.tar.gz".to_owned());
        assert_eq!(dfs[0].urls, expected);
    }

    #[test]
    fn mirror_gentoo_uses_filename_hash_layout() {
        let r = resolver(&["https://mirror.gentoo.org"]);
        let entries = SrcUriEntry::parse("mirror://gentoo/subdir/foo-1.0.tar.gz").unwrap();
        let dfs = r.resolve(&entries, &HashSet::new());
        // Keyed on the filename component, hashed-first then flat — the historical
        // subdir is dropped (content-mirror layout ignores it).
        assert_eq!(
            dfs[0].urls,
            gentoo_distfile_urls("https://mirror.gentoo.org", "foo-1.0.tar.gz")
        );
    }

    #[test]
    fn gentoo_filename_hash_subdir_matches_portage() {
        // portage's FilenameHashLayout("BLAKE2B", "8"): first 2 hex of
        // BLAKE2B-512(filename) as the subdir.
        let urls = gentoo_distfile_urls("https://m", "psmisc-23.7.tar.xz");
        let sub = format!(
            "{:02x}",
            Blake2b512::digest("psmisc-23.7.tar.xz".as_bytes())[0]
        );
        assert_eq!(
            urls[0],
            format!("https://m/distfiles/{sub}/psmisc-23.7.tar.xz")
        );
        assert_eq!(urls[1], "https://m/distfiles/psmisc-23.7.tar.xz");
    }

    #[test]
    fn mirror_restriction_suppresses_gentoo_fallback() {
        let r = resolver(&["https://mirror.gentoo.org"]);
        let entries = vec![SrcUriEntry::Renamed {
            url: "https://proprietary.example.com/secret.tar.gz".to_owned(),
            target: "secret.tar.gz".to_owned(),
            restriction: Some("mirror".to_owned()),
        }];
        let dfs = r.resolve(&entries, &HashSet::new());
        assert_eq!(
            dfs[0].urls,
            ["https://proprietary.example.com/secret.tar.gz"]
        );
        assert_eq!(dfs[0].urls.len(), 1);
    }

    #[test]
    fn thirdparty_mirror_expansion() {
        let r = resolver(&[]);
        let entries = SrcUriEntry::parse("mirror://kde/stable/frameworks/foo.tar.xz").unwrap();
        let dfs = r.resolve(&entries, &HashSet::new());
        assert_eq!(
            dfs[0].urls,
            ["https://mirrors.kde.org/stable/frameworks/foo.tar.xz"]
        );
    }

    #[test]
    fn resolve_skips_a_use_conditional_uri_when_the_flag_is_off() {
        let r = resolver(&[]);
        let entries = SrcUriEntry::parse(
            "https://example.com/base.tar.gz doc? ( https://example.com/doc.tar.gz )",
        )
        .unwrap();
        let dfs = r.resolve(&entries, &HashSet::new());
        let names: Vec<&str> = dfs.iter().map(|d| d.filename.as_str()).collect();
        assert_eq!(names, ["base.tar.gz"]);
    }

    #[test]
    fn resolve_all_ignores_use_conditionals_entirely() {
        // -F/--fetch-all-uri: every SRC_URI entry, regardless of USE — neither
        // an active nor an inactive flag gates it, unlike `resolve`.
        let r = resolver(&[]);
        let entries = SrcUriEntry::parse(
            "https://example.com/base.tar.gz doc? ( https://example.com/doc.tar.gz ) !static? ( https://example.com/dyn.tar.gz )",
        )
        .unwrap();
        let dfs = r.resolve_all(&entries);
        let mut names: Vec<&str> = dfs.iter().map(|d| d.filename.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["base.tar.gz", "doc.tar.gz", "dyn.tar.gz"]);
    }
}
