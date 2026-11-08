#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0

"""Regenerate the app icon (assets/koru.ico) from the logo master (assets/logo.png).

Workflow for trying a new logo:

    1. replace assets/logo.png with your candidate (a square PNG)
    2. koru.bat logo            # or: ./koru.sh logo  /  py -3 scripts/icons/set-logo.py
    3. koru.bat run             # build and look

The in-app mark (crates/koru-draw/src/brand.rs) and the window icon
(crates/koru-workspace-host/src/app.rs) embed assets/logo.png straight via
include_bytes!, so changing the file is already enough for those two -- cargo
rebuilds because it tracks the embedded file. The one *derived* artifact is
assets/koru.ico: a multi-size icon that Windows shows in the taskbar, Alt+Tab,
the window caption and Explorer. The .ico cannot be an include_bytes target
(apps/koru/build.rs embeds it through koru-winres), so this script is the
only step needed to keep it in step with the logo.

The size of the logo is not pinned anywhere -- the embed tests only assert it is
square, because the caption tile fills a square via scale_non_uniform -- so a new
logo of any square size needs no code or test edit.

Besides koru.ico (the brand teal, which is what a stable build embeds), this
script emits a tinted koru-<segment>.ico per release channel, and each app's
build.rs picks one by KORU_RELEASE_CHANNEL -- so a nightly tray icon looks
nightly. The tint recipe (set the hue, keep per-pixel saturation/lightness and
alpha) is implemented a second time in Rust for the surfaces that hold RGBA at
runtime: crates/koru-channel/src/tint.rs. The TINTS table below and tint_of()
there must stay in step, or the tray disagrees with the window.
"""
import colorsys
import sys
from pathlib import Path

try:
    from PIL import Image
except ImportError:
    sys.exit(
        "Pillow is required to rebuild the icon.\n"
        "    py -3 -m pip install Pillow"
    )

# Repo root is two levels up from scripts/<this file>.
ROOT = Path(__file__).resolve().parent.parent.parent
LOGO = ROOT / "assets" / "logo.png"
ICO = ROOT / "assets" / "koru.ico"
DOCK = ROOT / "assets" / "logo-dock.png"

# segment -> (hue in degrees, saturation multiplier), or None to desaturate.
# Mirrors koru_channel::tint::tint_of; stable is the untinted koru.ico.
TINTS = {
    "beta": (235.0, 1.0),
    "nightly": (38.0, 1.0),
    "internal-debug": None,
    "internal-release": (275.0, 0.5),
}

# The sizes Windows reads from one .ico. The taskbar, Alt+Tab, the window
# caption, Explorer thumbnails and the .exe's large icon view each pick from
# this set, so a single file covers every place the mark appears.
SIZES = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]

# The macOS Dock derivative. Apple's icon grid puts the artwork on an 824-wide
# tile inside a 1024 canvas — every neighbouring icon carries that margin
# baked in, so a full-bleed logo reads as *larger* than the apps beside it.
# 512/412 keeps the same ratio at a quarter of the bytes; the Dock never asks
# for more than 256@2x.
DOCK_CANVAS = 512
DOCK_ART = 412


