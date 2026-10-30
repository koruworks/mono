# SPDX-License-Identifier: MPL-2.0
"""Bounded SVG centerline renderer for the generated DOCUMENT and SPARKLE paths.

Supports M/L/H/V/C/A/Z (both cases), repetitions, compact numbers and subpaths.
Curves are flattened within 0.25 supersampled pixels, then stroked with round
caps/joins at 4x resolution. This is an offline asset tool, not an SVG engine.
Unknown syntax is an error, never silently omitted artwork.
"""
import json
import math
import re

from icon_pack import ROOT

TOKEN = re.compile(r"[A-Za-z]|[-+]?(?:\d*\.\d+|\d+\.?\d*)(?:[eE][-+]?\d+)?")
ARITY = {"M": 2, "L": 2, "H": 1, "V": 1, "C": 6, "A": 7}


def arc_points(start, args, relative, tolerance):
    """SVG endpoint-to-center conversion, including radii correction."""
    rx, ry, rotation, large, sweep, x, y = args
    if large not in (0, 1) or sweep not in (0, 1):
        raise ValueError("arc flags must be 0 or 1")
    end = (x + start[0], y + start[1]) if relative else (x, y)
    if end == start:
        return []
    rx, ry = abs(rx), abs(ry)
    if rx == 0 or ry == 0:
        return [end]
    phi = math.radians(rotation % 360)
    cos, sin = math.cos(phi), math.sin(phi)
    dx, dy = (start[0] - end[0]) / 2, (start[1] - end[1]) / 2
    xp, yp = cos * dx + sin * dy, -sin * dx + cos * dy
    ratio = (xp / rx)**2 + (yp / ry)**2
    if ratio > 1:
        factor = math.sqrt(ratio)
        rx, ry = rx * factor, ry * factor
    denominator = (rx * yp)**2 + (ry * xp)**2
    factor = math.sqrt(max(0, ((rx * ry)**2 - denominator) / denominator))
    if large == sweep:
        factor = -factor
    cxp, cyp = factor * rx * yp / ry, -factor * ry * xp / rx
    cx = cos * cxp - sin * cyp + (start[0] + end[0]) / 2
    cy = sin * cxp + cos * cyp + (start[1] + end[1]) / 2
    theta = math.atan2((yp - cyp) / ry, (xp - cxp) / rx)
    stop = math.atan2((-yp - cyp) / ry, (-xp - cxp) / rx)
    delta = (stop - theta) % math.tau
    if not sweep:
        delta -= math.tau
    step = 2 * math.acos(max(-1, 1 - min(tolerance / max(rx, ry), 1)))
    count = max(1, math.ceil(abs(delta) / step))
    if count > 100000:
        raise ValueError("arc exceeds bounded renderer budget")
    points = []
    for i in range(1, count + 1):
        a = theta + delta * i / count
        points.append((cx + cos * rx * math.cos(a) - sin * ry * math.sin(a),
                       cy + sin * rx * math.cos(a) + cos * ry * math.sin(a)))
    points[-1] = end
    return points


def cubic_points(start, control1, control2, end, tolerance, depth=0):
    def distance(point):
        dx, dy = end[0] - start[0], end[1] - start[1]
        length = dx * dx + dy * dy
        t = max(0, min(1, ((point[0]-start[0])*dx + (point[1]-start[1])*dy) / length)) if length else 0
        return math.hypot(point[0] - start[0] - t*dx, point[1] - start[1] - t*dy)
    if max(distance(control1), distance(control2)) <= tolerance:
        return [end]
    if depth == 20:
        raise ValueError("cubic exceeds bounded renderer budget")
    def midpoint(a, b):
        return ((a[0]+b[0])/2, (a[1]+b[1])/2)
    a, b, c = midpoint(start, control1), midpoint(control1, control2), midpoint(control2, end)
    d, e = midpoint(a, b), midpoint(b, c)
    center = midpoint(d, e)
    return (cubic_points(start, a, d, center, tolerance, depth+1) +
            cubic_points(center, e, c, end, tolerance, depth+1))


