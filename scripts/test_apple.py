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
  rejected The routes tried and rejected, kept as tripwires (`ctl apple try`): AXPress and
          AXAddToStage on a strip item and a Shift-click posted to WindowManager's pid still
          change nothing; Shift held around the focus route still switches and does not
          add. A red here means a macOS update made a rejected route work: see
          docs/STRATEGIES.md, "Tried and rejected".
  merge   `ctl merge N`: Bravo's stage of one, then a stage of three, brought into the stage
          in front by one Shift-click per window, all in one Shift press; timed to the moment
          the window list shows them joined (the job's own "done" printed as info); the
          pointer back where it was.
  split   `ctl new ID` (the drag onto the strip) takes Alpha's front window out of its stage
          of two to a stage of its own; `ctl send N ID` moves Bravo to that stage and comes
          back; the pointer back.

Each phase from switch on sets its own starting arrangement: every fixture alone in a stage
of its own (one that is not is quit and opened again), never through the routes under test.
Each fixture window opens at a frame of its own (FRAMES).
  pointer The pointer sampled every millisecond around a Shift-click and a drag: away about a
          frame for the click, back where it was after each. Sets up its own strip stage
          (Charlie opened again) when the phases before left none.
  open-here With [stages] new_apps_join_current on: Charlie quit and opened again while
          Alpha is in front joins Alpha's stage (time from the launch to the moment the window
          list shows one stage, the job's "done" as info, the route that ran, Alpha's windows
          never moving with the hide route); Bravo opened again with two windows: both join,
          in one Shift press.

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
# Each fixture window at a frame of its own, about 60 points apart ("x,y,w,h" from the
# visible frame's top-left): windows at one frame stack their thumbnails exactly, and the one
# behind shows no part of its own (live 11-16); real windows rarely coincide.
FRAMES = {
    "Alpha": ["300,40,520,392", "360,100,520,392"],
    "Bravo": ["420,160,520,392", "480,220,520,392"],
    "Charlie": ["540,280,520,392"],
}
DOMAIN, KEY = "com.apple.WindowManager", "GloballyEnabled"
FOLLOW_MAX_S = 1.0
SWITCH_MAX_S = 1.5
MERGE_MAX_S = 1.5          # one Shift press and Apple's motion (~0.8 s), a read, ctl's poll
OPEN_HERE_MAX_S = 3.0      # launch to the first window (~0.9 s), the settle, one press (~0.8 s)
PHASES = ("mirror", "switch", "rejected", "merge", "split", "pointer", "open-here")

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


def near(a, b):
    """The same pointer place within a point: a warp lands on whole points (live 11-16:
    (1086.8, 576.7) back as (1086.0, 576.0))."""
    return abs(a[0] - b[0]) <= 1.0 and abs(a[1] - b[1]) <= 1.0


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


def write_config(extra=""):
    with open(os.path.join(HOME, "config.toml"), "w", encoding="utf-8") as f:
        f.write('[windows]\nwith_apple = "augment"\n' + extra)


def open_fixture(name, windows=None):
    """Fixture `name` opened anew with `windows` windows (its own count by default), each at
    its own frame."""
    _, app, n = FIX[name]
    n = windows or n
    subprocess.run(["open", "-n", app, "--args", "--windows", str(n), "--title", name,
                    "--frames", ";".join(FRAMES[name][:n])], check=True)


def start():
    write_config()
    for name in FIX:
        open_fixture(name)
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


def settled_state(timeout=5):
    """`ctl state` once the mirror is settled (Apple's motion over)."""
    return wait_until(lambda: (lambda s: s if (s.get("apple") or {}).get("settled") else None)(ctl("state")), timeout, "the mirror settled")


def others(s):
    """The stages besides the one in front, as `ctl switch N` counts them: the strip's slots
    top to bottom, then the remembered ones."""
    a = s.get("apple") or {}
    return a.get("strip", []) + a.get("remembered", [])


