# Two `-p` divergences found 2026-06-18 (broad basket sweep)

STATUS: **B fixed; A narrowed to a report-coverage sub-gap.** B (the
`dev-lang/python` over-pull) was root-caused and fixed — see
`todo/target-derivation.md`. A is no longer a *plan* divergence: re-checked
2026-06-19, `em -p sys-apps/systemd` and `emerge -p` now list the **same 30
packages** (the earlier 30-vs-33 gap reflected a since-changed box state). What
remains is purely **blocker-advisory coverage** (Tier-2 reporting), below.

## A. `sys-apps/systemd` — blocker-report coverage (plan AT PARITY)

Re-verified 2026-06-19: package set matches emerge (30 == 30). The residual gap
is that em's blocker advisory surfaces **2 of emerge's 4** `blocks B` edges; the
prose below documents the original 30-vs-33 state for context.

**Context:** the test box is a **systemd-less** profile
(`default/linux/arm64/23.0`, OpenRC base — no `systemd` USE, no systemd
subprofile). `sys-apps/systemd-utils-255.18` is installed and provides
`virtual/tmpfiles`/`virtual/udev` (the systemd-less split). systemd is **not
masked** in the profile — asking for it directly is user error on this box, but
both tools resolve it; the divergence is in how they treat the resulting soft
blocks.

`em -p sys-apps/systemd` lists 30 vs emerge's 33. The 3 missing are all
**soft-blocker** edges emerge resolves by *replacing* the installed package
(`blocks B`), which em reports but does not act on (architecture "Known
divergences", Tier-2):

- `net-dns/openresolv` — systemd RDEPEND `resolvconf? ( !net-dns/openresolv )`.
  em keeps openresolv installed (soft-block) and reports `blocks B`; emerge
  **uninstalls** it to satisfy the blocker. (openresolv is installed here.)
- `sys-apps/gentoo-systemd-integration` — PDEPEND `!vanilla? ( … )`, soft-blocks
  the installed `sys-apps/systemd-utils-255.18`.
- `sys-apps/systemd-initctl` — PDEPEND `!sysv-utils? ( … )`.

emerge's full block report:
```
[blocks B] sys-apps/systemd ("sys-apps/systemd" is soft blocking sys-apps/systemd-utils-255.18)
[blocks B] sys-apps/gentoo-systemd-integration ("…" is soft blocking sys-apps/systemd-utils-255.18)
[blocks B] net-dns/openresolv ("net-dns/openresolv" is soft blocking sys-apps/systemd-260.2-r1)
[blocks B] sys-apps/systemd[resolvconf] ("…" is soft blocking net-dns/openresolv-3.17.4)
```

This is the documented "blockers: reported, not used to exclude/replace"
divergence, surfaced concretely on the destructive case. **No action expected**
beyond confirming em emits the matching `blocks B` report; closing it would
require promoting blockers to exclusion/replacement (a known Tier-2 → Tier-1
promotion item, see architecture §"Known divergences").

### Blocker advisory coverage — FIXED 4/4 (2026-06-19)
em's blocker advisory now matches emerge's four `blocks B` edges:
```
!!! Blocker conflict(s) detected:
  sys-apps/systemd:0-260.2-r1 blocks !net-dns/openresolv (weak(!))
  sys-apps/systemd-utils:0-255.18 blocks !sys-apps/gentoo-systemd-integration (weak(!))
  sys-apps/systemd-utils:0-255.18 blocks !sys-apps/systemd (weak(!))
  net-dns/openresolv:0-3.17.4 blocks !sys-apps/systemd[resolvconf] (weak(!))
```
The earlier framing ("em evaluates the USE condition but doesn't surface it") was
wrong: the blocker WAS evaluated; the blocked package was just installed-only
(nothing pulls it into the solve), so a solution-only search missed it. Fixed in
two directions, both keyed on packages the plan leaves in place:

- **forward** (`feat(blockers): report blockers against retained installed
  packages`) — `check_blockers` matches a solution package's blocker against
  retained installed packages, not just solution members → surfaces
  `systemd[resolvconf]` → `!openresolv`.
- **reciprocal** (`feat(blockers): report blockers declared by retained
  installed packages`) — the CLI extracts installed packages' active blocker
  atoms (`conflicts::installed_blocker_atoms`) and feeds them to the provider;
  `check_blockers` reports the ones a retained installed owner points at the plan
  → surfaces `openresolv` → `!systemd[resolvconf]`.
- **perf** (`perf(blockers): keep installed-blocker checks off the hot path`) —
  precompute the solution `(cpn, slot)` set for an O(1) retained check, hoist the
  extraction out of the solve fixpoint, and pre-scan to skip `evaluate_use` for
  packages with no blockers. Net `em -p` regression ~2-4% (was 6-8%).

This is the documented Tier-2 "blockers reported, not enforced" stance, now with
full *report* parity. The remaining (separate, deliberate) item is promoting
blockers to exclusion/replacement (Tier-2 → Tier-1) — i.e. actually uninstalling
openresolv as emerge does — which is the architecture-level decision, not a
report gap.

## B. `dev-lang/python` — over-pull → **diagnosed, moved**

Root-caused 2026-06-19 to the CLI target-derivation path (two bugs: the sibling
slot listed `[R]`, and slot/version qualifiers ignored entirely). Full diagnosis,
emerge reference table, and fix plan now live in
**`todo/target-derivation.md`**; intended behaviour is documented in
`docs/architecture.md` §"Target derivation: argv → request".
