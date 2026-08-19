//! A structured winnow parse failure, kept structured rather than rendered
//! to a string at parse time.
//!
//! This crate exposes the diagnostic as a [`miette::Diagnostic`] (source +
//! span + label). **Rendering** — color, graphical theme, writing to a
//! stream — is the application's job (e.g. `portage_cli::diag`), not this
//! library's. Carrying a pre-rendered `String` through tracing / bus hops
//! is how an earlier version mangled ANSI escapes live.
//!
//! `miette` is already in this workspace's dependency graph (`brush-parser`
//! depends on it for its own diagnostics), so this adds no new crate to
//! resolve.

use miette::Diagnostic;

/// A structured `SRC_URI`/`LICENSE`/`REQUIRED_USE`/`RESTRICT` parse failure.
///
/// `Display` gives a short one-line summary (safe for log lines, `anyhow`
/// chains, activity-bus `error` fields). Implementors of a UI should treat
/// this as a [`miette::Diagnostic`] and render a code frame at display time
/// (see `portage_cli::diag::print_diagnostic`).
#[derive(Debug, Clone, PartialEq, Eq, Diagnostic)]
pub struct ParseDiagnostic {
    what: &'static str,
    #[source_code]
    src: String,
    #[label("{label}")]
    span: miette::SourceSpan,
    label: String,
}

impl std::fmt::Display for ParseDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid {}", self.what)
    }
}

impl std::error::Error for ParseDiagnostic {}

/// Roughly how much source text to keep on each side of the failing span.
///
/// SRC_URI/LICENSE/etc. values are whitespace-separated token soup with no
/// newlines, so there's no natural line length to rely on — a go-module
/// `SRC_URI` can run to a few thousand characters.
///
/// Without cropping, miette renders the *entire* value as one unbroken
/// "line", with the caret thousands of columns right of anything a
/// terminal shows — exactly the illegibility this module exists to fix.
const CONTEXT_CHARS: usize = 60;

/// Crop `src` to a window around `span`, snapped to whitespace so a token
/// isn't cut mid-word, and remap `span` into the cropped string's own byte
/// offsets.
///
/// A `…` marks a cropped edge. No-ops (returns `src`/`span` untouched)
/// when the string is already window-sized.
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

impl ParseDiagnostic {
    /// Build a diagnostic from a winnow parse failure.
    ///
    /// `what` names the grammar being parsed (e.g. `"SRC_URI"`). The label
    /// under the code frame is winnow's own accumulated context (e.g. a
    /// `StrContext::Label` from a `cut_err`), or a generic fallback when
    /// the failing parser pushed no context (most plain token/URI
    /// mismatches don't).
    pub(crate) fn from_winnow<E: std::fmt::Display>(
        what: &'static str,
        err: winnow::error::ParseError<&str, E>,
    ) -> Self {
        let label = err.inner().to_string();
        let label = if label.trim().is_empty() {
            "parsing stopped here".to_string()
        } else {
            label
        };
        let (src, span) = windowed(err.input(), err.char_span());
        Self {
            what,
            src,
            span: (span.start, span.len()).into(),
            label,
        }
    }

    /// Render a no-color miette code frame (for unit tests and plain logs).
    ///
    /// Production UIs should prefer a color-aware handler at the application
    /// boundary (`portage_cli::diag::print_diagnostic`).
    pub fn render_nocolor(&self) -> String {
        let handler =
            miette::GraphicalReportHandler::new_themed(miette::GraphicalTheme::unicode_nocolor());
        let mut out = String::new();
        handler
            .render_report(&mut out, self)
            .expect("String's Write impl never fails");
        out
    }
}
