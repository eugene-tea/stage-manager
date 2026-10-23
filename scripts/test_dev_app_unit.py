#!/usr/bin/env python3
"""Offline tests for the dev copy's scripts (scripts/dev-app.sh, scripts/run.sh), review R7:
a running copy is never replaced, and a failed copy keeps the old one. The scripts run
unchanged in a scratch checkout with a fake app (its `ctl ping` answers as told) and shims
for `ps` and `open`: nothing real is launched, copied over or signalled.
Run: python3 scripts/test_dev_app_unit.py
"""

import os
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class DevApp(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="sm-dev-app-unit.")
        scripts = os.path.join(self.tmp, "scripts")
        os.makedirs(scripts)
        for name in ("dev-app.sh", "run.sh"):
            shutil.copy2(os.path.join(ROOT, "scripts", name), scripts)
        # The fresh build: its ctl ping answers when $FAKE_PING is set.
        exe = os.path.join(self.tmp, "target/Stage Manager.app/Contents/MacOS/stage-manager")
        os.makedirs(os.path.dirname(exe))
        self.write(exe, '#!/bin/sh\n[ "$1 $2" = "ctl ping" ] && [ -n "$FAKE_PING" ]\n', 0o755)
        # The dev copy already there, marked.
        self.dest = os.path.join(self.tmp, "dev/Stage Manager.app")
        os.makedirs(os.path.join(self.dest, "Contents/MacOS"))
        self.marker = os.path.join(self.dest, "old-bundle-marker")
        self.write(self.marker, "the old copy")
        # Shims: `ps` lists $FAKE_PS; `open` launches nothing and records that it was asked.
        self.bin = os.path.join(self.tmp, "bin")
        os.makedirs(self.bin)
        self.opened = os.path.join(self.tmp, "opened")
        self.write(os.path.join(self.bin, "ps"), '#!/bin/sh\nprintf "%s\\n" "/sbin/launchd" "$FAKE_PS"\n', 0o755)
        self.write(os.path.join(self.bin, "open"), f'#!/bin/sh\ntouch "{self.opened}"\nexit 1\n', 0o755)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    @staticmethod
    def write(path, text, mode=None):
        with open(path, "w") as f:
            f.write(text)
        if mode:
            os.chmod(path, mode)

    def run_script(self, name, ping=False, ps=""):
        env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}", SM_DEV_APP=self.dest,
                   STAGE_MANAGER_HOME=os.path.join(self.tmp, "home"), FAKE_PS=ps)
        env.pop("FAKE_PING", None)
        if ping:
            env["FAKE_PING"] = "1"
        return subprocess.run(["bash", os.path.join(self.tmp, "scripts", name)], env=env,
                              capture_output=True, text=True, timeout=60)

    def assert_old_copy_intact(self):
        self.assertTrue(os.path.exists(self.marker), "the old copy is untouched")
        self.assertFalse(os.path.exists(os.path.join(self.dest, "Contents/MacOS/stage-manager")))

    def leftovers(self):
        return [n for n in os.listdir(os.path.dirname(self.dest)) if n != "Stage Manager.app"]

    def test_run_refuses_before_touching_the_dev_copy(self):
        r = self.run_script("run.sh", ping=True)
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("already running", r.stderr)
        self.assert_old_copy_intact()
        self.assertFalse(os.path.exists(self.opened), "nothing launched")

    def test_a_copy_running_from_the_dev_copy_is_never_replaced(self):
        for line in (f"{self.dest}/Contents/MacOS/stage-manager",
                     f"{self.dest}/Contents/MacOS/stage-manager --guardian 123"):
            r = self.run_script("dev-app.sh", ps=line)
            self.assertEqual(r.returncode, 1, r.stderr)
            self.assertIn("is running from", r.stderr)
            self.assert_old_copy_intact()
        # Another home's run.sh pings nothing, yet the running dev copy still stops it.
        r = self.run_script("run.sh", ps=f"{self.dest}/Contents/MacOS/stage-manager")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assert_old_copy_intact()
        self.assertFalse(os.path.exists(self.opened), "nothing launched")

    def test_a_failed_copy_keeps_the_old_one(self):
        shutil.rmtree(os.path.join(self.tmp, "target"))
        r = self.run_script("dev-app.sh")
        self.assertNotEqual(r.returncode, 0)
        self.assertTrue(os.path.exists(self.marker), "the old copy stays")
        self.assertEqual(self.leftovers(), [], "no half copy left beside it")

    def test_a_stopped_dev_copy_is_replaced(self):
        # Another app whose path only contains the dev copy's is not "running from" it.
        r = self.run_script("dev-app.sh", ps=f"/usr/bin/tail -F {self.dest}/Contents/MacOS/stage-manager")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), self.dest)
        self.assertFalse(os.path.exists(self.marker), "replaced whole")
        self.assertTrue(os.path.exists(os.path.join(self.dest, "Contents/MacOS/stage-manager")))
        self.assertEqual(self.leftovers(), [])
        # And with no copy there yet.
        shutil.rmtree(self.dest)
        r = self.run_script("dev-app.sh")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(os.path.exists(os.path.join(self.dest, "Contents/MacOS/stage-manager")))


if __name__ == "__main__":
    unittest.main()
