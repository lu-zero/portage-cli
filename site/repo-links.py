#!/usr/bin/env python3
"""mdBook preprocessor: point links that leave docs/ at the file on GitHub, since the site only holds docs/."""

import json
import posixpath
import re
import sys

BLOB = "https://github.com/lu-zero/portage-cli/blob/master/"
LINK = re.compile(r"(\]\()([^)\s]+)(\))")


def rewrite(content: str, source_path: str) -> str:
    here = posixpath.dirname(source_path)

    def fix(m: re.Match) -> str:
        target = m.group(2)
        if re.match(r"^[a-z][a-z0-9+.-]*:|^#|^/", target):
            return m.group(0)
        path, _, anchor = target.partition("#")
        resolved = posixpath.normpath(posixpath.join("docs", here, path))
        if resolved.startswith("docs/"):
            return m.group(0)
        url = BLOB + resolved + (f"#{anchor}" if anchor else "")
        return f"{m.group(1)}{url}{m.group(3)}"

    return LINK.sub(fix, content)


def walk(items: list) -> None:
    for item in items:
        chapter = item.get("Chapter") if isinstance(item, dict) else None
        if not chapter:
            continue
        if chapter.get("source_path"):
            chapter["content"] = rewrite(chapter["content"], chapter["source_path"])
        walk(chapter.get("sub_items", []))


def main() -> None:
    if len(sys.argv) > 1 and sys.argv[1] == "supports":
        sys.exit(0)
    _context, book = json.load(sys.stdin)
    walk(book.get("items", book.get("sections", [])))
    json.dump(book, sys.stdout)


if __name__ == "__main__":
    main()
