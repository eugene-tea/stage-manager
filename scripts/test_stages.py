#!/usr/bin/env python3
"""Scripted checks of milestones 3, 4 and 6 against a private fixture app.

Run through scripts/test-stages.sh. Every run of the app here sets STAGE_MANAGER_HOME (a
scratch directory: config, state, socket, log) and STAGE_MANAGER_ONLY (the fixture's own
bundle ID, which no other run uses), so no other window is ever touched. Each phase ends with
every fixture window onscreen; the app and the fixture are quit at the end, whatever happens.

Phases: m3 (stages, switching, frames, z-order, merge, pin, reveal, children), safety (quit,
SIGTERM, SIGINT, panic, kill -9 with the guardian, kill -9 without it), m4 (rules, placement,
hot reload), m6 (persistence across a relaunch of the app and of the fixture), strip (a
synthetic click and shift-click on a card of the real strip; the click is posted by the app
only when its own strip window is topmost at that point).
"""

import json
import os
import signal
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
APP = os.path.join(ROOT, "target", "Stage Manager.app")
BIN = os.path.join(APP, "Contents", "MacOS", "stage-manager")
FIX_ID = "dev.eugene.stagemanager.fixture.stages"
FIX_APP = os.path.join(ROOT, "target", "SM Fixture Stages.app")
FIX_EXE = "SM Fixture Stages.app/Contents/MacOS/sm-fixture"
PARK_MARGIN = 64.0

HOME = os.environ.get("TEST_HOME") or tempfile.mkdtemp(prefix="sm-test.", dir="/tmp")
os.makedirs(HOME, exist_ok=True)
ENV = dict(os.environ, STAGE_MANAGER_HOME=HOME, STAGE_MANAGER_ONLY=FIX_ID)
LOG = os.path.join(HOME, "stage-manager.log")

passed, failed = [], []


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


def probe():
    r = run([BIN, "probe-windows", FIX_ID], timeout=10)
    return json.loads(r.stdout)


def wait_until(fn, timeout, what):
    end = time.time() + timeout
    last = None
    while time.time() < end:
        try:
            last = fn()
            if last:
                return last
        except Exception as e:  # noqa: BLE001 - polling a live system
            last = e
        time.sleep(0.2)
    say(f"  timeout waiting for {what} (last: {last!r})")
    return None


# ----- geometry --------------------------------------------------------------------------


def is_parked(b, d):
    """engine::park::is_parked on the main display."""
    x, y, w, h = b["x"], b["y"], b["w"], b["h"]
    dx, dy, dw, dh = d["x"], d["y"], d["w"], d["h"]
    if x >= dx + dw - PARK_MARGIN and y >= dy + dh - PARK_MARGIN:
        return True
    if x <= dx + dw / 2 or y <= dy + dh / 2:
        return False
    sx = min(x + w, dx + dw) - max(x, dx)
    sy = min(y + h, dy + dh) - max(y, dy)
    if sx <= 0 or sy <= 0:
        return w > 0 and h > 0
    return sx <= PARK_MARGIN and sy <= PARK_MARGIN


def close(a, b, tol=1.0):
    return all(abs(a[k] - b[k]) <= tol for k in ("x", "y", "w", "h"))


def fmt(b):
    return "{:.0f},{:.0f} {:.0f}x{:.0f}".format(b["x"], b["y"], b["w"], b["h"])


# ----- processes -------------------------------------------------------------------------


def pids_of(pattern):
    r = subprocess.run(["pgrep", "-f", pattern], capture_output=True, text=True)
    return [int(p) for p in r.stdout.split()]


def fixture_windows():
    """Fixture windows on layer 0 as the window server sees them, with the display."""
    p = probe()
    return [w for w in p["windows"] if w["layer"] == 0], p["display"]


def start_fixture(n, *extra):
    stop_fixture()
    subprocess.run(["open", "-n", "-g", FIX_APP, "--args", "--windows", str(n), "--background", *extra], check=True)
    want = n + (1 if "--panel" in extra else 0)
    return wait_until(lambda: len(fixture_windows()[0]) >= want, 15, f"{want} fixture windows")


def stop_fixture():
    for pid in pids_of(FIX_EXE):
        os.kill(pid, signal.SIGTERM)
    wait_until(lambda: not pids_of(FIX_EXE), 10, "the fixture to quit")


def app_pid():
    r = ctl("ping")
    return r.get("pid") if r.get("ok") else None


def guardian_pids():
    return pids_of("stage-manager --guardian")


