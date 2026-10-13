#!/usr/bin/env python3
"""Live check of Spaces (Desktops). Two modes:

- **Guided** (default), run with a person at the Mac: it needs two Desktops and asks you to
  switch between them (a script never switches Spaces itself in this mode).
- **Hands-free** (`--auto`, or `SM_AUTO=1`): nobody at the Mac (unlocked, untouched). Every
  ask becomes an action: the app types the Desktop switch shortcut ("Move left/right a
  space", `ctl debug-space`; while the app is down, the one-shot `debug-post-space`
  launched through LaunchServices), and a fixture window goes in and out of full screen
  through Accessibility (`ctl debug-fullscreen`). The neighbouring Desktop is found from the
  start one (right first, then left); no Desktop is created, removed or reordered, and
  Desktops are told apart by the app's synthetic ids and by which fixture windows the window
  server shows. The two by-eye questions become screenshots (in `$SM_SHOTS`, else the test
  home; never the repo) printed with "by eye:" for a person to look at after the run; they
  count neither pass nor fail. A Desktop change the test did not ask for (someone uses the
  Mac) stops the run; cleanup still runs but never switches Desktops then. Cleanup (also on
  Ctrl-C and SIGTERM): full screen left on every fixture window, back to the start Desktop,
  the app quit, no window left parked, the fixtures quit. It refuses to start while another
  Stage Manager runs, the screen is locked, or a full-screen app's Space shows.
  `--auto --dry-run` prints the preflight and the planned steps and does nothing.

Run it as `scripts/test-stages.sh spaces [--auto [--dry-run]]`: that builds the app and the
fixtures, copies the app to the fixed dev location, then runs this script.

Like test_stages.py, the app runs with its own STAGE_MANAGER_HOME and STAGE_MANAGER_ONLY
(the fixtures only): only fixture windows ever move. Window captures are off in every run
(STAGE_MANAGER_NO_CAPTURE=1, through test_stages.start_app), so no phase here raises macOS's
screen-access alert. No other Stage Manager may run (both
would manage the fixtures). Guided prompts say exactly what to do; answer with Return.

Checks: each Desktop has its own stages and strip; the same app has stages on both; a card
click on Desktop 2 switches stages and never changes the Desktop (the strip panel is on the
Desktop shown); going back shows Desktop 1's stages; quit with a window parked on the other
Desktop brings it back (Accessibility reaching another Space); a relaunch rebuilds both
Desktops' stages; kill -9 with a window parked on the other Desktop: the guardian waits,
says what for, brings it back as soon as you go there (on screen outside the park zone for a
second, the guardian's log saying it brought it back) and only then ends; a full-screen
window's Space has no strip, entering and leaving full screen (twice) keeps the Desktop's id,
its stages and windows, and the engine's view does not flap; Mission Control and App Exposé
(typed through the app, left with Escape, never a Desktop change) hide the strip, which then
comes back on the same Desktop with each Desktop's pane still on its own Desktop.

Every app run's log is kept (stage-manager.run-NN.log in the test home, renamed from the
day's YYYY-MM-DD.log; the guardian writes on into its run's file).
"""

import json
import os
import plistlib
import re
import signal
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import test_stages as ts  # noqa: E402

say, check, ctl, state = ts.say, ts.check, ts.ctl, ts.state

ARGS = sys.argv[1:]
AUTO = "--auto" in ARGS or os.environ.get("SM_AUTO") == "1"
DRY = "--dry-run" in ARGS


def ask(what):
    say(f"\n>>> {what}")
    input("    then press Return… ")


def yes(question):
    return input(f"\n??? {question} [y/N] ").strip().lower().startswith("y")


def wait_for(what, cond, timeout):
    """Polls `cond` until it holds for a second; no Return needed. None on timeout."""
    end, since = time.time() + timeout, None
    while time.time() < end:
        guard()
        try:
            ok = cond()
        except Exception:  # the app may be busy with the Space change
            ok = False
        if ok:
            since = since or time.time()
            if time.time() - since >= 1.0:
                return True
        else:
            since = None
        time.sleep(0.3)
    say(f"    (gave up waiting for {what})")
    return None


def holds(cond, seconds):
    """True when `cond` holds at every look (every 0.2 s) for `seconds`."""
    end = time.time() + seconds
    while time.time() < end:
        guard()
        try:
            if not cond():
                return False
        except Exception:  # noqa: BLE001 - a busy app counts as a miss
            return False
        time.sleep(0.2)
    return True


def switch_to(what):
    """Asks for a Desktop switch and notices it by itself (the shown Space changes)."""
    before = current_space()
    say(f"\n>>> {what} (the test notices by itself)")
    if not wait_for("the Desktop switch", lambda: current_space() != before, 120):
        input("    not seen; switch, then press Return… ")


def spaces():
    d = ctl("dump")
    return d.get("spaces") or {}


def current_space():
    return state()["space"]


def titles_on_space(s, space):
    ids = {w["id"]: w["title"] for w in s["windows"]}
    return sorted(ids[i] for st in s["stages"] if st["space"] == space for i in st["windows"])


def strip_titles(s):
    ids = {w["id"]: w["title"] for w in s["windows"]}
    by_number = {(st["space"], st["number"]): st for st in s["stages"]}
    out = []
    for n in s["strip"]:
        st = by_number.get((s["space"], n))
        out += [ids[i] for i in (st or {}).get("windows", [])]
    return sorted(out)


def parked_anywhere():
    """Fixture windows in the park zone on any Space (the window server lists other Spaces'
    windows too, off screen, with their frames)."""
    wins, d = ts.fixture_windows()
    return [w for w in wins if ts.is_parked(w["bounds"], d)]


