//! Remote binhost index freshness: a local on-disk cache for a fetched
//! `Packages` index, so `-g`/`--getbinpkg` doesn't re-download the whole
//! index on every run.
//!
//! Mirrors real portage's `binarytree._populate_remote_repo`
//! (`dbapi/bintree.py`): the cache lives at
//! `${EROOT}/var/cache/edb/binhost/<host>/<url-path>/Packages`, carrying two
//! extra header fields alongside the ones `em maint binhost`/portage already
//! write: `TIMESTAMP` (when the *server* generated this index — echoed back
//! as `If-Modified-Since` on the next fetch) and `DOWNLOAD_TIMESTAMP` (when
//! *we* last downloaded or revalidated it, used for the `TTL` check). A
//! binhost marked `frozen`, or one still within its own `TTL` header, skips
//! the network entirely; otherwise a conditional GET either confirms the
//! cache (HTTP 304) or returns fresh content.

use camino::{Utf8Path, Utf8PathBuf};

use crate::binhost::{IndexFetch, fetch_index};
use crate::error::{Error, Result};

/// The subset of a `Packages` index header this cache cares about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PackagesHeader {
    /// The server's own `TIMESTAMP:` field — when *it* generated this index.
    timestamp: Option<i64>,
    /// When *we* last downloaded or revalidated this cached copy.
    download_timestamp: Option<i64>,
    /// Seconds a downloaded copy is trusted without even asking the server
    /// (a server-configured `PORTAGE_BINHOST_TTL`, echoed in its own index).
    ttl: Option<i64>,
}

fn parse_packages_header(text: &str) -> PackagesHeader {
    let mut h = PackagesHeader::default();
    let header_block = text.split("\n\n").next().unwrap_or("");
    for line in header_block.lines() {
        let Some((k, v)) = line.split_once(": ") else {
            continue;
        };
        match k {
            "TIMESTAMP" => h.timestamp = v.trim().parse().ok(),
            "DOWNLOAD_TIMESTAMP" => h.download_timestamp = v.trim().parse().ok(),
            "TTL" => h.ttl = v.trim().parse().ok(),
            _ => {}
        }
    }
    h
}

/// Set (replacing any existing value) one `KEY: value` line in the header
/// block only — every package entry after the first blank line is untouched.
fn set_header_field(text: &str, key: &str, value: &str) -> String {
    let mut parts = text.splitn(2, "\n\n");
    let header = parts.next().unwrap_or("");
    let rest = parts.next();
    let prefix = format!("{key}: ");
    let mut lines: Vec<&str> = header.lines().filter(|l| !l.starts_with(&prefix)).collect();
    let new_line = format!("{key}: {value}");
    lines.push(&new_line);
    let mut out = lines.join("\n");
    out.push('\n');
    if let Some(rest) = rest {
        out.push('\n');
        out.push_str(rest);
    }
    out
}

/// `${EROOT}/var/cache/edb/binhost/<host>/<url-path>/Packages` — real
/// portage's own local-cache layout (`CACHE_PATH` = `var/cache/edb`).
/// `None` if `sync_uri` isn't a parseable absolute URL (no cache, so the
/// caller always fetches fresh — safe fallback, never a hard error over a
/// cache-path oddity).
fn local_cache_path(eroot: &Utf8Path, sync_uri: &str) -> Option<Utf8PathBuf> {
    let parsed = url::Url::parse(sync_uri).ok()?;
    let host = parsed.host_str()?;
    let path = parsed.path().trim_start_matches('/');
    Some(
        eroot
            .join("var/cache/edb/binhost")
            .join(host)
            .join(path)
            .join("Packages"),
    )
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn write_cache(path: &Utf8Path, text: &str) {
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("warning: could not create binhost cache dir {parent}: {e}");
        return;
    }
    if let Err(e) = std::fs::write(path, text) {
        eprintln!("warning: could not write binhost cache {path}: {e}");
    }
}

