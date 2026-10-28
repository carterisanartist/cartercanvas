#!/usr/bin/env python3
"""Draws the CarterCanvas app icon (1024×1024, carterware palette) into the asset catalog.

    python3 tools/make_icon.py

A thick brush-stroke "C" in the carterware red gradient on the near-black canvas, with a
highlight nib where the stroke ends.
"""
import math
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

SIZE = 1024
SS = 4  # supersampling for smooth edges
ACCENT = (0xE4, 0x00, 0x2B)
HOVER = (0xFF, 0x6B, 0x84)
CANVAS = (0x09, 0x04, 0x06)
PANEL = (0x1C, 0x0A, 0x0E)
TEXT = (0xFF, 0xF5, 0xF6)

OUT = Path(__file__).resolve().parent.parent / "CarterCanvas/Resources/Assets.xcassets/AppIcon.appiconset/icon-1024.png"


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def background(n):
    img = Image.new("RGB", (n, n), CANVAS)
    px = img.load()
    cx, cy = n * 0.42, n * 0.38
    rmax = n * 0.95
    for y in range(n):
        for x in range(n):
            d = math.hypot(x - cx, y - cy) / rmax
            px[x, y] = lerp(PANEL, CANVAS, min(1.0, d ** 0.8))
    return img


def stroke_mask(n):
    """The "C": an arc swept with a round brush whose width swells and tapers like a pen stroke."""
    mask = Image.new("L", (n, n), 0)
    d = ImageDraw.Draw(mask)
    cx, cy, r = n * 0.5, n * 0.5, n * 0.285
    start, end = math.radians(48), math.radians(318)
    steps = 900
    for i in range(steps + 1):
        t = i / steps
        a = start + (end - start) * t
        width = n * (0.032 + 0.058 * math.sin(math.pi * t) ** 0.55)
        x, y = cx + r * math.cos(a), cy - r * math.sin(a)
        d.ellipse((x - width, y - width, x + width, y + width), fill=255)
    return mask


def gradient(n):
    img = Image.new("RGB", (n, n))
    px = img.load()
    for y in range(n):
        for x in range(n):
            t = min(1.0, max(0.0, (x * 0.35 + (n - y) * 0.65) / n))
            px[x, y] = lerp(ACCENT, HOVER, t ** 1.6)
    return img


def main():
    n = SIZE * SS // 4  # gradients at quarter resolution, scaled up (they're smooth)
    bg = background(n).resize((SIZE * SS, SIZE * SS), Image.BICUBIC)
    fill = gradient(n).resize((SIZE * SS, SIZE * SS), Image.BICUBIC)
    mask = stroke_mask(SIZE * SS)

    glow = mask.filter(ImageFilter.GaussianBlur(SIZE * SS * 0.03))
    glow_layer = Image.new("RGB", bg.size, ACCENT)
    bg = Image.composite(glow_layer, bg, glow.point(lambda v: int(v * 0.45)))
    out = Image.composite(fill, bg, mask)

    # A soft inner highlight along the top of the stroke.
    shifted = ImageChops.offset(mask, 0, int(SIZE * SS * 0.012))
    highlight = ImageChops.subtract(mask, shifted).filter(ImageFilter.GaussianBlur(SIZE * SS * 0.004))
    out = Image.composite(Image.new("RGB", out.size, TEXT), out, highlight.point(lambda v: int(v * 0.55)))

    # The nib: where the stroke ends, a bright dot.
    cx, cy, r = SIZE * SS * 0.5, SIZE * SS * 0.5, SIZE * SS * 0.285
    a = math.radians(318)
    x, y = cx + r * math.cos(a), cy - r * math.sin(a)
    nib = SIZE * SS * 0.03
    ImageDraw.Draw(out).ellipse((x - nib, y - nib, x + nib, y + nib), fill=TEXT)

    out = out.resize((SIZE, SIZE), Image.LANCZOS)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    out.save(OUT, optimize=True)
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
