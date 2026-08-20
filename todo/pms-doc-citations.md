# Wrong PMS section numbers in comments / docs

Status: 🟡 partial (`architecture.md` 5.2.5 + `[flag]` tier). Related:
[[pms-compliance]]. Opportunistic; fix when touching the file.

PMS 9 numbering (not Portage, not older PMS):

| Current citation | Actual |
|------------------|--------|
| `architecture.md` "PMS 5.2.4" for directory-form profile files | **5.2.5** (5.2.4 is `make.defaults`) |
| `use_env.rs` "PMS 5.2.4" for `/etc/portage/package.use` dirs | user config (PMS 1.1); 5.2.5 is *profile* dirs |
| `in_iuse` "PMS 12.3.5" | **12.3.12** |
| `use` "PMS 12.3.1" | **12.3.12** (`use` / `usev` / `usex`) |
| `eapply` "PMS 11.3.3" | **12.3.7** |
| `has_version` "PMS 12.3.13 / 12.3.4" | **12.3.4** |
| `ForceMask` rustdoc leads with Portage `getUseMask` | PMS **5.2.12 Algorithm 5.1** |
| `architecture.md` cross-package `[flag]` still "Tier 2 unless `--autosolve-use`" | depgraph always co-solves; exit 1 |

`parse_package` comments: PMS 3.1.2 does **not** require an alphanumeric
first character (that is 3.1.4 for USE flags). `_cron-failure` is legal.
