# SPDX-License-Identifier: MPL-2.0
"""Write synthetic OCR fixtures with known text, so accuracy and the numbers in
docs/features/ocr/MEASUREMENTS.md can be reproduced without anyone's private
files.

usage: python -I make_fixtures.py <out-dir>

Writes pictures, PDFs and truth.json ({file name: [text of page 1, ...]}).
Deterministic: fixed seeds, fixed fonts. Fonts default to ones every Windows
install has; override with FIXTURE_FONT_LATIN / FIXTURE_FONT_ZH /
FIXTURE_FONT_JA (paths to .ttf/.ttc).
"""

import io
import json
import os
import random
import sys
import zlib

from PIL import Image, ImageDraw, ImageFilter, ImageFont
from pypdf import PdfReader, PdfWriter

WINFONTS = os.path.join(os.environ.get("WINDIR", "C:\\Windows"), "Fonts")
FONT_LATIN = os.environ.get("FIXTURE_FONT_LATIN", os.path.join(WINFONTS, "arial.ttf"))
FONT_ZH = os.environ.get("FIXTURE_FONT_ZH", os.path.join(WINFONTS, "msyh.ttc"))
FONT_JA = os.environ.get("FIXTURE_FONT_JA", os.path.join(WINFONTS, "YuGothM.ttc"))

EN = [
    "The quarterly invoice is due on the fifteenth of March.",
    "Please send the signed contract to the billing office.",
    "Total amount payable: 1,284.50 EUR including tax.",
    "Reference number 2024-0917 must appear on every payment.",
    "Questions about this statement can be sent by email.",
]
EN2 = [
    "Section two describes the warranty and its conditions.",
    "The device must be returned in its original packaging.",
    "Repairs take between five and ten working days.",
    "Keep this receipt as proof of the purchase date.",
]
ZH = ["今天天气很好，我们去公园散步。", "请在周五之前提交年度报告。", "发票号码和金额必须一致。"]
JA = ["これは日本語の文字認識テストです。", "東京都の天気は晴れのち曇りです。", "会議は午後三時から始まります。"]
PHOTO = ["OPENING HOURS", "Monday to Friday 9:00 - 18:00", "Saturday 10:00 - 14:00", "Closed on Sundays"]
SCREEN = ["Settings", "Account and privacy", "Download the reader to search text in pictures", "Storage used: 3.2 GB of 15 GB"]


def font(path, size):
    return ImageFont.truetype(path, size)


def render_lines(lines, fnt, width, line_h, margin=60, bg=255, fg=0, mode="L"):
    h = margin * 2 + line_h * len(lines)
    img = Image.new(mode, (width, h), bg)
    d = ImageDraw.Draw(img)
    for i, line in enumerate(lines):
        d.text((margin, margin + i * line_h), line, font=fnt, fill=fg)
    return img


