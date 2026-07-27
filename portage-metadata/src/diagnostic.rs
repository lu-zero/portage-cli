//! Renders a winnow parse failure through miette, highlighting the exact
//! byte span where parsing stopped in its surrounding context — instead of
//! winnow's own bare-caret `Display`, which becomes unreadable once the
//! source is long (a `mirror://` `SRC_URI` with dozens of entries, say):
//! the caret sits under a single unbroken line, invisible once it scrolls
//! off-screen.
//!
//! `miette` is already in this workspace's dependency graph (`brush-parser`
//! depends on it for its own diagnostics), so this adds no new crate to
//! resolve — just a small local bridge from `winnow::error::ParseError` to
//! `miette::Diagnostic`.

use miette::Diagnostic;

#[derive(Debug, thiserror::Error, Diagnostic)]
#[error("{message}")]
struct WinnowDiagnostic {
    message: String,
    #[source_code]
    src: String,
    #[label("{label}")]
    span: miette::SourceSpan,
    label: String,
}

/// Roughly how much source text to keep on each side of the failing span.
/// SRC_URI/LICENSE/etc. values are whitespace-separated token soup with no
/// newlines, so unlike a real source file there is no natural line length to
/// rely on — a go-module `SRC_URI` can run to a few thousand characters.
/// Without cropping, miette renders the *entire* value as one unbroken
/// "line", and the caret ends up thousands of columns to the right of
/// anything a terminal actually shows — exactly the illegibility this module
/// exists to fix.
const CONTEXT_CHARS: usize = 60;

/// Crop `src` to a window around `span`, snapped to whitespace so a token
/// isn't cut mid-word, and remap `span` into the cropped string's own byte
/// offsets. A `…` marks a cropped edge. No-ops (returns `src`/`span`
/// untouched) when the string is already window-sized.
fn windowed(src: &str, span: std::ops::Range<usize>) -> (String, std::ops::Range<usize>) {
    let span_len = span.end - span.start;
    if src.len() <= CONTEXT_CHARS * 2 + span_len + 10 {
        return (src.to_string(), span);
    }

    let mut start = span.start.saturating_sub(CONTEXT_CHARS);
    while start > 0 && !src.is_char_boundary(start) {
        start -= 1;
    }
    start = src[..start].rfind(char::is_whitespace).map_or(0, |i| i + 1);

    let mut end = (span.end + CONTEXT_CHARS).min(src.len());
    while end < src.len() && !src.is_char_boundary(end) {
        end += 1;
    }
    end += src[end..]
        .find(char::is_whitespace)
        .unwrap_or(src.len() - end);

    let prefix = if start > 0 { "… " } else { "" };
    let suffix = if end < src.len() { " …" } else { "" };
    let cropped = format!("{prefix}{}{suffix}", &src[start..end]);
    let shift = prefix.len() as isize - start as isize;
    let new_span = (span.start as isize + shift) as usize..(span.end as isize + shift) as usize;
    (cropped, new_span)
}

/// Render a winnow parse failure as a miette-formatted string with a code
/// frame. `what` names the grammar being parsed (e.g. `"SRC_URI"`), used in
/// the top-line message; the label under the code frame is winnow's own
/// accumulated context (e.g. a `StrContext::Label` from a `cut_err`), or a
/// generic fallback when the failing parser pushed no context (most plain
/// token/URI mismatches don't).
pub(crate) fn render<E: std::fmt::Display>(
    what: &str,
    err: winnow::error::ParseError<&str, E>,
) -> String {
    let label = err.inner().to_string();
    let label = if label.trim().is_empty() {
        "parsing stopped here".to_string()
    } else {
        label
    };
    let (src, span) = windowed(err.input(), err.char_span());
    let diag = WinnowDiagnostic {
        message: format!("invalid {what}"),
        src,
        span: (span.start, span.len()).into(),
        label,
    };
    // Explicitly no color, regardless of what miette's own terminal
    // detection would decide: this string is built here, far from any
    // terminal, then carried through `tracing::error!` to a writer this
    // crate never sees — a second, independent color decision on top of
    // whatever that writer already makes is how raw ANSI bytes end up
    // mis-handled (observed live: literal `\x1b[...]` text instead of an
    // actual color change). Keep the box-drawing/unicode structure, drop
    // color entirely; this crate has no business deciding that anyway.
    let handler =
        miette::GraphicalReportHandler::new_themed(miette::GraphicalTheme::unicode_nocolor());
    let mut out = String::new();
    handler
        .render_report(&mut out, &diag)
        .expect("String's Write impl never fails");
    out
}
