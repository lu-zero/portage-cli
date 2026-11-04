//! USE configuration tokens
//!
//! A profile or user configuration layer changes the USE set through an ordered list of
//! [`UseToken`]s. PMS defines the profile grammar: flag names, `-flag` and `-*`. Prefix wildcards
//! and the group replacement of a `USE_EXPAND` assignment come from user configuration, which PMS
//! leaves to the package manager; they follow Portage.

use gentoo_interner::{DefaultInterner, Interned, Interner};

/// One operation of a USE configuration layer, applied in order
#[derive(Debug, PartialEq, Eq)]
pub enum UseToken<I = DefaultInterner>
where
    I: Interner,
{
    /// `flag` or `-flag`
    Flag {
        /// The flag name
        flag: Interned<I>,
        /// `false` for `-flag`
        enable: bool,
    },
    /// `-*`: every flag set so far is dropped
    ClearAll,
    /// `-<prefix>*`, or a user `USE_EXPAND` assignment replacing its group: every flag starting
    /// with the prefix set so far is dropped
    ClearPrefix(Interned<I>),
    /// `<prefix>*` in a USE list: every IUSE flag starting with the prefix, at this point
    ExpandPrefix(Interned<I>),
    /// `*` in a user `USE_EXPAND` variable: every IUSE flag starting with the prefix, once the whole
    /// fold is done
    LiteralPrefix(Interned<I>),
}

impl<I: Interner> Clone for UseToken<I> {
    fn clone(&self) -> Self {
        match self {
            Self::Flag { flag, enable } => Self::Flag {
                flag: flag.clone(),
                enable: *enable,
            },
            Self::ClearAll => Self::ClearAll,
            Self::ClearPrefix(p) => Self::ClearPrefix(p.clone()),
            Self::ExpandPrefix(p) => Self::ExpandPrefix(p.clone()),
            Self::LiteralPrefix(p) => Self::LiteralPrefix(p.clone()),
        }
    }
}

impl<I: Interner> Copy for UseToken<I> where Interned<I>: Copy {}

/// A token outside the grammar of the place it was read from
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidUseToken {
    /// Not `-*`, nor a USE flag name with an optional `-`
    #[error("`{0}` is not a valid USE flag")]
    Name(String),
    /// `+value` or `-value` in a make.conf or environment `USE_EXPAND` variable
    #[error(
        "`{0}` in a USE_EXPAND variable: its values replace the group, they cannot add or remove one"
    )]
    Operator(String),
}

impl<I: Interner> UseToken<I> {
    /// The flag and whether it is turned on, for a [`UseToken::Flag`]
    pub fn as_flag(&self) -> Option<(Interned<I>, bool)> {
        match self {
            Self::Flag { flag, enable } => Some((flag.clone(), *enable)),
            _ => None,
        }
    }

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

    /// Parse a token of a user USE list: make.conf or environment `USE`, `/etc/portage/package.use`
    ///
    /// Portage reads `<prefix>_*` as a glob over the package's IUSE and accepts a `+` it warns
    /// about; a lone `+` is no token.
    ///
    /// ```
    /// use gentoo_core::UseToken;
    /// use gentoo_core::interner::Interned;
    ///
    /// let l10n = Interned::intern("l10n_");
    /// assert_eq!(UseToken::user("l10n_*"), Some(UseToken::ExpandPrefix(l10n)));
    /// assert_eq!(UseToken::user("-l10n_*"), Some(UseToken::ClearPrefix(l10n)));
    /// assert_eq!(UseToken::user("+"), None);
    /// ```
    pub fn user(token: &str) -> Option<Self> {
        if token == "-*" {
            return Some(Self::ClearAll);
        }
        let token = token.strip_prefix('+').unwrap_or(token);
        let (name, enable) = match token.strip_prefix('-') {
            Some(name) => (name, false),
            None => (token, true),
        };
        if name.is_empty() {
            return None;
        }
        Some(match name.strip_suffix('*').filter(|p| p.ends_with('_')) {
            Some(prefix) if enable => Self::ExpandPrefix(Interned::intern(prefix)),
            Some(prefix) => Self::ClearPrefix(Interned::intern(prefix)),
            None => Self::Flag {
                flag: Interned::intern(name),
                enable,
            },
        })
    }