def onscreen_ids(ids):
    """Which of these window ids the window server lists on screen now (no app needed)."""
    wins, _ = ts.fixture_windows()
    return [w["id"] for w in wins if w["id"] in ids and w["onscreen"]]


def switch_to_windows(what, ids):
    """Asks for a Desktop switch and notices it from the window server alone (the app may be
    gone): one of `ids` (windows of the target Desktop) shows on screen."""
    say(f"\n>>> {what} (the test notices by itself)")
    if not wait_for("the Desktop switch", lambda: bool(onscreen_ids(ids)), 120):
        input("    not seen; switch, then press Return… ")


def guardian_lines():
    return [line.split("guardian: ", 1)[-1][:200] for line in ts.log_text().splitlines() if "guardian: " in line]


class GuardianBack:
    """The guardian drill's verdict once the parked window's Desktop shows: `back` once every
    parked window is listed on screen outside the park zone for a whole second (one listing
    while a Desktop slides in may show an in-between frame); `early` when the guardian is
    gone while they are not back, with GRACE for the window server to list a window the
    guardian moved just before it ended."""
    HOLD, GRACE = 1.0, 1.5

    def __init__(self):
        self.since = self.gone_at = None
        self.back = self.early = False

    def step(self, now, guardian_alive, out):
        """One sample: the guardian looked at first, then the windows."""
        if out:
            self.since = now if self.since is None else self.since
            self.back = self.back or now - self.since >= self.HOLD
        else:
            self.since = None
            if not guardian_alive:
                self.gone_at = now if self.gone_at is None else self.gone_at
                self.early = self.early or now - self.gone_at >= self.GRACE
        return self.back


def watch_guardian_until_back(app, ids, timeout):
    """Samples the guardian of app pid `app` and windows `ids` until they are back
    ([`GuardianBack`]); returns the verdict."""
    v, end = GuardianBack(), time.time() + timeout
    while time.time() < end and not v.back:
        guard()
        alive = bool(ts.pids_of(f"stage-manager --guardian {app}$"))
        try:
            wins, d = ts.fixture_windows()
        except Exception:  # noqa: BLE001
            wins, d = [], None
        mine = [w for w in wins if w["id"] in ids]
        out = d is not None and len(mine) == len(ids) and all(w["onscreen"] and not ts.is_parked(w["bounds"], d) for w in mine)
        v.step(time.time(), alive, out)
        time.sleep(0.2)
    return v


def clear_of_strip(s):
    """Moves the shown windows of the Desktop shown right of the strip column: the strip hides
    while an active window overlaps it (Apple's auto-hide), and a click there would reach the
    desktop instead of a card."""
    vis = s["display"]["visible"]
    left = vis["x"] + s["strip_width"] + 60
    for w in s["windows"]:
        if w["space"] == s["space"] and not w["parked"] and w["role"] == "standard" and w["restore"]["x"] < left:
            r = w["restore"]
            ctl("debug-frame", w["id"], left, r["y"], r["w"], r["h"])
    time.sleep(0.9)


def other_stage_managers():
    ours = ts.app_pid()
    return [p for p in ts.pids_of("Stage Manager.app/Contents/MacOS/stage-manager$") if p != ours]


# ----- hands-free mode (--auto) ------------------------------------------------------------


class Interrupted(BaseException):
    """The Desktop shown changed without the test asking. A BaseException, so the polling
    helpers' `except Exception` never swallows it."""


class Stopped(BaseException):
    """SIGTERM."""


class Abort(Exception):
    """The run cannot go on (no neighbouring Desktop, a switch not seen)."""


def guard():
    """Raises Interrupted once the watcher saw a Desktop change nobody asked for (auto mode
    only; never during cleanup)."""
    if WATCH is not None and WATCH.reason and not CLEANING:
        raise Interrupted(WATCH.reason)


def probe_all():
    """Every fixture window as the window server lists it, in one probe."""
    r = ts.run([ts.BIN, "probe-windows", *ts.ALL_IDS], timeout=10)
    return [w for w in json.loads(r.stdout)["windows"] if w["layer"] == 0]


class Watch:
    """Watches, from the window server alone (works while the app is down), that the Desktop
    shown is the one the test expects: Desktop 1 shows its fixture windows and none of
    Desktop 2's, and so on. Paused while the test itself switches. A mismatch for three
    looks in a row (about 1.5 s) is a change nobody asked for; the screen locking (looked at
    every few seconds) stops the run too."""

    def __init__(self):
        self.lock = threading.Lock()
        self.ids = {"d1": set(), "d2": set()}
        self.expect = None  # None (paused), ("desk", "d1"|"d2") or ("fullscreen", window id)
        self.where = "d1"
        self.reason = None
        self.stopped = False
        self.misses = 0
        self.thread = threading.Thread(target=self._loop, name="watch", daemon=True)
        self.thread.start()

    def markers(self, name, ids):
        with self.lock:
            self.ids[name] = set(ids)

    def pause(self):
        with self.lock:
            self.expect, self.misses = None, 0

    def desk(self, name):
        with self.lock:
            self.expect, self.where, self.misses = ("desk", name), name, 0

    def fullscreen(self, wid):
        with self.lock:
            self.expect, self.misses = ("fullscreen", wid), 0

    def stop(self):
        self.stopped = True

    def mismatch(self, on):
        """What is wrong with the window ids `on` the screen shows, or None."""
        kind, what = self.expect
        d1, d2 = self.ids["d1"], self.ids["d2"]
        if kind == "fullscreen":
            if on & d1 or on & (d2 - {what}):
                return "a Desktop shows instead of the full-screen window's Space"
            return None
        mine, other = (d1, d2) if what == "d1" else (d2, d1)
        if on & other:
            return f"Desktop {2 if what == 'd1' else 1}'s windows show while Desktop {what[1]} is expected"
        if mine and not on & mine:
            return f"none of Desktop {what[1]}'s windows shows"
        return None

    def _loop(self):
        looks = 0
        while not self.stopped:
            time.sleep(0.5)
            looks += 1
            if looks % 8 == 0 and not self.reason and not CLEANING and screen_locked():
                self.reason = "the screen locked"
            with self.lock:
                expect = self.expect
            if expect is None or self.reason:
                continue
            try:
                on = {w["id"] for w in probe_all() if w["onscreen"]}
            except Exception:  # noqa: BLE001 - a busy window server: look again
                continue
            with self.lock:
                if self.expect != expect:
                    continue
                problem = self.mismatch(on)
                self.misses = self.misses + 1 if problem else 0
                if self.misses >= 3:
                    self.reason = (f"{problem}: the Desktop shown changed without the test asking "
                                   "(someone using the Mac, or the app moved it)")


