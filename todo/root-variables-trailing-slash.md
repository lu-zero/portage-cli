# `ROOT`, `EROOT`, `D`, `ED` end in a slash for EAPI 7 and later

Status: ✅ fixed 2026-10-10, both halves: `em` follows PMS and the brush
fork keeps the pattern's separators (`8d77c6d7`). Found while tracing
[[stage-root-links-carry-the-build-path]].

## What PMS says

From EAPI 7 on, `D`, `ED`, `ROOT` and `EROOT` have no trailing slash,
and `SYSROOT`, `ESYSROOT` and `BROOT` never had one. On a plain system
`ROOT` and `BROOT` are therefore the empty string. EAPI 6 and earlier:
`D`, `ED`, `ROOT`, `EROOT` end in a slash.

## What `em` does

`run_phase` normalises the root to end in `/` and `root_vars` builds
`ED` the same way, for every EAPI. Saved environment of an EAPI 8
package merged into `--root /root/s1`:

```
ROOT="/root/s1/"   EROOT="/root/s1/"   BROOT="/"   ESYSROOT="/"   SYSROOT=""
```

## The case that showed it

`alternatives.eclass` lists candidates with `echo ${EROOT}${pattern}`
and strips `${EROOT}` from each result. With `EROOT="/root/s1/"` the
pattern is `/root/s1//usr/bin/ptar-[0-9]*`. bash returns matches with
the doubled slash, and stripping `/root/s1/` leaves `/usr/bin/ptar-…`.
brush returns them with the slash collapsed, and stripping leaves
`usr/bin/ptar-…`, which the eclass then takes for a different directory
from the link's and links absolutely, with `ROOT` in front:

```
usr/bin/ptar -> /root/s1//usr/bin/ptar-3.120.0-perl-5.44.0
```

37 such links from perl alone in a stage1, all dangling once the root
is entered or moved. With a PMS `EROOT` there is no doubled slash and
the difference between the shells never arises.

So two things, either of which hides the other:

- **`em`: trailing slashes on the root variables in EAPI 7+.** The real
  defect; an ebuild written against PMS can rely on their absence.
- **brush: pathname expansion collapses `//`.** bash keeps the pattern's
  own slashes. Measured: `echo /usr//bin/env*` gives `/usr//bin/env …`
  in bash and `/usr/bin/env …` in brush.

## What a fix has to watch

- The value for a plain system: empty for `ROOT` and `BROOT`.
- `em`'s own builtins and helpers that read `D`/`ED`/`ROOT` from the
  shell and join paths by concatenation.
- `ed_image_dir` and anything else that trims or expects the slash.
- The 55 shell tests pin several of these values; they need the EAPI
  taken into account, not just updating.

## Fixed (2026-10-10)

- **`em`.** `slashed_for(eapi, path)` in `portage-repo/src/build/shell.rs`
  is applied where `ROOT`, `EROOT`, `D` and `ED` are set, in the phase
  runner and in the metadata sourcing: a slash up to EAPI 6, none from
  EAPI 7, the empty string for the system root. `SYSROOT`, `ESYSROOT`
  and `BROOT` never end in one. `em`'s own helpers that read these
  already trimmed or accepted both forms; none changed. Test: the rule
  for EAPI 6 and 8 against an offset root and `/`.
- **brush.** Pathname expansion spells its results as the pattern does:
  a doubled separator is kept in the literal directories before the
  first glob component and written once after it, as bash does. 25
  patterns compared with bash; a compat case.

Full stage run, sandbox `em-stage-slash` (toolchain and stage1 from
source with `-b`, then both with `-K`):

- four steps exit 0; 159 binary installs, no source build;
- the two roots are identical outright, 141 packages each;
- **no link in either root names the root's path** (71 before);
- against the previous run's source-built root: same packages, same
  CONTENTS path lists, same tree paths. Link targets differ in 85
  places, all for the better: the 71, and `usr/bin/{emerge,ebuild,…}`,
  which were `/usr/lib/python-exec/python-exec2` and are now
  `../lib/python-exec/python-exec2`, as on a real Gentoo system.

Seen there and fixed since: the dangling `gfortran` links, see
[[gcc-wrappers-outlive-a-reduced-rebuild]].
