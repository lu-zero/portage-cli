#!/usr/bin/env bash
# test-crossdev-binpkg-sandbox.sh — real cross-compiled binpkg regression
# test for the host/target CHOST split (todo/binpkg-subtargets.md's S1/S4:
# per-entry PKGDIR dual index for cross host BDEPEND reuse). Unlike
# test-binpkg-identity-sandbox.sh (same-CHOST, different -march) and
# test-crossdev-flavours.sh (toolchain bootstrap only, no package build),
# this actually cross-*builds* a real package for two different target
# tuples and checks the resulting binpkg's recorded CHOST matches the
# *target*, not the host, and that each target's PKGDIR is genuinely
# separate (a target's `maint binpkg list` never sees another target's, or
# the host's, binpkgs).
#
# Both targets are genuinely cross from this (aarch64) host — no qemu
# needed, since cross-compiling doesn't execute any target-arch code, only
# the (aarch64-native) cross-compiler itself. aarch64-unknown-linux-gnu
# (same arch as the host) is deliberately NOT one of the two: `em crossdev
# --setup` now rejects a same-arch target up front (see
# reject_same_arch_target in crossdev/mod.rs — found via this exact script
# 2026-07-20, a 980-file collision merging cross-*/linux-headers), so
# there's no real package build to test there anymore; that fast-rejection
# path is covered instead by test-crossdev-flavours.sh.
#
# Slow: two full `crossdev --setup` cross-toolchain bootstraps (gcc/
# binutils/glibc for the target) plus two real cross package builds. Expect
# this to run considerably longer than test-binpkg-identity-sandbox.sh.
#
# Usage: ./test-crossdev-binpkg-sandbox.sh [--keep] [--sandbox NAME]
#   --keep           Don't destroy the sandbox on exit. Mounts still torn down.
#   --sandbox NAME   Sandbox name (default: em-crossdev-binpkg). Always
#                     destroyed-then-recreated fresh.
#
#   CROSSDEV_STAGES_DIR   path to the crossdev-stages checkout
#                         (default: ~/Sources/crossdev-stages)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CROSSDEV_STAGES_DIR="${CROSSDEV_STAGES_DIR:-$HOME/Sources/crossdev-stages}"
SANDBOX="em-crossdev-binpkg"
KEEP=0
TARGETS=("riscv64-unknown-linux-gnu" "x86_64-pc-linux-gnu")

while [[ $# -gt 0 ]]; do
    case "$1" in
        --keep) KEEP=1 ;;
        --sandbox) SANDBOX="$2"; shift ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
    shift
done

CDS="$CROSSDEV_STAGES_DIR/target/release/crossdev-stages"
SB="$HOME/.cache/crossdev-stages/sandboxes/$SANDBOX"
FAIL=0

pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1"; FAIL=1; }

cleanup() {
    echo "--- cleaning up ---"
    sudo umount -R "$SB/dev" 2>/dev/null
    sudo umount "$SB/var/cache/distfiles" "$SB/sys" "$SB/proc" "$SB/var/db/repos/gentoo" 2>/dev/null
    if [[ $KEEP -eq 0 ]]; then
        # See test-scripts/README.md's "known crossdev-stages gotchas":
        # destroy runs unprivileged and can't remove root-owned build
        # artifacts a chroot build leaves behind.
        sudo rm -rf "$SB"
        "$CDS" sandbox destroy "$SANDBOX" 2>/dev/null
    else
        echo "kept sandbox: $SANDBOX ($SB)"
    fi
}
trap cleanup EXIT

echo "--- building em (release) ---"
( cd "$REPO_ROOT" && cargo build --release -p portage-cli ) || { echo "em build failed"; exit 1; }

echo "--- fresh sandbox: $SANDBOX ---"
sudo rm -rf "$SB"
"$CDS" sandbox destroy "$SANDBOX" 2>/dev/null
"$CDS" sandbox setup --arch aarch64 --name "$SANDBOX" || { echo "sandbox setup failed"; exit 1; }

echo "--- wiring sandbox ---"
sudo mkdir -p "$SB/usr/local/bin"
sudo cp "$REPO_ROOT/target/release/em" "$SB/usr/local/bin/em"
sudo mkdir -p "$SB/var/db/repos/gentoo" "$SB/proc" "$SB/dev" "$SB/sys" "$SB/var/cache/distfiles"
sudo mount --bind "$REPO_ROOT/portage-repo/gentoo" "$SB/var/db/repos/gentoo"
sudo mount --bind /proc "$SB/proc"
sudo mount --rbind /dev "$SB/dev"
sudo mount --bind /sys "$SB/sys"
sudo cp /etc/resolv.conf "$SB/etc/resolv.conf"
sudo mkdir -p /var/cache/distfiles
sudo mount --bind /var/cache/distfiles "$SB/var/cache/distfiles"

