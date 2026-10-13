#!/usr/bin/env python3
"""Offline unit tests for the hands-free Spaces check's parsing and state
(scripts/test_spaces.py): the screen-lock flag, the Desktop watcher's verdicts and the Space
change rule. Nothing is launched or switched. Run: python3 scripts/test_spaces_unit.py
"""

import os
import shutil
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


if __name__ == "__main__":
    try:
        unittest.main(verbosity=1)
    finally:
        shutil.rmtree(HOME, ignore_errors=True)
