#!/usr/bin/env python3
"""PreToolUse hook: block Edit/Write calls that introduce oversized comments.

Regular paragraphs cap at 3 lines, doc comments at 6, except Rust module
docs (unlimited) and trait docs (relaxed). Skipped for unknown extensions.
"""
import json
import os
import re
import sys

REGULAR_MAX = 3
DOC_MAX = 6
TRAIT_DOC_MAX = 18

TRAIT_DECL_RE = re.compile(r"^(pub(\(.*?\))?\s+)?(unsafe\s+)?(auto\s+)?trait\b")

RUST_EXTS = {".rs"}
C_EXTS = {".js", ".ts", ".tsx", ".jsx", ".go", ".c", ".h", ".cpp", ".cc", ".hpp", ".java"}
HASH_EXTS = {".sh", ".bash", ".zsh", ".toml", ".yaml", ".yml", ".rb"}
PY_EXTS = {".py"}


def check_line_runs(lines, is_comment, is_doc, max_regular, max_doc):
    violations = []
    i, n = 0, len(lines)
    while i < n:
        if not is_comment(lines[i]):
            i += 1
            continue
        doc = is_doc(lines[i])
        start = i
        while i < n and is_comment(lines[i]) and is_doc(lines[i]) == doc:
            i += 1
        run_len = i - start
        limit = max_doc if doc else max_regular
        if run_len > limit:
            kind = "doc comment" if doc else "comment"
            violations.append((start + 1, run_len, kind, limit))
    return violations


def check_block_runs(lines, opener, doc_opener, closer, max_regular, max_doc):
    violations = []
    i, n = 0, len(lines)
    while i < n:
        s = lines[i].strip()
        if s.startswith(doc_opener) or (s.startswith(opener) and not s.startswith(doc_opener)):
            doc = s.startswith(doc_opener)
            start = i
            j = i
            # closer may be on the opening line itself (single-line block comment)
            if closer not in lines[j][len(opener):]:
                j += 1
                while j < n and closer not in lines[j]:
                    j += 1
            end = min(j, n - 1)
            run_len = end - start + 1
            limit = max_doc if doc else max_regular
            if run_len > limit:
                kind = "doc block comment" if doc else "block comment"
                violations.append((start + 1, run_len, kind, limit))
            i = end + 1
            continue
        i += 1
    return violations


def check_python_docstrings(lines, max_doc):
    violations = []
    i, n = 0, len(lines)
    while i < n:
        s = lines[i].strip()
        for q in ('"""', "'''"):
            if s.startswith(q):
                rest = s[3:]
                if q in rest:
                    i += 1
                    break
                start = i
                j = i + 1
                while j < n and q not in lines[j]:
                    j += 1
                end = min(j, n - 1)
                run_len = end - start + 1
                if run_len > max_doc:
                    violations.append((start + 1, run_len, "docstring", max_doc))
                i = end + 1
                break
        else:
            i += 1
            continue
        continue
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
            run_len = i - start
            trait = is_trait_doc(lines, i)
            limit = TRAIT_DOC_MAX if trait else DOC_MAX
            if run_len > limit:
                kind = "trait doc comment" if trait else "doc comment"
                violations.append((start + 1, run_len, kind, limit))
            continue
        start = i
        while i < n and lines[i].strip().startswith("//") and not lines[i].strip().startswith(
            "///"
        ) and not lines[i].strip().startswith("//!"):
            i += 1
        run_len = i - start
        if run_len > REGULAR_MAX:
            violations.append((start + 1, run_len, "comment", REGULAR_MAX))
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
        run_len = end - start + 1
        i = end + 1
        if module:
            continue
        if doc:
            trait = is_trait_doc(lines, i)
            limit = TRAIT_DOC_MAX if trait else DOC_MAX
            kind = "trait doc block comment" if trait else "doc block comment"
        else:
            limit = REGULAR_MAX
            kind = "block comment"
        if run_len > limit:
            violations.append((start + 1, run_len, kind, limit))
    return violations


def check_rust(lines):
    return check_rust_line_comments(lines) + check_rust_block_comments(lines)


def check_c(lines):
    v = []
    v += check_line_runs(
        lines,
        is_comment=lambda l: l.strip().startswith("//"),
        is_doc=lambda l: False,
        max_regular=REGULAR_MAX,
        max_doc=DOC_MAX,
    )
    v += check_block_runs(lines, "/*", "/**", "*/", REGULAR_MAX, DOC_MAX)
    return v


def check_hash(lines):
    return check_line_runs(
        lines,
        is_comment=lambda l: l.strip().startswith("#") and not l.strip().startswith("#!"),
        is_doc=lambda l: False,
        max_regular=REGULAR_MAX,
        max_doc=DOC_MAX,
    )


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