def phase_switch():
    say("\n== switch: ctl switch N through the focus route ==")
    separate()
    s = settled_state()
    if not check(s is not None, "the mirror settles before the switches"):
        return
    count = len(others(s))
    if not check(count >= 1, f"Apple's shows {count} stages besides the one in front"):
        return
    # Each switch reorders the strip (the stage leaving takes the slot of the one coming in):
    # the stages are read again before each, and N runs over as many as the mirror reports.
    for n in range(1, count + 1):
        s = settled_state()
        if s is None or n > len(others(s)):
            check(False, f"switch {n}: the mirror still reports stage {n}")
            break
        target = titles(s, others(s)[n - 1])
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
    s = ctl("state")
    r = ctl("switch", len(others(s)) + 1)
    check(not r.get("ok"), f"a stage that is not there is refused ({r.get('error')})")


def windows_of(s, name):
    """Window ids of fixture `name` ("Alpha"), by title."""
    return sorted(w["id"] for w in s.get("windows", []) if w["title"].startswith(name + " "))


def stage_number(s, ids):
    """N for `ctl switch/merge/send N` of the stage holding `ids` (the strip's slots, then the
    remembered), or None."""
    for n, group in enumerate(others(s), 1):
        if set(ids) <= set(group):
            return n
    return None


def idle_jobs(s):
    a = s.get("apple") or {}
    return a.get("settled") and not a.get("pending") and not a.get("posting")


def quiet_state(timeout=8):
    """`ctl state` once the mirror is settled and no job of ours is under way."""
    return wait_until(lambda: (lambda s: s if idle_jobs(s) else None)(ctl("state")), timeout, "no step of ours under way")


def groups_of(s):
    """Every stage of the mirror's: the one in front, then the strip's and the remembered."""
    a = s.get("apple") or {}
    return ([a["current"]] if a.get("current") else []) + others(s)


def mixed(s):
    """The fixtures not alone in one stage of their own (sharing one, or split across)."""
    out = []
    for name in FIX:
        mine = set(windows_of(s, name))
        holding = [g for g in groups_of(s) if mine & set(g)]
        if mine and not (len(holding) == 1 and set(holding[0]) == mine):
            out.append(name)
    return out


def separate():
    """Each fixture in a stage of its own, whatever the phases before left (each phase sets
    its own starting arrangement): a fixture not alone in one stage is quit and opened again,
    and Apple's gives a newly opened app a stage of its own. Never through our own routes."""
    for _ in range(len(FIX) + 1):
        s = quiet_state()
        bad = mixed(s) if s else list(FIX)
        if not bad:
            return s
        relaunch(bad[0])
    s = quiet_state()
    check(s is not None and not mixed(s), f"each fixture in a stage of its own ({mixed(s) if s else 'no state'})")
    return s


def bring_front(name):
    """Fixture `name`'s stage in front by a plain activation, settled."""
    _, app, n = FIX[name]
    subprocess.run(["open", app], check=True)
    return wait_until(lambda: (lambda s: s if name in "".join(front_titles(s)) and idle_jobs(s) else None)(ctl("state")), 5, f"{name} in front")


def route_runs(s, route):
    return ((s.get("apple") or {}).get("routes") or {}).get(route, {}).get("runs", 0)