WATCH = None
CLEANING = False
ROUTE = {"toward": None, "back": None, "numpad": False}
IDS = {"d1": None, "d2": None}
STARTED = time.time()
BY_EYE = []
OPPOSITE = {"right": "left", "left": "right"}
# An app whose windows live only on that Desktop: activating it is macOS's own way there (the
# fallback when a typed shortcut is not seen).
ONLY_ON = {"d1": "a", "d2": "b"}


def shots_dir():
    """$SM_SHOTS (else $SM_SCREENSHOTS, else the test home), never inside the repo."""
    d = os.environ.get("SM_SHOTS") or os.environ.get("SM_SCREENSHOTS") or ts.HOME
    real, repo = os.path.realpath(d), os.path.realpath(ts.ROOT)
    if real == repo or real.startswith(repo + os.sep):
        say(f"    (screenshots never go into the repo; using {ts.HOME})")
        d = ts.HOME
    os.makedirs(d, exist_ok=True)
    return d


def shot(name, region=None):
    """A screenshot now (the main display, or `region` x,y,w,h); returns its path or None."""
    path = os.path.join(shots_dir(), f"spaces-auto-{time.strftime('%H%M%S')}-{name}.png")
    args = ["screencapture", "-x"] + (["-R", ",".join(f"{v:.0f}" for v in region)] if region else ["-m"]) + [path]
    r = subprocess.run(args, capture_output=True, text=True)
    if r.returncode == 0 and os.path.exists(path):
        return path
    say(f"    screenshot failed: {r.stderr.strip()}")
    return None


def by_eye(question, path):
    BY_EYE.append((question, path))
    say(f"    by eye: {path or '(no screenshot)'} — {question}")


def space_now():
    r = ctl("space")
    return r if r.get("ok") else None


def moved(before, now):
    """The Python side of SpaceView::moved_from."""
    return bool(now) and not now["settling"] and (
        now["changes"] > before["changes"] or now["space"] != before["space"] or now["fullscreen"] != before["fullscreen"])


def space_step(direction, numpad=False):
    """`ctl debug-space`: the app types the shortcut and reports the Space change. A change
    seen only after the app's wait still counts (looked at once more a second later)."""
    words = ["debug-space", direction] + (["numpad"] if numpad else [])
    r = ctl(*words)
    if not r.get("ok"):
        raise Abort(f"ctl {' '.join(words)}: {r.get('error')}")
    if not r.get("changed"):
        time.sleep(1.0)
        now = space_now()
        if moved(r["before"], now):
            r.update(changed=True, after=now)
    say(f"    typed {r.get('key')}{' +numpad' if numpad else ''} ({direction}): "
        + (f"Space {r['before']['space']} → {r['after']['space']}"
           + (" (full screen)" if r["after"]["fullscreen"] else "") if r.get("changed") else "no Space change")
        + f" in {r.get('ms')} ms")
    return r


def find_neighbour(what):
    """The first trip: right of the start Desktop, else left; a full-screen app's Space next
    door is left again at once. With neither, the same with the numeric-pad flag a hardware
    arrow key carries. Remembers the way there and back."""
    say(f"\n>>> {what}: the app types the shortcut (right first, then left)")
    WATCH.pause()
    start = space_now()
    if not start:
        raise Abort("the app does not say which Space shows")
    for numpad in (False, True):
        for direction in ("right", "left"):
            r = space_step(direction, numpad)
            if not r.get("changed"):
                continue
            if r["after"]["fullscreen"]:
                say(f"    the Space to the {direction} is a full-screen app's; going back")
                back = space_step(OPPOSITE[direction], numpad)
                if not back.get("changed") or back["after"]["fullscreen"] or back["after"]["space"] != start["space"]:
                    raise Abort("could not come back from the full-screen Space next door")
                continue
            ROUTE.update(toward=direction, back=OPPOSITE[direction], numpad=numpad)
            WATCH.desk("d2")
            return
    WATCH.desk("d1")
    raise Abort("no Desktop next to the start one was reached: the shortcut typed by the app was not seen "
                "(right and left, with and without the numeric-pad flag), or there is only one Desktop")


def activate_only_on(name):
    """Activates the fixture app whose windows live only on Desktop `name`: macOS goes to
    that Desktop (the fallback when a typed shortcut is not seen)."""
    path = ts.EXTRA[ONLY_ON[name]][1]
    say(f"    shortcut not seen; activating {os.path.basename(path)} (its windows live only on Desktop {name[1]})")
    subprocess.run(["open", path], capture_output=True)


