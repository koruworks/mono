# SPDX-License-Identifier: MPL-2.0
"""Pinned icon inputs and strict, build-time SVG normalization.

There is no XML parsing or archive access in the application. Tests use SVG
strings; only the explicit regeneration command reads the fetched archives.
"""
from dataclasses import dataclass
import hashlib
import json
import math
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
PACKS = {"tabler": "tabler-icons-3.31.0.tgz", "lucide": "lucide-static-0.477.0.tgz",
         "remix": "remixicon-4.6.0.tgz"}
SELECTABLE_PACKS = ("phosphor", *PACKS)
ALLOWED = {"svg", "g", "path", "line", "rect", "circle", "ellipse", "polyline", "polygon"}
NUMBER = re.compile(r"[-+]?(?:\d*\.\d+|\d+\.?\d*)(?:[eE][-+]?\d+)?")

# Original Koru artwork, MPL-2.0. Centerlines on the same 24-unit grid as
# upstream icons; neither path is traced or copied from a third-party mark.
CUSTOM = {
    "koru:key-return": "M3 5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z M16 9v4a1 1 0 0 1-1 1H9 M11 12l-2 2 2 2",
    "koru:tux": "M7 16C5 13 7 10 7 7C7 1 17 1 17 7C17 10 19 13 17 16 M7 10C3 13 3 16 5 17 M17 10C21 13 21 16 19 17 M8 11C6 14 7 18 12 18C17 18 18 14 16 11 M10 8l2 2 2-2-2-1z M9 6h.01 M15 6h.01 M7 17l-3 3 5 1 2-3 M17 17l3 3-5 1-2-3",
}


def num(value):
    value = float(value)
    if not math.isfinite(value):
        raise ValueError("non-finite SVG coordinate")
    return format(value, ".9g")


def shape_path(el):
    tag = el.tag.rsplit("}", 1)[-1]
    def n(key, default=0):
        return float(el.get(key, default))
    if tag == "path":
        return el.get("d", "")
    if tag == "line":
        return f"M{num(n('x1'))} {num(n('y1'))}L{num(n('x2'))} {num(n('y2'))}"
    if tag in {"polyline", "polygon"}:
        raw = el.get("points", "")
        if NUMBER.sub("", raw).strip(" ,\t\r\n"):
            raise ValueError("invalid points")
        points = NUMBER.findall(raw)
        if len(points) < 4 or len(points) % 2:
            raise ValueError("points need coordinate pairs")
        return "M" + " ".join(points[:2]) + "L" + " ".join(points[2:]) + ("Z" if tag == "polygon" else "")
    if tag in {"circle", "ellipse"}:
        x, y = n("cx"), n("cy")
        rx, ry = (n("r"), n("r")) if tag == "circle" else (n("rx"), n("ry"))
        if rx <= 0 or ry <= 0:
            raise ValueError("ellipse radii must be positive")
        return f"M{num(x-rx)} {num(y)}a{num(rx)} {num(ry)} 0 1 0 {num(2*rx)} 0a{num(rx)} {num(ry)} 0 1 0 {num(-2*rx)} 0Z"
    if tag == "rect":
        x, y, w, h = n("x"), n("y"), n("width"), n("height")
        if w <= 0 or h <= 0:
            raise ValueError("rectangle dimensions must be positive")
        rx = min(n("rx", el.get("ry", 0)), w/2)
        ry = min(n("ry", el.get("rx", 0)), h/2)
        if rx < 0 or ry < 0:
            raise ValueError("negative rectangle radius")
        if rx == 0 or ry == 0:
            return f"M{num(x)} {num(y)}h{num(w)}v{num(h)}h{num(-w)}Z"
        arc = f"a{num(rx)} {num(ry)} 0 0 1 "
        return (f"M{num(x+rx)} {num(y)}h{num(w-2*rx)}" + arc + f"{num(rx)} {num(ry)}v{num(h-2*ry)}" + arc +
                f"{num(-rx)} {num(ry)}h{num(-w+2*rx)}" + arc + f"{num(-rx)} {num(-ry)}v{num(-h+2*ry)}" + arc + f"{num(rx)} {num(-ry)}Z")
    raise ValueError(f"not a shape: {tag}")


