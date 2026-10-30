use portage_atom::Version;

/// A package's versions in ascending order, stored as one sorted `Vec`
///
/// Most provider packages are virtual choice nodes with two or three branches,
/// which a `Vec` holds without a `BTreeMap`'s per-node allocation.
pub(crate) struct VersionMap<V>(Vec<(Version, V)>);

impl<V> VersionMap<V> {
    pub(crate) const fn new() -> Self {
        Self(Vec::new())
    }

    fn position(&self, version: &Version) -> Result<usize, usize> {
        self.0.binary_search_by(|(v, _)| v.cmp(version))
    }

    pub(crate) fn get(&self, version: &Version) -> Option<&V> {
        self.position(version).ok().map(|i| &self.0[i].1)
    }

    pub(crate) fn get_mut(&mut self, version: &Version) -> Option<&mut V> {
        self.position(version).ok().map(|i| &mut self.0[i].1)
    }

    pub(crate) fn contains_key(&self, version: &Version) -> bool {
        self.position(version).is_ok()
    }

    /// Insert or replace, returning the replaced value
    pub(crate) fn insert(&mut self, version: Version, value: V) -> Option<V> {
        match self.position(&version) {
            Ok(i) => Some(std::mem::replace(&mut self.0[i].1, value)),
            Err(i) => {
                self.0.insert(i, (version, value));
                None
            }
        }
    }

    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = (&Version, &V)> {
        self.0.iter().map(|(v, d)| (v, d))
    }

    pub(crate) fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = (&Version, &mut V)> {
        self.0.iter_mut().map(|(v, d)| (&*v, d))
    }

    pub(crate) fn keys(&self) -> impl DoubleEndedIterator<Item = &Version> {
        self.0.iter().map(|(v, _)| v)
    }

    pub(crate) fn values(&self) -> impl DoubleEndedIterator<Item = &V> {
        self.0.iter().map(|(_, d)| d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn out_of_order_inserts_iterate_ascending_and_replace_in_place() {
        let mut map = VersionMap::new();
        for (ver, val) in [("2", 'b'), ("1.10", 'c'), ("1.9", 'a'), ("10", 'd')] {
            assert_eq!(map.insert(v(ver), val), None);
        }
        assert_eq!(map.insert(v("1.10"), 'C'), Some('c'));
        let order: Vec<_> = map.iter().map(|(k, &d)| (k.to_string(), d)).collect();
        assert_eq!(
            order,
            [
                ("1.9".into(), 'a'),
                ("1.10".into(), 'C'),
                ("2".into(), 'b'),
                ("10".into(), 'd')
            ]
        );
        assert_eq!(map.get(&v("2")), Some(&'b'));
        assert!(!map.contains_key(&v("3")));
    }
}
