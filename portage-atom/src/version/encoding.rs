//! The bytes behind a [`Version`](super::Version): an order key, then its spelling when needed

use smallvec::SmallVec;

use super::{Numbers, Revision, Suffix, SuffixKind, VersionParts};

const INLINE: usize = 21;

// The first `order_len` bytes are the order key: comparing them as bytes is comparing the versions
// (PMS Algorithms 3.1-3.7). The rest, present only when the version's spelling can't be rebuilt
// from the key (`003.03`, `1.00`, `_p01`), is its written form.
#[derive(Clone)]
pub(super) enum Repr {
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
pub(super) const ZERO_LED: u8 = 1;
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

fn order_key(parts: &VersionParts, out: &mut Vec<u8>) {
    for (i, &n) in parts.numbers.iter().enumerate() {
        if i == 0 {
            push_number(out, n);
            continue;
        }
        // Algorithm 3.3: a later component starting with `0` compares as a string with trailing
        // zeros dropped, and every such string sorts below a number written without one.
        let zeros = parts.leading_zeros.get(i).copied().unwrap_or(0);
        if zeros > 0 || n == 0 {
            out.push(ZERO_LED);
            if n != 0 {
                out.extend(std::iter::repeat_n(b'0', zeros as usize));
                out.extend_from_slice(n.to_string().trim_end_matches('0').as_bytes());
            }
            out.push(0);
        } else {
            out.push(NUMERIC);
            push_number(out, n);
        }
    }
    out.push(COMPONENT_END);
    push_number(out, parts.letter.map_or(0, u64::from));
    for suffix in &parts.suffixes {
        out.push(suffix_byte(suffix.kind));
        push_number(out, suffix.version.unwrap_or(0));
    }
    out.push(SUFFIX_END);
    push_number(out, parts.revision.0);
}

/// Whether rebuilding from the key would display differently from `spelling` (or from `parts`)
fn needs_spelling(parts: &VersionParts, spelling: Option<&str>) -> bool {
    if parts.leading_zeros.iter().any(|&z| z > 0)
        || parts.suffixes.iter().any(|s| s.version == Some(0))
    {
        return true;
    }
    // A suffix or revision number written with a leading zero (`_p01`, `-r0`).
    spelling.is_some_and(|raw| {
        raw.as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_lowercase() && w[1] == b'0')
    })
}

/// The canonical spelling of `parts`
pub(super) fn render(parts: &VersionParts) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for (i, &n) in parts.numbers.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        let zeros = parts.leading_zeros.get(i).copied().unwrap_or(0);
        out.extend(std::iter::repeat_n('0', zeros as usize));
        let _ = write!(out, "{n}");
    }
    if let Some(letter) = parts.letter {
        out.push(letter);
    }
    for suffix in &parts.suffixes {
        let _ = write!(out, "{suffix}");
    }
    let _ = write!(out, "{}", parts.revision);
    out
}

impl Repr {
    pub(super) fn encode(parts: &VersionParts, spelling: Option<&str>) -> Self {
        let mut bytes = Vec::with_capacity(INLINE);
        order_key(parts, &mut bytes);
        let order_len = bytes.len();
        if needs_spelling(parts, spelling) {
            match spelling {
                Some(s) => bytes.extend_from_slice(s.as_bytes()),
                None => bytes.extend_from_slice(render(parts).as_bytes()),
            }
        }
        if bytes.len() <= INLINE {
            let mut buf = [0; INLINE];
            buf[..bytes.len()].copy_from_slice(&bytes);
            Repr::Inline {
                order_len: order_len as u8,
                len: bytes.len() as u8,
                buf,
            }
        } else {
            Repr::Heap {
                order_len: u32::try_from(order_len).unwrap_or(u32::MAX),
                bytes: bytes.into_boxed_slice(),
            }
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Repr::Inline { len, buf, .. } => &buf[..usize::from(*len)],
            Repr::Heap { bytes, .. } => bytes,
        }
    }

