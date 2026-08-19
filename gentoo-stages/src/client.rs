use crate::{cache::Cache, error::Error, stage3::Stage3};
use bon::bon;
use futures::stream::StreamExt;
use gentoo_core::Arch;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};

/// Client for interacting with Gentoo distfiles mirrors
pub struct Client {
    mirror_url: String,
    arch: Arch,
    cache_dir: Cache,
    http_client: reqwest::Client,
}

#[bon]
impl Client {
    /// Create a new Client with default settings
    ///
    /// Uses <https://distfiles.gentoo.org> mirror, the host architecture,
    /// and a temporary cache directory.
    pub fn new() -> Result<Self, Error> {
        Client::builder().build()
    }

    /// Build a [`Client`] with explicit settings via [`Client::builder`]
    ///
    /// Each argument falls back to the same defaults as [`Client::new`] when
    /// left unset: the <https://distfiles.gentoo.org> mirror, the host
    /// architecture, and a temporary cache directory.
    #[builder(builder_type = "ClientBuilder")]
    pub fn new(
        mirror_url: Option<&str>,
        arch: Option<Arch>,
        #[builder(into)] cache_dir: Option<Cache>,
    ) -> Result<Self, Error> {
        let mirror_url = mirror_url
            .unwrap_or("https://distfiles.gentoo.org")
            .to_string();
        let arch = arch.unwrap_or_else(Arch::current);
        let cache_dir = cache_dir.map_or_else(|| tempfile::tempdir().map(Cache::Temp), Ok)?;
        let http_client = reqwest::Client::new();

        Ok(Self {
            mirror_url,
            arch,
            cache_dir,
            http_client,
        })
    }

    /// List all available stage3 images for the configured architecture
    ///
    /// Includes both remote images and locally cached images.
    pub async fn list(&self) -> Result<Vec<Stage3>, Error> {
        let mut stage3_list = self.fetch_all_stage3_flavors().await?;

        let cached_stage3s = self.scan_cached_stage3_files();

        for cached in cached_stage3s {
            if !stage3_list.iter().any(|s| s.name == cached.name) {
                stage3_list.push(cached);
            }
        }

        stage3_list.sort_by(|a, b| {
            let a_ts = extract_timestamp(&a.name);
            let b_ts = extract_timestamp(&b.name);
            b_ts.cmp(&a_ts)
        });

        Ok(stage3_list)
    }

    /// Get a specific stage3 variant (downloads if not cached)
    pub async fn get(&self, variant: &str) -> Result<Stage3, Error> {
        let stage3 = self
            .find(variant)
            .await?
            .ok_or_else(|| Error::VariantNotFound(variant.to_string()))?;

        if !stage3.is_cached() {
            self.download_stage3(&stage3).await?;
        }

        Ok(stage3)
    }

    /// Find a specific stage3 variant by name without downloading
    ///
    /// Returns `None` if the variant is not found in either the remote
    /// repository or the local cache.
    pub async fn find(&self, variant: &str) -> Result<Option<Stage3>, Error> {
        let stage3_list = self.list().await?;
        Ok(stage3_list.into_iter().find(|s| s.variant == variant))
    }

    /// Scan the cache directory for locally cached stage3 files
    fn scan_cached_stage3_files(&self) -> Vec<Stage3> {
        let arch_cache_dir = self
            .cache_dir
            .path()
            .join("stages")
            .join(self.arch.as_str());

        let mut cached_files = Vec::new();

        // Try to read the architecture-specific cache directory
        if let Ok(entries) = std::fs::read_dir(&arch_cache_dir) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_file()
                    && path.extension().and_then(|s| s.to_str()) == Some("xz")
                    && let Some(file_name) = path.file_name().and_then(|s| s.to_str())
                    && file_name.starts_with("stage3-")
                {
                    let variant = extract_variant_from_filename(file_name);
                    let date = extract_date_from_filename(file_name);

                    // No size or URL for cached files, so use placeholders.
                    let stage3 = Stage3::new(
                        file_name.to_string(),
                        String::new(), // Empty URL for cached files
                        0,             // Unknown size
                        date,
                        self.arch,
                        variant,
                        self.cache_dir.path(),
                    );

                    cached_files.push(stage3);
                }
            }
        }

        cached_files
    }

    /// Fetch the list of all available stage3 images for the architecture
    async fn fetch_all_stage3_flavors(&self) -> Result<Vec<Stage3>, Error> {
        let latest_url = format!(
            "{}/releases/{}/autobuilds/latest-stage3.txt",
            self.mirror_url.trim_end_matches('/'),
            self.arch.as_str()
        );

        info!("Fetching all stage3 variants from: {}", latest_url);

        let content = self
            .http_client
            .get(&latest_url)
            .send()
            .await?
            .text()
            .await?;

        debug!("Received {} bytes from stage3 list", content.len());
        self.parse_all_flavors_list(&content)
    }

    /// Parse stage3 list content into Stage3 structures (for all flavors)
    fn parse_all_flavors_list(&self, content: &str) -> Result<Vec<Stage3>, Error> {
        let mut stage3_images = Vec::new();
        let mut in_pgp_section = false;

        for line in content.lines() {
            let line = line.trim();

            if line.is_empty() || line.starts_with('#') || line.starts_with("Hash:") {
                continue;
            }

            if line == "-----BEGIN PGP SIGNED MESSAGE-----" {
                continue;
            }

            if line == "-----BEGIN PGP SIGNATURE-----" {
                in_pgp_section = true;
                continue;
            }

            if line == "-----END PGP SIGNATURE-----" {
                in_pgp_section = false;
                continue;
            }

            if in_pgp_section {
                continue;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let full_path = parts[0].to_string();
                let size = parts[1].parse::<u64>().map_err(|e| {
                    Error::ParseError(format!("Failed to parse size for {}: {}", full_path, e))
                })?;

                let name = full_path
                    .split('/')
                    .next_back()
                    .unwrap_or(&full_path)
                    .to_string();

                if name.starts_with("stage3-") {
                    let date = extract_date_from_filename(&name);
                    let variant = extract_variant_from_filename(&name);

                    stage3_images.push(Stage3::new(
                        name.clone(),
                        format!(
                            "{}/releases/{}/autobuilds/{}",
                            self.mirror_url.trim_end_matches('/'),
                            self.arch.as_str(),
                            full_path
                        ),
                        size,
                        date,
                        self.arch,
                        variant,
                        self.cache_dir.path(),
                    ));
                }
            }
        }

        if stage3_images.is_empty() {
            return Err(Error::ParseError(format!(
                "No stage3 images found for arch={}",
                self.arch
            )));
        }

        Ok(stage3_images)
    }

    /// Download a stage3 image
    async fn download_stage3(&self, stage3: &Stage3) -> Result<(), Error> {
        let arch_cache_dir = stage3.arch_cache_dir();
        tokio::fs::create_dir_all(&arch_cache_dir).await?;

        let cache_path = stage3.file_path();

        info!("Downloading stage3 image: {}", stage3.name);
        debug!("URL: {}", stage3.url);

        let response = self.http_client.get(&stage3.url).send().await?;

        let temp_file = tempfile::NamedTempFile::new_in(&arch_cache_dir)?;
        let std_file = temp_file.as_file().try_clone()?;
        let mut file = tokio::fs::File::from_std(std_file);
        let mut stream = response.bytes_stream();

        let mut downloaded: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            downloaded += chunk.len() as u64;
            file.write_all(&chunk).await?;
            debug!(
                "Downloaded {:>8} / {} bytes ({:.1}%)",
                downloaded,
                stage3.size,
                if stage3.size > 0 {
                    (downloaded as f64 / stage3.size as f64) * 100.0
                } else {
                    0.0
                }
            );
        }
        file.flush().await?;
        drop(file);

        if stage3.size > 0 && downloaded != stage3.size {
            return Err(Error::SizeMismatch {
                expected: stage3.size,
                got: downloaded,
            });
        }

        temp_file
            .persist(&cache_path)
            .map_err(|e| Error::IoError(e.error))?;

        info!("Downloaded stage3 image to: {}", cache_path.display());

        Ok(())
    }
}

