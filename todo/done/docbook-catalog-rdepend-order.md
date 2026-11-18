# `app-text/build-docbook-catalog` RDEPEND ordering (unconfirmed real impact)

Resolved 2026-11-18 by the split build ([[cycle-break-use-split]]): the docbook postinsts pass on a fresh `--local` root.

2026-11-18: confirmed to break a real build. In a `--local` bootstrap `docbook-xsl-stylesheets`' `pkg_postinst` dies (`build-docbook-catalog` not installed yet).
Cause: the cycle `docbook-xsl-stylesheets` -R-> `build-docbook-catalog` -R-> `util-linux` -B-> `po4a` -B-> `docbook-xsl-stylesheets` (util-linux `nls`).
No order is valid; em drops the first RDEPEND edge. `build-docbook-catalog`'s RDEPEND is `|| ( util-linux app-misc/getopt )`; em takes the first branch.
Plan: [[cycle-break-use-split]] (getopt is masked on Linux, so the `||` choice alone cannot fix it).
Direction (no per-package workaround): at resolution level, (1) choose the other `||` branch when the first closes a cycle (Portage's `dep_zapdeps`
demotes a choice that recreates a known circular dependency unless it is installed), (2) break a hard cycle by building one package with a flag off, then rebuilding it with the flag on.


Status: 🔴 not started. docbook-xml-dtd-4.1.2-r7 and docbook-xsl-stylesheets-1.79.1-r4 run `build-docbook-catalog` in `pkg_postinst`,
so a wrong order can make postinst die: not harmless. Open question: why `repair_soft_inversions` left it inverted. Found incidentally 2026-08-24 while verifying
[[jobs-rdepend-backwards-edge-race]] via the enriched `--json` output
(`em -p --json --local DIR dev-lang/python sys-apps/gentoo-functions`).
Not yet confirmed to cause a real build failure the way the
meson-format-array/python case did — just spotted as `order_ok: false`
in the diagnostic output.

## What was seen

Three `RDEPEND` edges land `order_ok: false` in the same resolve:

```
app-text/docbook-xml-dtd-4.1.2-r7        RDEPEND  app-text/build-docbook-catalog-2.4
app-text/docbook-xsl-stylesheets-1.79.1-r4  RDEPEND  app-text/build-docbook-catalog-2.4
app-text/docbook-xsl-ns-stylesheets-1.79.1  RDEPEND  app-text/build-docbook-catalog-2.4
```

i.e. three `docbook-xml`/`docbook-xsl` packages RDEPEND on
`build-docbook-catalog`, but the linear order places
`build-docbook-catalog` *after* all three.

## How to attack

1. Confirm whether this actually breaks anything real — unlike the
   meson-format-array case, `build-docbook-catalog`'s own postinst likely
   just registers already-installed docbook catalogs with `sgml-common`;
   if none of the three consumers *execute* it during their own build
   (only need it satisfied eventually, like an ordinary soft RDEPEND),
   this may be entirely benign and not worth touching — check each
   ebuild's `pkg_postinst`/`src_install` before assuming it needs a fix.
2. If it does matter, use the same enriched `--json` (`order_ok`,
   `hard_cycle_edges`) to find the blocking chain, the same way
   [[jobs-rdepend-backwards-edge-race]] did.
3. If it's a genuine bootstrap-only cycle like the python one, the same
   `package.provided`/`TIER1` (`portage-cli/src/setup/provided.rs`)
   mechanism is the established fix shape — not a code change to
   `graph.rs` unless the diagnosis calls for it.
