# em wall-clock: search + -p (pinned tree)

- **date (UTC):** 2026-07-25T22:01:25+00:00
- **host:** thalia (aarch64, 128 cores)
- **git (em):** 56a16bc (**dirty** uncommitted metadata-cache/builder/active)
- **gentoo tree:** `benchmarks/gentoo` @ **0833c9fed** (thalia pin)
- **ebuilds / md5-cache:** 32481 / 32480 (regenerated this session)
- **invocation:** `em --repo benchmarks/gentoo …`

## Results

### search

| command | mean | min | max |
|---------|------|-----|-----|
| `em search gcc` | **0.092s** | 0.066s | 0.117s |
| `em search python` | **0.094s** | 0.078s | 0.144s |
| `em search firefox` | **0.096s** | 0.077s | 0.119s |
| `em search rust` | **0.092s** | 0.070s | 0.116s |
| `em -s gcc` | **0.092s** | 0.080s | 0.109s |

### em -p

| command | mean | min | max |
|---------|------|-----|-----|
| `em -p firefox` | **1.382s** | 1.174s | 1.500s |
| `em -p gcc` | **0.931s** | 0.833s | 1.043s |
| `em -p openssh` | **0.957s** | 0.884s | 1.068s |
| `em -p python` | **0.928s** | 0.888s | 0.972s |
| `em -p qtbase` | **1.186s** | 1.130s | 1.270s |
| `em -p libreoffice` | **1.624s** | 1.516s | 1.903s |

## vs historical (thalia 2026-07-11)

| metric | historical | this run (pinned 0833c9f) | delta |
|--------|------------|---------------------------|-------|
| em -p firefox | 0.76–0.9s | **1.382s** | +66% **REGRESSION** |
| em -p libreoffice | ~0.94s | **1.624s** | +73% **REGRESSION** |
| em search gcc | ~0.1s | **0.092s** | -8% |

### Notes

- Prior host-tree run (`…-56a16bc-dirty/`) used `/var/db/repos/gentoo` @ `13726a35` — **not** comparable to history.
- This run matches the thalia pin: `benchmarks/gentoo` @ `0833c9f` with a full regen of md5-cache.
- firefox/libreoffice still exit 1 for USE changes; timed with hyperfine `--ignore-failure`.
- **`-p` still ~1.6–1.8× slower than 2026-07-11 baseline** even on the pinned tree — regression investigation still open.
