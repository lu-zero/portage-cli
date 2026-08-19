use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Cache configuration for stage3 images
#[derive(Debug)]
pub enum Cache {
    /// Temporary cache that will be automatically cleaned up
    Temp(TempDir),
    /// Persistent cache at a specific path
    Path(PathBuf),
}

impl Cache {
    /// Get the cache directory path
    pub fn path(&self) -> &Path {
        match self {
            Cache::Temp(temp_dir) => temp_dir.path(),
            Cache::Path(path) => path,
        }
    }
}

impl<T> From<T> for Cache
where
    T: Into<PathBuf>,
{
    fn from(path: T) -> Self {
        Cache::Path(path.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_cache_path_method() {
        let temp_cache = Cache::Temp(tempfile::tempdir().unwrap());
        assert!(temp_cache.path().exists());

        let path_cache = Cache::Path(PathBuf::from("./test_cache"));
        assert_eq!(path_cache.path(), Path::new("./test_cache"));
    }

    #[test]
    fn test_into_cache_conversions() {
        // Test PathBuf -> Cache
        let path_buf: PathBuf = PathBuf::from("./test_cache");
        let cache_from_pathbuf: Cache = path_buf.into();
        match cache_from_pathbuf {
            Cache::Path(p) => assert_eq!(p, PathBuf::from("./test_cache")),
            Cache::Temp(_) => panic!("Expected Path variant"),
        }

        // Test &str -> Cache
        let cache_from_str: Cache = "./test_cache".into();
        match cache_from_str {
            Cache::Path(p) => assert_eq!(p, PathBuf::from("./test_cache")),
            Cache::Temp(_) => panic!("Expected Path variant"),
        }

        // Test String -> Cache
        let string_path = "./test_cache".to_string();
        let cache_from_string: Cache = string_path.into();
        match cache_from_string {
            Cache::Path(p) => assert_eq!(p, PathBuf::from("./test_cache")),
            Cache::Temp(_) => panic!("Expected Path variant"),
        }

        // Test &Path -> Cache
        let path = Path::new("./test_cache");
        let cache_from_path: Cache = path.into();
        match cache_from_path {
            Cache::Path(p) => assert_eq!(p, PathBuf::from("./test_cache")),
            Cache::Temp(_) => panic!("Expected Path variant"),
        }
    }
}
