# `em --info <atom>`: the package section

Status: 🟡 steps 1 to 3 landed 2026-11-23; uninstalled packages and suggestions open.

## Why

`emerge --info` takes optional atoms and reports how each match was built.

## What emerge does

Checked against Portage 3.0.83.

**Atoms, before any output.** One bad atom means exit 1 and no report.

- Not a valid package atom: an error. `@set` is valid and expands, and bare `world` and `system`
  are sets too. An unknown set is an error.
- A short name expands against the installed packages, then the repositories, then binary
  packages under `--usepkg`. A real package beats a `virtual/` or `acct-*` of that name. Several
  real packages with no such shadow are an ambiguity error.
- No installed, repository, or (`--usepkg`) binary package for the name:
  `emerge: there are no ebuilds to satisfy "x".`, plus similar-name suggestions.
- The name exists but nothing matches, for example an uninstalled version: no error, no entry.

**System report.** With atoms, the first rule is followed by a centred `System Settings` title
and a second rule. Otherwise it is unchanged.

**Package section.** Printed after the report when at least one package matched, under a centred
`Package Settings` title. Every installed match of an atom gets:

```text
sys-libs/zlib-1.3.2-r1::gentoo was built with the following:
USE="-minizip -static-libs -verify-sig"
LDFLAGS="..."
```

- USE lists enabled flags, then disabled IUSE flags, each sorted. `--alphabetical` merges them.
- `ARCH` is left out. `USE_EXPAND` flags move to their own variable, and hidden groups are omitted.
- A forced or masked flag is parenthesised: `(-flag)`.
- `CHOST`, `CFLAGS`, `CXXFLAGS`, `FEATURES` and `LDFLAGS` come from `environment.bz2`. Each prints
  only when its words differ from the current config. One missing there goes on an `Unset:` line.

**`pkg_info`.** A package defining the phase then runs it from its saved ebuild.

**Not installed.** With no installed match, the best repository ebuild that defines `pkg_info`
(EAPI 4+) is reported as `would be built with the following:` and its phase runs. Under
`--usepkg`, a local binary package can be reported the same way.

## Plan

1. ✅ Validate atoms and sets, and fail before the report.
2. ✅ Print `System Settings` and `Package Settings` for installed matches, in text and `--json`.
   All 785 installed packages without `pkg_info` matched `emerge --info` on the development host.
3. ✅ Run `pkg_info` of installed packages, stricter than Portage, which leaves the phase on the
   host network and as root: a container with no network, read-only trees and a private `/tmp`.
   Root drops to `portage` first. Without a container the phase is skipped with a warning.
4. 🔴 Report uninstalled ebuilds and binary packages. Never run `pkg_info` from a binary package
   without an explicit opt-in.
5. 🔴 Add similar-name suggestions to the no-match error.
