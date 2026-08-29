# Distfile fetching — reliability (recurring build-killer)

STATUS: **all four original facets FIXED.** bash root cause `8a9558b`; facet A
(computed SRC_URI) `2965fa2` (2026-06-15); facet B fetch hardening
(C.3/C.4/C.5) 2026-06-27. `em select mirrors` (§D) also landed. The §E
GENTOO_MIRRORS parity audit (2026-06-28) found one fixed gap (iteration
order) and several remaining ones. **§F (2026-08-29) — two real,
live concurrency bugs, both found and fixed same-day.**

## F. Concurrent same-filename `Distfile` write races — ✅ fixed 2026-08-29

Live during a from-scratch riscv64 `em stages --stage1` run:
`dev-build/autoconf-2.73-r2` failed `unpack` — the cached
`autoconf-2.73.tar.xz` failed BLAKE2B verification. Downloaded the exact
same URL by hand: it matches the Manifest exactly. So `em` verified a
*corrupt* local file, not a bad upstream.

Root cause: `resolve`/`resolve_all` (`resolver.rs`) build **one `Distfile`
per raw `SRC_URI` token, not merged by filename** — already flagged in
`resolve_uri_map`'s own doc comment as "a bug for callers writing
concurrently". `autoconf-2.73-r2`'s `SRC_URI` has three entries
(`mirror://gnu/...`, two direct GNU-mirror URLs) that all resolve to the
same filename, so `collect_uri_pairs_all` + `build_distfiles` produce
three separate `Distfile` objects sharing one `filename`.
`Fetcher::fetch_all_digests` (`fetch.rs`) runs up to `max_concurrent`
(default **4** — a `portage-distfiles` constant, unrelated to `em
--jobs`) of these concurrently via `buffered()`, with no dedup by
filename, and the write itself is non-atomic (`fetch_builtin`, direct to
`dest`, no temp+rename unless `atomic_write` is set). Two concurrent
fetches for the same filename race writing the same `DISTDIR` path;
the interleaved result matches neither download.

Reproducible on any multi-URI `SRC_URI` whose entries share a filename,
independent of `em --jobs` — `max_concurrent` alone is enough.

**Fix direction**: merge `Distfile`s by filename in `resolve`/`resolve_all`
the same way `resolve_uri_map` already does (build one URL list per
filename, not per raw URI token) — removes the possibility of two
`Distfile`s for the same file existing at all, rather than trying to
serialize/lock around the race after the fact.

**Fixed** (`4fdc0b2`, 2026-08-29): `build_distfiles` now merges by
filename. Verified this specific shape gone — re-running the same
riscv64 stage1 build no longer shows a manifest-verification mismatch.

**Broader sibling race, also fixed** (2026-08-29, same day): `dev-build/
autoconf-2.73-r2` was built *twice* in one plan — once for the host
cross-toolchain sysroot, once for the target root — each its own
independent `resolve()`/`Fetcher` call, sharing one on-disk `DISTDIR`.
Under `em --jobs`, one job's fetch reported the file "already present"
while the *other* job was still mid-download of that same path; unpack
hit a genuinely truncated file (`xz: Compressed data is corrupt`). The
filename-merge fix above cannot close this — it's across independent
package-merge tasks, not within one `resolve()` call.

Fixed with a real per-distfile flock (`fetch_distfile_digests` now holds
an exclusive lock on `<dest>.lock` for the whole fetch, present-check
included), matching portage's own `FEATURES=distlocks` — covers both
`em --jobs` concurrency and two separate `em` invocations sharing a
`DISTDIR`. Unit-tested (`lock_distfile_serializes_concurrent_acquisitions`).

**Follow-up idea, 🔵 not started**: a flock only serializes contention
after the fact — every concurrent fetcher still independently resolves
and attempts each distfile it needs, one lock-wait at a time. A cleaner
long-term design: a single pre-pass over the whole build plan collects
every distfile every package will need (already dedupable by filename,
same as `resolve_uri_map`), fetches each exactly once up front, and only
then starts the parallel package builds — no per-package fetch contention
at all, and a build's real "am I ready to compile" moment is no longer
entangled with network I/O. Worth designing once `em stages`/`--jobs`
building is otherwise stable; out of scope for the immediate race fix.

## E. GENTOO_MIRRORS parity vs portage (audit 2026-06-28)

