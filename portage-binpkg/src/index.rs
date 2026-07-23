//! The binhost `Packages` index format: parsing, and the local/remote
//! reuse-matching readers `em`'s `-k`/`--usepkg`/`-g`/`--getbinpkg` build on.
//!
//! A binpkg is reusable only when it would produce the same result as a
//! fresh build — portage's rule: the binpkg's recorded `USE`, restricted to
//! the package's own `IUSE`, must equal the desired `USE` (similarly
//! restricted), and the slot/subslot must match. Version matches by `cpv`
//! lookup. This is the `_match_use`-style check portage applies to built
//! packages (`use = USE ∩ IUSE`), so a stale-USE binpkg is correctly
//! rejected and rebuilt — matching `emerge -k`
//! (<https://github.com/gentoo/portage/blob/ac461a29/lib/portage/dbapi/__init__.py>,
//! bug #453400).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use sokgi::{Dialect, FlagSet, Warning};

use crate::error::Result;
use crate::scan::find_gpkg_containers;

/// One `Packages` index entry, parsed into the fields the reuse check needs.
#[derive(Debug, Clone)]
pub struct BinpkgEntry {
    /// Path relative to `PKGDIR` (e.g. `app-test/foo-1.0-1.gpkg.tar`).
    pub path: String,
    /// The binpkg's recorded `USE`, as a bare-flag set.
    pub use_set: HashSet<String>,
    /// The package's `IUSE`, prefix-stripped (`+flag`/`-flag` → `flag`).
    pub iuse: HashSet<String>,
    /// Build `CHOST` from the entry (or empty if the index omitted it).
    ///
    /// Reuse rejects a mismatch against the live desired CHOST so an aarch64
    /// PKGDIR entry is never taken for a riscv64 plan (and vice versa).
    pub chost: String,
    /// Build-time `CFLAGS` (empty if absent).
    pub cflags: String,
    /// Build-time `CXXFLAGS` (empty if absent).
    pub cxxflags: String,
    /// Build-time `LDFLAGS` (empty if absent).
    pub ldflags: String,
    /// Build-time `RUSTFLAGS` (empty if absent).
    pub rustflags: String,
    /// Build environment key derived from cflags, cxxflags, ldflags, rustflags
    /// via sokgi. Empty if all flag sets are empty (skip gate).
    /// "__native__" if any flag set contains machine-dependent flags.
    pub build_env_key: String,
    /// Recorded `BUILD_ID` (`0` for the implicit single-instance case). Used
    /// to prefer the newest matching instance in [`BinpkgIndex::find_reusable`]
    /// / [`RemoteBinpkgIndex::find_reusable`] instead of the first one listed.
    pub build_id: u32,
}

/// A parsed `Packages` index, keyed by `cpv`, answering reuse queries.
#[derive(Debug, Default)]
pub struct BinpkgIndex {
    entries: BTreeMap<String, Vec<BinpkgEntry>>,
    /// Absolute `PKGDIR`, used to resolve each entry's relative `path`.
    pkgdir: PathBuf,
}

impl BinpkgIndex {
    /// Open the `Packages` index in `pkgdir`. If it is missing or unreadable,
    /// rebuild it on the fly by scanning `pkgdir` for `*.gpkg.tar` and reading
    /// each container's metadata (the slow fallback).
    pub fn open(pkgdir: &Path) -> Result<Self> {
        let index_path = pkgdir.join("Packages");
        if index_path.is_file() {
            let text = std::fs::read_to_string(&index_path)?;
            let idx = Self::parse(&text, pkgdir.to_path_buf());
            if !idx.entries.is_empty() {
                return Ok(idx);
            }
        }
        Self::scan(pkgdir)
    }

    /// Parse a `Packages` file. The first blank-line-separated block is the
    /// header; each later block is one package (`CPV:` required).
    fn parse(text: &str, pkgdir: PathBuf) -> Self {
        let entries = parse_packages_entries(text);
        Self { entries, pkgdir }
    }

