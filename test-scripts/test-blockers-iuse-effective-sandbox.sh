#!/usr/bin/env bash
# test-blockers-iuse-effective-sandbox.sh — regression test for the
# 2026-08-20 PMS pass: blocker auto-unmerge (PMS 8.3.2, conflicts.rs) and
# IUSE_EFFECTIVE (PMS 11.1.1 / table 12.20, iuse_effective.rs +
# use_flag.rs). Both were unit-tested in isolation but never exercised
# through a real merge/VDB/build-shell round trip — that's the gap this
# script closes.
#
# Runs entirely unprivileged: every command inside the sandbox goes through
# `crossdev-stages sandbox run` (hakoniwa — unshared mount/PID/user
# namespaces, its own fresh /proc, /dev, /tmp, DNS, uid/gid mapping), and
# every file this script writes goes *through* that same mechanism (piped
# into `dd`/`tar` running inside the sandbox) rather than being written
# *onto* the sandbox's host directory with `sudo`. That distinction matters:
# a file `sandbox run` creates lands on the host owned by your own uid
# (hakoniwa's namespace mapping), which `sandbox destroy` can always clean
# up itself; a file written with `sudo tee`/`sudo cp` lands owned by real
# root, a privilege domain `sandbox destroy`'s own (unprivileged) removal
# cannot touch — the first version of this script did that and left
# behind a sandbox `destroy` couldn't fully remove. No `sudo` at all here
# now, and — the earlier motivation for this rewrite — no `chroot` or
# manual `mount --bind` either: a raw chroot shares the host's mount
# namespace, so a script that dies mid-run can leave real mounts behind;
# hakoniwa's namespaces are torn down with the process, nothing to leak.
#
# `sandbox run`'s CMD is rejoined with plain spaces before being handed to
# `bash --login -c`, so nested shell syntax (`bash -c "a && b"`, `cat >
# file`) does not survive the round trip — every call below is a flat,
# unquoted argv command (`mkdir -p DIR`, `dd of=FILE`, `tar -xf - -C DIR`),
# never a string containing shell operators.
#
# Builds a tiny synthetic overlay (`test-pms`, via `em select repository
# create`) with seven trivial ebuilds — no SRC_URI, no network fetch —
# rather than hunting for a real Gentoo package pair with the right
# blocker/USE shape (the canonical systemd[resolvconf]/openresolv blocker
# pair is far too heavy to build here). This isolates exactly the
# integration behavior under test and keeps the whole run fast. The real
# `portage-repo/gentoo` checkout is streamed in via `tar` (not bind-mounted,
# not `cp`) so `masters = gentoo` has real profiles/licenses to inherit,
# without a live mount tying the sandbox to the host tree.
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
FAIL=0

pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1"; FAIL=1; }

# Every crossdev-stages invocation needs its cwd inside the checkout
# (--project-dir defaults to `.`); run() wraps that so every call site
# below reads as a plain command.
cds() { ( cd "$CROSSDEV_STAGES_DIR" && "$CDS" "$@" ); }

cleanup() {
    echo "--- cleaning up ---"
    if [[ $KEEP -eq 0 ]]; then
        # NAME is positional for destroy (unlike run/enter/setup/prepare's
        # --name) — the wrong form silently no-ops and leaves the sandbox
        # behind, so this checks the actual exit status.
        cds sandbox destroy "$SANDBOX" \
            || echo "warning: sandbox destroy failed — check crossdev-stages sandbox list" >&2
    else
        echo "kept sandbox: $SANDBOX — poke it with:"
        echo "  (cd $CROSSDEV_STAGES_DIR && $CDS sandbox enter --name $SANDBOX)"
    fi
}
trap cleanup EXIT

echo "--- building em (release) ---"
( cd "$REPO_ROOT" && cargo build --release -p portage-cli ) || { echo "em build failed"; exit 1; }

echo "--- fresh sandbox: $SANDBOX ---"
cds sandbox destroy "$SANDBOX" 2>/dev/null
cds sandbox setup --arch aarch64 --name "$SANDBOX" || { echo "sandbox setup failed"; exit 1; }

run_in() { cds sandbox run --name "$SANDBOX" -- "$@"; }
em() { run_in /usr/local/bin/em "$@"; }

# Pipe stdin into a file at the container path DEST, entirely through
# `sandbox run` — never a host-side `sudo tee`. `dd` (not a `cat >`
# redirect) because CMD's flattening can't carry shell syntax; `dd` reads
# stdin as plain argv, no redirect needed.
write_in() {
    local dest="$1"
    run_in mkdir -p "$(dirname "$dest")"
    cds sandbox run --name "$SANDBOX" -- dd "of=$dest" bs=1M status=none
}

