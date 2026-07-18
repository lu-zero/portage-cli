#!/usr/bin/env bash
# Wall-clock ELF image scan: em (serial/parallel) vs Portage's scanelf.
#
# Portage generates NEEDED.ELF.2 via pax-utils `scanelf` during the merge
# (LinkageMapELF / install image scan). This script compares apples-to-apples
# *scan cost* on the same directory tree — not a full emerge merge.
#
# Usage:
#   benchmarks/bench-elfscan.sh [image-dir]
#
# Env:
#   IMAGE_DIR   default /usr/lib64 (or first of /usr/lib64 /usr/lib)
#   RUNS        hyperfine runs (default 5)
#   WARMUP      hyperfine warmup (default 1)
#   SKIP_SCANELF=1   only time em
#
# Requires: cargo, hyperfine; scanelf from pax-utils for the Portage side.

set -euo pipefail
cd "$(dirname "$0")/.."

pick_dir() {
    if [[ -n "${1:-}" && -d "$1" ]]; then
        echo "$1"
        return
    fi
    if [[ -n "${IMAGE_DIR:-}" && -d "${IMAGE_DIR}" ]]; then
        echo "${IMAGE_DIR}"
        return
    fi
    for c in /usr/lib64 /usr/lib /lib64 /lib; do
        if [[ -d "$c" ]]; then
            echo "$c"
            return
        fi
    done
    echo "/usr"
}

DIR=$(pick_dir "${1:-}")
RUNS=${RUNS:-5}
WARMUP=${WARMUP:-1}
JOBS=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)

echo "== elfscan bench tree: $DIR"
echo "== building release elfscan_bench example"
cargo build -p portage-cli --release --example elfscan_bench -q
EM_SCAN=(./target/release/examples/elfscan_bench)

# Smoke once so we print counts.
echo "== smoke (parallel)"
"${EM_SCAN[@]}" --jobs "$JOBS" "$DIR"
echo "== smoke (serial)"
"${EM_SCAN[@]}" --jobs 1 "$DIR"

if ! command -v hyperfine >/dev/null 2>&1; then
    echo "hyperfine not installed; smoke only" >&2
    exit 0
fi

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

cmds=(
    --warmup "$WARMUP"
    --runs "$RUNS"
    --export-markdown "$tmpdir/table.md"
    --export-json "$tmpdir/results.json"
    -n "em serial (jobs=1)"
    "${EM_SCAN[*]} --jobs 1 $DIR"
    -n "em parallel (jobs=$JOBS)"
    "${EM_SCAN[*]} --jobs $JOBS $DIR"
)

if [[ "${SKIP_SCANELF:-0}" != 1 ]] && command -v scanelf >/dev/null 2>&1; then
    # Portage LinkageMapELF invokes scanelf roughly as:
    #   scanelf -yBF '%a;%F;%S;%r;%n' <tree>
    # (see portage/util/_dyn_libs/LinkageMapELF.py). -y: recursive; -B: format.
    # We use a format close to NEEDED.ELF.2 fields for comparable work.
    SCANELF_FMT='%a;%F;%S;%r;%n'
    # Write to /dev/null so we measure scan, not terminal I/O.
    cmds+=(
        -n "scanelf (portage pax-utils)"
        "scanelf -yBF '$SCANELF_FMT' '$DIR' >/dev/null"
    )
    echo "== scanelf format: scanelf -yBF '$SCANELF_FMT' (Portage-like)"
else
    echo "== scanelf skipped (missing or SKIP_SCANELF=1)"
fi

echo "== hyperfine"
hyperfine "${cmds[@]}"

echo
echo "== markdown"
cat "$tmpdir/table.md"

# ELF counts from em for the write-up.
echo
echo "== em result counts (parallel)"
"${EM_SCAN[@]}" --jobs "$JOBS" "$DIR"