def main() -> int:
    if not LOGO.is_file():
        sys.exit(f"no logo at {LOGO}\nput your candidate there as logo.png")

    src = Image.open(LOGO)
    w, h = src.size
    print(f"logo    {w}x{h}  {src.mode}  ({LOGO.name})")

    if w != h:
        print(
            f"warning: logo is not square ({w}x{h}). The in-app tile fills a\n"
            "         square and the embed tests assert square, so the build\n"
            "         will work but `koru.bat test` will not until it is square."
        )

    # Compose on RGBA so partial transparency (soft edges, shadows) survives
    # into the icon rather than being flattened to an opaque background.
    rgba = src if src.mode == "RGBA" else src.convert("RGBA")

    # Write through a temp file then atomically replace each .ico, so an
    # interrupted run never leaves a half-written file that the build would
    # then embed. build.rs declares the .ico as rerun-if-changed, so the next
    # build re-embeds it automatically.
    save_ico(rgba, ICO)
    for segment, tint in TINTS.items():
        save_ico(tinted(rgba, tint), ICO.parent / f"koru-{segment}.ico")

    print(
        "ico     koru.ico + "
        + ", ".join(f"koru-{s}.ico" for s in TINTS)
        + "  regenerated  "
        + ", ".join(f"{s[0]}" for s in SIZES)
        + "  (alpha preserved)"
    )

    # The Dock variant: the logo scaled onto Apple's grid, transparent margins
    # and all, consumed by koru-window's dock::set_dock_icon via
    # include_bytes!. Same atomic-replace discipline as the .ico.
    art = rgba.resize((DOCK_ART, DOCK_ART), Image.LANCZOS)
    canvas = Image.new("RGBA", (DOCK_CANVAS, DOCK_CANVAS), (0, 0, 0, 0))
    inset = (DOCK_CANVAS - DOCK_ART) // 2
    canvas.paste(art, (inset, inset), art)
    dock_part = DOCK.parent / (DOCK.name + ".part")
    canvas.save(dock_part, format="PNG")
    dock_part.replace(DOCK)
    print(f"dock    {DOCK.name}  regenerated  {DOCK_ART} on {DOCK_CANVAS} (Apple icon grid)")

    save_icns(rgba)
    print("done    build now:  koru.bat run   (or  ./koru.sh run)")
    return 0


def save_icns(rgba: Image.Image) -> None:
    """assets/koru.icns, the Finder/bundle icon the mac package requires.

    Built through `iconutil`, which only exists on macOS — elsewhere this
    prints the fact and moves on, the same posture as a missing font: the
    committed .icns keeps working until someone on a Mac regenerates it.
    Every size uses the same Apple-grid inset as the Dock PNG, so the Finder
    icon and the Dock icon are the same mark at the same visual weight.
    """
    import shutil
    import subprocess
    import tempfile

    icns = ROOT / "assets" / "koru.icns"
    if shutil.which("iconutil") is None:
        print(f"icns    skipped (no iconutil on this platform); {icns.name} unchanged")
        return
    with tempfile.TemporaryDirectory() as scratch:
        iconset = Path(scratch) / "koru.iconset"
        iconset.mkdir()
        for points in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                pixels = points * scale
                art = rgba.resize((pixels * DOCK_ART // DOCK_CANVAS,) * 2, Image.LANCZOS)
                canvas = Image.new("RGBA", (pixels, pixels), (0, 0, 0, 0))
                inset = (pixels - art.size[0]) // 2
                canvas.paste(art, (inset, inset), art)
                suffix = "" if scale == 1 else "@2x"
                canvas.save(iconset / f"icon_{points}x{points}{suffix}.png", format="PNG")
        # iconutil refuses an output that does not end in .icns, so the
        # atomic-replace temp keeps the suffix instead of gaining .part.
        part = icns.parent / "koru.part.icns"
        subprocess.run(
            ["iconutil", "--convert", "icns", "--output", str(part), str(iconset)],
            check=True,
        )
        part.replace(icns)
    print(f"icns    {icns.name}  regenerated  (Apple icon grid, 16..512@2x)")


def save_ico(rgba: Image.Image, path: Path) -> None:
    part = path.parent / (path.name + ".part")
    rgba.save(part, format="ICO", sizes=SIZES)
    part.replace(path)


def tinted(rgba: Image.Image, tint) -> Image.Image:
    """Set every pixel's hue (or desaturate when tint is None), keeping its
    lightness and alpha -- the same recipe as koru_channel::tint::apply."""
    out = rgba.copy()
    px = out.load()
    w, h = out.size
    for y in range(h):
        for x in range(w):
            r, g, b, a = px[x, y]
            hue, lig, sat = colorsys.rgb_to_hls(r / 255.0, g / 255.0, b / 255.0)
            if tint is None:
                sat = 0.0
            else:
                hue = tint[0] / 360.0
                sat = sat * tint[1]
            r, g, b = colorsys.hls_to_rgb(hue, lig, sat)
            px[x, y] = (round(r * 255), round(g * 255), round(b * 255), a)
    return out


if __name__ == "__main__":
    raise SystemExit(main())
