#!/usr/bin/env bash
# regression-matrix.sh — root-topology regression matrix for the toolchain/
# crossdev/stages bootstrap paths (docs/root-topology.md's three modes: bare,
# --root, --prefix, --local).
#
# Exists because "-p/pretend passes" was NOT enough to catch the two real
# bugs found 2026-07-16 (installed-view VDB sharing, CPPFLAGS-injecting
# bashrc) — both only manifested during a REAL build. Quick mode still runs
# the -p checks (fast, catches solver/ordering regressions); full mode also
# runs real builds for the toolchain-bootstrap matrix, which is what would
# have caught today's bugs before they needed a live investigation to find.
#
# Usage: ./regression-matrix.sh [--full] [--jobs N]
#   --full         Also run real (non -p) builds for stages --stage1 and
#                   crossdev --setup, not just the toolchain matrix.
#   --jobs N       em --jobs (default 8); MAKEOPTS is fixed at -j16.
#
#   CROSSDEV_STAGES_DIR   path to the crossdev-stages checkout
#                         (default: ~/Sources/crossdev-stages)
#   SANDBOX               sandbox name to use/create (default: em-regression)
#   CROSS_TARGET           cross target tuple (default: riscv64-unknown-linux-gnu)
#
# Known, accepted (not-a-regression) outcomes are checked explicitly rather
# than treated as plain pass/fail — see check_local_toolchain_partial below.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CROSSDEV_STAGES_DIR="${CROSSDEV_STAGES_DIR:-$HOME/Sources/crossdev-stages}"
SANDBOX="${SANDBOX:-em-regression}"
CROSS_TARGET="${CROSS_TARGET:-riscv64-unknown-linux-gnu}"
JOBS=8
FULL=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --full) FULL=1 ;;
        --jobs) JOBS="$2"; shift ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
    shift
done

if [[ ! -d "$CROSSDEV_STAGES_DIR" ]]; then
    echo "error: crossdev-stages checkout not found at $CROSSDEV_STAGES_DIR" >&2
    echo "  set CROSSDEV_STAGES_DIR to override" >&2
    exit 1
fi

CS() { (cd "$CROSSDEV_STAGES_DIR" && cargo run --release -- "$@"); }
SANDBOX_ROOT="$HOME/.cache/crossdev-stages/sandboxes/$SANDBOX/root"

RESULTS=()
record() {
    # record NAME STATUS DETAIL
    RESULTS+=("$1|$2|$3")
}

echo "Building em (release)..."
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" || { echo "em build failed"; exit 1; }
EM_BIN="$REPO_ROOT/target/release/em"

echo "Ensuring sandbox '$SANDBOX' exists and is prepared..."
if ! CS sandbox list 2>/dev/null | grep -q "^$SANDBOX "; then
    CS sandbox setup --name "$SANDBOX"
    CS sandbox prepare --name "$SANDBOX" --bare
fi
# Copy-then-rename, not a direct overwrite: if a previous run's em-bin is
# still executing in the sandbox (e.g. a killed-but-not-yet-reaped build),
# overwriting it in place fails with "Text file busy" — renaming over it
# doesn't, since the old inode stays open under its unlinked name until that
# process exits.
cp "$EM_BIN" "$SANDBOX_ROOT/em-bin.new"
chmod +x "$SANDBOX_ROOT/em-bin.new"
mv "$SANDBOX_ROOT/em-bin.new" "$SANDBOX_ROOT/em-bin"

sbx() { CS sandbox run --name "$SANDBOX" "$*" ; }
fresh_dir() { sbx "rm -rf $1; mkdir -p $1"; }

# --- native toolchain --setup: --root / --prefix / --local -----------------

run_native_toolchain() {
    local topo_flag="$1" dir="$2" label="$3"
    fresh_dir "$dir" >/dev/null 2>&1
    local log="/root/regress-$label.log"
    sbx "MAKEOPTS='-j16' /root/em-bin toolchain $topo_flag $dir --setup --autounmask-write --jobs $JOBS > $log 2>&1; echo EXIT=\$? >> $log"
    local exit_line
    exit_line=$(sbx "grep -o 'EXIT=[0-9]*' $log | tail -1")
    echo "$exit_line" "$log"
}

echo
echo "=== native toolchain --setup ==="

