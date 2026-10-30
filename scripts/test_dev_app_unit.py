#!/usr/bin/env python3
"""Offline tests for the dev copy's scripts (scripts/dev-app.sh, scripts/run.sh, `make
install`), reviews R7, C5 and C6: a running copy is never replaced, a failed copy keeps the
old one, and no other copy (the installed one) is ever taken for the dev copy. The scripts
run unchanged in a scratch checkout with a fake app (its `ctl ping` answers as told) and
shims for `ps`, `open` and `pgrep`, with HOME in the scratch folder: nothing real is
launched, copied over or signalled.
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
        shutil.copy2(os.path.join(ROOT, "Makefile"), self.tmp)
        self.home = os.path.join(self.tmp, "home")
        os.makedirs(self.home)
        # The fresh build: its ctl ping answers when $FAKE_PING is set.
        exe = os.path.join(self.tmp, "target/Stage Manager.app/Contents/MacOS/stage-manager")
        os.makedirs(os.path.dirname(exe))
        self.write(exe, '#!/bin/sh\n[ "$1 $2" = "ctl ping" ] && [ -n "$FAKE_PING" ]\n', 0o755)
        # The dev copy already there, marked.
        self.dest = os.path.join(self.tmp, "dev/Stage Manager.app")
        os.makedirs(os.path.join(self.dest, "Contents/MacOS"))
        self.marker = os.path.join(self.dest, "old-bundle-marker")
        self.write(self.marker, "the old copy")
        # Shims: `ps` lists $FAKE_PS (with pids: 1 and $FAKE_PID); `open` launches nothing,
        # records that it was asked and exits with $FAKE_OPEN; `pgrep` names $FAKE_PID.
        self.bin = os.path.join(self.tmp, "bin")
        os.makedirs(self.bin)
        self.opened = os.path.join(self.tmp, "opened")
        self.write(os.path.join(self.bin, "ps"),
                   '#!/bin/sh\ncase "$*" in *pid=*) printf "%s\\n" "    1 /sbin/launchd" "$FAKE_PID $FAKE_PS" ;;\n'
                   '*) printf "%s\\n" "/sbin/launchd" "$FAKE_PS" ;; esac\n', 0o755)
        self.write(os.path.join(self.bin, "open"), f'#!/bin/sh\ntouch "{self.opened}"\nexit "${{FAKE_OPEN:-1}}"\n', 0o755)
        self.write(os.path.join(self.bin, "pgrep"), '#!/bin/sh\necho "$FAKE_PID"\n', 0o755)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    @staticmethod
    def write(path, text, mode=None):
        with open(path, "w") as f:
            f.write(text)
        if mode:
            os.chmod(path, mode)

    def env(self, ping=False, ps="", **extra):
        env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}", SM_DEV_APP=self.dest, HOME=self.home,
                   STAGE_MANAGER_HOME=os.path.join(self.tmp, "sm-home"), FAKE_PS=ps, FAKE_PID="99999999")
        env.pop("FAKE_PING", None)
        if ping:
            env["FAKE_PING"] = "1"
        env.update(extra)
        return env

    def run_script(self, name, ping=False, ps="", **extra):
        return subprocess.run(["bash", os.path.join(self.tmp, "scripts", name)], env=self.env(ping, ps, **extra),
                              capture_output=True, text=True, timeout=60)

    def make_install(self, home, ps=""):
        # `sign` (and so the build) is taken as done: only the install recipe runs.
        env = self.env(ps=ps, HOME=home)
        del env["SM_DEV_APP"]
        return subprocess.run(["make", "-s", "-C", self.tmp, "-o", "sign", "-o", "bundle", "-o", "build",
                               "install", f"HOME={home}"], env=env, capture_output=True, text=True, timeout=60)

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


    def test_run_never_takes_another_copy_for_the_dev_copy(self):
        # Review C5: the dev copy never answers; an installed copy runs. run.sh finds no dev
        # copy (so it would never signal the installed one) and says so.
        installed = os.path.join(self.home, "Applications/Stage Manager.app/Contents/MacOS/stage-manager")
        r = self.run_script("run.sh", ps=installed, FAKE_OPEN="0")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("did not start", r.stderr)
        self.assertTrue(os.path.exists(self.opened), "the dev copy was opened")

    def test_dev_app_needs_a_home(self):
        env = self.env(HOME="")
        del env["SM_DEV_APP"]
        r = subprocess.run(["bash", os.path.join(self.tmp, "scripts", "dev-app.sh")], env=env,
                           capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("HOME", r.stderr)

    def test_install_refuses_a_running_copy_and_swaps_otherwise(self):
        # Review C6: `make install` never removes a running installed copy, and never works
        # without a home.
        app = os.path.join(self.home, "Applications/Stage Manager.app")
        os.makedirs(os.path.join(app, "Contents/MacOS"))
        marker = os.path.join(app, "old-bundle-marker")
        self.write(marker, "the installed copy")
        r = self.make_install(self.home, ps=f"{app}/Contents/MacOS/stage-manager")
        self.assertNotEqual(r.returncode, 0, r.stderr)
        self.assertIn("is running from", r.stderr)
        self.assertTrue(os.path.exists(marker), "the running copy is untouched")
        # A home that is no absolute folder (never an empty one here: an unguarded recipe
        # would then work on /Applications).
        r = self.make_install("not-a-folder")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("HOME", r.stderr)
        self.assertTrue(os.path.exists(marker))
        r = self.make_install(self.home)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.exists(marker), "replaced whole")
        self.assertTrue(os.path.exists(os.path.join(app, "Contents/MacOS/stage-manager")))
        self.assertEqual([n for n in os.listdir(os.path.dirname(app)) if n != "Stage Manager.app"], [])


if __name__ == "__main__":
    unittest.main()
