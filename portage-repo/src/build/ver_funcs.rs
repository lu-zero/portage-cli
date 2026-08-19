//! PMS 12.3.14 version manipulation builtins: `ver_cut`, `ver_rs`, `ver_test`
//!
//! Implements these as Rust builtins rather than bash functions to avoid
//! issues with bash arithmetic evaluation in array slicing expressions.
//!
//! See [PMS 12.3.14](https://projects.gentoo.org/pms/9/pms.html#ver-funcs).

use std::cmp::Ordering;
use std::io::Write;
use std::sync::OnceLock;

use brush_core::builtins;
use clap::Parser;
use regex::Regex;

// ── Version splitting ──────────────────────────────────────────────────────

/// Split a version string into alternating (separator, component) pairs
///
/// Returns a flat `Vec<String>` where:
/// - Even indices (0, 2, 4, ...) are separators: non-ASCII-alphanumeric runs,
///   or empty strings at digit↔letter transitions.
/// - Odd indices (1, 3, 5, ...) are components: ASCII digit runs or letter runs.
///
/// For `"3.12.12"` returns `["", "3", ".", "12", ".", "12"]`.
///
/// See [PMS 12.3.14](https://projects.gentoo.org/pms/9/pms.html#ver-funcs).
fn ver_split(v: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut rest = v;

    while !rest.is_empty() {
        // Separator: leading non-ASCII-alphanumeric chars (LC_ALL=C semantics)
        let sep_end = rest
            .find(|c: char| c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        let sep = rest[..sep_end].to_string();
        rest = &rest[sep_end..];

        // Component: ASCII digit run or ASCII letter run
        let comp = if rest.starts_with(|c: char| c.is_ascii_digit()) {
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            rest[..end].to_string()
        } else {
            let end = rest
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(rest.len());
            rest[..end].to_string()
        };
        rest = &rest[comp.len()..];

        result.push(sep);
        result.push(comp);
    }

    result
}

// ── Range parsing ──────────────────────────────────────────────────────────

/// Parse a version range string into `(start, end)` indices (1-indexed, inclusive)
///
/// Formats:
/// - `"M"` → `(M, M)`
/// - `"M-N"` → `(M, N)`, end clamped to `max`
/// - `"M-"` → `(M, max)`
///
/// See [PMS 12.3.14](https://projects.gentoo.org/pms/9/pms.html#ver-funcs).
fn parse_range(range: &str, max: usize) -> Result<(usize, usize), String> {
    if !range.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(format!("range must start with a number: {range}"));
    }

    if let Some(dash_pos) = range.find('-') {
        let start: usize = range[..dash_pos]
            .parse()
            .map_err(|_| format!("invalid range start: {range}"))?;
        let end_str = &range[dash_pos + 1..];
        if end_str.is_empty() {
            // "M-" means from M to max
            Ok((start, max))
        } else {
            let raw_end: usize = end_str
                .parse()
                .map_err(|_| format!("invalid range end: {range}"))?;
            // Check reversed range on raw (pre-clamp) values, matching bash behaviour
            if start > raw_end {
                return Err(format!("end of range must be >= start: {range}"));
            }
            Ok((start, raw_end.min(max)))
        }
    } else {
        let n: usize = range
            .parse()
            .map_err(|_| format!("invalid range: {range}"))?;
        Ok((n, n.min(max)))
    }
}

// ── ver_rs ─────────────────────────────────────────────────────────────────

