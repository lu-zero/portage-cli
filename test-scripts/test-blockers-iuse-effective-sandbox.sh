#!/usr/bin/env bash
# test-blockers-iuse-effective-sandbox.sh — regression test for the
# 2026-08-20 PMS pass: blocker auto-unmerge (PMS 8.3.2, conflicts.rs) and
# IUSE_EFFECTIVE (PMS 11.1.1 / table 12.20, iuse_effective.rs +
# use_flag.rs). Both were unit-tested in isolation but never exercised
# through a real merge/VDB/build-shell round trip — that's the gap this
# script closes.
#
# Every command inside the sandbox runs via `crossdev-stages sandbox run`
# (hakoniwa: unshared mount/PID/user namespaces, its own fresh /proc,
# /dev, /tmp, DNS, uid/gid mapping) — deliberately NOT `sudo chroot` +
# manual `mount --bind`. A raw chroot shares the host's mount namespace, so
# a script that dies mid-run (or a wrong umount order) leaves real mounts
# behind on the host; hakoniwa's namespaces are torn down with the process,
# nothing to leak. The only host-side privileged operations left are plain
# file writes into the sandbox's own directory (`sudo cp`/`sudo tee`) — no
# mount, no chroot, nothing that outlives this script.
#
# Builds a tiny synthetic overlay (`test-pms`, via `em select repository
# create`) with seven trivial ebuilds — no SRC_URI, no network fetch —
# rather than hunting for a real Gentoo package pair with the right
# blocker/USE shape (the canonical systemd[resolvconf]/openresolv blocker
# pair is far too heavy to build here). This isolates exactly the
# integration behavior under test and keeps the whole run fast. The real
# `portage-repo/gentoo` checkout is copied in (not bind-mounted) so
# `masters = gentoo` has real profiles/licenses to inherit, without a live
# mount tying the sandbox to the host tree.
#
# Usage: ./test-blockers-iuse-effective-sandbox.sh [--keep] [--sandbox NAME]
#   --keep           Don't destroy the sandbox on exit (for follow-up manual
#                     poking via `crossdev-stages sandbox enter --name NAME`).
#   --sandbox NAME   Sandbox name (default: em-blockers-iuse). Always
#                     destroyed-then-recreated fresh — never reused as-is.
#
#   CROSSDEV_STAGES_DIR   path to the crossdev-stages checkout
#                         (default: ~/Sources/crossdev-stages)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CROSSDEV_STAGES_DIR="${CROSSDEV_STAGES_DIR:-$HOME/Sources/crossdev-stages}"
SANDBOX="em-blockers-iuse"
KEEP=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --keep) KEEP=1 ;;
        --sandbox) SANDBOX="$2"; shift ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
    shift
done

if [[ -z "$SANDBOX" ]]; then
    echo "--sandbox must not be empty" >&2
    exit 1
fi

CDS="$CROSSDEV_STAGES_DIR/target/release/crossdev-stages"
SB="$HOME/.cache/crossdev-stages/sandboxes/$SANDBOX"
FAIL=0

pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1"; FAIL=1; }

cleanup() {
    echo "--- cleaning up ---"
    if [[ $KEEP -eq 0 ]]; then
        # `sandbox destroy`'s own removal runs inside its hakoniwa
        # namespace and can't touch the ebuild/em/repo files this script
        # wrote via `sudo tee`/`sudo cp` (real host-root-owned, not just
        # the stage3's own root-owned files it's built to handle) — it
        # reliably fails on those. `sudo rm -rf` on this one
        # already-validated, always-absolute `$SB` path is not the chroot
        # pattern this script was rewritten to drop: no mount, no
        # namespace, a single bounded removal of a path we just built.
        sudo rm -rf "$SB"
        ( cd "$CROSSDEV_STAGES_DIR" && "$CDS" sandbox destroy "$SANDBOX" ) 2>/dev/null
    else
        echo "kept sandbox: $SANDBOX ($SB) — poke it with:"
        echo "  (cd $CROSSDEV_STAGES_DIR && $CDS sandbox enter --name $SANDBOX)"
    fi
}
trap cleanup EXIT

echo "--- building em (release) ---"
( cd "$REPO_ROOT" && cargo build --release -p portage-cli ) || { echo "em build failed"; exit 1; }

echo "--- fresh sandbox: $SANDBOX ---"
( cd "$CROSSDEV_STAGES_DIR" && "$CDS" sandbox destroy "$SANDBOX" ) 2>/dev/null
( cd "$CROSSDEV_STAGES_DIR" && "$CDS" sandbox setup --arch aarch64 --name "$SANDBOX" ) \
    || { echo "sandbox setup failed"; exit 1; }

# Plain file writes into the sandbox's own directory tree — no mount, no
# chroot. `sandbox run` bind-mounts this whole directory as the container
# root, so anything placed here is visible inside at the same path.
echo "--- wiring sandbox (file copies only) ---"
sudo mkdir -p "$SB/usr/local/bin" "$SB/var/db/repos"
sudo cp "$REPO_ROOT/target/release/em" "$SB/usr/local/bin/em"
sudo cp -a "$REPO_ROOT/portage-repo/gentoo" "$SB/var/db/repos/gentoo"

