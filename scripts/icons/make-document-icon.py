#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0

"""Build the document Recent-list icon from the selected compiled vector pack.

Run sync-icon-pack.py to update all pack-dependent assets together. This script
can also run alone without fonts or archives; Pillow is needed only to rebuild.
"""
from pathlib import Path
from render_svg_path import render_icon

ROOT = Path(__file__).resolve().parent.parent.parent
OUT = ROOT / "assets" / "document.ico"

# Every size Explorer and the taskbar ask for. The largest is rendered and the
# rest are resampled from it, which keeps the shape identical at every scale.
SIZES = [256, 128, 64, 48, 32, 24, 16]

# The name in icons.rs, and the ink. A jump list row is drawn on the shell's own
# dark flyout, so a mid grey reads on both the light and dark taskbar without
# either disappearing.
GLYPH = "DOCUMENT"
INK = (0xC8, 0xC8, 0xCC, 0xFF)


def main() -> None:
    square = render_icon(GLYPH, SIZES[0], INK)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    square.save(OUT, format="ICO", sizes=[(s, s) for s in SIZES])
    print(f"wrote {OUT.relative_to(ROOT)} ({', '.join(str(s) for s in SIZES)})")


if __name__ == "__main__":
    main()

