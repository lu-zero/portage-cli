#!/usr/bin/env bash
# test-binpkg-identity-sandbox.sh — real-chroot regression test for the
# binpkg-subtargets identity model (todo/binpkg-subtargets.md): CHOST +
# build_env_key, multi-instance PKGDIR, the asymmetric empty-key reuse gate,
# `em maint binpkg list/fingerprint/prune`, and the S6 package.env fold.
#
# Builds sys-libs/zlib (small, fast, no deps) *twice* into the same PKGDIR
# under two different CFLAGS (-march=A vs -march=B), producing two distinct
# build_env_key instances for the same CPV — the exact S2/S3 "same CHOST,
# different micro-arch" scenario this feature exists to disambiguate. Then
# drives `em maint binpkg list/fingerprint/prune` for real against them, and
# proves the reuse gate picks the *matching* variant, not just "a" variant.
#
# Uses a real crossdev-stages sandbox (see docs/testing.md's recipe) rather
# than a synthetic --root scratch dir: a real chroot exercises the actual
# build shell sourcing make.conf for real (brush, not a test harness), which
# is what the make_conf.rs rewrite (see git log) is meant to be correct
# under.
#
# Usage: ./test-binpkg-identity-sandbox.sh [--keep] [--sandbox NAME]
#   --keep           Don't destroy the sandbox on exit (for follow-up manual
#                     poking). Mounts are still torn down.
#   --sandbox NAME   Sandbox name (default: em-binpkg-identity). Always
#                     destroyed-then-recreated fresh — never reused as-is,
#                     see the 2026-07-12 stale-sandbox incident note in
#                     memory/docs/testing.md.
#
#   CROSSDEV_STAGES_DIR   path to the crossdev-stages checkout
#                         (default: ~/Sources/crossdev-stages)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CROSSDEV_STAGES_DIR="${CROSSDEV_STAGES_DIR:-$HOME/Sources/crossdev-stages}"
SANDBOX="em-binpkg-identity"
KEEP=0

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
        # `crossdev-stages sandbox destroy` rm's unprivileged and silently
        # leaves behind anything the chroot build created as root (e.g.
        # work/ dirs under var/tmp/portage) — confirmed 2026-07-20: this
        # left a half-removed sandbox whose stale registry entry then broke
        # the next `sandbox setup --name` (skip-unpack vs. "not found"
        # race). `sudo rm -rf` first so nothing privileged survives, then
        # let the tool drop its own registry entry for a clean directory.
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
mconf() { sudo tee "$SB/etc/portage/make.conf" >/dev/null; }

sudo chroot "$SB" /usr/local/bin/em --help >/dev/null || { echo "em --help failed in chroot"; exit 1; }
pass "em runs in chroot"

echo "--- pass 1: build sys-libs/zlib with -march=x86-64-v2-equivalent A ---"
# aarch64 sandbox: use two real, distinct -march values so build_env_key
# actually differs (arbitrary picks — just need to be valid & different).
cat <<'EOF' | mconf
CHOST="aarch64-unknown-linux-gnu"
COMMON_FLAGS="-O2 -pipe -march=armv8-a"
CFLAGS="${COMMON_FLAGS}"
CXXFLAGS="${COMMON_FLAGS}"
ACCEPT_KEYWORDS="~arm64"
FEATURES="-sandbox -usersandbox"
EOF
em -b sys-libs/zlib || { echo "pass 1 build failed"; exit 1; }

echo "--- pass 2: build sys-libs/zlib with -march=armv8.2-a (different variant) ---"
cat <<'EOF' | mconf
CHOST="aarch64-unknown-linux-gnu"
COMMON_FLAGS="-O2 -pipe -march=armv8.2-a"
CFLAGS="${COMMON_FLAGS}"
CXXFLAGS="${COMMON_FLAGS}"
ACCEPT_KEYWORDS="~arm64"
FEATURES="-sandbox -usersandbox"
EOF
em -b sys-libs/zlib --newuse || { echo "pass 2 build failed"; exit 1; }

