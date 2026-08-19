//! Gentoo distfile mirror list and selection.
//!
//! Fetches and parses the official machine-readable mirror list at
//! <https://api.gentoo.org/mirrors/distfiles.xml> — the same source
//! `mirrorselect` uses — rather than scraping the human-facing HTML page.
//!
//! Mirrors are modelled the same way `mirrorselect` does (`mirrorset.py`): a
//! mirror is one site with several protocol endpoints (http/https/ftp/rsync),
//! each reachable over IPv4 and/or IPv6. For distfile fetching only HTTP/HTTPS
//! endpoints are relevant; [`MirrorList::preferred_urls`] collapses each mirror
//! to a single endpoint (HTTPS preferred) so a site never appears twice.

use std::time::Duration;

use crate::{Error, Result};

/// URL of Gentoo's structured mirror list (machine-readable XML).
const DISTFILES_MIRRORS_XML: &str = "https://api.gentoo.org/mirrors/distfiles.xml";

/// A single protocol endpoint of a [`Mirror`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The endpoint URI (e.g. `https://gentoo.osuosl.org/`).
    pub uri: String,
    /// The protocol: `https`, `http`, `ftp`, or `rsync`.
    pub protocol: String,
    /// Whether the endpoint is reachable over IPv4.
    pub ipv4: bool,
    /// Whether the endpoint is reachable over IPv6.
    pub ipv6: bool,
}

impl Endpoint {
    /// Whether this endpoint is HTTP or HTTPS — the protocols reqwest can fetch.
    pub fn is_http(&self) -> bool {
        matches!(self.protocol.as_str(), "http" | "https")
    }
}

/// A Gentoo distfile mirror: one site with one or more [`Endpoint`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mirror {
    /// Human-readable site name (e.g. "OSU Open Source Lab").
    pub name: String,
    /// Two-letter ISO country code (e.g. "US", "DE").
    pub country: String,
    /// The country's full name (e.g. "United States (USA)").
    pub country_name: String,
    /// Continent or region (e.g. "North America", "Europe").
    pub region: String,
    /// All protocol endpoints advertised for this site.
    pub endpoints: Vec<Endpoint>,
}

impl Mirror {
    /// Pick the best endpoint matching `protocols`, in priority order.
    ///
    /// Returns the first endpoint of any protocol if none of the preferred
    /// ones match. Mirrors `mirrorselect`'s `Mirror.preferred_endpoint`.
    pub fn preferred_endpoint(&self, protocols: &[&str]) -> Option<&Endpoint> {
        let first = self.endpoints.first()?;
        for proto in protocols {
            if let Some(e) = self.endpoints.iter().find(|e| e.protocol == *proto) {
                return Some(e);
            }
        }
        Some(first)
    }

    /// The best HTTP/HTTPS endpoint (HTTPS preferred), if the site has one.
    pub fn http_endpoint(&self) -> Option<&Endpoint> {
        self.preferred_endpoint(&["https", "http"])
            .filter(|e| e.is_http())
    }
}

/// A collection of Gentoo distfile mirrors.
#[derive(Debug, Clone, Default)]
pub struct MirrorList {
    mirrors: Vec<Mirror>,
}

impl MirrorList {
    /// Create an empty [`MirrorList`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a [`MirrorList`] from an owned vector of mirrors.
    pub fn from_vec(mirrors: Vec<Mirror>) -> Self {
        Self { mirrors }
    }

    /// Fetch the official mirror list from Gentoo's XML API and parse it.
    ///
    /// Infallible: any network or parse error falls back to
    /// [`default_mirror_list`] (with a warning on stderr), so listing/setting
    /// mirrors keeps working offline.
    pub async fn fetch() -> MirrorList {
        match try_fetch_mirrors_xml().await {
            Ok(xml) => match parse_mirrors_xml(&xml) {
                Ok(list) => list,
                Err(e) => {
                    eprintln!("Warning: failed to parse mirror list: {e}");
                    eprintln!("Using the built-in default mirror list.");
                    default_mirror_list()
                }
            },
            Err(e) => {
                eprintln!("Warning: failed to fetch mirror list: {e}");
                eprintln!("Using the built-in default mirror list.");
                default_mirror_list()
            }
        }
    }

    /// All mirrors, in document order.
    pub fn all(&self) -> &[Mirror] {
        &self.mirrors
    }

    /// Mirrors whose ISO country code matches `code` (case-insensitive).
    pub fn by_country(&self, code: &str) -> Vec<&Mirror> {
        self.mirrors
            .iter()
            .filter(|m| m.country.eq_ignore_ascii_case(code))
            .collect()
    }

    /// Mirrors whose region matches `region` (case-insensitive).
    pub fn by_region(&self, region: &str) -> Vec<&Mirror> {
        self.mirrors
            .iter()
            .filter(|m| m.region.eq_ignore_ascii_case(region))
            .collect()
    }

    /// One URL per mirror that has an HTTP/HTTPS endpoint, HTTPS preferred.
    pub fn preferred_urls(&self) -> Vec<String> {
        self.mirrors
            .iter()
            .filter_map(|m| m.http_endpoint().map(|e| e.uri.clone()))
            .collect()
    }

