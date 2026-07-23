# `--deep` / emptytree: bump `:*` any-slot deps to the newest slot

STATUS: DONE 2026-06-18 (commits 56e19a5 + b968a13). `em -pe www-client/firefox`
now matches `emerge -pe` exactly — 383 packages, identical set and N/R/U/NS tags
(5 U / 252 R / 123 N / 3 NS), ~11.6x faster (em ~0.62s vs emerge ~7.2s).

Implemented in two parts:
1. **Version-ranked slots** (`rank_slots_by_version`): `slots_for` orders slots
   by best version, not slot name — fixes the lex-last assumption that would
   otherwise pull bash's legacy `:5.1` compat slot under the bump. Mirrors
   portage's version-descending `:*` selection (`_iter_match_pkgs_atom` reverses
   `cp_list` → highest version first; the slot falls out). No ad-hoc filter in
   portage. Behaviour-neutral in default mode.
2. **`prefer_newest_slot` bump**: `choose_version` bypasses the installed-branch
   preference for `SlotChoice` under `--deep`/emptytree → version-ranked max().

**In-slot deep traversal (`-uD`)** — closed separately 2026-07-18 as
[[deep-in-slot-upgrades]] (`prefer_update` + host-satisfied BDEPEND retention).
This file remains the record for the **`prefer_newest_slot` / `:*` slot bump**
only. The older note that “em -uD stays shallow” is obsolete.

---


## emerge behaviour (sandbox aarch64, clean stage3 + rust-bin-1.93.1 installed, 2026-06-18)

`www-client/firefox`, observing `dev-lang/rust-bin` (slotted by version;
consumers use the rust-eclass `|| ( >=dev-lang/rust-bin-MIN:* >=dev-lang/rust-MIN:* )`,
max MIN in the closure = `>=1.88.0` from cargo-c, satisfied by installed 1.93.1):

| emerge invocation        | total ebuilds | rust-bin pulled            |
|--------------------------|---------------|----------------------------|
| `-p`   (plain)           | 125           | none (1.93.1 satisfies)    |
| `-up`  (update, shallow) | 125           | **none**                   |
| `-uDp` (update + deep)   | 131           | **`1.94.1` NS** [1.93.1]   |
| `-pe`  (emptytree)       | 383           | `1.93.1` R + `1.94.1` NS   |

**Takeaway:** the newest-slot bump for a `:*` any-slot dep is driven by
**`--deep`** (and by `--emptytree`, which implies deep) — **not** by `--update`
alone. `-u`/`-up` leaves the satisfied installed slot in place. `--deep`
re-examines transitive `:*` deps and pulls the newest slot even when an older
installed slot already satisfies the `>=MIN`. emptytree additionally reinstalls
the installed slot (R) alongside the new one (NS).

Direct-atom sanity (no `||` wrapper): `emerge -p ">=dev-lang/rust-bin-1.74.1:*"`
already picks newest `1.94.1` (no `--deep` needed) — the OR-group/`SlotChoice`
wrapper is what makes the installed-slot preference kick in for the firefox case.

## em after this work (historical “em today” section)

The planned wiring landed: `prefer_newest_slot` on under `--deep` and native
`--emptytree`; `SlotChoice` only (not all OR-groups). `Choice` USE-dep branch
selection unchanged.

- Emptytree slot parity for firefox was the original acceptance bar (383-set).
- Non-emptytree **in-slot** upgrades under `-uD` are documented in
  [[deep-in-slot-upgrades]] — not this file’s slot-only scope.
- `--newuse` remains open ([[newuse]]); do not conflate with `--deep`.
