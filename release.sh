#!/usr/bin/env bash
# release.sh — publish the workspace's published crates to crates.io.
#
# Usage: release.sh [--dry-run] [--yes] [--crate NAME]...
#   --dry-run        validate packaging via `cargo publish --dry-run`; no upload
#   --yes, -y        skip the upload confirmation prompt
#   --crate NAME     publish only NAME (repeatable); order stays topological
#   --help, -h       show this help
#
# Publishes the ten published crates in dependency order so each crate's
# dependencies are already on crates.io when it uploads. A crate already
# published at its current Cargo.toml version is skipped, so re-running
# after a partial failure resumes where it left off.
#
# The crates.io sparse index can lag a few seconds between a dependency
# landing and its dependents resolving it; the script detects that specific
# failure ("candidate versions ... didn't match") and retries with backoff.
#
# Requires: cargo (logged in or CARGO_REGISTRY_TOKEN set), network access.
# Run from the workspace root.

set -euo pipefail

# Publish order = topological (dependencies before dependents). Group breaks
# (the blank line) are cosmetic only; within a group the order is arbitrary.
ALL_ORDER=(
    gentoo-interner
    gentoo-core
    portage-atom

    portage-solver
    portage-metadata
    portage-atom-resolvo
    portage-binpkg
    gentoo-stages

    portage-atom-pubgrub
    portage-vdb
)

# How long to keep retrying a dependent whose just-published dep has not
# propagated through the crates.io sparse index yet. Override via env.
INDEX_LAG_RETRIES=${INDEX_LAG_RETRIES:-12}   # ~6 min ceiling at 30 s cadence
INDEX_LAG_WAIT=${INDEX_LAG_WAIT:-30}
CARGO=${CARGO:-cargo}

DRY_RUN=no
ASSUME_YES=no
SELECTED=()

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
}

die() { echo "release.sh: $*" >&2; exit 1; }

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --dry-run)    DRY_RUN=yes; shift ;;
            --yes|-y)     ASSUME_YES=yes; shift ;;
            --crate)      [ $# -ge 2 ] || die "--crate needs a NAME"; SELECTED+=("$2"); shift 2 ;;
            --help|-h)    usage; exit 0 ;;
            *)            die "unknown flag: $1 (try --help)" ;;
        esac
    done
    # Default: every published crate.
    if [ ${#SELECTED[@]} -eq 0 ]; then
        SELECTED=("${ALL_ORDER[@]}")
    else
        # Re-project the selection onto the topological order so --crate
        # flags given out of order still publish deps before dependents.
        local filtered=() found
        for c in "${ALL_ORDER[@]}"; do
            for s in "${SELECTED[@]}"; do
                [ "$c" = "$s" ] && { filtered+=("$c"); break; }
            done
        done
        SELECTED=("${filtered[@]}")
        [ ${#SELECTED[@]} -gt 0 ] || die "no --crate matched a known published crate"
    fi
}

# Read a crate's version from its [package] section.
crate_version() {
    local crate="$1"
    [ -f "${crate}/Cargo.toml" ] || die "$crate/Cargo.toml not found (run from workspace root)"
    awk -F'"' '/^version[[:space:]]*=/ {print $2; exit}' "${crate}/Cargo.toml"
}

# Classify a `cargo publish` failure stream. Echoes one of:
#   already   — version is already on crates.io (treat as success)
#   index-lag — a workspace dep was just published but the sparse index has
#               not caught up yet (retry)
#   other     — real error (abort)
classify_failure() {
    if echo "$1" | grep -qiE 'already (published|exists)|status 409|409 conflict'; then
        echo already
    elif echo "$1" | grep -qE "candidate versions? found which didn.t match"; then
        echo index-lag
    else
        echo other
    fi
}

publish_real() {
    local crate="$1" version attempt=0 out kind
    version=$(crate_version "$crate")
    while :; do
        if out=$("$CARGO" publish -p "$crate" --color always 2>&1); then
            printf '  [ OK ]   %-26s %s\n' "$crate $version" "published"
            return 0
        fi
        kind=$(classify_failure "$out")
        case "$kind" in
            already)
                printf '  [SKIP]   %-26s %s\n' "$crate $version" "already on crates.io"
                return 0
                ;;
            index-lag)
                attempt=$((attempt + 1))
                if [ "$attempt" -le "$INDEX_LAG_RETRIES" ]; then
                    printf '  [WAIT]   %-26s index lag, retry %d/%d in %ds\n' \
                        "$crate $version" "$attempt" "$INDEX_LAG_RETRIES" "$INDEX_LAG_WAIT"
                    sleep "$INDEX_LAG_WAIT"
                    continue
                fi
                printf '  [FAIL]   %-26s index never propagated after %d retries\n' \
                    "$crate $version" "$INDEX_LAG_RETRIES"
                ;;
            *)
                printf '  [FAIL]   %-26s cargo publish failed\n' "$crate $version"
                ;;
        esac
        printf '%s\n' "$out" | sed 's/^/           /' >&2
        return 1
    done
}

