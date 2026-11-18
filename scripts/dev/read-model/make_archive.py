# SPDX-License-Identifier: MPL-2.0
"""Write a generated Evernote export (.enex) of a few thousand notes, each with
one picture or PDF attached that carries made-up words, for the third
milestone's big run (OCR ROADMAP Plan 3.4, UC11 and UC12).

No real archive of that size exists to test with, and the Evernote importer
is the path the first audience arrives by, so this one is generated. It
imports through the real importer (`koru-import-evernote`): the shape is the
one that crate's own tests build, with the export header a real export
carries.

usage: python -I -B make_archive.py <out-dir> [notes] [seed]

  notes   how many notes (default 3000)
  seed    the seed everything is drawn from (default 1). The same seed gives
          the same bytes, encrypted files included.

Run it with the Python of the throwaway environment make-fixtures.sh builds
(<out>/venv-fixtures, Pillow and pypdf from fixtures.lock).

Writes, under <out-dir>:

  generated.enex   the archive
  words.json       for each note: the kind of its attachment, the MD5 of the
                   attachment's bytes (what the archive names it by), its
                   file name, and the made-up word on each page or picture,
                   in page order; plus the word every text page shares and a
                   word that is on no page. A run reads it to check what was
                   read; nothing else does.

The mix, as shares of the notes (the rest are text PDFs):

  screenshot      30%  a PNG screenshot with one made-up word
  photo-sign       6%  a photographed sign (JPEG, tilted, blurred), one word
  photo            8%  a photograph with no text in it
  text-pdf        ~32% a PDF with a real text layer, 1 to 4 pages
  scan-pdf         8%  a scanned PDF (pictures only), 2 to 4 pages
  password-pdf   0.5%  a PDF with real text behind a user password
  damaged-pdf    0.5%  a text PDF cut off part-way through
  duplicate        8%  the same bytes as an earlier note's attachment
  long-text-pdf    2   300-page text PDFs
  long-scan-pdf    1   a 210-page scan: the reader stops at 200 pages

Every made-up word is ten letters, consonant then vowel, and never repeats.
"""

import base64
import contextlib
import datetime
import hashlib
import json
import os
import random
import secrets
import sys
import time

from PIL import Image, ImageDraw, ImageFilter
from pypdf import PdfReader, PdfWriter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import make_fixtures as fx  # noqa: E402

CONSONANTS = "bdfgklmnprstvz"
VOWELS = "aeiou"
# On every text page, scanned page and screenshot: a word found on many pages.
COMMON = "reference"
FIXED_DATE = datetime.datetime(2024, 1, 1, 0, 0, 0)
# The same instant, in the form Pillow writes into a PDF's dates.
FIXED_PDF_DATE = time.gmtime(1704067200)

SHARES = [
    ("screenshot", 0.30),
    ("photo-sign", 0.06),
    ("photo", 0.08),
    ("scan-pdf", 0.08),
    ("password-pdf", 0.005),
    ("damaged-pdf", 0.005),
    ("duplicate", 0.08),
]
FIXED = [("long-text-pdf", 2), ("long-scan-pdf", 1)]


class Words:
    def __init__(self, rng):
        self.rng = rng
        self.taken = set()

    def new(self):
        while True:
            word = "".join(self.rng.choice(CONSONANTS) + self.rng.choice(VOWELS) for _ in range(5))
            if word not in self.taken:
                self.taken.add(word)
                return word


@contextlib.contextmanager
def seeded_secrets(rng):
    """pypdf draws an encrypted file's salt, IV and /ID from `secrets`; draw
    them from the seed instead, so the archive is the same on every run."""
    original = secrets.token_bytes
    secrets.token_bytes = lambda n=32: rng.randbytes(n)
    try:
        yield
    finally:
        secrets.token_bytes = original


