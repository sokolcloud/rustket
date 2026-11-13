#!/usr/bin/env python3
"""Converts a 24/32-bit ZPixmap XWD dump (`xwd -id <window>`) to PNG.

Usage: xwd2png.py <in.xwd> <out.png> [max_width]
"""
import struct
import sys

from PIL import Image


def main() -> None:
    src, dst = sys.argv[1], sys.argv[2]
    max_width = int(sys.argv[3]) if len(sys.argv) > 3 else 0
    data = open(src, "rb").read()
    h = struct.unpack(">25I", data[:100])
    header_size, width, height = h[0], h[4], h[5]
    byte_order, bits_per_pixel, bytes_per_line, ncolors = h[7], h[11], h[12], h[19]
    modes = {32: ("BGRX", "XRGB"), 24: ("BGR", "RGB")}
    if bits_per_pixel not in modes:
        sys.exit(f"unsupported bits per pixel: {bits_per_pixel}")
    raw = modes[bits_per_pixel][0 if byte_order == 0 else 1]
    start = header_size + ncolors * 12
    pixels = data[start : start + bytes_per_line * height]
    img = Image.frombuffer("RGB", (width, height), pixels, "raw", raw, bytes_per_line, 1)
    if max_width and width > max_width:
        img = img.resize((max_width, round(height * max_width / width)), Image.LANCZOS)
    img.save(dst, optimize=True)


if __name__ == "__main__":
    main()
