# SPDX-License-Identifier: MPL-2.0
"""In-memory parser and raster tests, including the four pinned taskbar paths.

SVG fixture paths: Tabler 3.31.0 (MIT), Lucide 0.477.0 (ISC / Feather MIT).
Full upstream notices: crates/koru-release/licences/bundled/icons-*.txt.
"""
import math
import unittest

from render_svg_path import flatten, render

FIXTURES = {
    "tabler-document": ('M14 3v4a1 1 0 0 0 1 1h4 M17 21h-10a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2h7l5 5v11a2 2 0 0 1 -2 2z M9 9l1 0 M9 13l6 0 M9 17l6 0', 5),
    "tabler-sparkle": ('M16 18a2 2 0 0 1 2 2a2 2 0 0 1 2 -2a2 2 0 0 1 -2 -2a2 2 0 0 1 -2 2zm0 -12a2 2 0 0 1 2 2a2 2 0 0 1 2 -2a2 2 0 0 1 -2 -2a2 2 0 0 1 -2 2zm-7 12a6 6 0 0 1 6 -6a6 6 0 0 1 -6 -6a6 6 0 0 1 -6 6a6 6 0 0 1 6 6z', 3),
    "lucide-document": ('M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z M14 2v4a2 2 0 0 0 2 2h4 M10 9H8 M16 13H8 M16 17H8', 5),
    "lucide-sparkle": ('M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z M20 3v4 M22 5h-4 M4 17v2 M5 18H3', 5),
}


class PathTests(unittest.TestCase):
    def test_relative_move_after_close_and_implicit_lines(self):
        self.assertEqual(flatten('m2 3 4 0 0 4zh2m3-1l2 1'),
                         [[(2, 3), (6, 3), (6, 7), (2, 3), (4, 3)], [(7, 2), (9, 3)]])

    def test_absolute_and_relative_forms_match(self):
        self.assertEqual(flatten('M1 2 L3 4 H7 V8 C8 8 9 9 10 10'),
                         flatten('m1 2 l2 2 h4 v4 c1 0 2 1 3 2'))

    def test_cubic_samples_follow_curve_and_end_exactly(self):
        points = flatten('M0 0C0 10 10 10 10 0')[0]
        self.assertGreater(len(points), 10)
        self.assertEqual(points[-1], (10, 0))
        self.assertAlmostEqual(max(y for _, y in points), 7.5)

    def test_arc_sampling_respects_circle_and_sweep(self):
        for sweep in (0, 1):
            points = flatten(f'M2 0A2 2 0 1 {sweep} -2 0')[0]
            self.assertEqual(points[-1], (-2, 0))
            for x, y in points:
                self.assertAlmostEqual(math.hypot(x, y), 2, places=10)
            self.assertEqual(any(y > 1 for _, y in points), bool(sweep))
        self.assertEqual(flatten('M0 0a0 2 0 0 1 3 4'), [[(0, 0), (3, 4)]])
        # Too-small radii are expanded; a rotated ellipse still reaches its endpoint.
        self.assertEqual(flatten('M0 0a1 2 45 1 1 10 10')[0][-1], (10, 10))

    def test_reject_unsupported_or_malformed_input(self):
        for path in ('M0 0Q1 2 3 4', 'M0 0 L1', 'M0 0L', 'L1 2', 'Z',
                     'M0 0a1 1 0 2 0 1 1', 'M0 0L1e309 2', 'M0 0@L1 2', 'M0 0'):
            with self.subTest(path=path), self.assertRaises(ValueError):
                flatten(path)

    def test_four_pinned_paths_keep_every_subpath_and_visible_pixels(self):
        for name, (path, count) in FIXTURES.items():
            with self.subTest(name=name):
                paths = flatten(path)
                self.assertEqual(len(paths), count)
                self.assertTrue(all(len(p) >= 2 for p in paths))
                for points in paths:
                    for x, y in points:
                        self.assertTrue(0 <= x <= 24 and 0 <= y <= 24)
                image = render(path, 1.85, 256)
                self.assertIsNotNone(image.getbbox())
                self.assertEqual(image.tobytes(), render(path, 1.85, 256).tobytes())
                # Each path has visible ink at its first point. A dropped relative
                # move or arc cannot pass by drawing only one of several subpaths.
                for points in paths:
                    x, y = points[0]
                    self.assertGreater(image.getpixel((round(x*256/24), round(y*256/24)))[3], 200)

    def test_round_caps_reach_beyond_centerline_endpoints(self):
        image = render('M8 12H16', 2, 240)
        self.assertGreater(image.getpixel((72, 120))[3], 200)
        self.assertEqual(image.getpixel((60, 120))[3], 0)
        self.assertGreater(image.getpixel((168, 120))[3], 200)


if __name__ == '__main__':
    unittest.main()
