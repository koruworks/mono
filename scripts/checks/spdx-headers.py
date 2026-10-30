#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0

"""Add or check the `SPDX-License-Identifier: MPL-2.0` header on first-party source.

    python scripts/checks/spdx-headers.py            # insert where missing
    python scripts/checks/spdx-headers.py --check    # list files missing it, exit 1 if any

Idempotent: a file that already names an SPDX identifier in its first lines is
left alone, so this can be re-run after every merge.

What is *not* headed, and why — each is a rule below, not a judgement per file:

- Third-party and generated code: `third-party/`, `contracts/gen/`, and
  anything marked `@generated`. Third-party files fetched into first-party
  directories (the Gradle wrapper, a grammar) are gitignored, so never listed.
- Test data: a non-Rust file under a `fixtures/` or `testdata/` directory is
  read as bytes by a test, and a header would change what the test reads. (A
  `.rs` file there is a module — `src/fixtures/tests.rs` — and is headed.)
- SQL. A shipped migration is never edited (`crates/koru-sqlite`): each runs
  once, counted by `user_version`, so an edit reaches only new databases.
- Everything outside `ROOTS` (`docs/`, for one), which is prose.
"""

import subprocess
import sys
from pathlib import Path

SPDX = "SPDX-License-Identifier: MPL-2.0"

# Extension -> the line written for it.
COMMENT = {
    ".rs": f"// {SPDX}",
    ".kt": f"// {SPDX}",
    ".kts": f"// {SPDX}",
    ".swift": f"// {SPDX}",
    ".proto": f"// {SPDX}",
    ".h": f"/* {SPDX} */",
    ".py": f"# {SPDX}",
    ".sh": f"# {SPDX}",
    ".ps1": f"# {SPDX}",
    ".psm1": f"# {SPDX}",
    ".bat": f"rem {SPDX}",
    ".nsi": f"; {SPDX}",
}

ROOTS = ("apps/", "crates/", "contracts/koru/", "scripts/", "tests/", "koru.sh", "koru.bat")


EXCLUDED_DIRS = ("fixtures", "testdata")

LOOKAHEAD = 5  # lines searched for an existing identifier or @generated marker


def candidates(root: Path) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "-z", "--", *ROOTS], cwd=root, check=True, capture_output=True
    ).stdout.decode()
    paths = []
    for path in filter(None, out.split("\0")):
        if Path(path).suffix not in COMMENT:
            continue
        in_fixtures = any(part in EXCLUDED_DIRS for part in Path(path).parts[:-1])
        if in_fixtures and not path.endswith(".rs"):
            continue
        paths.append(path)
    return paths


def head_lines(text: str) -> list[str]:
    return text.splitlines()[:LOOKAHEAD]


def needs_header(text: str) -> bool:
    head = head_lines(text)
    if any("SPDX-License-Identifier" in line for line in head):
        return False
    if any("@generated" in line for line in head):
        return False
    return True


def insert(text: str, suffix: str, newline: str) -> str:
    """Put the header after anything that must stay first in the file."""
    lines = text.split(newline)
    at = 0
    if lines and lines[0].startswith("#!"):
        at = 1
    if suffix == ".py" and len(lines) > at and "coding" in lines[at] and lines[at].startswith("#"):
        at += 1
    if suffix == ".swift" and lines and lines[0].startswith("// swift-tools-version"):
        at = 1
    if suffix == ".bat" and lines and lines[0].strip().lower() == "@echo off":
        at = 1
    header = [COMMENT[suffix]]
    # A blank line after the header, so it never reads as part of the doc
    # comment, docstring or help block that follows.
    if len(lines) > at and lines[at].strip():
        header.append("")
    return newline.join(lines[:at] + header + lines[at:])


def main() -> int:
    check = "--check" in sys.argv[1:]
    root = Path(
        subprocess.run(
            ["git", "rev-parse", "--show-toplevel"], check=True, capture_output=True, text=True
        ).stdout.strip()
    )
    missing = []
    for path in candidates(root):
        file = root / path
        raw = file.read_bytes()
        bom = raw.startswith(b"\xef\xbb\xbf")
        try:
            text = raw[3 if bom else 0 :].decode("utf-8")
        except UnicodeDecodeError:
            print(f"skipped (not UTF-8): {path}", file=sys.stderr)
            continue
        if not needs_header(text):
            continue
        missing.append(path)
        if check:
            continue
        # The first line's ending, not "any CRLF anywhere": a file with mixed
        # endings would otherwise split as one line and be headed at its end.
        first = text.split("\n", 1)[0]
        newline = "\r\n" if first.endswith("\r") else "\n"
        updated = insert(text, file.suffix, newline).encode("utf-8")
        file.write_bytes((b"\xef\xbb\xbf" if bom else b"") + updated)

    if check:
        for path in missing:
            print(path)
        if missing:
            print(f"{len(missing)} file(s) missing '{SPDX}'", file=sys.stderr)
        return 1 if missing else 0
    print(f"headed {len(missing)} file(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
