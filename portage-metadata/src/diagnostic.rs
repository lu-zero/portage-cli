//! A structured winnow parse failure, kept structured rather than rendered
//! to a string at parse time — so the miette code frame is built exactly
//! once, at the point something actually displays the error, with a color
//! decision made fresh at *that* point.
//!
//! Rendering eagerly (at parse time, deep in this crate) and carrying the
//! result as a plain `String` through however many `Display` hops the
//! wrapping error types add on the way to a terminal is how a previous
//! version of this went wrong live: the pre-rendered ANSI bytes reached
//! `tracing::error!` intact, but the text that actually reached the PTY
//! afterwards was corrupted into literal `\x1b[...]` — the exact wrapping
//! path responsible was never conclusively found, and didn't need to be:
//! rendering lazily, right before display, sidesteps the whole class of
//! "mangled somewhere in transit" bugs by never handing a pre-rendered
//! string to anything in between.
//!
//! `miette` is already in this workspace's dependency graph (`brush-parser`
//! depends on it for its own diagnostics), so this adds no new crate to
//! resolve.

use miette::Diagnostic;

/// A structured `SRC_URI`/`LICENSE`/`REQUIRED_USE`/`RESTRICT` parse failure.
/// `Display` gives a short one-line summary (safe to use anywhere an
/// ordinary error is expected — log lines, `anyhow` chains, ...); call
/// [`ParseDiagnostic::render`] explicitly to get the full miette code frame,
/// at the point something is actually about to show it to a user.
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

impl ParseDiagnostic {
    /// Build a diagnostic from a winnow parse failure. `what` names the
    /// grammar being parsed (e.g. `"SRC_URI"`). The label under the code
    /// frame is winnow's own accumulated context (e.g. a `StrContext::Label`
    /// from a `cut_err`), or a generic fallback when the failing parser
    /// pushed no context (most plain token/URI mismatches don't).
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

    /// Render the full miette code frame as a string, deciding color fresh
    /// right now — not at construction time. `anstream::AutoStream::choice`
    /// reads the same global `colorchoice` state `colorchoice-clap`'s
    /// `--color` flag sets (plus `NO_COLOR`/real terminal detection) that
    /// every other bit of color in this codebase already goes through, so
    /// this can't disagree with whatever else is printing to the same
    /// stream at the same time.
    pub fn render(&self) -> String {
        let colored = !matches!(
            anstream::AutoStream::choice(&std::io::stderr()),
            anstream::ColorChoice::Never
        );
        let theme = if colored {
            miette::GraphicalTheme::unicode()
        } else {
            miette::GraphicalTheme::unicode_nocolor()
        };
        let handler = miette::GraphicalReportHandler::new_themed(theme);
        let mut out = String::new();
        handler
            .render_report(&mut out, self)
            .expect("String's Write impl never fails");
        out
    }
}
