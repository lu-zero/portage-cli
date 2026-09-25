#!/usr/bin/env bash
# capture-plan.sh — snapshot `em -p` output for a fixed target set, for
# before/after comparison around a change to the resolve pipeline.
#
# Exists because `query/depgraph/mod.rs` has no unit tests and the binary crate
# has no end-to-end `em -p` test, so the plan output is the only real oracle for
# a refactor of `depgraph()`. Measured: that output is byte-identical across runs
# once the single timing line is masked.
#
# Two modes:
#   capture-plan.sh capture  <outdir>   — write one normalized file per case
#   capture-plan.sh compare  <a> <b>    — diff two capture directories
#
# Not a committed golden: the target tree moves, so an expected-output file in
# git would break on every sync. The A/B diff is the contract instead.
#
# Usage:
#   REPO=/var/db/repos/gentoo ./capture-plan.sh capture /tmp/plan-before
#   ...make the change, rebuild...
#   REPO=/var/db/repos/gentoo ./capture-plan.sh capture /tmp/plan-after
#   ./capture-plan.sh compare /tmp/plan-before /tmp/plan-after
set -euo pipefail

REPO="${REPO:-/var/db/repos/gentoo}"
EM="${EM:-target/quick/em}"
RUNS="${RUNS:-3}"

# Four pipeline stages, not four packages: a successful verbose plan, a failing
# plan (constraint list + USE-change report), the autounmask suggestion path
# (where the `# required by` narration lives), and the Level-C co-solve
# fixpoint. Each is a different part of `depgraph()`.
CASES=(
  "plan-verbose:-vp --emptytree dev-libs/openssl"
  "plan-system:-p --emptytree @system"
  "plan-system-autounmask:-p --emptytree --autounmask @system"
  "plan-autosolve-use:-p --emptytree --autosolve-use dev-libs/openssl"
)

# `Dependency resolution took N.NN s.` is the only nondeterministic line in an
# otherwise byte-identical plan; mask it rather than dropping whole lines.
normalize() {
  sed -E \
    -e 's/took [0-9]+(\.[0-9]+)? s\./took <T> s./' \
    -e 's#/tmp/[A-Za-z0-9._-]+#<TMP>#g' \
    -e "s#$REPO#<REPO>#g"
}

run_case() {
  local name="$1" flags="$2"
  # shellcheck disable=SC2086
  timeout 900 "$EM" $flags --repo "$REPO" 2>&1 || true
}

cmd_capture() {
  local outdir="$1"
  mkdir -p "$outdir"
  # Warm the secondary cache first: the plan body is stable either way, but a
  # warm run avoids doing cold-cache work inside the measured comparison.
  run_case warm "-p @system" >/dev/null

  for spec in "${CASES[@]}"; do
    local name="${spec%%:*}" flags="${spec#*:}"
    # Repeat and keep the first: a case that is still nondeterministic will show
    # up as a compare failure rather than silently drifting.
    for i in $(seq 1 "$RUNS"); do
      run_case "$name" "$flags" | normalize > "$outdir/$name.txt"
      if [ "$i" -lt "$RUNS" ]; then
        run_case "$name" "$flags" | normalize > "$outdir/$name.run$i.txt"
        if ! cmp -s "$outdir/$name.txt" "$outdir/$name.run$i.txt"; then
          echo "WARNING: $name is not deterministic across $RUNS runs" >&2
          diff -u "$outdir/$name.txt" "$outdir/$name.run$i.txt" >&2 || true
        fi
        rm -f "$outdir/$name.run$i.txt"
      fi
    done
    printf '  %-24s %5s lines\n' "$name" "$(wc -l < "$outdir/$name.txt")"
  done
}

cmd_compare() {
  local a="$1" b="$2" status=0
  for spec in "${CASES[@]}"; do
    local name="${spec%%:*}"
    if [ ! -f "$a/$name.txt" ] || [ ! -f "$b/$name.txt" ]; then
      echo "MISSING: $name" >&2
      status=1
      continue
    fi
    if cmp -s "$a/$name.txt" "$b/$name.txt"; then
      printf '  %-24s identical (%s lines)\n' "$name" "$(wc -l < "$a/$name.txt")"
    else
      printf '  %-24s DIFFERS\n' "$name"
      diff -u "$a/$name.txt" "$b/$name.txt" | head -60
      status=1
    fi
  done
  return $status
}

case "${1:-}" in
  capture)
    [ $# -eq 2 ] || { echo "usage: $0 capture <outdir>" >&2; exit 2; }
    [ -x "$EM" ] || { echo "no em binary at $EM (set EM=)" >&2; exit 2; }
    [ -d "$REPO" ] || { echo "no repo at $REPO (set REPO=)" >&2; exit 2; }
    echo "capturing from $REPO via $EM"
    cmd_capture "$2"
    ;;
  compare)
    [ $# -eq 3 ] || { echo "usage: $0 compare <a> <b>" >&2; exit 2; }
    echo "comparing $2 vs $3"
    cmd_compare "$2" "$3"
    ;;
  *)
    echo "usage: $0 capture <outdir> | compare <a> <b>" >&2
    exit 2
    ;;
esac