def scan_noise(img, seed):
    """A flatbed-scan look: slight blur, speckle, a tiny skew."""
    rng = random.Random(seed)
    img = img.convert("L").rotate(0.6, resample=Image.BICUBIC, expand=True, fillcolor=255)
    img = img.filter(ImageFilter.GaussianBlur(0.6))
    px = img.load()
    w, h = img.size
    for _ in range(w * h // 400):
        x, y = rng.randrange(w), rng.randrange(h)
        px[x, y] = rng.choice((0, 90, 200))
    return img


# ------------------------------------------------------------------ PDF writer

def pdf_escape(s):
    return s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


def write_pdf(path, objects, version="1.4", with_id=True):
    """objects: list of bytes, object 1 must be the catalog."""
    out = bytearray(b"%PDF-" + version.encode() + b"\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    for i, body in enumerate(objects, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % i + body + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    for o in offsets:
        out += b"%010d 00000 n \n" % o
    trailer = b"<< /Size %d /Root 1 0 R" % (len(objects) + 1)
    if with_id:
        trailer += b" /ID [<0123456789abcdef0123456789abcdef><0123456789abcdef0123456789abcdef>]"
    out += b"trailer\n" + trailer + b" >>\nstartxref\n%d\n%%%%EOF\n" % xref
    with open(path, "wb") as f:
        f.write(out)
    return bytes(out)


def stream(data, extra=b""):
    return b"<< /Length %d %s >>\nstream\n" % (len(data), extra) + data + b"\nendstream"


def text_content(lines, font_name=b"/F1", size=12, top=720, leading=16, render_mode=None):
    ops = [b"BT", font_name + b" %d Tf" % size, b"%d TL" % leading, b"72 %d Td" % top]
    if render_mode is not None:
        ops.append(b"%d Tr" % render_mode)
    for i, line in enumerate(lines):
        ops.append(b"(" + pdf_escape(line).encode("cp1252") + b") Tj")
        if i + 1 < len(lines):
            ops.append(b"T*")
    ops.append(b"ET")
    return b"\n".join(ops)


def text_pdf(path, pages, font_obj, version="1.4", with_id=True, render_mode=None):
    """One standard-font text page per entry of `pages` (a list of line lists)."""
    n = len(pages)
    # 1 catalog, 2 pages, 3 font, then (page, content) pairs.
    kids = b" ".join(b"%d 0 R" % (4 + 2 * i) for i in range(n))
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [" + kids + b"] /Count %d >>" % n,
        font_obj,
    ]
    for i, lines in enumerate(pages):
        objs.append(
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources << /Font << /F1 3 0 R >> >> /Contents %d 0 R >>" % (5 + 2 * i)
        )
        objs.append(stream(text_content(lines, render_mode=render_mode)))
    return write_pdf(path, objs, version, with_id)


def type3_unmapped_pdf(path, lines):
    """Text drawn with a Type3 font whose glyph names are not Unicode names and
    which has no ToUnicode map: every glyph is unmapped, as in some old or
    generated files."""
    used = sorted(set("".join(lines)))
    codes = {c: 33 + i for i, c in enumerate(used)}
    diffs = b" ".join(b"%d /g%d" % (codes[c], codes[c]) for c in used)
    procs = b" ".join(b"/g%d 5 0 R" % codes[c] for c in used)
    first, last = min(codes.values()), max(codes.values())
    widths = b" ".join(b"600" for _ in range(first, last + 1))
    font3 = (
        b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 600 700] /FontMatrix [0.001 0 0 0.001 0 0] "
        b"/CharProcs << " + procs + b" >> /Encoding << /Type /Encoding /Differences [" + diffs + b"] >> "
        b"/FirstChar %d /LastChar %d /Widths [" % (first, last) + widths + b"] /Resources << >> >>"
    )
    box = stream(b"600 0 0 0 600 700 d1\n50 0 500 650 re f")
    content_ops = [b"BT", b"/F1 12 Tf", b"16 TL", b"72 720 Td"]
    for i, line in enumerate(lines):
        content_ops.append(b"<" + b"".join(b"%02x" % codes[c] for c in line) + b"> Tj")
        if i + 1 < len(lines):
            content_ops.append(b"T*")
    content_ops.append(b"ET")
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>",
        font3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents 6 0 R >>",
        box,
        stream(b"\n".join(content_ops)),
    ]
    write_pdf(path, objs)


def image_page_pdf(path, img, overlay_lines=None):
    """One page holding a grayscale image, optionally with invisible text over
    it (render mode 3) - the shape of a scan that some tool already OCR'd."""
    w, h = img.size
    raw = zlib.compress(img.convert("L").tobytes(), 9)
    pw, ph = 612, int(612 * h / w)
    content = b"q %d 0 0 %d 0 0 cm /Im1 Do Q" % (pw, ph)
    res = b"/XObject << /Im1 5 0 R >>"
    if overlay_lines:
        content += b"\n" + text_content(overlay_lines, top=ph - 72, render_mode=3)
        res += b" /Font << /F1 6 0 R >>"
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 %d %d] /Resources << %s >> /Contents 4 0 R >>" % (pw, ph, res),
        stream(content),
        stream(raw, b"/Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode" % (w, h)),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    ]
    write_pdf(path, objs)


def huge_image_pdf(path, side=20000):
    """A page with one side x side 8-bit image that compresses to almost
    nothing: tiny on disk, hundreds of MB once decoded."""
    co = zlib.compressobj(9)
    row = bytes(side)
    raw = b"".join(co.compress(row) for _ in range(side)) + co.flush()
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>",
        stream(b"q 612 0 0 792 0 0 cm /Im1 Do Q"),
        stream(raw, b"/Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode" % (side, side)),
    ]
    write_pdf(path, objs)