def phase_rejected():
    say("\n== rejected: the tripwires (routes tried and rejected still do nothing) ==")
    separate()
    if not bring_front("Alpha"):
        check(False, "Alpha in front before the tripwires")
        return
    s = quiet_state()
    bravo = windows_of(s, "Bravo")
    if not check(bravo and stage_number(s, bravo), "Bravo's stage is in the strip"):
        return
    wid = bravo[0]
    for route, what in (("ax-press", "AXPress on Bravo's strip item"), ("ax-add", "AXAddToStage on Bravo's strip item"), ("pid-click", "a Shift-click posted to WindowManager's pid")):
        s = quiet_state()
        before = (front_titles(s), sorted(map(sorted, others(s))))
        p0 = pointer()
        r = ctl("apple", "try", route, wid)
        if not check(r.get("ok"), f"{route}: ran ({r})"):
            continue
        time.sleep(1.5)
        s = quiet_state()
        after = (front_titles(s), sorted(map(sorted, others(s))))
        check(after == before, f"{route}: {what} still changes nothing (else a macOS update made a rejected route work: docs/STRATEGIES.md) ({before[0]} -> {after[0]})")
        check(pointer() == p0, f"{route}: the pointer did not move")
    # Shift held around the focus route: switches, never adds.
    s = quiet_state()
    alpha = windows_of(s, "Alpha")
    r = ctl("apple", "try", "shift-focus", wid)
    if check(r.get("ok"), f"shift-focus: ran ({r})"):
        s = wait_until(lambda: (lambda s: s if idle_jobs(s) and front_titles(s) == ["Bravo 1"] else None)(ctl("state")), 4, "Bravo in front")
        check(s is not None, "shift-focus: still switches to Bravo's stage")
        if s:
            check(not set(alpha) & set(s["apple"]["current"]), "shift-focus: still does not add Bravo to Alpha's stage (else docs/STRATEGIES.md)")


def merge_into_front(n, want, what):
    """`ctl merge n`, timed to the moment the window list shows `want` in front (what is seen),
    then waited out for the job's own account. Returns (state once idle, seconds to joined)."""
    p0 = pointer()
    t0 = time.time()
    r = ctl("merge", n)
    if not check(r.get("ok"), f"merge {n}: planned ({r})"):
        return None, None
    joined = wait_until(lambda: (lambda s: s if front_titles(s) == want else None)(ctl("state")), 6, what)
    took = time.time() - t0
    s = quiet_state()
    last = ((s or {}).get("apple") or {}).get("last") or {}
    check(joined is not None, f"merge {n}: {what} ({took:.2f} s to the list showing it)")
    say(f"  info merge {n}: done at {last.get('ms')} ms, {last.get('clicks')} clicks in {last.get('presses')} presses")
    check(took <= MERGE_MAX_S, f"merge {n}: within about one motion ({took:.2f} s, at most {MERGE_MAX_S} s)")
    p1 = pointer()
    check(near(p1, p0), f"merge {n}: the pointer back where it was ({p0} -> {p1})")
    check(not last.get("given_up"), f"merge {n}: done, not given up ({last.get('given_up')})")
    return s, took


def phase_merge():
    say("\n== merge: ctl merge N, one Shift press per gather ==")
    separate()
    if not bring_front("Alpha"):
        check(False, "Alpha in front before the merge")
        return
    s = quiet_state()
    bravo, alpha = windows_of(s, "Bravo"), windows_of(s, "Alpha")
    n = stage_number(s, bravo)
    if not check(n is not None and len(bravo) == 1, f"Bravo's stage of one is stage {n}"):
        return
    s, _ = merge_into_front(n, sorted(["Alpha 1", "Alpha 2", "Bravo 1"]), "Bravo joined Alpha's stage")
    if s:
        last = s["apple"].get("last") or {}
        check(last.get("clicks") == 1, f"merge {n}: one Shift-click ({last.get('clicks')})")
    # A stage of three into Charlie's: three clicks, front to back, in one press.
    if not bring_front("Charlie"):
        check(False, "Charlie in front before the second merge")
        return
    s = quiet_state()
    n = stage_number(s, alpha + bravo)
    if not check(n is not None, f"Alpha's and Bravo's stage of three is stage {n}"):
        return
    s, _ = merge_into_front(n, sorted(["Alpha 1", "Alpha 2", "Bravo 1", "Charlie 1"]), "a stage of three brought in")
    if s:
        last = s["apple"].get("last") or {}
        check(last.get("clicks") == 3, f"merge {n}: three Shift-clicks ({last.get('clicks')})")
        check(last.get("presses") == 1, f"merge {n}: in one Shift press ({last.get('presses')})")


