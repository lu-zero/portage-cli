use std::path::{Path, PathBuf};

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

/// How to expand a Portage-style config path that may be a file or a directory.
///
/// Portage accepts many `/etc/portage/*` (and profile) paths as either a single
/// regular file or a directory of fragments. Fragment selection and order
/// depend on the path family:
///
/// - **[`ConfigFilesMode::Flat`]** — PMS 5.2.4 / profile form: only regular
///   files **directly** in the directory, sorted by filename. Nested
///   subdirectories are ignored. Used for profile files, `package.*`, etc.
/// - **[`ConfigFilesMode::Recursive`]** — Portage `_recursive_file_list`:
///   nested subdirectories are walked (VCS dirs skipped); yield order is
///   depth-first with lexicographic sibling order (same as Portage's reverse-
///   sorted stack). Used for `make.conf` and other `getconfig(..., recursive=True)`
///   paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigFilesMode {
    /// Direct children only (PMS 5.2.4 directory-form config files).
    #[default]
    Flat,
    /// Nested walk matching Portage `_recursive_file_list`.
    Recursive,
}

/// Portage's `_recursive_basename_filter` (`portage/util/__init__.py`): a
/// config directory's entry is data only if its basename does not start with
/// `.` (dotfiles) and does not end with `~` (editor backups).
pub fn config_basename_included(name: &str) -> bool {
    !name.starts_with('.') && !name.ends_with('~')
}

fn config_basename_included_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(config_basename_included)
}

/// VCS directory basenames skipped by Portage's recursive config walk
/// (`portage.const.VCS_DIRS`).
const VCS_DIRS: &[&str] = &["CVS", "RCS", "SCCS", ".bzr", ".git", ".hg", ".svn"];

fn is_vcs_dir_name(name: &str) -> bool {
    VCS_DIRS.contains(&name)
}

/// Expand a config path to the ordered list of regular files it contributes.
///
/// | `path` | result |
/// |--------|--------|
/// | missing | empty |
/// | regular file | `[path]` (basename filter not applied to the root path) |
/// | directory | matching regular files in application order |
/// | other (fifo, …) | empty |
///
/// See [`ConfigFilesMode`] for flat vs recursive rules. Basename filter
/// (skip `.…` and `…~`) applies to directory entries, not to a root that is
/// itself a regular file.
pub fn list_config_files(path: impl AsRef<Path>, mode: ConfigFilesMode) -> Result<Vec<PathBuf>> {
    let path = path.as_ref();
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(path, e)),
    };

    if meta.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !meta.is_dir() {
        return Ok(Vec::new());
    }

    match mode {
        ConfigFilesMode::Flat => list_config_files_flat(path),
        ConfigFilesMode::Recursive => list_config_files_recursive(path),
    }
}

/// Same as [`list_config_files`] as an owned iterator (application order).
///
/// The directory is fully listed and sorted up front so order matches Portage;
/// the iterator is a thin adapter over that list (not a live `read_dir` stream).
pub fn iter_config_files(
    path: impl AsRef<Path>,
    mode: ConfigFilesMode,
) -> Result<std::vec::IntoIter<PathBuf>> {
    Ok(list_config_files(path, mode)?.into_iter())
}

fn list_config_files_flat(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| io_err(dir, e))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && config_basename_included_path(p))
        .collect();
    entries.sort();
    Ok(entries)
}