    /// Selected mirrors (by reference) joined into a `GENTOO_MIRRORS` value:
    /// space-separated URLs, one per mirror (HTTPS preferred).
    pub fn to_gentoo_mirrors_string(&self, mirrors: &[&Mirror]) -> String {
        mirrors
            .iter()
            .filter_map(|m| m.http_endpoint().map(|e| e.uri.clone()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Fetch the distfiles mirror list XML.
///
/// Network errors are surfaced to the caller; [`MirrorList::fetch`] turns
/// them into the default fallback.
async fn try_fetch_mirrors_xml() -> Result<String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("em/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Network {
            url: DISTFILES_MIRRORS_XML.to_string(),
            source: e,
        })?;

    let response = client
        .get(DISTFILES_MIRRORS_XML)
        .send()
        .await
        .map_err(|e| Error::Network {
            url: DISTFILES_MIRRORS_XML.to_string(),
            source: e,
        })?;

    if !response.status().is_success() {
        return Err(Error::Http {
            url: DISTFILES_MIRRORS_XML.to_string(),
            status: response.status().as_u16(),
        });
    }

    response.text().await.map_err(|e| Error::Network {
        url: DISTFILES_MIRRORS_XML.to_string(),
        source: e,
    })
}

/// Parse the distfiles mirror list XML into a [`MirrorList`].
///
/// The document shape is:
/// ```xml
/// <mirrors>
///   <mirrorgroup region="North America" country="US" countryname="United States (USA)">
///     <mirror>
///       <name>OSU Open Source Lab</name>
///       <uri protocol="https" ipv4="y" ipv6="y">https://gentoo.osuosl.org/</uri>
///       <uri protocol="http" ipv4="y" ipv6="y">http://gentoo.osuosl.org/</uri>
///     </mirror>
///   </mirrorgroup>
/// </mirrors>
/// ```
///
/// XML comments (mirrors marked inactive in the source) are skipped
/// automatically by `roxmltree`.
fn parse_mirrors_xml(xml: &str) -> Result<MirrorList> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| Error::MirrorParse(e.to_string()))?;

    let mut mirrors = Vec::new();
    for group in doc.descendants().filter(|n| n.has_tag_name("mirrorgroup")) {
        let region = group.attribute("region").unwrap_or_default().to_string();
        let country = group.attribute("country").unwrap_or_default().to_string();
        let country_name = group
            .attribute("countryname")
            .unwrap_or_default()
            .to_string();

        for mirror in group.children().filter(|n| n.has_tag_name("mirror")) {
            let mut name = String::new();
            let mut endpoints = Vec::new();
            for child in mirror.children() {
                if child.has_tag_name("name") {
                    name = child.text().unwrap_or_default().trim().to_string();
                } else if child.has_tag_name("uri") {
                    let Some(uri) = child.text().map(str::trim).filter(|s| !s.is_empty()) else {
                        continue;
                    };
                    endpoints.push(Endpoint {
                        uri: uri.to_string(),
                        protocol: child.attribute("protocol").unwrap_or_default().to_string(),
                        ipv4: child.attribute("ipv4").is_some_and(|v| v == "y"),
                        ipv6: child.attribute("ipv6").is_some_and(|v| v == "y"),
                    });
                }
            }
            if endpoints.is_empty() {
                continue;
            }
            mirrors.push(Mirror {
                name,
                country: country.clone(),
                country_name: country_name.clone(),
                region: region.clone(),
                endpoints,
            });
        }
    }

    Ok(MirrorList::from_vec(mirrors))
}

/// A small built-in mirror list used when the network fetch fails.
///
/// Deliberately short — just enough to keep distfile fetching working offline.
pub fn default_mirror_list() -> MirrorList {
    MirrorList::from_vec(vec![
        Mirror {
            name: "OSU Open Source Lab".to_string(),
            country: "US".to_string(),
            country_name: "United States (USA)".to_string(),
            region: "North America".to_string(),
            endpoints: vec![
                https_ep("https://gentoo.osuosl.org/"),
                http_ep("http://gentoo.osuosl.org/"),
            ],
        },
        Mirror {
            name: "kernel.org".to_string(),
            country: "US".to_string(),
            country_name: "United States (USA)".to_string(),
            region: "North America".to_string(),
            endpoints: vec![
                https_ep("https://mirrors.kernel.org/gentoo/"),
                http_ep("http://mirrors.kernel.org/gentoo/"),
            ],
        },
        Mirror {
            name: "OVHcloud".to_string(),
            country: "FR".to_string(),
            country_name: "France".to_string(),
            region: "Europe".to_string(),
            endpoints: vec![
                https_ep("https://gentoo.mirrors.ovh.net/gentoo-distfiles/"),
                http_ep("http://gentoo.mirrors.ovh.net/gentoo-distfiles/"),
            ],
        },
    ])
}

fn https_ep(uri: &str) -> Endpoint {
    Endpoint {
        uri: uri.to_string(),
        protocol: "https".to_string(),
        ipv4: true,
        ipv6: true,
    }
}