def flatten(path, tolerance=0.005):
    if not math.isfinite(tolerance) or tolerance <= 0:
        raise ValueError("tolerance must be positive and finite")
    if TOKEN.sub("", path).strip(" ,\t\r\n"):
        raise ValueError("invalid path syntax")
    tokens = TOKEN.findall(path)
    i, command, point, origin = 0, None, (0.0, 0.0), None
    paths, current = [], None
    while i < len(tokens):
        if tokens[i].isalpha():
            command = tokens[i]
            i += 1
            if command.upper() == "Z":
                if current is None:
                    raise ValueError("close before move")
                current.append(origin)
                point = origin
                command = None
                continue
        if command is None or command.upper() not in ARITY:
            raise ValueError(f"unsupported or missing path command: {command}")
        op, relative = command.upper(), command.islower()
        n = ARITY[op]
        if i+n > len(tokens) or any(t.isalpha() for t in tokens[i:i+n]):
            raise ValueError(f"incomplete {command} command")
        args = list(map(float, tokens[i:i+n]))
        if any(not math.isfinite(v) or abs(v) > 1e6 for v in args):
            raise ValueError("path coordinate outside finite renderer bounds")
        i += n
        def xy(x, y):
            return (x+point[0], y+point[1]) if relative else (x, y)
        if op == "M":
            point = xy(*args)
            origin, current = point, [point]
            paths.append(current)
            command = "l" if relative else "L"
            continue
        if current is None:
            raise ValueError("path must start with move")
        if op == "L":
            points = [xy(*args)]
        elif op == "H":
            points = [(args[0]+point[0] if relative else args[0], point[1])]
        elif op == "V":
            points = [(point[0], args[0]+point[1] if relative else args[0])]
        elif op == "C":
            points = cubic_points(point, xy(*args[:2]), xy(*args[2:4]), xy(*args[4:]), tolerance)
        else:
            points = arc_points(point, args, relative, tolerance)
        if points:
            point = points[-1]
            current.extend(points)
    if not paths or not any(len(p) > 1 for p in paths):
        raise ValueError("path has no drawable segments")
    return paths


def generated_icon(name, source=None):
    """Read the committed data used by Rust; no archive or font is needed."""
    if source is None:
        source = (ROOT / "crates/koru-draw/src/icon_pack_data.rs").read_text(encoding="utf-8")
    literal = r'"(?:[^"\\]|\\.)*"'
    row = next((line for line in source.splitlines() if f'name: "{name}"' in line), None)
    if row is None:
        raise ValueError(f"generated catalog lacks {name}")
    stroke = json.loads(re.search(r"stroke_d: ("+literal+r")", row)[1])
    fill = json.loads(re.search(r"fill_d: ("+literal+r")", row)[1])
    if fill:
        raise ValueError(f"{name}: taskbar renderer supports centerlines only")
    width = float(re.search(r"DEFAULT_STROKE_WIDTH: f64 = ([^;]+);", source)[1])
    return stroke, width


def render(path, stroke_width, size=256, ink=(200, 200, 204, 255)):
    from PIL import Image, ImageDraw
    if not 0 < stroke_width <= 4 or not math.isfinite(stroke_width):
        raise ValueError("stroke width must be finite and in (0, 4]")
    if not isinstance(size, int) or not 1 <= size <= 1024:
        raise ValueError("size must be an integer in [1, 1024]")
    scale = size * 4 / 24
    paths = flatten(path, 0.25 / scale)
    image = Image.new("RGBA", (size*4, size*4), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    radius = stroke_width * scale / 2
    for path in paths:
        points = [(x*scale, y*scale) for x, y in path]
        draw.line(points, fill=ink, width=max(1, round(radius*2)))
        for x, y in points:
            draw.ellipse((x-radius, y-radius, x+radius, y+radius), fill=ink)
    return image.resize((size, size), Image.Resampling.LANCZOS)


def render_icon(name, size=256, ink=(200, 200, 204, 255)):
    path, width = generated_icon(name)
    return render(path, width, size, ink)
