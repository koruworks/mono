# SPDX-License-Identifier: MPL-2.0
"""Write the generated cases for the second milestone's acceptance run (OCR
UC6 to UC10): pictures, a scan and PDFs that carry made-up words **invented
afresh on every run**, so a pass cannot come from a word the reader, the index
or a previous run already knew.

usage: python -I make_cases.py <out-dir> [scan-pages]

Run it with the Python of the throwaway environment make-fixtures.sh builds
(<out>/venv-fixtures, Pillow and pypdf from fixtures.lock). The cases are
consumed by apps/koru-store-server/examples/reader_acceptance.rs.

Writes, under <out-dir>:

  uc6-named.png      a screenshot with one made-up word (pasted into a page,
                     and given a name there)
  uc6-unnamed.png    a second screenshot with another made-up word (pasted,
                     never named)
  uc7-scan.pdf       a scanned PDF (pictures only, no text layer) of
                     <scan-pages> pages (default 130), each with its own word
  uc10-password.pdf  a PDF with real text, behind a user password
  uc10-photo.jpg     a photograph with no text in it
  words.json         which word is where; the run reads it, nothing else does

The drawing helpers are make_fixtures.py's, so the cases look like the fixtures
the reader was measured on. Fonts as there (FIXTURE_FONT_LATIN overrides).
"""

import json
import os
import random
import sys

from PIL import Image, ImageDraw, ImageFilter
from pypdf import PdfReader, PdfWriter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import make_fixtures as fx  # noqa: E402

CONSONANTS = "bdfgklmnprstvz"
VOWELS = "aeiou"


def made_up_word(rng, taken):
    """Ten lowercase letters, consonant then vowel: pronounceable, in no
    dictionary, and read cleanly by a recognizer. Never the same twice."""
    while True:
        word = "".join(rng.choice(CONSONANTS) + rng.choice(VOWELS) for _ in range(5))
        if word not in taken:
            taken.add(word)
            return word


def screenshot(path, heading, line):
    shot = Image.new("RGB", (1280, 720), (243, 244, 246))
    d = ImageDraw.Draw(shot)
    d.rectangle((0, 0, 1280, 64), fill=(32, 33, 36))
    d.text((24, 14), heading, font=fx.font(fx.FONT_LATIN, 30), fill=(255, 255, 255))
    d.rectangle((40, 110, 1240, 640), fill=(255, 255, 255), outline=(220, 220, 220))
    d.text((80, 150), fx.SCREEN[1], font=fx.font(fx.FONT_LATIN, 28), fill=(20, 20, 20))
    d.text((80, 230), line, font=fx.font(fx.FONT_LATIN, 28), fill=(40, 40, 40))
    d.text((80, 300), fx.SCREEN[3], font=fx.font(fx.FONT_LATIN, 22), fill=(60, 60, 60))
    shot.save(path)


def main():
    out = sys.argv[1]
    pages = int(sys.argv[2]) if len(sys.argv) > 2 else 130
    os.makedirs(out, exist_ok=True)
    p = lambda name: os.path.join(out, name)
    rng = random.Random(int.from_bytes(os.urandom(8), "little"))
    taken = set()
    words = {}

    # UC6: two screenshots, each with its own word.
    words["uc6_named"] = made_up_word(rng, taken)
    screenshot(p("uc6-named.png"), "Settings", f"Reference code {words['uc6_named']} for this account")
    words["uc6_unnamed"] = made_up_word(rng, taken)
    screenshot(p("uc6-unnamed.png"), "Storage", f"Backup label {words['uc6_unnamed']} is ready")

    # UC7: a scan, one word per page.
    f_scan = fx.font(fx.FONT_LATIN, 30)
    images = []
    words["uc7_pages"] = {}
    for n in range(1, pages + 1):
        word = made_up_word(rng, taken)
        words["uc7_pages"][str(n)] = word
        lines = [f"Page {n} of the scanned manual.", f"Its reference word is {word} today.", fx.EN2[n % 4]]
        images.append(fx.scan_noise(fx.render_lines(lines, f_scan, 1700, 46, margin=120), n))
    images[0].save(p("uc7-scan.pdf"), save_all=True, append_images=images[1:], resolution=150.0)

    # UC10: real text behind a password, and a photograph with no text.
    words["uc10_password"] = made_up_word(rng, taken)
    plain = p("uc10-password.plain.pdf")
    fx.text_pdf(plain, [[f"Contract reference {words['uc10_password']}.", fx.EN[0]]], fx.HELVETICA)
    writer = PdfWriter(clone_from=PdfReader(plain))
    writer.encrypt(user_password="secret", owner_password="owner", algorithm="AES-256")
    with open(p("uc10-password.pdf"), "wb") as f:
        writer.write(f)
    os.remove(plain)

    seed = rng.randrange(1 << 30)
    noise = random.Random(seed)
    photo = Image.new("RGB", (1200, 900))
    px = photo.load()
    for y in range(900):
        for x in range(1200):
            v = noise.randrange(-20, 20)
            px[x, y] = (min(255, max(0, 70 + x // 11 + v)), min(255, max(0, 110 + v)), min(255, max(0, 150 + y // 8 + v)))
    photo.filter(ImageFilter.GaussianBlur(3)).save(p("uc10-photo.jpg"), quality=80)

    with open(p("words.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(words, f, indent=1, sort_keys=True)
    for name in sorted(os.listdir(out)):
        print(f"  {os.path.getsize(p(name)):>10}  {name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
