#!/usr/bin/env python3
"""What macOS keeps on screen of a parked window, per way of asking the park frame.

Measures, on a private fixture app's windows only, whether macOS accepts a park frame asked
with no point inside the display's side (x = max_x), or further out, and what each window
chrome keeps on screen when its top is asked at, below or far below the display's bottom
edge (measured on the owner's apps 10-13: Safari keeps 52 pt, Chrome 41, Telegram 66, most
32). `stage-manager probe-park` does the moves (see src/platform/park_probe.rs); this script
builds and opens the fixture with one window per chrome (plain title bar, unified toolbar,
compact, expanded, full-size content under a transparent title bar, a toolbar with the title
hidden), runs the probe for the bottom-right corner (and bottom-left with --left), puts every
window back, quits the fixture and prints a table plus the config lines of the best request.

    scripts/probe_park.py [--left] [--settle MS] [--out FILE]

Safety: only the fixture's windows move (bundle ID dev.eugene.stagemanager.fixture.park, no
other run uses it); nothing runs while any Stage Manager is up (it would manage the fixture);
no screen is captured. The JSON goes to --out (default: a new file under /tmp), never the
repo.
"""

import json
import os
import signal
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "stage-manager")
FIX_ID = "dev.eugene.stagemanager.fixture.park"
FIX_APP = os.path.join(ROOT, "target", "SM Fixture Park.app")
FIX_EXE = "SM Fixture Park.app/Contents/MacOS/sm-fixture"
STYLES = ["plain", "toolbar", "compact", "expanded", "fullsize", "titleless"]


def say(msg):
    print(msg, flush=True)


def pids_of(pattern):
    r = subprocess.run(["pgrep", "-f", pattern], capture_output=True, text=True)
    return [int(p) for p in r.stdout.split()]


def stop_fixture():
    for pid in pids_of(FIX_EXE):
        os.kill(pid, signal.SIGTERM)
    end = time.time() + 10
    while pids_of(FIX_EXE) and time.time() < end:
        time.sleep(0.2)


def fixture_window_count():
    r = subprocess.run([BIN, "probe-windows", FIX_ID], capture_output=True, text=True, timeout=10)
    try:
        return len([w for w in json.loads(r.stdout)["windows"] if w["layer"] == 0 and w["onscreen"]])
    except (json.JSONDecodeError, KeyError):
        return 0


def build():
    env = dict(os.environ, FIXTURE_ID=FIX_ID, FIXTURE_APP=FIX_APP, PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ["PATH"])
    subprocess.run(["nice", "-n", "18", "cargo", "build", "--release", "--manifest-path", os.path.join(ROOT, "Cargo.toml")], check=True, env=env)
    subprocess.run([os.path.join(ROOT, "scripts", "bundle.sh"), "fixture"], check=True, env=env, cwd=ROOT, stdout=subprocess.DEVNULL)
    subprocess.run([os.path.join(ROOT, "scripts", "sign.sh"), FIX_APP], check=True, env=env, cwd=ROOT, stdout=subprocess.DEVNULL)


def probe(corner, settle):
    r = subprocess.run([BIN, "probe-park", FIX_ID, "--corner", corner, "--settle", str(settle)],
                       capture_output=True, text=True, timeout=600)
    if r.returncode != 0:
        say(r.stdout + r.stderr)
        return None
    return json.loads(r.stdout)


def table(result):
    say(f"\n{result['corner']}: display {result['display']}, visible {result['visible']}")
    say("  window                          request (inside, drop)   landed x,y          on screen  shows w x h")
    for w in result["windows"]:
        for s in w["samples"]:
            l = s["landed"]
            say("  {:<32} {:>6} {:>6}            {:>8.0},{:<8.0}   {:<9}  {:.0} x {:.0}".format(
                w["title"][:32], s["inside"], s["drop"], l["x"], l["y"], str(s["onscreen"]), s["shown_w"], s["shown_h"]))
        say(f"  {w['title']}: best {w['best']}; back at {w['back']} (was {w['frame']})")
    say(f"  first request, worst window shows {result['first_request_worst_area']} pt²")
    say(f"  best request: {result['best']}")
    if result.get("config"):
        say("  config:\n    " + result["config"].replace("\n", "\n    "))


def main():
    left = "--left" in sys.argv
    settle = 250
    out = None
    args = sys.argv[1:]
    for i, a in enumerate(args):
        if a == "--settle":
            settle = int(args[i + 1])
        if a == "--out":
            out = args[i + 1]
    if pids_of("Stage Manager.app/Contents/MacOS/stage-manager$") or pids_of("target/(debug|release)/stage-manager$"):
        say("A Stage Manager is running: it would manage the fixture's windows. Quit it first.")
        return 2
    if not os.environ.get("SKIP_BUILD"):
        build()
    stop_fixture()
    try:
        subprocess.run(["open", "-n", "-g", FIX_APP, "--args", "--windows", str(len(STYLES)),
                        "--styles", ",".join(STYLES), "--background"], check=True)
        end = time.time() + 15
        while fixture_window_count() < len(STYLES) and time.time() < end:
            time.sleep(0.3)
        if fixture_window_count() < len(STYLES):
            say("the fixture's windows did not show")
            return 1
        results = []
        for corner in ["bottom-right"] + (["bottom-left"] if left else []):
            r = probe(corner, settle)
            if r is None:
                return 1
            results.append(r)
            table(r)
    finally:
        stop_fixture()
    path = out or tempfile.mkstemp(prefix="probe-park.", suffix=".json", dir="/tmp")[1]
    with open(path, "w") as f:
        json.dump(results, f, indent=1)
    say(f"\nwrote {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