def auto_switch(name, what):
    """A Desktop switch through the app: types the shortcut toward Desktop `name`; when no
    change is seen, activates an app living only there."""
    say(f"\n>>> {what}: the app types the shortcut")
    if ROUTE["toward"] is None:
        raise Abort("no way to the other Desktop is known")
    WATCH.pause()
    direction = ROUTE["toward"] if name == "d2" else ROUTE["back"]
    before = space_now()
    if not before:
        raise Abort("the app does not say which Space shows")
    r = space_step(direction, ROUTE["numpad"])
    if not r.get("changed"):
        activate_only_on(name)
        if not wait_for("the Desktop switch", lambda: moved(before, space_now()), 8):
            raise Abort(f"the switch to Desktop {name[1]} was not seen")
    now = space_now()
    if now and IDS[name] is not None and now["space"] != IDS[name]:
        say(f"    (the engine shows Space {now['space']}; Desktop {name[1]} was {IDS[name]})")
    WATCH.desk(name)


def appless_switch(name, what, ids):
    """A Desktop switch while the app is down (the guardian drill): the one-shot
    `debug-post-space`, launched through LaunchServices so it has the app's Accessibility;
    noticed from the window server (one of `ids` shows); the activation fallback after."""
    say(f"\n>>> {what}: a one-shot of the app types the shortcut")
    if ROUTE["toward"] is None:
        raise Abort("no way to the other Desktop is known")
    WATCH.pause()
    direction = ROUTE["toward"] if name == "d2" else ROUTE["back"]
    args = ["open", "-n", "-g", ts.APP, "--env", "STAGE_MANAGER_TEST=1", "--env", f"STAGE_MANAGER_HOME={ts.HOME}",
            "--args", "debug-post-space", direction] + (["numpad"] if ROUTE["numpad"] else [])
    subprocess.run(args, capture_output=True)
    if not wait_for("the Desktop switch", lambda: bool(onscreen_ids(ids)), 8):
        activate_only_on(name)
        if not wait_for("the Desktop switch", lambda: bool(onscreen_ids(ids)), 8):
            raise Abort(f"the switch to Desktop {name[1]} was not seen (app down)")
    WATCH.desk(name)


def go(name, guided_prompt):
    """Guided: asks for the switch. Auto: the app switches."""
    if AUTO:
        auto_switch(name, guided_prompt)
    else:
        switch_to(guided_prompt)


def sample_onscreen(ids, stop, hits):
    """Looks at the window server every ~0.1 s until `stop` is set; records any of `ids` on
    screen (a jump to their Desktop, however short)."""
    while not stop.is_set():
        try:
            on = [w["id"] for w in probe_all() if w["onscreen"] and w["id"] in ids]
        except Exception:  # noqa: BLE001
            on = []
        if on:
            hits.append((round(time.time(), 2), on))
        time.sleep(0.1)


def screen_locked(ioreg_text=None):
    """True while the session's screen is locked: `ioreg -n Root -d1` lists
    CGSSessionScreenIsLocked (absent when unlocked) in the console user's entry."""
    if ioreg_text is None:
        ioreg_text = subprocess.run(["ioreg", "-n", "Root", "-d1"], capture_output=True, text=True).stdout
    m = re.search(r'"CGSSessionScreenIsLocked"\s*=\s*(\w+)', ioreg_text)
    return bool(m) and m.group(1) != "No"


def covering_windows():
    """On-screen windows covering the whole main display (a full-screen app's Space)."""
    r = ts.run([ts.BIN, "probe-screen"], timeout=10)
    try:
        return json.loads(r.stdout).get("covering", [])
    except (json.JSONDecodeError, AttributeError):
        return [{"error": (r.stdout + r.stderr).strip()[:200]}]


def switch_shortcuts():
    """"Move left/right a space" as the user's settings have them: id → enabled."""
    r = subprocess.run(["defaults", "export", "com.apple.symbolichotkeys", "-"], capture_output=True)
    try:
        keys = plistlib.loads(r.stdout).get("AppleSymbolicHotKeys", {})
    except Exception:  # noqa: BLE001 - absent or unreadable: macOS defaults (on)
        keys = {}
    return {n: bool(keys.get(str(n), {}).get("enabled", True)) for n in (79, 81)}


def preflight():
    """Why a hands-free run must not start now (empty: it may)."""
    problems = []
    others = other_stage_managers()
    if others:
        problems.append(f"another Stage Manager runs (pids {others}); quit it first")
    if screen_locked():
        problems.append("the screen is locked (CGSSessionScreenIsLocked in `ioreg -n Root -d1`)")
    cover = covering_windows()
    if cover:
        problems.append(f"the Space shown looks like a full-screen app's (windows covering the display: {cover})")
    keys = switch_shortcuts()
    off = [n for n, on in keys.items() if not on]
    if off:
        problems.append(f"Move left/right a space is turned off in Keyboard Shortcuts (hotkeys {off})")
    return problems


PLAN = """\
planned steps (hands-free):
  preflight   refuse unless: no other Stage Manager, screen unlocked, a Desktop shows (not full screen),
              Move left/right a space on
  Desktop 1   open Fixture 1-2 and Alpha 1; start the app; ctl space says not full screen
  first trip  ctl debug-space right (else left; a full-screen Space next door is left at once; then the same
              with the numeric-pad flag); the way there and back is remembered; Desktops by synthetic id
  Desktop 2   Fixture 3 (SIGUSR1) and Bravo 1; checks as guided; by eye: screenshot of the strip column
  card click  debug-click on a card while the window server is sampled every ~0.1 s for Desktop 1's
              windows (any = a jump); by eye: screenshot of the main display
  expose      on Desktop 2: every pane Transient; ctl debug-expose mission-control, then app-windows (the
              user's shortcuts; a fixture frontmost first); while it shows the window server no longer
              lists the strip on screen; by eye: screenshot; ctl debug-expose escape; same Desktop, the
              strip back and drawn as before, a card click switches, each Desktop's pane on its own Desktop
  back        ctl debug-space <back> to Desktop 1; checks as guided (Desktop 2's pane stays there)
  quit        quit with a window parked on Desktop 2; relaunch; ctl debug-space <toward>; checks as guided
  guardian    debug-space <back> to Desktop 1; kill -9 the app; checks: still parked, the guardian waits;
              `open -n <app> --args debug-post-space <toward>` (app down) to Desktop 2, else activate Bravo;
              checks: restored (held a second), the guardian alive until then and logged it, it ends
  full screen twice: ctl debug-fullscreen <a shown Fixture window> on, checks, off, checks
  cleanup     (also on Ctrl-C/SIGTERM) full screen off on every fixture window; back to the start Desktop
              (not when someone else changed the Desktop); quit the app; no window parked; quit the fixtures
  watch       all along, the window server must show the expected Desktop's fixture windows; a change
              nobody asked for stops the run"""


