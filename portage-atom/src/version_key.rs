//! An order-preserving byte key for a version

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::version::{Revision, Suffix, SuffixKind, Version};

const INLINE: usize = 21;

/// A [`Version`] as bytes that compare, hash and test equal the way the version does
///
/// Ordered per [PMS 3.3](https://projects.gentoo.org/pms/9/pms.html#version-comparison). A
/// 24-byte value where [`Version`] is 208, for code that copies and compares versions
/// constantly, such as a solver. [`Self::to_version`] gives back what was written.
#[derive(Clone)]
pub struct VersionKey(Repr);

// The first `order_len` bytes are the order key. The rest, present only when the version's
// spelling can't be rebuilt from the key (`003.03`, `1.00`, `_p01`), is its written form.
#[derive(Clone)]
enum Repr {
    Inline {
        order_len: u8,
        len: u8,
        buf: [u8; INLINE],
    },
    Heap {
        order_len: u32,
        bytes: Box<[u8]>,
    },
}

// Field order is significant: it is the byte order of the key.
const COMPONENT_END: u8 = 0;
const ZERO_LED: u8 = 1;
const NUMERIC: u8 = 2;
const SUFFIX_END: u8 = 5;

fn suffix_byte(kind: SuffixKind) -> u8 {
    match kind {
        SuffixKind::Alpha => 1,
        SuffixKind::Beta => 2,
        SuffixKind::Pre => 3,
        SuffixKind::Rc => 4,
        SuffixKind::P => 6,
    }
}

fn suffix_kind(byte: u8) -> Option<SuffixKind> {
    Some(match byte {
        1 => SuffixKind::Alpha,
        2 => SuffixKind::Beta,
        3 => SuffixKind::Pre,
        4 => SuffixKind::Rc,
        6 => SuffixKind::P,
        _ => return None,
    })
}

/// Length byte, then the big-endian bytes without leading zeros (`0` is just `[0]`)
fn push_number(out: &mut Vec<u8>, n: u64) {
    let bytes = n.to_be_bytes();
    let skip = (n.leading_zeros() / 8) as usize;
    out.push((8 - skip) as u8);
    out.extend_from_slice(&bytes[skip..]);
}

fn order_key(v: &Version, out: &mut Vec<u8>) {
    let components = v.numbers.len().max(v.digits.len());
    for i in 0..components {
        let n = v.numbers.get(i).copied().unwrap_or(0);
        let written = v.digits.get(i).map(|d| d.as_str());
        if i == 0 {
            push_number(out, n);
            continue;
        }
        // Algorithm 3.3: a later component starting with `0` compares as a string with trailing
        // zeros dropped, and every such string sorts below a number written without one.
        match written.filter(|d| d.starts_with('0')) {
            Some(d) => {
                out.push(ZERO_LED);
                out.extend_from_slice(d.trim_end_matches('0').as_bytes());
                out.push(0);
            }
            None if n == 0 => out.extend_from_slice(&[ZERO_LED, 0]),
            None => {
                out.push(NUMERIC);
                push_number(out, n);
            }
        }
    }
    out.push(COMPONENT_END);
    push_number(out, v.letter.map_or(0, u64::from));
    for suffix in &v.suffixes {
        out.push(suffix_byte(suffix.kind));
        push_number(out, suffix.version.unwrap_or(0));
    }
    out.push(SUFFIX_END);
    push_number(out, v.revision.0);
}

/// Whether rebuilding from the key would display differently from `v`
fn needs_spelling(v: &Version) -> bool {
    if !v.digits.is_empty() || v.suffixes.iter().any(|s| s.version == Some(0)) {
        return true;
    }
    // A suffix or revision number written with a leading zero (`_p01`, `-r0`).
    v.raw.as_ref().is_some_and(|raw| {
        raw.as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_lowercase() && w[1] == b'0')
    })
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn byte(&mut self) -> u8 {
        let (&b, rest) = self.0.split_first().unwrap_or((&0, &[]));
        self.0 = rest;
        b
    }

    fn number(&mut self) -> u64 {
        let len = usize::from(self.byte()).min(self.0.len());
        let (bytes, rest) = self.0.split_at(len);
        self.0 = rest;
        bytes.iter().fold(0, |n, &b| (n << 8) | u64::from(b))
    }
}

