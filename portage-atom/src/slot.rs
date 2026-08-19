use std::fmt;
use std::str::FromStr;

use gentoo_interner::{DefaultInterner, Interned};
use winnow::combinator::{alt, opt, preceded};
use winnow::error::StrContext;
use winnow::prelude::*;

use crate::error::{Error, Result};

/// Slot operator for sub-slot rebuilds
///
/// See [PMS 8.3.3](https://projects.gentoo.org/pms/9/pms.html#slot-dependencies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SlotOperator {
    /// `:=` — the dependent package must be rebuilt when the dependency's
    /// slot or sub-slot changes
    Equal,
    /// `:*` — accept any slot; no rebuild is triggered on slot changes
    Star,
}

impl fmt::Display for SlotOperator {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SlotOperator::Equal => write!(f, "="),
            SlotOperator::Star => write!(f, "*"),
        }
    }
}

/// Slot name and optional sub-slot
///
/// Slots allow multiple versions of a package to be installed simultaneously
/// (e.g. `dev-lang/python:3.11` and `dev-lang/python:3.12`). Sub-slots
/// track ABI compatibility; a sub-slot change signals that reverse
/// dependencies using `:=` must be rebuilt.
///
/// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "builder", derive(bon::Builder))]
pub struct Slot {
    /// The slot name (e.g. `0`, `3.12`, `stable`)
    #[cfg_attr(feature = "builder", builder(into))]
    pub slot: Interned<DefaultInterner>,
    /// Optional sub-slot for ABI tracking (e.g. `1.2` in `:0/1.2`)
    #[cfg_attr(feature = "builder", builder(into))]
    pub subslot: Option<Interned<DefaultInterner>>,
}

impl Slot {
    /// Create a new slot without a sub-slot
    ///
    /// The value is interned automatically.
    pub fn new(slot: impl AsRef<str>) -> Self {
        Slot {
            slot: Interned::intern(slot.as_ref()),
            subslot: None,
        }
    }

    /// Create a new slot with a sub-slot
    ///
    /// Both values are interned automatically.
    pub fn with_subslot(slot: impl AsRef<str>, subslot: impl AsRef<str>) -> Self {
        Slot {
            slot: Interned::intern(slot.as_ref()),
            subslot: Some(Interned::intern(subslot.as_ref())),
        }
    }

    /// Split a `SLOT` value (`0`, `0/5.1`) into its name and sub-slot
    ///
    /// The form ebuilds and the VDB record. Only the first `/` separates —
    /// PMS forbids one in either part, so the rest belongs to the sub-slot.
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(Error::InvalidSlot("empty SLOT".to_string()));
        }
        Ok(match raw.split_once('/') {
            Some((name, sub)) if !name.is_empty() && !sub.is_empty() => {
                Self::with_subslot(name, sub)
            }
            Some(_) => return Err(Error::InvalidSlot(raw.to_string())),
            None => Self::new(raw),
        })
    }

    /// A slot known only by name, from a handle that is already interned
    ///
    /// For callers that identify a package by `(cpn, slot)` and never see its
    /// sub-slot — the solver, whose package identity is exactly that. Their
    /// `None` sub-slot means *unrecorded*, not *absent*, which is why
    /// [`crate::Dep::matches_cpv`] only enforces a sub-slot constraint when
    /// both sides carry one.
    ///
    /// Takes the interned handle rather than a `&str` so it does not resolve
    /// and re-intern a value the caller already holds.
    pub fn from_name(slot: Interned<DefaultInterner>) -> Self {
        Slot {
            slot,
            subslot: None,
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.slot)?;
        if let Some(subslot) = self.subslot {
            write!(f, "/{}", subslot)?;
        }
        Ok(())
    }
}

/// Slot dependency with optional operator
///
/// Represents the slot constraint portion of a dependency atom
/// (everything after the `:`), e.g. `:0`, `:0/2.1`, `:0=`, `:=`, `:*`.
///
/// See [PMS 8.3.3](https://projects.gentoo.org/pms/9/pms.html#slot-dependencies).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SlotDep {
    /// A named slot with optional sub-slot and optional operator,
    /// e.g. `0`, `0/1.2`, `0=`
    Slot {
        /// The slot and optional sub-slot (e.g. `0`, `0/1.2`)
        ///
        /// `None` only when a bare operator is present (e.g. `:=`).
        slot: Option<Slot>,
        /// The slot operator (`=` for rebuild-on-change, `*` for any-slot)
        ///
        /// See [PMS 8.3.3].
        ///
        /// [PMS 8.3.3]: https://projects.gentoo.org/pms/9/pms.html#slot-dependencies
        op: Option<SlotOperator>,
    },
    /// A bare operator without a named slot (`:=` or `:*`)
    Operator(SlotOperator),
}

impl SlotDep {
    /// Parse the slot dependency portion of an atom (without the leading `:`)
    ///
    /// Accepts forms like `0`, `0/1.2`, `0=`, `=`, `*`.
    pub fn parse(input: &str) -> Result<Self> {
        parse_slot_dep
            .parse(input)
            .map_err(|e| Error::InvalidSlot(format!("{}: {}", input, e)))
    }
}

impl fmt::Display for SlotDep {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SlotDep::Slot { slot, op } => {
                if let Some(s) = slot {
                    write!(f, "{}", s)?;
                }
                if let Some(o) = op {
                    write!(f, "{}", o)?;
                }
                Ok(())
            }
            SlotDep::Operator(op) => write!(f, "{}", op),
        }
    }
}

