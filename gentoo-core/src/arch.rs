//! Gentoo architecture types
//!
//! This module provides types for representing CPU architectures as they
//! appear in Gentoo's keyword system:
//!
//! - [`KnownArch`]: The 18 architectures officially supported by Gentoo Linux.
//!   Provides zero-cost representation and metadata like bitness.
//! - [`Arch`]: A generic architecture type that can be either a known Gentoo
//!   architecture or an overlay-defined keyword string.
//!
//! # Keywords vs Architecture Names
//!
//! Gentoo uses specific keyword strings in ebuilds (e.g., `~amd64`, `arm64`).
//! `KnownArch` maps to these canonical keywords via [`KnownArch::as_keyword`].
//! Some architectures share keywords (e.g., `riscv32` and `riscv64` both map
//! to `"riscv"`), reflecting how Gentoo keywords group related architectures.

use std::fmt;
use std::hash::Hash;
use std::str::FromStr;

use crate::Error;
use gentoo_interner::{DefaultInterner, Interned, Interner};

/// A CPU architecture officially supported by Gentoo Linux
///
/// Represents the 18 architectures with stable or testing keywords in the
/// Gentoo ebuild repository. Each variant maps to a canonical Gentoo keyword
/// string via [`KnownArch::as_keyword`].
///
/// # Keyword Grouping
///
/// Some architectures share keywords due to Gentoo's keyword conventions:
///
/// | Architecture | Keyword | Notes |
/// |--------------|---------|-------|
/// | `Riscv32`, `Riscv64` | `"riscv"` | RISC-V variants share a keyword |
/// | `Mips`, `Mips64` | `"mips"` | MIPS variants share a keyword |
/// | `Sparc`, `Sparc64` | `"sparc"` | SPARC variants share a keyword |
///
/// # Examples
///
/// ```
/// use gentoo_core::KnownArch;
///
/// let arch: KnownArch = "amd64".parse().unwrap();
/// assert_eq!(arch.as_keyword(), "amd64");
/// assert_eq!(arch.bitness(), 64);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum KnownArch {
    /// 32-bit ARM (keyword `arm`)
    Arm,
    /// 64-bit ARM / AArch64 (keyword `arm64`)
    AArch64,
    /// 32-bit x86 (keyword `x86`)
    X86,
    /// 64-bit x86 / x86-64 (keyword `amd64`)
    X86_64,
    /// 32-bit RISC-V (keyword `riscv`)
    Riscv32,
    /// 64-bit RISC-V (keyword `riscv`)
    Riscv64,
    /// 32-bit PowerPC (keyword `ppc`)
    Powerpc,
    /// 64-bit PowerPC (keyword `ppc64`)
    Powerpc64,
    /// 32-bit MIPS (keyword `mips`)
    Mips,
    /// 64-bit MIPS (keyword `mips`)
    Mips64,
    /// 32-bit SPARC (keyword `sparc`)
    Sparc,
    /// 64-bit SPARC (keyword `sparc`)
    Sparc64,
    /// 64-bit IBM Z / s390x (keyword `s390`)
    S390x,
    /// Motorola 68000 series (keyword `m68k`)
    M68k,
    /// 64-bit LoongArch (keyword `loong`)
    LoongArch64,
    /// DEC Alpha (keyword `alpha`)
    Alpha,
    /// HP PA-RISC (keyword `hppa`)
    Hppa,
    /// Intel Itanium / IA-64 (keyword `ia64`)
    Ia64,
}