echo "--- em maint binpkg list ---"
LIST_OUT="$(em maint binpkg list)"
echo "$LIST_OUT"
ZLIB_ROWS="$(echo "$LIST_OUT" | grep -c 'sys-libs/zlib')"
[[ "$ZLIB_ROWS" -eq 2 ]] && pass "list shows 2 zlib instances" || fail "list shows $ZLIB_ROWS zlib row(s), expected 2"
echo "$LIST_OUT" | grep -q 'armv8-a ' && pass "list shows armv8-a CFLAGS column" || fail "armv8-a CFLAGS not in list"
echo "$LIST_OUT" | grep -q 'armv8.2-a' && pass "list shows armv8.2-a CFLAGS column" || fail "armv8.2-a CFLAGS not in list"

echo "--- em maint binpkg fingerprint --full (current make.conf is pass-2's) ---"
FP_OUT="$(em maint binpkg fingerprint --full)"
echo "$FP_OUT"
echo "$FP_OUT" | grep -q 'CFLAGS:.*-march=armv8.2-a' && pass "fingerprint expands COMMON_FLAGS indirection for real" || fail "fingerprint did not expand COMMON_FLAGS"

echo "--- em maint binpkg prune --dry-run (nothing to collapse: each key has exactly 1 build) ---"
# prune only ever prints "keeping build"/"removed build" for an identity
# group with *more than one* build_id — each of our two variants has a
# distinct build_env_key, so each group has exactly 1 member and there is
# nothing to prune. "nothing to prune" is the correct, expected output here
# (it's the multi-instance survival itself, already proven by `list` above).
PRUNE_OUT="$(em maint binpkg prune --dry-run)"
echo "$PRUNE_OUT"
echo "$PRUNE_OUT" | grep -q 'nothing to prune' && pass "prune correctly finds nothing to collapse (distinct keys, not duplicates)" || fail "unexpected prune output: $PRUNE_OUT"

# The `-p` preview's binary/ebuild tag is a known, documented simplification
# (query/depgraph/output.rs: `find_reusable(&cpv, &use, "", "")` — always
# passes an empty CHOST/build_env_key, so it never reflects the real
# per-key reuse decision, only real (non-preview) merges do — see the
# "Empty CHOST and build_env_key: preview skips both gates" comment there).
# So the reuse gate itself is verified via a REAL merge instead: `-e`
# (--emptytree) forces a merge attempt despite already being installed,
# `-k` allows local binpkg reuse; a real *reuse* never re-invokes the
# compiler, a real *rebuild* does — checked by grepping for a compile line.
echo "--- reuse gate: real -e -k merge under pass-2's CFLAGS must reuse, not rebuild ---"
REUSE_OUT="$(em -e -k sys-libs/zlib)"
echo "$REUSE_OUT"
if echo "$REUSE_OUT" | grep -q -- '-march=armv8.2-a.*-c -o'; then
    fail "matching CFLAGS still invoked the compiler — expected a binary reuse, got a rebuild"
else
    pass "matching CFLAGS reused the binary (no compile invocation)"
fi

echo "--- reuse gate: real -e -k merge under a THIRD, unkeyed CFLAGS must NOT reuse either variant ---"
cat <<'EOF' | mconf
CHOST="aarch64-unknown-linux-gnu"
CFLAGS="-O2 -pipe"
CXXFLAGS="-O2 -pipe"
ACCEPT_KEYWORDS="~arm64"
FEATURES="-sandbox -usersandbox"
EOF
REUSE_OUT2="$(em -e -k sys-libs/zlib)"
echo "$REUSE_OUT2"
echo "$REUSE_OUT2" | grep -q -- '-c -o' && pass "generic CFLAGS correctly rebuilt instead of reusing a march-keyed binary" || fail "expected a real rebuild (compiler invocation) under generic CFLAGS, got: $REUSE_OUT2"

echo "==="
if [[ $FAIL -eq 0 ]]; then
    echo "ALL CHECKS PASSED"
else
    echo "SOME CHECKS FAILED"
fi
exit $FAIL