/// `ver_rs <range> <repl> [<range> <repl>]... [<version>]` — replace version separators
///
/// See [PMS 12.3.14](https://projects.gentoo.org/pms/9/pms.html#ver-funcs).
#[derive(Parser)]
pub(crate) struct VerRsCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for VerRsCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    fn new<I: IntoIterator<Item = String>>(args: I) -> Result<Self, clap::Error> {
        Ok(VerRsCommand {
            args: args.into_iter().skip(1).collect(),
        })
    }

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;

        // If odd number of args the last is the version string, otherwise use $PV.
        let pv_owned;
        let (pairs_args, v): (&[String], &str) = if self.args.len() % 2 == 1 {
            let v = self.args.last().map(|s| s.as_str()).unwrap_or("");
            (&self.args[..self.args.len() - 1], v)
        } else {
            pv_owned = shell
                .env_str("PV")
                .map(|cow| cow.into_owned())
                .unwrap_or_default();
            (&self.args[..], pv_owned.as_str())
        };

        let mut parts = ver_split(v);
        let max = parts.len() / 2;

        let mut i = 0;
        while i < pairs_args.len() {
            let range_str = &pairs_args[i];
            let repl = &pairs_args[i + 1];
            i += 2;

            let (start, end) = match parse_range(range_str, max) {
                Ok(r) => r,
                Err(e) => {
                    let _ = writeln!(context.params.stderr(shell), "die: ver_rs: {e}");
                    return Ok(brush_core::ExecutionResult::new(1));
                }
            };

            // Separator N is at flat index N*2.  Skip index 0 if it is empty
            // (no leading separator), matching bash ver_rs behaviour.
            for sep_idx in start..=end {
                let flat = sep_idx * 2;
                if flat >= parts.len() {
                    break;
                }
                if flat == 0 && parts[0].is_empty() {
                    continue;
                }
                parts[flat] = repl.clone();
            }
        }

        let output: String = parts.iter().map(|s| s.as_str()).collect();
        let _ = writeln!(context.params.stdout(shell), "{output}");
        Ok(brush_core::ExecutionResult::success())
    }
}

// ── ver_cut ────────────────────────────────────────────────────────────────

/// `ver_cut <range> [<version>]` — extract version components
///
/// Splits the version into (separator, component) pairs and concatenates
/// the pairs in the given 1-indexed range (inclusive).
///
/// See [PMS 12.3.14](https://projects.gentoo.org/pms/9/pms.html#ver-funcs).
#[derive(Parser)]
pub(crate) struct VerCutCommand {
    /// Component range: M, M-N, or M-
    range: String,
    /// Version string (defaults to `$PV`)
    version: Option<String>,
}

impl builtins::Command for VerCutCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    fn new<I: IntoIterator<Item = String>>(args: I) -> Result<Self, clap::Error> {
        // Bypass clap to avoid issues with empty-string args or hyphen values.
        let mut it = args.into_iter().skip(1); // first element is the program name
        let range = it.next().unwrap_or_default();
        let version = it.next();
        Ok(VerCutCommand { range, version })
    }

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;

        let pv_owned;
        let v: &str = if let Some(ref ver) = self.version {
            ver.as_str()
        } else {
            pv_owned = shell
                .env_str("PV")
                .map(|cow| cow.into_owned())
                .unwrap_or_default();
            pv_owned.as_str()
        };

        let pairs = ver_split(v);
        let max = pairs.len() / 2;

        let (start, end) = match parse_range(&self.range, max) {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(context.params.stderr(shell), "die: ver_cut: {e}");
                return Ok(brush_core::ExecutionResult::new(1));
            }
        };

        // Map 1-indexed component range to flat array slice.
        // Component N is at index 2N-1; its preceding separator is at 2N-2.
        // For start=0, include the leading separator at index 0.
        let array_start = if start > 0 { start * 2 - 1 } else { 0 };
        let array_end = end * 2;

        let output: String = if array_start < pairs.len() && array_end > array_start {
            let actual_end = array_end.min(pairs.len());
            pairs[array_start..actual_end]
                .iter()
                .map(|s| s.as_str())
                .collect()
        } else {
            String::new()
        };

        let _ = writeln!(context.params.stdout(shell), "{output}");
        Ok(brush_core::ExecutionResult::success())
    }
}

// ── Version comparison ─────────────────────────────────────────────────────

/// Compare two non-negative decimal integer strings of arbitrary length
///
/// Uses zero-padding to equalize lengths so that lexicographic order
/// matches numeric order (`"10"` > `"9"`, `""` == `"0"`).
fn compare_int(a: &str, b: &str) -> Ordering {
    let la = a.len();
    let lb = b.len();

    if la == lb {
        return a.cmp(b);
    }

    if la > lb {
        // Pad b to length la
        let padded: String = "0".repeat(la - lb) + b;
        a.cmp(padded.as_str())
    } else {
        // Pad a to length lb
        let padded: String = "0".repeat(lb - la) + a;
        padded.as_str().cmp(b)
    }
}