def phase_split():
    say("\n== split: ctl new ID (the drag), ctl send N ID ==")
    separate()
    bring_front("Alpha")
    s = quiet_state()
    if not s:
        check(False, "the mirror settles before the split")
        return
    alpha, bravo = windows_of(s, "Alpha"), windows_of(s, "Bravo")
    current = s["apple"]["current"]
    if not check(len(alpha) == 2 and sorted(current) == alpha and bravo, f"Alpha's stage of two in front ({len(current)} windows)"):
        return
    w, keep = current[0], current[1]
    drags = route_runs(s, "drag")
    p0 = pointer()
    t0 = time.time()
    r = ctl("new", w)
    check(r.get("ok") and r.get("route") == "drag", f"new {w}: the drag ({r})")
    s = wait_until(lambda: (lambda s: s if s["apple"]["current"] == [keep] and [w] in others(s) else None)(ctl("state")), 6, "Alpha's front window in a stage of its own")
    took = time.time() - t0
    check(s is not None, f"new: Alpha's front window out to a stage of its own ({took:.2f} s to the list showing it)")
    s = quiet_state()
    last = ((s or {}).get("apple") or {}).get("last") or {}
    say(f"  info new: done at {last.get('ms')} ms, {last.get('drags')} drags, {last.get('focuses')} focuses; given up: {last.get('given_up')}")
    p1 = pointer()
    check(near(p1, p0), f"new: the pointer back where it was ({p0} -> {p1})")
    if s:
        check(route_runs(s, "drag") == drags + 1, f"new: one drag ({route_runs(s, 'drag') - drags})")
    # Bravo, in the strip, to that window's stage, and the stage in front back.
    n = stage_number(s, [w]) if s else None
    if not check(n is not None, f"the window taken out is stage {n}"):
        return
    p0 = pointer()
    r = ctl("send", n, bravo[0])
    check(r.get("ok"), f"send {n} {bravo[0]}: planned ({r})")
    s = wait_until(lambda: (lambda s: s if s["apple"]["current"] == [keep] and sorted([w, bravo[0]]) in [sorted(g) for g in others(s)] and idle_jobs(s) else None)(ctl("state")), 8, "Bravo with the window taken out, the stage in front back")
    check(s is not None, "send: Bravo joined the stage of the window taken out, the stage left is in front again")
    p1 = pointer()
    check(near(p1, p0), f"send: the pointer back where it was ({p0} -> {p1})")


def sample_pointer(stop, out):
    """Appends (time, point) every millisecond until `stop` is set."""
    while not stop.is_set():
        out.append((time.time(), pointer()))
        time.sleep(0.001)


def away(samples, home):
    """The longest spell the pointer was away from `home`, in ms."""
    longest, since = 0.0, None
    for t, p in samples:
        if not near(p, home) and since is None:
            since = t
        elif near(p, home) and since is not None:
            longest = max(longest, t - since)
            since = None
    if since is not None:
        longest = max(longest, samples[-1][0] - since)
    return longest * 1000


def phase_pointer():
    import threading
    say("\n== pointer: back where it was after every posted step ==")
    separate()
    if not bring_front("Alpha"):
        check(False, "Alpha in front")
        return
    s = quiet_state()
    in_strip = lambda s: next((w for name in ("Bravo", "Charlie") for w in windows_of(s, name) if stage_number(s, [w])), None)
    target = in_strip(s) if s else None
    if not check(target is not None, "a fixture window in the strip to Shift-click"):
        return
    for route in ("shift-click", "drag"):
        s = quiet_state()
        if route == "drag":
            target = next((w for w in s["apple"]["current"] if len(s["apple"]["current"]) >= 2), None)
            if not check(target is not None, "a stage of two in front to drag from"):
                return
        home = pointer()
        stop, samples = threading.Event(), []
        th = threading.Thread(target=sample_pointer, args=(stop, samples), daemon=True)
        th.start()
        r = ctl("apple", "try", route, target)
        time.sleep(1.0)
        stop.set()
        th.join()
        if not check(r.get("ok"), f"{route}: posted ({r})"):
            continue
        gone = away(samples, home)
        p1 = pointer()
        check(near(p1, home), f"{route}: the pointer back where it was ({home} -> {p1})")
        limit = 60 if route == "shift-click" else 1500
        check(gone <= limit, f"{route}: the pointer away {gone:.0f} ms (at most {limit})")
        quiet_state()