Audited against `gentoo/portage` `fetch.py` + `make.conf(5). What em matches and
what it does not:

**Matched:**
- Read order: env → `make.conf` → `make.globals`, override (not incremental —
  `GENTOO_MIRRORS` is not in `const.INCREMENTALS`). em: `gentoo_mirrors_list`.
- `mirror://` scheme token is lowercase-only (case-sensitive). em: `strip_prefix`.
- filename-hash `BLAKE2B 8` layout (`distfiles/<xx>/<file>`, hash of the
  *filename*), hashed-first with flat as legacy fallback. Matches gentoo's live
  `layout.conf`. em: `gentoo_distfile_urls`.
- `RESTRICT=mirror` suppresses the gentoo-mirror fallback.

**Fixed (2026-06-28):**
- ✅ **Iteration order.** em used to try the upstream SRC_URI URL first, then
  GENTOO_MIRRORS; portage does the opposite — mirrors-before-upstream
  (`make.conf(5)`: "These locations are used to download files before the ones
  listed in the ebuild scripts"). Now mirrors-first. Reliability win: gentoo's
  CDN mirrors are tried before flaky upstreams (ftp.gnu.org, sourceforge).

**Remaining gaps (lower priority, tracked here):**
- 🔴 **No remote `layout.conf` fetch/cache.** em hardcodes `filename-hash BLAKE2B
  8`; portage fetches `<mirror>/distfiles/layout.conf`, caches it 24h in
  `$DISTDIR/.mirror-cache.json`, parses `[structure]` (flat / filename-hash /
  content-hash, uppercase algo names), and falls back to flat on any error. Fine
  while gentoo's layout is stable; wrong for a mirror that advertises a different
  layout. `fetch.py:632,733-790`.
- 🔴 **No `/etc/portage/mirrors` (`CUSTOM_MIRRORS_FILE`).** portage reads this
  `grabdict`: key `local` adds local/public mirrors; any other key overrides a
  `mirror://<key>/` set (tried before the official thirdparty list). em reads
  neither. `fetch.py:986`.
- 🔴 **No `/`-prefixed filesystem mirrors.** portage treats a `GENTOO_MIRRORS`
  entry starting with `/` as a mounted distfiles tree (`shutil.copyfile`,
  layout-aware). em treats it as a URL. `fetch.py:1028-1032,1510-1520`.
- 🟡 **`RESTRICT=primaryuri` / `RESTRICT=fetch` unhandled.** portage reorders so
  primaryuri values go first; `fetch` implies mirror-restriction and drops
  upstream unless `fetch+`/`mirror+` prefixed. em gates only on `mirror`.
  `fetch.py:883,1063,1119-1121,1189-1191`.
- 🟡 **`FEATURES=mirror`/`force-mirror`/`lmirror` unhandled.** `force-mirror`
  drops upstream URIs; `mirror` fetches all of SRC_URI regardless of USE.
  `fetch.py:883-892,1064`.
- ✅ **`mirror+`/`fetch+` URI prefixes — DONE.** `src_uri.rs` parses both;
  `resolve_uri_map` and merge `Fetcher` treat them as PMS 7.3.2 exemptions.
  `run_fetch` passes package `RESTRICT` as `RestrictGate`. See
  [[pms-fetch-plus]].
- 🔵 **thirdparty mirrors not `random.shuffle`d.** portage shuffles the
  thirdparty mirror set per-file for load balancing; em preserves list order.
  `fetch.py:1156`. Intentional divergence (deterministic); revisit if a mirror
  gets overloaded.
- 🔵 **`mirror://gentoo/<path>` routing divergence.** portage resolves it via
  the `thirdpartymirrors` "gentoo" entry (verbatim append → flat
  `…/distfiles/<path>`); em routes it through the filename-hash layout. Both work
  (the gentoo mirror serves flat + hashed), and em's is the modern layout, but it
  is a divergence from portage's literal behaviour.

## 0. RO-distdir file not exposed in DISTDIR — FIXED 2026-06-25 (the bash killer)

The real cause of the bash `eapply: patch failed: …/bash53-001` failure: the
patch lives in the host **read-only** distdir `/var/cache/distfiles`, em's fetch
fast-path found it there and returned `AlreadyPresent`, but never linked it into
the writable DISTDIR (`~/.cache/distfiles`) where unpack/eapply look. So em
reported "already present" for a file the build couldn't open — and because the
fetch genuinely succeeded, the fail-fast `bail!` never fired (it surfaced late at
`eapply`). Fixed in `fetch.rs::fetch_distfile`: when the valid copy is in a RO
distdir, symlink it into DISTDIR (hard-link/copy fallback), as portage does.
Unit-tested. Verified: bash builds and **runs in the stage1 chroot**.

## A. Dynamically-built `SRC_URI` extracted as EMPTY — FIXED (`2965fa2`, 2026-06-15)

Root cause: the `fetch` phase called `source_ebuild` again mid-run, re-sourcing
over an already-sourced shell. Eclass include guards fire on the second pass, so
the re-source no-op'd every eclass — dropping their global-scope effects. For
bash that meant the `for ((…))` loop building `my_urls+=( … )` and the
`${my_urls[*]}` join were lost, leaving `SRC_URI` empty; the build then died in
`src_prepare` at `eapply: patch failed: …/bash53-001` (file never fetched), and
because `SRC_URI` was genuinely empty the fail-fast `bail!` never fired — a fetch
problem surfaced as a late prepare failure.

Fix: fetch reads `SRC_URI` from the already-sourced live shell (the `pretend`
phase runs first in a merge) via `is_phase_sourced`, sourcing only when `fetch`
runs standalone with nothing sourced yet. Verified live: `em ebuild
bash-5.3_p15.ebuild fetch` computes the full SRC_URI (tarball + `bash53-001..015`).

Empty `SRC_URI` is otherwise legitimate (84 ebuilds ship `SRC_URI=""` — meta/
virtual packages), so there is intentionally no fail-fast on an empty value; the
bug was evaluation correctness, now fixed.

Original notes (kept for the diagnosis trail):

`app-shells/bash-5.3_p15` builds `SRC_URI` in global scope:

`app-shells/bash-5.3_p15` builds `SRC_URI` in global scope:

```bash
for (( my_patch_idx = 1; my_patch_idx <= PLEVEL; my_patch_idx++ )); do
    printf -v my_patch_ver %s-%03d "${my_p}" "${my_patch_idx}"
    my_urls+=( "mirror://gnu/bash/${MY_P}-patches/${my_patch_ver}" )
    MY_PATCHES+=( "${DISTDIR}/${my_patch_ver}" )
done
SRC_URI="${my_urls[*]} verify-sig? ( ${my_urls[*]/%/.sig} )"
```

em's metadata extraction returns **empty** `SRC_URI` for bash
(`fetch: nothing to fetch (SRC_URI is empty)`), so the `bash53-001..015`
patches are never fetched. The build then dies in `src_prepare`:
`die: eapply: patch failed: …/distfiles/bash53-001` (file absent).

Because SRC_URI is empty, em's fetch **fail-fast is bypassed** — it
"successfully fetches nothing" and proceeds to build, surfacing a *fetch*
problem as a late *prepare* failure. (Contrast: when SRC_URI is correct,
`ebuild.rs:687` `bail!("one or more distfiles could not be fetched")` aborts
before building — what libpcre2 hit.)

Likely a brush/metadata gap evaluating the loop + array-star join
`${my_urls[*]}` in global scope (cf. [[brush-ifs-star-join-fix]] — star-join was
fixed once; re-check `+=( )` array append + `${a[*]}` here). **Verify what
`em` extracts as bash's SRC_URI and where it goes empty** (metadata source vs a
stale/empty cache entry). High value: any ebuild with a computed SRC_URI
(bash, many `-patches` loops, git-snapshot fallbacks) is affected, and the
failure mode is silent-until-prepare.

