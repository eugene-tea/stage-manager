#!/usr/bin/env python3
"""Five live probes the rework waits on (rounds r0-probes and r2-probes2, 11-21; the review's
R3 and R4).

Run them as `scripts/test-stages.sh probe-system [dock] [flip] [names] [indicator] [cgs]
[--rounds N] [--window ID]` (which builds the app and the fixtures and refreshes the dev copy),
or with `python3 scripts/probe_system.py …` once built. With no name: dock, flip and names.
`indicator` runs only by name: it may raise macOS's screen-capture alert, so the owner is at
the Mac for it.

Each probe is a subcommand of the dev app's binary (`stage-manager probe-spaces …`,
src/platform/system_probe/), launched through LaunchServices (`open -n -W … --stdout FILE
--args …`) so it has the dev app's Accessibility and Screen Recording. This script opens the
fixture windows it needs, streams what the probe prints (plain tables, `VERDICT` lines, and
`RESULT measured` or `RESULT could not measure: …` last), and quits the fixtures after, also on
Ctrl-C. Nothing is written to the repo: the probe's output file is in the test home (/tmp).

  dock       Does the Dock announce Mission Control, App Exposé and Show Desktop
             (`AXExpose…` on the Dock, through Accessibility), and how soon against the window
             list read every 2 ms? Alpha (one window) in front; Mission Control's shortcut and
             key, App Exposé's and Show Desktop's shortcuts posted, each closed by the same key
             and by Escape, N times. Nobody may use the Mac (keys are posted). About 2-3 min.
  flip       When does the window server's "Current Space" flip during a slide (read every
             1 ms), against activeSpaceDidChangeNotification and the window list? Alpha and
             Bravo (one window each); Bravo's moved to the next Desktop of the main screen.
             The Desktop keys, Bravo then Alpha opened, a Desktop picked in Mission Control,
             Alpha's window into full screen and out, each N times there and back. Needs two
             Desktops on the main screen for all but full screen (skipped and said with one).
             The probe turns as an app does (`[NSApp run]`'s event fetch), so the notice comes.
             Nobody may use the Mac. About 2-3 min.
  names      Read only: SLSCopyManagedDisplaySpaces's screens and Spaces (identifiers by
             shape, never values), separate Spaces per display, SLSCopySpacesForWindows for a
             normal fixture window and one on every Desktop (Charlie started with
             --all-spaces), and for a window set to "All Desktops" by hand (--window ID; the
             probe prints how), and the call's cost over 2000 calls. Seconds.
  indicator  Which capture lights the purple screen-recording indicator: the window
             server's two calls, a ScreenCaptureKit still and a stream, 5 s each with 5 s of
             rest, on a fixture window on screen and one hidden in a stage Space, while the
             menu bar's windows are read every 100 ms; kinds the list cannot tell run again
             with a 3 s countdown each, for the owner to watch. About 1-2 min. Says how many
             screen-access alerts macOS showed meanwhile.
  cgs        Does the window server tell its clients? Every notice type 0..2000 registered
             both ways (SLSRegisterConnectionNotifyProc, SLSRegisterNotifyProc) and the
             fixture windows' own notices asked for (SLSRequestNotificationsForWindows), while
             N rounds of: Mission Control, App Exposé and Show Desktop opened and closed by
             their shortcuts, the Desktop key there and back, Bravo then Alpha opened (slides),
             Alpha into full screen and out, minimized and back, a Charlie window closed, and
             Alpha into a Space the probe made and back; 3 s rests between. Per action the
             types that came within 1.5 s and in no rest; the rests' types as noise; verdicts
             for Mission Control's open and close, a Desktop change, a Space membership
             change. Alpha, Bravo (on the next Desktop) and Charlie (N+1 windows). Nobody may
             use the Mac. About 2 min a round.

It refuses to run while any Stage Manager runs (the owner's or a test's). Only fixture windows
are moved, and each probe puts them back.
"""

import atexit
import os
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import test_stages as ts  # noqa: E402

say = ts.say

PROBES = {"dock": "dock-notices", "flip": "space-flip", "names": "names", "indicator": "indicator", "cgs": "cgs"}
DEFAULT = ["dock", "flip", "names"]
ALPHA, BRAVO, CHARLIE = ts.EXTRA["a"], ts.EXTRA["b"], ts.EXTRA["c"]


def parse(argv):
    names, rounds, window, i = [], 3, None, 0
    while i < len(argv):
        a = argv[i]
        if a in PROBES:
            names.append(a)
        elif a == "--rounds" and i + 1 < len(argv):
            rounds, i = int(argv[i + 1]), i + 1
        elif a == "--window" and i + 1 < len(argv):
            window, i = argv[i + 1], i + 1
        else:
            sys.exit(f"unknown argument {a!r}; probes: {', '.join(PROBES)}; options: --rounds N, --window ID")
        i += 1
    return names or DEFAULT, rounds, window


