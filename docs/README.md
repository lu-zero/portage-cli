# em

`em` is a Rust reimplementation of Gentoo's Portage package manager: one binary that does what `emerge`, `equery`, `euse`
and their companions do, keeping their behaviour and conventions.

- [Introduction](user/intro.md): what `em` is, and how it relates to the tools it replaces.
- [Command reference](user/cli/index.md): every command and option, generated from `em`'s own command line.
- [Architecture](design/architecture.md): how `em` resolves, plans and merges.

The source is on [GitHub](https://github.com/lu-zero/portage-cli); the library crates' API docs are on [docs.rs](https://docs.rs).
