# Project websites on luminem.org: em.luminem.org first, same pattern for winnow-args

Status: 🔴 planned 2026-11-18, nothing built. Hosting: GitHub Pages, one site per repo, each on its own `luminem.org` subdomain.

## Shape

| Site | Repo | Source |
|------|------|--------|
| `em.luminem.org` | `lu-zero/portage-cli` | `docs/user` (book), `docs/user/cli` (generated CLI reference), `docs/design` (internals) |
| `winnow-args.luminem.org` | `lu-zero/winnow-args` | `README.md`, `docs/*.md` (design, comparison, perf + SVGs), release notes |

Both repos are public with no Pages site yet (checked 2026-11-18), so each repo gets its own Pages site and custom domain.
`luminem.org` itself resolves to an OVH IPv6 host and stays as it is; only the two subdomains point at GitHub.

## Building: mdBook

- The docs are already Markdown, and the CLI reference is generated Markdown (`just docs`), so mdBook needs no conversion step.
- One `book.toml` plus `SUMMARY.md` per repo, in a `site/` directory that includes the existing `docs/` files; nothing moves.
- `mdbook-mermaid` for the two mermaid diagrams in `docs/design`, keeping the winnow-args help palette (AGENTS.md).
- API docs stay on docs.rs; the site links to them instead of hosting rustdoc.
- `just site` builds to `target/site`; `just site-serve` previews locally.

## Deploying: GitHub Actions, no `gh-pages` branch

- Workflow `pages.yml`: on push to `master` (and manual dispatch), run `just docs-check` and `just docs-links` (portage-cli), build with
  mdBook, upload with `actions/upload-pages-artifact`, publish with `actions/deploy-pages`. Repo Settings → Pages → Source: GitHub Actions.
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

1. portage-cli: `site/` with `book.toml` + `SUMMARY.md` over the existing docs, `just site`, local preview. Check the CLI reference links.
2. portage-cli: `pages.yml`; first deploy to the default `lu-zero.github.io/portage-cli` URL.
3. DNS records and domain verification (Luca), then the custom domain and HTTPS.
4. winnow-args: same, copying the files; then pull the shared parts into the template repo.

## Open questions

- Landing page: `docs/user/intro.md` as is, or a short new front page with install and a quick tour?
- Include `docs/design` on the public site, or keep the internals in the repo only?
- Should a top-level `luminem.org` page list the projects (served by the existing host, not GitHub)?
