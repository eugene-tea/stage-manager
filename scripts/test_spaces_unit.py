#!/usr/bin/env python3
"""Offline unit tests for the hands-free Spaces check's parsing and state
(scripts/test_spaces.py): the screen-lock flag, the Desktop watcher's verdicts and the Space
change rule, the guardian drill's verdict, the Mission Control check's pixels and pane
behavior, the activation check's stage lookup and its cover pixels (8L4). Nothing is
launched or switched.
Run: python3 scripts/test_spaces_unit.py
"""

import os
import shutil
import struct
import sys
import tempfile
import unittest

HOME = tempfile.mkdtemp(prefix="sm-unit.")
os.environ["TEST_HOME"] = HOME  # test_stages makes no scratch dir of its own
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import test_spaces as t  # noqa: E402


class ScreenLock(unittest.TestCase):
    def test_flag_present_means_locked(self):
        text = '| "IOConsoleUsers" = ({"kCGSSessionOnConsoleKey"=Yes,"CGSSessionScreenIsLocked"=Yes,"kSCSecureInput"=No})'
        self.assertTrue(t.screen_locked(text))

    def test_flag_absent_means_unlocked(self):
        text = '| "IOConsoleUsers" = ({"kCGSSessionOnConsoleKey"=Yes,"kSCSecureInput"=No})'
        self.assertFalse(t.screen_locked(text))

    def test_other_keys_saying_no_do_not_count(self):
        text = '{"CGSSessionScreenIsLocked"=Yes,"kSCSecureInput"=No}'
        self.assertTrue(t.screen_locked(text))
        self.assertFalse(t.screen_locked('{"CGSSessionScreenIsLocked"=No}'))


def watch(d1, d2, expect):
    w = t.Watch.__new__(t.Watch)  # no thread
    w.ids, w.expect = {"d1": set(d1), "d2": set(d2)}, expect
    return w


class Watcher(unittest.TestCase):
    def test_the_expected_desktop(self):
        self.assertIsNone(watch({1, 2}, {3}, ("desk", "d1")).mismatch({1}))
        self.assertIsNone(watch({1, 2}, {3}, ("desk", "d2")).mismatch({3}))

    def test_the_other_desktop_or_none(self):
        self.assertIn("Desktop 2's windows", watch({1}, {3}, ("desk", "d1")).mismatch({1, 3}))
        self.assertIn("none of Desktop 1's", watch({1}, {3}, ("desk", "d1")).mismatch(set()))
        self.assertIn("Desktop 1's windows", watch({1}, {3}, ("desk", "d2")).mismatch({1}))

    def test_an_empty_desktop_2_counts_until_its_windows_open(self):
        self.assertIsNone(watch({1}, set(), ("desk", "d2")).mismatch(set()))

    def test_full_screen(self):
        self.assertIsNone(watch({1}, {3, 4}, ("fullscreen", 3)).mismatch({3}))
        self.assertIsNotNone(watch({1}, {3, 4}, ("fullscreen", 3)).mismatch({3, 4}))
        self.assertIsNotNone(watch({1}, {3, 4}, ("fullscreen", 3)).mismatch({1}))


class SpaceChange(unittest.TestCase):
    before = {"space": 1, "fullscreen": False, "settling": False, "changes": 2}

    def test_like_the_app(self):
        self.assertFalse(t.moved(self.before, dict(self.before)))
        self.assertFalse(t.moved(self.before, None))
        self.assertFalse(t.moved(self.before, dict(self.before, changes=3, settling=True)))
        self.assertTrue(t.moved(self.before, dict(self.before, changes=3)))
        self.assertTrue(t.moved(self.before, dict(self.before, space=2)))
        self.assertTrue(t.moved(self.before, dict(self.before, fullscreen=True)))


class GuardianDrill(unittest.TestCase):
    def run_samples(self, samples):
        v = t.GuardianBack()
        for now, alive, out in samples:
            v.step(now, alive, out)
        return v

    def test_back_while_the_guardian_watches(self):
        v = self.run_samples([(0.0, True, False), (0.2, True, True), (0.6, False, True), (1.2, False, True)])
        self.assertTrue(v.back)
        self.assertFalse(v.early)

    def test_one_in_between_listing_is_not_back(self):
        v = self.run_samples([(0.0, True, False), (0.2, True, True), (0.4, True, False), (1.3, True, False)])
        self.assertFalse(v.back)

    def test_the_guardian_ending_while_the_window_is_parked_fails(self):
        # The 10-12 run: the guardian ended on one listing; the window stayed parked.
        v = self.run_samples([(0.0, True, False), (0.2, False, False), (1.0, False, False), (1.8, False, False)])
        self.assertTrue(v.early)
        self.assertFalse(v.back)

    def test_the_window_server_listing_a_moved_window_late_is_fine(self):
        v = self.run_samples([(0.0, True, False), (0.2, False, False), (0.4, False, True), (1.5, False, True)])
        self.assertTrue(v.back)
        self.assertFalse(v.early)


