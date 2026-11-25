#!/usr/bin/env python3
"""Live check: a screen going away keeps every stage's windows.

Run it as `SM_ENGINE=spaces scripts/test-stages.sh display-gone [frames] [main|secondary|ahead]
[--rounds N]` (default: every kind, 3 rounds each; `frames` runs main and secondary), or without `SM_ENGINE` for the
Accessibility (Corner) engine. Nobody needs to be at the Mac, but nobody may use it
either: the run adds a virtual screen (scripts/virtual-display, a CoreGraphics virtual display)
and takes it away again, as an unplug would.

Each round: the virtual screen is added (`main`: made the main screen, with the menu bar, the
built-in one to its right; `secondary`: right of the screens there were); fixture stages are
opened on its Desktop (Fixture 1-2, Alpha 1, Bravo 1, Charlie 1: four stages) and on the
built-in screen's Desktop shown (Fixture 3 and Charlie 2: two stages), the app (the test build,
fixtures only) is started, and one stage of each screen is in front, the others hidden. Then
the virtual screen goes. Checks, sampled every second for 65 s and then on every Desktop of
the screen left (slid to with the app's shortcut, `ctl debug-space`, then back to the start
one):

- no stage gains or loses a window (the app's model, by stage id);
- the window server shows no fixture window of a hidden stage, on any Desktop: only windows
  of the front stage of the Desktop shown are on screen (under the Corner engine, a hidden
  one is on screen only in a park zone);
- the log says `display: a gone screen's hidden windows kept in their stages` with `lost=0`
  (the Spaces engine), and never "windows of another Space are on screen" naming a window of a
  hidden stage.

The main screen gone, macOS ends the Desktop it made on the built-in screen when the virtual
one took the main role: that Desktop's hidden stage (Charlie 2) is kept whole too.

**A Dock click past the settle** (every round, 20 s after the screen went, within the 60 s
watch): when a Desktop not shown holds a hidden fixture stage whose app has windows on that
Desktop only, its Dock item is pressed (`ctl debug-dock-click`). The app arranges that Desktop
before macOS slides to it, so its stage in front goes to a card while the other Desktop still
shows. Checks: the clicked app's stage comes in front there; the stage that was in front stays
a stage of that Desktop with its windows, hidden; the log never says `window left this Space`
for its windows, and its hide's line (`a hide while a screen gone is watched`) takes the
Desktop the window was on as its home (`on` = `home`; 11-21: a Dock click 36 s after the change
hid it into a stage of the Desktop still shown, and it showed up on the other Desktop), and
that the Desktop was arranged at the press. With no such stage the step is skipped and says
so; `main` and `secondary` rounds leave every fixture on the Desktop shown, so the `ahead`
kind sets it up: Alpha 1 and Bravo 1 opened on another Desktop of the main screen (slid to
with `ctl debug-space`), a stage each, Bravo's in front, then back, a virtual screen added to
the right and removed. A run with the `ahead` kind whose Dock step ran in no round fails (a
run of `main` or `secondary` alone has no such stage to click for, and only says so).

**Frames per screen setup** (`display-gone frames [main|secondary] [--rounds N]`, with
`[displays] frames_per_setup = true` in the test home's config): the fixture stages are opened
on the built-in screen alone (Fixture 1-2, Alpha 1) and each stage's windows resized there, one
stage after another. The virtual screen is added (made main, or right of the built-in one):
the log says a new screen setup once the change settles. Every window shown is resized where
it is now, and the stage in front changed and its windows resized too. The screen is removed:
once the change settles the log says the windows were put back, and every window's frame
(the model's restore frame, and where the window server shows it when shown) is the one it
had on the built-in screen alone. The screen is added again: every window on the screen its
frame was kept on is back at its frame of the two-screen setup; one macOS put on another
screen is counted `elsewhere` by the log, never moved across.

Under the Spaces engine each round ends by printing the shadow of design r3 (`ctl state`'s
`desks`: where the model's Desktop and the window server's disagree, open and begun, the
longest) and the log's disagreement lines. A disagreement on a hidden window's Desktop still
open 10 s after the round's last look fails (design r3, slice 4).

The virtual screen is removed and the screens put back where they were whatever happens (the
tool does it on SIGTERM too); the app is quit and the fixtures closed after each round. It
refuses to start while another Stage Manager runs.
"""

import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import test_stages as ts  # noqa: E402

say, check, ctl = ts.say, ts.check, ts.ctl

