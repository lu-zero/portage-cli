use std::collections::BTreeMap;

use itertools::Itertools as _;

use crate::error::{Error, Result};

/// Type of a package maintainer
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaintainerKind {
    /// Individual maintainer (`<maintainer type="person">`)
    Person,
    /// Project or herd maintainer (`<maintainer type="project">`)
    Project,
    /// Maintainer type missing or not recognised in `metadata.xml`
    Unknown,
}

/// A single `<maintainer>` entry from `metadata.xml`
#[derive(Debug, Clone)]
pub struct Maintainer {
    /// Contact e-mail address
    pub email: String,
    /// Display name, if present
    pub name: Option<String>,
    /// Whether this is a person or project maintainer
    pub kind: MaintainerKind,
}

impl Maintainer {
    /// Return a compact `"Name <email>"` or `"<email>"` string
    pub fn display(&self) -> String {
        match &self.name {
            Some(n) => format!("{n} <{}>", self.email),
            None => format!("<{}>", self.email),
        }
    }
}

/// Package-level metadata from `metadata.xml`
///
/// Contains maintainer contacts, an optional long description, and per-flag
/// USE descriptions extracted from the `<use>` block.
///
/// The format is defined by the Gentoo
/// [metadata DTD](https://www.gentoo.org/dtd/metadata.dtd)
/// and [GLEP 68](https://www.gentoo.org/glep/glep-0068.html).
///
/// See also [PMS Appendix A](https://projects.gentoo.org/pms/9/pms.html#metadata-xml).
#[derive(Debug, Clone, Default)]
pub struct PkgMetadata {
    /// Maintainers, in document order
    pub maintainers: Vec<Maintainer>,

    /// Long description (`<longdescription lang="en">`), if present
    ///
    /// Whitespace is normalised to single spaces.
    pub longdescription: Option<String>,

    /// USE flag descriptions, keyed by flag name, sorted alphabetically
    ///
    /// Inner XML elements (e.g. `<pkg>`, `<b>`) are flattened to plain text.
    use_flags: BTreeMap<String, String>,
}

impl PkgMetadata {
    /// Parse the contents of a `metadata.xml` file
    pub fn parse(xml: &str) -> Result<Self> {
        let opts = roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        };
        let doc = roxmltree::Document::parse_with_options(xml, opts)
            .map_err(|e| Error::InvalidMetadataXml(e.to_string()))?;
        let root = doc.root_element();

        let mut maintainers = Vec::new();
        let mut longdescription = None;
        let mut use_flags = BTreeMap::new();