def start_app(expect_windows, extra_env=None, fresh_log=True):
    if app_pid():
        stop_app()
    if fresh_log and os.path.exists(LOG):
        os.remove(LOG)
    args = ["open", "-n", APP]
    env = {
        "STAGE_MANAGER_HOME": HOME,
        "STAGE_MANAGER_ONLY": FIX_ID,
        "STAGE_MANAGER_NO_PROMPT": "1",
        "RUST_LOG": "info,stage_manager=debug",
    }
    env.update(extra_env or {})
    for k, v in env.items():
        args += ["--env", f"{k}={v}"]
    subprocess.run(args, check=True)
    ok = wait_until(app_pid, 15, "the app")
    if not ok:
        return None
    return wait_until(
        lambda: (lambda s: s if s.get("ok") and len(s["windows"]) >= expect_windows else None)(ctl("state")),
        20,
        f"{expect_windows} managed windows",
    )


def wait_gone(pid, timeout=10):
    def gone():
        try:
            os.kill(pid, 0)
            return False
        except ProcessLookupError:
            return True

    return wait_until(gone, timeout, f"process {pid} to exit")


def stop_app():
    pid = app_pid()
    if pid:
        ctl("quit")
        wait_gone(pid)


def log_text():
    try:
        with open(LOG, encoding="utf-8", errors="replace") as f:
            return f.read()
    except FileNotFoundError:
        return ""


# ----- state helpers ---------------------------------------------------------------------


def state():
    s = ctl("state")
    if not s.get("ok"):
        raise RuntimeError(f"ctl state: {s}")
    return s


def by_title(s):
    return {w["title"]: w for w in s["windows"]}


def stage_titles(s):
    """Stage number -> titles of its standard windows, front to back."""
    titles = {w["id"]: w["title"] for w in s["windows"]}
    return {st["number"]: [titles[i] for i in st["windows"]] for st in s["stages"]}


def all_onscreen(what):
    wins, d = fixture_windows()
    stuck = [fmt(w["bounds"]) for w in wins if is_parked(w["bounds"], d)]
    return check(not stuck, f"{what}: every fixture window onscreen" + (f" (parked: {stuck})" if stuck else ""))


def consistent(what):
    """Every managed window is where the model says: shown ones at their restore frame (±1),
    parked ones in the park zone, and nothing parked that the model shows."""
    s = state()
    wins, d = fixture_windows()
    cg = {w["id"]: w for w in wins}
    bad = []
    for w in s["windows"]:
        c = cg.get(w["id"])
        if not c:
            continue
        parked_now = is_parked(c["bounds"], d)
        if w["parked"] and not parked_now:
            bad.append(f"{w['title']} should be parked, is at {fmt(c['bounds'])}")
        if not w["parked"]:
            if parked_now:
                bad.append(f"{w['title']} should be shown, is parked")
            elif w["role"] == "standard" and not close(c["bounds"], w["restore"]):
                bad.append(f"{w['title']} at {fmt(c['bounds'])}, expected {fmt(w['restore'])}")
    return check(not bad, f"{what}: windows match the model" + (f" ({'; '.join(bad)})" if bad else ""))


def z_titles(titles):
    """The given titles ordered front to back by the window server."""
    s = state()
    ids = {w["title"]: w["id"] for w in s["windows"]}
    wins, _ = fixture_windows()
    z = {w["id"]: w["z"] for w in wins}
    present = [t for t in titles if z.get(ids.get(t)) is not None]
    return sorted(present, key=lambda t: z[ids[t]])


def frames_of(titles):
    s = state()
    ids = {w["title"]: w["id"] for w in s["windows"]}
    wins, _ = fixture_windows()
    cg = {w["id"]: w["bounds"] for w in wins}
    return {t: cg[ids[t]] for t in titles}


def do(*words):
    r = ctl(*words)
    time.sleep(0.4)
    if not r.get("ok"):
        say(f"  ctl {' '.join(map(str, words))}: {r}")
    return r


# ----- phases ----------------------------------------------------------------------------