impl KnownArch {
    /// Gentoo keyword string for this architecture (e.g. `"amd64"`)
    pub fn as_keyword(&self) -> &'static str {
        match self {
            KnownArch::Arm => "arm",
            KnownArch::AArch64 => "arm64",
            KnownArch::X86 => "x86",
            KnownArch::X86_64 => "amd64",
            KnownArch::Riscv32 | KnownArch::Riscv64 => "riscv",
            KnownArch::Powerpc => "ppc",
            KnownArch::Powerpc64 => "ppc64",
            KnownArch::Mips | KnownArch::Mips64 => "mips",
            KnownArch::Sparc | KnownArch::Sparc64 => "sparc",
            KnownArch::S390x => "s390",
            KnownArch::M68k => "m68k",
            KnownArch::LoongArch64 => "loong",
            KnownArch::Alpha => "alpha",
            KnownArch::Hppa => "hppa",
            KnownArch::Ia64 => "ia64",
        }
    }

    /// Parse from a keyword or common alias string (case-insensitive)
    pub fn parse(arch: &str) -> Result<Self, Error> {
        match arch.to_lowercase().as_str() {
            "arm" | "armv7" | "armv7a" | "armv7l" | "armv7hl" => Ok(KnownArch::Arm),
            "aarch64" | "arm64" | "armv8" | "armv8a" => Ok(KnownArch::AArch64),
            "x86" | "i386" | "i486" | "i586" | "i686" => Ok(KnownArch::X86),
            "x86_64" | "amd64" => Ok(KnownArch::X86_64),
            "riscv32" => Ok(KnownArch::Riscv32),
            "riscv64" | "riscv" => Ok(KnownArch::Riscv64),
            "powerpc" | "ppc" => Ok(KnownArch::Powerpc),
            "powerpc64" | "ppc64" => Ok(KnownArch::Powerpc64),
            "mips" => Ok(KnownArch::Mips),
            "mips64" => Ok(KnownArch::Mips64),
            "sparc" => Ok(KnownArch::Sparc),
            "sparc64" => Ok(KnownArch::Sparc64),
            "s390" | "s390x" => Ok(KnownArch::S390x),
            "m68k" => Ok(KnownArch::M68k),
            "loong" | "loongarch64" => Ok(KnownArch::LoongArch64),
            "alpha" => Ok(KnownArch::Alpha),
            "hppa" => Ok(KnownArch::Hppa),
            "ia64" => Ok(KnownArch::Ia64),
            _ => Err(Error::ParseError(format!("Unknown architecture: {arch}"))),
        }
    }

    /// Bitness (32 or 64) of this architecture
    pub fn bitness(&self) -> u32 {
        match self {
            KnownArch::Arm
            | KnownArch::X86
            | KnownArch::Riscv32
            | KnownArch::Powerpc
            | KnownArch::Mips
            | KnownArch::Sparc
            | KnownArch::M68k
            | KnownArch::Hppa => 32,
            KnownArch::AArch64
            | KnownArch::X86_64
            | KnownArch::Riscv64
            | KnownArch::Powerpc64
            | KnownArch::Mips64
            | KnownArch::Sparc64
            | KnownArch::S390x
            | KnownArch::LoongArch64
            | KnownArch::Alpha
            | KnownArch::Ia64 => 64,
        }
    }

    /// Current system architecture from [`std::env::consts::ARCH`]
    pub fn current() -> Result<Self, Error> {
        Self::parse(std::env::consts::ARCH)
    }
}

impl fmt::Display for KnownArch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            KnownArch::Arm => "arm",
            KnownArch::AArch64 => "aarch64",
            KnownArch::X86 => "x86",
            KnownArch::X86_64 => "x86_64",
            KnownArch::Riscv32 => "riscv32",
            KnownArch::Riscv64 => "riscv64",
            KnownArch::Powerpc => "powerpc",
            KnownArch::Powerpc64 => "powerpc64",
            KnownArch::Mips => "mips",
            KnownArch::Mips64 => "mips64",
            KnownArch::Sparc => "sparc",
            KnownArch::Sparc64 => "sparc64",
            KnownArch::S390x => "s390x",
            KnownArch::M68k => "m68k",
            KnownArch::LoongArch64 => "loongarch64",
            KnownArch::Alpha => "alpha",
            KnownArch::Hppa => "hppa",
            KnownArch::Ia64 => "ia64",
        };
        write!(f, "{name}")
    }
}

