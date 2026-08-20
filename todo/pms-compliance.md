# PMS 9 compliance (audit 2026-08-20)

Status: 🟡 queue opened. Full pass against
[PMS 9](https://projects.gentoo.org/pms/9/pms.html) (commit `2cee9c6`).
User config is out of scope (PMS 1.1): `make.conf`, `USE_ORDER`,
`ACCEPT_KEYWORDS`, `/etc/portage/*`, `FEATURES` are not scored as defects.

Recently matched: **5.2.10** (`b0e4b11`) profile `package.use` over
`make.defaults`; **5.2.12** Algorithm 5.1 (`d4aec5f`) per-node force/mask.

## Tackle order (live-behaviour first)

| Pri | Item | PMS | Detail |
|-----|------|-----|--------|
| 1 | `IUSE_EFFECTIVE` | 11.1.1 / 12.3.12 / table 12.20 | [[pms-iuse-effective]] 🟡 |
| 2 | Empty `\|\|` / `^^` after USE strip | table 8.6 | [[pms-empty-dep-groups]] ✅ |
| 3 | `fetch+` / `mirror+` on merge fetch | 7.3.2 | [[pms-fetch-plus]] ✅ |
| 4 | Strong blockers still install | 8.3.2 | [[blocker-enforcement]] (PMS note added) |
| 5 | `REQUIRED_USE` does not mask | 7.3.4 | [[pms-required-use-mask]] |
| 6 | IDEPEND native root | table 8.2 | [[pms-idepend-broot]] ✅ |
| 7 | `D`-symlink rewrite EAPI 0–8 | 13.4.1 | [[pms-symlink-rewrite]] ✅ |
| 8 | `CONFIG_PROTECT` longest-prefix | 13.3.3 | [[pms-config-protect]] |

## Letter-of-PMS, empty on current gentoo

| Item | PMS | Detail |
|------|-----|--------|
| `use.stable` / `package.use.stable` | 5.2.11 | [[use-stable-in-defaults]] |
| Duplicate parents sourced once | 5.2.1 | [[pms-profile-stack]] |
| `ENV_UNSET` not stacked / applied | 5.3.1 | [[pms-env-unset]] |
| EAPI 9 missing-`eapi` default | table 5.1 | [[pms-profile-stack]] |
| `package.provided` on EAPI 7+ | table 5.3 | [[pms-profile-stack]] |
| `RDEPEND=DEPEND` EAPI ≤ 3 | 7.3.7 | [[pms-rdepend-fallback]] |

## Parser / docs (opportunistic)

- Too-lenient names, no EAPI gates: [[pms-parser-lenient]]
- `einstalldocs` vs algorithm 12.3: [[pms-einstalldocs]]
- Wrong section numbers in comments: [[pms-doc-citations]]
- Portage `repo` USE_ORDER layer (PMS-silent): [[use-order-repo-layer]]