def phase_m3():
    say("\n== milestone 3: stages, switching, frames, z-order, merge, pin, reveal, children ==")
    start_fixture(4, "--panel")
    s = start_app(5)
    if not check(s is not None, "app manages the 4 fixture windows and the panel"):
        return
    t = by_title(s)
    check(len(s["stages"]) == 1 and s["active"] == 1, "one active stage at launch")
    panel = t.get("Fixture Panel")
    check(panel is not None and panel["role"] == "child" and panel["parent"] is not None, "the panel is a child with a parent")
    check("hotkeys registered active=21" in log_text(), "21 hotkeys registered")
    w = {n: t[f"Fixture {n}"]["id"] for n in range(1, 5)}

    do("send", 2, w[3])
    do("send", 3, w[4])
    s = state()
    check(stage_titles(s) == {1: ["Fixture 1", "Fixture 2"], 2: ["Fixture 3"], 3: ["Fixture 4"]}, "send builds stages 2 and 3")
    check(s["strip"] == [2, 3], "stages 2 and 3 are strip cards")
    consistent("after send")

    do("switch", 1)
    before_z = z_titles(["Fixture 1", "Fixture 2"])
    before = frames_of(["Fixture 1", "Fixture 2"])
    for n in (2, 3, 1, 3, 2, 1):
        do("switch", n)
        s = state()
        check(s["active"] == n, f"switch {n}: stage {n} active")
        consistent(f"switch {n}")
    after = frames_of(["Fixture 1", "Fixture 2"])
    check(all(close(before[k], after[k]) for k in before), "stage 1 frames restored exactly (±1 pt)")
    check(z_titles(["Fixture 1", "Fixture 2"]) == before_z, f"stage 1 z-order kept ({before_z})")
    time.sleep(1.2)
    check(state()["active"] == 1, "no bounce from late focus or activation echoes")

    do("switch", 2)
    s = state()
    check(s["active"] == 2, "back on stage 2")
    do("merge", 1)
    s = state()
    check(sorted(stage_titles(s)[s["active"]]) == ["Fixture 1", "Fixture 2", "Fixture 3"], "merge pulls stage 1 into the active stage")
    consistent("after merge")

    do("pin", w[4])
    s = state()
    check(by_title(s)["Fixture 4"]["home"] == "pinned", "pin: window 4 pinned")
    consistent("after pin")
    do("new", w[2])
    do("switch", 2)
    s = state()
    check(not by_title(s)["Fixture 4"]["parked"], "a pinned window stays out on every stage")
    consistent("pinned across a switch")
    do("pin", w[4])
    check(by_title(state())["Fixture 4"]["home"] != "pinned", "pin again unpins")

    active = state()["active"]
    do("reveal")
    s = state()
    shown = [x for x in s["windows"] if not x["parked"] and x["role"] == "standard"]
    check(not shown, "reveal parks the active stage")
    consistent("after reveal")
    do("reveal")
    s = state()
    check(s["active"] == active or len([x for x in s["windows"] if not x["parked"]]) > 0, "a second reveal brings it back")
    consistent("after the second reveal")

    p = by_title(state())["Fixture Panel"]
    parent = next(x for x in state()["windows"] if x["id"] == p["parent"])
    check(p["parked"] == parent["parked"] and p["home"] == parent["home"], "the panel follows its parent")


def ensure_something_parked():
    s = state()
    if any(w["parked"] for w in s["windows"]):
        return True
    ids = [w["id"] for w in s["windows"] if w["role"] == "standard" and w["home"] != "pinned"]
    if ids:
        do("new", ids[0])
    return check(any(w["parked"] for w in state()["windows"]), "drill setup: a window is parked")


def drill(name, kill, evidence):
    say(f"-- drill: {name}")
    start_app(1, extra_env={"STAGE_MANAGER_TEST": "1"})
    ensure_something_parked()
    pid = app_pid()
    guards = guardian_pids()
    kill(pid)
    wait_gone(pid)
    for g in guards:
        wait_gone(g, 15)
    time.sleep(0.5)
    all_onscreen(name)
    text = log_text()
    check(all(e in text for e in evidence), f"{name}: the log shows {evidence}")


def phase_safety():
    say("\n== safety drills ==")
    if not pids_of(FIX_EXE):
        start_fixture(4, "--panel")
    start_app(1, extra_env={"STAGE_MANAGER_TEST": "1"})
    clean = ["unparked every window", "guardian: clean exit"]
    drill("ctl quit", lambda pid: ctl("quit"), clean)
    drill("SIGTERM", lambda pid: os.kill(pid, signal.SIGTERM), ["signal received", *clean])
    drill("SIGINT", lambda pid: os.kill(pid, signal.SIGINT), ["signal received", *clean])
    drill("debug-panic", lambda pid: ctl("debug-panic"), ["debug-panic requested over ctl", "safety: registry unparked"])
    drill("kill -9 (the guardian restores)", lambda pid: os.kill(pid, signal.SIGKILL), ["guardian: recovery scan done", "recovery: window brought back reason=\"guardian\""])

    say("-- drill: kill -9 of the app and the guardian (the next launch restores)")
    start_app(1, extra_env={"STAGE_MANAGER_TEST": "1"})
    ensure_something_parked()
    time.sleep(1.0)  # let state.json catch up (debounced)
    pid = app_pid()
    for g in guardian_pids():
        os.kill(g, signal.SIGKILL)
    os.kill(pid, signal.SIGKILL)
    wait_gone(pid)
    time.sleep(0.5)
    wins, d = fixture_windows()
    check(any(is_parked(w["bounds"], d) for w in wins), "windows were left parked")
    start_app(1, extra_env={"STAGE_MANAGER_TEST": "1"})
    check("recovery: window brought back reason=\"launch\"" in log_text(), "the launch scan brought them back")
    consistent("after the relaunch")
    stop_app()
    all_onscreen("quit after the relaunch")