    /// Slow path: no usable index — scan `pkgdir` and read each container's
    /// metadata via [`crate::read_metadata`].
    fn scan(pkgdir: &Path) -> Result<Self> {
        let mut entries: BTreeMap<String, Vec<BinpkgEntry>> = BTreeMap::new();
        let mut files = Vec::new();
        find_gpkg_containers(pkgdir, pkgdir, &mut files)?;
        for (rel, full) in &files {
            let meta = match crate::read_metadata(full) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("warning: skipping {}: {e:#}", full.display());
                    continue;
                }
            };
            let cat = meta.get("CATEGORY").map(String::as_str).unwrap_or("");
            let pf = meta.get("PF").map(String::as_str).unwrap_or("");
            if cat.is_empty() || pf.is_empty() {
                continue;
            }
            let cpv = format!("{cat}/{pf}");
            let cflags = meta.get("CFLAGS").cloned().unwrap_or_default();
            let cxxflags = meta.get("CXXFLAGS").cloned().unwrap_or_default();
            let ldflags = meta.get("LDFLAGS").cloned().unwrap_or_default();
            let rustflags = meta.get("RUSTFLAGS").cloned().unwrap_or_default();
            let build_env_key = build_env_key(&cflags, &cxxflags, &ldflags, &rustflags);
            let build_id = meta
                .get("BUILD_ID")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);

            let entry = BinpkgEntry {
                path: rel.clone(),
                use_set: split_use(meta.get("USE").map(String::as_str).unwrap_or("")),
                iuse: split_iuse(meta.get("IUSE").map(String::as_str).unwrap_or("")),
                chost: meta.get("CHOST").cloned().unwrap_or_default(),
                cflags,
                cxxflags,
                ldflags,
                rustflags,
                build_id,
                build_env_key,
            };
            entries.entry(cpv).or_default().push(entry);
        }
        Ok(Self {
            entries,
            pkgdir: pkgdir.to_path_buf(),
        })
    }

    /// The number of indexed packages (for reporting).
    pub fn len(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
    }

    /// Whether the index has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find a reusable binpkg for `cpv` given the desired `USE`, `CHOST`, and
    /// `build_env_key`, returning the absolute container path. `None` if no
    /// binpkg exists for the cpv, or if its recorded USE/CHOST/build_env_key
    /// does not match (i.e. it must be rebuilt). Version and slot match by
    /// `cpv` lookup (a binpkg for a cpv is that ebuild's slot).
    ///
    /// `desired_chost` empty skips the CHOST gate (legacy/test callers); a
    /// missing CHOST on the binpkg also skips it so sparse indexes still work.
    ///
    /// `desired_build_env_key` empty skips the build_env_key gate (backward compat);
    /// a missing/empty build_env_key on the binpkg also skips it.
    ///
    /// Build-env fields ([`BinpkgEntry::cflags`] / [`BinpkgEntry::cxxflags`] /
    /// [`BinpkgEntry::ldflags`]) are checked via the computed build_env_key.
    pub fn find_reusable(
        &self,
        cpv: &str,
        desired_use: &[String],
        desired_chost: &str,
        desired_build_env_key: &str,
    ) -> Option<PathBuf> {
        let entries = self.entries.get(cpv)?;
        // Among all matching entries, prefer the newest BUILD_ID — not just
        // the first one listed (scan/parse order is not BUILD_ID order).
        let best = entries
            .iter()
            .filter(|entry| entry_reusable(entry, desired_use, desired_chost))
            .filter(|entry| build_env_key_compatible(&entry.build_env_key, desired_build_env_key))
            .max_by_key(|entry| entry.build_id)?;
        Some(self.pkgdir.join(&best.path))
    }

    /// Look up the raw index entries for `cpv` (including CFLAGS/CXXFLAGS/
    /// LDFLAGS provenance), if present. Does not apply USE/CHOST reuse rules.
    /// Returns all entries for the CPV (there may be multiple variants).
    pub fn get(&self, cpv: &str) -> Option<&[BinpkgEntry]> {
        self.entries.get(cpv).map(|v| v.as_slice())
    }
}

/// Split a `USE` string into a bare-flag set.
pub(crate) fn split_use(s: &str) -> HashSet<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

/// Split an `IUSE` string, stripping `+`/`-` default-on/off prefixes so the
/// flag names compare against `USE`.
pub(crate) fn split_iuse(s: &str) -> HashSet<String> {
    s.split_whitespace()
        .map(|t| t.trim_start_matches(['+', '-']).to_owned())
        .collect()
}

/// Split a `Packages` text into `(header, body)` at the first blank line or
/// the first `CPV:` line, whichever comes first. Real portage always writes
/// a blank line after the header, but its own reader tolerates the header
/// glued directly onto the first entry (no blank line) — so must we: without
/// this shared boundary, a naive `split("\n\n")` over the whole text either
/// drops a glued header's fields entirely (`parse_index_header`) or merges
/// them into the first entry's own fields (`parse_index_blocks`), leaking
/// e.g. a header `CHOST:` into an entry that legitimately omitted one.
fn split_header_body(text: &str) -> (&str, &str) {
    let mut pos = 0usize;
    let mut rest = text;
    while !rest.is_empty() {
        let line_end = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
        let trimmed = rest[..line_end].trim_end_matches('\n');
        if trimmed.starts_with("CPV:") {
            return (&text[..pos], &text[pos..]);
        }
        if trimmed.is_empty() {
            return (&text[..pos], &text[pos + line_end..]);
        }
        pos += line_end;
        rest = &rest[line_end..];
    }
    (text, "")
}

/// Split a `Packages` index into its per-package `KEY: VALUE` blocks (the
/// header block is excluded — see `split_header_body`). Shared by every
/// consumer that needs a different subset of fields than [`BinpkgEntry`]
/// carries — e.g. `em maint binpkg`'s verify/list/prune, which also need
/// `MD5`/`SHA1`/`SIZE`/`BUILD_ID`.
pub fn parse_index_blocks(text: &str) -> Vec<BTreeMap<&str, &str>> {
    let (_, body) = split_header_body(text);
    let mut blocks = Vec::new();
    for block in body.split("\n\n") {
        let block = block.trim();
        if block.is_empty() || !block.lines().any(|l| l.starts_with("CPV:")) {
            continue;
        }
        let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
        for line in block.lines() {
            if let Some((k, v)) = line.split_once(": ") {
                fields.insert(k, v);
            }
        }
        if fields.contains_key("CPV") {
            blocks.push(fields);
        }
    }
    blocks
}

/// Derive the build-env key from a parsed `Packages` block's flag fields
/// (`CFLAGS`/`CXXFLAGS`/`LDFLAGS`/`RUSTFLAGS`, any subset absent) — the same
/// computation [`parse_packages_entries`] does per entry, extracted so
/// `em maint binpkg list`/`fingerprint`-style consumers can derive it from a
/// block without re-parsing the four fields by hand.
pub fn build_env_key_from_fields(fields: &BTreeMap<&str, &str>) -> String {
    build_env_key(
        fields.get("CFLAGS").copied().unwrap_or(""),
        fields.get("CXXFLAGS").copied().unwrap_or(""),
        fields.get("LDFLAGS").copied().unwrap_or(""),
        fields.get("RUSTFLAGS").copied().unwrap_or(""),
    )
}

