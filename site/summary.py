#!/usr/bin/env python3
"""Write docs/SUMMARY.md for mdBook from site/SUMMARY.in.md.

Bare `- path.md` lines get the page's first heading as their title (a bare `path.md` is a prefix chapter);
`<!-- cli -->` expands to the generated command reference under docs/user/cli, where `x.md` lists its subcommands in `x/`.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"


def title(page: pathlib.Path) -> str:
    for line in page.read_text(encoding="utf-8").splitlines():
        if line.startswith("# "):
            return line[2:].strip()
    sys.exit(f"{page}: no '# ' heading to use as its title")


def entry(page: pathlib.Path, depth: int) -> str:
    return f"{'  ' * depth}- [{title(page)}]({page.relative_to(DOCS).as_posix()})"


def cli_tree(page: pathlib.Path, depth: int) -> list[str]:
    lines = [entry(page, depth)]
    children = page.parent if page.name == "index.md" else page.with_suffix("")
    if children.is_dir():
        for child in sorted(children.glob("*.md")):
            if child != page:
                lines += cli_tree(child, depth + 1)
    return lines


def main() -> None:
    out = []
    for line in (ROOT / "site" / "SUMMARY.in.md").read_text(encoding="utf-8").splitlines():
        bare = re.fullmatch(r"(\s*)(- )?(\S+\.md)", line)
        if bare and bare.group(2):
            out.append(entry(DOCS / bare.group(3), len(bare.group(1)) // 2))
        elif bare:
            out.append(entry(DOCS / bare.group(3), 0)[2:])
        elif line.strip() == "<!-- cli -->":
            out += cli_tree(DOCS / "user" / "cli" / "index.md", 0)
        else:
            out.append(line)
    (DOCS / "SUMMARY.md").write_text("\n".join(out) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