CONFIG_M4 = f"""
[[rule]]
app = "{FIX_ID}"
title = "^Fixture 2$"
placement = "cascade"

[[rule]]
app = "{FIX_ID}"
title = "^Fixture 3$"
action = "new"

[[rule]]
app = "{FIX_ID}"
title = "^Fixture 4$"
action = "float"
placement = "center"
"""


def phase_m4():
    say("\n== milestone 4: rules, placement, hot reload ==")
    stop_app()
    stop_fixture()
    for f in ("state.json", "config.toml"):
        p = os.path.join(HOME, f)
        if os.path.exists(p):
            os.remove(p)
    with open(os.path.join(HOME, "config.toml"), "w") as f:
        f.write(CONFIG_M4)
    s = start_app(0)
    check(s is not None and s["config_error"] is None, "the app starts with the rules config")
    start_fixture(4)
    s = wait_until(lambda: (lambda x: x if len(x["windows"]) >= 4 else None)(state()), 15, "4 managed windows")
    if not check(s is not None, "the new fixture windows are managed"):
        return
    time.sleep(0.5)
    s = state()
    t = by_title(s)
    check(stage_titles(s) == {1: ["Fixture 2", "Fixture 1"], 2: ["Fixture 3"]}, f"Fixture 3 opened a new stage ({stage_titles(s)})")
    check(s["active"] == 2, "and the new stage is active")
    check(t["Fixture 4"]["home"] == "pinned", "Fixture 4 floats (pinned)")
    vis = s["display"]["visible"]
    f4 = frames_of(["Fixture 4"])["Fixture 4"]
    cx = vis["x"] + (vis["w"] - f4["w"]) / 2
    cy = vis["y"] + (vis["h"] - f4["h"]) / 2
    check(abs(f4["x"] - cx) <= 1 and abs(f4["y"] - cy) <= 1, f"Fixture 4 centered ({fmt(f4)})")
    r1, r2 = t["Fixture 1"]["restore"], t["Fixture 2"]["restore"]
    check(abs(r2["x"] - r1["x"] - 32) <= 1 and abs(r2["y"] - r1["y"] - 32) <= 1, "Fixture 2 cascaded 32 pt from Fixture 1")
    consistent("after the rules")

    with open(os.path.join(HOME, "config.toml"), "a") as f:
        f.write('\n[stages]\nnames = ["Mail"]\n')
    s = wait_until(lambda: (lambda x: x if any(st["name"] == "Mail" for st in x["stages"]) else None)(state()), 5, "hot reload")
    check(s is not None, "hot reload: the named stage Mail appears")
    with open(os.path.join(HOME, "config.toml"), "a") as f:
        f.write("\n[strip]\norder = \"sideways\"\n")
    s = wait_until(lambda: (lambda x: x if x["config_error"] else None)(state()), 5, "the config error")
    check(s is not None and "sideways" in (s["config_error"] or ""), "a bad config shows its error")
    check(s is not None and any(st["name"] == "Mail" for st in s["stages"]), "and the last good config stays")
    with open(os.path.join(HOME, "config.toml"), "w") as f:
        f.write(CONFIG_M4)
    r = ctl("reload")
    check(r.get("ok"), "reload over ctl clears the error")
    do("pin", t["Fixture 4"]["id"])
    stop_app()
    all_onscreen("m4 quit")