/// Short, path-safe display/slug form of a build-env key: `"generic"` for the
/// empty key (unkeyed/no ISA-relevant flags), `"native"` for `"__native__"`,
/// else the first 12 hex chars of the MD5 of the full key. Shared by
/// `em maint binpkg list`'s KEY column and `em maint binpkg fingerprint`, so
/// the two correlate and the slug is usable as a PKGDIR path component
/// (`todo/binpkg-subtargets.md` recipe 1).
pub fn short_build_env_key(key: &str) -> String {
    match key {
        "" => "generic".to_string(),
        "__native__" => "native".to_string(),
        _ => hex::encode(md5::compute(key).0)[..12].to_string(),
    }
}

/// Parse a `Packages` index into `cpv → entry`. Shared by the local and remote
/// consumers (the only difference is how `path` is resolved: a local `PKGDIR`
/// join vs a remote `base_uri` join).
pub fn parse_packages_entries(text: &str) -> BTreeMap<String, Vec<BinpkgEntry>> {
    let mut entries: BTreeMap<String, Vec<BinpkgEntry>> = BTreeMap::new();
    for fields in parse_index_blocks(text) {
        let Some(&cpv) = fields.get("CPV") else {
            continue;
        };
        let cflags = fields.get("CFLAGS").copied().unwrap_or("").to_string();
        let cxxflags = fields.get("CXXFLAGS").copied().unwrap_or("").to_string();
        let ldflags = fields.get("LDFLAGS").copied().unwrap_or("").to_string();
        let rustflags = fields.get("RUSTFLAGS").copied().unwrap_or("").to_string();
        let build_env_key = build_env_key_from_fields(&fields);
        let build_id = fields
            .get("BUILD_ID")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let entry = BinpkgEntry {
            path: fields.get("PATH").copied().unwrap_or("").to_string(),
            use_set: split_use(fields.get("USE").copied().unwrap_or("")),
            iuse: split_iuse(fields.get("IUSE").copied().unwrap_or("")),
            chost: fields.get("CHOST").copied().unwrap_or("").to_string(),
            cflags,
            cxxflags,
            ldflags,
            rustflags,
            build_id,
            build_env_key,
        };
        entries.entry(cpv.to_string()).or_default().push(entry);
    }
    entries
}

/// Header fields of a `Packages` index (see `split_header_body` for where
/// the header ends). Used for server-controlled `URI` / BASE_URI.
pub fn parse_index_header(text: &str) -> BTreeMap<String, String> {
    let (header, _) = split_header_body(text);
    let header = header.trim();
    if header.is_empty() {
        return BTreeMap::new();
    }
    let mut fields = BTreeMap::new();
    for line in header.lines() {
        if let Some((k, v)) = line.split_once(": ") {
            fields.insert(k.to_string(), v.to_string());
        }
    }
    fields
}

/// A remote binhost's `Packages` index, parsed from a fetched index text and a
/// base URI. Mirrors [`BinpkgIndex`] but resolves each entry's `PATH` to a
/// download URL instead of a local file — used by `-g`/`--getbinpkg`.
#[derive(Debug, Clone)]
pub struct RemoteBinpkgIndex {
    entries: BTreeMap<String, Vec<BinpkgEntry>>,
    /// Download base: index header `URI` when present, else the configured
    /// `sync-uri` / `PORTAGE_BINHOST` the index was fetched from (Portage
    /// `bintree.py`: `remote_base_uri = pkgindex.header.get("URI", base_url)`).
    base_uri: String,
    /// This binhost's `binrepos.conf` `verify-signature` setting — carried
    /// alongside the index (not enforced here; the CLI reads it back via
    /// [`Self::verify_signature`] to build a `VerifyPolicy` for whatever it
    /// fetches from this index).
    verify_signature: bool,
}

impl RemoteBinpkgIndex {
    /// Build from a fetched index body and the binhost base URI (the `sync-uri`
    /// / `PORTAGE_BINHOST` entry the index was fetched from).
    ///
    /// If the index header carries `URI:`, that overrides `configured_base_uri`
    /// for every download path (server-controlled BASE_URI).
    pub fn new(index_text: &str, configured_base_uri: &str) -> Self {
        let header = parse_index_header(index_text);
        let base = header
            .get("URI")
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(configured_base_uri);
        Self {
            entries: parse_packages_entries(index_text),
            base_uri: base.trim_end_matches('/').to_string(),
            verify_signature: false,
        }
    }

    /// Set this binhost's `binrepos.conf` `verify-signature` setting
    /// (default `false` from [`Self::new`]).
    pub fn with_verify_signature(mut self, v: bool) -> Self {
        self.verify_signature = v;
        self
    }

    /// This binhost's `verify-signature` setting (see [`Self::with_verify_signature`]).
    pub fn verify_signature(&self) -> bool {
        self.verify_signature
    }

    /// The effective download base URI (after header `URI` override).
    pub fn base_uri(&self) -> &str {
        &self.base_uri
    }

    /// The number of indexed packages (for reporting).
    pub fn len(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
    }

