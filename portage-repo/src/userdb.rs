//! Reading the `passwd`/`group` account databases.
//!
//! Both are colon-separated text with the name first and a numeric id in field
//! 2 (`name:pw:uid:gid:…` and `name:pw:gid:…`), so one parser serves both.
//!
//! Deliberately *not* NSS. Two reasons: this workspace has no C-library
//! dependency to call `getpwnam` through, and — the substantive one — most
//! callers here are not asking about the *running* system at all. Resolving
//! `fowners root:portage` inside an install image means reading that image's
//! own `etc/passwd`, and a stage build's target root routinely has accounts the
//! host does not (or the reverse). A name that only NSS could resolve returns
//! `None`, and every caller has a defined fallback for that.

use std::path::Path;

/// The numeric id `name` maps to in the contents of a `passwd` or `group`
/// database.
pub fn id_in(db: &str, name: &str) -> Option<u32> {
    db.lines().find_map(|line| {
        let mut cols = line.split(':');
        (cols.next() == Some(name))
            .then(|| cols.nth(1))
            .flatten()
            .and_then(|id| id.parse().ok())
    })
}

/// [`id_in`] against a database file. A missing or unreadable file is simply
/// "not found".
pub fn id_in_file(db: &Path, name: &str) -> Option<u32> {
    id_in(&std::fs::read_to_string(db).ok()?, name)
}

/// Look up `name`, returning the requested 0-based fields — for callers that
/// need more than the id (a `passwd` row's uid *and* primary gid, say).
///
/// `None` if the file is missing, the name is absent, or any requested field is
/// past the end of its row.
pub fn lookup(db: &Path, name: &str, fields: &[usize]) -> Option<Vec<String>> {
    let content = std::fs::read_to_string(db).ok()?;
    // The first row for `name` decides, exactly as a duplicate entry in a real
    // `passwd` is resolved: a short row is a failed lookup, not a reason to
    // keep searching for a longer one further down.
    let row = content
        .lines()
        .find(|line| line.split(':').next() == Some(name))?;
    let cols: Vec<&str> = row.split(':').collect();
    fields
        .iter()
        .map(|&i| cols.get(i).map(|s| (*s).to_string()))
        .collect()
}

/// Whether `db` exists and holds at least one real entry.
///
/// Distinguishes "this root has no account database yet" (a pre-baselayout
/// stage build, where falling back to the host's ids is the only way forward)
/// from "it has one and the name is not in it" (which must stay an error, or an
/// install image silently gets ids from the wrong system).
pub fn is_populated(db: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(db) else {
        return false;
    };
    content.lines().any(|line| {
        let line = line.trim();
        !line.is_empty() && !line.starts_with('#')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n\
                          portage:x:250:250:portage:/var/tmp/portage:/sbin/nologin\n";
    const GROUP: &str = "root:x:0:\nportage:x:250:\nwheel:x:10:lu_zero\n";

    #[test]
    fn the_id_is_field_two_in_both_databases() {
        assert_eq!(id_in(PASSWD, "portage"), Some(250));
        assert_eq!(id_in(GROUP, "portage"), Some(250));
        assert_eq!(id_in(GROUP, "wheel"), Some(10));
        assert_eq!(id_in(PASSWD, "root"), Some(0));
        assert_eq!(id_in(PASSWD, "nobody"), None);
        // A name must match the whole first column, not a prefix of it.
        assert_eq!(id_in(GROUP, "port"), None);
        assert_eq!(id_in("", "portage"), None);
    }

    #[test]
    fn lookup_returns_the_requested_fields() {
        let dir = tempfile::tempdir().unwrap();
        let passwd = dir.path().join("passwd");
        std::fs::write(&passwd, PASSWD).unwrap();

        // uid + primary gid, what an `fowners <user>` resolution needs.
        assert_eq!(
            lookup(&passwd, "portage", &[2, 3]),
            Some(vec!["250".to_string(), "250".to_string()])
        );
        assert_eq!(lookup(&passwd, "nobody", &[2]), None);
        // A field past the end of the row fails the whole lookup rather than
        // returning a short vector the caller would misindex — and the first
        // matching row is the answer, so a later, longer one is not consulted.
        assert_eq!(lookup(&passwd, "portage", &[2, 99]), None);
        let dupes = dir.path().join("dupes");
        std::fs::write(&dupes, "portage:x\nportage:x:250:250:\n").unwrap();
        assert_eq!(lookup(&dupes, "portage", &[2]), None);
        assert_eq!(lookup(&dir.path().join("absent"), "portage", &[2]), None);
    }

    #[test]
    fn populated_means_a_real_entry_not_just_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("passwd");
        assert!(!is_populated(&path), "missing file");
        std::fs::write(&path, "").unwrap();
        assert!(!is_populated(&path), "empty file");
        std::fs::write(&path, "# only a comment\n\n").unwrap();
        assert!(!is_populated(&path), "comments are not entries");
        std::fs::write(&path, PASSWD).unwrap();
        assert!(is_populated(&path));
    }
}
