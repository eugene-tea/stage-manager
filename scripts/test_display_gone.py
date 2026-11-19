#!/usr/bin/env python3
"""Live check: a screen going away keeps every stage's windows.

Run it as `SM_ENGINE=spaces scripts/test-stages.sh display-gone [main|secondary] [--rounds N]`
(default: both kinds, 3 rounds each), or without `SM_ENGINE` for the Accessibility (Corner)
engine. Nobody needs to be at the Mac, but nobody may use it
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
KINDS = [k for k in ARGS if k in ("main", "secondary")] or ["main", "secondary"]
ROUNDS = int(ARGS[ARGS.index("--rounds") + 1]) if "--rounds" in ARGS else 3
WATCH_SECONDS = float(os.environ.get("SM_DISPLAY_GONE_WATCH") or 65)
TOOL_BUILD = os.path.join(ts.ROOT, "scripts", "virtual-display", "build.sh")
PID_FILE = os.path.join(ts.HOME, "virtual-display.pid")
KEPT = "display: a gone screen's hidden windows kept in their stages"
FOREIGN = "windows of another Space are on screen"


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
        while time.time() - t0 < WATCH_SECONDS:
            try:
                look(before, f"{int(time.time() - t0)} s", out)
            except Exception as e:  # noqa: BLE001 - the app may be busy with the change
                say(f"    look at {int(time.time() - t0)} s: {e}")
            time.sleep(1.0)
        problems_watch = sorted(out)
        check(not problems_watch, f"{kind} {n}: {WATCH_SECONDS:.0f} s after the screen went, every stage keeps its windows "
              "and no hidden one shows" + (f": {problems_watch[:8]}" if problems_watch else ""))
        out.clear()
        seen = desktops_tour(before, out, home)
        check(not out, f"{kind} {n}: on every Desktop ({len(seen)}), the same" + (f": {sorted(out)[:8]}" if out else ""))
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


def main():
    if ts.owner_app_running():
        say("STOP: another Stage Manager is running (the owner's?). Quit it first.")
        sys.exit(2)
    say(f"display-gone: {KINDS} x {ROUNDS}, engine {ts.ENGINE}, test home {ts.HOME}")
    try:
        for kind in KINDS:
            for n in range(1, ROUNDS + 1):
                one_round(kind, n)
    finally:
        ts.stop_app()
        ts.stop_all_fixtures()
    say(f"\n{len(ts.passed)} passed, {len(ts.failed)} failed")
    for f in ts.failed:
        say(f"  FAIL {f}")
    sys.exit(1 if ts.failed else 0)


if __name__ == "__main__":
    main()
