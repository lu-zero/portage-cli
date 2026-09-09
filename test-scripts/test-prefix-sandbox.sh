#!/usr/bin/env bash
# test-prefix-sandbox.sh — fast, real (non-pretend) smoke test that `--prefix`
# still works: layout bootstrap, a real compile+install into the prefix (not
# just -p), VDB registration, and the `em active` set/env/list flow.
#
# Exists to sanity-check --prefix after CLI-parser-level changes (e.g. the
# clap -> usage-rs migration on branch usage-rs) without paying for a full
# toolchain bootstrap — regression-matrix.sh's --full mode already covers the
# heavier native/cross toolchain matrix; this is the few-seconds version for
# "did the --prefix plumbing itself survive a parser swap".
#
# Uses ONLY `crossdev-stages sandbox run` — never `sudo chroot` / manual
# `mount --bind`. Some older scripts in this directory (test-binpkg-identity-
# sandbox.sh, test-crossdev-binpkg-sandbox.sh, and part of test-crossdev-
# flavours.sh) still do that; it's blocked in Claude Code sessions on this
# repo via .claude/settings.local.json's permissions.deny (`sudo chroot`/
# `chroot` are denied outright — see the crossdev-stages-sandbox memory for
# why: repeated real data loss). Don't copy that pattern into new scripts.
#
# Usage: ./test-prefix-sandbox.sh [--worktree DIR] [--sandbox NAME] [--keep]
#   --worktree DIR   portage-cli checkout to build `em` from
#                     (default: this script's own repo root — pass a sibling
#                     worktree, e.g. ../portage-cli-usage-rs, to test a branch)
#   --sandbox NAME   sandbox name (default: em-prefix-check; always
#                     destroyed-then-recreated fresh, never reused)
#   --keep           don't destroy the sandbox on exit (for manual poking)
#
#   CROSSDEV_STAGES_DIR   path to the crossdev-stages checkout
#                         (default: ~/Sources/crossdev-stages)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CROSSDEV_STAGES_DIR="${CROSSDEV_STAGES_DIR:-$HOME/Sources/crossdev-stages}"
WORKTREE="$REPO_ROOT"
SANDBOX="em-prefix-check"
KEEP=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --worktree) WORKTREE="$2"; shift ;;
        --sandbox) SANDBOX="$2"; shift ;;
        --keep) KEEP=1 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
    shift
done

if [[ ! -d "$CROSSDEV_STAGES_DIR" ]]; then
    echo "error: crossdev-stages checkout not found at $CROSSDEV_STAGES_DIR" >&2
    echo "  set CROSSDEV_STAGES_DIR to override" >&2
    exit 1
fi
if [[ ! -d "$WORKTREE" ]]; then
    echo "error: worktree not found at $WORKTREE" >&2
    exit 1
fi

CS() { (cd "$CROSSDEV_STAGES_DIR" && cargo run --release -- "$@"); }
# The sandbox's own filesystem root on the host — NOT the sandbox's /root
# home dir (regression-matrix.sh's SANDBOX_ROOT is that instead, a
# same-looking but different path one level down).
SANDBOX_DIR="$HOME/.cache/crossdev-stages/sandboxes/$SANDBOX"

RESULTS=()
record() { RESULTS+=("$1|$2|$3"); } # NAME STATUS DETAIL

cleanup() {
    if [[ "$KEEP" -eq 0 ]]; then
        echo "Destroying sandbox '$SANDBOX'..."
        CS sandbox destroy "$SANDBOX" >/dev/null 2>&1
    else
        echo "Keeping sandbox '$SANDBOX' ($SANDBOX_DIR) — 'crossdev-stages sandbox destroy $SANDBOX' when done."
    fi
}
trap cleanup EXIT

echo "Building em (release) from $WORKTREE..."
cargo build -p portage-cli --release --manifest-path "$WORKTREE/Cargo.toml" || { echo "em build failed"; exit 1; }
EM_BIN="$WORKTREE/target/release/em"

echo "Recreating fresh sandbox '$SANDBOX'..."
CS sandbox destroy "$SANDBOX" >/dev/null 2>&1 || true
CS sandbox setup --name "$SANDBOX" || exit 1
CS sandbox prepare --name "$SANDBOX" --bare || exit 1

cp "$EM_BIN" "$SANDBOX_DIR/usr/local/bin/em.new"
chmod +x "$SANDBOX_DIR/usr/local/bin/em.new"
mv "$SANDBOX_DIR/usr/local/bin/em.new" "$SANDBOX_DIR/usr/local/bin/em"

sbx() { CS sandbox run --name "$SANDBOX" "$*"; }

PREFIX=/root/testprefix

echo
echo "=== em setup --prefix ==="
if sbx "em setup --prefix $PREFIX" | tail -5; then
    record "setup --prefix" PASS "layout bootstrapped"
else
    record "setup --prefix" FAIL "see sandbox for details"
fi

echo
echo "=== real build+install under --prefix (sys-libs/zlib --nodeps) ==="
if sbx "em --prefix $PREFIX --nodeps sys-libs/zlib" | tail -5; then
    record "build+install --prefix" PASS "zlib compiled and installed"
else
    record "build+install --prefix" FAIL "merge failed"
fi

echo
echo "=== files landed under the prefix, not the sandbox root ==="
if sbx "test -e $PREFIX/usr/lib64/libz.so"; then
    record "prefix file placement" PASS "libz.so under $PREFIX"
else
    record "prefix file placement" FAIL "libz.so missing under $PREFIX"
fi

echo
echo "=== VDB registration ==="
if sbx "test -d $PREFIX/var/db/pkg/sys-libs/zlib-1.3.2-r1"; then
    record "VDB registration" PASS "zlib VDB entry present"
else
    record "VDB registration" FAIL "no VDB entry found"
fi

echo
echo "=== em active set / env / list ==="
if sbx "em active set --prefix $PREFIX" | grep -q "active prefix"; then
    record "active set" PASS "registered"
else
    record "active set" FAIL "unexpected output"
fi
if sbx "em active env" | grep -q "EM_ACTIVE_PATH=\"$PREFIX\""; then
    record "active env" PASS "exports the right path"
else
    record "active env" FAIL "wrong or missing EM_ACTIVE_PATH"
fi
if sbx "em active list" | grep -q "testprefix"; then
    record "active list" PASS "shows the registered entry"
else
    record "active list" FAIL "entry not listed"
fi

echo
echo "############################################################"
echo "### Results"
echo "############################################################"
FAIL=0
for r in "${RESULTS[@]}"; do
    IFS='|' read -r name status detail <<<"$r"
    printf '%-28s %-4s  %s\n' "$name" "$status" "$detail"
    [[ "$status" == FAIL ]] && FAIL=1
done

if [[ "$FAIL" -eq 0 ]]; then
    echo "ALL CHECKS PASSED"
else
    echo "SOME CHECKS FAILED"
fi
exit "$FAIL"
