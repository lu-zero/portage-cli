//! USE configuration tokens
//!
//! A profile or user configuration layer changes the USE set through an ordered list of
//! [`UseToken`]s. PMS defines the profile grammar: flag names, `-flag` and `-*`. Prefix wildcards
//! and the group replacement of a `USE_EXPAND` assignment come from user configuration, which PMS
//! leaves to the package manager; they follow Portage.

use gentoo_interner::{DefaultInterner, Interned};

/// One operation of a USE configuration layer, applied in order
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UseToken {
    /// `flag` or `-flag`
    Flag {
        /// The flag name
        flag: Interned<DefaultInterner>,
        /// `false` for `-flag`
        enable: bool,
    },
    /// `-*`: every flag set so far is dropped
    ClearAll,
    /// `-<prefix>*`, or a user `USE_EXPAND` assignment replacing its group: every flag starting
    /// with the prefix set so far is dropped
    ClearPrefix(Interned<DefaultInterner>),
    /// `<prefix>*` in a USE list: every IUSE flag starting with the prefix, at this point
    ExpandPrefix(Interned<DefaultInterner>),
    /// `*` in a user `USE_EXPAND` variable: every IUSE flag starting with the prefix, once the whole
    /// fold is done
    LiteralPrefix(Interned<DefaultInterner>),
}

/// A token outside the grammar of the file it was read from
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidUseToken {
    /// Not `-*`, nor a USE flag name with an optional `-`
    #[error("`{0}` is not a valid USE flag")]
    Name(String),
}

impl UseToken {
    /// Parse a token of a profile file (`make.defaults` USE, `package.use`) per PMS
    ///
    /// ```
    /// use gentoo_core::{InvalidUseToken, UseToken};
    ///
    /// assert_eq!(UseToken::profile("-*"), Ok(UseToken::ClearAll));
    /// assert!(matches!(UseToken::profile("-l10n_de"), Ok(UseToken::Flag { enable: false, .. })));
    /// assert_eq!(
    ///     UseToken::profile("l10n_*"),
    ///     Err(InvalidUseToken::Name("l10n_*".into()))
    /// );
    /// ```
    pub fn profile(token: &str) -> Result<Self, InvalidUseToken> {
        if token == "-*" {
            return Ok(Self::ClearAll);
        }
        let (name, enable) = match token.strip_prefix('-') {
            Some(name) => (name, false),
            None => (token, true),
        };
        if !is_flag_name(name) {
            return Err(InvalidUseToken::Name(token.to_owned()));
        }
        Ok(Self::Flag {
            flag: Interned::intern(name),
            enable,
        })
    }
}

/// PMS USE flag name: `[A-Za-z0-9+_@-]`, starting alphanumeric
fn is_flag_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'_' | b'@' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_tokens_follow_the_pms_name_grammar() {
        for ok in [
            "x",
            "l10n_pt-BR",
            "cpu_flags_x86_sse4_1",
            "a+b",
            "linguas_sr@latin",
        ] {
            assert!(UseToken::profile(ok).is_ok(), "{ok}");
            assert!(UseToken::profile(&format!("-{ok}")).is_ok(), "-{ok}");
        }
        for bad in [
            "", "-", "*", "l10n_*", "-l10n_*", "+x", "_x", "-+x", "L10N:", "a.b",
        ] {
            assert!(UseToken::profile(bad).is_err(), "{bad}");
        }
    }
}