    /// Whether the index has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find a reusable remote binpkg for `cpv`, returning its download URL.
    /// `None` if the cpv is absent or its USE/CHOST/build_env_key does not match
    /// (same rules as the local index). URL = `base_uri` + `/` + `PATH`.
    ///
    /// CFLAGS/CXXFLAGS/LDFLAGS are carried on the entry ([`Self::get`]) and
    /// checked via the computed build_env_key.
    pub fn find_reusable(
        &self,
        cpv: &str,
        desired_use: &[String],
        desired_chost: &str,
        desired_build_env_key: &str,
    ) -> Option<String> {
        let entries = self.entries.get(cpv)?;
        // Among all matching entries, prefer the newest BUILD_ID — not just
        // the first one listed (scan/parse order is not BUILD_ID order).
        let best = entries
            .iter()
            .filter(|entry| entry_reusable(entry, desired_use, desired_chost))
            .filter(|entry| build_env_key_compatible(&entry.build_env_key, desired_build_env_key))
            .max_by_key(|entry| entry.build_id)?;
        Some(format!("{}/{path}", self.base_uri, path = best.path))
    }

    /// Look up the raw index entries for `cpv` (including CFLAGS/CXXFLAGS/
    /// LDFLAGS), if present. Does not apply USE/CHOST reuse rules.
    /// Returns all entries for the CPV (there may be multiple variants).
    pub fn get(&self, cpv: &str) -> Option<&[BinpkgEntry]> {
        self.entries.get(cpv).map(|v| v.as_slice())
    }
}

fn entry_reusable(entry: &BinpkgEntry, desired_use: &[String], desired_chost: &str) -> bool {
    if !use_compatible(&entry.use_set, &entry.iuse, desired_use) {
        return false;
    }
    chost_compatible(&entry.chost, desired_chost)
}

/// CHOST gate: both sides set and unequal → not reusable. Either side empty
/// skips the check (sparse index / unknown desired CHOST).
fn chost_compatible(binpkg_chost: &str, desired_chost: &str) -> bool {
    if binpkg_chost.is_empty() || desired_chost.is_empty() {
        return true;
    }
    binpkg_chost == desired_chost
}

/// Build-env key gate. Asymmetric: an unkeyed binpkg (`binpkg_key` empty —
/// sparse index / old GPKG predating this field) is permissive, matching
/// anything (backward compatibility). But once a binpkg *is* keyed (built
/// with recorded ISA/ABI flags), an empty *desired* key (a generic/unknown
/// build) must not silently match it — that would reuse a march-specific
/// binpkg on a generic board. Both non-empty: exact match only.
fn build_env_key_compatible(binpkg_key: &str, desired_key: &str) -> bool {
    if binpkg_key.is_empty() {
        return true;
    }
    if desired_key.is_empty() {
        return false;
    }
    binpkg_key == desired_key
}

/// The reuse core: is a binpkg's `USE` (restricted to its `IUSE`) equal to the
/// desired `USE` (restricted to `IUSE`)? Flags outside `IUSE` (USE_EXPAND
/// defaults, profile-implicit flags) don't affect the package and are ignored.
/// This is portage's built-package USE check (bug #453400).
pub fn use_compatible(
    binpkg_use: &HashSet<String>,
    iuse: &HashSet<String>,
    desired_use: &[String],
) -> bool {
    for flag in iuse {
        let in_binpkg = binpkg_use.contains(flag);
        let in_desired = desired_use.iter().any(|d| d == flag);
        if in_binpkg != in_desired {
            return false;
        }
    }
    true
}

/// Compute a **sub-target / ISA identity** key for binpkg reuse.
///
/// Returns empty string if nothing ABI-relevant remains (skip gate).
/// Returns `"__native__"` if any retained flag is machine-dependent
/// (e.g. `-march=native`).
///
/// A binpkg is reusable iff CPV, USE∩IUSE, CHOST, and this key all match
/// (empty CHOST/key still skip their gates).
///
/// ## What goes into the key
///
/// Only **ABI / micro-arch** flags are kept, then fed to sokgi for
/// canonicalization + stable hash. Dropped as noise for board caches:
/// `-O*`, `-g*`, `-pipe`, warnings, defines, include/lib paths, most
/// `-f*`, generic linker args, Rust opt-level, etc.
///
/// Retained (C/CXX/LD):
/// - `-march=` / `-mcpu=` / `-mtune=` / `-mabi=` / `-mfpu=` / `-mfloat-abi=`
/// - a few bare ISA mode toggles (`-mthumb`, `-msoft-float`, …)
///
/// Retained (Rust):
/// - `-C target-cpu=…` / `-C target-feature=…` (joined or split forms)
///
/// Algorithm per flag set: filter → sokgi parse → MachineDependent check →
/// `stable_hash_hex`; non-empty hashes sorted and joined.
pub fn build_env_key(cflags: &str, cxxflags: &str, ldflags: &str, rustflags: &str) -> String {
    let mut all_hashes: Vec<String> = Vec::new();
    let mut has_machine_dependent = false;

    let flag_sets = [
        (filter_c_family_abi_flags(cflags), Dialect::C),
        (filter_c_family_abi_flags(cxxflags), Dialect::Cxx),
        (filter_c_family_abi_flags(ldflags), Dialect::Ld),
        (filter_rust_abi_flags(rustflags), Dialect::Rust),
    ];

    for (flags, dialect) in flag_sets {
        if flags.is_empty() {
            continue;
        }

        match FlagSet::parse(&flags, dialect) {
            Ok((set, warnings)) => {
                if warnings
                    .iter()
                    .any(|w| matches!(w, Warning::MachineDependent(_)))
                {
                    has_machine_dependent = true;
                }

                let hash = set.stable_hash_hex();
                if !hash.is_empty() {
                    all_hashes.push(hash);
                }
            }
            Err(_) => continue,
        }
    }

    if has_machine_dependent {
        return "__native__".to_string();
    }

    if all_hashes.is_empty() {
        return String::new();
    }

    // CFLAGS and CXXFLAGS commonly share the same -march/-mcpu selector (or
    // even the same value outright, e.g. CXXFLAGS="${CFLAGS}") -- dedupe so
    // that doesn't double the same hash in the key for no reason. Safe to
    // change: build_env_key is always recomputed from the raw CFLAGS/etc
    // fields (never itself persisted), so producer and consumer stay
    // self-consistent across this change.
    all_hashes.sort();
    all_hashes.dedup();
    all_hashes.join(" ")
}