def install_guards():
    """Auto mode: every ctl call, check and wait looks for a Desktop change nobody asked for."""
    global check, ctl
    plain_ctl, plain_check, plain_wait = ts.ctl, ts.check, ts.wait_until

    def guarded_ctl(*words):
        guard()
        return plain_ctl(*words)

    def guarded_check(cond, msg):
        guard()
        return plain_check(cond, msg)

    def guarded_wait(fn, timeout, what):
        def step():
            guard()
            return fn()
        return plain_wait(step, timeout, what)

    ts.ctl, ts.check, ts.wait_until = guarded_ctl, guarded_check, guarded_wait
    check, ctl = guarded_check, guarded_ctl


def on_sigterm(_sig, _frame):
    raise Stopped()


def auto_cleanup(interrupted):
    """Full screen left on every fixture window, back to the start Desktop (unless someone
    else changed it), the app quit, no window left parked, the fixtures quit. Never raises."""
    global CLEANING
    CLEANING = True
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    say("\n== cleanup ==")
    if WATCH:
        WATCH.pause()
    try:
        if ts.app_pid():
            for w in probe_all():
                r = ctl("debug-fullscreen", w["id"], "off")
                if r.get("changed"):
                    say(f"    window {w['id']} left full screen")
            wait_for("full screen to end", lambda: (lambda sp: sp is None or (not sp["fullscreen"] and not sp["settling"]))(space_now()), 15)
    except Exception as e:  # noqa: BLE001
        say(f"    leaving full screen: {e}")
    try:
        if interrupted:
            say("    not switching Desktops: someone else changed the Desktop shown")
        elif IDS["d1"] is not None and ROUTE["toward"]:
            back_to_start()
    except BaseException as e:  # noqa: BLE001 - cleanup goes on
        say(f"    could not return to the start Desktop ({e}); it was Space {IDS['d1']}")
    ts.stop_app()
    try:
        left = parked_anywhere()
        ts.check(not left, "cleanup: no fixture window parked on any Desktop" + (f" (left: {[w['id'] for w in left]})" if left else ""))
    except Exception as e:  # noqa: BLE001
        say(f"    parked check: {e}")
    ts.stop_all_fixtures()
    if WATCH:
        WATCH.stop()


def back_to_start():
    """The Desktop the run started on, only from Desktop 2 as the window server shows it
    (anywhere else, nobody knows the way): through the app while it runs, else the one-shot."""
    d1_ids, d2_ids = WATCH.ids["d1"], WATCH.ids["d2"]
    on = {w["id"] for w in probe_all() if w["onscreen"]}
    if on & d1_ids:
        return
    if not on & d2_ids:
        say(f"    not on Desktop 1 or 2 as far as the window server shows; not switching (the start was Space {IDS['d1']})")
        return
    if ts.app_pid():
        auto_switch("d1", "Back to the start Desktop")
    else:
        appless_switch("d1", "Back to the start Desktop", d1_ids)


# ----- the check ---------------------------------------------------------------------------


