//! SPDX -> Gentoo license mapping — mirrors `pycargoebuild/license.py`.
//! Uses `spdx` crate for SPDX parsing + `minijinja` for ebuild `LICENSE` wrapping.
//! Mapping file is `/var/db/repos/gentoo/metadata/license-mapping.conf` (fallback).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

/// Minimal extra mapping loader — `pycargoebuild.toml:license-mapping` + `paths.license-mapping`
pub fn load_mapping(path: &Path) -> Result<HashMap<String, String>> {
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("reading license-mapping {}", path.display()))?;
    let mut map = HashMap::new();
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            continue;
        }
        // format: `SPDX = Gentoo` where SPDX may be `Apache-2.0 WITH LLVM-exception`
        if let Some((k, v)) = line.split_once('=') {
            let spdx = k.trim().to_lowercase();
            let gentoo = v.trim().trim_matches('"').to_string();
            if !spdx.is_empty() && !gentoo.is_empty() {
                map.insert(spdx, gentoo);
            }
        } else {
            // fallback whitespace split (older pycargoebuild)
            let mut iter = line.split_whitespace();
            if let (Some(spdx), Some(gentoo)) = (iter.next(), iter.next()) {
                map.insert(spdx.to_lowercase(), gentoo.trim_matches('"').to_string());
            }
        }
    }
    Ok(map)
}