impl FromStr for SlotDep {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

// Winnow parsers

/// Parse slot name (alphanumeric, _, -, +, .)
///
/// PMS 3.1.3: must not begin with hyphen, dot, or plus.
fn parse_slot_name(input: &mut &str) -> ModalResult<Interned<DefaultInterner>> {
    use crate::parsers::parse_ident_with_dot;

    parse_ident_with_dot
        .verify(|s: &str| {
            let first_char = s.chars().next().unwrap();
            !matches!(first_char, '-' | '.' | '+')
        })
        .map(|s: &str| Interned::intern(s))
        .parse_next(input)
}

/// Parse slot with optional subslot
fn parse_slot(input: &mut &str) -> ModalResult<Slot> {
    (parse_slot_name, opt(preceded('/', parse_slot_name)))
        .map(|(slot, subslot)| Slot { slot, subslot })
        .parse_next(input)
}

/// Parse slot operator
fn parse_slot_operator(input: &mut &str) -> ModalResult<SlotOperator> {
    alt((
        '='.value(SlotOperator::Equal),
        '*'.value(SlotOperator::Star),
    ))
    .parse_next(input)
}

/// Parse slot dependency (after the : has been consumed)
pub(crate) fn parse_slot_dep(input: &mut &str) -> ModalResult<SlotDep> {
    alt((
        // Just operator: := or :*
        parse_slot_operator.map(SlotDep::Operator),
        // Slot with optional operator: :0, :0=, :0/1.2
        (parse_slot, opt(parse_slot_operator)).map(|(slot, op)| SlotDep::Slot {
            slot: Some(slot),
            op,
        }),
    ))
    .context(StrContext::Label("slot"))
    .parse_next(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slot_parsing() {
        let slot = SlotDep::parse("0").unwrap();
        match slot {
            SlotDep::Slot {
                slot: Some(s),
                op: None,
            } => {
                assert_eq!(s.slot, "0");
                assert_eq!(s.subslot, None);
            }
            _ => panic!("unexpected slot dep"),
        }
    }

    #[test]
    fn test_slot_with_subslot() {
        let slot = SlotDep::parse("0/2.1").unwrap();
        match slot {
            SlotDep::Slot {
                slot: Some(s),
                op: None,
            } => {
                assert_eq!(s.slot, "0");
                assert_eq!(s.subslot, Some(Interned::intern("2.1")));
            }
            _ => panic!("unexpected slot dep"),
        }
    }

    #[test]
    fn test_slot_operators() {
        let slot = SlotDep::parse("=").unwrap();
        assert_eq!(slot, SlotDep::Operator(SlotOperator::Equal));

        let slot = SlotDep::parse("*").unwrap();
        assert_eq!(slot, SlotDep::Operator(SlotOperator::Star));

        let slot = SlotDep::parse("0=").unwrap();
        match slot {
            SlotDep::Slot {
                slot: Some(s),
                op: Some(SlotOperator::Equal),
            } => {
                assert_eq!(s.slot, "0");
            }
            _ => panic!("unexpected slot dep"),
        }
    }

    #[test]
    fn test_slot_name_validation() {
        // PMS 3.1.3: "A slot name may contain any of the characters [A-Za-z0-9+_.-].
        // It must not begin with a hyphen, a dot or a plus sign."

        // Valid slot names
        assert!(SlotDep::parse("0").is_ok());
        assert!(SlotDep::parse("3.12").is_ok());
        assert!(SlotDep::parse("stable").is_ok());
        assert!(SlotDep::parse("slot+name").is_ok());

        // Invalid: starts with forbidden characters
        assert!(SlotDep::parse("-slot").is_err());
        assert!(SlotDep::parse(".slot").is_err());
        assert!(SlotDep::parse("+slot").is_err());
    }

    #[test]
    #[cfg(feature = "builder")]
    fn test_slot_builder() {
        let slot = Slot::builder().slot("0").subslot("1.75").build();
        assert_eq!(slot.slot, "0");
        assert_eq!(slot.subslot, Some(Interned::intern("1.75")));
        assert_eq!(slot.to_string(), "0/1.75");
    }

    // --- PMS 8.3.3 compliance tests ---

    #[test]
    fn test_bare_slot_operators() {
        // := and :* (no named slot)
        let dep = SlotDep::parse("=").unwrap();
        assert_eq!(dep, SlotDep::Operator(SlotOperator::Equal));

        let dep = SlotDep::parse("*").unwrap();
        assert_eq!(dep, SlotDep::Operator(SlotOperator::Star));
    }

    #[test]
    fn test_named_slot_with_operator() {
        // PMS 8.3.3: slot= (named slot with = operator)
        let dep = SlotDep::parse("0=").unwrap();
        match dep {
            SlotDep::Slot {
                slot: Some(s),
                op: Some(SlotOperator::Equal),
            } => {
                assert_eq!(s.slot, "0");
                assert!(s.subslot.is_none());
            }
            _ => panic!("expected Slot with Equal operator"),
        }
    }

    #[test]
    fn test_subslot_with_operator() {
        let dep = SlotDep::parse("0/1.75=").unwrap();
        match dep {
            SlotDep::Slot {
                slot: Some(s),
                op: Some(SlotOperator::Equal),
            } => {
                assert_eq!(s.slot, "0");
                assert_eq!(s.subslot, Some(Interned::intern("1.75")));
            }
            _ => panic!("expected Slot with subslot and Equal operator"),
        }
    }

    #[test]
    fn test_slot_round_trip() {
        let inputs = ["0", "3.12", "0/1.75", "=", "*", "0=", "0/1.75="];
        for input in inputs {
            let dep = SlotDep::parse(input).unwrap();
            assert_eq!(dep.to_string(), input, "round-trip failed for: {input}");
        }
    }
}
