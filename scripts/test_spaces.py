#!/usr/bin/env python3
"""Guided live check of Spaces (Desktops), run with a person at the Mac: it needs two
Desktops and asks you to switch between them (a script never switches Spaces itself).

Run it as `scripts/test-stages.sh spaces`: that builds the app and the fixtures, copies the
app to the fixed dev location, then runs this script.

Like test_stages.py, the app runs with its own STAGE_MANAGER_HOME and STAGE_MANAGER_ONLY
(the fixtures only): only fixture windows ever move. No other Stage Manager may run (both
would manage the fixtures). Prompts say exactly what to do; answer with Return.

Checks: each Desktop has its own stages and strip; the same app has stages on both; a card
click never changes the Desktop; going back shows Desktop 1's stages; quit with a window
parked on the other Desktop brings it back (Accessibility reaching another Space); a
relaunch rebuilds both Desktops' stages; kill -9 with a window parked on the other Desktop:
the guardian brings it back when you go there; a full-screen window's Space has no strip.
"""

import os
import signal
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import test_stages as ts  # noqa: E402

say, check, ctl, state = ts.say, ts.check, ts.ctl, ts.state


def ask(what):
    say(f"\n>>> {what}")
    input("    then press Return… ")


def yes(question):
    return input(f"\n??? {question} [y/N] ").strip().lower().startswith("y")


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


def other_stage_managers():
    ours = ts.app_pid()
    return [p for p in ts.pids_of("Stage Manager.app/Contents/MacOS/stage-manager$") if p != ours]


def run():
    if other_stage_managers():
        say("STOP: another Stage Manager is running (the owner's?). Quit it first; both would manage the fixtures.")
        ts.failed.append("another Stage Manager runs")
        return
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
    check(len(s["stages"]) == 2, f"two stages on Desktop 1 ({ts.stage_titles(s)})")

    say("\n== Desktop 2: Fixture 3 (the same app) and Bravo 1 ==")
    ask("Switch to Desktop 2 (Control-Right Arrow)")
    pid = ts.pids_of(ts.FIX_EXE)
    if pid:
        os.kill(pid[0], signal.SIGUSR1)
    ts.start_extra("b", 1)
    s = ts.wait_state(lambda x: len(x["windows"]) >= 5, "five managed windows", timeout=10)
    if not check(s is not None, "Desktop 2's windows are managed"):
        return
    d2 = s["space"]
    check(d2 != d1, f"Desktop 2 is its own Space ({d1} then {d2})")
    check(titles_on_space(s, d2) == ["Bravo 1", "Fixture 3"], f"Desktop 2's stages hold only its windows: {titles_on_space(s, d2)}")
    check(set(titles_on_space(s, d1)) == {"Alpha 1", "Fixture 1", "Fixture 2"}, "Desktop 1's stages are untouched")
    check(set(strip_titles(s)) <= {"Bravo 1", "Fixture 3"}, f"the strip shows only Desktop 2's windows: {strip_titles(s)}")
    check(yes("Does the strip on Desktop 2 show only Bravo or Fixture 3 (no Fixture 1, 2 or Alpha)?"), "strip on Desktop 2 (by eye)")

    say("\n== a card click on Desktop 2 never changes the Desktop ==")
    vis = s["display"]["visible"]
    x, y = vis["x"] + 70, vis["y"] + vis["h"] / 2
    time.sleep(1.5)
    before = s["active"]
    r = ctl("debug-click", x, y)
    check(r.get("ok"), f"clicked the card ({r.get('error', 'ok')})")
    s = ts.wait_state(lambda x: x["active"] != before, "the card click to switch", timeout=4)
    check(s is not None, "the card click switched stages")
    time.sleep(1.5)
    check(current_space() == d2, "still on Desktop 2 (as the engine sees it)")
    check(not yes("Did the screen jump to Desktop 1 (even for a moment)?"), "no jump to Desktop 1 (by eye)")
    check(ctl("switch", 1).get("ok") and current_space() == d2, "a hotkey switch stays on Desktop 2")

    say("\n== back on Desktop 1 ==")
    ask("Switch back to Desktop 1 (Control-Left Arrow)")
    s = ts.wait_state(lambda x: x["space"] == d1, "Desktop 1 recognized", timeout=5)
    check(s is not None, "Desktop 1 recognized by its windows")
    if s:
        check(set(strip_titles(s)) <= {"Alpha 1", "Fixture 1", "Fixture 2"}, f"Desktop 1's strip: {strip_titles(s)}")
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
    ask("Switch to Desktop 2")
    s = ts.wait_state(lambda x: x["space"] == d2, "Desktop 2 rebuilt", timeout=6)
    check(s is not None, "Desktop 2 rebuilt with its saved id")
    if s:
        check(titles_on_space(s, d2) == ["Bravo 1", "Fixture 3"], f"Desktop 2's windows in its stages: {titles_on_space(s, d2)}")

    say("\n== kill -9 with a window parked on the other Desktop: the guardian ==")
    s = state()
    if s["space"] == d2 and not [w for w in s["windows"] if w["space"] == d2 and w["parked"]]:
        ctl("switch", 1 if s["active"] != 1 else 2)
    check(any(w["space"] == d2 and w["parked"] for w in state()["windows"]), "a window is parked on Desktop 2")
    ask("Switch to Desktop 1")
    time.sleep(1.5)
    app = ts.app_pid()
    guardians = ts.guardian_pids(app)
    check(bool(guardians), "the guardian runs")
    os.kill(app, signal.SIGKILL)
    ts.wait_gone(app)
    time.sleep(3)
    still = bool(ts.pids_of(f"stage-manager --guardian {app}$"))
    say(f"    guardian still watching: {still}; parked anywhere now: {[w['id'] for w in parked_anywhere()]}")
    ask("Switch to Desktop 2 and wait three seconds")
    time.sleep(3)
    left = parked_anywhere()
    check(not left, "the guardian brought Desktop 2's parked window back once Desktop 2 showed")
    check(not ts.pids_of(f"stage-manager --guardian {app}$"), "the guardian ended once nothing was left")

    say("\n== a full-screen app's Space has no strip ==")
    s = ts.start_app(5, extra_env={"STAGE_MANAGER_TEST": "1"})
    if yes("Optional: make a Fixture window full screen (green button) now and answer y once it is; n skips"):
        time.sleep(2)
        check(spaces().get("fullscreen") is True, "the engine knows a full-screen Space shows")
        check(not yes("Is a strip (cards on the left) visible over the full-screen window?"), "no strip on the full-screen Space (by eye)")
        ask("Leave full screen (green button or Control-Command-F)")
        time.sleep(2)
        check(spaces().get("fullscreen") is False, "back on a Desktop")
    ts.stop_app()
    left = parked_anywhere()
    check(not left, "the end: no fixture window parked on any Desktop")


def main():
    if not os.path.exists(ts.BIN):
        say("build first: scripts/test-stages.sh builds the app and the fixtures")
        return 2
    say(f"home: {ts.HOME}")
    try:
        run()
    finally:
        ts.stop_app()
        ts.stop_all_fixtures()
    say(f"\n{len(ts.passed)} passed, {len(ts.failed)} failed")
    for f in ts.failed:
        say(f"  FAIL {f}")
    return 1 if ts.failed else 0


if __name__ == "__main__":
    sys.exit(main())
