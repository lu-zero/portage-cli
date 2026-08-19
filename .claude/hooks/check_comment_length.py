#!/usr/bin/env python3
"""PreToolUse hook: block Edit/Write calls that introduce oversized comments.

Regular paragraphs cap at 3 lines, doc comments at 6, except Rust module
docs (unlimited) and trait docs (relaxed). A blank comment line starts a new
paragraph. Skipped for unknown extensions.
"""
import json
import os
import re
import sys

REGULAR_MAX = 3
DOC_MAX = 6
TRAIT_DOC_MAX = 18

TRAIT_DECL_RE = re.compile(r"^(pub(\(.*?\))?\s+)?(unsafe\s+)?(auto\s+)?trait\b")
REFERENCE_RE = re.compile(r"(docs?/[\w./-]+\.md|\[[^\]]+\]\([^)]+\.md\))", re.IGNORECASE)

RUST_EXTS = {".rs"}
C_EXTS = {".js", ".ts", ".tsx", ".jsx", ".go", ".c", ".h", ".cpp", ".cc", ".hpp", ".java"}
HASH_EXTS = {".toml", ".yaml", ".yml", ".rb"}
PY_EXTS = {".py"}


def split_paragraphs(contents):
    """Split comment content into paragraphs on blank lines.

    ``` fenced regions are skipped entirely: not counted, don't break a
    paragraph. Returns (start_idx, end_idx_exclusive, prose_len) tuples.
    """
    paragraphs = []
    in_fence = False
    start = None
    length = 0
    last = None
    for idx, c in enumerate(contents):
        s = c.strip()
        if s.startswith("```"):
            in_fence = not in_fence
            if start is None:
                start = idx
            last = idx
            continue
        if in_fence:
            last = idx
            continue
        if s == "":
            if start is not None and length > 0:
                paragraphs.append((start, last + 1, length))
            start, length, last = None, 0, None
            continue
        if start is None:
            start = idx
        length += 1
        last = idx
    if start is not None and length > 0:
        paragraphs.append((start, last + 1, length))
    return paragraphs


def reference_leeway(raw_lines):
    """+3 lines when a paragraph points to a docs/*.md reference instead of inlining."""
    return 3 if any(REFERENCE_RE.search(l) for l in raw_lines) else 0


def check_line_runs(lines, marker, exclude, max_len):
    violations = []
    i, n = 0, len(lines)

    def is_comment(l):
        s = l.strip()
        return s.startswith(marker) and not (exclude and s.startswith(exclude))

    while i < n:
        if not is_comment(lines[i]):
            i += 1
            continue
        start = i
        while i < n and is_comment(lines[i]):
            i += 1
        raw = lines[start:i]
        contents = [l.strip()[len(marker):] for l in raw]
        for pstart, pend, plen in split_paragraphs(contents):
            seg = raw[pstart:pend]
            limit = max_len + reference_leeway(seg)
            if plen > limit:
                violations.append((start + pstart + 1, plen, "comment", limit))
    return violations


def block_content_lines(raw_lines, opener_len):
    """Per-line text of a block comment, stripped of `/**`, `*/`, and `* ` bullets."""
    out = []
    for idx, line in enumerate(raw_lines):
        s = line.strip()
        if idx == 0:
            s = s[opener_len:]
        if idx == len(raw_lines) - 1:
            pos = s.rfind("*/")
            if pos != -1:
                s = s[:pos]
        s = s.strip()
        if s.startswith("*") and not s.startswith("*/"):
            s = s[1:].strip()
        out.append(s)
    return out


def check_block_runs(lines, opener, doc_opener, closer, max_regular, max_doc):
    violations = []
    i, n = 0, len(lines)
    while i < n:
        s = lines[i].strip()
        if not (s.startswith(doc_opener) or (s.startswith(opener) and not s.startswith(doc_opener))):
            i += 1
            continue
        doc = s.startswith(doc_opener)
        opener_len = len(doc_opener) if doc else len(opener)
        start = i
        j = i
        if closer not in lines[j][opener_len:]:
            j += 1
            while j < n and closer not in lines[j]:
                j += 1
        end = min(j, n - 1)
        block_lines = lines[start : end + 1]
        contents = block_content_lines(block_lines, opener_len)
        max_len = max_doc if doc else max_regular
        kind = "doc block comment" if doc else "block comment"
        for pstart, pend, plen in split_paragraphs(contents):
            seg = block_lines[pstart:pend]
            limit = max_len + reference_leeway(seg)
            if plen > limit:
                violations.append((start + pstart + 1, plen, kind, limit))
        i = end + 1
    return violations


def check_python_docstrings(lines, max_doc):
    violations = []
    i, n = 0, len(lines)
    while i < n:
        s = lines[i].strip()
        matched = False
        for q in ('"""', "'''"):
            if not s.startswith(q):
                continue
            matched = True
            if q in s[3:]:
                i += 1
                break
            start = i
            j = i + 1
            while j < n and q not in lines[j]:
                j += 1
            end = min(j, n - 1)
            block_lines = lines[start : end + 1]
            contents = []
            for idx, raw in enumerate(block_lines):
                c = raw.strip()
                if idx == 0:
                    c = c[3:]
                if idx == len(block_lines) - 1:
                    pos = c.rfind(q)
                    if pos != -1:
                        c = c[:pos]
                contents.append(c)
            for pstart, pend, plen in split_paragraphs(contents):
                seg = block_lines[pstart:pend]
                limit = max_doc + reference_leeway(seg)
                if plen > limit:
                    violations.append((start + pstart + 1, plen, "docstring", limit))
            i = end + 1
            break
        if not matched:
            i += 1
    return violations


