#!/usr/bin/env python3
"""Guided live check of Spaces (Desktops), run with a person at the Mac: it needs two
Desktops and asks you to switch between them (a script never switches Spaces itself).

Run it as `scripts/test-stages.sh spaces`: that builds the app and the fixtures, copies the
app to the fixed dev location, then runs this script.

Like test_stages.py, the app runs with its own STAGE_MANAGER_HOME and STAGE_MANAGER_ONLY
(the fixtures only): only fixture windows ever move. No other Stage Manager may run (both
would manage the fixtures). Prompts say exactly what to do; answer with Return.

Checks: each Desktop has its own stages and strip; the same app has stages on both; a card
click on Desktop 2 switches stages and never changes the Desktop (the strip panel is on the
Desktop shown); going back shows Desktop 1's stages; quit with a window parked on the other
Desktop brings it back (Accessibility reaching another Space); a relaunch rebuilds both
Desktops' stages; kill -9 with a window parked on the other Desktop: the guardian waits,
says what for, brings it back as soon as you go there and only then ends; a full-screen
window's Space has no strip, entering and leaving full screen (twice) keeps the Desktop's id,
its stages and windows, and the engine's view does not flap.

Every app run's log is kept (stage-manager.run-NN.log in the test home; the guardian writes
on into its run's file).
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


def wait_for(what, cond, timeout):
    """Polls `cond` until it holds for a second; no Return needed. None on timeout."""
    end, since = time.time() + timeout, None
    while time.time() < end:
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
    switch_to("Switch to Desktop 2 (Control-Right Arrow)")
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
    clear_of_strip(s)
    s = state()
    panel = wait_for("the strip panel on Desktop 2",
                     lambda: (lambda p: p.get("on_active_space") and p.get("ordered_in") and p.get("shown"))(state().get("strip_panel") or {}), 4)
    say(f"    strip panel: {state().get('strip_panel')}")
    check(panel is True, "the strip panel is on Desktop 2, ordered in and slid in")
    listed = ts.strip_panel([w for w in ts.our_windows()])
    check(listed is not None, f"the window server lists the strip on screen ({listed and listed['bounds']})")
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
    switch_to("Switch back to Desktop 1 (Control-Left Arrow)")
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
    switch_to("Switch to Desktop 2")
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
    switch_to("Switch to Desktop 1")
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
    switch_to_windows("Switch to Desktop 2", d2_ids)
    back = wait_for("the parked window to come back", lambda: not parked_anywhere(), 8)
    check(back is True, "the guardian brought Desktop 2's parked window back once Desktop 2 showed")
    ended = wait_for("the guardian to end", lambda: not ts.pids_of(f"stage-manager --guardian {app}$"), 6)
    check(ended is True, "the guardian ended once nothing was left")
    for line in guardian_lines()[-8:]:
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
        say(f"\n>>> Round {round_no} of 2: make a Fixture window full screen (green button) within a minute;")
        say("    the test notices by itself, then asks you to leave full screen")
        if not wait_for("a full-screen window", lambda: spaces().get("fullscreen") is True, 60):
            say("    full-screen check skipped")
            break
        check(True, f"round {round_no}: the engine knows a full-screen Space shows")
        check(holds(lambda: spaces().get("fullscreen") is True, 3), f"round {round_no}: it stays known for three seconds (no flapping)")
        panel = ts.strip_panel([w for w in ts.our_windows() if w.get("onscreen", True)])
        check(panel is None, f"round {round_no}: no strip on the full-screen Space ({panel and panel['bounds']})")
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
    say(f"\n{len(ts.passed)} passed, {len(ts.failed)} failed (every app run's log: {ts.HOME}/stage-manager*.log)")
    for f in ts.failed:
        say(f"  FAIL {f}")
    return 1 if ts.failed else 0


if __name__ == "__main__":
    sys.exit(main())