def noise(rng, size, spread):
    """A grey noise layer from the seed (fast, and the same every run)."""
    w, h = size
    return Image.frombytes("L", (w, h), rng.randbytes(w * h)).point(lambda v: 128 - spread + v * 2 * spread // 255)


def gradient(size, a, b):
    w, h = size
    horizontal = Image.linear_gradient("L").rotate(90).resize((w, h))
    vertical = Image.linear_gradient("L").resize((w, h))
    return Image.merge(
        "RGB",
        (
            horizontal.point(lambda v: a[0] + v * (b[0] - a[0]) // 255),
            vertical.point(lambda v: a[1] + v * (b[1] - a[1]) // 255),
            horizontal.point(lambda v: a[2] + v * (b[2] - a[2]) // 255),
        ),
    )


def with_noise(img, rng, spread):
    n = noise(rng, img.size, spread)
    channels = [Image.blend(c, n, 0.25) for c in img.split()]
    return Image.merge("RGB", channels)


def encode(img, fmt, **kw):
    import io

    buf = io.BytesIO()
    img.save(buf, fmt, **kw)
    return buf.getvalue()


def screenshot(rng, word, n):
    shot = Image.new("RGB", (1280, 720), (243, 244, 246))
    d = ImageDraw.Draw(shot)
    d.rectangle((0, 0, 1280, 64), fill=(32, 33, 36))
    d.text((24, 14), f"Window {n}", font=fx.font(fx.FONT_LATIN, 30), fill=(255, 255, 255))
    d.rectangle((40, 110, 1240, 640), fill=(255, 255, 255), outline=(220, 220, 220))
    d.text((80, 150), fx.SCREEN[1], font=fx.font(fx.FONT_LATIN, 28), fill=(20, 20, 20))
    d.text((80, 230), f"The {COMMON} code {word} is ready", font=fx.font(fx.FONT_LATIN, 28), fill=(40, 40, 40))
    d.text((80, 300), fx.SCREEN[3], font=fx.font(fx.FONT_LATIN, 22), fill=(60, 60, 60))
    return encode(shot, "PNG")


def photo_sign(rng, word):
    w, h = 1000, 700
    photo = with_noise(gradient((w, h), (170, 140, 100), (210, 190, 130)), rng, 30)
    d = ImageDraw.Draw(photo)
    d.rectangle((150, 170, 850, 530), fill=(245, 242, 230))
    d.text((200, 220), "NOTICE", font=fx.font(fx.FONT_LATIN, 56), fill=(30, 30, 90))
    d.text((200, 330), word.upper(), font=fx.font(fx.FONT_LATIN, 48), fill=(20, 20, 20))
    d.text((200, 420), "Open daily", font=fx.font(fx.FONT_LATIN, 36), fill=(20, 20, 20))
    angle = rng.uniform(-7, 7)
    photo = photo.rotate(angle, resample=Image.BICUBIC, fillcolor=(90, 80, 70)).filter(ImageFilter.GaussianBlur(0.9))
    return encode(photo, "JPEG", quality=75)


def photo(rng):
    w, h = 800, 600
    a = tuple(rng.randrange(30, 120) for _ in range(3))
    b = tuple(rng.randrange(130, 230) for _ in range(3))
    img = with_noise(gradient((w, h), a, b), rng, 40).filter(ImageFilter.GaussianBlur(3))
    return encode(img, "JPEG", quality=80)


def text_pages(words, n):
    return [
        [f"Note {n}, page {i + 1}.", f"The {COMMON} word is {word} on this page.", fx.EN[(n + i) % len(fx.EN)]]
        for i, word in enumerate(words)
    ]


# make_fixtures' PDF writer writes a file as it goes; this one lives in the
# output directory and is removed at the end.
SCRATCH = None


def text_pdf(words, n):
    return fx.text_pdf(SCRATCH, text_pages(words, n), fx.HELVETICA)


SCAN_FONT = None


def scan_pdf(rng, words, n):
    global SCAN_FONT
    if SCAN_FONT is None:
        SCAN_FONT = fx.font(fx.FONT_LATIN, 30)
    images = []
    for i, word in enumerate(words):
        lines = [
            f"Note {n}, scanned page {i + 1}.",
            f"Its {COMMON} word is {word} today.",
            fx.EN2[(n + i) % len(fx.EN2)],
            fx.EN[(n + i) % len(fx.EN)],
        ]
        # A letter-sized sheet at 150 dpi, the text in its upper part, so a
        # page costs the reader what a scanned page does.
        sheet = Image.new("L", (1240, 1650), 255)
        sheet.paste(fx.render_lines(lines, SCAN_FONT, 1240, 46, margin=120), (0, 0))
        images.append(fx.scan_noise(sheet, rng.randrange(1 << 30)))
    return encode(
        images[0],
        "PDF",
        save_all=True,
        append_images=images[1:],
        resolution=150.0,
        creationDate=FIXED_PDF_DATE,
        modDate=FIXED_PDF_DATE,
        producer="generated",
        creator="generated",
    )


def password_pdf(rng, word, n):
    plain = text_pdf([word], n)
    import io

    writer = PdfWriter(clone_from=PdfReader(io.BytesIO(plain)))
    with seeded_secrets(rng):
        writer.encrypt(user_password="secret", owner_password="owner", algorithm="AES-256")
        buf = io.BytesIO()
        writer.write(buf)
    return buf.getvalue()


def damaged_pdf(words, n):
    data = text_pdf(words, n)
    return data[: len(data) * 55 // 100]


def plan(rng, notes):
    kinds = []
    for kind, share in SHARES:
        kinds += [kind] * max(1, round(notes * share))
    for kind, count in FIXED:
        kinds += [kind] * count
    kinds += ["text-pdf"] * max(0, notes - len(kinds))
    kinds = kinds[:notes]
    rng.shuffle(kinds)
    # A duplicate needs an earlier attachment to copy: never the first.
    if kinds and kinds[0] == "duplicate":
        swap = next(i for i, k in enumerate(kinds) if k != "duplicate")
        kinds[0], kinds[swap] = kinds[swap], kinds[0]
    return kinds


MIME = {"png": "image/png", "jpg": "image/jpeg", "pdf": "application/pdf"}


def attachment(rng, words, kind, n):
    """(bytes, extension, made-up word per page)"""
    if kind == "screenshot":
        w = words.new()
        return screenshot(rng, w, n), "png", [w]
    if kind == "photo-sign":
        w = words.new()
        return photo_sign(rng, w), "jpg", [w]
    if kind == "photo":
        return photo(rng), "jpg", []
    if kind == "text-pdf":
        ws = [words.new() for _ in range(rng.randint(1, 4))]
        return text_pdf(ws, n), "pdf", ws
    if kind == "scan-pdf":
        ws = [words.new() for _ in range(rng.randint(2, 4))]
        return scan_pdf(rng, ws, n), "pdf", ws
    if kind == "long-text-pdf":
        ws = [words.new() for _ in range(300)]
        return text_pdf(ws, n), "pdf", ws
    if kind == "long-scan-pdf":
        ws = [words.new() for _ in range(210)]
        return scan_pdf(rng, ws, n), "pdf", ws
    if kind == "password-pdf":
        w = words.new()
        return password_pdf(rng, w, n), "pdf", [w]
    if kind == "damaged-pdf":
        ws = [words.new() for _ in range(3)]
        return damaged_pdf(ws, n), "pdf", ws
    raise ValueError(kind)


def note_xml(n, title, data, ext, md5, filename):
    mime = MIME[ext]
    stamp = (FIXED_DATE + datetime.timedelta(minutes=n)).strftime("%Y%m%dT%H%M%SZ")
    body = (
        '<?xml version="1.0" encoding="UTF-8"?>'
        '<!DOCTYPE en-note SYSTEM "http://xml.evernote.com/pub/enml2.dtd">'
        f"<en-note><div>Generated note {n}.</div>"
        f'<div><en-media type="{mime}" hash="{md5}"/></div></en-note>'
    )
    attach = "" if ext != "pdf" else "<attachment>true</attachment>"
    return (
        f"  <note>\n    <title>{title}</title>\n"
        f"    <content><![CDATA[{body}]]></content>\n"
        f"    <created>{stamp}</created>\n    <updated>{stamp}</updated>\n"
        f'    <resource>\n      <data encoding="base64">{base64.b64encode(data).decode("ascii")}</data>\n'
        f"      <mime>{mime}</mime>\n"
        f"      <resource-attributes><file-name>{filename}</file-name>{attach}</resource-attributes>\n"
        f"    </resource>\n  </note>\n"
    )


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    out = sys.argv[1]
    notes = int(sys.argv[2]) if len(sys.argv) > 2 else 3000
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else 1
    os.makedirs(out, exist_ok=True)
    global SCRATCH
    SCRATCH = os.path.join(out, "scratch.pdf")
    rng = random.Random(seed)
    words = Words(rng)
    kinds = plan(rng, notes)
    made = []  # (bytes, ext, words, kind) of every original attachment
    record = []
    totals = {}
    with open(os.path.join(out, "generated.enex"), "w", encoding="utf-8", newline="\n") as f:
        f.write('<?xml version="1.0" encoding="UTF-8"?>\n')
        f.write('<!DOCTYPE en-export SYSTEM "http://xml.evernote.com/pub/evernote-export3.dtd">\n')
        f.write('<en-export export-date="20240101T000000Z" application="Evernote" version="10">\n')
        for n, kind in enumerate(kinds):
            duplicate_of = None
            if kind == "duplicate":
                duplicate_of = rng.randrange(len(made))
                data, ext, page_words, original_kind = made[duplicate_of]
            else:
                data, ext, page_words = attachment(rng, words, kind, n)
                original_kind = kind
                made.append((data, ext, page_words, kind))
            md5 = hashlib.md5(data).hexdigest()
            filename = f"{original_kind}-{n:05d}.{ext}"
            f.write(note_xml(n, f"Generated note {n:05d}", data, ext, md5, filename))
            record.append(
                {
                    "note": n,
                    "kind": original_kind,
                    "duplicate": duplicate_of is not None,
                    "md5": md5,
                    "bytes": len(data),
                    "file": filename,
                    "words": page_words,
                }
            )
            key = "duplicate" if duplicate_of is not None else original_kind
            count, size = totals.get(key, (0, 0))
            totals[key] = (count + 1, size + len(data))
            if (n + 1) % 250 == 0:
                print(f"  {n + 1} of {notes} notes", flush=True)
        f.write("</en-export>\n")
    if os.path.exists(SCRATCH):
        os.remove(SCRATCH)
    absent = words.new()
    with open(os.path.join(out, "words.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(
            {"seed": seed, "notes": notes, "common": COMMON, "absent": absent, "attachments": record},
            f,
            indent=0,
        )
    print(f"notes {notes}, seed {seed}")
    for key in sorted(totals):
        count, size = totals[key]
        print(f"  {key:<14} {count:>6} attachments {size:>12} bytes")
    print(f"  generated.enex {os.path.getsize(os.path.join(out, 'generated.enex')):>12} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
