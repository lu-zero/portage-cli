#!/usr/bin/env python3
"""PreToolUse hook: block `git commit` if rustdoc is broken.

Runs `cargo doc` with `-D warnings` (catches broken intra-doc links, bad
rustdoc syntax, etc.) and `cargo test --doc` (catches failing doctests)
before allowing a commit to go through.
"""
import json
import os
import re
import subprocess
import sys

TIMEOUT = 570


def tail(text: str, n: int = 40) -> str:
    return "\n".join(text.splitlines()[-n:])


def deny(reason: str) -> None:
    print(
        json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason[:4000],
                }
            }
        )
    )


def main() -> None:
    payload = json.load(sys.stdin)
    if payload.get("tool_name") != "Bash":
        return
    command = payload.get("tool_input", {}).get("command", "")
    if not re.search(r"(^|[;&|]\s*)git\s+commit\b", command):
        return

    env = os.environ.copy()
    env["RUSTDOCFLAGS"] = (env.get("RUSTDOCFLAGS", "") + " -D warnings").strip()

    try:
        doc = subprocess.run(
            ["cargo", "doc", "--workspace", "--no-deps"],
            capture_output=True,
            text=True,
            env=env,
            timeout=TIMEOUT,
        )
    except subprocess.TimeoutExpired:
        deny("cargo doc timed out; rustdoc health could not be verified.")
        return
    if doc.returncode != 0:
        deny(f"cargo doc failed (broken rustdoc):\n{tail(doc.stderr)}")
        return

    try:
        tests = subprocess.run(
            ["cargo", "test", "--doc", "--workspace"],
            capture_output=True,
            text=True,
            env=env,
            timeout=TIMEOUT,
        )
    except subprocess.TimeoutExpired:
        deny("cargo test --doc timed out; doctest health could not be verified.")
        return
    if tests.returncode != 0:
        deny(f"cargo test --doc failed (broken doctest):\n{tail(tests.stdout + tests.stderr)}")
        return


if __name__ == "__main__":
    main()