class Screenshots(unittest.TestCase):
    def test_never_into_the_repo(self):
        old = os.environ.get("SM_SHOTS")
        os.environ["SM_SHOTS"] = os.path.join(t.ts.ROOT, "shots")
        try:
            self.assertEqual(t.shots_dir(), t.ts.HOME)
        finally:
            if old is None:
                os.environ.pop("SM_SHOTS")
            else:
                os.environ["SM_SHOTS"] = old
        self.assertFalse(os.path.exists(os.path.join(t.ts.ROOT, "shots")))


def bmp(pixels, top_down=True):
    """A 32-bit BMP (BITMAPINFOHEADER) of rows of (b, g, r) pixels."""
    h, w = len(pixels), len(pixels[0])
    body = b"".join(bytes([*px, 255]) for row in (pixels if top_down else pixels[::-1]) for px in row)
    info = struct.pack("<IiiHHIIiiII", 40, w, -h if top_down else h, 1, 32, 0, len(body), 2835, 2835, 0, 0)
    return b"BM" + struct.pack("<IHHI", 14 + len(info) + len(body), 0, 0, 14 + len(info)) + info + body


class ExposePixels(unittest.TestCase):
    def write(self, name, data):
        path = os.path.join(HOME, name)
        with open(path, "wb") as f:
            f.write(data)
        return path

    def test_bmp_rows_come_top_down_either_way(self):
        rows = [[(1, 2, 3), (4, 5, 6)], [(7, 8, 9), (10, 11, 12)]]
        for top_down in (True, False):
            w, h, out, bp = t.ts.read_bmp(self.write("x.bmp", bmp(rows, top_down)))
            self.assertEqual((w, h, bp), (2, 2, 4))
            self.assertEqual(out[0][:3], bytes([1, 2, 3]))
            self.assertEqual(out[1][4:7], bytes([10, 11, 12]))

    def test_only_the_cards_pixels_count(self):
        # A 4x4 point region at 2x (8x8 pixels); one card covers its left half.
        dark = [[(0, 0, 0)] * 8 for _ in range(8)]
        right_lit = [[(0, 0, 0)] * 4 + [(255, 255, 255)] * 4 for _ in range(8)]
        left_lit = [[(255, 255, 255)] * 4 + [(0, 0, 0)] * 4 for _ in range(8)]
        grab = lambda rows: ((10, 20, 4, 4), t.ts.read_bmp(self.write("g.bmp", bmp(rows))))  # noqa: E731
        card = [{"x": 10, "y": 20, "w": 2, "h": 4}]
        self.assertEqual(t.ts.changed_fraction(grab(dark), grab(right_lit), card), 0.0)
        self.assertEqual(t.ts.changed_fraction(grab(dark), grab(left_lit), card), 1.0)

    def test_a_small_step_is_not_a_change(self):
        a = [[(100, 100, 100)] * 4 for _ in range(4)]
        b = [[(100 + t.ts.PIXEL_STEP, 100, 100)] * 4 for _ in range(4)]
        grab = lambda rows: ((0, 0, 4, 4), t.ts.read_bmp(self.write("s.bmp", bmp(rows))))  # noqa: E731
        self.assertEqual(t.ts.changed_fraction(grab(a), grab(b), [{"x": 0, "y": 0, "w": 4, "h": 4}]), 0.0)


class ExposeBehavior(unittest.TestCase):
    def test_transient_panes_on_one_desktop_pass(self):
        ok, told = t.ts.panes_hide_in_expose({"strip_panels": [{"space": 1, "behavior": 8 | 64}, {"space": 2, "behavior": 8 | 64}]})
        self.assertTrue(ok, told)
        self.assertIn("transient", told)

    def test_stationary_or_space_hopping_panes_fail(self):
        for bits in (16 | 64, 8 | 16, 8 | 1, 8 | 2, 64):
            ok, _ = t.ts.panes_hide_in_expose({"strip_panels": [{"space": 1, "behavior": bits}]})
            self.assertFalse(ok, bits)
        self.assertFalse(t.ts.panes_hide_in_expose({"strip_panels": []})[0])


class StageHolding(unittest.TestCase):
    def test_the_stage_of_a_titled_window(self):
        s = {"windows": [{"id": 7, "title": "Alpha 1"}, {"id": 8, "title": "Fixture 1"}],
             "stages": [{"number": 1, "windows": [8]}, {"number": 2, "windows": [7]}]}
        self.assertEqual(t.stage_holding(s, "Alpha 1"), 2)
        self.assertEqual(t.stage_holding(s, "Fixture 1"), 1)
        self.assertIsNone(t.stage_holding(s, "Bravo 1"))


