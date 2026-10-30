# SPDX-License-Identifier: MPL-2.0
"""Pure converter tests: no files, archives, network, or application state."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from icon_pack import declarations, normalize


def svg(content, attrs='fill="none" stroke="currentColor"'):
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" {attrs}>{content}</svg>'


class SvgConversion(unittest.TestCase):
    def test_inherited_paint_and_invisible_reset(self):
        stroke, fill = normalize(svg('<path stroke="none" fill="none" d="M0 0h24v24H0z"/><g fill="currentColor" stroke="none"><circle cx="4" cy="5" r="1"/></g><path d="M2 3h4"/>'))
        self.assertEqual(stroke, "M2 3h4")
        self.assertEqual(fill, "M3 5a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z")

    def test_a_shape_can_have_both_stroke_and_fill(self):
        stroke, fill = normalize(svg('<path fill="currentColor" d="M2 2h4v4z"/>'))
        self.assertEqual(stroke, fill)

    def test_primitives_and_rounded_rect_clamping(self):
        stroke, fill = normalize(svg('<line x1="1" y1="2" x2="3" y2="4"/><rect x="2" y="3" width="4" height="6" rx="8"/><polyline points="1,2 3,4 5,6"/><polygon points="1,2 3,4 5,6"/><ellipse cx="12" cy="12" rx="2" ry="3"/>'))
        self.assertIn("M1 2L3 4", stroke)
        self.assertIn("M4 3h0a2 3", stroke)
        self.assertIn("M1 2L3 4 5 6Z", stroke)
        self.assertIn("M10 12a2 3", stroke)
        self.assertEqual(fill, "")

    def test_unsupported_structure_is_named_not_silently_dropped(self):
        for content in ['<path transform="scale(2)" d="M1 1L2 2"/>', '<path fill-rule="evenodd" d="M1 1L2 2"/>', '<text>PDF</text>', '<path style="fill:red" d="M1 1L2 2"/>', '<path opacity=".5" d="M1 1L2 2"/>']:
            with self.subTest(content=content), self.assertRaisesRegex(ValueError, "FILE_PDF:.*unsupported"):
                normalize(svg(content), "FILE_PDF")
        with self.assertRaisesRegex(ValueError, "viewBox"):
            normalize(svg('<path d="M1 1L2 2"/>').replace("0 0 24 24", "0 0 256 256"))

    def test_empty_invalid_geometry_is_rejected(self):
        for content in ['<path/>', '<circle r="-1"/>', '<polyline points="1 2 3"/>']:
            with self.subTest(content=content), self.assertRaises(ValueError):
                normalize(svg(content))

    def test_aliases_resolve_without_changing_codepoints(self):
        source = "pub const DIVIDER: char = MINUS;\npub const MINUS: char = '\\u{E32A}';\npub const WINDOW_MINIMISE: char = DIVIDER;"
        self.assertEqual(set(declarations(source).values()), {0xE32A})
        with self.assertRaisesRegex(ValueError, "unresolved"):
            declarations("pub const A: char = B; pub const B: char = A;")


if __name__ == "__main__":
    unittest.main()