/// Extract timestamp from stage3 filename as a sortable integer
fn extract_timestamp(filename: &str) -> u64 {
    extract_date_from_filename(filename)
        .and_then(|ts| ts.replace('T', "").trim_end_matches('Z').parse().ok())
        .unwrap_or(0)
}

/// Extract variant from stage3 filename
///
/// The variant is everything between "stage3-" and the final "-{timestamp}.tar.xz".
fn extract_variant_from_filename(filename: &str) -> String {
    let without_ext = filename.strip_suffix(".tar.xz").unwrap_or(filename);
    let without_prefix = without_ext.strip_prefix("stage3-").unwrap_or(without_ext);

    // The last hyphen separates variant from the -YYYYMMDDTHHMMSSZ timestamp.
    if let Some(last_hyphen_pos) = without_prefix.rfind('-') {
        let potential_timestamp = &without_prefix[last_hyphen_pos + 1..];
        if potential_timestamp.contains('T') && potential_timestamp.ends_with('Z') {
            return without_prefix[..last_hyphen_pos].to_string();
        }
    }

    without_prefix.to_string()
}

/// Extract date from stage3 filename
///
/// Returns the full datetime string (e.g., "20260216T163057Z"), or `None`
/// if no valid timestamp can be extracted.
fn extract_date_from_filename(filename: &str) -> Option<&str> {
    // Split from the right to handle complex arch names with hyphens
    let mut parts = filename.rsplit('-');
    let last_part = parts.next()?;
    let timestamp_part = last_part.strip_suffix(".tar.xz").unwrap_or(last_part);

    if timestamp_part.contains('T') && timestamp_part.ends_with('Z') {
        Some(timestamp_part)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_timestamp() {
        assert_eq!(
            extract_timestamp("stage3-amd64-openrc-20260216T163057Z.tar.xz"),
            20260216163057
        );
        assert_eq!(
            extract_timestamp("stage3-riscv64-rv64_lp64d-openrc-20231018T010001Z.tar.xz"),
            20231018010001
        );
        assert_eq!(extract_timestamp("invalid"), 0);
    }

    #[test]
    fn test_extract_variant_from_filename() {
        assert_eq!(
            extract_variant_from_filename("stage3-amd64-openrc-20260216T163057Z.tar.xz"),
            "amd64-openrc"
        );
        assert_eq!(
            extract_variant_from_filename(
                "stage3-riscv64-rv64_lp64d-openrc-20231018T010001Z.tar.xz"
            ),
            "riscv64-rv64_lp64d-openrc"
        );
        assert_eq!(
            extract_variant_from_filename("stage3-armv7a-hardfloat-openrc-20240101T120000Z.tar.xz"),
            "armv7a-hardfloat-openrc"
        );
    }

    #[test]
    fn test_extract_date_from_filename() {
        assert_eq!(
            extract_date_from_filename("stage3-amd64-openrc-20260216T163057Z.tar.xz"),
            Some("20260216T163057Z")
        );
        assert_eq!(
            extract_date_from_filename("stage3-riscv64-openrc-20231018T010001Z.tar.xz"),
            Some("20231018T010001Z")
        );
        assert_eq!(extract_date_from_filename("stage3-no-date.tar.xz"), None);
        assert_eq!(extract_date_from_filename("invalid"), None);
    }
}
