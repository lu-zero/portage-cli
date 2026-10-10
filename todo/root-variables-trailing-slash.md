# `ROOT`, `EROOT`, `D`, `ED` end in a slash for EAPI 7 and later

Status: 🔴 found 2026-10-10 while tracing
[[stage-root-links-carry-the-build-path]]; not fixed. Touches the
environment of every phase.

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
