//! How `em -s`/`-S` reads its search key, as `emerge -s` does.
//!
//! A leading `%` makes the key a regular expression; a leading `@`, or a `/` in the key, matches
//! `category/name` instead of the name. A key with regular-expression characters that compiles is
//! one too (`--regex-search-auto`). Any other key is literal text, matched without regard to case,
//! and also by similarity (`--fuzzy-search`, `--search-similarity`): the ratio of the characters
//! found in common blocks, Ratcliff/Obershelp as difflib computes it, to the characters in both.

use regex::{Regex, RegexBuilder};

/// What decides how a key is read
#[derive(Debug, Clone, Copy)]
pub struct KeyOptions {
    /// Names also match by similarity
    pub fuzzy: bool,
    /// The percentage of similarity that matches, 0 to 100
    pub similarity: f64,
    /// A key that looks like a regular expression is one
    pub regex_auto: bool,
}

impl Default for KeyOptions {
    fn default() -> Self {
        Self {
            fuzzy: true,
            similarity: 80.0,
            regex_auto: true,
        }
    }
}

/// A search key, ready to test names and descriptions
#[derive(Debug)]
pub struct SearchKey {
    pattern: Regex,
    category: bool,
    fuzzy: Option<Fuzzy>,
}

#[derive(Debug)]
struct Fuzzy {
    cutoff: f64,
    /// The key's lowercase characters, one entry for the name or two for `category/name`
    parts: Vec<Vec<char>>,
}

impl SearchKey {
    /// Read `key`; an explicit regular expression that does not compile is an error
    pub fn new(key: &str, options: KeyOptions) -> Result<Self, regex::Error> {
        let (mut explicit, mut category) = (false, false);
        let mut text = key;
        if let Some(rest) = text.strip_prefix('%') {
            explicit = true;
            text = rest;
        }
        if let Some(rest) = text.strip_prefix('@') {
            category = true;
            text = rest;
        }
        category |= text.contains('/');

        let build = |source: &str| RegexBuilder::new(source).case_insensitive(true).build();
        let auto = !explicit && options.regex_auto && looks_like_a_regex(text);
        let (pattern, regex) = match auto.then(|| build(text)) {
            Some(Ok(pattern)) => (pattern, true),
            _ if explicit => (build(text)?, true),
            _ => (build(&regex::escape(text))?, false),
        };
        let fuzzy = (!regex && options.fuzzy).then(|| Fuzzy {
            cutoff: options.similarity / 100.0,
            parts: split(text, category)
                .iter()
                .map(|part| part.to_lowercase().chars().collect())
                .collect(),
        });
        Ok(Self {
            pattern,
            category,
            fuzzy,
        })
    }

    /// Whether the package `name` of `category` matches
    pub fn matches_name(&self, category: &str, name: &str) -> bool {
        let full;
        let subject = if self.category {
            full = format!("{category}/{name}");
            full.as_str()
        } else {
            name
        };
        self.pattern.is_match(subject)
            || self
                .fuzzy
                .as_ref()
                .is_some_and(|f| f.matches(subject, self.category))
    }

    /// Whether the package set `name` matches: its last path segment, or all of it for a category key
    pub fn matches_set(&self, name: &str) -> bool {
        let subject = if self.category {
            name
        } else {
            name.rsplit('/').next().unwrap_or(name)
        };
        self.pattern.is_match(subject)
            || self
                .fuzzy
                .as_ref()
                .is_some_and(|f| f.matches(subject, self.category))
    }

    /// Whether a description matches; similarity does not apply to it
    pub fn matches_text(&self, text: &str) -> bool {
        self.pattern.is_match(text)
    }
}

impl Fuzzy {
    fn matches(&self, subject: &str, category: bool) -> bool {
        let parts = split(subject, category);
        self.parts.iter().zip(parts).all(|(key, part)| {
            let part: Vec<char> = part.to_lowercase().chars().collect();
            similar(&part, key, self.cutoff)
        })
    }
}

fn split(text: &str, category: bool) -> Vec<&str> {
    match text.split_once('/').filter(|_| category) {
        Some((category, name)) => vec![category, name],
        None => vec![text],
    }
}

fn looks_like_a_regex(text: &str) -> bool {
    text.contains(['^', '$', '*', '[', ']', '{', '}', '|', '?']) || text.contains(".+")
}

/// Whether the similarity of `a` to `b` reaches `cutoff`, once cheap upper bounds allow it
fn similar(a: &[char], b: &[char], cutoff: f64) -> bool {
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0 >= cutoff;
    }
    let ratio = |matches: usize| 2.0 * matches as f64 / total as f64;
    if ratio(a.len().min(b.len())) < cutoff {
        return false;
    }
    let mut counts = std::collections::HashMap::new();
    for c in b {
        *counts.entry(*c).or_insert(0usize) += 1;
    }
    let mut shared = 0;
    for c in a {
        if let Some(left) = counts.get_mut(c).filter(|left| **left > 0) {
            *left -= 1;
            shared += 1;
        }
    }
    ratio(shared) >= cutoff && ratio(matching(a, b)) >= cutoff
}

