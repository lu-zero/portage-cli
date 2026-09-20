#!/usr/bin/env python3
"""Fail on documentation that points at something that is not there.

Checks, over git-tracked files (todo/ and dated benchmark results excluded):
  - relative links in Markdown and in Rust doc comments resolve to a file;
  - `docs/<name>.md` mentions resolve to a file;
  - no Rust doc comment cites a `todo/` path (planning scratch, see AGENTS.md).
"""

import os
import re
import subprocess
import sys

SKIP = ("todo/", "target/", "benchmarks/results/")


def tracked(*patterns):
    out = subprocess.run(
        ["git", "ls-files", *patterns], capture_output=True, text=True, check=True
    ).stdout
    return [f for f in out.split() if not f.startswith(SKIP)]


def exists_from(path, target):
    return os.path.exists(os.path.normpath(os.path.join(os.path.dirname(path), target)))


def is_external(target):
    return re.match(r"(https?:|mailto:|#)", target) or set(target) <= {"."}


def check_markdown_links(problems):
    for f in tracked("*.md"):
        text = re.sub(r"```.*?```", "", open(f).read(), flags=re.S)
        for m in re.finditer(r"\]\(([^)\s]+)\)", text):
            target = m.group(1)
            path = target.split("#")[0]
            if not is_external(target) and path and not exists_from(f, path):
                problems.append(f"{f}: dead link ({target})")


def check_rust_links_and_todo(problems):
    for f in tracked("*.rs"):
        for n, line in enumerate(open(f, errors="ignore"), 1):
            if re.match(r"\s*//", line):
                for m in re.finditer(r"\]\(((?:\.\./)+[^)#\s]+)", line):
                    if not exists_from(f, m.group(1)):
                        problems.append(f"{f}:{n}: dead link ({m.group(1)})")
            if re.match(r"\s*//[/!]", line) and "todo/" in line:
                problems.append(f"{f}:{n}: doc comment cites todo/")


def check_doc_mentions(problems):
    crates = [d for d in os.listdir(".") if os.path.isdir(d)]
    rx = re.compile(r"(?<![\w.-])((?:\.\./)*(?:[a-z][a-z0-9_-]*/)*docs/[A-Za-z0-9_./-]+?\.md)")
    for f in tracked("*.md", "*.rs", "*.sh", "*.toml"):
        if f.startswith("benchmarks/"):
            continue
        for n, line in enumerate(open(f, errors="ignore"), 1):
            for m in rx.finditer(line):
                p = m.group(1)
                candidates = [os.path.join(os.path.dirname(f), p), p]
                candidates += [os.path.join(c, p) for c in crates]
                if not any(os.path.isfile(os.path.normpath(c)) for c in candidates):
                    problems.append(f"{f}:{n}: no such doc ({p})")


def main():
    problems = []
    check_markdown_links(problems)
    check_rust_links_and_todo(problems)
    check_doc_mentions(problems)
    for p in problems:
        print(p)
    print(f"{len(problems)} documentation problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
