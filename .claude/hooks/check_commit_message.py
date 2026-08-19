#!/usr/bin/env python3
"""PreToolUse hook: block `git commit` calls whose subject line is too long."""
import json
import re
import sys

MAX_SUBJECT_LEN = 150


def extract_subject(command: str) -> str | None:
    # heredoc form: git commit -m "$(cat <<'EOF' ... EOF)"
    m = re.search(r"<<[-]?['\"]?(\w+)['\"]?\n(.*?)\n\1", command, re.DOTALL)
    if m:
        body = m.group(2)
        first = body.strip("\n").split("\n", 1)[0]
        return first

    # simple -m "..." / -m '...' form
    m = re.search(r"-m\s+\"((?:[^\"\\]|\\.)*)\"", command)
    if m:
        return m.group(1).split("\\n", 1)[0]
    m = re.search(r"-m\s+'((?:[^'\\]|\\.)*)'", command)
    if m:
        return m.group(1).split("\\n", 1)[0]

    return None


def main() -> None:
    payload = json.load(sys.stdin)
    if payload.get("tool_name") != "Bash":
        return
    command = payload.get("tool_input", {}).get("command", "")
    if not re.search(r"(^|[;&|]\s*)git\s+commit\b", command):
        return
    if "-m" not in command and "<<" not in command:
        return

    subject = extract_subject(command)
    if subject is None:
        return

    length = len(subject)
    if length > MAX_SUBJECT_LEN:
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": (
                            f"Commit subject is {length} chars (max {MAX_SUBJECT_LEN}): "
                            f"{subject!r}. Shorten the first line; put detail in the body."
                        ),
                    }
                }
            )
        )


if __name__ == "__main__":
    main()