echo "--- wiring sandbox (streamed in, no sudo) ---"
run_in mkdir -p /usr/local/bin /var/db/repos
dd if="$REPO_ROOT/target/release/em" bs=1M status=none | write_in /usr/local/bin/em
run_in chmod +x /usr/local/bin/em
tar -cf - -C "$REPO_ROOT/portage-repo" gentoo \
    | cds sandbox run --name "$SANDBOX" -- tar -xf - -C /var/db/repos \
    || { echo "streaming the gentoo tree in failed"; exit 1; }

cat <<'EOF' | write_in /etc/portage/make.conf
CHOST="aarch64-unknown-linux-gnu"
CFLAGS="-O2 -pipe"
CXXFLAGS="-O2 -pipe"
ACCEPT_KEYWORDS="~arm64"
FEATURES="-sandbox -usersandbox"
EOF

em --help >/dev/null || { echo "em --help failed in the sandbox"; exit 1; }
pass "em runs under crossdev-stages sandbox run, no sudo"

echo "--- creating synthetic overlay: test-pms ---"
em select repository create test-pms || { echo "overlay create failed"; exit 1; }
OVERLAY=/var/db/repos/test-pms
printf 'test-blockers\ntest-iuse\n' | write_in "$OVERLAY/profiles/categories"

write_ebuild() {
    # write_ebuild CAT/PN-VER <<'EOF' ... EOF
    write_in "$OVERLAY/${1}.ebuild"
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

# F4 regression: real dependency (not just naming) forces this to build
# strictly after attacker-weak, then always dies. Proves the AfterBlocker
# unmerge is gated on whether attacker-weak itself finished, not on the
# whole run's Result — an unrelated failure after it must not silently,
# permanently drop the pending unmerge.
write_ebuild test-blockers/fails-after/fails-after-1.0 <<'EOF'
EAPI=8
DESCRIPTION="blocker test: F4 regression, fails after the real blocker owner"
SLOT="0"
KEYWORDS="~arm64"
LICENSE="GPL-2"
RDEPEND="test-blockers/attacker-weak"
S="${WORKDIR}"
src_install() { die "intentional failure for the F4 regression test"; }
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

installed() { run_in test -d "/var/db/pkg/$1"; }

echo "=== IUSE_EFFECTIVE: known flag merges clean, VDB records the set ==="
OUT="$(em test-iuse/known-flag 2>&1)"
echo "$OUT"
if installed test-iuse/known-flag-1.0; then
    pass "known-flag merged"
else
    fail "known-flag did not merge: $OUT"
fi
IEFF="$(run_in cat /var/db/pkg/test-iuse/known-flag-1.0/IUSE_EFFECTIVE 2>/dev/null)"
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
echo "$PRETEND_OUT" | grep -q "\[uninstall.*\].*victim" && pass "-p previews the weak-blocker unmerge" || fail "-p did not preview the unmerge: $PRETEND_OUT"
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

echo "--- reset: re-merge victim for the F4 partial-failure regression ---"
em -C test-blockers/attacker-weak test-blockers/fails-after >/dev/null 2>&1
em test-blockers/victim >/dev/null || { echo "victim re-merge failed"; exit 1; }

echo "=== F4 regression: the AfterBlocker unmerge survives an unrelated later failure ==="
# fails-after real-depends on attacker-weak, so attacker-weak (the actual
# blocker trigger) always finishes first, then fails-after always dies —
# proving the pending unmerge is gated on attacker-weak's own completion,
# not on the whole run's final (necessarily nonzero) exit code.
OUT="$(em test-blockers/attacker-weak test-blockers/fails-after 2>&1; echo "exit=$?")"
echo "$OUT"
echo "$OUT" | grep -q "exit=0" && fail "expected a nonzero exit (fails-after must fail)" || pass "run failed overall as expected (fails-after dies)"
installed test-blockers/attacker-weak-1.0 && pass "attacker-weak (the real blocker trigger) merged before the failure" || fail "attacker-weak did not merge: $OUT"
installed test-blockers/fails-after-1.0 && fail "fails-after should never install (it always dies)" || pass "fails-after did not install"
installed test-blockers/victim-1.0 && fail "F4 regression: victim survived despite its trigger (attacker-weak) already succeeding" || pass "victim was still unmerged despite the later unrelated failure (F4 fixed)"

echo "==="
if [[ $FAIL -eq 0 ]]; then
    echo "ALL CHECKS PASSED"
else
    echo "SOME CHECKS FAILED"
fi
exit $FAIL
