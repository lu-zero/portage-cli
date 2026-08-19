//! Remote binhost transport: fetch the `Packages` index and binary packages
//! from a `PORTAGE_BINHOST` / `binrepos.conf` URI over http(s).
//!
//! This crate owns network I/O (reqwest); the index *format* lives in the `em`
//! binary's `binpkg` module. Matches portage's `binarytree._populate_remote`
//! shape: try `Packages.gz` first (gzip), fall back to plain `Packages`, then
//! download individual binpkgs lazily (per-merge, never bulk).

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use crate::error::{Error, Result};

const PKG_VERSION: &str = concat!("em/", env!("CARGO_PKG_VERSION"));

/// Outcome of a conditional [`fetch_index`] call.
pub enum IndexFetch {
    /// The server confirmed the caller's `if_modified_since` value is still
    /// current (HTTP 304) — the caller's own cached copy remains valid.
    NotModified,
    /// Fresh index content, plus the response's `Last-Modified` header if the
    /// server sent one (portage records this as the cached copy's new
    /// `TIMESTAMP`, used as `if_modified_since` on the *next* conditional
    /// fetch).
    Fresh {
        text: String,
        last_modified: Option<String>,
    },
}

/// Fetch a binhost's `Packages` index as text, optionally as a conditional
/// GET (`If-Modified-Since: <if_modified_since>`, an RFC 7231 HTTP-date —
/// see `httpdate::fmt_http_date`).
///
/// Tries `<base>/Packages.gz` first (gzip-decompressed), falling back to
/// `<base>/Packages` when the `.gz` is absent (portage: "not guaranteed to
/// exist"). Any other HTTP error is surfaced. `base` has its trailing slash
/// trimmed, matching portage's URL construction.
pub async fn fetch_index(base_url: &str, if_modified_since: Option<&str>) -> Result<IndexFetch> {
    let client = reqwest::Client::builder()
        .user_agent(PKG_VERSION)
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| Error::Network {
            url: base_url.to_string(),
            source: e,
        })?;

    let gz_url = format!("{}/Packages.gz", base_url.trim_end_matches('/'));
    let mut req = client.get(&gz_url);
    if let Some(v) = if_modified_since {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, v);
    }
    match req.send().await {
        Ok(resp) if resp.status() == reqwest::StatusCode::NOT_MODIFIED => {
            Ok(IndexFetch::NotModified)
        }
        Ok(resp) if resp.status().is_success() => {
            let last_modified = last_modified_header(&resp);
            let bytes = resp.bytes().await.map_err(|e| Error::Network {
                url: gz_url.clone(),
                source: e,
            })?;
            let text =
                gunzip(&bytes).map_err(|e| Error::Manifest(format!("gunzip {gz_url}: {e}")))?;
            Ok(IndexFetch::Fresh {
                text,
                last_modified,
            })
        }
        Ok(resp)
            if resp.status() == reqwest::StatusCode::NOT_FOUND
                || resp.status() == reqwest::StatusCode::FORBIDDEN =>
        {
            // .gz optional — fall through to the plain index.
            fetch_plain(&client, base_url, if_modified_since).await
        }
        Ok(resp) => Err(Error::Http {
            url: gz_url,
            status: resp.status().as_u16(),
        }),
        Err(e) => {
            // Network error on .gz: don't retry plain — a transient failure
            // shouldn't silently mask. Portage treats only 404 as "absent".
            if e.is_connect() || e.is_timeout() {
                return Err(Error::Network {
                    url: gz_url,
                    source: e,
                });
            }
            fetch_plain(&client, base_url, if_modified_since).await
        }
    }
}

fn last_modified_header(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

async fn fetch_plain(
    client: &reqwest::Client,
    base_url: &str,
    if_modified_since: Option<&str>,
) -> Result<IndexFetch> {
    let url = format!("{}/Packages", base_url.trim_end_matches('/'));
    let mut req = client.get(&url);
    if let Some(v) = if_modified_since {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, v);
    }
    let resp = req.send().await.map_err(|e| Error::Network {
        url: url.clone(),
        source: e,
    })?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(IndexFetch::NotModified);
    }
    if !resp.status().is_success() {
        return Err(Error::Http {
            url,
            status: resp.status().as_u16(),
        });
    }
    let last_modified = last_modified_header(&resp);
    let text = resp
        .text()
        .await
        .map_err(|e| Error::Network { url, source: e })?;
    Ok(IndexFetch::Fresh {
        text,
        last_modified,
    })
}

/// Download a binary package from `url` into `dest` (a file path).
///
/// Streams to avoid buffering whole packages in memory. `dest`'s parent is
/// created if missing. Portage downloads to `<dest>.partial` then renames;
/// we do the same so a half-fetched file never appears complete to a
/// concurrent `-k` lookup.
pub async fn fetch_binpkg(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let partial = dest.with_extension("gpkg.tar.partial");
    if partial.exists() {
        let _ = std::fs::remove_file(&partial);
    }

    let client = reqwest::Client::builder()
        .user_agent(PKG_VERSION)
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| Error::Network {
            url: url.to_string(),
            source: e,
        })?;
    let resp = client.get(url).send().await.map_err(|e| Error::Network {
        url: url.to_string(),
        source: e,
    })?;
    if !resp.status().is_success() {
        return Err(Error::Http {
            url: url.to_string(),
            status: resp.status().as_u16(),
        });
    }
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    {
        let mut file = std::fs::File::create(&partial).map_err(|source| Error::Io {
            path: partial.clone(),
            source,
        })?;
        use std::io::Write;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Error::Network {
                url: url.to_string(),
                source: e,
            })?;
            file.write_all(&chunk).map_err(|source| Error::Io {
                path: partial.clone(),
                source,
            })?;
        }
        file.flush().map_err(|source| Error::Io {
            path: partial.clone(),
            source,
        })?;
    }
    std::fs::rename(&partial, dest).map_err(|source| Error::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Gzip-decode `bytes` to a string. Portage's `Packages.gz` is always text.
fn gunzip(bytes: &[u8]) -> std::result::Result<String, std::io::Error> {
    let mut decoder = flate2::read::GzDecoder::new(bytes);
    let mut out = String::new();
    decoder.read_to_string(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gunzip_decodes_a_packages_index() {
        // gzip of "CPV: app-test/foo-1.0\n\n" (the flate2 encoder round-trips).
        let raw = "VERSION: 0\nPACKAGES: 1\n\nCPV: app-test/foo-1.0\nPATH: foo-1.0-1.gpkg.tar\n";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write;
        encoder.write_all(raw.as_bytes()).unwrap();
        let gz = encoder.finish().unwrap();
        assert_eq!(gunzip(&gz).unwrap(), raw);
    }
}