## B. Genuine fetch failures aren't always recoverable

`dev-libs/libpcre2-10.47` (cross `less`) → `one or more distfiles could not be
fetched`. Here the SRC_URI was correct and em **did** fail-fast — but the file
couldn't be downloaded (mirror/availability/timeout). Recurs often enough to
warrant: more mirrors / `GENTOO_MIRRORS` fallback ordering, retries with
backoff, and resumable/partial-download handling. Confirm em walks
`mirror://` → `GENTOO_MIRRORS` and the upstream URLs in order, and honours
`thirdpartymirrors`.

## C. Three concrete bugs from the @system stage build (2026-06-26)

The `em --root @system` shakeout ([[stage-build-shakeout]]) failed popt/tar/psmisc
— not flakiness, three distinct bugs:

1. **No GENTOO_MIRRORS fallback — FIXED (`e0bae58`).** `gentoo_mirrors_list()`
   read only the env + `/etc/portage/make.conf`, never `make.globals` (where the
   default `GENTOO_MIRRORS="http://distfiles.gentoo.org"` lives). make.conf rarely
   overrides it → empty mirror list → a distfile whose upstream URL failed had no
   fallback (popt: `error decoding response body` on ftp.rpm.org). Now reads
   make.globals last. Verified popt fetches.
2. **Success-after-fallback still marked failed — appears RESOLVED.** The
   `fetch_distfile` URL loop early-returns `Ok(Downloaded)` on the first URL that
   succeeds, and the `ebuild.rs` caller only sets `any_failed` on a per-distfile
   `Err`. So a 404-then-ok within one distfile is reported ok. If it recurs it'll
   be the two-distfile (`.sig`) case — re-check then.
