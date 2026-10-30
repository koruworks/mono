#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Regenerate compiled icon paths from checksum-pinned upstream archives."""
import argparse
import math
import subprocess
import sys

from icon_pack import ROOT, SELECTABLE_PACKS, bundled_notice, catalog_notices, load_icons, rust_catalog


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pack", choices=SELECTABLE_PACKS, default="tabler")
    parser.add_argument("--stroke-width", type=float, default=1.85)
    args = parser.parse_args()
    if not math.isfinite(args.stroke_width) or not 0 < args.stroke_width <= 4:
        parser.error("stroke width must be finite and in (0, 4]")
    icons, notices = load_icons(args.pack)
    supported_notices = catalog_notices()
    source = rust_catalog(args.pack, args.stroke_width)
    # Validate the bounded rasterizer and Pillow before changing any artifacts.
    from render_svg_path import render_icon
    for name in ("DOCUMENT", "SPARKLE"):
        render_icon(name, source=source)
    target = ROOT / "crates/koru-draw/src/icon_pack_data.rs"
    target.write_text(source, encoding="utf-8", newline="\n")
    # Keep every supported license in distributions, including Tabler brand
    # fallbacks when Lucide is selected. Their content never depends on selection.
    for pack, notice in supported_notices.items():
        (ROOT / f"crates/koru-release/licences/bundled/icons-{pack}.txt").write_text(
            bundled_notice(pack, notice), encoding="utf-8", newline="\n")
    weight = "original outline/fill weights" if args.pack in ("phosphor", "remix") else f"stroke width {args.stroke_width:g}"
    print(f"Generated {len(icons)} {args.pack} icons with {weight}")
    for script in ("make-document-icon.py", "make-ai-chat-icon.py"):
        subprocess.run([sys.executable, str(ROOT / "scripts/icons" / script)], check=True)


if __name__ == "__main__":
    main()