impl VersionKey {
    /// The key of `v`
    pub fn new(v: &Version) -> Self {
        let mut bytes = Vec::with_capacity(INLINE);
        order_key(v, &mut bytes);
        let order_len = bytes.len();
        if needs_spelling(v) {
            bytes.extend_from_slice(v.to_string().as_bytes());
        }
        Self::from_parts(order_len, bytes)
    }

    fn from_parts(order_len: usize, bytes: Vec<u8>) -> Self {
        if bytes.len() <= INLINE {
            let mut buf = [0; INLINE];
            buf[..bytes.len()].copy_from_slice(&bytes);
            Self(Repr::Inline {
                order_len: order_len as u8,
                len: bytes.len() as u8,
                buf,
            })
        } else {
            Self(Repr::Heap {
                order_len: u32::try_from(order_len).unwrap_or(u32::MAX),
                bytes: bytes.into_boxed_slice(),
            })
        }
    }

    fn bytes(&self) -> &[u8] {
        match &self.0 {
            Repr::Inline { len, buf, .. } => &buf[..usize::from(*len)],
            Repr::Heap { bytes, .. } => bytes,
        }
    }

    fn order(&self) -> &[u8] {
        let order_len = match &self.0 {
            Repr::Inline { order_len, .. } => usize::from(*order_len),
            Repr::Heap { order_len, .. } => *order_len as usize,
        };
        &self.bytes()[..order_len]
    }

    fn spelling(&self) -> Option<&str> {
        let tail = &self.bytes()[self.order().len()..];
        (!tail.is_empty())
            .then(|| std::str::from_utf8(tail).ok())
            .flatten()
    }

    /// The version this was built from, spelled as it was written
    pub fn to_version(&self) -> Version {
        if let Some(parsed) = self.spelling().and_then(|s| Version::parse(s).ok()) {
            return parsed;
        }
        let mut r = Reader(self.order());
        let mut numbers = vec![r.number()];
        loop {
            match r.byte() {
                NUMERIC => numbers.push(r.number()),
                ZERO_LED => {
                    let end = r.0.iter().position(|&b| b == 0).unwrap_or(r.0.len());
                    let digits = std::str::from_utf8(&r.0[..end]).unwrap_or("");
                    numbers.push(digits.parse().unwrap_or(0));
                    r.0 = r.0.get(end + 1..).unwrap_or(&[]);
                }
                _ => break,
            }
        }
        let mut v = Version::new(&numbers);
        v.letter = char::from_u32(r.number() as u32).filter(|&c| c != '\0');
        while let Some(kind) = suffix_kind(r.byte()) {
            let n = r.number();
            v.suffixes.push(Suffix {
                kind,
                version: (n != 0).then_some(n),
            });
        }
        v.revision = Revision(r.number());
        v
    }
}

impl From<&Version> for VersionKey {
    fn from(v: &Version) -> Self {
        Self::new(v)
    }
}

impl PartialEq for VersionKey {
    fn eq(&self, other: &Self) -> bool {
        self.order() == other.order()
    }
}

impl Eq for VersionKey {}

impl PartialOrd for VersionKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VersionKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order().cmp(other.order())
    }
}

impl Hash for VersionKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.order().hash(state);
    }
}

impl fmt::Display for VersionKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.spelling() {
            Some(s) => f.write_str(s),
            None => self.to_version().fmt(f),
        }
    }
}