em() { ( cd "$CROSSDEV_STAGES_DIR" && "$CDS" sandbox run --name "$SANDBOX" -- /usr/local/bin/em "$@" ); }
mconf() { sudo tee "$SB/etc/portage/make.conf" >/dev/null; }

cat <<'EOF' | mconf
CHOST="aarch64-unknown-linux-gnu"
CFLAGS="-O2 -pipe"
CXXFLAGS="-O2 -pipe"
ACCEPT_KEYWORDS="~arm64"
FEATURES="-sandbox -usersandbox"
EOF

em --help >/dev/null || { echo "em --help failed in the sandbox"; exit 1; }
pass "em runs under crossdev-stages sandbox run"

echo "--- creating synthetic overlay: test-pms ---"
em select repository create test-pms || { echo "overlay create failed"; exit 1; }
OV="$SB/var/db/repos/test-pms"
printf 'test-blockers\ntest-iuse\n' | sudo tee "$OV/profiles/categories" >/dev/null

write_ebuild() {
    # write_ebuild CAT/PN-VER <<'EOF' ... EOF
    local cpv="$1" dir file
    dir="$OV/${cpv%/*}"
    file="$OV/${cpv}.ebuild"
    sudo mkdir -p "$dir"
    sudo tee "$file" >/dev/null
}

write_ebuild test-blockers/victim/victim-1.0 <<'EOF'
EAPI=8
DESCRIPTION="blocker test: unmerge target"
SLOT="0"
KEYWORDS="~arm64"
LICENSE="GPL-2"
S="${WORKDIR}"
src_install() { keepdir /usr/share/${PN}; }
EOF

write_ebuild test-blockers/needs-victim/needs-victim-1.0 <<'EOF'
EAPI=8
DESCRIPTION="blocker test: keeps victim installed"
SLOT="0"
KEYWORDS="~arm64"
LICENSE="GPL-2"
RDEPEND="test-blockers/victim"
S="${WORKDIR}"
src_install() { keepdir /usr/share/${PN}; }
EOF

write_ebuild test-blockers/attacker-weak/attacker-weak-1.0 <<'EOF'
EAPI=8
DESCRIPTION="blocker test: weak (!) blocker on victim"
SLOT="0"
KEYWORDS="~arm64"
LICENSE="GPL-2"
RDEPEND="!test-blockers/victim"
S="${WORKDIR}"
src_install() { keepdir /usr/share/${PN}; }
EOF

write_ebuild test-blockers/attacker-strong/attacker-strong-1.0 <<'EOF'
EAPI=8
DESCRIPTION="blocker test: strong (!!) blocker on victim"
SLOT="0"
KEYWORDS="~arm64"
LICENSE="GPL-2"
RDEPEND="!!test-blockers/victim"
S="${WORKDIR}"
src_install() { keepdir /usr/share/${PN}; }
EOF

write_ebuild test-iuse/known-flag/known-flag-1.0 <<'EOF'
EAPI=8
DESCRIPTION="IUSE_EFFECTIVE test: query a declared flag"
SLOT="0"
KEYWORDS="~arm64"
LICENSE="GPL-2"
IUSE="foo"
S="${WORKDIR}"
src_install() {
	use foo && einfo "foo enabled" || einfo "foo disabled"
	keepdir /usr/share/${PN}
}
EOF

write_ebuild test-iuse/unknown-flag/unknown-flag-1.0 <<'EOF'
EAPI=8
DESCRIPTION="IUSE_EFFECTIVE test: query a flag outside IUSE_EFFECTIVE"
SLOT="0"
KEYWORDS="~arm64"
LICENSE="GPL-2"
IUSE="foo"
S="${WORKDIR}"
src_install() {
	use bar
	keepdir /usr/share/${PN}
}
EOF

# PMS 11.1.1's non-injection branch (EAPI < 5): IUSE_EFFECTIVE also includes
# every ARCH value, not just declared IUSE. `use arm64` here has no IUSE
# entry at all — this only succeeds if that branch actually adds ARCH.
write_ebuild test-iuse/eapi4-arch/eapi4-arch-1.0 <<'EOF'
EAPI=4
DESCRIPTION="IUSE_EFFECTIVE test: EAPI 4 (non-injection) still allows querying ARCH"
SLOT="0"
KEYWORDS="~arm64"
LICENSE="GPL-2"
S="${WORKDIR}"
src_install() {
	use arm64 && einfo "on arm64" || einfo "not arm64"
	keepdir /usr/share/${PN}
}
EOF

echo "--- em regen test-pms ---"
em regen test-pms || { echo "regen failed"; exit 1; }

installed() { sudo test -d "$SB/var/db/pkg/$1"; }

echo "=== IUSE_EFFECTIVE: known flag merges clean, VDB records the set ==="
OUT="$(em test-iuse/known-flag 2>&1)"
echo "$OUT"
if installed test-iuse/known-flag-1.0; then
    pass "known-flag merged"
else
    fail "known-flag did not merge: $OUT"
