#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0

"""Generate the Koru AI Chat icon family from Koru's current channel icons.

The base remains the ordinary Koru mark. A compact sparkle badge in its upper
right distinguishes standalone chat in the taskbar and app switcher while
keeping the product relationship obvious. The normal logo generator invokes
this script after rebuilding the channel icon family; it can also run alone.
"""
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw
except ImportError:
    sys.exit("Pillow is required: py -3 -m pip install --user pillow")

from render_svg_path import render_icon

ROOT = Path(__file__).resolve().parent.parent.parent
SIZES = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
FAMILIES = ["", "-beta", "-nightly", "-internal-debug", "-internal-release"]


def largest_ico(path: Path) -> Image.Image:
    source = Image.open(path)
    sizes = source.ico.sizes()
    source.size = max(sizes, key=lambda size: size[0] * size[1])
    return source.convert("RGBA")


def add_badge(base: Image.Image) -> Image.Image:
    canvas = base.resize((256, 256), Image.Resampling.LANCZOS)
    draw = ImageDraw.Draw(canvas)
    # The pale disc separates the badge from every channel tint and survives
    # the taskbar's smallest size. Its border keeps it legible on white.
    box = (137, 8, 250, 121)
    draw.ellipse(box, fill=(248, 248, 250, 255), outline=(92, 92, 98, 255), width=5)
    mark = render_icon("SPARKLE", 78, (58, 58, 64, 255))
    cx = (box[0] + box[2]) // 2
    cy = (box[1] + box[3]) // 2
    canvas.alpha_composite(mark, (cx - mark.width // 2, cy - mark.height // 2))
    return canvas


def main() -> None:
    for suffix in FAMILIES:
        source = ROOT / "assets" / f"koru{suffix}.ico"
        stem = ROOT / "assets" / f"koru-ai-chat{suffix}"
        image = add_badge(largest_ico(source))
        image.save(stem.with_suffix(".png"), format="PNG", optimize=True)
        image.save(stem.with_suffix(".ico"), format="ICO", sizes=SIZES)
        print(f"wrote {stem.relative_to(ROOT)}.png/.ico")


if __name__ == "__main__":
    main()