def pid_gone(app, timeout=5):
    return wait_until(lambda: not pids(app), timeout, "the app quit")


def relaunch(name):
    """Fixture `name` quit and opened again (Apple's gives it a stage of its own, in front),
    once the mirror shows its windows and no step of ours is under way."""
    _, app, n = FIX[name]
    for pid in pids(app):
        os.kill(pid, signal.SIGTERM)
    pid_gone(app)
    open_fixture(name)
    return wait_until(lambda: (lambda s: s if len(windows_of(s, name)) == n and idle_jobs(s) else None)(ctl("state")), 8, f"{name} open again")


def phase_open_here():
    say("\n== open-here: a newly opened app joins the stage in front ==")
    separate()
    write_config("[stages]\nnew_apps_join_current = true\n")
    check(ctl("reload").get("ok"), "config reloaded with [stages] new_apps_join_current on")
    try:
        for name, windows in (("Charlie", 1), ("Bravo", 2)):
            _, app, _ = FIX[name]
            for pid in pids(app):
                os.kill(pid, signal.SIGTERM)
            pid_gone(app)
            if not bring_front("Alpha"):
                check(False, f"{name}: Alpha in front before the launch")
                continue
            s = quiet_state()
            # The stage in front's windows only: the strip's thumbnails shift to make room for
            # the new one (live 11-16), which is Apple's own.
            front = s["apple"]["current"]
            listed0 = {w["id"]: w.get("listed") for w in s["windows"] if w["id"] in front}
            want = sorted(alpha_t for alpha_t in front_titles(s)) + [f"{name} {k}" for k in range(1, windows + 1)]
            want.sort()
            moved = False
            t0 = time.time()
            open_fixture(name, windows)
            # Timed to the moment the window list shows one stage (what is seen); the job's
            # own "done" comes after and is printed as info.
            end, joined = time.time() + 10, None
            while time.time() < end:
                s = ctl("state")
                for w in s.get("windows", []):
                    if w["id"] in listed0 and w.get("listed") != listed0[w["id"]]:
                        moved = True
                if front_titles(s) == want:
                    joined = s
                    break
                time.sleep(0.05)
            took = time.time() - t0
            if not check(joined is not None, f"{name}: opened into Alpha's stage ({took:.2f} s from the launch to the list showing it)"):
                continue
            s = quiet_state()
            last = ((s or {}).get("apple") or {}).get("last") or {}
            route = last.get("route")
            say(f"  info {name}: route {route}, done at {last.get('ms')} ms, first window at {last.get('first_window_ms')} ms, {last.get('clicks')} clicks in {last.get('presses')} presses")
            check(route in ("hide", "round_trip"), f"{name}: the route said ({route})")
            check(not last.get("given_up"), f"{name}: not given up ({last.get('given_up')})")
            check(took <= OPEN_HERE_MAX_S, f"{name}: within about {OPEN_HERE_MAX_S:.0f} s ({took:.2f} s)")
            if route == "hide":
                check(not moved, f"{name}: the stage in front's windows never moved (the hide route)")
                check(last.get("clicks") == windows and last.get("presses") == 1, f"{name}: one Shift-click per window, one press ({last.get('clicks')} in {last.get('presses')})")
    finally:
        write_config()
        ctl("reload")


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
    phases = sys.argv[1:] or list(PHASES)
    unknown = [p for p in phases if p not in PHASES]
    if unknown:
        sys.exit(f"unknown phase {unknown}; phases: {', '.join(PHASES)}")
    build_fixtures()
    original = read_apple()
    atexit.register(cleanup)
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, on_signal)
    set_apple(False)
    if start() is None:
        return 1
    runs = {"mirror": phase_mirror, "switch": phase_switch, "rejected": phase_rejected, "merge": phase_merge,
            "split": phase_split, "pointer": phase_pointer, "open-here": phase_open_here}
    for name in PHASES:
        if name in phases:
            runs[name]()
    say(f"\n{len(passed)} passed, {len(failed)} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