def phase_m6():
    say("\n== milestone 6: persistence ==")
    stop_app()
    stop_fixture()
    for f in ("state.json", "config.toml"):
        p = os.path.join(HOME, f)
        if os.path.exists(p):
            os.remove(p)
    start_fixture(4)
    s = start_app(4)
    if not check(s is not None, "app up"):
        return
    t = by_title(s)
    do("send", 2, t["Fixture 3"]["id"])
    do("send", 2, t["Fixture 4"]["id"])
    do("send", 3, t["Fixture 2"]["id"])
    do("switch", 2)
    want = stage_titles(state())
    want_active = sorted(want[state()["active"]])
    say(f"  stages before: {want}, active {want_active}")
    time.sleep(1.0)
    stop_app()
    all_onscreen("quit")
    check(os.path.exists(os.path.join(HOME, "state.json")), "state.json written")

    s = start_app(4, fresh_log=False)
    got = stage_titles(s)
    check(sorted(map(sorted, got.values())) == sorted(map(sorted, want.values())), f"relaunch: stages came back ({got})")
    check(sorted(got[s["active"]]) == want_active, "relaunch: the active stage came back")
    consistent("relaunch")
    stop_app()
    all_onscreen("quit again")

    say("-- the fixture relaunches too (new window IDs; matched by title)")
    stop_fixture()
    start_fixture(4)
    s = start_app(4, fresh_log=False)
    got = stage_titles(s)
    check(sorted(map(sorted, got.values())) == sorted(map(sorted, want.values())), f"fixture relaunch: stages came back ({got})")
    check(sorted(got[s["active"]]) == want_active, "fixture relaunch: the active stage came back")
    consistent("fixture relaunch")
    stop_app()
    all_onscreen("final quit")


def phase_strip():
    say("\n== the strip end to end: a card click switches, a shift-click merges ==")
    stop_app()
    stop_fixture()
    p = os.path.join(HOME, "state.json")
    if os.path.exists(p):
        os.remove(p)
    start_fixture(4)
    s = start_app(4, extra_env={"STAGE_MANAGER_TEST": "1"})
    if not check(s is not None, "app up"):
        return
    t = by_title(s)
    # Stage 1 keeps only Fixture 3 (clear of the strip area, so the strip stays shown);
    # the others go to stage 2, which is the single card.
    for n in (1, 2, 4):
        do("send", 2, t[f"Fixture {n}"]["id"])
    s = state()
    check(s["active"] == 1 and s["strip"] == [2], "one card (stage 2) in the strip")
    vis = s["display"]["visible"]
    x, y = vis["x"] + 70, vis["y"] + vis["h"] / 2
    time.sleep(1.5)
    r = ctl("debug-click", x, y)
    check(r.get("ok"), f"clicked the card at {x:.0f},{y:.0f} ({r.get('error', 'ok')})")
    s = wait_until(lambda: (lambda x: x if x["active"] == 2 else None)(state()), 3, "the card click to switch")
    check(s is not None, "a card click switches to its stage")
    consistent("after the card click")
    do("switch", 1)
    time.sleep(1.5)
    r = ctl("debug-click", x, y, "shift")
    check(r.get("ok"), f"shift-clicked the card ({r.get('error', 'ok')})")
    s = wait_until(lambda: (lambda x: x if len(x["stages"]) == 1 else None)(state()), 3, "the shift-click to merge")
    check(s is not None and len(stage_titles(s)[1]) == 4, "a shift-click brings the card's windows into the active stage")
    consistent("after the shift-click")
    stop_app()
    all_onscreen("strip quit")


def frontmost_bundle():
    r = subprocess.run(["lsappinfo", "info", "-only", "bundleid", "front"], capture_output=True, text=True)
    out = r.stdout.strip()
    return out.split("=", 1)[1].strip().strip('"') if "=" in out else None


def main():
    phases = sys.argv[1:] or ["m3", "safety", "m4", "m6", "strip"]
    if not os.path.exists(BIN) or not os.path.exists(FIX_APP):
        say("build first: scripts/test-stages.sh builds the app and the fixture")
        return 2
    if pids_of("Stage Manager.app/Contents/MacOS/stage-manager$") and not app_pid():
        say("note: another Stage Manager is running (not ours); it uses another home and is left alone")
    say(f"home: {HOME}")
    front = frontmost_bundle()
    try:
        for p in phases:
            {"m3": phase_m3, "safety": phase_safety, "m4": phase_m4, "m6": phase_m6, "strip": phase_strip}[p]()
    finally:
        stop_app()
        if pids_of(FIX_EXE):
            all_onscreen("cleanup")
        stop_fixture()
        for g in guardian_pids():
            pass  # guardians exit with their app; nothing to kill
        if front and front != FIX_ID:
            subprocess.run(["open", "-b", front], capture_output=True)
    say(f"\n{len(passed)} passed, {len(failed)} failed")
    for f in failed:
        say(f"  FAIL {f}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
