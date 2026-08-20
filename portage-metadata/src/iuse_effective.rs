//! IUSE_EFFECTIVE construction (PMS 11.1.1)

use std::collections::{BTreeSet, HashMap, HashSet};

use gentoo_core::KnownArch;

use crate::eapi::Eapi;

/// Build `IUSE_EFFECTIVE` for an ebuild (PMS 11.1.1)
///
/// `iuse` is the ebuild's calculated IUSE (with optional `+`/`-` prefixes).
/// The expand maps are profile `USE_EXPAND` / `USE_EXPAND_IMPLICIT` /
/// `USE_EXPAND_UNPREFIXED` and `USE_EXPAND_VALUES_${v}` token lists.
///
/// Non-injection EAPIs (table 5.6) get PMS 11.1.1's narrower set instead.
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
        non_injection_extras(use_expand, expand_values, &mut out);
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

/// PMS 11.1.1's non-injection additions: every known `ARCH` keyword, plus
/// any flag prefixed `${lower_v}_` for `v` in the profile's own
/// `USE_EXPAND` — approximated as the profile's actual
/// `USE_EXPAND_VALUES_${v}` entries, since "all legal use flag names" has
/// no other enumerable source.
///
/// `ARCH` prefers the profile's own `USE_EXPAND_VALUES_ARCH` (real profiles
/// define it, e.g. `arch/base/make.defaults`, and it includes multi-OS
/// values like `arm64-macos` that [`KnownArch`] doesn't model) — the
/// hardcoded [`KnownArch::ALL`] list is a fallback only, for a caller that
/// never threads the profile value through.
fn non_injection_extras(
    use_expand: impl IntoIterator<Item = impl AsRef<str>>,
    expand_values: &HashMap<String, Vec<String>>,
    out: &mut BTreeSet<String>,
) {
    match expand_values.get("ARCH") {
        Some(vals) => out.extend(vals.iter().filter(|s| !s.is_empty()).cloned()),
        None => out.extend(KnownArch::ALL.iter().map(|a| a.as_keyword().to_string())),
    }
    for v in use_expand {
        let v = v.as_ref();
        let prefix = v.to_ascii_lowercase();
        let Some(vals) = expand_values.get(v) else {
            continue;
        };
        for x in vals {
            if !x.is_empty() {
                out.insert(format!("{prefix}_{x}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eapi4_adds_all_arch_keywords_and_expand_prefixed_flags() {
        // Non-injection EAPIs (PMS 11.1.1): IUSE, every ARCH keyword, and
        // flags prefixed by the profile's own USE_EXPAND — not just the
        // subset that's also USE_EXPAND_IMPLICIT (that filter only applies
        // to the injection branch).
        let set = iuse_effective(
            Eapi::Four,
            ["+ssl", "-debug", "nls"],
            ["prefix"], // IUSE_IMPLICIT is an injection-branch-only input
            ["ELIBC"],
            ["ARCH"], // ELIBC is deliberately NOT in USE_EXPAND_IMPLICIT
            [] as [&str; 0],
            &HashMap::from([("ELIBC".into(), vec!["glibc".into()])]),
        );
        assert!(set.contains("ssl"));
        assert!(set.contains("debug"));
        assert!(set.contains("nls"));
        assert!(!set.contains("prefix"), "IUSE_IMPLICIT must not leak in");
        assert!(
            set.contains("elibc_glibc"),
            "USE_EXPAND prefix applies without the IMPLICIT filter"
        );
        for arch in gentoo_core::KnownArch::ALL {
            assert!(set.contains(arch.as_keyword()), "missing ARCH {arch:?}");
        }
    }

    #[test]
    fn eapi4_prefers_the_profiles_own_arch_list_over_the_hardcoded_fallback() {
        // Real profiles (e.g. arch/base/make.defaults) define
        // USE_EXPAND_VALUES_ARCH with multi-OS values KnownArch doesn't
        // model at all (arm64-macos) — when the profile supplies it, use
        // it verbatim rather than the hardcoded fallback list.
        let set = iuse_effective(
            Eapi::Four,
            [] as [&str; 0],
            [] as [&str; 0],
            [] as [&str; 0],
            [] as [&str; 0],
            [] as [&str; 0],
            &HashMap::from([("ARCH".into(), vec!["amd64".into(), "arm64-macos".into()])]),
        );
        assert!(set.contains("amd64"));
        assert!(set.contains("arm64-macos"));
        assert!(
            !set.contains("riscv"),
            "must not silently union in the hardcoded fallback once the profile provides its own list"
        );
    }

    #[test]
    fn eapi4_use_query_accepts_a_real_but_unlisted_arch_keyword() {
        // PMS: "All possible values for the ARCH variable" — not just the
        // one this profile happens to use.
        let set = iuse_effective(
            Eapi::Four,
            [] as [&str; 0],
            [] as [&str; 0],
            [] as [&str; 0],
            [] as [&str; 0],
            [] as [&str; 0],
            &HashMap::new(),
        );
        assert!(set.contains("riscv"));
        assert!(set.contains("loong"));
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
