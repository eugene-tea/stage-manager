#!/usr/bin/env python3
"""Measurement probes of Apple's Stage Manager (fixture apps only), beside scripts/test_apple.py.

Run with `SM_TEST_TOGGLE_APPLE_SM=1 scripts/probe_apple.py [PHASE...]` (all four by default,
in this order). These are measurements: each phase prints what Apple's did, with times; a FAIL
is only a probe that could not run (its setup, a refused step). Built on test_apple.py: the
fixture copies Alpha (two windows), Bravo and Charlie, the dev app in the Apple mode
(`apple_stage_manager_on = "integrate"`, STAGE_MANAGER_ONLY = the fixtures) mirroring Apple's
stages in `ctl state` `apple`. Apple's Stage Manager (`GloballyEnabled`) and "Show windows
from an application" (`AppWindowGroupingBehavior`) are put back as they were on every exit
(success, failure, Ctrl-C, kill), and so is the virtual screen.

  dock-front  A Dock click on the app in front whose windows are in two stages. Under each
              "Show windows from an application" setting (1 All at Once, then 0 One at a
              Time): the fixtures opened anew (Alpha last); under All at Once the split is
              tried (Bravo in front, Alpha's strip item Shift-clicked in: one window or both
              join? Then Alpha's front window dragged to the strip: one or both leave?). Then
              Alpha 1's stage in front (the focus route) and Alpha's Dock item clicked three
              times 1.5 s apart (`ctl debug-dock-click`: found, hit-tested, AXPress; with
              SM_PROBE_DOCK_CLICK=real a press and release posted on the item instead). After
              each: the stage in front, where each Alpha window is (front, strip), the window
              list's frames, and when the list moved (sampled every 20 ms). About 1.5 min.
  mission-pick Picking a hidden stage's window in Mission Control. Each fixture alone in a
              stage (All at Once for the phase), Bravo's frame read in front, Alpha's stage in
              front, Mission Control opened by its key (`ctl debug-expose`), Bravo's window
              clicked where Mission Control lays it out (`ctl debug-mission-pick`: only with
              Mission Control shown, no other app's window at the point; never when the list
              shows no layout). The window list sampled every 10 ms from the pick to 1.5 s
              after (fixtures, and WindowManager's and the Dock's covers: when Mission Control
              closes), the mirror every ~50 ms (when the strip changes). Prints each window's
              path (still, moving, to where), when it reaches its own frame, and any motion
              after Mission Control closed (a second motion). About 30 s.
  mission-pick-ours The same pick under our Spaces engine (STAGE_MANAGER_ENGINE=spaces,
              Apple's off, our own strip, [windows] mission_control_stages at its default):
              the dev app started anew, each fixture in a stage of its own (`ctl new`),
              Alpha's in front (`ctl switch`). Same sampling and summary, for comparison.
              About 30 s.
  frames-setup Window frames per screen setup. Alpha opened with one window. Built-in screen
              only: Bravo 1 set to frame A_b (in front), Alpha 1 to A_a (in front; Bravo to the
              strip). A virtual screen added right of the screens (scripts/virtual-display,
              1920x1080): where both are; then Bravo 1 moved and resized to B_b on it, Alpha 1
              to B_a (Alpha in front, Bravo in a strip). The virtual screen removed: where are
              they (A? B clamped? elsewhere?), Bravo also read once brought in front. Added
              again: B again? Frames (the window list's and the model's) at each step, each
              judged against A and B. Decides setup_frames::ON_BY_DEFAULT. About 2 min.

Nobody needs to be at the Mac, but nobody may use it either: the phases post clicks (the Dock
item, Mission Control) and keys (Mission Control's own shortcut, Escape to a fixture). Only
fixture windows are moved; the owner's own windows are arranged by Apple's Stage Manager
while it is on, as in test_apple.py. No screen captures.
"""

import atexit
import ctypes
import json
import os
import signal
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import test_apple as ta  # noqa: E402

say, check, ctl, wait_until = ta.say, ta.check, ta.ctl, ta.wait_until

PHASES = ("dock-front", "mission-pick", "mission-pick-ours", "frames-setup")
APPLE_PHASES = ("dock-front", "mission-pick", "frames-setup")
GROUPING = "AppWindowGroupingBehavior"
DOCK_CLICK = (os.environ.get("SM_PROBE_DOCK_CLICK") or "press").strip().lower()
BUNDLES = [b for b, _, _ in ta.FIX.values()]
COVERS = ["com.apple.WindowManager", "com.apple.dock"]
MOVE_TOL = 1.0          # points: a change in the list smaller than this is no move
FRAME_TOL = 2.0         # points: the same frame
MOTION_GAP_MS = 150     # changes further apart than this are two motions
EXPOSE_SETTLE = 1.2     # Mission Control's opening animation, with room
SCREEN_SETTLE = 6.0     # a screen added or removed: macOS's and Apple's moves, with room
SAMPLE_MS = 1800        # a pick's sampling: from 0.1 s before the pick to past 1.5 s after
grouping_original = None  # (had, type, value) of AppWindowGroupingBehavior
screen = None             # the virtual screen while it is up


# ----- "Show windows from an application", restored on every exit -----------------------


def read_grouping():
    r = subprocess.run(["defaults", "read", ta.DOMAIN, GROUPING], capture_output=True, text=True)
    if r.returncode != 0:
        return (False, None, None)
    t = subprocess.run(["defaults", "read-type", ta.DOMAIN, GROUPING], capture_output=True, text=True).stdout
    kind = "-bool" if "boolean" in t else "-int"
    return (True, kind, r.stdout.strip())


