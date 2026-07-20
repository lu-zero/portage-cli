#!/usr/bin/env bash
# test-crossdev-flavours.sh — runs regression-matrix.sh's existing
# crossdev/toolchain matrix (bare/--root/--prefix/--local x `crossdev
# --setup`, real builds) for the genuinely-cross riscv64-unknown-linux-gnu
# flavour, plus a fast, separate check that a same-arch target
# (aarch64-unknown-linux-gnu, identical to this machine's own host CHOST)
# is correctly *rejected* rather than attempted.
#
# aarch64-unknown-linux-gnu does NOT get the full matrix: `crossdev
# --setup` for a target tuple identical to the host's own CHOST is a
# fundamentally invalid configuration (cross-*/linux-headers is the real
# upstream ebuild, symlinked in — it decides its own install path by
# checking CTARGET != CHOST, so a same-arch tuple makes it install straight
# into /usr/include and collide with the native package already there;
# real crossdev has the identical limitation). `em crossdev --setup` now
# rejects this up front (see reject_same_arch_target in crossdev/mod.rs,
# added 2026-07-20 after this exact scenario produced a 980-file-collision
# failure here) — so the useful live check is "does it fail *fast* with
# the expected message", not "does the multi-minute bootstrap matrix pass".
#
# Usage: ./test-crossdev-flavours.sh [--full] [--jobs N]
#   Flags are forwarded to regression-matrix.sh's riscv64 run as-is.
#
#   CROSSDEV_STAGES_DIR   path to the crossdev-stages checkout
#                         (default: ~/Sources/crossdev-stages)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CROSSDEV_STAGES_DIR="${CROSSDEV_STAGES_DIR:-$HOME/Sources/crossdev-stages}"
CDS="$CROSSDEV_STAGES_DIR/target/release/crossdev-stages"
FAIL=0

echo "############################################################"
echo "### crossdev flavour: riscv64-unknown-linux-gnu (genuine cross, full matrix)"
echo "############################################################"
if CROSS_TARGET="riscv64-unknown-linux-gnu" SANDBOX="em-regression-riscv64" "$SCRIPT_DIR/regression-matrix.sh" "$@"; then
    echo "=== riscv64-unknown-linux-gnu: no regressions detected ==="
else
    echo "=== riscv64-unknown-linux-gnu: REGRESSION DETECTED ==="
    FAIL=1
fi

echo
echo "############################################################"
echo "### crossdev flavour: aarch64-unknown-linux-gnu (same-arch as host — must fail fast)"
echo "############################################################"
SANDBOX="em-regression-aarch64-samearch"
SB="$HOME/.cache/crossdev-stages/sandboxes/$SANDBOX"
cleanup() {
    sudo rm -rf "$SB"
    "$CDS" sandbox destroy "$SANDBOX" 2>/dev/null
}
trap cleanup EXIT

echo "--- building em (release) ---"
( cd "$REPO_ROOT" && cargo build --release -p portage-cli ) || { echo "em build failed"; exit 1; }

sudo rm -rf "$SB"
"$CDS" sandbox destroy "$SANDBOX" 2>/dev/null
"$CDS" sandbox setup --arch aarch64 --name "$SANDBOX" || { echo "sandbox setup failed"; exit 1; }
sudo mkdir -p "$SB/usr/local/bin"
sudo cp "$REPO_ROOT/target/release/em" "$SB/usr/local/bin/em"

OUT="$(sudo chroot "$SB" /usr/local/bin/em --target aarch64-unknown-linux-gnu crossdev --setup 2>&1)"
RC=$?
echo "$OUT"
if [[ $RC -ne 0 ]] && echo "$OUT" | grep -q "identical to the host's own CHOST"; then
    echo "=== aarch64-unknown-linux-gnu: correctly rejected fast (no wasted bootstrap attempt) ==="
else
    echo "=== aarch64-unknown-linux-gnu: REGRESSION — did not reject cleanly (rc=$RC) ==="
    FAIL=1
fi

if [[ $FAIL -eq 1 ]]; then
    echo
    echo "SOME FLAVOURS REGRESSED"
    exit 1
fi
echo
echo "ALL FLAVOURS PASSED"