ARGS = sys.argv[1:]
FRAMES = "frames" in ARGS
KINDS = [k for k in ARGS if k in ("main", "secondary", "ahead")] or ["main", "secondary", "ahead"]
ROUNDS = int(ARGS[ARGS.index("--rounds") + 1]) if "--rounds" in ARGS else 3
WATCH_SECONDS = float(os.environ.get("SM_DISPLAY_GONE_WATCH") or 65)
TOOL_BUILD = os.path.join(ts.ROOT, "scripts", "virtual-display", "build.sh")
PID_FILE = os.path.join(ts.HOME, "virtual-display.pid")
KEPT = "display: a gone screen's hidden windows kept in their stages"
FRAMES_CONFIG = "[displays]\nframes_per_setup = true\n"
PUT_BACK = "display: a known screen setup: windows put back at their frames"
NEW_SETUP = "display: a new screen setup"
# A display change settles in 10 s (DISPLAY_SETTLE); the tick rests at most 2 s.
SETTLE_WAIT = 15
# A snapshot of the frames is kept after its 3 s quiet, on a tick (at most 2 s apart).
QUIET_WAIT = 8
FRAME_TOL = 2.0
FOREIGN = "windows of another Space are on screen"
# The Dock click to a Desktop not shown: past the change's settle (10 s), within the watch (60 s).
AHEAD_AT = 20
LEFT_SPACE = "window left this Space"
ARRANGED_AT_PRESS = "the Desktop the press goes to is arranged at the press"
WATCHED_HIDE = "display: a hide while a screen gone is watched"