def set_grouping(value):
    """1 = All at Once, 0 = One at a Time, written in the type the owner's value has."""
    kind = (grouping_original or (False, None, None))[1] or "-int"
    word = ("true" if value else "false") if kind == "-bool" else str(value)
    subprocess.run(["defaults", "write", ta.DOMAIN, GROUPING, kind, word], check=True)


def restore_grouping():
    if grouping_original is None:
        return
    had, kind, value = grouping_original
    if had:
        word = ("true" if value == "1" else "false") if kind == "-bool" else value
        subprocess.run(["defaults", "write", ta.DOMAIN, GROUPING, kind, word], capture_output=True)
    else:
        subprocess.run(["defaults", "delete", ta.DOMAIN, GROUPING], capture_output=True)
    say(f"restored {ta.DOMAIN} {GROUPING} to {value if had else '<absent>'}")


def grouping_name(value):
    return "All at Once" if value else "One at a Time"


def cleanup():
    global screen
    if screen is not None:
        say(f"virtual screen removed (cleanup): {screen.stop()}")
        screen = None
    restore_grouping()
    ta.cleanup()


# ----- the window list, sampled ------------------------------------------------------------


def probe(bundles, every=None, span=None):
    """`probe-windows` of `bundles` once, or sampled every `every` ms for `span` ms."""
    args = [ta.BIN, "probe-windows"] + (["--every", str(every), "--for", str(span)] if every else []) + list(bundles)
    r = subprocess.run(args, capture_output=True, text=True, timeout=30 + (span or 0) / 1000)
    return json.loads(r.stdout)


def listed():
    """The fixtures' windows on layer 0 now: {id: probe entry}."""
    return {w["id"]: w for w in probe(BUNDLES)["windows"] if w["layer"] == 0}


class Sampler:
    """`probe-windows --every MS --for MS` of `bundles` in the background."""

    def __init__(self, bundles, every, span):
        args = [ta.BIN, "probe-windows", "--every", str(every), "--for", str(span), *bundles]
        self.proc = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)

    def result(self):
        """[(epoch seconds, {id: entry})], the display ({x,y,w,h}); ([], None) when it failed."""
        try:
            out, _ = self.proc.communicate(timeout=30)
            d = json.loads(out)
        except (subprocess.TimeoutExpired, json.JSONDecodeError) as e:
            self.proc.kill()
            say(f"  the sampler failed: {e}")
            return [], None
        return [(s["t"], {w["id"]: w for w in s["windows"]}) for s in d.get("samples", [])], d.get("display")


def fmt(b):
    return f"{b['x']:.0f},{b['y']:.0f} {b['w']:.0f}x{b['h']:.0f}" if b else "-"


def moved(a, b, tol=MOVE_TOL):
    return a is None or b is None or any(abs(a[k] - b[k]) > tol for k in "xywh")


def close(a, b, tol=FRAME_TOL):
    return a is not None and b is not None and not moved(a, b, tol)


def covers(b, d):
    return d is not None and all(abs(b[k] - d[k]) < 1.0 for k in "xywh")


def path_of(samples, wid, t0):
    """Window `wid`'s frames over `samples`: [(ms after t0, bounds or None)] at each change."""
    out, prev = [], "start"
    for t, ws in samples:
        w = ws.get(wid)
        b = w["bounds"] if w and w.get("layer") == 0 and w.get("onscreen") else None
        if prev == "start" or (b is None) != (prev is None) or (b is not None and moved(b, prev)):
            out.append((round((t - t0) * 1000), b))
            prev = b
    return out


def motions(changes):
    """The changes after the first sample grouped into motions (a gap over MOTION_GAP_MS
    splits them): [(start ms, end ms, from, to, changes)]."""
    if len(changes) < 2:
        return []
    out = []
    start, frm, last_t, n = changes[1][0], changes[0][1], changes[1][0], 0
    to = changes[0][1]
    for i in range(1, len(changes)):
        t, b = changes[i]
        if t - last_t > MOTION_GAP_MS and n:
            out.append((start, last_t, frm, to, n))
            start, frm, n = t, to, 0
        to, last_t, n = b, t, n + 1
    out.append((start, last_t, frm, to, n))
    return out


def reached(changes, own):
    """The ms from which the window stays at `own` to the end of the sampling, or None."""
    at = None
    for t, b in changes:
        if close(b, own):
            at = t if at is None else at
        else:
            at = None
    return at