/// Compiled regex for PMS version format
fn ver_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^([0-9]+(\.[0-9]+)*)([a-z]?)((_(alpha|beta|pre|rc|p)[0-9]*)*)(-r[0-9]+)?$")
            .unwrap()
    })
}

/// Compare two Gentoo version strings per PMS algorithm 3.1
///
/// Returns `Some(Ordering)`, or `None` if either version is invalid.
fn ver_compare(va: &str, vb: &str) -> Option<Ordering> {
    let re = ver_re();
    let ca = re.captures(va)?;
    let cb = re.captures(vb)?;

    let an = ca.get(1).map_or("", |m| m.as_str());
    let al = ca.get(3).map_or("", |m| m.as_str());
    let as_ = ca.get(4).map_or("", |m| m.as_str());
    let ar = ca.get(7).map_or("", |m| m.as_str());

    let bn = cb.get(1).map_or("", |m| m.as_str());
    let bl = cb.get(3).map_or("", |m| m.as_str());
    let bs = cb.get(4).map_or("", |m| m.as_str());
    let br = cb.get(7).map_or("", |m| m.as_str());

    // PMS algorithm 3.2: compare first numeric component
    let mut an_rest = an;
    let mut bn_rest = bn;

    let a0 = an_rest.split('.').next().unwrap_or("0");
    let b0 = bn_rest.split('.').next().unwrap_or("0");
    let ord = compare_int(a0, b0);
    if ord != Ordering::Equal {
        return Some(ord);
    }
    // Advance past first component
    an_rest = an_rest.find('.').map_or("", |i| &an_rest[i + 1..]);
    bn_rest = bn_rest.find('.').map_or("", |i| &bn_rest[i + 1..]);

    // PMS algorithm 3.3: compare subsequent dot-separated numeric components
    loop {
        let a_next = if an_rest.is_empty() {
            None
        } else {
            Some(an_rest.split('.').next().unwrap_or(""))
        };
        let b_next = if bn_rest.is_empty() {
            None
        } else {
            Some(bn_rest.split('.').next().unwrap_or(""))
        };

        match (a_next, b_next) {
            (None, None) => break,
            (Some(_), None) => return Some(Ordering::Greater),
            (None, Some(_)) => return Some(Ordering::Less),
            (Some(a), Some(b)) => {
                let ord = if a.starts_with('0') || b.starts_with('0') {
                    // Strip trailing zeros then compare lexicographically
                    let a_trim = a.trim_end_matches('0');
                    let b_trim = b.trim_end_matches('0');
                    a_trim.cmp(b_trim)
                } else {
                    compare_int(a, b)
                };
                if ord != Ordering::Equal {
                    return Some(ord);
                }
                // Advance
                an_rest = an_rest.find('.').map_or("", |i| &an_rest[i + 1..]);
                bn_rest = bn_rest.find('.').map_or("", |i| &bn_rest[i + 1..]);
            }
        }
    }

    // PMS algorithm 3.4: compare letter components
    let ord = al.cmp(bl);
    if ord != Ordering::Equal {
        return Some(ord);
    }

    // PMS algorithm 3.5: compare suffixes.
    // Transform: strip leading `_`, add trailing `_` as sentinel.
    // e.g. "_alpha1_beta2" → "alpha1_beta2_"
    let mut as_rest: String = if as_.is_empty() {
        String::new()
    } else {
        format!("{}_", &as_[1..])
    };
    let mut bs_rest: String = if bs.is_empty() {
        String::new()
    } else {
        format!("{}_", &bs[1..])
    };

    loop {
        match (as_rest.is_empty(), bs_rest.is_empty()) {
            (true, true) => break,
            (false, true) => {
                // a has remaining suffixes; only _p is greater than no-suffix
                let word = suffix_word(&as_rest);
                return Some(if word == "p" {
                    Ordering::Greater
                } else {
                    Ordering::Less
                });
            }
            (true, false) => {
                let word = suffix_word(&bs_rest);
                return Some(if word == "p" {
                    Ordering::Less
                } else {
                    Ordering::Greater
                });
            }
            (false, false) => {
                // PMS algorithm 3.6: compare one suffix from each side
                let a_sfx = as_rest.split('_').next().unwrap_or("");
                let b_sfx = bs_rest.split('_').next().unwrap_or("");

                let a_word = a_sfx.trim_end_matches(|c: char| c.is_ascii_digit());
                let b_word = b_sfx.trim_end_matches(|c: char| c.is_ascii_digit());
                let a_num = &a_sfx[a_word.len()..];
                let b_num = &b_sfx[b_word.len()..];

                if a_word == b_word {
                    let ord = compare_int(a_num, b_num);
                    if ord != Ordering::Equal {
                        return Some(ord);
                    }
                } else {
                    if a_word == "p" {
                        return Some(Ordering::Greater);
                    }
                    if b_word == "p" {
                        return Some(Ordering::Less);
                    }
                    // alpha < beta < pre < rc alphabetically
                    return Some(a_word.cmp(b_word));
                }

                // Advance past this suffix (sfx.len() + 1 for the trailing `_`)
                as_rest = as_rest[a_sfx.len() + 1..].to_string();
                bs_rest = bs_rest[b_sfx.len() + 1..].to_string();
            }
        }
    }

    // PMS algorithm 3.7: compare revision components
    let a_rev = ar.strip_prefix("-r").unwrap_or("0");
    let b_rev = br.strip_prefix("-r").unwrap_or("0");
    let ord = compare_int(a_rev, b_rev);
    if ord != Ordering::Equal {
        return Some(ord);
    }

    Some(Ordering::Equal)
}