def pages_cycle_pdf(path):
    """A page tree whose /Kids contains its own /Pages node."""
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 2 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
        stream(b"0 0 m 100 100 l S"),
    ]
    write_pdf(path, objs)


def form_recursion_pdf(path):
    """A form XObject that draws itself, and a content stream of 200,000
    nested q operators."""
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Fx 5 0 R >> >> /Contents 4 0 R >>",
        stream(b"q " * 200000 + b"/Fx Do " + b"Q " * 200000),
        stream(b"0.9 0 0 0.9 10 10 cm /Fx Do", b"/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Resources << /XObject << /Fx 5 0 R >> >>"),
    ]
    write_pdf(path, objs)


HELVETICA = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
TIMES_NO_ENCODING = b"<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>"


def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    truth = {}
    p = lambda name: os.path.join(out, name)

    # --- pictures
    f_en = font(FONT_LATIN, 34)
    img = render_lines(EN, f_en, 1400, 52)
    img.save(p("text-en.png"))
    img.convert("RGB").save(p("text-en.jpg"), quality=85)
    truth["text-en.png"] = truth["text-en.jpg"] = ["\n".join(EN)]

    small = render_lines(EN2, font(FONT_LATIN, 15), 700, 24, margin=20)
    small.save(p("text-small.png"))
    truth["text-small.png"] = ["\n".join(EN2)]

    f_zh, f_ja = font(FONT_ZH, 40), font(FONT_JA, 40)
    zh = render_lines(ZH, f_zh, 1000, 64)
    ja = render_lines(JA, f_ja, 1000, 64)
    cjk = Image.new("L", (1000, zh.height + ja.height), 255)
    cjk.paste(zh, (0, 0))
    cjk.paste(ja, (0, zh.height))
    cjk.save(p("text-cjk.png"))
    truth["text-cjk.png"] = ["\n".join(ZH + JA)]

    # A photographed sign: colored, gradient, noise, rotated, blurred, JPEG.
    rng = random.Random(7)
    w, h = 1600, 1000
    photo = Image.new("RGB", (w, h))
    px = photo.load()
    for y in range(h):
        for x in range(0, w):
            v = rng.randrange(-12, 12)
            px[x, y] = (min(255, max(0, 180 + y // 12 + v)), min(255, max(0, 150 + x // 20 + v)), min(255, max(0, 110 + v)))
    d = ImageDraw.Draw(photo)
    d.rectangle((250, 220, 1350, 780), fill=(245, 242, 230))
    f_big, f_mid = font(FONT_LATIN, 64), font(FONT_LATIN, 44)
    d.text((320, 270), PHOTO[0], font=f_big, fill=(30, 30, 90))
    for i, line in enumerate(PHOTO[1:]):
        d.text((320, 420 + i * 100), line, font=f_mid, fill=(20, 20, 20))
    photo = photo.rotate(8, resample=Image.BICUBIC, fillcolor=(90, 80, 70)).filter(ImageFilter.GaussianBlur(1.1))
    photo.save(p("photo-rotated.jpg"), quality=70)
    truth["photo-rotated.jpg"] = ["\n".join(PHOTO)]

    # A photograph with no text at all.
    blank = Image.new("RGB", (1200, 900))
    bp = blank.load()
    for y in range(900):
        for x in range(1200):
            v = rng.randrange(-20, 20)
            bp[x, y] = (min(255, max(0, 60 + x // 10 + v)), min(255, max(0, 120 + v)), min(255, max(0, 160 + y // 9 + v)))
    blank = blank.filter(ImageFilter.GaussianBlur(3))
    blank.save(p("photo-no-text.jpg"), quality=80)
    truth["photo-no-text.jpg"] = [""]

    # A screenshot-like picture.
    shot = Image.new("RGB", (1280, 720), (243, 244, 246))
    d = ImageDraw.Draw(shot)
    d.rectangle((0, 0, 1280, 64), fill=(32, 33, 36))
    d.text((24, 14), SCREEN[0], font=font(FONT_LATIN, 30), fill=(255, 255, 255))
    d.rectangle((40, 110, 1240, 640), fill=(255, 255, 255), outline=(220, 220, 220))
    d.text((80, 150), SCREEN[1], font=font(FONT_LATIN, 28), fill=(20, 20, 20))
    d.text((80, 230), SCREEN[2], font=font(FONT_LATIN, 22), fill=(60, 60, 60))
    d.text((80, 300), SCREEN[3], font=font(FONT_LATIN, 22), fill=(60, 60, 60))
    shot.save(p("screenshot.png"))
    truth["screenshot.png"] = ["\n".join(SCREEN)]

    # --- PDFs with a real text layer
    text_pdf(p("text-layer.pdf"), [EN, EN2, EN[:2] + EN2[:2]], HELVETICA)
    truth["text-layer.pdf"] = ["\n".join(EN), "\n".join(EN2), "\n".join(EN[:2] + EN2[:2])]

    # PDF 1.1 shape: no /ID, non-embedded Times without an /Encoding.
    text_pdf(p("text-layer-old-1.1.pdf"), [EN2], TIMES_NO_ENCODING, version="1.1", with_id=False)
    truth["text-layer-old-1.1.pdf"] = ["\n".join(EN2)]

    long_pages = [[f"Page {i + 1} of the long manual."] + EN[i % 5 : i % 5 + 1] + EN2[i % 4 : i % 4 + 1] for i in range(300)]
    text_pdf(p("text-layer-300-pages.pdf"), long_pages, HELVETICA)
    truth["text-layer-300-pages.pdf"] = ["\n".join(lp) for lp in long_pages]

    type3_unmapped_pdf(p("text-layer-unmapped.pdf"), EN2)
    truth["text-layer-unmapped.pdf"] = ["\n".join(EN2)]

    # --- scanned PDFs (images only), and a scan with an invisible OCR layer
    s1 = scan_noise(render_lines(EN, font(FONT_LATIN, 30), 1700, 46, margin=120), 1)
    s2 = scan_noise(render_lines(EN2, font(FONT_LATIN, 30), 1700, 46, margin=120), 2)
    s1.save(p("scan-2-pages.pdf"), save_all=True, append_images=[s2], resolution=150.0)
    truth["scan-2-pages.pdf"] = ["\n".join(EN), "\n".join(EN2)]

    image_page_pdf(p("scan-with-invisible-text.pdf"), s1, overlay_lines=EN)
    truth["scan-with-invisible-text.pdf"] = ["\n".join(EN)]

    # --- encrypted
    for name, kw in (
        ("encrypted-user-password-rc4.pdf", dict(user_password="secret", owner_password="owner", algorithm="RC4-128")),
        ("encrypted-user-password-aes256.pdf", dict(user_password="secret", owner_password="owner", algorithm="AES-256")),
        ("encrypted-owner-only-aes128.pdf", dict(user_password="", owner_password="owner", algorithm="AES-128")),
    ):
        wr = PdfWriter(clone_from=PdfReader(p("text-layer.pdf")))
        wr.encrypt(**kw)
        with open(p(name), "wb") as f:
            wr.write(f)
    truth["encrypted-owner-only-aes128.pdf"] = truth["text-layer.pdf"]

    # --- damaged
    data = open(p("text-layer.pdf"), "rb").read()
    open(p("damaged-truncated-60pct.pdf"), "wb").write(data[: len(data) * 6 // 10])
    scan_data = open(p("scan-2-pages.pdf"), "rb").read()
    open(p("damaged-scan-truncated-50pct.pdf"), "wb").write(scan_data[: len(scan_data) // 2])
    bad = bytearray(data)
    xi = bad.rfind(b"xref\n")
    for i in range(xi + 30, min(len(bad) - 40, xi + 30 + 18 * 6)):
        if chr(bad[i]).isdigit():
            bad[i] = ord("9")
    open(p("damaged-bad-xref.pdf"), "wb").write(bytes(bad))
    truth["damaged-bad-xref.pdf"] = truth["text-layer.pdf"]
    grng = random.Random(3)
    open(p("damaged-garbage.pdf"), "wb").write(b"%PDF-1.4\n" + bytes(grng.randrange(256) for _ in range(65536)))

    # --- hostile: a decompression bomb of an image
    huge_image_pdf(p("hostile-huge-image.pdf"))
    pages_cycle_pdf(p("hostile-pages-cycle.pdf"))
    form_recursion_pdf(p("hostile-form-recursion.pdf"))

    with open(p("truth.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(truth, f, ensure_ascii=False, indent=1, sort_keys=True)
    for name in sorted(os.listdir(out)):
        print(f"  {os.path.getsize(p(name)):>10}  {name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