def run():
    if other_stage_managers():
        say("STOP: another Stage Manager is running (the owner's?). Quit it first; both would manage the fixtures.")
        ts.failed.append("another Stage Manager runs")
        return
    if AUTO:
        problems = preflight()
        if problems:
            for p in problems:
                say(f"STOP: {p}")
            ts.failed.append("preflight: " + "; ".join(problems))
            return
    else:
        ask("Make sure there are two Desktops (Mission Control) and Desktop 1 is shown")
    p = os.path.join(ts.HOME, "state.json")
    if os.path.exists(p):
        os.remove(p)

    say("\n== Desktop 1: Fixture 1-2 and Alpha 1 ==")
    ts.start_fixture(2)
    ts.start_extra("a", 1)
    s = ts.start_app(3, extra_env={"STAGE_MANAGER_TEST": "1"})
    if not check(s is not None, "app up with three windows"):
        return
    d1 = current_space()
    if AUTO:
        IDS["d1"] = d1
        sp = space_now()
        if not sp or sp["fullscreen"]:
            raise Abort(f"the app does not see a Desktop at the start ({sp})")
        WATCH.markers("d1", [w["id"] for w in probe_all() if w["onscreen"]])
        WATCH.desk("d1")
    check(len(s["stages"]) == 2, f"two stages on Desktop 1 ({ts.stage_titles(s)})")

    say("\n== Desktop 2: Fixture 3 (the same app) and Bravo 1 ==")
    if AUTO:
        find_neighbour("Switch to Desktop 2")
    else:
        switch_to("Switch to Desktop 2 (Control-Right Arrow)")
    pid = ts.pids_of(ts.FIX_EXE)
    if pid:
        os.kill(pid[0], signal.SIGUSR1)
    ts.start_extra("b", 1)
    s = ts.wait_state(lambda x: len(x["windows"]) >= 5, "five managed windows", timeout=10)
    if not check(s is not None, "Desktop 2's windows are managed"):
        return
    d2 = s["space"]
    if AUTO:
        IDS["d2"] = d2
        WATCH.markers("d2", [w["id"] for w in probe_all() if w["onscreen"] and w["id"] not in WATCH.ids["d1"]])
    check(d2 != d1, f"Desktop 2 is its own Space ({d1} then {d2})")
    check(titles_on_space(s, d2) == ["Bravo 1", "Fixture 3"], f"Desktop 2's stages hold only its windows: {titles_on_space(s, d2)}")
    check(set(titles_on_space(s, d1)) == {"Alpha 1", "Fixture 1", "Fixture 2"}, "Desktop 1's stages are untouched")
    check(set(strip_titles(s)) <= {"Bravo 1", "Fixture 3"}, f"the strip shows only Desktop 2's windows: {strip_titles(s)}")
    if AUTO:
        vis = s["display"]["visible"]
        by_eye("the strip on Desktop 2 shows only Bravo or Fixture 3 (no Fixture 1, 2 or Alpha)",
               shot("strip-desktop-2", (vis["x"], vis["y"], s["strip_width"] + 40, vis["h"])))
    else:
        check(yes("Does the strip on Desktop 2 show only Bravo or Fixture 3 (no Fixture 1, 2 or Alpha)?"), "strip on Desktop 2 (by eye)")

    say("\n== a card click on Desktop 2 never changes the Desktop ==")
    vis = s["display"]["visible"]
    x, y = vis["x"] + 70, vis["y"] + vis["h"] / 2
    clear_of_strip(s)
    s = state()
    panel = wait_for("the strip panel on Desktop 2",
                     lambda: (lambda p: p.get("on_active_space") and p.get("ordered_in") and p.get("shown"))(state().get("strip_panel") or {}), 4)
    say(f"    strip panel: {state().get('strip_panel')}")
    say(f"    strip panels (one per Desktop): {state().get('strip_panels')}")
    check(panel is True, "the strip panel is on Desktop 2, ordered in and slid in")
    listed = ts.strip_panel([w for w in ts.our_windows()])
    check(listed is not None, f"the window server lists the strip on screen ({listed and listed['bounds']})")
    before = s["active"]
    if AUTO:
        # A jump to Desktop 1 is the sampler's to judge, not the watcher's.
        WATCH.pause()
        stop, hits = threading.Event(), []
        sampler = threading.Thread(target=sample_onscreen, args=(WATCH.ids["d1"], stop, hits), daemon=True)
        sampler.start()
    r = ctl("debug-click", x, y)
    check(r.get("ok"), f"clicked the card ({r.get('error', 'ok')})")
    s = ts.wait_state(lambda x: x["active"] != before, "the card click to switch", timeout=4)
    check(s is not None, "the card click switched stages")
    time.sleep(1.5)
    check(current_space() == d2, "still on Desktop 2 (as the engine sees it)")
    if AUTO:
        stop.set()
        sampler.join(timeout=3)
        check(not hits, "no Desktop 1 window on screen during the click and 1.5 s after (window server, every ~0.1 s)"
              + (f": {hits[:3]}" if hits else ""))
        WATCH.desk("d2")
        by_eye("the screen shows Desktop 2 (no jump to Desktop 1; a still cannot show a short jump, the sampling above does)",
               shot("after-card-click"))
    else:
        check(not yes("Did the screen jump to Desktop 1 (even for a moment)?"), "no jump to Desktop 1 (by eye)")
    check(ctl("switch", 1).get("ok") and current_space() == d2, "a hotkey switch stays on Desktop 2")

    say("\n== Mission Control and App Exposé on Desktop 2: the strip hides, then comes back ==")
    expose_on_desktop_2(d1, d2)

    say("\n== back on Desktop 1 ==")
    go("d1", "Switch back to Desktop 1 (Control-Left Arrow)")
    s = ts.wait_state(lambda x: x["space"] == d1, "Desktop 1 recognized", timeout=5)
    check(s is not None, "Desktop 1 recognized by its windows")
    if s:
        check(set(strip_titles(s)) <= {"Alpha 1", "Fixture 1", "Fixture 2"}, f"Desktop 1's strip: {strip_titles(s)}")

        def own_pane():
            panes = state().get("strip_panels") or []
            mine = [p for p in panes if p["active"]]
            return len(panes) == 2 and bool(mine) and mine[0]["on_active_space"] and mine[0]["space"] == d1

        back_home = wait_for("Desktop 1's own strip panel", own_pane, 4)
        check(back_home is True, "Desktop 1's own strip panel is back, on the Desktop shown; Desktop 2's stays there"
              f" ({state().get('strip_panels')})")
        others = [p for p in state().get("strip_panels") or [] if not p["active"]]
        check(bool(others) and all(p["space"] == d2 and not p["on_active_space"] for p in others),
              f"Desktop 2's pane did not follow to Desktop 1 (Transient panes stay on their Desktop): {others}")
        ok, told = ts.panes_hide_in_expose(state())
        check(ok, f"both panes are Transient, not Stationary, joining no other Space ({told})")
    sp = spaces()
    check(len(sp.get("list", [])) == 2, f"ctl dump lists two Spaces: {sp}")

    say("\n== quit with a window parked on Desktop 2 ==")
    s = state()
    parked_d2 = [w["title"] for w in s["windows"] if w["space"] == d2 and w["parked"]]
    check(bool(parked_d2), f"a window is parked on Desktop 2: {parked_d2}")
    ts.stop_app()
    left = parked_anywhere()
    check(not left, "after quit no fixture window is parked on any Desktop" + (f" (left: {[w['id'] for w in left]})" if left else ""))
    if left:
        say("    (Accessibility could not move a window on the other Desktop; the guardian watches: go there to check)")

    say("\n== relaunch: both Desktops' stages come back ==")
    s = ts.start_app(3, extra_env={"STAGE_MANAGER_TEST": "1"})
    check(s is not None and s["space"] == d1, "Desktop 1 recognized at launch with its saved id")
    check(spaces().get("saved_spaces_not_visited") == 1, "Desktop 2 waits for its first visit")
    go("d2", "Switch to Desktop 2")
    s = ts.wait_state(lambda x: x["space"] == d2, "Desktop 2 rebuilt", timeout=6)
    check(s is not None, "Desktop 2 rebuilt with its saved id")
    if s:
        check(titles_on_space(s, d2) == ["Bravo 1", "Fixture 3"], f"Desktop 2's windows in its stages: {titles_on_space(s, d2)}")

    say("\n== kill -9 with a window parked on the other Desktop: the guardian ==")
    s = state()
    if s["space"] == d2 and not [w for w in s["windows"] if w["space"] == d2 and w["parked"]]:
        ctl("switch", 1 if s["active"] != 1 else 2)
    s = state()
    check(any(w["space"] == d2 and w["parked"] for w in s["windows"]), "a window is parked on Desktop 2")
    d2_ids = [w["id"] for w in s["windows"] if w["space"] == d2]
    go("d1", "Switch to Desktop 1")
    time.sleep(1.5)
    app = ts.app_pid()
    guardians = ts.guardian_pids(app)
    check(bool(guardians), "the guardian runs")
    os.kill(app, signal.SIGKILL)
    ts.wait_gone(app)
    time.sleep(3)
    still = bool(ts.pids_of(f"stage-manager --guardian {app}$"))
    parked = [w["id"] for w in parked_anywhere()]
    say(f"    guardian still watching: {still}; parked anywhere now: {parked}")
    check(still and bool(parked), "the guardian waits while the window's Desktop is not shown")
    check(any("waiting for" in line and "on_desktops_not_shown=[\"" in line for line in guardian_lines()),
          "the guardian's log says it waits for a window on a Desktop not shown")
    if AUTO:
        appless_switch("d2", "Switch to Desktop 2", d2_ids)
    else:
        switch_to_windows("Switch to Desktop 2", d2_ids)
    v = watch_guardian_until_back(app, parked, 12)
    check(v.back, f"Desktop 2's parked window {parked} is back: on screen outside the park zone for a second")
    check(not v.early, "the guardian kept watching until the window was seen back (it never ended while it was parked)")
    lines = guardian_lines()
    told = [i for i in parked if any("window brought back" in line and re.search(rf"(?<![a-z])id={i}\b", line) for line in lines)]
    check(told == parked, f"the guardian's log says it brought {parked} back (brought back: {told})")
    ended = wait_for("the guardian to end", lambda: not ts.pids_of(f"stage-manager --guardian {app}$"), 6)
    check(ended is True, "the guardian ended once nothing was left")
    for line in guardian_lines()[-14:]:
        say(f"    log: {line}")

    say("\n== a full-screen app's Space has no strip; the Desktop keeps its id ==")
    # Only the Desktop shown has its windows managed at launch (here Desktop 2's two); the
    # others' wait for a visit.
    shown_now = [w for w in ts.fixture_windows()[0] if w["onscreen"]]
    s = ts.start_app(len(shown_now), extra_env={"STAGE_MANAGER_TEST": "1"})
    if not check(s is not None, f"the app is up with the {len(shown_now)} windows of the Desktop shown"):
        return
    desk = s["space"]
    stages_before = sorted(sorted(map(str, st["windows"])) for st in s["stages"] if st["space"] == desk)
    windows_before = sorted(w["id"] for w in s["windows"])
    ids_before = sorted(x["id"] for x in spaces().get("list", []))
    for round_no in (1, 2):
        if AUTO:
            wid = fullscreen_on(round_no, desk)
            if wid is None:
                break
            if not wait_for("a full-screen window", lambda: spaces().get("fullscreen") is True, 20):
                say("    full-screen check skipped")
                break
            WATCH.fullscreen(wid)
        else:
            say(f"\n>>> Round {round_no} of 2: make a Fixture window full screen (green button) within a minute;")
            say("    the test notices by itself, then asks you to leave full screen")
            if not wait_for("a full-screen window", lambda: spaces().get("fullscreen") is True, 60):
                say("    full-screen check skipped")
                break
        check(True, f"round {round_no}: the engine knows a full-screen Space shows")
        check(holds(lambda: spaces().get("fullscreen") is True, 3), f"round {round_no}: it stays known for three seconds (no flapping)")
        panel = ts.strip_panel([w for w in ts.our_windows() if w.get("onscreen", True)])
        check(panel is None, f"round {round_no}: no strip on the full-screen Space ({panel and panel['bounds']})")
        if AUTO:
            WATCH.pause()
            r = ctl("debug-fullscreen", wid, "off")
            check(r.get("ok"), f"round {round_no}: window {wid} asked to leave full screen ({r.get('error', r.get('via', 'ok'))})")
            back = wait_for("full screen to end", lambda: (lambda d: d.get("fullscreen") is False and not d.get("settling"))(spaces()), 30)
            WATCH.desk(WATCH.where)
        else:
            say("\n>>> Leave full screen (green button or Control-Command-F); the test notices by itself")
            back = wait_for("full screen to end", lambda: (lambda d: d.get("fullscreen") is False and not d.get("settling"))(spaces()), 120)
        check(back is True, f"round {round_no}: back on a Desktop")
        time.sleep(2.5)  # past the once-a-second checks
        s = state()
        check(s["space"] == desk, f"round {round_no}: the Desktop keeps its id ({desk} then {s['space']})")
        ids_now = sorted(x["id"] for x in spaces().get("list", []))
        check(ids_now == ids_before, f"round {round_no}: no Space id minted ({ids_before} then {ids_now})")
        stages_now = sorted(sorted(map(str, st["windows"])) for st in s["stages"] if st["space"] == desk)
        check(stages_now == stages_before, f"round {round_no}: the Desktop's stages are unchanged")
        check(sorted(w["id"] for w in s["windows"]) == windows_before,
              f"round {round_no}: no window added or lost (the animation's stand-in is never managed)")
    ts.stop_app()
    left = parked_anywhere()
    check(not left, "the end: no fixture window parked on any Desktop")