/// The characters in the common blocks of `a` and `b`: the longest common block, earliest in `a`
/// and then in `b`, then the same of what lies left and right of it
fn matching(a: &[char], b: &[char]) -> usize {
    let mut total = 0;
    let mut pending = vec![(0, a.len(), 0, b.len())];
    while let Some((alo, ahi, blo, bhi)) = pending.pop() {
        let (i, j, size) = longest(a, b, (alo, ahi), (blo, bhi));
        if size == 0 {
            continue;
        }
        total += size;
        if alo < i && blo < j {
            pending.push((alo, i, blo, j));
        }
        if i + size < ahi && j + size < bhi {
            pending.push((i + size, ahi, j + size, bhi));
        }
    }
    total
}

/// The longest block `a[i..i + size] == b[j..j + size]` within the ranges, first found scanning
/// `a` and then `b` upwards
fn longest(
    a: &[char],
    b: &[char],
    arange: (usize, usize),
    brange: (usize, usize),
) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut best) = (arange.0, brange.0, 0);
    let mut previous = vec![0usize; b.len() + 1];
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate().take(arange.1).skip(arange.0) {
        current.fill(0);
        for (j, cb) in b.iter().enumerate().take(brange.1).skip(brange.0) {
            if ca == cb {
                let size = previous[j] + 1;
                current[j + 1] = size;
                if size > best {
                    (besti, bestj, best) = (i + 1 - size, j + 1 - size, size);
                }
            }
        }
        std::mem::swap(&mut previous, &mut current);
    }
    (besti, bestj, best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    fn ratio(a: &str, b: &str) -> f64 {
        let (a, b) = (chars(a), chars(b));
        2.0 * matching(&a, &b) as f64 / (a.len() + b.len()) as f64
    }

    fn key(text: &str) -> SearchKey {
        SearchKey::new(text, KeyOptions::default()).unwrap()
    }

    #[test]
    fn the_ratio_is_difflibs() {
        // Values difflib.SequenceMatcher(None, a, b).ratio() gives.
        assert_eq!(ratio("zlib", "zenlib"), 0.8);
        assert_eq!(ratio("ripgrep", "igrep"), 10.0 / 12.0);
        assert_eq!(ratio("cython", "python"), 10.0 / 12.0);
        assert_eq!(ratio("abcde", "bcdea"), 0.8);
        assert_eq!(ratio("dev-python", "dev-pythno"), 0.9);
        assert!((ratio("probe", "rush") - 2.0 / 9.0).abs() < 1e-12);
    }

    #[test]
    fn a_literal_key_matches_a_name_by_text_or_by_similarity() {
        let zlib = key("zlib");
        assert!(zlib.matches_name("sys-libs", "ZLIB-ng"));
        assert!(zlib.matches_name("dev-python", "zenlib"));
        assert!(!zlib.matches_name("app-misc", "rush"));
        let strict = SearchKey::new(
            "zlib",
            KeyOptions {
                fuzzy: false,
                ..KeyOptions::default()
            },
        )
        .unwrap();
        assert!(!strict.matches_name("dev-python", "zenlib"));
        let exact = SearchKey::new(
            "zlib",
            KeyOptions {
                similarity: 90.0,
                ..KeyOptions::default()
            },
        )
        .unwrap();
        assert!(!exact.matches_name("dev-python", "zenlib"));
    }

    #[test]
    fn a_key_that_looks_like_a_regex_is_one() {
        assert!(key("^probe$").matches_name("sci-chemistry", "probe"));
        assert!(!key("^probe$").matches_name("sci-chemistry", "probe-ng"));
        assert!(key("rus+h").matches_name("net-misc", "rush"));
        assert!(key("%^py.*magic$").matches_name("dev-python", "python-magic"));
        // `+` alone is text, so is a key that does not compile.
        assert!(key("c++").matches_name("dev-lang", "c++"));
        assert!(key("[").matches_name("app-misc", "a[b"));
        let plain = SearchKey::new(
            "^probe$",
            KeyOptions {
                regex_auto: false,
                fuzzy: false,
                ..KeyOptions::default()
            },
        )
        .unwrap();
        assert!(!plain.matches_name("sci-chemistry", "probe"));
        assert!(SearchKey::new("%[", KeyOptions::default()).is_err());
    }

    #[test]
    fn a_slash_or_an_at_matches_the_category_too() {
        let full = key("dev-python/python-magic");
        assert!(full.matches_name("dev-python", "python-magic"));
        assert!(!full.matches_name("app-misc", "python-magic"));
        // Each part has to be similar, so a short name does not match a long category.
        assert!(!key("@dev-libs/tk").matches_name("dev-libs", "zlib"));
    }

    #[test]
    fn a_set_matches_by_its_last_segment_unless_the_key_names_a_category() {
        assert!(key("^world$").matches_set("world"));
        assert!(key("sel").matches_set("selected-sets"));
        assert!(key("^sub$").matches_set("my/sub"));
        assert!(!key("@my/sub").matches_set("other/sub"));
    }

    #[test]
    fn a_description_matches_by_text_only() {
        assert!(key("packing").matches_text("Evaluates atomic PACKING"));
        assert!(!key("packin").matches_text("pakcing"));
    }
}
