use std::sync::{Arc, OnceLock};

/// A lazily-parsed value: raw source text stored as-is, parsed to `T` on
/// first [`Self::get`] call and memoized
///
/// Building block for md5-cache fields (`LazyDepList`, `LazySrcUriList`)
/// that only a fraction of a repo's ebuilds ever need parsed. `Err`'s
/// payload is `(T, String)` — the `T::default()` fallback plus the real
/// error's formatted text, kept via [`Self::parse_error`] since it costs
/// nothing once the error is already being turned into a `String` anyway.
#[derive(Debug)]
pub struct Lazy<T> {
    raw: Option<Arc<str>>,
    parsed: OnceLock<Result<T, (T, String)>>,
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
        let _ = parsed.set(Ok(value));
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
    ///
    /// A `parse` error falls back to `T::default()`, recorded so
    /// [`Self::parse_failed`]/[`Self::parse_error`] can still report it —
    /// nothing here needs to *act* differently on failure, only to be able
    /// to tell it apart from a genuinely empty field afterward.
    pub fn get<E: std::fmt::Display>(&self, parse: impl FnOnce(&str) -> Result<T, E>) -> &T
    where
        T: Default,
    {
        let r = self.parsed.get_or_init(|| match &self.raw {
            Some(s) => parse(s).map_err(|e| (T::default(), e.to_string())),
            None => Ok(T::default()),
        });
        match r {
            Ok(v) => v,
            Err((v, _)) => v,
        }
    }

    /// Force the parse via `parse`, then give mutable access
    ///
    /// Drops the raw text: after mutation there is no source left to
    /// re-derive from. [`Self::parse_failed`] still reports the original
    /// text's outcome, even though the value has since been mutated.
    pub fn get_mut<E: std::fmt::Display>(
        &mut self,
        parse: impl FnOnce(&str) -> Result<T, E>,
    ) -> &mut T
    where
        T: Default,
    {
        self.get(parse);
        self.raw = None;
        match self.parsed.get_mut().expect("just initialized by get()") {
            Ok(v) => v,
            Err((v, _)) => v,
        }
    }

    /// Whether the raw text failed to parse cleanly
    ///
    /// `false` while unforced — nothing has looked yet, so there is nothing
    /// to report — and `false` for genuinely empty input. Never forces a
    /// parse itself; call after [`Self::get`]/[`Self::get_mut`] for a real
    /// answer.
    pub fn parse_failed(&self) -> bool {
        matches!(self.parsed.get(), Some(Err(_)))
    }

    /// The parse error's formatted text, if [`Self::parse_failed`] would
    /// report `true`
    ///
    /// Never forces a parse itself, same as `parse_failed`.
    pub fn parse_error(&self) -> Option<&str> {
        match self.parsed.get() {
            Some(Err((_, msg))) => Some(msg),
            _ => None,
        }
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
        if let Some(r) = self.parsed.get() {
            let _ = parsed.set(r.clone());
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
        assert_eq!(*lazy.get(|s| s.parse::<i32>()), 42);
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
        assert_eq!(*lazy.get(|s| s.parse::<i32>()), 0);
    }

    #[test]
    fn from_value_never_reparses() {
        let lazy = Lazy::from_value(7);
        assert_eq!(
            *lazy
                .get(|_| -> Result<i32, std::convert::Infallible> { panic!("must not be called") }),
            7
        );
    }

    #[test]
    fn clone_preserves_memoized_state() {
        let lazy = Lazy::from_raw("42");
        lazy.get(|s| s.parse::<i32>());
        let cloned = lazy.clone();
        assert!(cloned.parsed.get().is_some());
        assert_eq!(
            *cloned
                .get(|_| -> Result<i32, std::convert::Infallible> { panic!("must not be called") }),
            *lazy
                .get(|_| -> Result<i32, std::convert::Infallible> { panic!("must not be called") })
        );
    }

    #[test]
    fn get_mut_forces_and_drops_raw() {
        let mut lazy = Lazy::from_raw("42");
        *lazy.get_mut(|s| s.parse::<i32>()) += 1;
        assert!(lazy.is_empty_raw());
        assert_eq!(
            *lazy
                .get(|_| -> Result<i32, std::convert::Infallible> { panic!("must not be called") }),
            43
        );
    }

    #[test]
    fn parse_failed_is_false_until_forced() {
        let lazy = Lazy::from_raw("not a number");
        assert!(!lazy.parse_failed(), "must not force a parse to answer");
        lazy.get(|s| s.parse::<i32>());
        assert!(lazy.parse_failed());
    }

    #[test]
    fn parse_failed_is_false_for_genuinely_empty_input() {
        let lazy: Lazy<i32> = Lazy::from_raw("");
        lazy.get(|s| s.parse::<i32>());
        assert!(!lazy.parse_failed());
    }

    #[test]
    fn parse_failed_is_false_for_a_clean_parse() {
        let lazy = Lazy::from_raw("42");
        lazy.get(|s| s.parse::<i32>());
        assert!(!lazy.parse_failed());
    }

    #[test]
    fn parse_error_carries_the_real_error_text() {
        let lazy = Lazy::from_raw("not a number");
        assert!(lazy.parse_error().is_none(), "must not force a parse");
        lazy.get(|s| s.parse::<i32>());
        let msg = lazy
            .parse_error()
            .expect("parse failed, must have a message");
        assert!(
            msg.contains("invalid digit"),
            "expected the real parse error text, got: {msg}"
        );
    }

    #[test]
    fn parse_error_is_none_for_a_clean_parse() {
        let lazy = Lazy::from_raw("42");
        lazy.get(|s| s.parse::<i32>());
        assert!(lazy.parse_error().is_none());
    }
}
