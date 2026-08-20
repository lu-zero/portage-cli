# `einstalldocs` vs algorithm 12.3

Status: 🔴 not started. Related: [[pms-compliance]].

## PMS

Algorithm 12.3: default file set is `README*` `ChangeLog` `AUTHORS` `NEWS`
`TODO` `CHANGES` `THANKS` `BUGS` `FAQ` `CREDITS` `CHANGELOG`. `HTML_DOCS`
uses `docinto /usr/share/doc/${PF}/html` and restores dest.

## What `em` does

Default globs: `README*` `CHANGES*` `ChangeLog*` `CHANGELOG*` `AUTHORS*`
`NEWS*` `TODO*` `THANKS*` — extra `*` on several, missing `BUGS` / `FAQ` /
`CREDITS`. No dest save/restore for `HTML_DOCS`.

## How to attack

Rewrite the default list and the `HTML_DOCS` dest handling to match 12.3.
Synthetic ebuild test, not a live canary.