echo "--- --root ---"
res=$(run_native_toolchain "--root" "/root/regress-toolchain-root" "toolchain-root")
if [[ "$res" == EXIT=0* ]]; then
    record "toolchain --setup --root" PASS "clean build"
else
    record "toolchain --setup --root" FAIL "expected clean build, got: $res"
fi

echo "--- --prefix ---"
res=$(run_native_toolchain "--prefix" "/root/regress-toolchain-prefix" "toolchain-prefix")
if [[ "$res" == EXIT=0* ]]; then
    record "toolchain --setup --prefix" PASS "clean build"
else
    record "toolchain --setup --prefix" FAIL "expected clean build, got: $res"
fi

echo "--- --local (known genuine hard-cycle partial failure expected) ---"
LOCAL_LOG="/root/regress-toolchain-local.log"
fresh_dir "/root/regress-toolchain-local" >/dev/null 2>&1
sbx "/root/em-bin toolchain --local /root/regress-toolchain-local --setup --autounmask-write -p > $LOCAL_LOG 2>&1; echo EXIT=\$? >> $LOCAL_LOG"
gdbm_line=$(sbx "grep -n 'sys-libs/gdbm' $LOCAL_LOG | head -1 | cut -d: -f1")
elt_line=$(sbx "grep -n 'app-portage/elt-patches' $LOCAL_LOG | head -1 | cut -d: -f1")
if [[ -n "$gdbm_line" && -n "$elt_line" && "$gdbm_line" -gt "$elt_line" ]]; then
    # Known-cycle failure signature: only the already-documented cycle
    # members (meson/gettext/gcc-glibc[cet]/glibc-python/elt-patches-xz), not
    # anything new. A regression here is a *new* package name appearing in
    # the preflight error, not the presence of a preflight error itself.
    unexpected=$(sbx "grep -A20 'pre-flight dependency check failed' $LOCAL_LOG | grep 'needs:' | grep -Ev 'meson|gettext|glibc\[cet|python:3|elt-patches'")
    if [[ -z "$unexpected" ]]; then
        record "toolchain --setup --local" KNOWN-PARTIAL "gdbm orders after elt-patches (SCC fix intact); only known hard-cycle members unsatisfied"
    else
        record "toolchain --setup --local" FAIL "new/unexpected preflight failures: $unexpected"
    fi
else
    record "toolchain --setup --local" FAIL "gdbm/elt-patches ordering regressed (SCC tie-break bug back?)"
fi

# --- em stages --stage1: -p across all three, real build under --full -----

echo
echo "=== em stages --stage1 (-p) ==="
for topo in --root --prefix --local; do
    dir="/root/regress-stage1-$(echo "$topo" | tr -d '-')"
    fresh_dir "$dir" >/dev/null 2>&1
    log="/root/regress-stage1-$(echo "$topo" | tr -d '-').log"
    sbx "/root/em-bin stages $topo $dir --stage1 -p --autosolve-use > $log 2>&1; echo EXIT=\$? >> $log"
    exit_line=$(sbx "grep -o 'EXIT=[0-9]*' $log | tail -1")
    record "stages --stage1 $topo (-p)" INFO "$exit_line — see $log"
done

if [[ "$FULL" -eq 1 ]]; then
    echo
    echo "=== em stages --stage1 (real, --root and --prefix only — --local has no complete toolchain) ==="
    for topo_pair in "--root:/root/regress-toolchain-root" "--prefix:/root/regress-toolchain-prefix"; do
        topo="${topo_pair%%:*}"
        dir="${topo_pair##*:}"
        log="/root/regress-stage1-real-$(echo "$topo" | tr -d '-').log"
        sbx "/root/em-bin stages $topo $dir --stage1 --autosolve-use --jobs $JOBS > $log 2>&1; echo EXIT=\$? >> $log"
        exit_line=$(sbx "grep -o 'EXIT=[0-9]*' $log | tail -1")
        if [[ "$exit_line" == EXIT=0* ]]; then
            record "stages --stage1 $topo (real)" PASS "clean build"
        else
            record "stages --stage1 $topo (real)" FAIL "expected clean build, got: $exit_line (see $log)"
        fi
    done
fi

# --- em crossdev --setup: bare / --root / --prefix / --local ---------------

echo
echo "=== em crossdev --setup ($CROSS_TARGET) ==="

