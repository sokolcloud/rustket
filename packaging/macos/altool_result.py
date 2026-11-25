#!/usr/bin/env python3
"""Decides whether an `xcrun altool` run succeeded, from its output.

    python3 -I packaging/macos/altool_result.py <log file> <exit code>

altool has exited 0 after printing errors, so the output decides as well as
the exit code: the run fails on a non-zero exit, empty output, any
`ERROR:`/`ERROR ITMS-…`/`*** Error` line (also behind a timestamp or log prefix), or a JSON
result with `product-errors`. Prints the reason and exits 1 on failure, 0
otherwise. Used by .github/workflows/app-store.yml.
"""

import json
import re
import sys

ERROR_LINE = [
    re.compile(r"^\s*error\b", re.I | re.M),
    re.compile(r"\*\*\*\s*error\b", re.I),
    re.compile(r"\bERROR:"),
    re.compile(r"\bERROR\s+ITMS-\d+", re.I),
]


def json_results(text):
    """Every JSON object in `text`, which may mix log lines and JSON."""
    decoder = json.JSONDecoder()
    found = []
    at = text.find("{")
    while at >= 0:
        try:
            value, end = decoder.raw_decode(text, at)
        except ValueError:
            at = text.find("{", at + 1)
            continue
        if isinstance(value, dict):
            found.append(value)
        at = text.find("{", end)
    return found


def failure(text, code):
    if code != 0:
        return f"altool exited with {code}"
    if not text.strip():
        return "altool printed nothing"
    for pattern in ERROR_LINE:
        match = pattern.search(text)
        if match:
            start = text.rfind("\n", 0, match.start()) + 1
            line = text[start:].splitlines()[0].strip()
            return f"altool reported an error: {line}"
    for result in json_results(text):
        if result.get("product-errors"):
            return f"altool reported product errors: {json.dumps(result['product-errors'])}"
    return None


def main():
    if len(sys.argv) != 3:
        sys.exit("usage: altool_result.py <log file> <exit code>")
    with open(sys.argv[1], encoding="utf-8", errors="replace") as f:
        text = f.read()
    reason = failure(text, int(sys.argv[2]))
    if reason:
        print(f"::error::{reason}")
        sys.exit(1)


if __name__ == "__main__":
    main()