/// Keep only ISA/ABI-relevant tokens from a GCC-style flag string.
fn filter_c_family_abi_flags(flags: &str) -> String {
    flags
        .split_whitespace()
        .filter(|t| is_c_family_abi_token(t))
        .collect::<Vec<_>>()
        .join(" ")
}

/// GCC/Clang document every `-m*` option as a "machine dependent option"
/// (target/ISA/ABI selector) — `-march=`/`-mcpu=`/`-mabi=`/`-mrvv-vector-bits=`/
/// `-mno-outline-atomics`/`-mavx2`/… An allowlist of specific flag names is
/// perpetually incomplete (a missed selector means silent wrong-arch binpkg
/// reuse); treat the whole `-m` namespace as ABI-relevant instead. Over-keying
/// only costs an extra rebuild (the safe direction) — see the design doc's
/// explicit "Policy B: stricter, more rebuilds" tradeoff.
fn is_c_family_abi_token(tok: &str) -> bool {
    tok.starts_with("-m") && tok != "-m"
}

/// Keep Rust target-cpu / target-feature settings; drop opt-level and noise.
fn filter_rust_abi_flags(flags: &str) -> String {
    let toks: Vec<&str> = flags.split_whitespace().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i];
        // `-C target-cpu=foo` or `-Ctarget-cpu=foo`
        if let Some(rest) = t.strip_prefix("-C") {
            let rest = rest.trim_start_matches('=');
            if rest.is_empty() {
                // Split form: `-C` `target-cpu=…`
                if let Some(next) = toks.get(i + 1)
                    && is_rust_abi_codegen(next)
                {
                    out.push(format!("-C {next}"));
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            if is_rust_abi_codegen(rest) {
                out.push(format!("-C {rest}"));
            }
            i += 1;
            continue;
        }
        // Unprefixed `target-cpu=…` is unusual; ignore.
        i += 1;
    }
    out.join(" ")
}