fi
IEFF="$(sudo cat "$SB/var/db/pkg/test-iuse/known-flag-1.0/IUSE_EFFECTIVE" 2>/dev/null)"
echo "$IEFF" | grep -qw foo && pass "VDB IUSE_EFFECTIVE contains 'foo'" || fail "VDB IUSE_EFFECTIVE missing 'foo': '$IEFF'"

echo "=== IUSE_EFFECTIVE: flag outside the set dies (EAPI 8, PMS table 12.20) ==="
OUT="$(em test-iuse/unknown-flag 2>&1)"
echo "$OUT"
if installed test-iuse/unknown-flag-1.0; then
    fail "unknown-flag merged despite querying an out-of-set USE flag"
else
    pass "unknown-flag did not merge"
fi
echo "$OUT" | grep -q "not in IUSE_EFFECTIVE" && pass "die message names IUSE_EFFECTIVE" || fail "expected an IUSE_EFFECTIVE die message, got: $OUT"

echo "=== IUSE_EFFECTIVE: EAPI 4 (non-injection) still allows querying ARCH ==="
OUT="$(em test-iuse/eapi4-arch 2>&1)"
echo "$OUT"
if installed test-iuse/eapi4-arch-1.0; then
    pass "eapi4-arch merged (ARCH is in the non-injection IUSE_EFFECTIVE set)"
else
    fail "eapi4-arch did not merge — PMS 11.1.1's non-injection ARCH bullet regressed: $OUT"
fi

echo "=== blockers: -p preview must never touch the VDB ==="
em test-blockers/victim >/dev/null || { echo "victim merge (setup) failed"; exit 1; }
PRETEND_OUT="$(em -p test-blockers/attacker-weak 2>&1)"
echo "$PRETEND_OUT"
echo "$PRETEND_OUT" | grep -q ">>> would unmerge:.*victim" && pass "-p previews the weak-blocker unmerge" || fail "-p did not preview the unmerge: $PRETEND_OUT"
installed test-blockers/victim-1.0 && pass "-p left victim installed (no side effect)" || fail "-p actually unmerged victim!"

echo "=== blockers: weak (!) auto-unmerges the orphaned victim after the merge ==="
OUT="$(em test-blockers/attacker-weak 2>&1)"
echo "$OUT"
installed test-blockers/attacker-weak-1.0 && pass "attacker-weak merged" || fail "attacker-weak did not merge: $OUT"
installed test-blockers/victim-1.0 && fail "victim still installed after weak-blocker merge" || pass "victim auto-unmerged (weak, after blocker)"

echo "--- reset: drop attacker-weak, re-merge victim ---"
em -C test-blockers/attacker-weak >/dev/null 2>&1
em test-blockers/victim >/dev/null || { echo "victim re-merge failed"; exit 1; }

echo "=== blockers: strong (!!) also auto-unmerges when nothing else needs the victim ==="
OUT="$(em test-blockers/attacker-strong 2>&1)"
echo "$OUT"
installed test-blockers/attacker-strong-1.0 && pass "attacker-strong merged" || fail "attacker-strong did not merge: $OUT"
installed test-blockers/victim-1.0 && fail "victim still installed after strong-blocker merge" || pass "victim auto-unmerged (strong, before blocker)"

echo "--- reset: drop attacker-strong, re-merge victim + a dependent ---"
em -C test-blockers/attacker-strong >/dev/null 2>&1
em test-blockers/victim test-blockers/needs-victim >/dev/null || { echo "victim+needs-victim merge failed"; exit 1; }

echo "=== blockers: strong (!!) against a still-needed victim is a hard, unresolvable conflict ==="
OUT="$(em test-blockers/attacker-strong 2>&1; echo "exit=$?")"
echo "$OUT"
echo "$OUT" | grep -q "exit=0" && fail "strong blocker against a still-needed package exited 0" || pass "strong blocker against a still-needed package failed (nonzero exit)"
echo "$OUT" | grep -q "still required by" && pass "reports the still-needed obstacle" || fail "missing the still-required-by advisory: $OUT"
installed test-blockers/victim-1.0 && pass "victim was NOT unmerged (unresolvable, not auto-removable)" || fail "victim was unmerged despite being an unresolvable conflict!"
installed test-blockers/attacker-strong-1.0 && fail "attacker-strong merged despite an unresolved hard conflict" || pass "attacker-strong did not merge"

echo "--- reset: drop needs-victim, keep victim installed ---"
em -C test-blockers/needs-victim >/dev/null 2>&1

echo "=== blockers: -B/--buildpkgonly never installs, so it must not unmerge either ==="
em -B test-blockers/attacker-weak >/dev/null 2>&1
installed test-blockers/attacker-weak-1.0 && fail "-B actually installed attacker-weak" || pass "-B did not install attacker-weak"
installed test-blockers/victim-1.0 && pass "-B left victim installed (no unmerge on a package-only build)" || fail "-B unmerged victim despite never installing anything"

echo "==="
if [[ $FAIL -eq 0 ]]; then
    echo "ALL CHECKS PASSED"
else
    echo "SOME CHECKS FAILED"
fi
exit $FAIL