em() { sudo chroot "$SB" /usr/local/bin/em "$@"; }

sudo chroot "$SB" /usr/local/bin/em --help >/dev/null || { echo "em --help failed in chroot"; exit 1; }
pass "em runs in chroot"

HOST_CHOST="$(em maint binpkg fingerprint --full | grep '^CHOST:' | awk '{print $2}')"
echo "host CHOST: $HOST_CHOST"

for target in "${TARGETS[@]}"; do
    dir="/root/cross-$target"
    echo
    echo "############################################################"
    echo "### target: $target"
    echo "############################################################"

    sudo chroot "$SB" rm -rf "$dir"

    echo "--- crossdev --setup (real cross-toolchain bootstrap) ---"
    if em --root "$dir" --target "$target" crossdev --setup; then
        pass "$target: crossdev --setup succeeded"
    else
        fail "$target: crossdev --setup failed"
        continue
    fi

    echo "--- real cross build: sys-libs/zlib -b ---"
    if em --root "$dir" --target "$target" -b sys-libs/zlib; then
        pass "$target: cross build succeeded"
    else
        fail "$target: cross build failed"
        continue
    fi

    echo "--- em --root $dir --target $target maint binpkg list ---"
    LIST_OUT="$(em --root "$dir" --target "$target" maint binpkg list)"
    echo "$LIST_OUT"
    echo "$LIST_OUT" | grep -q "sys-libs/zlib" || { fail "$target: zlib missing from target's own binpkg list"; continue; }
    echo "$LIST_OUT" | grep -q "$target" && pass "$target: binpkg recorded with the target's own CHOST" \
        || fail "$target: binpkg list does not show CHOST=$target — got: $LIST_OUT"

    echo "--- em --root $dir --target $target maint binpkg fingerprint --full ---"
    FP_OUT="$(em --root "$dir" --target "$target" maint binpkg fingerprint --full)"
    echo "$FP_OUT"
    FP_CHOST="$(echo "$FP_OUT" | grep '^CHOST:' | awk '{print $2}')"
    [[ "$FP_CHOST" == "$target" ]] && pass "$target: fingerprint reports the target's own CHOST" \
        || fail "$target: fingerprint CHOST is '$FP_CHOST', expected '$target'"

    echo "--- em --root $dir --target $target maint binpkg fingerprint --full --host ---"
    FP_HOST_OUT="$(em --root "$dir" --target "$target" maint binpkg fingerprint --full --host)"
    echo "$FP_HOST_OUT"
    FP_HOST_CHOST="$(echo "$FP_HOST_OUT" | grep '^CHOST:' | awk '{print $2}')"
    [[ "$FP_HOST_CHOST" == "$HOST_CHOST" ]] && pass "$target: --host fingerprint reports the real host CHOST ($HOST_CHOST)" \
        || fail "$target: --host fingerprint CHOST is '$FP_HOST_CHOST', expected host CHOST '$HOST_CHOST'"
done

echo
echo "--- cross-PKGDIR isolation: each target's list must not see the others' or the host's binpkgs ---"
for target in "${TARGETS[@]}"; do
    dir="/root/cross-$target"
    LIST_OUT="$(em --root "$dir" --target "$target" maint binpkg list 2>/dev/null)"
    leaked=0
    for other in "${TARGETS[@]}"; do
        [[ "$other" == "$target" ]] && continue
        echo "$LIST_OUT" | grep -q "$other" && leaked=1
    done
    if [[ $leaked -eq 0 ]]; then
        pass "$target: PKGDIR isolated from other targets"
    else
        fail "$target: another target's CHOST leaked into this target's binpkg list"
    fi
done
HOST_LIST_OUT="$(em maint binpkg list 2>/dev/null)"
echo "$HOST_LIST_OUT" | grep -q "sys-libs/zlib" && fail "host PKGDIR unexpectedly contains a cross-built zlib" \
    || pass "host PKGDIR untouched by cross builds"

echo "==="
if [[ $FAIL -eq 0 ]]; then
    echo "ALL CHECKS PASSED"
else
    echo "SOME CHECKS FAILED"
fi
exit $FAIL