def normalize(svg, label="SVG"):
    """Return independent stroke/fill paths; fail closed on unsupported input."""
    try:
        root = ET.fromstring(svg)
        if root.tag.rsplit("}", 1)[-1] != "svg" or root.get("viewBox", "").split() != ["0", "0", "24", "24"]:
            raise ValueError("expected viewBox 0 0 24 24")
        strokes, fills = [], []
        def visit(el, inherited):
            tag = el.tag.rsplit("}", 1)[-1]
            if tag not in ALLOWED:
                raise ValueError(f"unsupported element {tag}")
            for attr in ("transform", "style", "opacity", "fill-opacity", "stroke-opacity", "stroke-dasharray", "clip-path", "mask"):
                if attr in el.attrib:
                    raise ValueError(f"unsupported attribute {attr}")
            if el.get("fill-rule", "nonzero") != "nonzero":
                raise ValueError("unsupported fill-rule")
            paint = {k: el.get(k, inherited[k]) for k in ("fill", "stroke")}
            if tag not in {"svg", "g"}:
                if all(v == "none" for v in paint.values()):
                    return
                if any(v not in {"none", "currentColor", "black"} for v in paint.values()):
                    raise ValueError("only monochrome paths are supported")
                d = shape_path(el)
                if not d.strip():
                    raise ValueError("empty visible path")
                # Each SVG shape starts at the origin. Concatenating independent
                # paths must not make an initial relative move inherit the end
                # of the previous shape. Keep its implicit relative line pairs.
                if d.lstrip().startswith("m"):
                    d = "M0 0 " + d.lstrip()
                if paint["stroke"] != "none":
                    strokes.append(d)
                if paint["fill"] != "none":
                    fills.append(d)
            for child in el:
                visit(child, paint)
        visit(root, {"fill": "black", "stroke": "none"})
        if not strokes and not fills:
            raise ValueError("icon has no visible geometry")
        return " ".join(strokes), " ".join(fills)
    except (ET.ParseError, ValueError) as error:
        raise ValueError(f"{label}: {error}") from error


def declarations(source):
    result = {}
    aliases = {}
    for name, value in re.findall(r"pub const (\w+): char = ([^;]+);", source):
        literal = re.fullmatch(r"'\\u\{([0-9a-fA-F]+)\}'", value)
        if literal:
            result[name] = int(literal[1], 16)
        else:
            aliases[name] = value
    while aliases:
        resolved = {name: result[target] for name, target in aliases.items() if target in result}
        if not resolved:
            raise ValueError(f"unresolved icon aliases: {aliases}")
        result.update(resolved)
        for name in resolved:
            del aliases[name]
    return result


def verified_archive(pack, root=ROOT):
    path = root / "third-party/icons" / PACKS[pack]
    relative = path.relative_to(root).as_posix()
    lines = (root / "third-party/fetch.manifest").read_text(encoding="utf-8").splitlines()
    entries = [line.split() for line in lines if line.startswith(f"file {relative} ")]
    if len(entries) != 1:
        raise ValueError(f"missing archive pin for {pack}")
    if not path.exists():
        script = root / "scripts/setup" / ("fetch-third-party.ps1" if sys.platform == "win32" else "fetch-third-party.sh")
        command = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(script)] if sys.platform == "win32" else ["sh", str(script)]
        subprocess.run(command, check=True, cwd=root)
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual != entries[0][2]:
        raise ValueError(f"{relative}: checksum mismatch; refetch the pinned input")
    return tarfile.open(path, "r:gz")


@dataclass(frozen=True)
class Icon:
    name: str
    codepoint: int
    source: str
    stroke: str
    fill: str
    active_fill: str | None


