use std::collections::HashMap;
use std::path::Path;

use super::util;
use crate::error::{Error, Result};

/// Parsed representation of `metadata/layout.conf`
///
/// This file describes repository-level metadata such as master repositories,
/// cache format, and profile format preferences.
///
/// See [Repository format — layout.conf](https://wiki.gentoo.org/wiki/Repository_format/metadata/layout.conf)
/// and [PMS 4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
#[derive(Debug, Clone)]
pub struct LayoutConf {
    /// Master repositories this repository depends on (space-separated in file)
    pub masters: Vec<String>,
    /// Whether thin manifests are used
    pub thin_manifests: bool,
    /// Whether manifests are signed
    pub sign_manifests: bool,
    /// Cache formats used (e.g. `md5-dict`)
    pub cache_formats: Vec<String>,
    /// Profile formats (e.g. `portage-2`)
    pub profile_formats: Vec<String>,
    /// Banned EAPIs
    pub eapis_banned: Vec<String>,
    /// Deprecated EAPIs
    pub eapis_deprecated: Vec<String>,
    /// Repository aliases
    pub aliases: Vec<String>,
    /// All raw key-value pairs from the file
    pub properties: HashMap<String, String>,
}

impl LayoutConf {
    /// Parse a `layout.conf` from its text content
    pub fn parse(input: &str) -> Result<Self> {
        let mut properties = HashMap::new();

        for line in input.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                properties.insert(key.trim().to_string(), value.trim().to_string());
            }
        }

        let split_ws = |key: &str| -> Vec<String> {
            properties
                .get(key)
                .map(|v| v.split_whitespace().map(String::from).collect())
                .unwrap_or_default()
        };

        let get_bool = |key: &str, default: bool| -> bool {
            properties.get(key).map(|v| v == "true").unwrap_or(default)
        };

        Ok(LayoutConf {
            masters: split_ws("masters"),
            thin_manifests: get_bool("thin-manifests", false),
            sign_manifests: get_bool("sign-manifests", true),
            cache_formats: split_ws("cache-formats"),
            profile_formats: split_ws("profile-formats"),
            eapis_banned: split_ws("eapis-banned"),
            eapis_deprecated: split_ws("eapis-deprecated"),
            aliases: split_ws("aliases"),
            properties,
        })
    }

    /// Read and parse `metadata/layout.conf` from a repository root
    pub fn from_repo(repo_root: &Path) -> Result<Self> {
        let path = repo_root.join("metadata").join("layout.conf");
        match std::fs::read_to_string(&path) {
            Ok(contents) => Self::parse(&contents),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::InvalidLayout(
                "metadata/layout.conf not found".into(),
            )),
            Err(e) => Err(util::io_err(&path, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_layout() {
        let input = r#"
# Example layout.conf
masters = gentoo
thin-manifests = true
sign-manifests = false
cache-formats = md5-dict
profile-formats = portage-2
"#;
        let layout = LayoutConf::parse(input).unwrap();
        assert_eq!(layout.masters, vec!["gentoo"]);
        assert!(layout.thin_manifests);
        assert!(!layout.sign_manifests);
        assert_eq!(layout.cache_formats, vec!["md5-dict"]);
        assert_eq!(layout.profile_formats, vec!["portage-2"]);
    }

    #[test]
    fn parse_multiple_masters() {
        let input = "masters = gentoo foo bar\n";
        let layout = LayoutConf::parse(input).unwrap();
        assert_eq!(layout.masters, vec!["gentoo", "foo", "bar"]);
    }

    #[test]
    fn parse_empty_masters() {
        let input = "masters =\n";
        let layout = LayoutConf::parse(input).unwrap();
        assert!(layout.masters.is_empty());
    }

    #[test]
    fn parse_defaults() {
        let input = "";
        let layout = LayoutConf::parse(input).unwrap();
        assert!(layout.masters.is_empty());
        assert!(!layout.thin_manifests);
        assert!(layout.sign_manifests);
        assert!(layout.cache_formats.is_empty());
    }

    #[test]
    fn parse_with_comments_and_blanks() {
        let input = "# comment\n\nmasters = gentoo\n# another\naliases = overlay\n";
        let layout = LayoutConf::parse(input).unwrap();
        assert_eq!(layout.masters, vec!["gentoo"]);
        assert_eq!(layout.aliases, vec!["overlay"]);
    }

    #[test]
    fn parse_eapis() {
        let input = "eapis-banned = 0 1 2 3\neapis-deprecated = 4 5\n";
        let layout = LayoutConf::parse(input).unwrap();
        assert_eq!(layout.eapis_banned, vec!["0", "1", "2", "3"]);
        assert_eq!(layout.eapis_deprecated, vec!["4", "5"]);
    }

    #[test]
    fn raw_properties_preserved() {
        let input = "masters = gentoo\ncustom-key = custom-value\n";
        let layout = LayoutConf::parse(input).unwrap();
        assert_eq!(layout.properties.get("custom-key").unwrap(), "custom-value");
    }
}
