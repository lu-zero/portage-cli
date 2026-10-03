# rustc anchor: 1.99.0 vs 1.100.0-beta.3, 2026-10-03

Stable `1.100.0` is not published. The 1.100 that is available today is
`1.100.0-beta.3` (beta channel, manifest date 2026-10-03). The before side is
the previous stable, rustup `1.99.0`. `/usr/bin/rustc` on this host is still
Gentoo `1.98.1` and was not used for either binary.

- **before:** rustc 1.99.0 (`b940084d7`, 2026-09-28), rustup stable
- **after:** rustc 1.100.0-beta.3 (`7bbda45cb`, 2026-10-01), rustup beta
- **tree:** `483d875a` plus the uncommitted depgraph split. Both binaries were
  built from that same tree, so the ratio is the compiler.
- **machine:** thalia, `numactl -N 0 -m 0` —
  [`machines/thalia.md`](../../machines/thalia.md)
- **workload:** `em -p www-client/firefox`, host repo `/var/db/repos/gentoo`
- **profile:** `--release` (`lto = true`). Each build took about 7 minutes.

## Same plan

Captured stdout and stderr are byte-identical (75 ebuild rows, exit 1 from
the usual USE-change and constraint report). The timed work matches.

## Result: no measurable difference

| method | 1.99.0 | 1.100.0-beta.3 | delta |
|---|---|---|---|
| interleaved, one invocation | 1.023 s ± 0.042 | 1.017 s ± 0.027 | −0.5% |
| each alone, own invocation | 1.028 s ± 0.034 | 1.043 s ± 0.016 | +1.5% |
| minimum of 10 runs (interleaved) | 0.980 s | 0.970 s | −1.0% |
| minimum of 10 runs (alone) | 0.979 s | 1.012 s | +3.4% |

The two methods disagree in sign. Both deltas sit inside the run-to-run
spread (~0.02–0.04 s on a 1.02 s command). `em -p firefox` does not move
between these two compilers.

Absolute times are higher than the 2026-09-26 firefox figures (~0.84–1.01 s
on different commits and a different rustc). Use this directory as the
same-session pair. Do not subtract it from an older session's absolute number.

## Files

- `meta.env` — toolchains, shas, load, plan statement
- `plan-1.99.txt`, `plan-1.100.txt` — identical plans
- `hyperfine-interleaved.json` / `.md` — both binaries, one invocation
- `hyperfine-1.99-alone.json` / `.md`, `hyperfine-1.100-alone.json` / `.md`