/// Fetch a binhost's `Packages` index, using the local cache to skip the
/// network when possible. Returns the index text (fresh, cached, or
/// revalidated) and a short reason for the caller to report.
///
/// `sync_uri` is the binhost's base URI (`binrepos.conf`'s `sync-uri` /
/// `PORTAGE_BINHOST` entry); `frozen` is that repo's `frozen =` setting
/// ("prefer the local cache, don't even ask the server"); `eroot` is where
/// the cache directory is rooted.
///
/// Order of checks (matching `_populate_remote_repo`):
/// 1. `frozen` + a cached copy exists → use it, no network at all.
/// 2. Within the cached copy's own `TTL` (`download_timestamp + ttl > now`)
///    → use it, no network at all.
///
/// 3. Otherwise, a conditional GET (`If-Modified-Since` = the cached
///    `TIMESTAMP`, if any). A 304 revalidates the cache; fresh content
///    replaces it (`DOWNLOAD_TIMESTAMP` = now, `TIMESTAMP` from the
///    response's `Last-Modified` when the index's own content has none).
/// 4. A network/HTTP failure falls back to a stale cached copy rather than
///    failing the whole `--getbinpkg` run over one unreachable binhost.
pub async fn fetch_index_cached(
    sync_uri: &str,
    frozen: bool,
    eroot: &Utf8Path,
) -> Result<(String, &'static str)> {
    let cache_path = local_cache_path(eroot, sync_uri);
    let cached = cache_path
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok());
    let header = cached
        .as_deref()
        .map(parse_packages_header)
        .unwrap_or_default();
    let now = now_unix();

    if let Some(cached_text) = &cached {
        if frozen {
            return Ok((cached_text.clone(), "frozen (using cached index)"));
        }
        if let (Some(dl), Some(ttl)) = (header.download_timestamp, header.ttl)
            && ttl > 0
            && dl + ttl > now
        {
            return Ok((cached_text.clone(), "within TTL (using cached index)"));
        }
    }

    let if_modified_since = header.timestamp.map(|ts| {
        httpdate::fmt_http_date(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(ts.max(0) as u64),
        )
    });

    match fetch_index(sync_uri, if_modified_since.as_deref()).await {
        Ok(IndexFetch::NotModified) => {
            let Some(cached_text) = cached else {
                return Err(Error::StaleNotModified {
                    url: sync_uri.to_string(),
                });
            };
            if let Some(path) = &cache_path {
                write_cache(
                    path,
                    &set_header_field(&cached_text, "DOWNLOAD_TIMESTAMP", &now.to_string()),
                );
            }
            Ok((cached_text, "not modified (304)"))
        }
        Ok(IndexFetch::Fresh {
            text,
            last_modified,
        }) => {
            let mut cached_copy = set_header_field(&text, "DOWNLOAD_TIMESTAMP", &now.to_string());
            if parse_packages_header(&cached_copy).timestamp.is_none()
                && let Some(lm) = last_modified.as_deref()
                && let Ok(t) = httpdate::parse_http_date(lm)
                && let Ok(secs) = t.duration_since(std::time::UNIX_EPOCH)
            {
                cached_copy =
                    set_header_field(&cached_copy, "TIMESTAMP", &secs.as_secs().to_string());
            }
            if let Some(path) = &cache_path {
                write_cache(path, &cached_copy);
            }
            Ok((text, "fetched fresh"))
        }
        Err(e) => {
            if let Some(cached_text) = cached {
                eprintln!(
                    "warning: could not refresh binhost index {sync_uri} ({e:#}); using cached copy"
                );
                Ok((cached_text, "stale cache (refresh failed)"))
            } else {
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_packages_header_reads_all_three_fields() {
        let h = parse_packages_header(
            "TIMESTAMP: 100\nDOWNLOAD_TIMESTAMP: 200\nTTL: 300\n\nCPV: a/b-1\n",
        );
        assert_eq!(h.timestamp, Some(100));
        assert_eq!(h.download_timestamp, Some(200));
        assert_eq!(h.ttl, Some(300));
    }

    #[test]
    fn parse_packages_header_missing_fields_are_none() {
        let h = parse_packages_header("PACKAGES: 1\n\nCPV: a/b-1\n");
        assert_eq!(h, PackagesHeader::default());
    }

    #[test]
    fn set_header_field_replaces_existing_value() {
        let text = "TIMESTAMP: 1\nDOWNLOAD_TIMESTAMP: 2\n\nCPV: a/b-1\n";
        let updated = set_header_field(text, "DOWNLOAD_TIMESTAMP", "999");
        let h = parse_packages_header(&updated);
        assert_eq!(h.download_timestamp, Some(999));
        assert_eq!(h.timestamp, Some(1));
        // The package block is untouched.
        assert!(updated.contains("CPV: a/b-1"));
    }

    #[test]
    fn set_header_field_adds_a_missing_key() {
        let text = "TIMESTAMP: 1\n\nCPV: a/b-1\n";
        let updated = set_header_field(text, "DOWNLOAD_TIMESTAMP", "42");
        assert_eq!(parse_packages_header(&updated).download_timestamp, Some(42));
    }

    #[test]
    fn local_cache_path_matches_real_portages_layout() {
        let eroot = Utf8Path::new("/");
        let path = local_cache_path(eroot, "https://example.com:8080/binhost/amd64/").unwrap();
        assert_eq!(
            path.as_str(),
            "/var/cache/edb/binhost/example.com/binhost/amd64/Packages"
        );
    }

    #[test]
    fn local_cache_path_none_for_an_unparseable_uri() {
        assert!(local_cache_path(Utf8Path::new("/"), "not a url").is_none());
    }
}