impl FromStr for KnownArch {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

// ── Arch<I> ───────────────────────────────────────────────────────────────────

/// Opaque key for an overlay-defined keyword string
///
/// Type alias for [`Interned<I>`](crate::interner::Interned).
/// See that type for the full API and serde behaviour.
pub type ExoticKey<I> = Interned<I>;

/// A Gentoo architecture keyword
///
/// Represents either a well-known Gentoo architecture or an overlay-specific
/// keyword string. This type is used when parsing ebuild `KEYWORDS` or other
/// architecture references that may include non-standard values.
///
/// # Variants
///
/// - `Known(KnownArch)`: A recognized Gentoo architecture. Zero-cost and `Copy`.
/// - `Exotic(ExoticKey<I>)`: An overlay-defined keyword stored via interning.
///
/// # Memory Efficiency
///
/// With the default `interner` feature, `Arch<GlobalInterner>` is `Copy` (4 bytes)
/// and identical exotic strings share a single allocation. This is useful when
/// processing large numbers of ebuilds.
///
/// # Examples
///
/// ```
/// use gentoo_core::Arch;
///
/// // Known architectures are recognized automatically
/// let known = Arch::intern("amd64");
/// assert_eq!(known.as_str(), "amd64");
///
/// // Unknown strings become exotic keys
/// let exotic = Arch::intern("my-custom-board");
/// assert_eq!(exotic.as_str(), "my-custom-board");
/// ```
#[derive(Debug, PartialEq, Eq, Hash)]
pub enum Arch<I = DefaultInterner>
where
    I: Interner,
{
    /// A well-known Gentoo architecture keyword
    Known(KnownArch),
    /// An overlay-defined keyword string interned via `I`
    Exotic(ExoticKey<I>),
}

impl<I: Interner> Clone for Arch<I> {
    fn clone(&self) -> Self {
        match self {
            Self::Known(arch) => Self::Known(*arch),
            Self::Exotic(key) => Self::Exotic(key.clone()),
        }
    }
}

impl<I: Interner> Copy for Arch<I> where Interned<I>: Copy {}

impl<I: Interner> Arch<I> {
    /// Intern `keyword` using the interner `I`
    pub fn intern(keyword: &str) -> Self {
        if let Ok(known) = KnownArch::parse(keyword) {
            Self::Known(known)
        } else {
            Self::Exotic(ExoticKey::intern(keyword))
        }
    }

    /// Current system architecture as its Gentoo keyword
    ///
    /// Returns `Known` for recognized architectures, `Exotic` otherwise.
    ///
    /// On macOS this surfaces the Gentoo Prefix Darwin keyword (`arm64-macos`,
    /// `x64-macos`, …) rather than the bare CPU arch, since that is what ebuild
    /// `KEYWORDS` on a Prefix Darwin profile are written against — the host CPU
    /// (`arm64`) and the keyword (`arm64-macos`) differ, and keyword acceptance
    /// must use the latter.
    pub fn current() -> Self {
        Self::intern(current_keyword())
    }

    /// Extract the CPU arch from a GNU CHOST triple using the interner `I`
    ///
    /// Returns `None` only when `chost` is empty.
    pub fn from_chost(chost: &str) -> Option<Self> {
        let cpu = chost.split('-').next().filter(|s| !s.is_empty())?;
        Some(Self::intern(&normalize_chost_cpu(cpu)))
    }

    /// Resolve to the Gentoo keyword string using the interner `I`
    pub fn as_str(&self) -> &str {
        match self {
            Self::Known(arch) => arch.as_keyword(),
            Self::Exotic(key) => key.resolve(),
        }
    }

    /// The Gentoo keyword for this architecture
    ///
    /// For known architectures, returns the canonical keyword (e.g., `"amd64"`).
    /// For exotic architectures, returns the interned string directly.
    pub fn as_keyword(&self) -> &str {
        self.as_str()
    }
}

/// The Gentoo keyword for the platform this binary runs on
///
/// Gentoo Prefix on macOS keys keywords by an OS-suffixed arch (`arm64-macos`,
/// `x64-macos`, `x86-macos`, `ppc-macos`, `ppc64-macos`) whose CPU spelling also
/// differs from the Linux keyword (`x86_64` → `x64`, not `amd64`), so it needs an
/// explicit map. Elsewhere the bare CPU arch (`std::env::consts::ARCH`) interns to
/// the right keyword directly (`aarch64` → `arm64`).
#[cfg(target_os = "macos")]
fn current_keyword() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64-macos",
        "x86_64" => "x64-macos",
        "x86" => "x86-macos",
        "powerpc" => "ppc-macos",
        "powerpc64" => "ppc64-macos",
        other => other,
    }
}

#[cfg(not(target_os = "macos"))]
fn current_keyword() -> &'static str {
    std::env::consts::ARCH
}

