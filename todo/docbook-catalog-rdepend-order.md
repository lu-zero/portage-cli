# `app-text/build-docbook-catalog` RDEPEND ordering (unconfirmed real impact)

Status: 🔴 not started, found incidentally 2026-08-24 while verifying
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
