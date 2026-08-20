//! IUSE_EFFECTIVE construction (PMS 11.1.1)

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::eapi::Eapi;

/// Build `IUSE_EFFECTIVE` for an ebuild (PMS 11.1.1)
///
/// `iuse` is the ebuild's calculated IUSE (with optional `+`/`-` prefixes).
/// The expand maps are profile `USE_EXPAND` / `USE_EXPAND_IMPLICIT` /
/// `USE_EXPAND_UNPREFIXED` and `USE_EXPAND_VALUES_${v}` token lists.
pub fn iuse_effective(
    eapi: Eapi,
    iuse: impl IntoIterator<Item = impl AsRef<str>>,
    iuse_implicit: impl IntoIterator<Item = impl AsRef<str>>,
    use_expand: impl IntoIterator<Item = impl AsRef<str>>,
    use_expand_implicit: impl IntoIterator<Item = impl AsRef<str>>,
    use_expand_unprefixed: impl IntoIterator<Item = impl AsRef<str>>,
    expand_values: &HashMap<String, Vec<String>>,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for token in iuse {
        let name = token.as_ref().trim_start_matches(['+', '-']);
        if !name.is_empty() {
            out.insert(name.to_string());
        }
    }
    if !eapi.has_profile_iuse_injection() {
        return out;
    }
    for token in iuse_implicit {
        let name = token.as_ref();
        if !name.is_empty() {
            out.insert(name.to_string());
        }
    }
    let expand: HashSet<String> = use_expand
        .into_iter()
        .map(|s| s.as_ref().to_string())
        .collect();
    let implicit: HashSet<String> = use_expand_implicit
        .into_iter()
        .map(|s| s.as_ref().to_string())
        .collect();
    let unprefixed: HashSet<String> = use_expand_unprefixed
        .into_iter()
        .map(|s| s.as_ref().to_string())
        .collect();
    for v in implicit.intersection(&unprefixed) {
        if let Some(vals) = expand_values.get(v) {
            out.extend(vals.iter().filter(|s| !s.is_empty()).cloned());
        }
    }
    for v in implicit.intersection(&expand) {
        let prefix = v.to_ascii_lowercase();
        if let Some(vals) = expand_values.get(v) {
            for x in vals {
                if !x.is_empty() {
                    out.insert(format!("{prefix}_{x}"));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eapi4_is_just_iuse() {
        let set = iuse_effective(
            Eapi::Four,
            ["+ssl", "-debug", "nls"],
            ["prefix"],
            ["ELIBC"],
            ["ELIBC", "ARCH"],
            ["ARCH"],
            &HashMap::from([
                ("ELIBC".into(), vec!["glibc".into()]),
                ("ARCH".into(), vec!["amd64".into()]),
            ]),
        );
        assert_eq!(
            set,
            BTreeSet::from(["ssl".into(), "debug".into(), "nls".into()])
        );
    }

    #[test]
    fn eapi5_injects_implicit_and_expand() {
        let set = iuse_effective(
            Eapi::Five,
            ["+ssl"],
            ["prefix"],
            ["ELIBC"],
            ["ELIBC", "ARCH"],
            ["ARCH"],
            &HashMap::from([
                ("ELIBC".into(), vec!["glibc".into(), "musl".into()]),
                ("ARCH".into(), vec!["amd64".into()]),
            ]),
        );
        assert!(set.contains("ssl"));
        assert!(set.contains("prefix"));
        assert!(set.contains("amd64"));
        assert!(set.contains("elibc_glibc"));
        assert!(set.contains("elibc_musl"));
        assert!(!set.contains("ELIBC"));
    }
}
