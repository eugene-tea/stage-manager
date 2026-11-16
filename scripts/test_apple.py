#!/usr/bin/env python3
"""Live check of the Apple mode ([windows] with_apple = "augment"): Stage Manager follows
Apple's Stage Manager while it is on (docs/STRATEGIES.md, "Apple-augmented").

OFF BY DEFAULT: it turns Apple's Stage Manager on on this Mac (Apple's then arranges every
window on screen for the run), so it runs only with SM_TEST_TOGGLE_APPLE_SM=1, as
scripts/test-apple-stage-manager.sh does. The original GloballyEnabled value is restored on
every exit (success, failure, Ctrl-C, kill).

Phases (all by default, or name them):

  mirror  Apple's stages changed by plain app activation (`open` of each fixture app in
          turn): the mirror (`ctl state` `apple.current`) shows the activated app's windows
          in front within a second, settled, and the model's stage in front holds them.
  switch  `ctl switch N` for each stage of Apple's strip: the focus route runs, Apple's
          brings that stage in front within 1.5 s, and the pointer does not move.

Needs: make dev (the dev copy). Builds the fixture copies Alpha, Bravo and Charlie (their own
bundle ids, scripts/bundle.sh's FIXTURE_ID and FIXTURE_APP) when stale. Manages only them
(STAGE_MANAGER_ONLY), in a scratch STAGE_MANAGER_HOME whose config.toml sets
with_apple = "augment". Window titles are the fixture's own ("Alpha 1", ...).
"""

import atexit
import ctypes
import ctypes.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
APP = os.environ.get("SM_DEV_APP") or os.path.expanduser(
    "~/Library/Application Support/Stage Manager Dev/Stage Manager.app")
BIN = os.path.join(APP, "Contents", "MacOS", "stage-manager")
EXE = "Contents/MacOS/sm-fixture"
BASE_ID = "dev.eugene.stagemanager.fixture.apple"
# name: (bundle id, app path, window count)
FIX = {
    name: (f"{BASE_ID}.{name.lower()}", os.path.join(ROOT, "target", f"SM Fixture Apple {name}.app"), n)
    for name, n in (("Alpha", 2), ("Bravo", 1), ("Charlie", 1))
}
DOMAIN, KEY = "com.apple.WindowManager", "GloballyEnabled"
FOLLOW_MAX_S = 1.0
SWITCH_MAX_S = 1.5

HOME = tempfile.mkdtemp(prefix="sm-apple-mode.", dir="/tmp")
ENV = dict(os.environ, STAGE_MANAGER_HOME=HOME)
passed, failed = [], []
original = None  # (had, value) of GloballyEnabled, read before anything changes it


def say(msg):
    print(msg, flush=True)


def check(cond, msg):
    (passed if cond else failed).append(msg)
    say(("  ok   " if cond else "  FAIL ") + msg)
    return cond


def run(args, **kw):
    return subprocess.run(args, env=ENV, capture_output=True, text=True, **kw)


def ctl(*words):
    r = run([BIN, "ctl", *map(str, words)], timeout=15)
    try:
        return json.loads(r.stdout.strip() or "{}")
    except json.JSONDecodeError:
        return {"ok": False, "error": r.stdout + r.stderr}


def wait_until(fn, timeout, what, step=0.05):
    end = time.time() + timeout
    last = None
    while time.time() < end:
        try:
            last = fn()
            if last:
                return last
        except Exception as e:  # noqa: BLE001 - polling a live system
            last = e
        time.sleep(step)
    say(f"  timeout waiting for {what} (last: {last!r})")
    return None


# ----- Apple's Stage Manager, restored on every exit -------------------------------------


def read_apple():
    r = subprocess.run(["defaults", "read", DOMAIN, KEY], capture_output=True, text=True)
    return (True, r.stdout.strip()) if r.returncode == 0 else (False, "")


def set_apple(on):
    subprocess.run(["defaults", "write", DOMAIN, KEY, "-bool", "true" if on else "false"], check=True)


