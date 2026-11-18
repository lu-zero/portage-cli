# Local equivalent of CI, plus documentation checks. Run `just` to list recipes.

default:
    @just --list

# Formatting, lints, rustdoc, documentation, and the test suite
check: fmt-check docs-links clippy doc docs-check test

# Apply rustfmt
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --exclude portage-bench -- -D warnings

# Rustdoc warnings (broken links, bad code blocks) are hard errors in CI
doc:
    RUSTDOCFLAGS='-D warnings' cargo doc --workspace --exclude portage-bench --no-deps

# Unit, integration and doc tests (what CI's test job runs)
test:
    cargo test --workspace --exclude portage-bench

# Regenerate the CLI reference in docs/user/cli from what em's derives write
docs:
    WINNOW_ARGS_SPEC="$PWD/target/cli-spec/$(date +%s%N)" UPDATE_CLI_DOCS=1 cargo test -p portage-cli --test cli_docs

# Fail if docs/user/cli is stale by content (CI only checks the page set); regenerates in place
docs-check:
    #!/usr/bin/env bash
    set -euo pipefail
    before=$(mktemp -d)
    trap 'rm -rf "$before"' EXIT
    cp -r docs/user/cli/. "$before"
    WINNOW_ARGS_SPEC="$PWD/target/cli-spec/$(date +%s%N)" UPDATE_CLI_DOCS=1 cargo test -q -p portage-cli --test cli_docs
    if ! diff -r "$before" docs/user/cli >/dev/null; then
        echo "docs/user/cli was out of date and has been regenerated; review and commit it" >&2
        exit 1
    fi

# Build the em.luminem.org site from docs/ into target/site
site:
    python3 site/summary.py
    mdbook build site

# Serve the site at http://localhost:3000, rebuilding as docs/ changes
site-serve:
    python3 site/summary.py
    mdbook serve site

# Dead links and doc paths in Markdown and Rust doc comments
docs-links:
    ./check-doc-links.py