def expose_on_desktop_2(d1, d2):
    """Mission Control, then App Exposé, typed through the app (`ctl debug-expose`, the
    user's own shortcuts) and left with Escape; never a Desktop change (ts.expose_round has
    the checks). Then each Desktop's pane is still on its own Desktop: Desktop 2's on the
    Space shown, Desktop 1's not."""
    s = state()
    ok, told = ts.panes_hide_in_expose(s)
    check(ok, f"every strip pane is Transient, not Stationary, joining no other Space ({told})")
    if AUTO:
        WATCH.pause()  # Mission Control draws other Desktops; the round checks the Desktop itself
    take = (lambda name: by_eye(f"{name}: no strip at the left edge (Apple's hides)", shot(f"expose-{name}"))) if AUTO else None
    try:
        for which, label in (("mission-control", "Mission Control"), ("app-windows", "App Exposé")):
            if not ts.expose_round(which, label, take):
                break
    finally:
        if AUTO:
            WATCH.desk("d2")
    if not AUTO:
        check(not yes("Did the screen show another Desktop at any point, or the strip over Mission Control?"),
              "Mission Control: no other Desktop and no strip over it (by eye)")
    s = state()
    check(s["space"] == d2, f"still on Desktop 2 after Mission Control ({d2} then {s['space']})")
    panes = s.get("strip_panels") or []
    mine = [p for p in panes if p["active"]]
    check(len(panes) == 2 and bool(mine) and mine[0]["space"] == d2 and mine[0]["on_active_space"]
          and all(not p["on_active_space"] for p in panes if not p["active"]),
          f"each Desktop's pane still on its own Desktop after Mission Control ({panes})")