def report_path(name, changes, own=None, closed_ms=None, waypoints=6):
    """Prints one window's path: where it starts, each motion (times, from, to, a few
    waypoints), when it reaches `own` and the motions after Mission Control closed."""
    if not changes:
        say(f"    {name}: not listed")
        return
    ms = motions(changes)
    say(f"    {name}: at {fmt(changes[0][1])} at {changes[0][0]} ms; {len(ms)} motion(s)")
    for start, end, frm, to, n in ms:
        mid = [c for c in changes if start <= c[0] <= end]
        step = max(1, len(mid) // waypoints)
        pts = " -> ".join(f"{t}:{fmt(b)}" for t, b in mid[::step][:waypoints])
        say(f"      {start}-{end} ms ({n} changes): {fmt(frm)} -> {fmt(to)}   [{pts}]")
    if own is not None:
        r = reached(changes, own)
        say(f"      its own frame {fmt(own)}: " + (f"reached at {r} ms, stays" if r is not None else f"not at it at the end ({fmt(changes[-1][1])})"))
    if closed_ms is not None:
        after = [m for m in ms if m[1] > closed_ms + 20]
        say(f"      after Mission Control closed ({closed_ms} ms): " + (", ".join(f"{max(s, closed_ms)}-{e} ms to {fmt(to)}" for s, e, _, to, _ in after) if after else "still"))


def cover_times(samples, display, t0):
    """(ms after t0 when WindowManager's layer-19 cover was last listed, the Dock's
    full-display cover's last) over `samples`: when Mission Control closed."""
    wm, dock = None, None
    for t, ws in samples:
        for w in ws.values():
            if w.get("onscreen") and covers(w["bounds"], display):
                if w["layer"] == 19:
                    wm = round((t - t0) * 1000)
                elif w["layer"] == 20:
                    dock = round((t - t0) * 1000)
    return wm, dock


class Poller(threading.Thread):
    """`snap(ctl state)` every ~50 ms until stopped: [(epoch seconds, snapshot)] at each change."""

    def __init__(self, snap):
        super().__init__(daemon=True)
        self.snap, self.stop_flag, self.seen = snap, threading.Event(), []

    def run(self):
        last = object()
        while not self.stop_flag.is_set():
            s = ctl("state")
            v = self.snap(s) if s.get("ok") else None
            if v is not None and v != last:
                self.seen.append((time.time(), v))
                last = v
            time.sleep(0.04)

    def stop(self):
        self.stop_flag.set()
        self.join(timeout=5)
        return self.seen


# ----- the mirror ---------------------------------------------------------------------------


def title_of(s):
    return {w["id"]: w["title"] for w in s.get("windows", [])}


def where(s, wid):
    """Where Apple's shows window `wid`: front, strip N, remembered, away, on another screen."""
    a = s.get("apple") or {}
    if wid in (a.get("current") or []):
        return "front"
    for n, g in enumerate(a.get("strip") or [], 1):
        if wid in g:
            return f"strip {n}"
    if any(wid in g for g in a.get("remembered") or []):
        return "remembered"
    if any(wid in g for g in a.get("away") or []):
        return "away"
    for sc in a.get("screens") or []:
        if wid in (sc.get("current") or []):
            return f"front of screen {sc.get('id')}"
        for n, g in enumerate(sc.get("strip") or [], 1):
            if wid in g:
                return f"strip {n} of screen {sc.get('id')}"
    return "?"


def mirror_snapshot(s):
    """(front titles, strip groups by title) of the main screen's mirror."""
    t = title_of(s)
    a = s.get("apple") or {}
    return (tuple(sorted(t.get(i, str(i)) for i in a.get("current") or [])),
            tuple(tuple(sorted(t.get(i, str(i)) for i in g)) for g in a.get("strip") or []))


def windows_line(s, ws, names=None):
    """Each fixture window (of `names`): where, and its listed frame."""
    t = title_of(s)
    out = []
    for wid in sorted(ws, key=lambda i: t.get(i, str(i))):
        title = t.get(wid, str(wid))
        if names and not any(title.startswith(n + " ") for n in names):
            continue
        w = ws[wid]
        out.append(f"{title} {where(s, wid)} {fmt(w['bounds'])}{'' if w.get('onscreen') else ' (off screen)'}")
    return "; ".join(out)


def quit_fixture(name):
    _, app, _ = ta.FIX[name]
    for pid in ta.pids(app):
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    ta.pid_gone(app)


def reopen_all(order=("Bravo", "Charlie", "Alpha"), windows=None):
    """Every fixture quit, then opened again in `order` (the last one in front), each window
    at its own frame; `ctl state` once the mirror shows them all and is quiet."""
    for name in ta.FIX:
        quit_fixture(name)
    for name in order:
        n = (windows or {}).get(name)
        ta.open_fixture(name, n)
        time.sleep(1.2)
    want = sum((windows or {}).get(name) or ta.FIX[name][2] for name in order)
    return wait_until(lambda: (lambda s: s if len(s.get("windows", [])) == want and ta.idle_jobs(s) else None)(ctl("state")), 15,
                      f"{want} fixture windows mirrored")


def id_of(s, title):
    return next((w["id"] for w in s.get("windows", []) if w["title"] == title), None)


def focus_front(s, title):
    """Window `title`'s stage in front by the focus route (`ctl apple try focus`: Apple's
    switches to the stage of a window focused), quiet after."""
    wid = id_of(s, title)
    r = ctl("apple", "try", "focus", wid)
    if not r.get("ok"):
        say(f"  focus {title}: {r.get('error')}")
    return wait_until(lambda: (lambda x: x if wid in ((x.get("apple") or {}).get("current") or []) and ta.idle_jobs(x) else None)(ctl("state")), 5,
                      f"{title}'s stage in front")


# ----- dock-front ---------------------------------------------------------------------------


def try_split(s):
    """Under All at Once: can Alpha's two windows be in two stages? Bravo in front, Alpha's
    strip item Shift-clicked; then Alpha's stage in front and its front window dragged to the
    strip. Prints what Apple's did each time."""
    say("  -- the split, under All at Once --")
    if not ta.bring_front("Bravo"):
        check(False, "Bravo in front before the Shift-click")
        return
    s = ta.quiet_state()
    alpha = ta.windows_of(s, "Alpha")
    target = next((w for w in alpha if w in (s["apple"].get("click_at") or {}) or str(w) in (s["apple"].get("click_at") or {})), alpha[0] if alpha else None)
    say(f"    before: {ta.title_groups(s)}")
    r = ctl("apple", "try", "shift-click", target)
    say(f"    Shift-click on Alpha's strip item (window {title_of(s).get(target)}): {r.get('error') or 'posted'}")
    time.sleep(1.5)
    s = ta.quiet_state()
    if s:
        joined = [title_of(s).get(w, str(w)) for w in alpha if w in s["apple"]["current"]]
        say(f"    after: {ta.title_groups(s)}; Alpha's windows that joined Bravo's stage: {joined or 'none'}"
            f" ({'both' if len(joined) == len(alpha) else 'one' if len(joined) == 1 else 'none'})")
    # The drag: Alpha's stage in front, its front window dragged onto the strip.
    if not ta.bring_front("Alpha"):
        return
    s = ta.quiet_state()
    front = [w for w in s["apple"]["current"] if w in alpha]
    if not front:
        say("    Alpha's windows are not in front: no drag")
        return
    ctl("apple", "try", "focus", front[0])
    s = ta.quiet_state()
    say(f"    before the drag: {ta.title_groups(s)}")
    r = ctl("apple", "try", "drag", front[0])
    say(f"    drag of {title_of(s).get(front[0])} onto the strip: {r.get('error') or 'posted'}")
    time.sleep(1.5)
    s = ta.quiet_state()
    if s:
        left = [title_of(s).get(w, str(w)) for w in alpha if w not in s["apple"]["current"]]
        say(f"    after: {ta.title_groups(s)}; Alpha's windows that left the stage in front: {left or 'none'}"
            f" ({'both' if len(left) == len(alpha) else 'one' if len(left) == 1 else 'none'})")


def dock_clicks(label):
    """Alpha's Dock item clicked three times, 1.5 s apart; after each the stage in front,
    where Alpha's windows are and the list's frames, and when the list moved."""
    s = ta.quiet_state()
    alpha = ta.windows_of(s, "Alpha")
    bundle = ta.FIX["Alpha"][0]
    say(f"    start: front {ta.front_titles(s)}; {windows_line(s, listed())}")
    for k in (1, 2, 3):
        sampler = Sampler(BUNDLES, 20, 1500)
        time.sleep(0.1)
        t0 = time.time()
        r = ctl("debug-dock-click", bundle, *(["real"] if DOCK_CLICK == "real" else []))
        if not check(r.get("ok"), f"{label} click {k}: Alpha's Dock item {'clicked' if DOCK_CLICK == 'real' else 'pressed'} ({r.get('error') or 'ok'})"):
            sampler.result()
            return
        samples, _ = sampler.result()
        time.sleep(max(0.0, t0 + 1.5 - time.time()))
        s = ta.quiet_state(4) or ctl("state")
        ws = listed()
        moves = {}
        for wid in sorted(ws):
            ms = motions(path_of(samples, wid, t0))
            if ms:
                moves[title_of(s).get(wid, str(wid))] = f"{ms[0][0]}-{ms[-1][1]} ms"
        say(f"    click {k}: front {ta.front_titles(s)}; Alpha: {', '.join(f'{title_of(s).get(w)} {where(s, w)}' for w in alpha)}")
        say(f"      frames: {windows_line(s, ws)}")
        say(f"      the list moved: {moves or 'nothing'}")


def phase_dock_front():
    say("\n== dock-front: a Dock click on the app in front whose windows are in two stages ==")
    say(f"  (the Dock item {'clicked: a press and a release posted' if DOCK_CLICK == 'real' else 'pressed: AXPress, as test_spaces.py does'}; SM_PROBE_DOCK_CLICK=real|press)")
    for value in (1, 0):
        say(f"\n  -- \"Show windows from an application\": {grouping_name(value)} ({GROUPING} {value}) --")
        set_grouping(value)
        time.sleep(1.5)
        s = reopen_all()
        if not check(s is not None, f"{grouping_name(value)}: the fixtures opened anew, Alpha last"):
            continue
        s = ta.quiet_state()
        groups = [g for g in ta.groups_of(s) if set(g) & set(ta.windows_of(s, "Alpha"))]
        say(f"  stages: {ta.title_groups(s)}; Alpha's windows in {len(groups)} stage(s)")
        if value == 1:
            try_split(s)
        elif len(groups) < 2:
            say("  One at a Time set, yet Alpha's two windows share a stage: the setting did not take while WindowManager ran (or Apple's groups a fresh launch)")
        s = ta.quiet_state()
        groups = [g for g in ta.groups_of(s) if set(g) & set(ta.windows_of(s, "Alpha"))]
        split = len(groups) >= 2
        say(f"  Alpha's windows now in {len(groups)} stage(s): {[ta.titles(s, g) for g in groups]}")
        if split:
            s = focus_front(s, "Alpha 1")
            check(s is not None, f"{grouping_name(value)}: Alpha 1's stage in front (the focus route)")
        else:
            say("  no split: the Dock clicks below are on Alpha in front with both windows in one stage (the baseline)")
            s = ta.bring_front("Alpha")
        if s is None:
            continue
        dock_clicks(grouping_name(value))
    say(f"\n  {GROUPING} back to the owner's value for the next phases")
    restore_grouping()
    time.sleep(1.5)
    reopen_all()


# ----- mission-pick ---------------------------------------------------------------------------


def overlays():
    r = subprocess.run([ta.BIN, "probe-screen"], capture_output=True, text=True, timeout=10)
    try:
        return json.loads(r.stdout).get("overlays", [])
    except (json.JSONDecodeError, AttributeError):
        return []


def open_mission_control():
    """Mission Control by its own shortcut (`ctl debug-expose`), or its launcher when the
    shortcut is off; the overlays listed before, or None when none came."""
    before = {o["id"] for o in overlays()}
    r = ctl("debug-expose", "mission-control")
    if not r.get("ok") and "turned off" in str(r.get("error")):
        say(f"  {r.get('error')}; opening Mission Control (com.apple.exposelauncher) instead")
        r = {"ok": subprocess.run(["open", "-b", "com.apple.exposelauncher"]).returncode == 0}
    if not check(r.get("ok"), f"Mission Control's shortcut typed ({r.get('key', r.get('error'))})"):
        return None
    seen = wait_until(lambda: [o for o in overlays() if o["id"] not in before], 3, "Mission Control to show")
    if not check(bool(seen), f"Mission Control is listed ({[(o['owner'], o['layer']) for o in seen or []]})"):
        ctl("debug-expose", "escape")
        return None
    return before


def close_mission_control(before):
    """Escape while Mission Control still shows (a fixture is frontmost), then its end."""
    if [o for o in overlays() if o["id"] not in before]:
        ctl("debug-expose", "escape")
    wait_until(lambda: not [o for o in overlays() if o["id"] not in before], 5, "Mission Control to end")


def settled_layout(ids, timeout=4):
    """The fixtures' listed frames once two reads 150 ms apart agree (Mission Control's
    animation over)."""
    end, last = time.time() + timeout, None
    while time.time() < end:
        now = listed()
        if last and all(i in now and i in last and close(now[i]["bounds"], last[i]["bounds"], 0.5) for i in ids):
            return now
        last = now
        time.sleep(0.15)
    return last or {}


def frames_line(names, ws, ids):
    """`ids` by title with their frames in `ws` (a listing)."""
    return ", ".join(names.get(i, str(i)) + " " + fmt((ws.get(i) or {}).get("bounds")) for i in ids)


def mission_round(label, s, pick, others, own, snap, names):
    """One pick: Mission Control open, `pick` (window id) clicked where it is laid out, the
    list and the mirror sampled; prints the paths. `others`: the stage left's window ids;
    `own`: {id: frame in front}; `snap`: state -> snapshot for the poller; `names`: id -> title."""
    ids = [pick] + others
    before_list = listed()
    say(f"  before: {frames_line(names, before_list, ids)}")
    opened = open_mission_control()
    if opened is None:
        return
    time.sleep(EXPOSE_SETTLE)
    laid = settled_layout(ids)
    say(f"  Mission Control's layout: {frames_line(names, laid, ids)}")
    # Laid out: listed on screen, away from where it was listed before (a window hidden in a
    # stage Space, ours, may be listed nowhere on screen before).
    blind = not all(i in laid and laid[i].get("onscreen")
                    and (i not in before_list or not before_list[i].get("onscreen") or moved(laid[i]["bounds"], before_list[i]["bounds"], 8.0))
                    for i in ids)
    if blind:
        say("  the window list shows no Mission Control layout for these windows (a window still where it was): no click (it could land on another app's window)")
        close_mission_control(opened)
        check(False, f"{label}: the list shows Mission Control's layout of {[names[i] for i in ids]}")
        return
    sampler = Sampler(BUNDLES, 10, SAMPLE_MS)
    covers_sampler = Sampler(COVERS, 10, SAMPLE_MS)
    poller = Poller(snap)
    poller.start()
    time.sleep(0.1)
    t0 = time.time()
    r = ctl("debug-mission-pick", pick)
    if not check(r.get("ok"), f"{label}: {names[pick]} picked where Mission Control lays it out ({r.get('at')}; {r.get('error') or 'ok'})"):
        poller.stop()
        sampler.result()
        covers_sampler.result()
        close_mission_control(opened)
        return
    samples, display = sampler.result()
    cover_samples, cdisplay = covers_sampler.result()
    time.sleep(0.2)
    seen = poller.stop()
    wm_ms, dock_ms = cover_times(cover_samples, cdisplay or display, t0)
    closed = dock_ms if dock_ms is not None else wm_ms
    say(f"  Mission Control's covers last listed: WindowManager's (layer 19) at {wm_ms} ms, the Dock's (layer 20) at {dock_ms} ms after the pick"
        f" ({len(samples)} list samples, about {round(SAMPLE_MS / max(1, len(samples)))} ms apart, the last at"
        f" {round((samples[-1][0] - t0) * 1000) if samples else '-'} ms)")
    say(f"  paths (ms after the pick; 'own frame' = where the window was when its stage was in front):")
    for i in ids:
        report_path(names[i], path_of(samples, i, t0), own.get(i), closed)
    say("  the mirror (front; strip) at each change, ms after the pick:")
    for t, v in seen:
        say(f"    {round((t - t0) * 1000)} ms: {v}")
    close_mission_control(opened)


def phase_mission_pick():
    say("\n== mission-pick: Bravo's window (a stage in the strip) picked in Mission Control, Apple's ==")
    value_was = ta.all_at_once()
    if not value_was:
        set_grouping(1)
        time.sleep(1.5)
        reopen_all()
    try:
        ta.separate()
        s = ta.bring_front("Bravo")
        if not check(s is not None, "Bravo in front, to read its own frame"):
            return
        s = ta.quiet_state()
        names = title_of(s)
        bravo, alpha = ta.windows_of(s, "Bravo"), ta.windows_of(s, "Alpha")
        ws = listed()
        own = {i: ws[i]["bounds"] for i in bravo if i in ws}
        s = ta.bring_front("Alpha")
        if not check(s is not None, "Alpha's stage in front, Bravo's in the strip"):
            return
        s = ta.quiet_state()
        ws = listed()
        own.update({i: ws[i]["bounds"] for i in alpha if i in ws})
        say(f"  stages: {ta.title_groups(s)}; Bravo {where(s, bravo[0])}")
        mission_round("mission-pick", s, bravo[0], alpha, own, mirror_snapshot, names)
        s = ta.quiet_state()
        say(f"  after: front {ta.front_titles(s) if s else '?'}; {windows_line(s or ctl('state'), listed())}")
    finally:
        if not value_was:
            restore_grouping()
            time.sleep(1.5)


# ----- mission-pick-ours ----------------------------------------------------------------------


def launch_ours():
    """The dev app opened under the Spaces engine (Apple's off), fixtures only."""
    only = ",".join(BUNDLES)
    args = ["open", "-n", ta.APP]
    for k, v in {"STAGE_MANAGER_HOME": ta.HOME, "STAGE_MANAGER_ONLY": only, "STAGE_MANAGER_NO_PROMPT": "1",
                 "STAGE_MANAGER_TEST": "1", "STAGE_MANAGER_NO_CAPTURE": "1", "STAGE_MANAGER_ENGINE": "spaces",
                 "RUST_LOG": "info,stage_manager=debug"}.items():
        args += ["--env", f"{k}={v}"]
    subprocess.run(args, check=True)
    if not wait_until(lambda: ctl("ping").get("ok"), 15, "the app", step=0.2):
        return None
    return wait_until(lambda: (lambda s: s if s.get("ok") and s.get("engine", {}).get("kind") == "spaces" and len(s.get("windows", [])) == 4 else None)(ctl("state")),
                      20, "the Spaces engine managing the four fixture windows", step=0.3)


def stage_of(s, wid):
    return next((st for st in s.get("stages", []) if wid in st["windows"]), None)


def ours_snapshot(s):
    """(the stage in front's titles, the hidden stages' titles) under our engine."""
    t = title_of(s)
    front = next((st for st in s.get("stages", []) if st["number"] == s.get("active")), None)
    rest = [st for st in s.get("stages", []) if st is not front and st["windows"]]
    return (tuple(sorted(t.get(i, str(i)) for i in (front or {}).get("windows", []))),
            tuple(sorted(tuple(sorted(t.get(i, str(i)) for i in st["windows"])) for st in rest)))


def switch_to(title):
    s = ctl("state")
    st = stage_of(s, id_of(s, title))
    if st is None:
        return None
    ctl("switch", st["number"])
    return wait_until(lambda: (lambda x: x if (stage_of(x, id_of(x, title)) or {}).get("number") == x.get("active") else None)(ctl("state")), 5, f"{title}'s stage in front")


def phase_mission_pick_ours():
    say("\n== mission-pick-ours: the same pick under our Spaces engine (Apple's off) ==")
    ctl("quit")
    wait_until(lambda: not ctl("ping").get("ok"), 10, "Stage Manager quit", step=0.2)
    ta.set_apple(False)
    time.sleep(2.0)
    for name in ta.FIX:
        quit_fixture(name)
    for name in ("Bravo", "Charlie", "Alpha"):
        ta.open_fixture(name)
        time.sleep(1.2)
    wait_until(lambda: len(listed()) >= 4, 10, "four fixture windows")
    s = launch_ours()
    if not check(s is not None, "our Spaces engine runs with the four fixture windows"):
        return
    for title in ("Bravo 1", "Charlie 1"):
        s = ctl("state")
        st = stage_of(s, id_of(s, title))
        if st and len(st["windows"]) > 1:
            ctl("new", id_of(s, title))
            time.sleep(1.0)
    s = wait_until(lambda: (lambda x: x if all(len((stage_of(x, id_of(x, t)) or {}).get("windows", [])) == 1 for t in ("Bravo 1", "Charlie 1")) else None)(ctl("state")), 6,
                   "Bravo and Charlie each alone in a stage")
    if not check(s is not None, f"each fixture in a stage of its own ({ours_snapshot(ctl('state'))})"):
        return
    s = switch_to("Bravo 1")
    time.sleep(1.0)
    names = title_of(ctl("state"))
    bravo, alpha = [id_of(s, "Bravo 1")], [i for i in (id_of(s, "Alpha 1"), id_of(s, "Alpha 2")) if i]
    ws = listed()
    own = {i: ws[i]["bounds"] for i in bravo if i in ws}
    s = switch_to("Alpha 1")
    if not check(s is not None, "Alpha's stage in front, Bravo's hidden"):
        return
    time.sleep(1.0)
    ws = listed()
    own.update({i: ws[i]["bounds"] for i in alpha if i in ws})
    # Escape must reach a fixture: Alpha frontmost.
    subprocess.run(["open", ta.FIX["Alpha"][1]], check=True)
    time.sleep(0.8)
    say(f"  stages: {ours_snapshot(ctl('state'))}")
    mission_round("mission-pick-ours", s, bravo[0], alpha, own, ours_snapshot, names)
    time.sleep(1.0)
    say(f"  after: {ours_snapshot(ctl('state'))}")


# ----- frames-setup ---------------------------------------------------------------------------


class CGRect(ctypes.Structure):
    _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double), ("w", ctypes.c_double), ("h", ctypes.c_double)]