        for child in root.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "maintainer" => {
                    let kind = match child.attribute("type") {
                        Some("person") => MaintainerKind::Person,
                        Some("project") => MaintainerKind::Project,
                        _ => MaintainerKind::Unknown,
                    };
                    let email = child
                        .children()
                        .find(|n| n.has_tag_name("email"))
                        .map(collect_text)
                        .unwrap_or_default();
                    let name = child
                        .children()
                        .find(|n| n.has_tag_name("name"))
                        .map(collect_text)
                        .filter(|s| !s.is_empty());
                    if !email.is_empty() {
                        maintainers.push(Maintainer { email, name, kind });
                    }
                }
                "longdescription" => {
                    // Prefer the English version; fall back to the first one found.
                    let lang = child.attribute("lang").unwrap_or("en");
                    if lang == "en" || longdescription.is_none() {
                        let text = collect_text(child);
                        if !text.is_empty() {
                            longdescription = Some(text);
                        }
                    }
                }
                "use" => {
                    for flag_node in child.children().filter(|n| n.has_tag_name("flag")) {
                        if let Some(name) = flag_node.attribute("name") {
                            use_flags.insert(name.to_string(), collect_text(flag_node));
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(PkgMetadata {
            maintainers,
            longdescription,
            use_flags,
        })
    }

    /// USE flag descriptions keyed by flag name, in alphabetical order
    ///
    /// Inner XML elements (e.g. `<pkg>`, `<b>`) are flattened to plain text.
    pub fn use_flags(&self) -> &BTreeMap<String, String> {
        &self.use_flags
    }

    /// Consume `self` and return the USE flag map
    pub fn into_use_flags(self) -> BTreeMap<String, String> {
        self.use_flags
    }
}

/// Recursively collect all text content from an XML node, stripping element tags
///
/// Runs of whitespace (spaces, tabs, newlines) are collapsed to a single space
/// so that multi-line descriptions come out as a clean single line.
fn collect_text(node: roxmltree::Node<'_, '_>) -> String {
    let mut buf = String::new();
    for child in node.children() {
        if child.is_text() {
            if let Some(text) = child.text() {
                buf.push_str(text);
            }
        } else if child.is_element() {
            buf.push_str(&collect_text(child));
        }
    }
    buf.split_whitespace().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_use_flags() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE pkgmetadata SYSTEM "https://www.gentoo.org/dtd/metadata.dtd">
<pkgmetadata>
  <use>
    <flag name="foo">Enable foo support</flag>
    <flag name="bar">Enable bar via <pkg>dev-libs/bar</pkg></flag>
  </use>
</pkgmetadata>"#;
        let meta = PkgMetadata::parse(xml).unwrap();
        assert_eq!(meta.use_flags["foo"], "Enable foo support");
        assert_eq!(meta.use_flags["bar"], "Enable bar via dev-libs/bar");
    }

    #[test]
    fn parse_maintainers() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE pkgmetadata SYSTEM "https://www.gentoo.org/dtd/metadata.dtd">
<pkgmetadata>
  <maintainer type="person">
    <email>alice@gentoo.org</email>
    <name>Alice</name>
  </maintainer>
  <maintainer type="project">
    <email>tools@gentoo.org</email>
  </maintainer>
</pkgmetadata>"#;
        let meta = PkgMetadata::parse(xml).unwrap();
        assert_eq!(meta.maintainers.len(), 2);
        assert_eq!(meta.maintainers[0].email, "alice@gentoo.org");
        assert_eq!(meta.maintainers[0].name.as_deref(), Some("Alice"));
        assert_eq!(meta.maintainers[0].kind, MaintainerKind::Person);
        assert_eq!(meta.maintainers[1].kind, MaintainerKind::Project);
        assert!(meta.maintainers[1].name.is_none());
        assert_eq!(meta.maintainers[0].display(), "Alice <alice@gentoo.org>");
        assert_eq!(meta.maintainers[1].display(), "<tools@gentoo.org>");
    }

    #[test]
    fn parse_longdescription() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE pkgmetadata SYSTEM "https://www.gentoo.org/dtd/metadata.dtd">
<pkgmetadata>
  <longdescription>
    A very long description that spans
    multiple lines and has extra   whitespace.
  </longdescription>
</pkgmetadata>"#;
        let meta = PkgMetadata::parse(xml).unwrap();
        let desc = meta.longdescription.unwrap();
        assert!(desc.contains("multiple lines"));
        assert!(!desc.contains('\n'));
    }

    #[test]
    fn parse_no_use_block() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE pkgmetadata SYSTEM "https://www.gentoo.org/dtd/metadata.dtd">
<pkgmetadata>
  <maintainer type="person">
    <email>user@example.com</email>
  </maintainer>
</pkgmetadata>"#;
        let meta = PkgMetadata::parse(xml).unwrap();
        assert!(meta.use_flags.is_empty());
        assert_eq!(meta.maintainers.len(), 1);
    }

    #[test]
    fn parse_multiline_description() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE pkgmetadata SYSTEM "https://www.gentoo.org/dtd/metadata.dtd">
<pkgmetadata>
  <use>
    <flag name="dbus">
      Enable dependencies required by glib libraries
      using dbus service to manage settings saving
    </flag>
  </use>
</pkgmetadata>"#;
        let meta = PkgMetadata::parse(xml).unwrap();
        let desc = &meta.use_flags["dbus"];
        assert!(desc.contains("dbus service"));
        assert!(!desc.starts_with('\n'));
    }

    #[test]
    fn parse_invalid_xml_returns_error() {
        let result = PkgMetadata::parse("<not valid xml");
        assert!(matches!(
            result,
            Err(crate::error::Error::InvalidMetadataXml(_))
        ));
    }
}