impl fmt::Debug for VersionKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "VersionKey({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDGES: &[&str] = &[
        "0",
        "00",
        "1",
        "01",
        "001",
        "1.0",
        "1.00",
        "1.000",
        "1.01",
        "1.010",
        "1.001",
        "1.1",
        "1.09",
        "1.1.0",
        "1.10",
        "1.2",
        "1.2.0",
        "1.2.01",
        "1.2a",
        "1.2z",
        "1.2.0a",
        "1.2_alpha",
        "1.2_alpha1",
        "1.2_alpha01",
        "1.2_beta",
        "1.2_pre",
        "1.2_rc",
        "1.2_rc1",
        "1.2_rc1_p",
        "1.2_rc1_alpha",
        "1.2_p",
        "1.2_p0",
        "1.2_p1",
        "1.2_p1_rc1",
        "1.2_p1_p1",
        "1.2-r1",
        "1.2-r01",
        "1.2-r0",
        "1.2_p-r1",
        "1.2a_rc1_p2-r5",
        "003.03",
        "3.03",
        "019",
        "19",
        "26.04.0",
        "26.4.0",
        "9999",
        "1.9999",
        "18446744073709551615",
        "1.18446744073709551615",
        "2.0_pre20240101",
        "20240101",
        "0.0.0_pre20240101_p20240202-r12",
    ];

    fn edges() -> Vec<Version> {
        EDGES.iter().map(|s| Version::parse(s).unwrap()).collect()
    }

    #[test]
    fn key_order_and_equality_match_version_for_every_pair() {
        let versions = edges();
        let keys: Vec<_> = versions.iter().map(VersionKey::new).collect();
        for (a, ka) in versions.iter().zip(&keys) {
            for (b, kb) in versions.iter().zip(&keys) {
                assert_eq!(ka.cmp(kb), a.cmp(b), "{a} vs {b}");
            }
        }
    }

    #[test]
    fn every_version_displays_and_rebuilds_as_written() {
        for v in edges() {
            let key = VersionKey::new(&v);
            assert_eq!(key.to_string(), v.to_string());
            let back = key.to_version();
            assert_eq!(back.to_string(), v.to_string());
            assert_eq!(back.cmp(&v), Ordering::Equal, "{v}");
        }
    }

    #[test]
    fn spelling_stays_out_of_equality_and_hash() {
        use std::hash::BuildHasher;
        let state = std::collections::hash_map::RandomState::new();
        for (a, b) in [
            ("1.0", "1.00"),
            ("3.03", "003.03"),
            ("1.2_p", "1.2_p0"),
            ("1-r1", "1-r01"),
        ] {
            let (a, b) = (Version::parse(a).unwrap(), Version::parse(b).unwrap());
            let (ka, kb) = (VersionKey::new(&a), VersionKey::new(&b));
            assert_eq!(a, b);
            assert_eq!(ka, kb);
            assert_eq!(state.hash_one(&ka), state.hash_one(&kb));
            assert_ne!(ka.to_string(), kb.to_string());
        }
    }

    #[test]
    fn a_built_version_rebuilds_to_the_same_fields() {
        let mut v = Version::new(&[1, 0, 3]);
        v.letter = Some('b');
        v.suffixes.push(Suffix {
            kind: SuffixKind::Rc,
            version: Some(2),
        });
        v.revision = Revision(4);
        let back = VersionKey::new(&v).to_version();
        assert_eq!(back.numbers, v.numbers);
        assert_eq!(back.to_string(), "1.0.3b_rc2-r4");
    }

    #[test]
    fn common_versions_stay_inline() {
        assert_eq!(std::mem::size_of::<VersionKey>(), 24);
        for s in ["1.2.3", "5.15.2-r3", "2.0_pre20240101", "1.2.3_rc1-r1"] {
            let key = VersionKey::new(&Version::parse(s).unwrap());
            assert!(matches!(key.0, Repr::Inline { .. }), "{s}");
        }
    }
}
