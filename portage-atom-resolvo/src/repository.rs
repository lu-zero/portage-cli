//! Package repository abstraction
//!
//! [`PackageRepository`] provides read-only access to a package database.
//! [`InMemoryRepository`] is a simple implementation for testing.

use std::collections::HashMap;

use portage_atom::Cpn;

use crate::pool::PackageMetadata;

/// Read-only package database
pub trait PackageRepository {
    /// Return all distinct category/package names in the repository
    fn all_packages(&self) -> Vec<Cpn>;

    /// Return every version available for the given category/package
    fn versions_for(&self, cpn: &Cpn) -> Vec<PackageMetadata>;
}

/// In-memory repository backed by a `HashMap`, useful for tests
pub struct InMemoryRepository {
    packages: HashMap<Cpn, Vec<PackageMetadata>>,
}

impl InMemoryRepository {
    /// Create an empty repository
    pub fn new() -> Self {
        Self {
            packages: HashMap::new(),
        }
    }

    /// Add a package version to the repository
    pub fn add(&mut self, meta: PackageMetadata) {
        self.packages.entry(meta.cpv.cpn).or_default().push(meta);
    }
}

impl Default for InMemoryRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl PackageRepository for InMemoryRepository {
    fn all_packages(&self) -> Vec<Cpn> {
        self.packages.keys().cloned().collect()
    }

    fn versions_for(&self, cpn: &Cpn) -> Vec<PackageMetadata> {
        self.packages.get(cpn).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::PackageDeps;
    use portage_atom::Cpv;
    use std::collections::HashSet;

    #[test]
    fn in_memory_add_and_query() {
        let mut repo = InMemoryRepository::new();
        let meta = PackageMetadata {
            cpv: Cpv::parse("dev-lang/rust-1.75.0").unwrap(),
            slot: Some("0".into()),
            subslot: None,
            iuse: vec![],
            use_flags: HashSet::new(),
            repo: None,
            dependencies: PackageDeps::default(),
        };
        repo.add(meta);

        let cpn = Cpn::new("dev-lang", "rust");
        let pkgs = repo.all_packages();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0], cpn);

        let versions = repo.versions_for(&cpn);
        assert_eq!(versions.len(), 1);
    }

    #[test]
    fn versions_for_unknown_package() {
        let repo = InMemoryRepository::new();
        let versions = repo.versions_for(&Cpn::new("dev-lang", "rust"));
        assert!(versions.is_empty());
    }
}