class OwnersWindows(unittest.TestCase):
    """8L3: a synthetic click or drag the app refused over another process's window (the
    owner's own, on screen while his Stage Manager is quit for a run) is a skip, not a failure."""

    def test_a_refusal_over_another_window_is_noted(self):
        reply = {"ok": False, "error": "the topmost window at 70,735 is not ours or a fixture's (pid 999999999)"}
        note = t.ts.other_window_note(reply)
        self.assertTrue(note.startswith("the topmost window at 70,735 is not ours or a fixture's (pid 999999999)"), note)
        # Also from a drag's output text.
        self.assertIsNotNone(t.ts.other_window_note('{"ok":false,"error":"the topmost window at 580,90 is not ours or a fixture\'s (pid 1)"}'))

    def test_any_other_reply_is_not(self):
        for reply in ({"ok": True}, {"ok": False, "error": "no window at 70,735 (our windows: )"}, "", None):
            self.assertIsNone(t.ts.other_window_note(reply), reply)

    def test_blocked_counts_a_skip(self):
        before = len(t.ts.skipped)
        self.assertTrue(t.ts.blocked({"error": "the topmost window at 1,2 is not ours or a fixture's (pid 999999999)"}, "a drill"))
        self.assertFalse(t.ts.blocked({"ok": True}, "a drill"))
        self.assertEqual(len(t.ts.skipped), before + 1)


class CoverPixels(unittest.TestCase):
    """8L4: the activation check's boxes in the switch's cover and its verdict on a read."""

    corner = {"x": 2440.0, "y": 1300.0, "w": 120.0, "h": 140.0}
    parked = {"x": 2500.0, "y": 1350.0, "w": 900.0, "h": 700.0}

    def test_boxes_lie_on_the_arriving_sliver_inside_the_cover(self):
        boxes = t.ts.cover_boxes(self.corner, [self.parked], [])
        self.assertTrue(boxes)
        self.assertLessEqual(len(boxes), 3)
        for b in boxes:
            self.assertTrue(t.ts.contains(self.corner, b, 0.0), b)
            self.assertTrue(t.ts.contains(self.parked, b, 0.0), b)

    def test_boxes_keep_off_the_icons(self):
        icon = {"x": 2490.0, "y": 1340.0, "w": 80.0, "h": 110.0}
        boxes = t.ts.cover_boxes(self.corner, [self.parked], [icon])
        self.assertEqual(boxes, [], "the icon fills the sliver's part of the cover")
        small = {"x": 2540.0, "y": 1420.0, "w": 20.0, "h": 20.0}
        for b in t.ts.cover_boxes(self.corner, [self.parked], [small]):
            self.assertIsNone(t.ts.intersect(b, {"x": 2534.0, "y": 1414.0, "w": 32.0, "h": 32.0}), b)

    def test_no_box_without_a_corner_or_a_sliver(self):
        self.assertEqual(t.ts.cover_boxes(None, [self.parked], []), [])
        self.assertEqual(t.ts.cover_boxes(self.corner, [], []), [])

    def test_a_read_matches_the_wallpaper_or_not(self):
        read = {"ok": True, "bounds": {"x": 0, "y": 0, "w": 8, "h": 8}, "mean": [40, 80, 120, 255], "wallpaper": [44, 78, 125, 255]}
        self.assertTrue(t.ts.cover_matches(read)[0])
        window = dict(read, mean=[236, 236, 236, 255], still=[236, 236, 236, 255])
        ok, note = t.ts.cover_matches(window)
        self.assertFalse(ok)
        self.assertIn("still", note)

    def test_a_read_that_cannot_be_judged(self):
        base = {"ok": True, "bounds": {"x": 0, "y": 0, "w": 8, "h": 8}, "mean": [40, 80, 120, 255]}
        self.assertIsNone(t.ts.cover_matches(base)[0], "no wallpaper picture")
        self.assertIsNone(t.ts.cover_matches(dict(base, bounds=None, wallpaper=[1, 2, 3, 255]))[0], "the overlay gone")
        self.assertIsNone(t.ts.cover_matches(dict(base, mean=[40, 80, 120, 90], wallpaper=[1, 2, 3, 255]))[0], "not covered")
        self.assertIsNone(t.ts.cover_matches({"ok": False, "error": "x"})[0])

    def test_only_the_expected_reasons_skip_the_grow(self):
        self.assertEqual(t.ts.AT_ONCE_CAPTURES_OFF, ("window captures are off",))


if __name__ == "__main__":
    try:
        unittest.main(verbosity=1)
    finally:
        shutil.rmtree(HOME, ignore_errors=True)
