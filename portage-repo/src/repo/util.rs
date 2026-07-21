use std::path::Path;

use crate::error::{Error, Result};

/// Create an `Error::Io` from a path and an `io::Error`.
pub(crate) fn io_err(path: impl AsRef<Path>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.as_ref().to_path_buf(),
        source,
    }
}

/// Read a file to a string, mapping I/O errors to `Error::Io`.
pub(crate) fn read_to_string(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    std::fs::read_to_string(path).map_err(|e| io_err(path, e))
}

/// Portage's `_recursive_basename_filter` (`portage/util/__init__.py`): a
/// config directory's entry is data only if its basename does not start with
/// `.` (dotfiles) and does not end with `~` (editor backups).
fn config_basename_included(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| !n.starts_with('.') && !n.ends_with('~'))
}

/// Read non-blank, non-comment lines from a file.
///
/// Lines starting with `#` (after trimming) are treated as comments.
/// Returns an empty `Vec` if the file does not exist.
pub fn read_lines(path: impl AsRef<Path>) -> Result<Vec<String>> {
    let path = path.as_ref();
    // PMS 5.2.4: a profile configuration file may instead be a *directory*, in
    // which case the regular files directly within it are concatenated, sorted
    // by filename. Portage additionally skips dotfiles and `~` editor backups
    // (`_recursive_basename_filter`). Portage's `/etc/portage/profile` commonly
    // uses this form (e.g. `package.use.mask/<name>`).
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => {
            let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(path)
                .map_err(|e| io_err(path, e))?
                .filter_map(std::result::Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_file() && config_basename_included(p))
                .collect();
            entries.sort();
            let mut out = Vec::new();
            for entry in entries {
                out.extend(read_file_lines(&entry)?);
            }
            Ok(out)
        }
        Ok(_) => read_file_lines(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(io_err(path, e)),
    }
}

/// Read a single regular file as trimmed, comment-stripped, non-blank lines.
fn read_file_lines(path: &Path) -> Result<Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(io_err(path, e)),
    }
}

/// Read the first non-blank, non-comment line from a file.
///
/// Returns `None` if the file does not exist.
pub(crate) fn read_single_line(path: impl AsRef<Path>) -> Result<Option<String>> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_lines_skips_blanks_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "# comment").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "  alpha  ").unwrap();
        writeln!(f, "# another comment").unwrap();
        writeln!(f, "beta").unwrap();

        let lines = read_lines(&path).unwrap();
        assert_eq!(lines, vec!["alpha", "beta"]);
    }

    #[test]
    fn read_lines_missing_file_returns_empty() {
        let lines = read_lines(Path::new("/nonexistent/path/file.txt")).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn read_lines_directory_concatenates_sorted_skipping_dotfiles() {
        // PMS 5.2.4: a profile file may be a directory whose regular files are
        // concatenated in filename order; dotfiles are ignored.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("use.mask");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("20-b"), "bravo\n# c\ncharlie\n").unwrap();
        std::fs::write(sub.join("10-a"), "alpha\n").unwrap();
        std::fs::write(sub.join(".hidden"), "ignored\n").unwrap();
        // `~` editor backups are skipped too (portage `_recursive_basename_filter`).
        std::fs::write(sub.join("30-a~"), "backup\n").unwrap();
        std::fs::create_dir(sub.join("nested")).unwrap();

        let lines = read_lines(&sub).unwrap();
        assert_eq!(lines, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn read_single_line_returns_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "# comment\n\nfirst\nsecond\n").unwrap();

        let line = read_single_line(&path).unwrap();
        assert_eq!(line.as_deref(), Some("first"));
    }

    #[test]
    fn read_single_line_missing_returns_none() {
        let line = read_single_line(Path::new("/nonexistent/path/file.txt")).unwrap();
        assert!(line.is_none());
    }
}