    /// The tokens of a make.conf or environment assignment `var="value"` of a `USE_EXPAND`
    /// variable
    ///
    /// The group is replaced: its flags so far go, the listed values come in, and `*` keeps every
    /// IUSE value of the group on to the end. Portage ignores `+value`/`-value` there, with a
    /// warning; they come back as errors.
    ///
    /// ```
    /// use gentoo_core::{InvalidUseToken, UseToken};
    /// use gentoo_core::interner::Interned;
    ///
    /// let l10n = Interned::intern("l10n_");
    /// let tokens: Vec<_> = UseToken::assignment("L10N", "de -fr *").collect();
    /// assert_eq!(
    ///     tokens,
    ///     [
    ///         Ok(UseToken::ClearPrefix(l10n)),
    ///         Ok(UseToken::Flag { flag: Interned::intern("l10n_de"), enable: true }),
    ///         Err(InvalidUseToken::Operator("-fr".into())),
    ///         Ok(UseToken::LiteralPrefix(l10n)),
    ///     ]
    /// );
    /// ```
    pub fn assignment<'v>(
        var: &str,
        value: &'v str,
    ) -> impl Iterator<Item = Result<Self, InvalidUseToken>> + 'v {
        let prefix = format!("{}_", var.to_lowercase());
        let group = Interned::<I>::intern(&prefix);
        std::iter::once(Ok(Self::ClearPrefix(group.clone()))).chain(value.split_whitespace().map(
            move |v| match v {
                "*" => Ok(Self::LiteralPrefix(group.clone())),
                _ if v.starts_with(['+', '-']) => Err(InvalidUseToken::Operator(v.to_owned())),
                _ => Ok(Self::Flag {
                    flag: Interned::intern(&format!("{prefix}{v}")),
                    enable: true,
                }),
            },
        ))
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

    type Token = UseToken<DefaultInterner>;

    #[test]
    fn profile_tokens_follow_the_pms_name_grammar() {
        for ok in [
            "x",
            "l10n_pt-BR",
            "cpu_flags_x86_sse4_1",
            "a+b",
            "linguas_sr@latin",
        ] {
            assert!(Token::profile(ok).is_ok(), "{ok}");
            assert!(Token::profile(&format!("-{ok}")).is_ok(), "-{ok}");
        }
        for bad in [
            "", "-", "*", "l10n_*", "-l10n_*", "+x", "_x", "-+x", "L10N:", "a.b",
        ] {
            assert!(Token::profile(bad).is_err(), "{bad}");
        }
    }

    // Only a trailing `_*` is a glob, as Portage's `x[-2:] == "_*"`.
    #[test]
    fn a_user_glob_needs_the_underscore_before_the_star() {
        let flag = |name: &str, enable| {
            Some(Token::Flag {
                flag: Interned::intern(name),
                enable,
            })
        };
        assert_eq!(Token::user("l10n_pt*"), flag("l10n_pt*", true));
        assert_eq!(Token::user("*"), flag("*", true));
        assert_eq!(Token::user("+ssl"), flag("ssl", true));
        assert_eq!(
            Token::user("cpu_flags_*"),
            Some(Token::ExpandPrefix(Interned::intern("cpu_flags_")))
        );
    }

    #[test]
    fn an_empty_assignment_still_replaces_the_group() {
        let tokens: Vec<_> = Token::assignment("VIDEO_CARDS", "").collect();
        assert_eq!(
            tokens,
            [Ok(Token::ClearPrefix(Interned::intern("video_cards_")))]
        );
    }
}