def tool():
    r = subprocess.run([TOOL_BUILD], capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit(f"virtual-display did not build: {r.stderr}")
    return r.stdout.strip()


class Screen:
    """The virtual screen: added on start, removed (and the screens put back) on stop."""

    def __init__(self, main):
        # Made main, it takes the main screen's Desktops and their windows (macOS moves them):
        # at the main screen's size none of them is resized on the way. A placement the
        # system refused (the tool ends at once, 11-18) is tried again once, a moment later.
        size = ["--size", "main", "--main"] if main else ["--size", "1920x1080"]
        for attempt in (1, 2):
            self.proc = subprocess.Popen([tool(), *size, "--pid-file", PID_FILE],
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            line = self.proc.stdout.readline()
            try:
                self.info = json.loads(line)
            except json.JSONDecodeError:
                self.stop()
                raise SystemExit(f"virtual-display: {line}{self.proc.stderr.read()}") from None
            if self.info.get("ready") or attempt == 2:
                break
            say(f"    virtual screen not placed ({self.info}); trying again")
            self.stop()
            time.sleep(5)
        if not self.info.get("ready"):
            self.stop()
            raise SystemExit(f"virtual-display: not placed: {self.info}")

    def stop(self):
        if self.proc.poll() is None:
            self.proc.terminate()
        try:
            out, err = self.proc.communicate(timeout=20)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            out, err = self.proc.communicate()
        return (out.strip().splitlines() or [""])[-1] + (f" {err.strip()}" if err.strip() else "")


def names(s):
    return {w["id"]: w["title"] for w in s["windows"]}


def stages_by_id(s):
    """Stage id -> sorted window titles, every screen's stages, from the windows' `stage`."""
    out = {}
    for w in s["windows"]:
        if w.get("stage") is not None:
            out.setdefault(w["stage"], []).append(w["title"])
    return {k: sorted(v) for k, v in out.items()}


def front_stage_ids(s):
    """The front stage of every screen's Desktop shown."""
    return {d.get("active_stage") for d in s.get("displays", [])} - {None}


def shown_fixture_windows():
    r = ts.run([ts.BIN, "probe-windows", *ts.ALL_IDS], timeout=10)
    return [w for w in json.loads(r.stdout)["windows"] if w["layer"] == 0]


def parked_anywhere(bounds, s):
    """True when `bounds` sit in the park zone of a screen there now (the Corner engine)."""
    for d in s.get("displays", []):
        f = d.get("frame")
        if f and ts.is_parked(bounds, dict(f, corner=d.get("corner"), visible=d.get("visible"))):
            return True
    return False


def look(before, what, out):
    """One look: the model's stages against `before`, the window server's shown windows
    against the front stages. Appends problems to `out` (deduplicated by the caller)."""
    s = ts.state()
    now = stages_by_id(s)
    title = names(s)
    for sid, titles in before["stages"].items():
        if now.get(sid) != titles:
            out.add(f"{what}: stage {sid} {titles} is now {now.get(sid)}")
    for sid in set(now) - set(before["stages"]):
        out.add(f"{what}: a new stage {sid} {now[sid]}")
    fronts = front_stage_ids(s)
    stage_of = {w["id"]: w.get("stage") for w in s["windows"]}
    for w in shown_fixture_windows():
        if not w["onscreen"] or (not ts.SPACES and parked_anywhere(w["bounds"], s)):
            continue
        sid = stage_of.get(w["id"])
        if sid not in fronts:
            out.add(f"{what}: {title.get(w['id'], w['id'])} of hidden stage {sid} on screen")
    return s


def fixture_frames(origin_x, n, start_x=120):
    """--frames for n windows on the screen whose left edge is `origin_x` points right of the
    main screen's visible left edge (the fixtures' frames are relative to it)."""
    return ";".join(f"{origin_x + start_x + i * 140},{90 + i * 60},700,460" for i in range(n))


def open_fixtures(virtual_x, laptop_x):
    """Fixture 1-2 (virtual), 3 (built-in); Alpha 1, Bravo 1 (virtual); Charlie 1 (virtual),
    2 (built-in)."""
    frames = fixture_frames(virtual_x, 2) + ";" + fixture_frames(laptop_x, 1)
    ts.stop_fixture()
    subprocess.run(["open", "-n", "-g", ts.FIX_APP, "--args", "--windows", "3", "--background", "--frames", frames], check=True)
    for k, n, frames in (("a", 1, fixture_frames(virtual_x, 1, 500)), ("b", 1, fixture_frames(virtual_x, 1, 800)),
                         ("c", 2, fixture_frames(virtual_x, 1, 1100) + ";" + fixture_frames(laptop_x, 1, 500))):
        bundle, path, _, title = ts.EXTRA[k]
        ts.stop_extra(k)
        subprocess.run(["open", "-n", "-g", path, "--args", "--windows", str(n), "--title", title, "--background", "--frames", frames], check=True)
    return ts.wait_until(lambda: len(shown_fixture_windows()) >= 7, 15, "seven fixture windows")


def log_lines(text, since_line):
    return text.splitlines()[since_line:]


def others_shown():
    """The ids of every other app's windows on screen (layer 0): which of the owner's Desktops
    shows, told apart without reading a title."""
    r = ts.run([ts.BIN, "probe-windows", "--all"], timeout=10)
    ours = {w["id"] for w in shown_fixture_windows()}
    return {w["id"] for w in json.loads(r.stdout)["windows"] if w["layer"] == 0 and w["onscreen"] and w["id"] not in ours}


def desktops_tour(before, out, home):
    """Slides to every Desktop of the screen left: leftmost first, then right to the end; one
    look on each. Then back to the Desktop that showed before the run (the one whose other
    windows on screen are `home`), else to the one the tour started on."""
    start = ctl("space")
    seen, marks = [], []

    def step(direction):
        r = ctl("debug-space", direction)
        if not r.get("ok"):
            say(f"    debug-space {direction}: {r.get('error')}")
            return False
        if not r.get("changed"):
            time.sleep(1.0)
            now = ctl("space")
            return bool(now.get("ok")) and now["space"] != r["before"]["space"]
        return True

    lefts = 0
    while lefts < 12 and step("left"):
        lefts += 1
        time.sleep(1.5)
    rights = 0
    while True:
        time.sleep(2.0)
        s = look(before, f"Desktop {rights + 1} from the left", out)
        seen.append(s["space"])
        marks.append(others_shown())
        if rights >= 12 or not step("right"):
            break
        rights += 1
    target = lefts
    if home:
        overlap = [len(m & home) for m in marks]
        if max(overlap) > 0:
            target = overlap.index(max(overlap))
    for _ in range(rights - target):
        step("left")
        time.sleep(1.0)
    time.sleep(1.5)
    end = ctl("space")
    say(f"    Desktops seen (left to right): {seen}; back on {end.get('space')} (Desktop {target + 1} from the left; "
        f"the tour started on {start.get('space')})")
    check(not home or bool(others_shown() & home), "after the tour, the Desktop that showed before the run shows again")
    return seen


def ahead_candidate(s):
    """A hidden fixture stage on a Desktop not shown whose app has windows on that Desktop only,
    with that Desktop's stage in front (of another app): (stage, its bundle, the front stage),
    else None."""
    shown = {d.get("space") for d in s.get("displays", [])}
    win = {w["id"]: w for w in s["windows"]}
    fixture = set(ts.ALL_IDS)

    def bundles(st):
        return {win[i]["bundle_id"] for i in st["windows"] if i in win}

    for st in s.get("stages", []):
        b = bundles(st)
        if st["space"] in shown or st.get("active") or len(b) != 1 or not b <= fixture or not st["windows"]:
            continue
        bundle = next(iter(b))
        if any(w["space"] != st["space"] for w in s["windows"] if w["bundle_id"] == bundle):
            continue
        front = next((f for f in s["stages"] if f["space"] == st["space"] and f.get("active")), None)
        if front and front["windows"] and bundles(front) <= fixture and bundle not in bundles(front):
            return st, bundle, front
    return None


def dock_ahead(what):
    """The Dock click to a Desktop not shown, past the settle (module docs)."""
    s = ts.state()
    found = ahead_candidate(s)
    if not found:
        say(f"    {what}: no hidden stage on a Desktop not shown whose app is there only; the Dock step is skipped")
        return False
    st, bundle, front = found
    title = names(s)
    front_ids = set(front["windows"])
    say(f"    {what}: Dock click to {bundle} (stage {st['id']} on Desktop {st['space']}); in front there: stage {front['id']} "
        f"{sorted(title.get(i, i) for i in front_ids)}")
    mark = len(ts.log_text().splitlines())
    r = ctl("debug-dock-click", bundle)
    if not check(r.get("ok"), f"{what}: the Dock item was pressed ({r.get('error') or 'ok'})"):
        return True

    def arrived(x):
        here = next((y for y in x.get("stages", []) if y["id"] == st["id"]), None)
        return here is not None and here.get("active") and st["space"] in {d.get("space") for d in x.get("displays", [])}

    x = ts.wait_state(arrived, f"{what}: the clicked app's stage in front on its Desktop", timeout=6)
    check(x is not None, f"{what}: the clicked app's stage {st['id']} comes in front on Desktop {st['space']}")
    time.sleep(4)  # a window hidden on another Desktop is counted gone 3 s later
    x = ts.state()
    now = next((y for y in x.get("stages", []) if y["id"] == front["id"]), None)
    check(now is not None and now["space"] == front["space"] and set(now["windows"]) == front_ids and not now.get("active"),
          f"{what}: the stage that was in front ({front['id']}) stays on Desktop {front['space']} with its windows, hidden"
          + ("" if now else ": gone") + (f": now {now}" if now and set(now["windows"]) != front_ids else ""))
    lines = log_lines(ts.log_text(), mark)
    check(any(ARRANGED_AT_PRESS in ln for ln in lines), f"{what}: the Desktop was arranged at the press, ahead of the slide "
          "(else the front stage is hidden only once its Desktop shows, and nothing is tried)")
    left = [ln[:200] for ln in lines if LEFT_SPACE in ln and any(re.search(rf"\bid={i}\b", ln) for i in front_ids)]
    check(not left, f"{what}: no '{LEFT_SPACE}' for the front stage's windows" + (f": {left[:2]}" if left else ""))
    homes = []
    for ln in lines:
        if WATCHED_HIDE not in ln:
            continue
        m = re.search(r"\bid=(\d+)\b.*\bon: (Some\(\d+\)|None),.*\bhome=(Some\(\d+\)|None)", ln)
        if m and int(m.group(1)) in front_ids:
            homes.append((int(m.group(1)), m.group(2), m.group(3)))
    say(f"    {what}: the front stage's hides while watched (id, on, home): {homes}")
    check(bool(homes) and all(on == home for _, on, home in homes),
          f"{what}: each was hidden on the Desktop it was on" + ("" if homes else ": no such log line"))
    return True


# Rounds whose Dock step ran (pressed the Dock item): none in a whole run with the `ahead`
# kind is a failure.
DOCK_RAN = []


def slide(direction):
    """One Desktop over on the screen there (`ctl debug-space`); True when it changed."""
    r = ctl("debug-space", direction)
    time.sleep(1.5)
    return bool(r.get("ok") and r.get("changed"))


def ahead_round(n):
    """The 11-21 shape, two screens going to one: on the main screen, a Desktop not shown holds
    Bravo 1 in front and Alpha 1 hidden (Alpha's only window). A virtual screen is added to the
    right and removed; 20 s later Alpha's Dock item is pressed (the Dock step)."""
    say(f"\n== a Dock click to a Desktop not shown after a screen went, round {n} ==")
    p = os.path.join(ts.HOME, "state.json")
    if os.path.exists(p):
        os.remove(p)
    home = others_shown()
    ts.stop_all_fixtures()
    screen = None
    back = None
    away = False  # on the fixtures' Desktop, to slide back from at the end
    try:
        if not check(ts.start_app(0, extra_env={"STAGE_MANAGER_TEST": "1"}) is not None, f"ahead {n}: app up"):
            return
        went = next((d for d in ("right", "left") if slide(d)), None)
        if not check(went is not None, f"ahead {n}: another Desktop on the main screen"):
            return
        back = "left" if went == "right" else "right"
        away = True
        for k, x in (("a", 300), ("b", 700)):
            bundle, path, _, title = ts.EXTRA[k]
            ts.stop_extra(k)
            subprocess.run(["open", "-n", "-g", path, "--args", "--windows", "1", "--title", title, "--background",
                            "--frames", fixture_frames(0, 1, x)], check=True)
            time.sleep(2.0)

        def both(x):
            ws = [w for w in x["windows"] if w["bundle_id"] in (ts.EXTRA["a"][0], ts.EXTRA["b"][0])]
            return len(ws) == 2 and len({w["stage"] for w in ws}) == 2

        s = ts.wait_state(both, f"ahead {n}: Alpha 1 and Bravo 1 in stages of their own", timeout=10)
        if not check(s is not None, f"ahead {n}: Alpha 1 and Bravo 1, a stage each, on the other Desktop"):
            return
        d2 = s["space"]
        if not check(slide(back), f"ahead {n}: back on the Desktop the run started on"):
            return
        away = False
        screen = Screen(False)
        say(f"    virtual screen: {screen.info}")
        time.sleep(4)
        s = ts.state()
        before = {"stages": stages_by_id(s)}
        say(f"    stages: {before['stages']}; on Desktop {d2}: {[(st['id'], st.get('active')) for st in s['stages'] if st['space'] == d2]}")
        check(ahead_candidate(s) is not None, f"ahead {n}: a hidden stage on the Desktop not shown, its front another app's")
        t0 = time.time()
        say(f"    screen removed: {screen.stop()}")
        out = set()
        ran = False
        while time.time() - t0 < WATCH_SECONDS:
            try:
                look(before, f"{int(time.time() - t0)} s", out)
            except Exception as e:  # noqa: BLE001 - the app may be busy with the change
                say(f"    look at {int(time.time() - t0)} s: {e}")
            if not ran and time.time() - t0 >= AHEAD_AT:
                if ahead_candidate(ts.state()) is None and ctl("space").get("space") == d2:
                    say("    macOS shows the Desktop with the fixtures: back to the other one first")
                    slide(back)
                if dock_ahead(f"ahead {n}: Dock click at {int(time.time() - t0)} s"):
                    DOCK_RAN.append(f"ahead {n}")
                    away = ctl("space").get("space") == d2
                ran = True
            time.sleep(1.0)
        check(not out, f"ahead {n}: every stage keeps its windows and no hidden one shows" + (f": {sorted(out)[:8]}" if out else ""))
        out.clear()
        seen = desktops_tour(before, out, home)
        away = False
        check(not out, f"ahead {n}: on every Desktop ({len(seen)}), the same" + (f": {sorted(out)[:8]}" if out else ""))
        desk_shadow(f"ahead {n}", 0)
    finally:
        if screen is not None and screen.proc.poll() is None:
            say(f"    screen removed (cleanup): {screen.stop()}")
        if away and back is not None and app_up():
            slide(back)
        ts.stop_app()
        ts.stop_all_fixtures()
        say(f"    log kept: {ts.archive_log()}")


SHADOW_LINE = "desktops: the model and the window server disagree"


def desk_shadow(label, since_line):
    """The shadow's counts for this round (design r3; `ctl state`'s `desks`) and the log's
    disagreements since `since_line`. A disagreement on a hidden window's Desktop (the
    engine's home against the Desktop the model has the window on) still open 10 s after
    this look fails (design r3, slice 4: one record; the look returns as soon as none is
    open, so a round that agrees waits for nothing). Under the Corner engine the listing is
    not read."""
    try:
        d = ts.state().get("desks") or {}
    except Exception as e:  # noqa: BLE001 - the app may be busy or gone
        say(f"    desks ({label}): not read: {e}")
        return
    if d.get("listing") is None:
        say(f"    desks ({label}): the window server's listing is not read (the reads off or resting, or a display change's settle under the Corner engine)")
        return
    def homes_agree():
        try:
            return (ts.state().get("desks") or {}).get("disagree", {}).get("homes") == 0 or None
        except Exception:  # noqa: BLE001 - the app may be busy
            return None

    agreed = d["disagree"]["homes"] == 0 or ts.wait_until(homes_agree, 10, "the hidden windows' Desktops to agree") is not None
    try:
        g = (ts.state().get("desks") or d)["disagree"]
    except Exception:  # noqa: BLE001
        g = d["disagree"]
    say(f"    desks ({label}): Desktop shown: {g['shown']} open, {g['shown_begun']} begun; window homes: {g['homes']} open, "
        f"{g['homes_begun']} begun; the longest {g['longest_ms']} ms")
    for o in g["open"]:
        say(f"      open: {o}")
    check(agreed and g["homes"] == 0, f"desks ({label}): the engine and the model agree on every hidden window's Desktop within 10 s "
          f"({g['homes']} open)")
    for ln in [ln for ln in log_lines(ts.log_text(), since_line) if SHADOW_LINE in ln][:12]:
        say(f"    log: {ln[:300]}")


def app_up():
    return bool(ctl("ping").get("ok"))


def one_round(kind, n):
    say(f"\n== screen gone ({kind}), round {n} ==")
    p = os.path.join(ts.HOME, "state.json")
    if os.path.exists(p):
        os.remove(p)
    home = others_shown()
    screen = Screen(kind == "main")
    say(f"    virtual screen: {screen.info}")
    result = None
    try:
        time.sleep(3)
        vb = screen.info["bounds"]
        # The fixtures' frames are relative to the main screen's visible left edge.
        if kind == "main":
            virtual_x, laptop_x = 0, vb[2]
        else:
            virtual_x, laptop_x = vb[0], 0
        if not check(open_fixtures(virtual_x, laptop_x), f"{kind} {n}: seven fixture windows"):
            return
        s = ts.start_app(7, extra_env={"STAGE_MANAGER_TEST": "1"})
        if not check(s is not None, f"{kind} {n}: app up with seven windows"):
            return
        time.sleep(2)
        s = ts.state()
        say(f"    displays: {[(d['id'], d['frame'], d['space'], d['active_stage']) for d in s['displays']]}")
        say(f"    stages: {stages_by_id(s)}; front: {front_stage_ids(s)}")
        say(f"    engine spaces: {s.get('engine', {}).get('spaces')}")
        before = {"stages": stages_by_id(s)}
        hidden = [sid for sid in before["stages"] if sid not in front_stage_ids(s)]
        check(len(before["stages"]) == 6 and len(hidden) == 4, f"{kind} {n}: six stages, four hidden ({before['stages']})")
        out = set()
        look(before, "before", out)
        check(not out, f"{kind} {n}: before the screen goes, only front stages show" + (f": {sorted(out)}" if out else ""))
        out.clear()
        log_mark = len(ts.log_text().splitlines())
        t0 = time.time()
        say(f"    screen removed: {screen.stop()}")
        time.sleep(1)
        ahead_done = False
        while time.time() - t0 < WATCH_SECONDS:
            try:
                look(before, f"{int(time.time() - t0)} s", out)
            except Exception as e:  # noqa: BLE001 - the app may be busy with the change
                say(f"    look at {int(time.time() - t0)} s: {e}")
            if not ahead_done and time.time() - t0 >= AHEAD_AT:
                ahead_done = True
                if dock_ahead(f"{kind} {n}: Dock click at {int(time.time() - t0)} s"):
                    DOCK_RAN.append(f"{kind} {n}")
            time.sleep(1.0)
        problems_watch = sorted(out)
        check(not problems_watch, f"{kind} {n}: {WATCH_SECONDS:.0f} s after the screen went, every stage keeps its windows "
              "and no hidden one shows" + (f": {problems_watch[:8]}" if problems_watch else ""))
        out.clear()
        seen = desktops_tour(before, out, home)
        check(not out, f"{kind} {n}: on every Desktop ({len(seen)}), the same" + (f": {sorted(out)[:8]}" if out else ""))
        desk_shadow(f"{kind} {n}", log_mark)
        lines = log_lines(ts.log_text(), log_mark)
        kept = [ln for ln in lines if KEPT in ln]
        lost = [ln for ln in kept if not re.search(r"\blost=0\b", ln)]
        for ln in kept:
            say(f"    log: {ln[:300]}")
        if ts.SPACES:
            check(bool(kept) and not lost, f"{kind} {n}: the log keeps the hidden windows (lost=0)"
                  + ("" if kept else ": no such line") + (f": {lost[:2]}" if lost else ""))
        s = ts.state()
        title = names(s)
        hidden_ids = {w["id"] for w in s["windows"] if w.get("stage") in set(hidden)}
        foreign = []
        for ln in lines:
            if FOREIGN in ln:
                ids = {int(x) for x in re.findall(r"\d+", ln.split(FOREIGN)[0].split("windows=")[-1])}
                if ids & hidden_ids:
                    foreign.append(ln[:300])
                say(f"    log: {ln[:300]}")
        check(not foreign, f"{kind} {n}: no 'windows of another Space' naming a hidden stage's window" + (f": {foreign[:2]}" if foreign else ""))
        for ln in lines:
            if "display" in ln and ("WARN" in ln or "could not" in ln):
                say(f"    log: {ln[:300]}")
        result = {"stages_before": before["stages"], "stages_after": stages_by_id(s), "titles": title}
    finally:
        if screen.proc.poll() is None:
            say(f"    screen removed (cleanup): {screen.stop()}")
        ts.stop_app()
        ts.stop_all_fixtures()
        log = ts.archive_log()
        say(f"    log kept: {log}")
    return result


def display_by_id(s):
    return {d["id"]: d for d in s.get("displays", [])}


def kept_frames(s):
    """Title -> (screen key, frame relative to that screen's top-left): every managed window's
    restore frame as the model has it (a hidden one's included)."""
    shows = display_by_id(s)
    out = {}
    for w in s["windows"]:
        d = shows.get(w["display"])
        if d is None or w.get("role") != "standard":
            continue
        r, f = w["restore"], d["frame"]
        out[w["title"]] = (d["key"], {"x": r["x"] - f["x"], "y": r["y"] - f["y"], "w": r["w"], "h": r["h"]})
    return out


def resize_shown(s, step):
    """Resizes every shown managed fixture window where it is (`debug-frame`: the app's own
    placement, no click), each to a frame of its own inside its screen's visible frame.
    Returns the titles resized."""
    shows = display_by_id(s)
    done = []
    for i, w in enumerate(sorted((w for w in s["windows"] if not w["parked"] and w.get("role") == "standard"), key=lambda w: w["title"])):
        v = shows.get(w["display"], {}).get("visible")
        if not v:
            continue
        k = i + step
        width, height = min(980 - 40 * k, v["w"] - 200), min(640 - 30 * k, v["h"] - 120)
        x, y = v["x"] + 140 + 60 * k, v["y"] + 40 + 35 * k
        r = ctl("debug-frame", w["id"], x, y, width, height)
        if not r.get("ok"):
            say(f"    debug-frame {w['title']}: {r.get('error')}")
            continue
        done.append(w["title"])
        time.sleep(0.6)
    return done


def frames_match(s, want, what):
    """Each window on the screen its frame was kept on: its restore frame, and its frame on
    screen when shown, at that frame. Returns (matched, problems, elsewhere titles)."""
    shows = display_by_id(s)
    live = {w["id"]: w["bounds"] for w in shown_fixture_windows() if w["onscreen"]}
    matched, problems, elsewhere = [], [], []
    for w in s["windows"]:
        if w["title"] not in want:
            continue
        key, rel = want[w["title"]]
        d = shows.get(w["display"])
        if d is None or d["key"] != key:
            elsewhere.append(w["title"])
            continue
        f = d["frame"]
        expect = {"x": rel["x"] + f["x"], "y": rel["y"] + f["y"], "w": rel["w"], "h": rel["h"]}
        if not ts.close(w["restore"], expect, FRAME_TOL):
            problems.append(f"{what}: {w['title']} kept at {ts.fmt(w['restore'])}, not {ts.fmt(expect)}")
            continue
        if not w["parked"] and w["id"] in live and not ts.close(live[w["id"]], expect, FRAME_TOL):
            problems.append(f"{what}: {w['title']} shows at {ts.fmt(live[w['id']])}, not {ts.fmt(expect)}")
            continue
        matched.append(w["title"])
    return matched, problems, elsewhere


def log_counts(line):
    return {k: int(v) for k, v in re.findall(r"\b(put_back|touched|elsewhere|none_kept)=(\d+)", line)}


def settled_line(mark, needle):
    """The first log line with `needle` after line `mark`, waited for through a settle."""
    found = ts.wait_until(lambda: next((ln for ln in log_lines(ts.log_text(), mark) if needle in ln), None), SETTLE_WAIT + 10, needle)
    return found if isinstance(found, str) else None


def frames_round(kind, n):
    say(f"\n== frames per screen setup ({kind}), round {n} ==")
    ts.write_config(FRAMES_CONFIG)
    screen = None
    try:
        ts.stop_fixture()
        subprocess.run(["open", "-n", "-g", ts.FIX_APP, "--args", "--windows", "2", "--background", "--frames", fixture_frames(0, 2)], check=True)
        bundle, path, _, title = ts.EXTRA["a"]
        ts.stop_extra("a")
        subprocess.run(["open", "-n", "-g", path, "--args", "--windows", "1", "--title", title, "--background", "--frames", fixture_frames(0, 1, 500)], check=True)
        if not check(ts.wait_until(lambda: len(shown_fixture_windows()) >= 3, 15, "three fixture windows"), f"frames {kind} {n}: three fixture windows"):
            return
        s = ts.start_app(3, extra_env={"STAGE_MANAGER_TEST": "1"})
        if not check(s is not None, f"frames {kind} {n}: app up with three windows"):
            return
        time.sleep(2)
        # The built-in screen alone: each stage in front in turn, its windows resized.
        s = ts.state()
        numbers = sorted(ts.stage_titles(s))
        first = s["displays"][0].get("active")
        resized = []
        for k, number in enumerate(numbers):
            ctl("switch", number)
            time.sleep(1.5)
            resized += resize_shown(ts.state(), 2 * k)
        if first:
            ctl("switch", first)
            time.sleep(1.5)
        time.sleep(QUIET_WAIT)
        s = ts.state()
        alone = kept_frames(s)
        say(f"    built-in screen alone: {len(alone)} windows kept, resized {resized}; stages {ts.stage_titles(s)}")
        check(len(resized) >= 3, f"frames {kind} {n}: every fixture window resized on the built-in screen ({resized})")

        # The virtual screen comes: a new setup, macOS's way.
        mark = len(ts.log_text().splitlines())
        screen = Screen(kind == "main")
        say(f"    virtual screen: {screen.info}")
        new = settled_line(mark, NEW_SETUP)
        check(bool(new), f"frames {kind} {n}: the log says a new screen setup once the change settles")
        time.sleep(2)
        s = ts.state()
        say(f"    displays: {[(d['id'], d['key'], d['frame']) for d in s['displays']]}")
        changed = resize_shown(s, 1)
        others = [num for num in sorted(ts.stage_titles(s)) if num != s["displays"][0].get("active")]
        if others:
            ctl("switch", others[0])
            time.sleep(1.5)
            changed += resize_shown(ts.state(), 4)
        time.sleep(QUIET_WAIT)
        s = ts.state()
        both = kept_frames(s)
        say(f"    two screens: resized {changed}; kept {[(t, k, ts.fmt(r)) for t, (k, r) in sorted(both.items())]}")
        check(bool(changed), f"frames {kind} {n}: windows resized with the virtual screen there ({changed})")

        # The virtual screen goes: the built-in screen alone's frames come back.
        mark = len(ts.log_text().splitlines())
        say(f"    screen removed: {screen.stop()}")
        line = settled_line(mark, PUT_BACK)
        if check(bool(line), f"frames {kind} {n}: the screen gone, the log says the windows were put back"):
            say(f"    log: {line[:300]}")
            counts = log_counts(line)
            check(counts.get("put_back", 0) >= 3 and counts.get("elsewhere", 1) == 0, f"frames {kind} {n}: every window put back ({counts})")
        time.sleep(3)
        matched, problems, elsewhere = frames_match(ts.state(), alone, "alone again")
        check(not problems and not elsewhere and len(matched) == len(alone),
              f"frames {kind} {n}: every window at its frame of the built-in screen alone ({matched})"
              + (f": {problems[:6]}" if problems else "") + (f"; elsewhere {elsewhere}" if elsewhere else ""))

        # The virtual screen again: the two-screen frames come back where macOS puts the windows
        # on the screen they were kept on.
        mark = len(ts.log_text().splitlines())
        screen = Screen(kind == "main")
        say(f"    virtual screen again: {screen.info}")
        line = settled_line(mark, PUT_BACK)
        counts = {}
        if check(bool(line), f"frames {kind} {n}: the screen back, the log says the windows were put back"):
            say(f"    log: {line[:300]}")
            counts = log_counts(line)
        time.sleep(3)
        matched, problems, elsewhere = frames_match(ts.state(), both, "both again")
        say(f"    on the screen their frame was kept on: {matched}; on another (macOS's): {elsewhere}")
        check(not problems and bool(matched), f"frames {kind} {n}: every window on its screen at its two-screen frame"
              + (f": {problems[:6]}" if problems else ""))
        check(not counts or counts.get("elsewhere") == len(elsewhere),
              f"frames {kind} {n}: the log counts {counts.get('elsewhere')} elsewhere, {len(elsewhere)} seen on another screen")
    finally:
        if screen is not None and screen.proc.poll() is None:
            say(f"    screen removed (cleanup): {screen.stop()}")
        ts.stop_app()
        ts.stop_all_fixtures()
        ts.write_config(None)
        log = ts.archive_log()
        say(f"    log kept: {log}")


def main():
    if ts.owner_app_running():
        say("STOP: another Stage Manager is running (the owner's?). Quit it first.")
        sys.exit(2)
    say(f"display-gone{' frames' if FRAMES else ''}: {KINDS} x {ROUNDS}, engine {ts.ENGINE}, test home {ts.HOME}")
    try:
        for kind in KINDS:
            for n in range(1, ROUNDS + 1):
                if FRAMES:
                    if kind != "ahead":
                        frames_round(kind, n)
                elif kind == "ahead":
                    ahead_round(n)
                else:
                    one_round(kind, n)
        if not FRAMES and "ahead" in KINDS:
            check(bool(DOCK_RAN), "the Dock step ran in at least one round" + (f": {DOCK_RAN}" if DOCK_RAN else ": skipped in every round"))
        elif not FRAMES and not DOCK_RAN:
            say("  the Dock step was skipped in every round (only the `ahead` kind sets up a stage to click for)")
    finally:
        ts.stop_app()
        ts.stop_all_fixtures()
    say(f"\n{len(ts.passed)} passed, {len(ts.failed)} failed")
    for f in ts.failed:
        say(f"  FAIL {f}")
    sys.exit(1 if ts.failed else 0)


if __name__ == "__main__":
    main()