def probe_pids():
    return ts.pids_of("Contents/MacOS/stage-manager probe-spaces ")


def launch(sub, *args, timeout):
    """`stage-manager probe-spaces SUB ARGS` from the dev app through LaunchServices, its
    output printed as it comes; the RESULT line, or None when it never came."""
    out, err = (os.path.join(ts.HOME, f"probe-{sub}.{k}") for k in ("out", "err"))
    for p in (out, err):
        open(p, "w").close()
    cmd = ["open", "-n", "-W", "-g", "--stdout", out, "--stderr", err, "--env", f"STAGE_MANAGER_HOME={ts.HOME}",
           ts.APP, "--args", "probe-spaces", sub, *map(str, args)]
    proc = subprocess.Popen(cmd)
    result, end = None, time.time() + timeout
    with open(out) as f:
        while True:
            where = f.tell()
            line = f.readline()
            if line and (line.endswith("\n") or proc.poll() is not None):
                say("  " + line.rstrip("\n"))
                if line.startswith("RESULT"):
                    result = line.strip()
                continue
            if line:
                f.seek(where)
            if proc.poll() is not None:
                break
            if time.time() > end:
                say(f"  the probe ran past {timeout} s: stopped")
                for pid in probe_pids():
                    os.kill(pid, signal.SIGTERM)
                proc.wait(10)
                break
            time.sleep(0.05)
    with open(err) as f:
        lines = [x.rstrip() for x in f if x.strip()]
    if lines:
        say("  stderr:\n" + "\n".join("    " + x for x in lines[-20:]))
    return result


def run_dock(rounds, _window):
    ts.start_extra("a", 1)
    ts.activate("a")
    time.sleep(1.0)
    return launch("dock-notices", "--rounds", rounds, timeout=90 + 80 * rounds)


def run_flip(rounds, _window):
    ts.start_extra("a", 1)
    ts.start_extra("b", 1)
    ts.activate("a")
    time.sleep(1.0)
    return launch("space-flip", ALPHA[0], BRAVO[0], "--rounds", rounds, timeout=120 + 90 * rounds)


def run_names(_rounds, window):
    ts.start_extra("a", 1)
    ts.start_extra_args("c", 1, "--all-spaces")
    time.sleep(0.5)
    extra = ["--window", window] if window else []
    return launch("names", ALPHA[0], "--sticky", CHARLIE[0], *extra, timeout=120)


def run_indicator(_rounds, _window):
    since = time.strftime("%Y-%m-%d %H:%M:%S")
    ts.start_fixture(2)
    time.sleep(0.5)
    r = launch("indicator", ts.FIX_ID, timeout=300)
    time.sleep(1.0)
    alerts = ts.replayd_alerts(since)
    say(f"  screen-access alerts macOS showed during the probe (any app): {'unknown' if alerts is None else alerts}")
    return r


def run_cgs(rounds, _window):
    ts.start_extra("a", 1)
    ts.start_extra("b", 1)
    ts.start_extra("c", rounds + 1)
    ts.activate("a")
    time.sleep(1.0)
    return launch("cgs", ALPHA[0], BRAVO[0], CHARLIE[0], "--rounds", rounds, timeout=120 + 180 * rounds)


def cleanup():
    for pid in probe_pids():
        os.kill(pid, signal.SIGTERM)
    ts.stop_all_fixtures()


def main():
    names, rounds, window = parse(sys.argv[1:])
    if not os.access(ts.BIN, os.X_OK):
        sys.exit(f"{ts.BIN} missing: run scripts/test-stages.sh probe-system (it builds the dev copy)")
    if ts.pids_of("Stage Manager.app/Contents/MacOS/stage-manager$") or ts.pids_of("Stage Manager.app/Contents/MacOS/stage-manager --guardian "):
        sys.exit("a Stage Manager is running (the owner's, or a test's): quit it first; no probe runs beside it")
    if not all(os.path.exists(p) for p in (ALPHA[1], BRAVO[1], CHARLIE[1], ts.FIX_APP)):
        sys.exit("the fixtures are missing: run scripts/test-stages.sh probe-system (it builds them)")
    front = ts.frontmost_bundle()
    atexit.register(cleanup)
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, lambda *_: sys.exit(130))
    runs = {"dock": run_dock, "flip": run_flip, "names": run_names, "indicator": run_indicator, "cgs": run_cgs}
    results = []
    try:
        for name in names:
            say(f"\n######## probe {name} (probe-spaces {PROBES[name]}) ########")
            r = runs[name](rounds, window)
            results.append((name, r))
            ts.stop_all_fixtures()
    finally:
        if front and front not in ts.ALL_IDS:
            subprocess.run(["open", "-b", front], capture_output=True)
    say("\nsummary:")
    for name, r in results:
        say(f"  {name}: {r or 'RESULT none (the probe printed no result line)'}")
    return 0 if all(r == "RESULT measured" for _, r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