/// A boolean license tree, built from the postfix `spdx::Expression` token
/// stream. Same-operator nodes are flattened as they're built (n-ary
/// `And`/`Or`, not a binary tree) so the renderer below can decide grouping.
enum LicenseNode<'e> {
    Leaf(&'e spdx::expression::ExpressionReq),
    And(Vec<LicenseNode<'e>>),
    Or(Vec<LicenseNode<'e>>),
}

/// Convert the expression's postfix token stream into a [`LicenseNode`] tree
fn build_tree(expr: &spdx::Expression) -> LicenseNode<'_> {
    use spdx::expression::{ExprNode, Operator};

    let mut stack: Vec<LicenseNode> = Vec::new();
    for node in expr.iter() {
        match node {
            ExprNode::Req(req) => stack.push(LicenseNode::Leaf(req)),
            ExprNode::Op(op) => {
                let b = stack.pop().expect("well-formed postfix expression");
                let a = stack.pop().expect("well-formed postfix expression");
                let combined = match op {
                    Operator::And => {
                        let mut children = flatten_and(a);
                        children.extend(flatten_and(b));
                        LicenseNode::And(children)
                    }
                    Operator::Or => {
                        let mut children = flatten_or(a);
                        children.extend(flatten_or(b));
                        LicenseNode::Or(children)
                    }
                };
                stack.push(combined);
            }
        }
    }
    stack.pop().expect("expression has at least one term")
}

fn flatten_and(node: LicenseNode<'_>) -> Vec<LicenseNode<'_>> {
    match node {
        LicenseNode::And(children) => children,
        other => vec![other],
    }
}

fn flatten_or(node: LicenseNode<'_>) -> Vec<LicenseNode<'_>> {
    match node {
        LicenseNode::Or(children) => children,
        other => vec![other],
    }
}

/// The Gentoo-mapped token sequence for one SPDX license requirement leaf —
/// same key construction as before (`id[+][ WITH exception]`, lower-cased,
/// `+` stripped as a fallback).
fn mapped_leaf_tokens(
    req: &spdx::expression::ExpressionReq,
    mapping: &HashMap<String, String>,
) -> Vec<String> {
    let lic = &req.req.license;
    let exception = &req.req.exception;
    let key = match lic {
        spdx::LicenseItem::Spdx { id, or_later } => {
            let base = id.name.to_string();
            let mut k = if *or_later { format!("{base}+") } else { base };
            if let Some(exc) = exception {
                k.push_str(&format!(" WITH {}", exc.name));
            }
            k
        }
        spdx::LicenseItem::Other { .. } => lic.to_string(),
    };
    let lower = key.to_lowercase();
    let mapped = mapping
        .get(&lower)
        .or_else(|| {
            lower
                .ends_with('+')
                .then(|| mapping.get(lower.trim_end_matches('+')))
                .flatten()
        })
        .cloned()
        .unwrap_or_else(|| match lic {
            spdx::LicenseItem::Spdx { id, .. } => id.name.to_string(),
            _ => lic.to_string(),
        });
    mapped.split_whitespace().map(str::to_string).collect()
}

/// Is `tokens` a single, unnested `|| ( ... )` any-of group — i.e. can it be
/// unwrapped and re-nested directly into an outer `|| ( ... )` without a
/// redundant `|| ( || ( ... ) )`?
fn is_pure_or(tokens: &[String]) -> bool {
    let mut it = tokens.iter();
    if it.next().map(String::as_str) != Some("||") {
        return false;
    }
    if it.next().map(String::as_str) != Some("(") {
        return false;
    }
    let mut depth = 1i32;
    for (i, tok) in it.enumerate() {
        match tok.as_str() {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return i == tokens.len() - 3; // must be the last token
                }
            }
            _ if depth == 0 => return false,
            _ => {}
        }
    }
    false
}

/// Render a [`LicenseNode`] tree into Gentoo `LICENSE=` tokens, preserving
/// AND/OR grouping — independent reimplementation of the same algorithm
/// shape `pycargoebuild/license.py`'s `sub()` walk uses (any-of flattening
/// for boolean-expression pretty-printing), not a transliteration.
fn render(
    node: &LicenseNode<'_>,
    in_or: bool,
    mapping: &HashMap<String, String>,
    out: &mut Vec<String>,
) {
    match node {
        LicenseNode::And(children) => {
            if in_or {
                out.push("(".to_string());
            }
            for child in children {
                render(child, false, mapping, out);
            }
            if in_or {
                out.push(")".to_string());
            }
        }
        LicenseNode::Or(children) => {
            if !in_or {
                out.push("||".to_string());
                out.push("(".to_string());
            }
            for child in children {
                render(child, true, mapping, out);
            }
            if !in_or {
                out.push(")".to_string());
            }
        }
        LicenseNode::Leaf(req) => {
            let mapped = mapped_leaf_tokens(req, mapping);
            if mapped.len() > 1 && in_or {
                if is_pure_or(&mapped) {
                    // Already `|| ( ... )` — unwrap so we don't nest
                    // `|| ( || ( ... ) )`.
                    out.extend(mapped[2..mapped.len() - 1].iter().cloned());
                } else {
                    out.push("(".to_string());
                    out.extend(mapped);
                    out.push(")".to_string());
                }
            } else {
                out.extend(mapped);
            }
        }
    }
}

/// Map an SPDX-2.0 expression (e.g. `(MIT OR Apache-2.0) AND Unicode-3.0`)
/// to Gentoo `LICENSE=` syntax, preserving the AND/OR structure.
///
/// `spdx::Expression` only exposes a flat `requirements()` iterator over
/// leaves, so the tree is rebuilt from its postfix token stream
/// (`build_tree`) before rendering, rather than flattening every leaf into
/// one `|| ( ... )` regardless of grouping.
pub fn spdx_to_ebuild(spdx: &str, mapping: &HashMap<String, String>) -> Result<String> {
    let expr = spdx::Expression::parse(spdx).context("spdx parse")?;
    let tree = build_tree(&expr);
    let mut tokens = Vec::new();
    render(&tree, false, mapping, &mut tokens);
    let joined = tokens.join(" ");
    // Validate through `portage_metadata::LicenseExpr`
    let _ = portage_metadata::LicenseExpr::parse(&joined).context("gentoo license parse")?;
    Ok(joined)
}

/// Wrap license var `LICENSE="..."` with `~80` col wrapping — mirrors `pycargoebuild/format.py:format_license_var`
pub fn format_license_var(value: &str, prefix: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let full = format!("{prefix}{value}\"");
    if full.len() <= 80 {
        return value.to_string();
    }
    // multiline: `\n\t...` per `portage_metadata` formatting
    format!("\n\t{}", value.replace(' ', "\n\t"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping() -> HashMap<String, String> {
        [
            ("mit", "MIT"),
            ("apache-2.0", "Apache-2.0"),
            ("unicode-3.0", "Unicode-3.0"),
            ("bsd-2-clause", "BSD-2"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn single_license() {
        assert_eq!(spdx_to_ebuild("MIT", &mapping()).unwrap(), "MIT");
    }

    #[test]
    fn plain_or() {
        assert_eq!(
            spdx_to_ebuild("MIT OR Apache-2.0", &mapping()).unwrap(),
            "|| ( MIT Apache-2.0 )"
        );
    }

    #[test]
    fn plain_and() {
        assert_eq!(
            spdx_to_ebuild("MIT AND Apache-2.0", &mapping()).unwrap(),
            "MIT Apache-2.0"
        );
    }

    /// Regression: an OR-of-licenses combined with a mandatory AND-ed
    /// license used to collapse into a single flat `|| ( ... )`, silently
    /// turning "pick one, plus this mandatory license" into "pick any one".
    #[test]
    fn or_and_mandatory_and_preserves_grouping() {
        let out = spdx_to_ebuild("(MIT OR Apache-2.0) AND Unicode-3.0", &mapping()).unwrap();
        assert_eq!(out, "|| ( MIT Apache-2.0 ) Unicode-3.0");
    }

    #[test]
    fn and_nested_inside_or_gets_explicit_group() {
        let out = spdx_to_ebuild("(MIT AND BSD-2-Clause) OR Apache-2.0", &mapping()).unwrap();
        assert_eq!(out, "|| ( ( MIT BSD-2 ) Apache-2.0 )");
    }
}