3. **Corrupt partial cached, never refetched — FIXED (2026-06-27).** `fetch_builtin`
   now resumes only a *size-plausible* partial (`is_resumable`: present and
   `< manifest size`; never `>=` expected, never unknown-size), and on **any**
   resume/download that fails to verify it **discards the file** (`verify_or_discard`)
   and does one fresh non-Range download. A corrupt/short/HTML leftover can no
   longer be Ranged-into and wedge every retry. Unit test
   `resume_only_strict_size_partials` (incl. the psmisc 139 KB-vs-432 KB case).
   The dead incremental SHA512/BLAKE2B hashing in the old `fetch_builtin` (computed,
   never compared — `verify_file` re-reads) was dropped in the rewrite.
4. **Mirror flat-layout 404 — FIXED (2026-06-27).** `resolver.rs`
   `gentoo_distfile_urls` now builds the **filename-hash** path
   `distfiles/<xx>/<filename>` (`<xx>` = first 8 bits / 2 hex of
   `BLAKE2B-512(filename)`, matching portage's `FilenameHashLayout` and the live
   `layout.conf` `filename-hash BLAKE2B 8`), hashed-first with the flat path as a
   legacy fallback — for both the GENTOO_MIRRORS fallback and `mirror://gentoo/`.
   **Verified live**: `…/distfiles/28/psmisc-23.7.tar.xz` → HTTP 200 (432208 B);
   flat → 404. Tests `gentoo_filename_hash_subdir_matches_portage`,
   `mirror_gentoo_uses_filename_hash_layout`. Note: it hashes the *filename*, not
   the manifest content hash (that's the `filename-hash`, not `content-hash`,
   layout — the mirror's `layout.conf` is authoritative).
5. **sourceforge HTML/error body accepted — FIXED (2026-06-27).** `download_full`
   rejects a 2xx whose `Content-Type` is `text/html` (`is_html`) — a distfile is
   never HTML, so an SF "file not found"/mirror-picker page is treated as a fetch
   failure, the next URL is tried, and (via C.3) it's never cached. Combined with
   the C.4 hashed mirror path, psmisc fetches from `distfiles.gentoo.org` even
   when the SF upstream returns junk. (A `Content-Length` wildly below the
   manifest size could be added as a second guard, but the HTML check + manifest
   verify + discard already cover the observed cases.)

## D. `em select mirrors` — DONE

`eselect mirror` / mirrorselect workalike, in the `em select` family. Landed as
`select/mirrors.rs`:
- `em select mirrors list [--country <CC>|--region <R>]` — the official mirror
  list from Gentoo's structured XML API (`portage_distfiles::MirrorList`), marking
  those already in `GENTOO_MIRRORS`.
- `em select mirrors show` — the current `GENTOO_MIRRORS` from make.conf.
- `em select mirrors set <url>... [--country <CC>|--region <R>]` — write
  `GENTOO_MIRRORS` to the make.conf the root flags select.

(No `rank`/benchmark subcommand yet — mirrorselect's latency/throughput ranking is
the one unimplemented piece; lower priority now that mirror selection works.)

## Fix direction

1. **Make SRC_URI extraction capture computed values** (facet A) — the metadata
   phase must evaluate global-scope SRC_URI construction (loops, array joins,
   `+=`), not just literal assignments. Add a regression around bash-style
   `${arr[*]}` SRC_URI.
2. **Fetch must fail-fast on *any* required distfile** — already does when the
   file is in the list; the gap is the list being empty/incomplete (facet A).
3. **Harden the fetcher** (facet B) — mirror fallback order, retries, resume.

## Repro
- `em --root /var/tmp/stage1-arm64 --config-root / --oneshot app-shells/bash`
  → `SRC_URI is empty` → `eapply: patch failed: bash53-001`.
- `em --local --cross riscv64-unknown-linux-gnu sys-apps/less` → libpcre2
  `distfiles could not be fetched`.