fn http_ep(uri: &str) -> Endpoint {
    Endpoint {
        uri: uri.to_string(),
        protocol: "http".to_string(),
        ipv4: true,
        ipv6: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<mirrors>
  <mirrorgroup region="North America" country="US" countryname="United States (USA)">
    <mirror>
      <name>OSU Open Source Lab</name>
      <uri protocol="https" ipv4="y" ipv6="y" partial="n">https://gentoo.osuosl.org/</uri>
      <uri protocol="http" ipv4="y" ipv6="y" partial="n">http://gentoo.osuosl.org/</uri>
    </mirror>
    <!-- an inactive mirror, commented out in the source -->
    <mirror>
      <name>Georgia Tech</name>
      <uri protocol="ftp" ipv4="y" ipv6="n" partial="n">ftp://www.gtlib.gatech.edu/pub/gentoo</uri>
    </mirror>
  </mirrorgroup>
  <mirrorgroup region="Europe" country="DE" countryname="Germany">
    <mirror>
      <name>Ruhr-Universität Bochum</name>
      <uri protocol="https" ipv4="y" ipv6="y" partial="n">https://linux.rz.ruhr-uni-bochum.de/download/gentoo-mirror/</uri>
      <uri protocol="rsync" ipv4="y" ipv6="y" partial="n">rsync://linux.rz.ruhr-uni-bochum.de/gentoo</uri>
    </mirror>
  </mirrorgroup>
</mirrors>"#;

    #[test]
    fn parse_extracts_mirrors_with_endpoints() {
        let list = parse_mirrors_xml(SAMPLE_XML).unwrap();
        assert_eq!(list.all().len(), 3);

        let osu = &list.all()[0];
        assert_eq!(osu.name, "OSU Open Source Lab");
        assert_eq!(osu.country, "US");
        assert_eq!(osu.country_name, "United States (USA)");
        assert_eq!(osu.region, "North America");
        assert_eq!(osu.endpoints.len(), 2);
    }

    #[test]
    fn http_endpoint_prefers_https() {
        let list = parse_mirrors_xml(SAMPLE_XML).unwrap();
        let osu = &list.all()[0];
        assert_eq!(
            osu.http_endpoint().unwrap().uri,
            "https://gentoo.osuosl.org/"
        );
    }

    #[test]
    fn http_endpoint_is_none_for_ftp_only_site() {
        let list = parse_mirrors_xml(SAMPLE_XML).unwrap();
        let gt = &list.all()[1];
        assert_eq!(gt.name, "Georgia Tech");
        assert!(gt.http_endpoint().is_none());
    }

    #[test]
    fn preferred_urls_emits_one_url_per_http_mirror() {
        let list = parse_mirrors_xml(SAMPLE_XML).unwrap();
        let urls = list.preferred_urls();
        // Georgia Tech is ftp-only, so it's excluded; the other two appear once.
        assert_eq!(
            urls,
            vec![
                "https://gentoo.osuosl.org/",
                "https://linux.rz.ruhr-uni-bochum.de/download/gentoo-mirror/",
            ]
        );
    }

    #[test]
    fn by_country_matches_iso_code_case_insensitively() {
        let list = parse_mirrors_xml(SAMPLE_XML).unwrap();
        assert_eq!(list.by_country("us").len(), 2);
        assert_eq!(list.by_country("DE").len(), 1);
        assert!(list.by_country("jp").is_empty());
    }

    #[test]
    fn by_region_matches_case_insensitively() {
        let list = parse_mirrors_xml(SAMPLE_XML).unwrap();
        assert_eq!(list.by_region("north america").len(), 2);
        assert_eq!(list.by_region("Europe").len(), 1);
    }

    #[test]
    fn to_gentoo_mirrors_string_joins_selected() {
        let list = parse_mirrors_xml(SAMPLE_XML).unwrap();
        let us: Vec<&Mirror> = list.by_country("US");
        let s = list.to_gentoo_mirrors_string(&us);
        // Georgia Tech is ftp-only, so only OSU's https URL is emitted.
        assert_eq!(s, "https://gentoo.osuosl.org/");
    }

    #[test]
    fn default_mirror_list_has_http_mirrors() {
        let list = default_mirror_list();
        assert!(!list.preferred_urls().is_empty());
        // Every default mirror has a usable https endpoint.
        assert!(list.all().iter().all(|m| m.http_endpoint().is_some()));
    }

    #[test]
    fn endpoint_is_http() {
        assert!(https_ep("https://x").is_http());
        assert!(http_ep("http://x").is_http());
        assert!(
            !Endpoint {
                uri: "ftp://x".into(),
                protocol: "ftp".into(),
                ipv4: true,
                ipv6: false,
            }
            .is_http()
        );
    }

    #[test]
    fn empty_document_yields_empty_list() {
        let list = parse_mirrors_xml("<?xml version=\"1.0\"?><mirrors></mirrors>").unwrap();
        assert!(list.all().is_empty());
    }
}