/// Extract the word (non-digit prefix) from the first suffix in a sentinel string
///
/// E.g. `"p10_"` → `"p"`, `"alpha_"` → `"alpha"`.
fn suffix_word(s: &str) -> &str {
    let sfx = s.split('_').next().unwrap_or("");
    sfx.trim_end_matches(|c: char| c.is_ascii_digit())
}

// ── ver_test ───────────────────────────────────────────────────────────────

/// `ver_test [<v1>] <op> <v2>` — compare version strings
///
/// Compares versions using PMS algorithm 3.1. The operator is one of
/// `-eq`, `-ne`, `-lt`, `-le`, `-gt`, `-ge`.
/// If only two arguments are given, `$PVR` is used as `v1`.
///
/// See [PMS 12.3.14](https://projects.gentoo.org/pms/9/pms.html#ver-funcs).
#[derive(Parser)]
pub(crate) struct VerTestCommand {
    /// Arguments: [v1] op v2
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for VerTestCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    fn new<I: IntoIterator<Item = String>>(args: I) -> Result<Self, clap::Error> {
        // Bypass clap to correctly preserve hyphen-prefixed operator args.
        Ok(VerTestCommand {
            args: args.into_iter().skip(1).collect(), // skip program name
        })
    }

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;

        let pvr_owned;
        let (va, op, vb): (&str, &str, &str) = match self.args.len() {
            2 => {
                pvr_owned = shell
                    .env_str("PVR")
                    .map(|cow| cow.into_owned())
                    .unwrap_or_default();
                (
                    pvr_owned.as_str(),
                    self.args[0].as_str(),
                    self.args[1].as_str(),
                )
            }
            3 => (
                self.args[0].as_str(),
                self.args[1].as_str(),
                self.args[2].as_str(),
            ),
            _ => {
                let _ = writeln!(
                    context.params.stderr(shell),
                    "die: ver_test: bad number of arguments"
                );
                return Ok(brush_core::ExecutionResult::new(1));
            }
        };

        let valid_ops = ["-eq", "-ne", "-lt", "-le", "-gt", "-ge"];
        if !valid_ops.contains(&op) {
            let _ = writeln!(
                context.params.stderr(shell),
                "die: ver_test: invalid operator: {op}"
            );
            return Ok(brush_core::ExecutionResult::new(1));
        }

        let Some(ord) = ver_compare(va, vb) else {
            let _ = writeln!(
                context.params.stderr(shell),
                "die: ver_test: invalid version: {va} or {vb}"
            );
            return Ok(brush_core::ExecutionResult::new(1));
        };