fn is_rust_abi_codegen(s: &str) -> bool {
    s.starts_with("target-cpu=") || s.starts_with("target-feature=")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn desired(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn identical_use_is_reusable() {
        // binpkg built with nls,debug; IUSE nls,debug,ssl; desired nls,debug.
        assert!(use_compatible(
            &set(&["nls", "debug"]),
            &set(&["nls", "debug", "ssl"]),
            &desired(&["nls", "debug"]),
        ));
    }

    #[test]
    fn differing_iuse_flag_is_stale() {
        // binpkg has nls on, desired has it off (within IUSE) → not reusable.
        assert!(!use_compatible(
            &set(&["nls", "debug"]),
            &set(&["nls", "debug", "ssl"]),
            &desired(&["debug"]),
        ));
    }

    #[test]
    fn non_iuse_flags_are_ignored() {
        // A USE_EXPAND/implicit flag not in IUSE (python_targets_3_13) differs,
        // but since it's not in IUSE it must not block reuse.
        assert!(use_compatible(
            &set(&["nls", "python_targets_3_13"]),
            &set(&["nls"]),
            &desired(&["nls"]),
        ));
        // And conversely.
        assert!(use_compatible(
            &set(&["nls"]),
            &set(&["nls"]),
            &desired(&["nls", "python_targets_3_13"]),
        ));
    }

    #[test]
    fn iuse_default_prefixes_are_stripped() {
        // IUSE="+ssl -debug" → flags ssl,debug. A binpkg with ssl off and desired
        // ssl on (within IUSE) is stale.
        let iuse = split_iuse("+ssl -debug nls");
        assert_eq!(iuse, set(&["ssl", "debug", "nls"]));
        assert!(!use_compatible(
            &set(&["nls"]),
            &iuse,
            &desired(&["nls", "ssl"])
        ));
    }

    #[test]
    fn ssl_off_in_both_is_reusable() {
        // ssl is in IUSE but off in both binpkg and desired → reusable.
        assert!(use_compatible(
            &set(&["nls"]),
            &set(&["nls", "ssl"]),
            &desired(&["nls"]),
        ));
    }

    #[test]
    fn parses_packages_index_blocks() {
        let text = "\
CHOST: aarch64-unknown-linux-gnu
VERSION: 0
PACKAGES: 2
TIMESTAMP: 1700000000

BUILD_ID: 1
CPV: app-test/foo-1.0
IUSE: +nls -debug
PATH: app-test/foo-1.0-1.gpkg.tar
SLOT: 0/5.1
USE: nls

CPV: app-test/bar-2.0
IUSE: ssl
PATH: app-test/bar-2.0-1.gpkg.tar
SLOT: 0
USE: ssl
";
        let idx = BinpkgIndex::parse(text, PathBuf::from("/pkgdir"));
        assert_eq!(idx.len(), 2);

        // foo: nls on matches desired [nls]; debug off in both → reusable.
        let p = idx
            .find_reusable("app-test/foo-1.0", &desired(&["nls"]), "", "")
            .unwrap();
        assert_eq!(p, PathBuf::from("/pkgdir/app-test/foo-1.0-1.gpkg.tar"));

        // foo with nls off → stale (nls is in IUSE, differs) → None.
        assert!(
            idx.find_reusable("app-test/foo-1.0", &desired(&[]), "", "")
                .is_none()
        );

        // bar: ssl matches → reusable.
        assert!(
            idx.find_reusable("app-test/bar-2.0", &desired(&["ssl"]), "", "")
                .is_some()
        );

        // Wrong cpv → None.
        assert!(
            idx.find_reusable("app-test/missing-9.9", &desired(&["nls"]), "", "")
                .is_none()
        );
    }

    #[test]
    fn chost_mismatch_rejects_reuse() {
        let text = "\
VERSION: 0

CPV: app-test/foo-1.0
CHOST: aarch64-unknown-linux-gnu
IUSE: nls
PATH: app-test/foo-1.0-1.gpkg.tar
USE: nls
";
        let idx = BinpkgIndex::parse(text, PathBuf::from("/pkgdir"));
        assert!(
            idx.find_reusable(
                "app-test/foo-1.0",
                &desired(&["nls"]),
                "aarch64-unknown-linux-gnu",
                ""
            )
            .is_some()
        );
        assert!(
            idx.find_reusable(
                "app-test/foo-1.0",
                &desired(&["nls"]),
                "riscv64-unknown-linux-gnu",
                ""
            )
            .is_none()
        );
        // Empty desired CHOST: no gate.
        assert!(
            idx.find_reusable("app-test/foo-1.0", &desired(&["nls"]), "", "")
                .is_some()
        );
    }

    #[test]
    fn remote_index_resolves_to_download_url() {
        // Same index text as the local case, but resolved against a binhost
        // base URI → find_reusable returns a URL, not a local path.
        let text = "\
VERSION: 0
PACKAGES: 1

CPV: app-test/foo-1.0
IUSE: +nls -debug
PATH: app-test/foo-1.0-1.gpkg.tar
USE: nls
";
        let idx = RemoteBinpkgIndex::new(text, "https://binhost.example/");
        assert_eq!(idx.len(), 1);
        // Trailing slash on base_uri is trimmed; URL = base + "/" + PATH.
        assert_eq!(
            idx.find_reusable("app-test/foo-1.0", &desired(&["nls"]), "", "",)
                .unwrap(),
            "https://binhost.example/app-test/foo-1.0-1.gpkg.tar"
        );
        // Stale USE → None (same use_compatible rule as the local index).
        assert!(
            idx.find_reusable("app-test/foo-1.0", &desired(&["nls", "debug"]), "", "",)
                .is_none()
        );
        // Unknown cpv → None.
        assert!(
            idx.find_reusable("app-test/missing-9.9", &desired(&["nls"]), "", "",)
                .is_none()
        );
    }

    #[test]
    fn parse_carries_cflags_cxxflags_and_gates_reuse_on_them() {
        let text = "\
VERSION: 0

CPV: app-test/foo-1.0
CHOST: riscv64-unknown-linux-gnu
CFLAGS: -O2 -pipe -march=rv64gcv
CXXFLAGS: -O2 -pipe -march=rv64gcv
IUSE: nls
PATH: app-test/foo-1.0-1.gpkg.tar
USE: nls
";
        let idx = BinpkgIndex::parse(text, PathBuf::from("/pkgdir"));
        let e = idx.get("app-test/foo-1.0").unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].cflags, "-O2 -pipe -march=rv64gcv");
        assert_eq!(e[0].cxxflags, "-O2 -pipe -march=rv64gcv");
        // Matching desired key → reusable.
        assert!(
            idx.find_reusable(
                "app-test/foo-1.0",
                &desired(&["nls"]),
                "riscv64-unknown-linux-gnu",
                &build_env_key(
                    "-O2 -pipe -march=rv64gcv",
                    "-O2 -pipe -march=rv64gcv",
                    "",
                    ""
                )
            )
            .is_some()
        );
        // This binpkg IS keyed (built with a specific -march). A generic/empty
        // desired key (a board with no ISA-specific CFLAGS) must not silently
        // reuse it — that would be wrong-arch reuse.
        assert!(
            idx.find_reusable(
                "app-test/foo-1.0",
                &desired(&["nls"]),
                "riscv64-unknown-linux-gnu",
                ""
            )
            .is_none()
        );
    }

    #[test]
    fn empty_desired_key_does_not_match_keyed_binpkg() {
        let text = "\
VERSION: 0

CPV: app-test/foo-1.0
CFLAGS: -march=rv64gcv
IUSE: nls
PATH: app-test/foo-1.0-1.gpkg.tar
USE: nls
";
        let idx = BinpkgIndex::parse(text, PathBuf::from("/pkgdir"));
        assert!(
            idx.find_reusable("app-test/foo-1.0", &desired(&["nls"]), "", "")
                .is_none()
        );
    }

    #[test]
    fn empty_binpkg_key_is_legacy_permissive() {
        // A binpkg with no CFLAGS metadata at all (pre-dates this field, or a
        // generic board) has an empty build_env_key — must still match any
        // desired key, keyed or not (backward compat with old GPKGs).
        let text = "\
VERSION: 0

CPV: app-test/foo-1.0
IUSE: nls
PATH: app-test/foo-1.0-1.gpkg.tar
USE: nls
";
        let idx = BinpkgIndex::parse(text, PathBuf::from("/pkgdir"));
        assert!(
            idx.find_reusable("app-test/foo-1.0", &desired(&["nls"]), "", "")
                .is_some()
        );
        assert!(
            idx.find_reusable(
                "app-test/foo-1.0",
                &desired(&["nls"]),
                "",
                &build_env_key("-march=rv64gcv", "", "", "")
            )
            .is_some()
        );
    }

    #[test]
    fn find_reusable_prefers_newest_build_id_among_matches() {
        let text = "\
VERSION: 0

CPV: app-test/foo-1.0
CHOST: riscv64-unknown-linux-gnu
IUSE: nls
PATH: app-test/foo-1.0-1.gpkg.tar
USE: nls
BUILD_ID: 1

CPV: app-test/foo-1.0
CHOST: riscv64-unknown-linux-gnu
IUSE: nls
PATH: app-test/foo-1.0-3.gpkg.tar
USE: nls
BUILD_ID: 3

CPV: app-test/foo-1.0
CHOST: riscv64-unknown-linux-gnu
IUSE: nls
PATH: app-test/foo-1.0-2.gpkg.tar
USE: nls
BUILD_ID: 2
";
        let idx = BinpkgIndex::parse(text, PathBuf::from("/pkgdir"));
        // Listed out of BUILD_ID order (1, 3, 2) — must still pick BUILD_ID 3.
        assert_eq!(
            idx.find_reusable(
                "app-test/foo-1.0",
                &desired(&["nls"]),
                "riscv64-unknown-linux-gnu",
                ""
            ),
            Some(PathBuf::from("/pkgdir/app-test/foo-1.0-3.gpkg.tar"))
        );
    }

    #[test]
    fn build_env_key_empty_for_empty_flags() {
        assert_eq!(build_env_key("", "", "", ""), "");
    }

    #[test]
    fn build_env_key_empty_when_only_noise_flags() {
        // Optimisation / debug / pipe must not create a sub-target key.
        assert_eq!(
            build_env_key("-O3 -pipe -g", "-O2 -Wall", "-Wl,--as-needed", ""),
            ""
        );
    }

    #[test]
    fn build_env_key_returns_native_for_march_native() {
        let key = build_env_key("-O2 -pipe -march=native", "", "", "");
        assert_eq!(key, "__native__");
    }

    #[test]
    fn build_env_key_returns_native_for_mcpu_native() {
        let key = build_env_key("", "-O2 -mcpu=native", "", "");
        assert_eq!(key, "__native__");
    }

    #[test]
    fn build_env_key_returns_native_for_mtune_native_in_cflags() {
        let key = build_env_key("-O2 -mtune=native", "", "", "");
        assert_eq!(key, "__native__");
    }

    #[test]
    fn build_env_key_different_for_different_march() {
        let key1 = build_env_key(
            "-O2 -pipe -march=rv64gcv",
            "-O2 -pipe -march=rv64gcv",
            "",
            "",
        );
        let key2 = build_env_key(
            "-O2 -pipe -march=rva23u64",
            "-O2 -pipe -march=rva23u64",
            "",
            "",
        );
        assert!(!key1.is_empty());
        assert!(!key2.is_empty());
        assert_ne!(key1, key2);
        assert_ne!(key1, "__native__");
        assert_ne!(key2, "__native__");
    }

    #[test]
    fn build_env_key_different_for_different_rvv_vector_bits() {
        // Not on the old explicit allowlist — same march, differing vector
        // width must still split the cache (riscv64 zvl boards).
        let key1 = build_env_key("-march=rv64gcv -mrvv-vector-bits=256", "", "", "");
        let key2 = build_env_key("-march=rv64gcv -mrvv-vector-bits=zvl", "", "", "");
        assert!(!key1.is_empty());
        assert!(!key2.is_empty());
        assert_ne!(key1, key2);
    }

    #[test]
    fn build_env_key_different_for_outline_atomics() {
        // aarch64: named explicitly in the design doc as a real ISA selector.
        let key1 = build_env_key("-march=armv8-a -mno-outline-atomics", "", "", "");
        let key2 = build_env_key("-march=armv8-a", "", "", "");
        assert!(!key1.is_empty());
        assert!(!key2.is_empty());
        assert_ne!(key1, key2);
    }

    #[test]
    fn build_env_key_different_for_x86_feature_toggle() {
        // x86 target-attribute-style flags aren't -march/-mcpu/-mtune but are
        // still ISA-affecting (avx2 vs no-avx2 code is not binary compatible).
        let key1 = build_env_key("-march=x86-64-v3 -mavx2", "", "", "");
        let key2 = build_env_key("-march=x86-64-v3 -mno-avx2", "", "", "");
        assert!(!key1.is_empty());
        assert!(!key2.is_empty());
        assert_ne!(key1, key2);
    }

    #[test]
    fn build_env_key_ignores_opt_level_and_order() {
        // Same -march, different -O / order / noise → same key (board identity).
        let key1 = build_env_key("-O2 -pipe -march=rv64gcv_zvl256b", "-O2 -g", "", "");
        let key2 = build_env_key(
            "-march=rv64gcv_zvl256b -O3 -pipe",
            "-O3 -Wall -pipe",
            "-Wl,-O1",
            "",
        );
        assert!(!key1.is_empty());
        assert_eq!(key1, key2);
    }

    #[test]
    fn build_env_key_same_for_equivalent_flags() {
        let key1 = build_env_key("-O2 -march=rv64gcv", "-g", "", "");
        let key2 = build_env_key("-march=rv64gcv -O2", "-g", "", "");
        assert_eq!(key1, key2);
    }

    #[test]
    fn build_env_key_ignores_generic_ldflags() {
        let key1 = build_env_key("-march=x86-64-v3", "", "", "");
        let key2 = build_env_key("-march=x86-64-v3", "", "-Wl,--as-needed -O1", "");
        assert_eq!(key1, key2);
    }

    #[test]
    fn build_env_key_dedupes_identical_cflags_cxxflags() {
        // CXXFLAGS mirroring CFLAGS (or sharing the same -march) is the
        // common case (e.g. CXXFLAGS="${CFLAGS}") -- must not double the
        // same hash in the key for no reason.
        let key = build_env_key("-march=x86-64-v3", "-march=x86-64-v3", "", "");
        assert!(!key.contains(' '), "expected one hash, got {key:?}");
        assert_eq!(key, build_env_key("-march=x86-64-v3", "", "", ""));
    }

    #[test]
    fn build_env_key_ignores_rust_opt_level() {
        // opt-level alone is not a sub-target.
        assert_eq!(build_env_key("", "", "", "-C opt-level=2"), "");
    }

    #[test]
    fn build_env_key_keeps_rust_target_cpu() {
        let key1 = build_env_key("", "", "", "-C target-cpu=generic");
        let key2 = build_env_key("", "", "", "-C target-cpu=native");
        assert!(!key1.is_empty());
        // native path: sokgi may not warn on rust dialect the same way — at
        // least the two cpus must differ if both parse.
        if key2 != "__native__" {
            assert_ne!(key1, key2);
        }
    }

    #[test]
    fn short_build_env_key_generic_native_and_hash() {
        assert_eq!(short_build_env_key(""), "generic");
        assert_eq!(short_build_env_key("__native__"), "native");

        let key = build_env_key("-march=rv64gcv", "", "", "");
        let slug = short_build_env_key(&key);
        assert_ne!(slug, "generic");
        assert_ne!(slug, "native");
        assert_eq!(slug.len(), 12);
        // Stable: same input -> same slug.
        assert_eq!(slug, short_build_env_key(&key));
    }

    #[test]
    fn remote_index_header_uri_overrides_sync_uri() {
        // Portage: remote_base_uri = pkgindex.header.get("URI", base_url).
        let text = "\
VERSION: 0
URI: https://cdn.example/gentoo/arm64

CPV: app-test/foo-1.0
IUSE: nls
PATH: app-test/foo-1.0-1.gpkg.tar
USE: nls
";
        let idx = RemoteBinpkgIndex::new(text, "https://binhost.example/unused/");
        assert_eq!(idx.base_uri(), "https://cdn.example/gentoo/arm64");
        assert_eq!(
            idx.find_reusable("app-test/foo-1.0", &desired(&["nls"]), "", "",)
                .unwrap(),
            "https://cdn.example/gentoo/arm64/app-test/foo-1.0-1.gpkg.tar"
        );
    }

    #[test]
    fn header_uri_survives_without_a_trailing_blank_line() {
        // Same as remote_index_header_uri_overrides_sync_uri but the header
        // is glued directly onto the first entry (no blank line) — a shape
        // real portage's own reader tolerates.
        let text = "\
VERSION: 0
URI: https://cdn.example/gentoo/arm64
CPV: app-test/foo-1.0
IUSE: nls
PATH: app-test/foo-1.0-1.gpkg.tar
USE: nls
";
        let idx = RemoteBinpkgIndex::new(text, "https://binhost.example/unused/");
        assert_eq!(idx.base_uri(), "https://cdn.example/gentoo/arm64");
        assert_eq!(
            idx.find_reusable("app-test/foo-1.0", &desired(&["nls"]), "", "",)
                .unwrap(),
            "https://cdn.example/gentoo/arm64/app-test/foo-1.0-1.gpkg.tar"
        );
    }

    #[test]
    fn glued_header_field_does_not_leak_into_first_entry() {
        // The header carries a CHOST-shaped line; the entry itself has none.
        // Before the shared header/body boundary, this would have merged
        // into the first entry's fields (BTreeMap built from every line in
        // the glued block) and made an unrelated binpkg falsely CHOST-gated.
        let text = "\
VERSION: 0
CHOST: this-is-a-header-value-not-an-entrys
CPV: app-test/foo-1.0
IUSE: nls
PATH: app-test/foo-1.0-1.gpkg.tar
USE: nls
";
        let idx = BinpkgIndex::parse(text, PathBuf::from("/pkgdir"));
        let e = idx.get("app-test/foo-1.0").unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(
            e[0].chost, "",
            "the header's CHOST must not leak into the entry"
        );
    }

    #[test]
    fn parse_index_header_empty_when_file_starts_with_cpv() {
        let text = "\
CPV: app-test/foo-1.0
PATH: app-test/foo-1.0-1.gpkg.tar
";
        assert!(parse_index_header(text).is_empty());
        assert_eq!(parse_index_blocks(text).len(), 1);
    }

    #[test]
    fn parse_index_header_of_header_only_file() {
        // No entries at all, no trailing blank line, straight to EOF.
        let text = "VERSION: 0\nPACKAGES: 0\n";
        let header = parse_index_header(text);
        assert_eq!(header.get("VERSION").map(String::as_str), Some("0"));
        assert!(parse_index_blocks(text).is_empty());
    }
}
