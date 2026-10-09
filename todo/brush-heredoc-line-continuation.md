# The shell drops or rejects what follows a here-document on its line

Status: ✅ fixed 2026-10-09 in the brush fork (`fc8f96f6` on
`for-portage-repo`, pushed) and `em` moved to it. Found while fixing the
function printer ([[saved-environment-functions]]), in the fork's winnow
parser, which is the one `em` builds with (`experimental-parser`).

## What happens

Measured with the fork at `3f1ab573`, against bash:

| Line | brush | bash |
|------|-------|------|
| `cat <<EOF \| tr a-z A-Z` | correct | |
| `cat <<EOF >file \| cmd` | correct | |
| `cat <<EOF \| tr a-z A-Z \| rev` | second stage dropped, no error | runs both |
| `cat <<EOF && echo and` | `&& …` dropped, no error | runs it |
| `false <<EOF \|\| echo or` | `\|\| …` dropped, no error | runs it |
| `cat <<EOF \| tr a-z A-Z && echo and` | `&& …` dropped | runs it |
| `cat <<EOF; echo semi` | syntax error | runs both |
| `if cat <<EOF; then …; fi` | syntax error | runs |
| `( cat <<EOF ) \| tr a-z A-Z` | syntax error | runs |

## Why it matters here

`cat > file <<-EOF || die` is the standard way an ebuild writes a file.
The Gentoo tree has about 690 lines of that shape in ebuilds and
eclasses (grep, 2026-10-09). With `|| die` dropped the write still
happens, so builds succeed; what is lost is the failure check. A second
pipe stage after a heredoc occurs once in the tree, `;` never.

## Cause

`brush-parser/src/parser/winnow_str`: when the parser meets `<<EOF` it
reads the body at once and keeps the rest of the operator line as a
string (`pending_heredoc_trailing`). `pipe_sequence` then recovers from
that string the redirects and one `| command`. Nothing else looks at
it, so an and-or continuation is discarded, and a `;` or `)` that the
grammar expects next is no longer in the input.

## Direction

The body belongs after the line; the line should be parsed as it
stands. Two ways:

- Parse on past the operator, and read the bodies when the line's
  newline is reached, filling them into the redirects already built.
  This is how bash and the fork's other (PEG) parser behave. It touches
  how the winnow grammar consumes newlines.
- Keep the trailing string and teach each level to continue from it:
  the pipeline takes every `| command`, the and-or list takes `&&` and
  `||`, the list takes `;` and `&`. Smaller, but three places and still
  no answer for a compound command closing on that line (`fi`, `)`).

Three compat cases for this are in the fork as known failures
(`brush-shell/tests/cases/compat/here.yaml`).

## Fixed (2026-10-09)

The first direction. The operator's line is parsed in place; the body
is read ahead when the operator is met and its lines are recorded per
parse; the newline parser steps over them when it consumes the newline
ending the operator's line. The trailing-string plumbing is gone. An
unquoted delimiter now also ends at an operator character (`<<EOF;`).

Checked: every row of the table and 17 further shapes give bash's
output; a parser test has both parsers agree on twelve such lines; the
three compat cases pass; the fork's compat suite has 2498 succeeding and
0 failing. In `em`: an ebuild whose second `cat > … <<-EOF || die`
fails was installed as if it had worked before, and now dies.

Known limits, as before the change: the operator's line is taken to end
at the first newline, so a quoted string or a backslash continuation
that spans lines after the operator is not handled.
