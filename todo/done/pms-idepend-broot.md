# IDEPEND satisfaction root (PMS table 8.2)

Status: ✅ IDEPEND → BROOT (PMS table 8.2). Related: [[pms-compliance]],
[`docs/design/root-topology.md`](../docs/design/root-topology.md).

## PMS

Table 8.2 groups **BDEPEND and IDEPEND**: CBUILD, offset-prefix `${BROOT}`,
query `-b`. RDEPEND/PDEPEND use ROOT + `${EPREFIX}`.

## What `em` does

```
BDEPEND  → broot
IDEPEND  → broot when cross-arch, else merge_root()  (same as RDEPEND)
```

(`portage-resolve/src/roots.rs` `satisfaction_root`). Harmless when
BROOT ≡ ROOT. Wrong for `ROOT=/somewhere` native and for prefix, where
install-time tools live on the build host.

The comment cites table 8.2 and then describes the Portage-shaped split.

## How to attack

1. IDEPEND → `broot` unconditionally, same as BDEPEND.
2. Unit-test a `ROOT != /` native `Roots` value: IDEPEND and BDEPEND share
   a root, RDEPEND does not.
3. Live canary: prefix or `ROOT=/tmp/root` plan that has an IDEPEND.