run_crossdev() {
    local extra_flags="$1" dir_flag="$2" dir="$3" label="$4"
    [[ -n "$dir" ]] && fresh_dir "$dir" >/dev/null 2>&1
    local log="/root/regress-crossdev-$label.log"
    # No separate `em ... setup` pre-step here: `crossdev --setup`'s own
    # `init_target` already bootstraps the outer root itself
    # (`crossdev/mod.rs`'s `init_target` calls `setup::bootstrap(&globals
    # .outer_roots())`). A prior version of this script ran `em --target $T
    # $dir_flag $dir setup` first — with `--target` set, that hits
    # `Applet::Setup => setup::bootstrap(&globals.roots())`, which follows
    # the `--target` sysroot substitution instead of the outer root, writing
    # the generic self-contained-root make.conf template (ACCEPT_KEYWORDS
    # mirroring the *host* arch) directly into the cross sysroot. Because
    # `crossdev --setup` itself lays down config via `FillGapsOnly` (which
    # treats "a file already exists at this path" as done, regardless of
    # content), its own correct sysroot make.conf write was then silently
    # skipped — found live 2026-07-17 chasing an unrelated riscv64
    # `sys-apps/systemd-utils --emptytree` resolution failure that traced
    # back to `ACCEPT_KEYWORDS="~arm64"` (the host's arch) in the riscv64
    # sysroot's own make.conf.
    if [[ -n "$dir" ]]; then
        sbx "/root/em-bin crossdev --target $CROSS_TARGET $dir_flag $dir $extra_flags --setup > $log 2>&1; echo EXIT=\$? >> $log"
    else
        sbx "/root/em-bin crossdev --target $CROSS_TARGET $extra_flags --setup > $log 2>&1; echo EXIT=\$? >> $log"
    fi
    sbx "grep -o 'EXIT=[0-9]*' $log | tail -1"
}

echo "--- bare ---"
res=$(run_crossdev "" "" "" "bare")
[[ "$res" == EXIT=0* ]] && record "crossdev --setup bare" PASS "clean build" \
    || record "crossdev --setup bare" FAIL "expected clean build, got: $res"

echo "--- --root (now expected to be REJECTED: crossdev never flattens RootArg, so ---root is a clap parse error in any position) ---"
res=$(run_crossdev "" "--root" "/root/regress-crossdev-root" "root")
[[ "$res" != EXIT=0* ]] && record "crossdev --setup --root" PASS "correctly rejected: $res" \
    || record "crossdev --setup --root" FAIL "expected --root + crossdev to be rejected, got a clean build"

echo "--- --prefix ---"
res=$(run_crossdev "" "--prefix" "/root/regress-crossdev-prefix" "prefix")
[[ "$res" == EXIT=0* ]] && record "crossdev --setup --prefix" PASS "clean build" \
    || record "crossdev --setup --prefix" FAIL "expected clean build, got: $res"

echo "--- --local (open question as of 2026-07-16 — informational, not a hard pass/fail) ---"
fresh_dir "/root/regress-crossdev-local" >/dev/null 2>&1
sbx "/root/em-bin setup --local /root/regress-crossdev-local >/dev/null 2>&1"
# Known to exit non-zero (the genuine hard-cycle partial failure) — this is
# a deliberate prerequisite step, not something we're asserting on here.
sbx "/root/em-bin toolchain --local /root/regress-crossdev-local --setup --autounmask-write --jobs $JOBS >/dev/null 2>&1 || true"
log="/root/regress-crossdev-local.log"
sbx "/root/em-bin crossdev --target $CROSS_TARGET --local /root/regress-crossdev-local --setup > $log 2>&1; echo EXIT=\$? >> $log"
res=$(sbx "grep -o 'EXIT=[0-9]*' $log | tail -1")
record "crossdev --setup --local" INFO "$res — see $log (depends on --local's own toolchain, which is known-partial)"

# --- summary -----------------------------------------------------------

echo
echo "=== Summary ==="
fail=0
for r in "${RESULTS[@]}"; do
    IFS='|' read -r name status detail <<< "$r"
    printf "%-32s %-14s %s\n" "$name" "$status" "$detail"
    [[ "$status" == "FAIL" ]] && fail=1
done

if [[ "$fail" -eq 1 ]]; then
    echo
    echo "REGRESSION DETECTED"
    exit 1
fi
echo
echo "No regressions detected (INFO rows are informational, not asserted)."