publish_dry() {
    local crate="$1" version out kind
    version=$(crate_version "$crate")
    if out=$("$CARGO" publish -p "$crate" --dry-run --color always 2>&1); then
        printf '  [ OK ]   %-26s %s\n' "$crate $version" "packaging valid"
        return 0
    fi
    kind=$(classify_failure "$out")
    # In dry-run, index-lag is expected for every non-leaf crate (its dep is
    # not on the registry yet). Report it as expected, not a failure.
    if [ "$kind" = index-lag ]; then
        printf '  [ OK ]   %-26s %s\n' "$crate $version" "packaging valid (dep not on registry yet)"
        return 0
    fi
    printf '  [FAIL]   %-26s packaging invalid\n' "$crate $version"
    printf '%s\n' "$out" | sed 's/^/           /' >&2
    return 1
}

main() {
    parse_args "$@"

    # Guard: must be at the workspace root.
    grep -q '^\[workspace\]' Cargo.toml 2>/dev/null \
        || die "no [workspace] Cargo.toml here — run from the workspace root"

    printf 'Publishing %d crate(s), topological order:\n' "${#SELECTED[@]}"
    for c in "${SELECTED[@]}"; do
        printf '    %-24s %s\n' "$c" "$(crate_version "$c")"
    done
    echo

    if [ "$DRY_RUN" = yes ]; then
        echo "Dry-run — validating packaging only, no upload."
        echo "Non-leaf crates report 'dep not on registry yet'; that is expected."
        echo
        local rc=0
        for c in "${SELECTED[@]}"; do publish_dry "$c" || rc=1; done
        echo
        if [ "$rc" -eq 0 ]; then echo "All packaging valid."; else echo "Some crates failed."; fi
        return "$rc"
    fi

    if [ "$ASSUME_YES" = no ]; then
        echo "About to UPLOAD to crates.io. Dependencies must already be published"
        echo "in order; a failure stops the chain. Type 'yes' to proceed:"
        local ans
        read -r ans || die "no confirmation (stdin closed — pass --yes)"
        [ "$ans" = "yes" ] || { echo "aborted."; exit 1; }
        echo
    fi

    local rc=0
    for c in "${SELECTED[@]}"; do
        publish_real "$c" || rc=1
        # A dependent cannot resolve until its dep is published, so a real
        # failure stops the chain immediately.
        if [ "$rc" -ne 0 ]; then
            echo "Stopping: $c failed; its dependents cannot be published yet."
            echo "Fix the issue and re-run — already-published crates are skipped."
            break
        fi
    done
    echo
    if [ "$rc" -eq 0 ]; then echo "All crates published."; else echo "Publish incomplete."; fi
    return "$rc"
}

main "$@"
