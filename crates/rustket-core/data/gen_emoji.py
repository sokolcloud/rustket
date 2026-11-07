#!/usr/bin/env python3
"""Generates emoji.tsv from emojibase-data 16.0.3, the data set Rocket.Chat 8.9
builds its native emoji list from.

Mirrors apps/meteor/app/emoji-native/lib/generateEmojiData.ts (JoyPixels
shortcodes first, emojibase shortcodes as fallback, skin tones as
`<primary>_tone<n>`) and legacyEmojioneMap.ts.

Usage:
    npm pack emojibase-data@16.0.3 && tar xzf emojibase-data-16.0.3.tgz
    python3 -I gen_emoji.py package > emoji.tsv

Output (tab-separated, in emojibase order):
    E  <group 0-9 | r>  <emoji>  <shortcodes, primary first, space-separated>  <in picker 0/1>
    T  <base primary>   <tone spec, e.g. 3 or 5-4>  <emoji>  <extra shortcodes>
    L  <shortcode>      <emoji>
"""

import json
import sys

LEGACY = [
    ("bride_with_veil_tone1", "👰🏻"),
    ("bride_with_veil_tone2", "👰🏼"),
    ("bride_with_veil_tone3", "👰🏽"),
    ("bride_with_veil_tone4", "👰🏾"),
    ("bride_with_veil_tone5", "👰🏿"),
    ("bride_with_veil", "👰"),
    ("digit_zero", "0️⃣"),
    ("digit_one", "1️⃣"),
    ("digit_two", "2️⃣"),
    ("digit_three", "3️⃣"),
    ("digit_four", "4️⃣"),
    ("digit_five", "5️⃣"),
    ("digit_six", "6️⃣"),
    ("digit_seven", "7️⃣"),
    ("digit_eight", "8️⃣"),
    ("digit_nine", "9️⃣"),
    ("pound_symbol", "#️⃣"),
    ("asterisk_symbol", "*️⃣"),
    ("tone1", "🏻"),
    ("tone2", "🏼"),
    ("tone3", "🏽"),
    ("tone4", "🏾"),
    ("tone5", "🏿"),
]


def main(pkg):
    data = json.load(open(f"{pkg}/en/data.json", encoding="utf-8"))
    jp = json.load(open(f"{pkg}/en/shortcodes/joypixels.json", encoding="utf-8"))
    eb = json.load(open(f"{pkg}/en/shortcodes/emojibase.json", encoding="utf-8"))

    def codes(hexcode):
        e = jp.get(hexcode)
        if e is None:
            e = eb.get(hexcode)
        if e is None:
            return []
        return e if isinstance(e, list) else [e]

    def regional(hexcode):
        if "-" in hexcode:
            return False
        return 0x1F1E6 <= int(hexcode, 16) <= 0x1F1FF

    out = []
    keys = set()
    for d in data:
        group = d.get("group")
        if group == 2:
            continue
        ri = regional(d["hexcode"])
        if group is None and not ri:
            continue
        cs = codes(d["hexcode"])
        if not cs:
            continue
        for c in cs:
            assert "\t" not in c and " " not in c, c
        primary = cs[0]
        picker = 0 if (d.get("tone") or ri) else 1
        g = "r" if group is None else str(group)
        out.append(f"E\t{g}\t{d['emoji']}\t{' '.join(cs)}\t{picker}")
        keys.update(cs)
        for s in d.get("skins") or []:
            t = s.get("tone")
            if not t:
                continue
            spec = "-".join(str(x) for x in (t if isinstance(t, list) else [t]))
            extra = codes(s["hexcode"])
            out.append(f"T\t{primary}\t{spec}\t{s['emoji']}\t{' '.join(extra)}")
            keys.update(extra)
            keys.add(f"{primary}_tone{spec}")
    for code, emoji in LEGACY:
        if code not in keys:
            out.append(f"L\t{code}\t{emoji}")
    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main(sys.argv[1])