def load_icons(pack, root=ROOT):
    mapping = json.loads((root / "scripts/icons/icon-map.json").read_text(encoding="utf-8"))
    codes = declarations((root / "crates/koru-draw/src/icons.rs").read_text(encoding="utf-8"))
    selected = [codes[name] for name in mapping]
    if len(set(selected)) != len(selected) or set(selected) != set(codes.values()):
        raise ValueError("icon-map.json must cover every declared codepoint exactly once")
    if pack == "phosphor":
        return load_phosphor(mapping, codes, root)
    archives = {}
    notices = {}
    def archive(source):
        if source not in archives:
            archives[source] = verified_archive(source, root)
            names = archives[source].getnames()
            license_path = next((n for n in ("package/LICENSE", "package/LICENSE.txt", "package/License") if n in names), None)
            if license_path is None:
                raise ValueError(f"{source}: archive lacks license")
            notices[source] = archives[source].extractfile(license_path).read().decode("utf-8")
            if source == "lucide":
                # The pinned npm LICENSE names Feather's MIT portions but omits
                # the MIT terms. Retain its attribution and the full MIT text.
                # See https://lucide.dev/license for the upstream split.
                mit = (root / "crates/koru-release/licences/spdx/MIT.txt").read_text(encoding="utf-8")
                notices[source] += "\nFeather-derived portions:\n\n" + mit.replace(
                    "Copyright (c) <year> <copyright holders>",
                    "Copyright (c) 2013-2022 Cole Bemis")
        return archives[source]
    def read(source, slug, kind, name):
        path = f"package/icons/{kind + '/' if source == 'tabler' else ''}{slug}.svg"
        try:
            svg = archive(source).extractfile(path).read()
        except KeyError as error:
            raise ValueError(f"{name}: missing {source} asset {path}") from error
        return normalize(svg, f"{name}: {path}")
    icons = []
    try:
        for name, row in mapping.items():
            slug = row[pack]
            if slug in CUSTOM:
                icons.append(Icon(name, codes[name], "Koru", CUSTOM[slug], "", None))
                continue
            source = pack
            if ":" in slug:
                source, slug = slug.split(":", 1)
            stroke, fill = read(source, slug, "outline", name)
            active = None
            filled_slug = row.get("filled") if pack == "tabler" else row.get("remix_filled") if pack == "remix" else None
            if filled_slug:
                active_stroke, active = read(source, filled_slug, "filled", name)
                if active_stroke:
                    raise ValueError(f"{name}: filled variant unexpectedly needs strokes")
            icons.append(Icon(name, codes[name], source.title(), stroke, fill, active))
    finally:
        for tar in archives.values():
            tar.close()
    return sorted(icons, key=lambda icon: icon.codepoint), notices


def load_phosphor(mapping, codes, root):
    """Export the original pinned regular/fill glyphs into the shared 24-unit slot."""
    from fontTools.ttLib import TTFont
    from fontTools.pens.svgPathPen import SVGPathPen
    from fontTools.pens.transformPen import TransformPen
    from fontTools.pens.qu2cuPen import Qu2CuPen

    pins = dict(line.split("=", 1) for line in
                (root / "scripts/setup/fonts.manifest").read_text().splitlines()
                if line and not line.startswith("#"))
    outlines = []
    for filename in ("Phosphor.ttf", "Phosphor-Fill.ttf"):
        path = root / "assets/fonts" / filename
        if hashlib.sha256(path.read_bytes()).hexdigest() != pins[filename]:
            raise ValueError(f"{filename}: checksum mismatch; run the font fetcher")
        with TTFont(path) as font:
            glyphs, cmap = font.getGlyphSet(), font.getBestCmap()
            scale = 24 / font["head"].unitsPerEm
            transform = (scale, 0, 0, -scale, 0, font["hhea"].ascent * scale)
            paths = {}
            for name in mapping:
                if codes[name] not in cmap:
                    raise ValueError(f"{filename}: missing {name}")
                pen = SVGPathPen(glyphs, ntos=num)
                glyphs[cmap[codes[name]]].draw(TransformPen(
                    Qu2CuPen(pen, max_err=0.0001, all_cubic=True), transform))
                paths[name] = pen.getCommands()
            outlines.append(paths)
    notice = (root / "crates/koru-release/licences/bundled/font-phosphor.txt").read_text(encoding="utf-8").split("---\n", 1)[1]
    icons = [Icon(name, codes[name], "Phosphor", "", outlines[0][name], outlines[1][name])
             for name in mapping]
    return sorted(icons, key=lambda icon: icon.codepoint), {"phosphor": notice}


