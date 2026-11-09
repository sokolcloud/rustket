"""Turns a failed `cargo test` log into GitHub error annotations.

Job logs need repository admin rights to download, annotations do not: this
makes failing tests, panics and compile errors readable through the public
check-runs API. Usage: python test-annotations.py <log file>
"""

import re
import sys

LIMIT = 3500  # characters per annotation message
MAX = 9  # GitHub shows at most 10 error annotations per step


def escape(text: str) -> str:
    return text.replace("%", "%25").replace("\r", "").replace("\n", "%0A")


def escape_property(text: str) -> str:
    return escape(text).replace(":", "%3A").replace(",", "%2C")


def main() -> None:
    log = open(sys.argv[1], encoding="utf-8", errors="replace").read()
    log = re.sub(r"\x1b\[[0-9;]*m", "", log)
    blocks = []
    # libtest prints each failure's captured output as "---- name stdout ----".
    for m in re.finditer(r"^---- (\S+) stdout ----\n(.*?)(?=^---- |^failures:$)", log, re.M | re.S):
        blocks.append((m.group(1), m.group(2).strip()))
    if not blocks:
        for m in re.finditer(r"^(error(?:\[E\d+\])?: .*?)(?=^\s*$)", log, re.M | re.S):
            blocks.append(("compile error", m.group(1).strip()))
    if not blocks:
        tail = "\n".join(log.splitlines()[-60:])
        blocks.append(("cargo test failed", tail))
    for title, body in blocks[:MAX]:
        print(f"::error title={escape_property(title)}::{escape(body[:LIMIT])}")
    if len(blocks) > MAX:
        rest = ", ".join(t for t, _ in blocks[MAX:])
        print(f"::error title=more failures::{escape(rest[:LIMIT])}")


main()
