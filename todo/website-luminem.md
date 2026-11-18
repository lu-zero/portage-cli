# Project websites on luminem.org: em.luminem.org first, same pattern for winnow-args

Status: 🟡 em site prepared 2026-11-18 (`site/`, `just site`, `.github/workflows/pages.yml`), not deployed: needs Pages enabled with
source "GitHub Actions" and the DNS records below. Hosting: GitHub Pages, one site per repo, each on its own `luminem.org` subdomain.

## Shape

| Site | Repo | Source |
|------|------|--------|
| `em.luminem.org` | `lu-zero/portage-cli` | `docs/user` (book), `docs/user/cli` (generated CLI reference), `docs/design` (internals) |
| `winnow-args.luminem.org` | `lu-zero/winnow-args` | `README.md`, `docs/*.md` (design, comparison, perf + SVGs), release notes |

Both repos are public with no Pages site yet (checked 2026-11-18), so each repo gets its own Pages site and custom domain.
`luminem.org` itself can move to GitHub Pages too, built with Zola from its own repo (a landing page listing the projects).
An apex domain on Pages takes `A`/`AAAA` records to GitHub's Pages addresses instead of a `CNAME`, and replaces the current OVH host.

## Building: mdBook (done for em)

- `site/book.toml` builds from `docs/` itself (`src = "../docs"`) into `target/site`; nothing moved.
- `site/SUMMARY.in.md` is the hand-written outline; `site/summary.py` writes the gitignored `docs/SUMMARY.md`, titling each page from
  its first heading and expanding the generated command reference (`x.md` + `x/`), so `just docs` never leaves it stale.
- `site/repo-links.py`, an mdBook preprocessor, points links that leave `docs/` (AGENTS.md, sources, other crates) at GitHub.
- `docs/README.md` is the front page (mdBook renders it as `index.html`; the first chapter there broke its relative links).
- Mermaid: `mdbook-mermaid` turns the blocks into `pre.mermaid`, and `site/mermaid-init.js` loads mermaid 11.4.1 from jsDelivr
  instead of vendoring `mermaid.min.js`. Each diagram keeps its own `config:` palette header.
- `just site` builds, `just site-serve` previews. 0 broken internal links in the built site (2026-11-18).

## Deploying: GitHub Actions, no `gh-pages` branch

- Workflow `pages.yml`: on a push to `master` touching `docs/` or `site/` (and manual dispatch), run `./check-doc-links.py`, build with
  mdBook (CI's `cli-reference` job already guards the generated pages), upload with `actions/upload-pages-artifact`, publish with `actions/deploy-pages`. Repo Settings → Pages → Source: GitHub Actions.
- Custom domain set in Settings → Pages (with Actions deploys no `CNAME` file is needed), then "Enforce HTTPS" once the certificate is issued.

## DNS and domain safety (needs Luca, at the luminem.org DNS provider)

1. `em.luminem.org.  CNAME  lu-zero.github.io.` and `winnow-args.luminem.org.  CNAME  lu-zero.github.io.`
2. Verify `luminem.org` under GitHub account Settings → Pages → Verified domains (a `_github-pages-challenge-lu-zero` TXT record).
   This stops any other GitHub account from claiming a `*.luminem.org` subdomain that points at github.io.

## The shared pattern

- A small template repo (or a directory copied by hand at first): `theme/` CSS with the shared palette, the `pages.yml` workflow as a
  reusable `workflow_call` workflow, and a `book.toml` skeleton. Each project keeps only `site/book.toml`, `site/SUMMARY.md`, and a
  ten-line `pages.yml` calling the shared workflow with its project name.
- Start by copying into portage-cli; extract the template when winnow-args is the second user, not before.

## Order of work

1. portage-cli: `site/`, `just site`, local preview (done).
2. portage-cli: `pages.yml` (written); enable Pages with source "GitHub Actions", first deploy.
3. DNS records and domain verification (Luca), then the custom domain and HTTPS.
4. winnow-args: same, copying the files; then pull the shared parts into the template repo.
5. luminem.org: Zola landing site in its own repo, apex records to GitHub Pages.

## Open questions

- Front page: `docs/README.md` is a short placeholder; grow it into install steps and a quick tour?
- Include `docs/design` on the public site, or keep the internals in the repo only?
