# SPDX-License-Identifier: MPL-2.0
"""Write the recognizer's character list out of its inference.yml.

PaddleOCR ships the recognizer's alphabet inside inference.yml
(PostProcess.character_dict), not as a separate file. ONNX consumers
(oar-ocr among them) want one character per line. The decoder adds the CTC
blank in front and the space at the end itself, so neither is written here:
18,708 entries + blank + space = the 18,710 classes the model outputs.

usage: python -I extract_dict.py <inference.yml> <out.txt>
"""

import sys

import yaml


def main() -> int:
    src, dst = sys.argv[1], sys.argv[2]
    with open(src, encoding="utf-8") as f:
        doc = yaml.safe_load(f)
    chars = doc["PostProcess"]["character_dict"]
    bad = [
        i
        for i, c in enumerate(chars)
        if not isinstance(c, str) or len(c) != 1 or c in "\r\n"
    ]
    if bad:
        # oar-ocr keeps only the first character of each line and drops empty
        # lines, which would shift every index after the first bad entry.
        print(f"refusing: {len(bad)} entries are not exactly one character", file=sys.stderr)
        return 1
    # newline="\n" so the bytes are the same on every platform.
    with open(dst, "w", encoding="utf-8", newline="\n") as f:
        for c in chars:
            f.write(c + "\n")
    print(f"dictionary: {len(chars)} characters")
    return 0


if __name__ == "__main__":
    sys.exit(main())