impl<I: Interner> fmt::Display for Arch<I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<I: Interner> PartialEq<str> for Arch<I> {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl<I: Interner> PartialEq<&str> for Arch<I> {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl<I: Interner> PartialEq<String> for Arch<I> {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl<I: Interner> FromStr for Arch<I> {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match KnownArch::from_str(s) {
            Ok(known) => Ok(Self::Known(known)),
            Err(_) => Ok(Self::Exotic(ExoticKey::intern(s))),
        }
    }
}

/// Normalise the CPU field of a GNU CHOST triple before matching known arches
fn normalize_chost_cpu(cpu: &str) -> String {
    let s = cpu.to_lowercase();

    // powerpc64le / powerpc64be → powerpc64
    for suffix in &["le", "be"] {
        if let Some(base) = s.strip_suffix(suffix)
            && base == "powerpc64"
        {
            return base.to_string();
        }
    }

    // mipsel / mipseb → mips;  mips64el / mips64eb → mips64
    for suffix in &["el", "eb"] {
        if let Some(base) = s.strip_suffix(suffix)
            && (base == "mips" || base == "mips64")
        {
            return base.to_string();
        }
    }

    // riscv64gc, riscv64imac → riscv64;  riscv32gc → riscv32
    if let Some(after_riscv) = s.strip_prefix("riscv") {
        if let Some(end) = after_riscv.find(|c: char| !c.is_ascii_digit())
            && end > 0
        {
            return format!("riscv{}", &after_riscv[..end]);
        }
        return s;
    }

    // hppa2.0w, hppa1.1 → hppa
    if s.starts_with("hppa") && s.len() > "hppa".len() {
        return "hppa".to_string();
    }

    s
}

// ── Serde impls ──────────────────────────────────────────────────────────────

/// `Arch<I>` serializes as its keyword string (e.g. `"amd64"`, `"mymachine"`),
/// regardless of how the underlying interner key is stored.
#[cfg(feature = "serde")]
impl<I: Interner> serde::Serialize for Arch<I> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Deserializes from the keyword string, interning via `I`
#[cfg(feature = "serde")]
impl<'de, I: Interner> serde::Deserialize<'de> for Arch<I> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <String as serde::Deserialize<'de>>::deserialize(deserializer)?;
        Ok(Self::intern(&s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── KnownArch ────────────────────────────────────────────────────────────

    #[test]
    fn known_arch_keywords() {
        assert_eq!(KnownArch::Arm.as_keyword(), "arm");
        assert_eq!(KnownArch::AArch64.as_keyword(), "arm64");
        assert_eq!(KnownArch::X86.as_keyword(), "x86");
        assert_eq!(KnownArch::X86_64.as_keyword(), "amd64");
        assert_eq!(KnownArch::Riscv32.as_keyword(), "riscv");
        assert_eq!(KnownArch::Riscv64.as_keyword(), "riscv");
        assert_eq!(KnownArch::Powerpc.as_keyword(), "ppc");
        assert_eq!(KnownArch::Powerpc64.as_keyword(), "ppc64");
        assert_eq!(KnownArch::LoongArch64.as_keyword(), "loong");
        assert_eq!(KnownArch::Hppa.as_keyword(), "hppa");
    }

    #[test]
    fn known_arch_parsing() {
        assert!(KnownArch::parse("arm").is_ok());
        assert!(KnownArch::parse("amd64").is_ok());
        assert!(KnownArch::parse("AMD64").is_ok());
        assert!(KnownArch::parse("invalid").is_err());
    }

    #[test]
    fn known_arch_from_str() {
        assert_eq!("amd64".parse::<KnownArch>().unwrap(), KnownArch::X86_64);
        assert!("invalid".parse::<KnownArch>().is_err());
    }

    // ── Arch convenience methods (DefaultInterner) ───────────────────────────

    #[test]
    #[cfg(target_os = "macos")]
    #[test]
    fn arch_current_is_macos_keyword() {
        // On macOS, current() must surface the Prefix Darwin keyword, not the
        // bare CPU arch, so keyword acceptance matches the profile.
        let cur = <Arch>::current();
        assert!(
            cur.as_str().ends_with("-macos"),
            "expected a *-macos keyword, got {}",
            cur.as_str()
        );
        assert!(matches!(cur, Arch::Exotic(_)));
    }

    #[test]
    fn arch_intern_known() {
        assert!(matches!(<Arch>::intern("amd64"), Arch::Known(_)));
        assert!(matches!(<Arch>::intern("arm64"), Arch::Known(_)));
        assert!(matches!(<Arch>::intern("loong"), Arch::Known(_)));
        assert!(matches!(<Arch>::intern("hppa"), Arch::Known(_)));
    }

    #[test]
    fn arch_intern_exotic() {
        let a1: Arch = Arch::intern("mymachine");
        assert!(matches!(a1, Arch::Exotic(_)));
        assert_eq!(Arch::intern("mymachine"), a1); // same key
        assert_eq!(a1.as_str(), "mymachine");
    }

    #[test]
    fn arch_from_chost_known() {
        let cases = [
            ("x86_64-pc-linux-gnu", "amd64"),
            ("aarch64-unknown-linux-gnu", "arm64"),
            ("i686-pc-linux-gnu", "x86"),
            ("powerpc-unknown-linux-gnu", "ppc"),
            ("s390x-linux-gnu", "s390"),
        ];
        for (chost, expected) in cases {
            let arch: Arch = Arch::from_chost(chost).unwrap();
            assert_eq!(arch.as_str(), expected, "chost={chost}");
            assert!(
                matches!(arch, Arch::Known(_)),
                "chost={chost} should be Known"
            );
        }
    }

    #[test]
    fn arch_chost_normalization() {
        let cases = [
            ("powerpc64le-unknown-linux-gnu", "ppc64"),
            ("riscv64gc-unknown-linux-gnu", "riscv"),
            ("mipsel-unknown-linux-gnu", "mips"),
            ("mips64el-unknown-linux-gnu", "mips"),
            ("hppa2.0w-hp-linux-gnu", "hppa"),
        ];
        for (chost, expected) in cases {
            assert_eq!(
                <Arch>::from_chost(chost).unwrap().as_str(),
                expected,
                "chost={chost}"
            );
        }
    }

    #[test]
    fn arch_empty_chost() {
        assert!(<Arch>::from_chost("").is_none());
    }

    #[test]
    fn arch_from_str_known() {
        let arch: Arch = "amd64".parse().unwrap();
        assert!(matches!(arch, Arch::Known(KnownArch::X86_64)));
        assert_eq!(arch.as_str(), "amd64");

        let arch: Arch = "arm64".parse().unwrap();
        assert!(matches!(arch, Arch::Known(KnownArch::AArch64)));
        assert_eq!(arch.as_str(), "arm64");
    }

    #[test]
    fn arch_from_str_exotic() {
        let arch: Arch = "mymachine".parse().unwrap();
        assert!(matches!(arch, Arch::Exotic(_)));
        assert_eq!(arch.as_str(), "mymachine");
    }

    // ── Serde roundtrip ──────────────────────────────────────────────────────

    #[cfg(feature = "serde")]
    mod serde {
        use super::*;

        #[test]
        fn arch_known_serializes_as_keyword() {
            let arch: Arch = Arch::intern("amd64");
            let json = serde_json::to_string(&arch).unwrap();
            assert_eq!(json, r#""amd64""#);
        }

        #[test]
        fn arch_exotic_serializes_as_string() {
            let arch: Arch = Arch::intern("mymachine");
            let json = serde_json::to_string(&arch).unwrap();
            assert_eq!(json, r#""mymachine""#);
        }

        #[test]
        fn arch_known_roundtrip() {
            let original = Arch::intern("arm64");
            let json = serde_json::to_string(&original).unwrap();
            let restored: Arch = serde_json::from_str(&json).unwrap();
            assert_eq!(original, restored);
            assert!(matches!(restored, Arch::Known(KnownArch::AArch64)));
        }

        #[test]
        fn arch_exotic_roundtrip() {
            let original = Arch::intern("mymachine");
            let json = serde_json::to_string(&original).unwrap();
            let restored: Arch = serde_json::from_str(&json).unwrap();
            assert_eq!(original, restored);
            assert!(matches!(restored, Arch::Exotic(_)));
        }

        #[test]
        fn arch_deserialize_known_from_alias() {
            let restored: Arch = serde_json::from_str(r#""x86_64""#).unwrap();
            assert_eq!(restored, Arch::intern("amd64"));
        }
    }
}