def restore_apple():
    if original is None:
        return
    had, value = original
    if had:
        set_apple(value == "1")
    else:
        subprocess.run(["defaults", "delete", DOMAIN, KEY], capture_output=True)
    say(f"restored {DOMAIN} {KEY} to {value if had else '<absent>'}")


def pids(app):
    r = subprocess.run(["pgrep", "-f", os.path.join(app, EXE)], capture_output=True, text=True)
    return [int(p) for p in r.stdout.split()]


def quit_fixtures():
    for _, app, _ in FIX.values():
        for pid in pids(app):
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass


def cleanup():
    ctl("quit")
    quit_fixtures()
    restore_apple()
    if failed:
        say(f"kept for its log: {HOME}")
    else:
        shutil.rmtree(HOME, ignore_errors=True)


def on_signal(signum, _frame):
    sys.exit(128 + signum)


# ----- the pointer, read through CoreGraphics --------------------------------------------


class CGPoint(ctypes.Structure):
    _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]


_cg = ctypes.cdll.LoadLibrary(ctypes.util.find_library("CoreGraphics"))
_cg.CGEventCreate.restype = ctypes.c_void_p
_cg.CGEventCreate.argtypes = [ctypes.c_void_p]
_cg.CGEventGetLocation.restype = CGPoint
_cg.CGEventGetLocation.argtypes = [ctypes.c_void_p]
_cf = ctypes.cdll.LoadLibrary(ctypes.util.find_library("CoreFoundation"))
_cf.CFRelease.argtypes = [ctypes.c_void_p]


def pointer():
    e = _cg.CGEventCreate(None)
    p = _cg.CGEventGetLocation(e)
    _cf.CFRelease(e)
    return (round(p.x, 1), round(p.y, 1))


# ----- setup ------------------------------------------------------------------------------


def build_fixtures():
    src = os.path.join(ROOT, "scripts", "fixture", "main.swift")
    for bundle_id, app, _ in FIX.values():
        exe = os.path.join(app, EXE)
        if os.path.exists(exe) and os.path.getmtime(exe) >= os.path.getmtime(src):
            continue
        env = dict(os.environ, FIXTURE_ID=bundle_id, FIXTURE_APP=app)
        for cmd in (["scripts/bundle.sh", "fixture"], ["scripts/sign.sh", app]):
            r = subprocess.run(cmd, cwd=ROOT, env=env, capture_output=True, text=True)
            if r.returncode != 0:
                sys.exit(f"cannot build {app}: {r.stdout}{r.stderr}")


def start():
    with open(os.path.join(HOME, "config.toml"), "w", encoding="utf-8") as f:
        f.write('[windows]\nwith_apple = "augment"\n')
    for name, (_, app, n) in FIX.items():
        subprocess.run(["open", "-n", app, "--args", "--windows", str(n), "--title", name], check=True)
        time.sleep(0.8)
    only = ",".join(b for b, _, _ in FIX.values())
    args = ["open", "-n", APP]
    for k, v in {"STAGE_MANAGER_HOME": HOME, "STAGE_MANAGER_ONLY": only, "STAGE_MANAGER_NO_PROMPT": "1",
                 "STAGE_MANAGER_TEST": "1", "STAGE_MANAGER_NO_CAPTURE": "1", "SM_TEST_TOGGLE_APPLE_SM": "1",
                 "RUST_LOG": "info,stage_manager=debug"}.items():
        args += ["--env", f"{k}={v}"]
    subprocess.run(args, check=True)
    if not wait_until(lambda: ctl("ping").get("ok"), 15, "the app", step=0.2):
        sys.exit("the app did not start")
    set_apple(True)
    s = wait_until(lambda: (lambda s: s if s.get("engine", {}).get("kind") == "apple" and s.get("ok") else None)(ctl("state")),
                   20, "the Apple mode following Apple's", step=0.2)
    check(s is not None, "Apple's on with with_apple = \"augment\": the Apple mode follows it")
    return s


def titles(s, ids):
    by_id = {w["id"]: w["title"] for w in s.get("windows", [])}
    return sorted(by_id.get(i, str(i)) for i in ids)


def front_titles(s):
    return titles(s, (s.get("apple") or {}).get("current", []))