ta._cg.CGGetActiveDisplayList.argtypes = [ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint32), ctypes.POINTER(ctypes.c_uint32)]
ta._cg.CGDisplayBounds.argtypes = [ctypes.c_uint32]
ta._cg.CGDisplayBounds.restype = CGRect


def screens():
    """The active screens: [(id, {x,y,w,h})], global top-left points."""
    ids, n = (ctypes.c_uint32 * 16)(), ctypes.c_uint32(0)
    ta._cg.CGGetActiveDisplayList(16, ids, ctypes.byref(n))
    out = []
    for i in range(n.value):
        b = ta._cg.CGDisplayBounds(ids[i])
        out.append((ids[i], {"x": b.x, "y": b.y, "w": b.w, "h": b.h}))
    return out


class Screen:
    """The virtual screen (scripts/virtual-display), right of the screens there are."""

    def __init__(self):
        r = subprocess.run([os.path.join(ta.ROOT, "scripts", "virtual-display", "build.sh")], capture_output=True, text=True)
        if r.returncode != 0:
            raise SystemExit(f"virtual-display did not build: {r.stderr}")
        tool = r.stdout.strip()
        self.info = {}
        for attempt in (1, 2):
            self.proc = subprocess.Popen([tool, "--size", "1920x1080", "--pid-file", os.path.join(ta.HOME, "virtual-display.pid")],
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            line = self.proc.stdout.readline()
            try:
                self.info = json.loads(line)
            except json.JSONDecodeError:
                self.info = {"error": line}
            if self.info.get("ready") or attempt == 2:
                break
            say(f"    virtual screen not placed ({self.info}); trying again")
            self.stop()
            time.sleep(5)

    def stop(self):
        if self.proc.poll() is None:
            self.proc.terminate()
        try:
            out, err = self.proc.communicate(timeout=20)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            out, err = self.proc.communicate()
        return (out.strip().splitlines() or [""])[-1] + (f" {err.strip()}" if err.strip() else "")


def rect(x, y, w, h):
    return {"x": float(x), "y": float(y), "w": float(w), "h": float(h)}


def set_frame(wid, f):
    r = ctl("debug-frame", wid, int(f["x"]), int(f["y"]), int(f["w"]), int(f["h"]))
    if not r.get("ok"):
        say(f"    debug-frame {wid}: {r.get('error')}")
    time.sleep(0.8)
    return r.get("ok")


def on_screen_of(b, scr):
    """The screen (index in `scr`) holding the middle of `b`, or None."""
    cx, cy = b["x"] + b["w"] / 2, b["y"] + b["h"] / 2
    for k, (_, f) in enumerate(scr):
        if f["x"] <= cx < f["x"] + f["w"] and f["y"] <= cy < f["y"] + f["h"]:
            return k
    return None


def judge(b, a, bb):
    """`b` against frame A (built-in screen alone) and B (the virtual screen)."""
    if b is None:
        return "not listed"
    if close(b, a):
        return "A"
    if close(b, bb):
        return "B"
    same_size = lambda f: abs(b["w"] - f["w"]) <= FRAME_TOL and abs(b["h"] - f["h"]) <= FRAME_TOL
    if same_size(bb):
        return "B's size elsewhere (clamped or moved)"
    if same_size(a):
        return "A's size elsewhere"
    return "elsewhere"


def settle_frames(ids, at_least=SCREEN_SETTLE, timeout=20):
    """Waits `at_least`, then until the fixtures' frames hold for 2 s (at most `timeout`)."""
    time.sleep(at_least)
    end, last, since = time.time() + timeout, None, time.time()
    while time.time() < end:
        now = listed()
        if last is None or any(moved(now.get(i, {}).get("bounds"), last.get(i, {}).get("bounds")) for i in ids):
            last, since = now, time.time()
        elif time.time() - since >= 2.0:
            return now
        time.sleep(0.25)
    return last or listed()


def step_report(label, ids, frames_a, frames_b):
    """One step's frames: where each window is (Apple's), its listed frame judged against
    A and B, the screen it is on, and the model's restore frame."""
    s = ta.quiet_state(6) or ctl("state")
    ws, scr = listed(), screens()
    say(f"  {label}: screens {[(sid, fmt(f)) for sid, f in scr]}")
    restore = {w["id"]: w.get("restore") for w in s.get("windows", [])}
    for i in ids:
        b = (ws.get(i) or {}).get("bounds")
        k = on_screen_of(b, scr) if b else None
        say(f"    {title_of(s).get(i, i)}: {where(s, i)}; listed {fmt(b)} on screen {k if k is not None else '-'}: "
            f"{judge(b, frames_a[i], frames_b.get(i)) if frames_b.get(i) else ('A' if close(b, frames_a[i]) else 'not A')}"
            f"; model's restore {fmt(restore.get(i)) if isinstance(restore.get(i), dict) else restore.get(i)}")
    return s, ws


def front_anywhere(name):
    """Fixture `name` activated (`open`) until its first window is in front on its screen,
    whichever screen that is; `ctl state` then, or None."""
    _, app, _ = ta.FIX[name]
    subprocess.run(["open", app], check=True)
    wid = id_of(ctl("state"), f"{name} 1")
    return wait_until(lambda: (lambda s: s if where(s, wid).startswith("front") and ta.idle_jobs(s) else None)(ctl("state")), 6, f"{name} in front")


def front_frame(name, wid, frames_a, frames_b, back_to):
    """Fixture `name` brought in front: its listed frame judged; then `back_to` in front again."""
    if not front_anywhere(name):
        say(f"    {name} could not be brought in front")
        return
    time.sleep(1.0)
    b = (listed().get(wid) or {}).get("bounds")
    say(f"    {name} 1 in front: {fmt(b)}: {judge(b, frames_a[wid], frames_b[wid]) if frames_b.get(wid) else ('A' if close(b, frames_a[wid]) else 'not A')}")
    front_anywhere(back_to)
    time.sleep(1.0)


def phase_frames_setup():
    global screen
    say("\n== frames-setup: window frames per screen setup, Apple's ==")
    if not check(len(screens()) == 1, f"the built-in screen alone at the start ({len(screens())} screens)"):
        return
    value_was = ta.all_at_once()
    if not value_was:
        set_grouping(1)
        time.sleep(1.5)
    try:
        s = reopen_all(windows={"Alpha": 1})
        if not check(s is not None, "the fixtures opened anew, Alpha with one window"):
            return
        s = ta.quiet_state()
        alpha, bravo = id_of(s, "Alpha 1"), id_of(s, "Bravo 1")
        ids = [alpha, bravo]
        v = (s.get("display") or {}).get("visible") or (s.get("display") or {}).get("frame")
        frames_a = {bravo: rect(v["x"] + 380, v["y"] + 160, 640, 420), alpha: rect(v["x"] + 200, v["y"] + 80, 700, 450)}
        frames_b = {}
        # The built-in screen alone: each at frame A, Alpha in front last.
        for name, wid in (("Bravo", bravo), ("Alpha", alpha)):
            if not front_anywhere(name) or not set_frame(wid, frames_a[wid]):
                check(False, f"{name} 1 set to frame A in front")
                return
        time.sleep(3.0)
        step_report("1. the built-in screen alone (A set)", ids, frames_a, frames_b)

        screen = Screen()
        if not check(screen.info.get("ready"), f"the virtual screen added ({screen.info})"):
            return
        vb = screen.info["bounds"]
        settle_frames(ids)
        step_report("2. the virtual screen added", ids, frames_a, frames_b)
        frames_b = {bravo: rect(vb[0] + 420, vb[1] + 200, 760, 480), alpha: rect(vb[0] + 150, vb[1] + 120, 820, 520)}
        for name, wid in (("Bravo", bravo), ("Alpha", alpha)):
            if not front_anywhere(name) or not set_frame(wid, frames_b[wid]):
                check(False, f"{name} 1 moved to the virtual screen at frame B")
                return
        front_anywhere("Alpha")
        time.sleep(3.0)
        step_report("3. both moved to the virtual screen (B set; Alpha in front)", ids, frames_a, frames_b)

        say(f"    virtual screen removed: {screen.stop()}")
        screen = None
        settle_frames(ids)
        step_report("4. the virtual screen removed", ids, frames_a, frames_b)
        front_frame("Bravo", bravo, frames_a, frames_b, "Alpha")

        screen = Screen()
        if not check(screen.info.get("ready"), f"the virtual screen added again ({screen.info})"):
            return
        if screen.info.get("bounds") != vb:
            say(f"    the virtual screen came back at {screen.info.get('bounds')}, first at {vb}: B judged at the first place")
        settle_frames(ids)
        step_report("5. the virtual screen added again", ids, frames_a, frames_b)
        front_frame("Bravo", bravo, frames_a, frames_b, "Alpha")
        step_report("6. after Bravo's turn in front", ids, frames_a, frames_b)
        say("\n  frames-setup, what it means: Apple's keeps frames per setup if step 4 shows A for both (Bravo's once in front)"
            " and step 5 shows B; macOS's squeeze alone shows B's size clamped onto the built-in screen at 4 and no move back at 5.")
    finally:
        if screen is not None:
            say(f"    virtual screen removed: {screen.stop()}")
            screen = None
        if not value_was:
            restore_grouping()
            time.sleep(1.5)
        reopen_all()


# ----- main -----------------------------------------------------------------------------------


def main():
    global grouping_original
    if os.environ.get("SM_TEST_TOGGLE_APPLE_SM") != "1":
        say("skipped: set SM_TEST_TOGGLE_APPLE_SM=1 to toggle Apple's Stage Manager on this Mac (the owner's call)")
        return 0
    if not os.access(ta.BIN, os.X_OK):
        sys.exit(f"{ta.BIN} missing; run make dev")
    r = subprocess.run([ta.BIN, "ctl", "ping"], capture_output=True, text=True, env={k: v for k, v in os.environ.items() if k != "STAGE_MANAGER_HOME"})
    if r.returncode == 0:
        sys.exit("a Stage Manager is running (the installed app?); quit it first")
    phases = sys.argv[1:] or list(PHASES)
    unknown = [p for p in phases if p not in PHASES]
    if unknown:
        sys.exit(f"unknown phase {unknown}; phases: {', '.join(PHASES)}")
    ta.build_fixtures()
    ta.original = ta.read_apple()
    grouping_original = read_grouping()
    atexit.register(cleanup)
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, ta.on_signal)
    say(f"home {ta.HOME}; {GROUPING} {grouping_original[2] if grouping_original[0] else '<absent>'}; Dock click: {DOCK_CLICK}")
    ta.set_apple(False)
    if any(p in APPLE_PHASES for p in phases):
        if ta.start() is None:
            return 1
        runs = {"dock-front": phase_dock_front, "mission-pick": phase_mission_pick, "frames-setup": phase_frames_setup}
        for name in APPLE_PHASES:
            if name in phases:
                runs[name]()
    if "mission-pick-ours" in phases:
        phase_mission_pick_ours()
    say(f"\n{len(ta.passed)} steps ran, {len(ta.failed)} could not")
    return 1 if ta.failed else 0


if __name__ == "__main__":
    sys.exit(main())
