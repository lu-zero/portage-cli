# `fetch+` / `mirror+` on the merge fetch path (PMS 7.3.2)

Status: ✅ merge path. Related: [[pms-compliance]],
[[distfile-fetch-reliability]] (`resolve_uri_map` was already right).

## PMS

EAPI 8+ (table 7.3): a `fetch+` / `mirror+` prefix on a SRC_URI component
**exempts** that URI from package-level `RESTRICT=fetch` / `RESTRICT=mirror`.
It is not itself a restriction.

A file already in DISTDIR satisfies `RESTRICT=fetch` (the user placed it);
`pkg_nofetch` runs only when a needed file is still missing.

## What `em` does

`DistfileResolver::resolve_uri_map` (emirrordist) treats the prefixes as
exemptions. Merge fetch uses `resolve()` + `Fetcher::fetch_distfile`, which
returns `FetchRestricted` when `df.restriction == Some("fetch")` **before**
checking DISTDIR — so a `fetch+` URI is the one that is *not* downloaded.

`build_distfiles` also skips GENTOO_MIRRORS when the URI is `mirror+`, the
opposite of an exemption. Package `RESTRICT` is never passed into merge
fetch, so plain `RESTRICT=fetch` still tries the network.

The field's own doc comment already calls this inverted.

## How to attack

1. `fetch_distfile`: already-present first; then `RestrictGate.fetch` with
   `fetch+`/`mirror+` as exemptions, else `FetchRestricted`.
2. `resolve` / `build_distfiles`: skip GENTOO_MIRRORS when the *package*
   is mirror-restricted and the URI is not `mirror+`.
3. `run_fetch` parses ebuild `RESTRICT` into `RestrictGate` and passes it.
4. Tests: `fetch+` downloads under `RESTRICT=fetch`; unprefixed does not;
   a file already in DISTDIR is `AlreadyPresent` either way.