# ----- phases -----------------------------------------------------------------------------


def phase_mirror():
    say("\n== mirror: Apple's stages changed by plain app activation ==")
    for name, (_, app, n) in FIX.items():
        want = sorted(f"{name} {k}" for k in range(1, n + 1))
        t0 = time.time()
        subprocess.run(["open", app], check=True)
        s = wait_until(lambda: (lambda s: s if front_titles(s) == want and s["apple"]["settled"] else None)(ctl("state")), 5, f"{name} in front")
        took = time.time() - t0
        if not check(s is not None, f"{name} activated: the mirror shows its stage in front ({took:.2f} s)"):
            continue
        check(took <= FOLLOW_MAX_S + 0.5, f"{name}: followed within about a second of the activation ({took:.2f} s, the open included)")
        active = s.get("active")
        stage = next((st for st in s["stages"] if st["id"] == active), None)
        check(stage is not None and titles(s, stage["windows"]) == want, f"{name}: the model's stage in front holds its windows")
        check(all(st.get("where") in ("current", "strip", "remembered") for st in s["stages"]), "every stage says where Apple's shows it")
        check(s["apple"]["list_vs_ax_misses"] == 0, f"the list and Accessibility agree ({s['apple']['list_vs_ax_misses']} misses)")


def phase_switch():
    say("\n== switch: ctl switch N through the focus route ==")
    s = ctl("state")
    count = len((s.get("apple") or {}).get("strip", []))
    if not check(count >= 1, f"Apple's strip lists {count} stages"):
        return
    for n in range(1, count + 1):
        s = ctl("state")
        target = titles(s, s["apple"]["strip"][n - 1]) if n - 1 < len(s["apple"]["strip"]) else []
        runs = s["apple"]["routes"]["focus"]["runs"]
        before = pointer()
        t0 = time.time()
        r = ctl("switch", n)
        if not check(r.get("ok") and r.get("route") == "focus", f"switch {n}: the focus route ({r})"):
            continue
        s = wait_until(lambda: (lambda s: s if front_titles(s) == target and s["apple"]["settled"] else None)(ctl("state")), 5, f"stage {n} in front")
        took = time.time() - t0
        check(s is not None, f"switch {n}: Apple's brought {target} in front ({took:.2f} s)")
        check(took <= SWITCH_MAX_S + 0.5, f"switch {n}: within {SWITCH_MAX_S} s and a read ({took:.2f} s)")
        check(pointer() == before, f"switch {n}: the pointer did not move ({before} -> {pointer()})")
        if s:
            check(s["apple"]["routes"]["focus"]["runs"] >= runs + 1, f"switch {n}: the route counted its run")
    r = ctl("switch", count + 5)
    check(not r.get("ok"), f"a stage that is not there is refused ({r.get('error')})")


def main():
    global original
    if os.environ.get("SM_TEST_TOGGLE_APPLE_SM") != "1":
        say("skipped: set SM_TEST_TOGGLE_APPLE_SM=1 to toggle Apple's Stage Manager on this Mac (the owner's call)")
        return 0
    if not os.access(BIN, os.X_OK):
        sys.exit(f"{BIN} missing; run make dev")
    # The installed app (the default socket) would follow or stand down too: never beside it.
    r = subprocess.run([BIN, "ctl", "ping"], capture_output=True, text=True, env={k: v for k, v in os.environ.items() if k != "STAGE_MANAGER_HOME"})
    if r.returncode == 0:
        sys.exit("a Stage Manager is running (the installed app?); quit it first")
    phases = sys.argv[1:] or ["mirror", "switch"]
    unknown = [p for p in phases if p not in ("mirror", "switch")]
    if unknown:
        sys.exit(f"unknown phase {unknown}; phases: mirror, switch")
    build_fixtures()
    original = read_apple()
    atexit.register(cleanup)
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, on_signal)
    set_apple(False)
    if start() is None:
        return 1
    if "mirror" in phases:
        phase_mirror()
    if "switch" in phases:
        phase_switch()
    say(f"\n{len(passed)} passed, {len(failed)} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