    pub(super) fn order(&self) -> &[u8] {
        let order_len = match self {
            Repr::Inline { order_len, .. } => usize::from(*order_len),
            Repr::Heap { order_len, .. } => *order_len as usize,
        };
        &self.bytes()[..order_len]
    }

    pub(super) fn spelling(&self) -> Option<&str> {
        let tail = &self.bytes()[self.order().len()..];
        (!tail.is_empty())
            .then(|| std::str::from_utf8(tail).ok())
            .flatten()
    }
}

/// The order key split into its numeric components and what follows them
pub(super) struct Layout<'a> {
    /// Each component's bytes: equal bytes are equal components (Algorithms 3.2-3.3)
    pub(super) components: SmallVec<[&'a [u8]; 6]>,
    /// Letter, suffixes and revision
    rest: &'a [u8],
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn byte(&mut self) -> u8 {
        let (&b, rest) = self.0.split_first().unwrap_or((&0, &[]));
        self.0 = rest;
        b
    }

    fn take(&mut self, n: usize) -> &'a [u8] {
        let (head, rest) = self.0.split_at(n.min(self.0.len()));
        self.0 = rest;
        head
    }

    fn number(&mut self) -> u64 {
        let len = usize::from(self.byte());
        self.take(len)
            .iter()
            .fold(0, |n, &b| (n << 8) | u64::from(b))
    }

    fn number_bytes(&self) -> usize {
        1 + usize::from(self.0.first().copied().unwrap_or(0))
    }
}

impl<'a> Layout<'a> {
    pub(super) fn of(order: &'a [u8]) -> Self {
        let mut r = Reader(order);
        let mut components = SmallVec::new();
        let first = r.number_bytes();
        components.push(r.take(first));
        loop {
            let len = match r.0.first() {
                Some(&NUMERIC) => 1 + Reader(&r.0[1..]).number_bytes(),
                Some(&ZERO_LED) => {
                    2 + r.0[1..]
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(r.0.len() - 1)
                }
                _ => break,
            };
            components.push(r.take(len));
        }
        r.byte();
        Layout {
            components,
            rest: r.0,
        }
    }

    /// The value of component `segment`, with the zeros written before it
    pub(super) fn component(segment: &[u8], first: bool) -> (u64, u32) {
        if first {
            return (Reader(segment).number(), 0);
        }
        match segment.split_first() {
            Some((&NUMERIC, rest)) => (Reader(rest).number(), 0),
            Some((_, rest)) => {
                let digits =
                    std::str::from_utf8(&rest[..rest.len().saturating_sub(1)]).unwrap_or("");
                let value = digits.trim_start_matches('0');
                let zeros = (digits.len() - value.len()) as u32;
                (value.parse().unwrap_or(0), zeros)
            }
            None => (0, 0),
        }
    }

    /// Letter, suffixes, revision, and how many key bytes precede the revision
    pub(super) fn tail(&self, order_len: usize) -> (Option<char>, Vec<Suffix>, Revision, usize) {
        let mut r = Reader(self.rest);
        let letter = char::from_u32(r.number() as u32).filter(|&c| c != '\0');
        let mut suffixes = Vec::new();
        while let Some(kind) = suffix_kind(r.byte()) {
            let n = r.number();
            suffixes.push(Suffix {
                kind,
                version: (n != 0).then_some(n),
            });
        }
        let before_revision = order_len - r.0.len();
        (letter, suffixes, Revision(r.number()), before_revision)
    }

    /// The parts this key encodes, as they would be rebuilt without a spelling
    pub(super) fn parts(&self, order_len: usize) -> VersionParts {
        let mut numbers = Numbers::new();
        let mut leading_zeros = Vec::new();
        for (i, segment) in self.components.iter().enumerate() {
            let (n, zeros) = Self::component(segment, i == 0);
            numbers.push(n);
            if zeros > 0 {
                leading_zeros.resize(i, 0);
                leading_zeros.push(zeros);
            }
        }
        let (letter, suffixes, revision, _) = self.tail(order_len);
        VersionParts {
            numbers,
            leading_zeros,
            letter,
            suffixes,
            revision,
        }
    }
}
