use std::sync::{Arc, OnceLock};

/// A lazily-parsed value: raw source text stored as-is, parsed to `T` on
/// first [`Self::get`] call and memoized
///
/// Building block for md5-cache fields (`LazyDepList`, `LazySrcUriList`)
/// that only a fraction of a repo's ebuilds ever need parsed — eagerly
/// parsing every one of them for the whole tree on `load_repos()` is often
/// pure waste. Callers own the parse-failure tradeoff (typically: parse to
/// `T::default()` rather than propagate an error) via the closure passed
/// to [`Self::get`], not this type.
#[derive(Debug)]
pub struct Lazy<T> {
    raw: Option<Arc<str>>,
    parsed: OnceLock<T>,
}

impl<T> Lazy<T> {
    /// Wrap raw source text, parsed on first [`Self::get`] call
    pub fn from_raw(s: &str) -> Self {
        if s.is_empty() {
            return Self::empty();
        }
        Self {
            raw: Some(Arc::from(s)),
            parsed: OnceLock::new(),
        }
    }

    /// Wrap an already-computed value — no raw text, so [`Self::get`]
    /// never has anything to parse
    pub fn from_value(value: T) -> Self {
        let parsed = OnceLock::new();
        let _ = parsed.set(value);
        Self { raw: None, parsed }
    }

    /// No raw text and no value
    pub fn empty() -> Self {
        Self {
            raw: None,
            parsed: OnceLock::new(),
        }
    }

    /// Whether this holds no raw text, without forcing a parse
    ///
    /// A value built via [`Self::from_value`] is never "empty raw" even if
    /// the value itself is empty — there is no raw text to report on. Check
    /// the parsed value directly (`get(..).is_empty()`) if that distinction
    /// matters and forcing the parse is acceptable.
    pub fn is_empty_raw(&self) -> bool {
        self.raw.is_none()
    }

    /// The parsed value, computed via `parse` and memoized on first call
    pub fn get(&self, parse: impl FnOnce(&str) -> T) -> &T
    where
        T: Default,
    {
        self.parsed.get_or_init(|| match &self.raw {
            Some(s) => parse(s),
            None => T::default(),
        })
    }

    /// Force the parse via `parse`, then give mutable access
    ///
    /// Drops the raw text: after mutation there is no source left to
    /// re-derive from.
    pub fn get_mut(&mut self, parse: impl FnOnce(&str) -> T) -> &mut T
    where
        T: Default,
    {
        self.get(parse);
        self.raw = None;
        self.parsed.get_mut().expect("just initialized by get()")
    }
}

impl<T> Default for Lazy<T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T: Clone> Clone for Lazy<T> {
    fn clone(&self) -> Self {
        let parsed = OnceLock::new();
        if let Some(v) = self.parsed.get() {
            let _ = parsed.set(v.clone());
        }
        Self {
            raw: self.raw.clone(),
            parsed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_on_first_access_only() {
        let lazy = Lazy::from_raw("42");
        assert!(!lazy.is_empty_raw());
        assert!(lazy.parsed.get().is_none(), "must not parse eagerly");
        assert_eq!(*lazy.get(|s| s.parse::<i32>().unwrap_or_default()), 42);
        assert!(
            lazy.parsed.get().is_some(),
            "must memoize after first access"
        );
    }

    #[test]
    fn empty_raw_never_forces() {
        let lazy: Lazy<i32> = Lazy::from_raw("");
        assert!(lazy.is_empty_raw());
        assert!(lazy.parsed.get().is_none());
    }

    #[test]
    fn malformed_input_falls_back_to_default() {
        let lazy = Lazy::from_raw("not a number");
        assert_eq!(*lazy.get(|s| s.parse::<i32>().unwrap_or_default()), 0);
    }

    #[test]
    fn from_value_never_reparses() {
        let lazy = Lazy::from_value(7);
        assert_eq!(*lazy.get(|_| panic!("must not be called")), 7);
    }

    #[test]
    fn clone_preserves_memoized_state() {
        let lazy = Lazy::from_raw("42");
        lazy.get(|s| s.parse::<i32>().unwrap_or_default());
        let cloned = lazy.clone();
        assert!(cloned.parsed.get().is_some());
        assert_eq!(
            *cloned.get(|_| panic!("must not be called")),
            *lazy.get(|_| panic!("must not be called"))
        );
    }

    #[test]
    fn get_mut_forces_and_drops_raw() {
        let mut lazy = Lazy::from_raw("42");
        *lazy.get_mut(|s| s.parse::<i32>().unwrap_or_default()) += 1;
        assert!(lazy.is_empty_raw());
        assert_eq!(*lazy.get(|_| panic!("must not be called")), 43);
    }
}