def rust_data(pack, width, icons, notices):
    out = ["// @generated by scripts/icons/sync-icon-pack.py; do not edit.",
           "// Koru converts the upstream SVG/font artwork into compiled vector paths.",
           "// Upstream path data retains its licenses below. Koru custom paths: MPL-2.0."]
    for source, notice in sorted(notices.items()):
        out.append(f"// {source} upstream notice:")
        out.extend(("// " + line).rstrip() for line in notice.splitlines())
    out += ["use super::vector::IconDef;", f'pub const ACTIVE_PACK: &str = "{pack.title()}";', f"pub const DEFAULT_STROKE_WIDTH: f64 = {float(width)!r};", "#[rustfmt::skip]", f"pub static ICON_DEFS: [IconDef; {len(icons)}] = ["]
    for icon in icons:
        fields = [f"codepoint: '\\u{{{icon.codepoint:X}}}'", f"name: {json.dumps(icon.name)}", f"source: {json.dumps(icon.source)}", f"stroke_d: {json.dumps(icon.stroke)}", f"fill_d: {json.dumps(icon.fill)}", "active_fill_d: " + (f"Some({json.dumps(icon.active_fill)})" if icon.active_fill else "None")]
        out.append("    IconDef { " + ", ".join(fields) + " },")
    return "\n".join(out + ["];", ""])


def bundled_notice(pack, notice):
    version, source, license = {
        "tabler": ("3.31.0", "https://github.com/tabler/tabler-icons", "MIT"),
        "lucide": ("0.477.0", "https://github.com/lucide-icons/lucide", "ISC AND MIT"),
        "remix": ("4.6.0", "https://github.com/Remix-Design/RemixIcon/tree/v4.6.0", "Apache-2.0"),
    }[pack]
    return (f"Component: {pack.title()} Icons (compiled vector artwork)\n"
            f"Version: {version}\nSource: {source}\n"
            "Files: icon_pack_data.rs, document.ico\n"
            f"License: {license}\nPlatforms: windows, macos, linux\n---\n"
            "Koru converts the upstream SVG artwork into compiled vector paths.\n\n" + notice)


def catalog_notices():
    """All bundled SVG packs and their fallbacks, independent of selection."""
    notices = {}
    for pack in PACKS:
        _, pack_notices = load_icons(pack)
        notices.update(pack_notices)
    return notices


def rust_catalog(default, width):
    """Compile every selectable pack; keep the build default first for OS assets."""
    out = ["// @generated by scripts/icons/sync-icon-pack.py; do not edit.",
           f"pub const DEFAULT_PACK: super::vector::IconPack = super::vector::IconPack::{default.title()};",
           f"pub use {default}::{{ACTIVE_PACK, DEFAULT_STROKE_WIDTH, ICON_DEFS}};"]
    for pack in (default, *(p for p in SELECTABLE_PACKS if p != default)):
        icons, notices = load_icons(pack)
        body = rust_data(pack, width, icons, notices).replace(
            "use super::vector::IconDef;", "use super::super::vector::IconDef;")
        # rustfmt indents the module header but preserves the skipped data table.
        header, table = body.split("pub static ICON_DEFS", 1)
        body = "\n".join("    " + line for line in header.splitlines()) + "\npub static ICON_DEFS" + table.rstrip("\n")
        out.extend([f"pub mod {pack} {{", body, "}"])
    return "\n".join(out) + "\n"