def fullscreen_on(round_no, desk):
    """Auto: a shown Fixture window of the Desktop shown goes full screen through the app
    (`ctl debug-fullscreen`). Returns its id, or None when none could."""
    s = state()
    shown = [w for w in s["windows"] if w["space"] == desk and not w["parked"] and w["role"] == "standard"]
    shown.sort(key=lambda w: (not w["title"].startswith("Fixture"), w["title"]))
    if not shown:
        say("    no shown fixture window to make full screen; full-screen check skipped")
        return None
    wid = shown[0]["id"]
    say(f"\n>>> Round {round_no} of 2: {shown[0]['title']} goes full screen (ctl debug-fullscreen {wid} on)")
    WATCH.pause()
    r = ctl("debug-fullscreen", wid, "on")
    if not check(r.get("ok"), f"round {round_no}: window {wid} asked into full screen ({r.get('error', r.get('via', 'ok'))})"):
        WATCH.desk(WATCH.where)
        return None
    return wid


def dry_run():
    """Prints the preflight and the plan; acts on nothing."""
    say("dry run: nothing is launched, switched or moved")
    say(f"home (scratch): {ts.HOME}")
    say(f"screenshots would go to: {shots_dir()}")
    say(f"app binary for the probes: {ts.BIN}")
    say(f"Move left/right a space (79/81) on: {switch_shortcuts()}")
    problems = preflight()
    for p in problems:
        say(f"  a real run would refuse now: {p}")
    if not problems:
        say("  preflight clear: a real run would start now")
    say(PLAN)
    return 0


def main():
    global WATCH
    if AUTO and DRY:
        target_bin = os.path.join(ts.ROOT, "target", "Stage Manager.app", "Contents", "MacOS", "stage-manager")
        if os.path.exists(target_bin):
            ts.BIN = target_bin  # the dev copy is not refreshed for a dry run
        return dry_run()
    if not os.path.exists(ts.BIN):
        say("build first: scripts/test-stages.sh builds the app and the fixtures")
        return 2
    say(f"home: {ts.HOME}")
    if not AUTO:
        try:
            run()
        finally:
            ts.stop_app()
            ts.stop_all_fixtures()
        return report()
    install_guards()
    signal.signal(signal.SIGTERM, on_sigterm)
    # No display sleep or screen saver (and so no lock) while the run lasts.
    subprocess.Popen(["caffeinate", "-d", "-i", "-w", str(os.getpid())])
    WATCH = Watch()
    interrupted = False
    try:
        run()
    except Interrupted as e:
        interrupted = True
        say(f"\nSTOP: {e}")
        ts.failed.append(f"stopped: {e}")
    except (KeyboardInterrupt, Stopped):
        say("\nSTOP: interrupted by a signal")
        ts.failed.append("stopped by a signal")
    except Abort as e:
        say(f"\nSTOP: {e}")
        ts.failed.append(f"aborted: {e}")
    finally:
        auto_cleanup(interrupted)
    if BY_EYE:
        say("\nby eye (not counted):")
        for q, path in BY_EYE:
            say(f"  {path or '(no screenshot)'}: {q}")
    took = time.time() - STARTED
    say(f"\nthe run took {int(took // 60)} min {int(took % 60)} s")
    return report()


def report():
    say(f"\n{len(ts.passed)} passed, {len(ts.failed)} failed (every app run's log: {ts.HOME}/stage-manager.run-*.log and today's {ts.HOME}/YYYY-MM-DD.log)")
    for f in ts.failed:
        say(f"  FAIL {f}")
    return 1 if ts.failed else 0


if __name__ == "__main__":
    sys.exit(main())