        let result = match op {
            "-eq" => ord == Ordering::Equal,
            "-ne" => ord != Ordering::Equal,
            "-lt" => ord == Ordering::Less,
            "-le" => ord != Ordering::Greater,
            "-gt" => ord == Ordering::Greater,
            "-ge" => ord != Ordering::Less,
            _ => unreachable!(),
        };

        Ok(brush_core::ExecutionResult::new(if result { 0 } else { 1 }))
    }
}

// ── ver_replacing ─────────────────────────────────────────────────────────────

/// `ver_replacing`  (PMS 12.3.14 / EAPI 9)
///
/// Outputs the versions being replaced, one per line.  During metadata
/// extraction no package is being replaced, so the output is always empty.
///
/// See [PMS 12.3.14](https://projects.gentoo.org/pms/9/pms.html#ver-funcs).
#[derive(Parser)]
pub(crate) struct VerReplacingCommand {}

impl builtins::Command for VerReplacingCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        _context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        // No package is being replaced during metadata extraction.
        Ok(brush_core::ExecutionResult::new(0))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ver_split_basic() {
        assert_eq!(ver_split("3.12.12"), ["", "3", ".", "12", ".", "12"]);
    }

    #[test]
    fn test_ver_split_letter_transition() {
        // digit→letter transition has empty separator
        assert_eq!(ver_split("1a"), ["", "1", "", "a"]);
        assert_eq!(ver_split("1.0a"), ["", "1", ".", "0", "", "a"]);
    }

    #[test]
    fn test_ver_split_single() {
        assert_eq!(ver_split("42"), ["", "42"]);
    }

    #[test]
    fn test_parse_range_single() {
        assert_eq!(parse_range("2", 3), Ok((2, 2)));
    }

    #[test]
    fn test_parse_range_dash() {
        assert_eq!(parse_range("1-2", 3), Ok((1, 2)));
    }

    #[test]
    fn test_parse_range_open() {
        assert_eq!(parse_range("2-", 3), Ok((2, 3)));
    }

    #[test]
    fn test_parse_range_clamp() {
        assert_eq!(parse_range("1-99", 3), Ok((1, 3)));
    }

    #[test]
    fn test_compare_int() {
        assert_eq!(compare_int("9", "10"), Ordering::Less);
        assert_eq!(compare_int("10", "9"), Ordering::Greater);
        assert_eq!(compare_int("", "0"), Ordering::Equal);
        assert_eq!(compare_int("0", ""), Ordering::Equal);
        assert_eq!(compare_int("1", "1"), Ordering::Equal);
        assert_eq!(compare_int("100", "99"), Ordering::Greater);
    }

    #[test]
    fn test_ver_compare_basic() {
        assert_eq!(ver_compare("1.0", "1.0"), Some(Ordering::Equal));
        assert_eq!(ver_compare("1.0", "1.1"), Some(Ordering::Less));
        assert_eq!(ver_compare("1.1", "1.0"), Some(Ordering::Greater));
        assert_eq!(ver_compare("2.0", "1.9"), Some(Ordering::Greater));
    }

    #[test]
    fn test_ver_compare_revision() {
        assert_eq!(ver_compare("1.0-r0", "1.0"), Some(Ordering::Equal));
        assert_eq!(ver_compare("1.0-r1", "1.0"), Some(Ordering::Greater));
        assert_eq!(ver_compare("1.0", "1.0-r1"), Some(Ordering::Less));
    }

    #[test]
    fn test_ver_compare_suffixes() {
        assert_eq!(ver_compare("1.0_alpha1", "1.0_beta1"), Some(Ordering::Less));
        assert_eq!(ver_compare("1.0_rc1", "1.0"), Some(Ordering::Less));
        assert_eq!(ver_compare("1.0_p1", "1.0"), Some(Ordering::Greater));
        assert_eq!(ver_compare("1.0_p1", "1.0_p2"), Some(Ordering::Less));
    }

    #[test]
    fn test_ver_compare_letter() {
        assert_eq!(ver_compare("1.0a", "1.0b"), Some(Ordering::Less));
        assert_eq!(ver_compare("1.0b", "1.0a"), Some(Ordering::Greater));
        assert_eq!(ver_compare("1.0a", "1.0a"), Some(Ordering::Equal));
    }
}
