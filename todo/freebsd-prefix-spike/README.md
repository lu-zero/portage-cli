# `em --local` on real FreeBSD — spike status (2026-08-14)

Branch: `freebsd-prefix-spike` (off `master`@`4cb1c50`, which already has the
real, merged fix — see below). This directory holds the exploratory
artifacts that don't belong in `::gentoo` or `master`.

## What's already landed on `master` (real, tested fixes — not part of this branch)

Live-tested `em --local` end-to-end on a real, unmodified FreeBSD 14.4
aarch64 qemu VM (bypassing incus — its VM launcher is broken on this host,
see the `incus-docker-firewall-and-idmap` memory). Found and fixed five
real bugs, all genuinely platform-independent (just never exercised on a
non-GNU/non-Linux host before):

- `5c90917` — `select profile set/show` ignored `--local`
- `8ec531a` — `bzip2`/`xz` added to the host-tool preflight
- `ec833da` — banner-less host-tool checks (`python3`, `tar`, …) never fell
  back to the raw `$PATH`
- `d7250f9` — `make` needed the same GNU-vs-BSD banner check `sed`/`grep`
  already had
- `7217f81` — `mkdir_p_mode` called `mkdir("/")` for every install helper
  (harmless `EEXIST` on Linux, fatal `EISDIR` on FreeBSD)
- `4cb1c50` — `toolchain --setup`'s native plan skips the libc step under
  `USE=prefix-guest` (real Gentoo Prefix's own "host owns this OS's libc"
  mechanism — `virtual/libc`/`virtual/os-headers`'s RDEPEND already collapse
  to a bare blocker under it, `toolchain.eclass` already gates gcc's own
  libc-linking on it; `em` just needed to stop hardcoding `sys-libs/glibc`
  for the one step that bypasses the virtual via `--nodeps`)

`em setup --local` completes a full real bootstrap (repo sync, profile
link, `package.provided`, `sys-apps/baselayout` merge) end-to-end on
FreeBSD today, same as on the two real Linux hosts tested (Gentoo sandbox,
Debian 12).

## What's on *this* branch — the profile spike (not upstreamable as-is)

`toolchain --setup` needs a real *profile* to run against — `::gentoo` has
none for `arm64-freebsd` (or any FreeBSD arch at all; the whole `bsd`/
`freebsd` category and every `~*-fbsd`/`~*-freebsd`-suffixed `KEYWORDS`
token have been fully dropped from the current tree, matching the dead
Gentoo/FreeBSD project). `profile-bsd-freebsd-arm64/` in this directory is
a throwaway profile crafted by hand this session, modeled on
`profiles/prefix/darwin/macos`'s shape:

```
prefix/bsd/                  # family — ELIBC="FreeBSD" KERNEL="FreeBSD"
prefix/bsd/freebsd/          # OS
prefix/bsd/freebsd/arm64/    # arch leaf — ARCH="arm64-freebsd",
                              # ACCEPT_KEYWORDS="~arm64-freebsd",
                              # CHOST="aarch64-unknown-freebsd14.4",
                              # ABI/MULTILIB_ABIS/LIBDIR_arm64, use.mask/
                              # use.force for the ABI-select flag
```

Confirmed working live: `em --local DIR --info` resolves `CHOST`/`ELIBC`/
`KERNEL`/`ARCH` correctly with zero errors from unregistered
`profiles/desc` values; `sys-apps/baselayout` merges cleanly under it.

**Real historical convention differs from this guess** (found via web
search, not verified against actual source): NetBSD-under-Prefix really
does use `profiles/prefix/bsd/netbsd/<ver>/<arch>` — same *shape* as this
spike, good sign — but the standalone **"Gentoo FreeBSD"** wiki project
(a separate, non-Prefix, native distribution effort — CHOST like
`default/bsd/fbsd/amd64/`, real `sys-freebsd/*` ebuilds for the base
system) is a *different* thing entirely and doesn't apply here: `em
--local` is specifically Gentoo **Prefix** (EPREFIX-based, borrows the
host via `prefix-guest`), so `prefix/bsd/freebsd/` is the conceptually
right family — it just needs real reference content instead of guesses
for the exact keyword suffix (`-freebsd` vs historical `-fbsd`?), ABI
naming, and anything else a real profile would carry.

**Candidate real sources, not yet checked against actual file content**:
- [haubi/prefix-overlay](https://github.com/haubi/prefix-overlay) — mirror
  of the actual Gentoo Prefix staging overlay (content not yet merged to
  `::gentoo`, or dropped from it) — most likely place to find the real,
  once-real `prefix/bsd/freebsd/` tree instead of reinventing it.
- [zoujiaqing/gentoo-bsd](https://github.com/zoujiaqing/gentoo-bsd) —
  dedicated overlay explicitly carrying `sys-freebsd/*` packages (the
  `freebsd-lib`-style base-system ebuilds Luca recalled correctly existing
  historically, confirmed dropped from the current `::gentoo` mirror).

**Next session should start here**: clone/inspect those two repos for real
`prefix/bsd/freebsd/` (or equivalent) content before continuing to
hand-guess. If real content exists, prefer copying it (adapted, EAPI-bumped
if needed) over this spike's guesses.

## `package.accept_keywords` — the other blocker, separate from the profile

Even with the profile right, **zero packages in `::gentoo` have any
`~*-freebsd` (or `~*-fbsd`) keyword at all** — expected, nobody's tagged
this platform since the historical project's removal. `toolchain --setup
-p` walks deeper into the dependency tree with each `package.accept_keywords`
addition (baselayout needed one, unlocked binutils, which needed its own
for `elt-patches`/`gnuconfig`/`gettext`/`binutils-config`/`virtual/zlib`/
`app-alternatives/{lex,yacc}`, plus transitively `dev-vcs/git`). This is
real, expected whack-a-mole for a brand-new platform, not an `em` bug — but
going all the way to a working `sys-devel/gcc` will need several more
rounds plus real (potentially lengthy) compile time.

`package.accept_keywords` in this directory is the exact file state
reached this session, on `/root/em-fbsd/etc/portage/package.accept_keywords`
in the FreeBSD VM — a snapshot to resume from, not a finished answer.
Note: `Dep::parse` does **not** support the bare `*/*` wildcard atom real
portage's `package.accept_keywords` allows (confirmed empirically — a
`*/* **` line had zero effect); every entry has to name a real atom. Worth
a separate, small `em` fix on its own merits if this pattern keeps
recurring (real portage users write `*/* **` for exactly this "temporarily
accept everything" bring-up case).

## Environments still live (as of this session)

- FreeBSD VM: qemu process still running, `/root/em-fbsd` has the profile +
  `package.accept_keywords` above already in place, ready to resume
  `toolchain --setup -p` from where this session left off.
- `em-clean4`/`em-fbsd` on the same VM, plus `em-local-test` (crossdev-stages
  Gentoo sandbox) and `em-local-debian` (incus Debian 12 container) from
  the earlier `--local` setup/host-tool-prereq verification — all still up,
  not part of this branch.