def next_real_line(lines, idx):
    n = len(lines)
    j = idx
    while j < n:
        s = lines[j].strip()
        if s == "" or s.startswith("#[") or s.startswith("#!["):
            j += 1
            continue
        return s
    return ""


def is_trait_doc(lines, end_idx):
    return bool(TRAIT_DECL_RE.match(next_real_line(lines, end_idx)))


def check_rust_line_comments(lines):
    violations = []
    i, n = 0, len(lines)
    while i < n:
        s = lines[i].strip()
        if not s.startswith("//"):
            i += 1
            continue
        if s.startswith("//!"):
            while i < n and lines[i].strip().startswith("//!"):
                i += 1
            continue
        if s.startswith("///"):
            start = i
            while i < n and lines[i].strip().startswith("///"):
                i += 1
            raw = lines[start:i]
            contents = [l.strip()[3:] for l in raw]
            trait = is_trait_doc(lines, i)
            max_len = TRAIT_DOC_MAX if trait else DOC_MAX
            kind = "trait doc comment" if trait else "doc comment"
            for pstart, pend, plen in split_paragraphs(contents):
                seg = raw[pstart:pend]
                limit = max_len + reference_leeway(seg)
                if plen > limit:
                    violations.append((start + pstart + 1, plen, kind, limit))
            continue
        start = i
        while i < n and lines[i].strip().startswith("//") and not lines[i].strip().startswith(
            "///"
        ) and not lines[i].strip().startswith("//!"):
            i += 1
        raw = lines[start:i]
        contents = [l.strip()[2:] for l in raw]
        for pstart, pend, plen in split_paragraphs(contents):
            seg = raw[pstart:pend]
            limit = REGULAR_MAX + reference_leeway(seg)
            if plen > limit:
                violations.append((start + pstart + 1, plen, "comment", limit))
    return violations


def check_rust_block_comments(lines):
    violations = []
    i, n = 0, len(lines)
    while i < n:
        s = lines[i].strip()
        if not s.startswith("/*"):
            i += 1
            continue
        module = s.startswith("/*!")
        doc = s.startswith("/**") and not module
        opener_len = 3 if (module or doc) else 2
        start = i
        j = i
        if "*/" not in lines[j][opener_len:]:
            j += 1
            while j < n and "*/" not in lines[j]:
                j += 1
        end = min(j, n - 1)
        i = end + 1
        if module:
            continue
        block_lines = lines[start : end + 1]
        contents = block_content_lines(block_lines, opener_len)
        trait = is_trait_doc(lines, i) if doc else False
        if doc:
            max_len = TRAIT_DOC_MAX if trait else DOC_MAX
            kind = "trait doc block comment" if trait else "doc block comment"
        else:
            max_len = REGULAR_MAX
            kind = "block comment"
        for pstart, pend, plen in split_paragraphs(contents):
            seg = block_lines[pstart:pend]
            limit = max_len + reference_leeway(seg)
            if plen > limit:
                violations.append((start + pstart + 1, plen, kind, limit))
    return violations


def check_rust(lines):
    return check_rust_line_comments(lines) + check_rust_block_comments(lines)


def check_c(lines):
    v = []
    v += check_line_runs(lines, marker="//", exclude=None, max_len=REGULAR_MAX)
    v += check_block_runs(lines, "/*", "/**", "*/", REGULAR_MAX, DOC_MAX)
    return v


def check_hash(lines):
    return check_line_runs(lines, marker="#", exclude="#!", max_len=REGULAR_MAX)


def check_python(lines):
    v = check_hash(lines)
    v += check_python_docstrings(lines, DOC_MAX)
    return v


def dispatch(ext, text):
    lines = text.split("\n")
    if ext in RUST_EXTS:
        return check_rust(lines)
    if ext in C_EXTS:
        return check_c(lines)
    if ext in PY_EXTS:
        return check_python(lines)
    if ext in HASH_EXTS:
        return check_hash(lines)
    return []


def main() -> None:
    payload = json.load(sys.stdin)
    tool_name = payload.get("tool_name")
    if tool_name not in ("Edit", "Write"):
        return
    tool_input = payload.get("tool_input", {})
    file_path = tool_input.get("file_path", "")
    text = tool_input.get("content") if tool_name == "Write" else tool_input.get("new_string")
    if not text:
        return

    ext = os.path.splitext(file_path)[1]
    violations = dispatch(ext, text)
    if not violations:
        return

    lines_desc = "; ".join(
        f"line {ln} ({kind}, {length} lines, max {limit})" for ln, length, kind, limit in violations
    )
    print(
        json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": (
                        f"Oversized comment block(s) in {file_path or 'new content'}: {lines_desc}. "
                        "Trim to the paragraph/doc-comment line limits."
                    ),
                }
            }
        )
    )


if __name__ == "__main__":
    main()
