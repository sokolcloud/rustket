#!/usr/bin/env python3
"""Writes the third-party notices for the rustket binary to stdout.

Every crate compiled into the binary (normal dependencies of `rustket`, from
`cargo tree`) with its license expression and the full text of each license,
copyright and notice file the crate ships (located through `cargo metadata`).
Identical texts are printed once with the list of crates they cover. Bundled
non-crate assets (fonts, icon font, data tables) come from packaging/licenses
and the font directory.

    python3 -I packaging/notices.py > THIRD-PARTY-NOTICES
"""

import hashlib
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|copyright|notice|unlicense|authors)", re.I)
ASSETS = [
    ("Noto Sans fonts (bundled in the binary)", "crates/rustket-ui/assets/fonts/OFL.txt"),
    ("Lucide icons", "packaging/licenses/Lucide.txt"),
    ("Emoji table", "packaging/licenses/emoji-data.txt"),
    ("BIP-39 word list", "packaging/licenses/bip39.txt"),
]

MIT = """Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE."""


def cargo(*args):
    return subprocess.run(
        ["cargo", *args], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout


def linked_crates():
    out = cargo(
        "tree", "--locked", "-p", "rustket", "-e", "normal",
        "--prefix", "none", "--format", "{p}",
    )
    crates = set()
    for line in out.splitlines():
        parts = line.split()
        # Workspace members print their path: "rocketchat v0.1.0 (/…)";
        # proc macros run at build time only.
        if len(parts) >= 2 and not any(p.startswith("(/") or p == "(proc-macro)" for p in parts):
            crates.add((parts[0], parts[1].lstrip("v")))
    return crates


def license_files(manifest_path):
    root = os.path.dirname(manifest_path)
    found = []
    for name in sorted(os.listdir(root)):
        path = os.path.join(root, name)
        if os.path.isfile(path) and LICENSE_FILE.match(name):
            found.append(path)
    return found


def main():
    wanted = linked_crates()
    meta = json.loads(cargo("metadata", "--locked", "--format-version", "1"))
    texts = {}  # sha256 -> (text, [crate labels])
    order = []
    missing = []
    crates = []
    for pkg in meta["packages"]:
        if (pkg["name"], pkg["version"]) not in wanted or pkg.get("source") is None:
            continue
        label = f"{pkg['name']} {pkg['version']}"
        crates.append((label, pkg.get("license") or "see license file"))
        files = license_files(pkg["manifest_path"])
        if not files:
            authors = ", ".join(pkg.get("authors") or []) or "the authors"
            missing.append((label, pkg.get("license") or "?", pkg.get("repository") or "", authors))
        for path in files:
            with open(path, encoding="utf-8", errors="replace") as f:
                text = f.read().strip()
            key = hashlib.sha256(text.encode()).hexdigest()
            if key not in texts:
                texts[key] = (text, [])
                order.append(key)
            texts[key][1].append(f"{label} ({os.path.basename(path)})")

    w = sys.stdout.write
    w("Third-party software in Rustket\n")
    w("===============================\n\n")
    w("The rustket binary is statically linked with the Rust crates listed below and\n")
    w("embeds the assets in the next section. Their copyright notices and license\n")
    w("texts follow.\n\n")
    for title, rel in ASSETS:
        with open(os.path.join(ROOT, rel), encoding="utf-8") as f:
            text = f.read().strip()
        w(f"{'-' * 78}\n{title}\n{'-' * 78}\n\n{text}\n\n")
    w(f"{'=' * 78}\nRust crates ({len(crates)})\n{'=' * 78}\n\n")
    for label, lic in sorted(crates):
        w(f"{label} | {lic}\n")
    w("\n")
    for key in order:
        text, owners = texts[key]
        w(f"{'-' * 78}\n")
        for owner in sorted(owners):
            w(f"{owner}\n")
        w(f"{'-' * 78}\n\n{text}\n\n")
    if missing:
        w(f"{'-' * 78}\n")
        w("Crates that ship no license file. License expression and authors are from\n")
        w("their Cargo.toml; the standard license texts follow the list.\n\n")
        for label, lic, repo, authors in sorted(missing):
            w(f"{label} | {lic} | {repo}\n    Copyright (c) {authors}\n")
        w(f"\n{'-' * 78}\nMIT License (for the crates above that use it)\n{'-' * 78}\n\n{MIT}\n")
        for spdx in ("Apache-2.0", "CC0-1.0"):
            path = os.path.join("/usr/share/common-licenses", spdx)
            if any(spdx in lic for _, lic, _, _ in missing) and os.path.isfile(path):
                with open(path, encoding="utf-8") as f:
                    text = f.read().strip()
                w(f"\n{'-' * 78}\n{spdx} (for the crates above that use it)\n{'-' * 78}\n\n{text}\n")


if __name__ == "__main__":
    main()