/// Portage `_recursive_file_list`: DFS with reverse-sorted child push so
/// pops yield lexicographic sibling order; nested dirs expanded in place.
fn list_config_files_recursive(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    // Stack of paths still to process (files and dirs).
    let mut stack = vec![root.to_path_buf()];

    while let Some(fullpath) = stack.pop() {
        let meta = match std::fs::metadata(&fullpath) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_err(&fullpath, e)),
        };

        if meta.is_dir() {
            let fname = fullpath.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // Root is always entered; children apply basename + VCS filters.
            if fullpath.as_path() != root
                && (is_vcs_dir_name(fname) || !config_basename_included(fname))
            {
                continue;
            }
            let mut children: Vec<PathBuf> = match std::fs::read_dir(&fullpath) {
                Ok(rd) => rd
                    .filter_map(std::result::Result::ok)
                    .map(|e| e.path())
                    .collect(),
                Err(e) => return Err(io_err(&fullpath, e)),
            };
            // Reverse sort so pop() yields lexicographic order.
            children.sort();
            children.reverse();
            stack.extend(children);
        } else if meta.is_file()
            && (fullpath.as_path() == root || config_basename_included_path(&fullpath))
        {
            out.push(fullpath);
        }
    }

    Ok(out)
}

/// Read non-blank, non-comment lines from a file or directory of files.
///
/// Lines starting with `#` (after trimming) are treated as comments.
/// Returns an empty `Vec` if the path does not exist.
///
/// Directory form uses [`ConfigFilesMode::Flat`] (PMS 5.2.4): regular files
/// directly within the directory are concatenated in filename order; nested
/// subdirectories, dotfiles, and `~` editor backups are skipped.
pub fn read_lines(path: impl AsRef<Path>) -> Result<Vec<String>> {
    let path = path.as_ref();
    let mut out = Vec::new();
    for entry in list_config_files(path, ConfigFilesMode::Flat)? {
        out.extend(read_file_lines(&entry)?);
    }
    Ok(out)
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

/// A repo's name: `profiles/repo_name`, or `x-<basename>` when that file is
/// missing — matches real portage's own fallback (`RepoConfig.
/// _read_repo_name`), used both when opening a repo and when synthesizing a
/// `RepoEntry` for a `PORTDIR_OVERLAY` directory that has no `repos.conf`
/// section of its own.
pub(crate) fn resolve_repo_name(repo_path: &Path) -> Result<String> {
    Ok(
        read_single_line(repo_path.join("profiles").join("repo_name"))?.unwrap_or_else(|| {
            format!(
                "x-{}",
                repo_path
                    .file_name()
                    .map(|s| s.to_string_lossy())
                    .unwrap_or_default()
            )
        }),
    )
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
    fn list_config_files_flat_skips_nested() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("conf");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("10-a"), "").unwrap();
        let nested = root.join("subdir");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("20-b"), "").unwrap();

        let files = list_config_files(&root, ConfigFilesMode::Flat).unwrap();
        assert_eq!(files, vec![root.join("10-a")]);
    }

    #[test]
    fn list_config_files_recursive_walks_nested_portage_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("make.conf");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("10-base"), "").unwrap();
        let sub = root.join("subdir");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("20-more"), "").unwrap();
        std::fs::write(root.join(".hidden"), "").unwrap();
        std::fs::write(root.join("backup~"), "").unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join(".git").join("config"), "").unwrap();

        let files = list_config_files(&root, ConfigFilesMode::Recursive).unwrap();
        assert_eq!(
            files,
            vec![root.join("10-base"), root.join("subdir").join("20-more")]
        );
    }

    #[test]
    fn list_config_files_file_is_singleton() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("make.conf");
        std::fs::write(&path, "USE=\"\"").unwrap();
        let files = list_config_files(&path, ConfigFilesMode::Flat).unwrap();
        assert_eq!(files, vec![path]);
    }

    #[test]
    fn list_config_files_missing_is_empty() {
        let files =
            list_config_files(Path::new("/nonexistent/make.conf"), ConfigFilesMode::Flat).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn iter_config_files_matches_list() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("c");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("b"), "").unwrap();
        std::fs::write(root.join("a"), "").unwrap();
        let listed = list_config_files(&root, ConfigFilesMode::Flat).unwrap();
        let iterated: Vec<_> = iter_config_files(&root, ConfigFilesMode::Flat)
            .unwrap()
            .collect();
        assert_eq!(listed, iterated);
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
