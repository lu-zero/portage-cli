#!/usr/bin/env bash
# Wall-clock + package-count compare for resolve flag modes vs Portage.
#
# Covers the shallow / update / deep / newuse matrix:
#   -p  -up  -uNp  -uDp  -uNDp
#
# Usage:
#   benchmarks/bench-resolve-modes.sh [path-to-em]
#
# Env:
#   EM          binary (arg overrides; default target/release/em)
#   PKG         atom (default www-client/firefox)
#   RUNS        hyperfine runs (default 5)
#   WARMUP      hyperfine warmup (default 1)
#   SKIP_TIMING=1   counts/parity only
#   MODES       space-separated flag sets (default: all five)
#
# Requires: hyperfine (timing), jq (summary table). emerge on PATH.

set -euo pipefail
cd "$(dirname "$0")/.."

EM=${1:-${EM:-target/release/em}}
PKG=${PKG:-www-client/firefox}
RUNS=${RUNS:-5}
WARMUP=${WARMUP:-1}
# shellcheck disable=SC2206
MODES=(${MODES:--p -up -uNp -uDp -uNDp})

if [[ ! -x $EM ]]; then
    echo "error: $EM not found (cargo build --release -p portage-cli first)" >&2
    exit 1
fi
if ! command -v emerge >/dev/null 2>&1; then
    echo "error: emerge not on PATH" >&2
    exit 1
fi

# cpn identity for membership (drop version suffix after last -N…)
extract_cpn() {
    grep -E '^\[' \
        | sed -E 's/^[^]]*\][[:space:]]+//' \
        | awk '{print $1}' \
        | sed -E 's/::.*//' \
        | sed -E 's/-[0-9].*$//' \
        | sort -u
}

count_lines() {
    local f=$1
    if [[ -s $f ]]; then
        grep -cE '^\[' "$f" || true
    else
        echo 0
    fi
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "== resolve modes bench"
echo "   EM=$EM"
echo "   PKG=$PKG"
echo "   MODES=${MODES[*]}"
echo "   RUNS=$RUNS WARMUP=$WARMUP"
echo

echo "### Package counts and set parity (em vs emerge)"
echo
echo '| mode | emerge n | em n | cpn diffs | notes |'
echo '|------|----------|------|-----------|-------|'

for mode in "${MODES[@]}"; do
    # shellcheck disable=SC2086
    emerge $mode "$PKG" 2>/dev/null | tee "$tmp/eg.out" | extract_cpn >"$tmp/eg.cpn" || true
    # shellcheck disable=SC2086
    "$EM" $mode "$PKG" 2>/dev/null | tee "$tmp/em.out" | extract_cpn >"$tmp/em.cpn" || true

    eg_n=$(count_lines "$tmp/eg.out")
    em_n=$(count_lines "$tmp/em.out")
    diffs=$(diff "$tmp/eg.cpn" "$tmp/em.cpn" 2>/dev/null | grep -c '^[<>]' || true)

    note=""
    case "$mode" in
        -p | -up) note="shallow" ;;
        -uNp) note="shallow+newuse" ;;
        -uDp) note="deep updates" ;;
        -uNDp) note="deep+newuse cleanup" ;;
    esac
    printf '| `%s` | %s | %s | %s | %s |\n' "$mode" "$eg_n" "$em_n" "$diffs" "$note"

    # sanitize mode for filename (drop leading -)
    key=${mode//-/}
    cp "$tmp/eg.cpn" "$tmp/eg.${key}.cpn"
    cp "$tmp/em.cpn" "$tmp/em.${key}.cpn"
done

echo
echo "### em mode-to-mode deltas (cpn set)"
echo
echo '| from → to | only in from | only in to | shared |'
echo '|-----------|--------------|------------|--------|'
pair_delta() {
    local a=$1 b=$2
    local ka=${a//-/} kb=${b//-/}
    local only_a only_b shared
    only_a=$(comm -23 "$tmp/em.${ka}.cpn" "$tmp/em.${kb}.cpn" | wc -l)
    only_b=$(comm -13 "$tmp/em.${ka}.cpn" "$tmp/em.${kb}.cpn" | wc -l)
    shared=$(comm -12 "$tmp/em.${ka}.cpn" "$tmp/em.${kb}.cpn" | wc -l)
    printf '| `%s` → `%s` | %s | %s | %s |\n' "$a" "$b" "$only_a" "$only_b" "$shared"
}
pair_delta -up -uNp
pair_delta -up -uDp
pair_delta -uDp -uNDp
pair_delta -uNp -uNDp

if [[ ${SKIP_TIMING:-0} == 1 ]]; then
    echo
    echo "== timing skipped (SKIP_TIMING=1)"
    exit 0
fi

if ! command -v hyperfine >/dev/null 2>&1; then
    echo
    echo "== timing skipped (hyperfine not installed)"
    exit 0
fi

echo
echo "### Wall-clock (hyperfine, warmup=$WARMUP runs=$RUNS)"
echo

hf_args=(
    --ignore-failure
    --warmup "$WARMUP"
    --runs "$RUNS"
    --export-markdown "$tmp/hf.md"
    --export-json "$tmp/hf.json"
)

for mode in "${MODES[@]}"; do
    # shellcheck disable=SC2086
    hf_args+=(-n "em $mode" "$EM $mode $PKG")
    # shellcheck disable=SC2086
    hf_args+=(-n "emerge $mode" "emerge $mode $PKG")
done

hyperfine "${hf_args[@]}"

echo
echo "### Timing table"
if [[ -s $tmp/hf.md ]]; then
    cat "$tmp/hf.md"
fi

if command -v jq >/dev/null 2>&1 && [[ -s $tmp/hf.json ]]; then
    echo
    echo "### Timing summary (em vs emerge per mode)"
    echo
    echo '| mode | em mean | emerge mean | speedup (emerge/em) |'
    echo '|------|---------|-------------|---------------------|'
    n=${#MODES[@]}
    for ((i = 0; i < n; i++)); do
        mode=${MODES[$i]}
        em_i=$((i * 2))
        eg_i=$((i * 2 + 1))
        em_mean=$(jq -r --argjson i "$em_i" '.results[$i].mean // empty' "$tmp/hf.json")
        eg_mean=$(jq -r --argjson i "$eg_i" '.results[$i].mean // empty' "$tmp/hf.json")
        if [[ -z $em_mean || -z $eg_mean || $em_mean == null || $eg_mean == null ]]; then
            printf '| `%s` | (failed) | (failed) | - |\n' "$mode"
            continue
        fi
        ratio=$(jq -n --argjson a "$eg_mean" --argjson b "$em_mean" 'if $b == 0 then 0 else $a / $b end')
        printf '| `%s` | %.3f s | %.3f s | %.2f× |\n' \
            "$mode" "$em_mean" "$eg_mean" "$ratio"
    done
fi

echo
echo "== done"
