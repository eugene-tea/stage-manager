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
only when its own strip window is topmost at that point), native (Apple's defaults with three
more fixture apps: first start, new windows, Shift sugar, card drag and window drag; every
synthetic click and drag is posted only where our own or a fixture's windows are topmost
along the whole path, and the pointer is put back), visible (by default, Apple's: no window
is moved or resized, ever; with `[windows] fit = "narrow"` windows kept clear of the strip on
first start, switch and creation, a too-wide one narrowed and given back its exact frame on
quit; with `fit = "move"` plus the launch reveal and the edge tab turned on, those; a
menu-path quit right after a first start with many windows), parity (the generated config has
no live values and registers no hotkey; Cmd-M sends a window to the strip; a click on a
card's app icon brings every window of that app), motion (the strip below windows and lifted
while a window is dragged over it, with the dragged window's card in the motion overlay; a
window dropped above the first card takes that slot; a card dragged to another slot keeps it
across a relaunch; a card click animates and every frame is exact afterwards;
`level = "above_windows"` keeps the strip on top).

Tests that need an extension turn it on in their own config.toml.

No phase runs while the owner's installed app is up, and only our own app's guardian is
ever signalled.

SM_FIXTURE_ID sets the main fixture's bundle ID (default dev.eugene.stagemanager.fixture.stages);
the native phase's apps are <id>.a, <id>.b and <id>.c.
"""

import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# The fixed dev copy (scripts/dev-app.sh): macOS asks for screen access once per app path.
APP = os.environ.get("SM_DEV_APP") or os.path.expanduser(
    "~/Library/Application Support/Stage Manager Dev/Stage Manager.app")
BIN = os.path.join(APP, "Contents", "MacOS", "stage-manager")
FIX_ID = os.environ.get("SM_FIXTURE_ID") or "dev.eugene.stagemanager.fixture.stages"
FIX_NAME = "SM Fixture " + FIX_ID.rsplit(".", 1)[-1].capitalize()
FIX_APP = os.path.join(ROOT, "target", FIX_NAME + ".app")
FIX_EXE = FIX_NAME + ".app/Contents/MacOS/sm-fixture"
# The native phase's three apps: (bundle ID, app path, process pattern, title prefix).
EXTRA = {
    k: (f"{FIX_ID}.{k}", os.path.join(ROOT, "target", f"{FIX_NAME} {k.upper()}.app"),
        f"{FIX_NAME} {k.upper()}.app/Contents/MacOS/sm-fixture", title)
    for k, title in (("a", "Alpha"), ("b", "Bravo"), ("c", "Charlie"))
}
ALL_IDS = [FIX_ID] + [e[0] for e in EXTRA.values()]
PARK_MARGIN = 64.0

HOME = os.environ.get("TEST_HOME") or tempfile.mkdtemp(prefix="sm-test.", dir="/tmp")
os.makedirs(HOME, exist_ok=True)
ONLY = ",".join(ALL_IDS)
ENV = dict(os.environ, STAGE_MANAGER_HOME=HOME, STAGE_MANAGER_ONLY=ONLY)
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


def probe(bundle=FIX_ID):
    r = run([BIN, "probe-windows", bundle], timeout=10)
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


def fixture_windows(bundles=None):
    """Fixture windows on layer 0 as the window server sees them, with the display."""
    out, display = [], None
    for b in bundles or ALL_IDS:
        p = probe(b)
        display = p["display"]
        out += [w for w in p["windows"] if w["layer"] == 0]
    return out, display


def start_fixture(n, *extra):
    stop_fixture()
    subprocess.run(["open", "-n", "-g", FIX_APP, "--args", "--windows", str(n), "--background", *extra], check=True)
    want = n + (1 if "--panel" in extra else 0)
    return wait_until(lambda: len(fixture_windows([FIX_ID])[0]) >= want, 15, f"{want} fixture windows")


def stop_fixture():
    for pid in pids_of(FIX_EXE):
        os.kill(pid, signal.SIGTERM)
    wait_until(lambda: not pids_of(FIX_EXE), 10, "the fixture to quit")


def app_pid():
    r = ctl("ping")
    return r.get("pid") if r.get("ok") else None


def guardian_pids(app):
    """The guardian of our own app (pid `app`) only: never another Stage Manager's."""
    return pids_of(f"stage-manager --guardian {app}$") if app else []


def owner_app_running():
    """Any Stage Manager that is not this run's own app (the owner's, from ~/Applications,
    a repo's target/ or anywhere else): no live phase runs while it is up."""
    ours = app_pid()
    return any(p != ours for p in pids_of("Stage Manager.app/Contents/MacOS/stage-manager$"))


def archive_log():
    """Keeps the last app run's log as stage-manager.run-NN.log (renamed, not removed: its
    guardian keeps writing into the same file), so a run leaves the whole story."""
    if not os.path.exists(LOG):
        return None
    n = 1 + max([int(f[len("stage-manager.run-"):-4]) for f in os.listdir(HOME)
                 if f.startswith("stage-manager.run-") and f.endswith(".log") and f[18:-4].isdigit()] or [0])
    path = os.path.join(HOME, f"stage-manager.run-{n:02d}.log")
    os.rename(LOG, path)
    return path


def start_app(expect_windows, extra_env=None, fresh_log=True):
    if app_pid():
        stop_app()
    if fresh_log:
        archive_log()
    args = ["open", "-n", APP]
    env = {
        "STAGE_MANAGER_HOME": HOME,
        "STAGE_MANAGER_ONLY": ONLY,
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
    with open(os.path.join(HOME, "config.toml"), "w") as f:
        f.write("[hotkeys]\nenabled = true\n")
    start_fixture(4, "--panel")
    s = start_app(5)
    if not check(s is not None, "app manages the 4 fixture windows and the panel"):
        return
    t = by_title(s)
    check(len(s["stages"]) == 1 and s["active"] == 1, "one active stage at launch")
    panel = t.get("Fixture Panel")
    check(panel is not None and panel["role"] == "child" and panel["parent"] is not None, "the panel is a child with a parent")
    check("hotkeys registered active=21" in log_text(), "21 hotkeys registered (turned on in this test's config)")
    w = {n: t[f"Fixture {n}"]["id"] for n in range(1, 5)}

    do("send", 2, w[3])
    do("send", 3, w[4])
    s = state()
    # A stage lists its windows front to back: the first start took the window server's order.
    front_first = z_titles(["Fixture 1", "Fixture 2"])
    check(stage_titles(s) == {1: front_first, 2: ["Fixture 3"], 3: ["Fixture 4"]},
          f"send builds stages 2 and 3 ({stage_titles(s)}, stage 1 front to back {front_first})")
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
    guards = guardian_pids(pid)
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

    drill_away()

    say("-- drill: kill -9 of the app and the guardian (the next launch restores)")
    start_app(1, extra_env={"STAGE_MANAGER_TEST": "1"})
    ensure_something_parked()
    time.sleep(1.0)  # let state.json catch up (debounced)
    pid = app_pid()
    for g in guardian_pids(pid):
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


def drill_away():
    """kill -9 while a parked window's Desktop is not shown, emulated by the fixture: on
    SIGWINCH it orders its parked windows out (the window server lists them off screen,
    Accessibility no longer does), on SIGINFO back in (their Desktop shows)."""
    say("-- drill: kill -9 with a window parked on a Desktop not shown (emulated): the guardian waits")
    start_app(1, extra_env={"STAGE_MANAGER_TEST": "1"})
    ensure_something_parked()
    time.sleep(1.0)
    fixture = pids_of(FIX_EXE)
    parked_ids = [w["id"] for w in state()["windows"] if w["parked"] and w["role"] == "standard"]
    if not check(bool(fixture) and bool(parked_ids), "drill setup: a parked fixture window"):
        return
    pid = app_pid()
    guards = guardian_pids(pid)

    def listed(ids):
        wins, d = fixture_windows([FIX_ID])
        return [w for w in wins if w["id"] in ids]

    # Frozen first, so the app never hears of the window leaving the screen (Accessibility
    # reports an ordered-out window as destroyed; a real Desktop switch does not).
    os.kill(pid, signal.SIGSTOP)
    os.kill(fixture[0], signal.SIGWINCH)
    away = wait_until(lambda: (lambda ws: ws if ws and not any(w["onscreen"] for w in ws) else None)(listed(parked_ids)), 3,
                      "the parked windows to leave the screen")
    os.kill(pid, signal.SIGKILL)
    wait_gone(pid)
    if not check(away is not None, "the parked windows are off screen (their Desktop not shown)"):
        os.kill(fixture[0], signal.SIGINFO)
        return
    time.sleep(3.0)
    _, d = fixture_windows([FIX_ID])
    check(bool(guards) and bool(pids_of(f"stage-manager --guardian {pid}$")),
          "the guardian keeps waiting while a registered window is parked out of reach")
    check(all(is_parked(w["bounds"], d) for w in listed(parked_ids)), "the window is still parked (nothing could reach it)")
    check("guardian: waiting for" in log_text() and "registered)\"] shown_now=[]" in log_text(),
          "the guardian's log says what it waits for (the registered window, its Desktop not shown)")
    os.kill(fixture[0], signal.SIGINFO)
    back = wait_until(lambda: (lambda ws: ws if ws and not any(is_parked(w["bounds"], d) for w in ws) else None)(listed(parked_ids)), 5,
                      "the window to come back")
    check(back is not None, "the window comes back as soon as its Desktop shows")
    check("guardian: its Desktop shows; window brought back" in log_text(), "the log says the guardian brought it back")
    for g in guards:
        wait_gone(g, 5)
    check(not pids_of(f"stage-manager --guardian {pid}$"), "the guardian ends once nothing is left parked")
    all_onscreen("kill -9 with a window away")


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
    # Stage 1 lists its windows front to back, as the window server orders them.
    want = {1: z_titles(["Fixture 1", "Fixture 2"]), 2: ["Fixture 3"]}
    check(stage_titles(s) == want, f"Fixture 3 opened a new stage ({stage_titles(s)}, expected {want})")
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


# ----- native: Apple's defaults, Shift sugar, drag and drop ------------------------------


def start_extra(k, n):
    bundle, path, pattern, title = EXTRA[k]
    stop_extra(k)
    subprocess.run(["open", "-n", "-g", path, "--args", "--windows", str(n), "--title", title, "--background"], check=True)
    return wait_until(lambda: len(fixture_windows([bundle])[0]) >= n, 15, f"{n} {title} windows")


def stop_extra(k):
    pattern = EXTRA[k][2]
    for pid in pids_of(pattern):
        os.kill(pid, signal.SIGTERM)
    wait_until(lambda: not pids_of(pattern), 10, f"{EXTRA[k][3]} to quit")


def activate(k):
    """Brings fixture app `k` (the running instance, from this tree's target/) to the front.
    By path, never `open -b`: every worktree registers its own copy under the same bundle ID,
    and `open -b` launched LaunchServices' preferred copy (often another tree's) as a second
    instance with default windows, which then sat on the screen as a stray."""
    subprocess.run(["open", EXTRA[k][1]], check=True)


def extra_pid(k):
    p = pids_of(EXTRA[k][2])
    return p[0] if p else None


def stop_all_fixtures():
    stop_fixture()
    for k in EXTRA:
        stop_extra(k)


def stage_sets(s):
    """Stage number -> set of window titles."""
    return {n: set(t) for n, t in stage_titles(s).items()}


def active_titles(s):
    return set(stage_titles(s).get(s["active"], []))


def wait_state(pred, what, timeout=6):
    return wait_until(lambda: (lambda x: x if pred(x) else None)(state()), timeout, what)


def topmost_point(prefix):
    """The center of the frontmost shown window whose title starts with `prefix`, brought to
    the front first (in place) so no other app's window covers it: when a fixture app quits,
    macOS activates another app, whose windows may cover the fixtures."""
    s = state()
    shown = [w for w in s["windows"] if w["title"].startswith(prefix) and not w["parked"]]
    if shown:
        r = shown[0]["restore"]
        ctl("debug-frame", shown[0]["id"], r["x"], r["y"], r["w"], r["h"])
        time.sleep(0.9)  # past the echo window of that raise
    s = state()
    ids = {w["id"] for w in s["windows"] if w["title"].startswith(prefix) and not w["parked"]}
    wins, _ = fixture_windows()
    mine = sorted((w for w in wins if w["id"] in ids and w["z"] is not None), key=lambda w: w["z"])
    if not mine:
        return None
    b = mine[0]["bounds"]
    return b["x"] + b["w"] / 2, b["y"] + b["h"] / 2


def phase_native():
    say("\n== native: first start, new windows, Shift sugar, card drag, window drag ==")
    stop_app()
    stop_all_fixtures()
    for f in ("state.json", "config.toml"):
        p = os.path.join(HOME, f)
        if os.path.exists(p):
            os.remove(p)
    start_extra("a", 2)
    start_extra("b", 1)
    start_extra("c", 1)
    activate("b")
    time.sleep(1.0)
    # Recency comes from the window server's z-order at launch (orderBack and an `open` from
    # a background shell do not order the apps predictably): each app's frontmost window.
    wins, _ = fixture_windows([EXTRA["a"][0], EXTRA["c"][0]])
    pid_of = {w["pid"]: k for k in ("a", "c") for w in fixture_windows([EXTRA[k][0]])[0]}
    best = {}
    for w in wins:
        if w["z"] is not None:
            k = pid_of[w["pid"]]
            best[k] = min(best.get(k, 1 << 30), w["z"])
    expect_strip = [{"a": {"Alpha 1", "Alpha 2"}, "c": {"Charlie 1"}}[k] for k in sorted(best, key=best.get)]
    s = start_app(4, extra_env={"STAGE_MANAGER_TEST": "1"})
    if not check(s is not None, "app up with three fixture apps"):
        return
    sets = stage_sets(s)
    check(sorted(map(sorted, sets.values())) == [["Alpha 1", "Alpha 2"], ["Bravo 1"], ["Charlie 1"]],
          f"first start: one stage per app ({stage_titles(s)})")
    check(active_titles(s) == {"Bravo 1"}, "first start: the frontmost app's stage is active")
    strip = [sets[n] for n in s["strip"]]
    check(strip == expect_strip, f"first start: the strip in recency order by z ({strip}, expected {expect_strip})")
    consistent("first start")

    say("-- a newly opened app gets a new stage")
    stop_extra("c")
    wait_state(lambda x: len(x["stages"]) == 2, "Charlie's stage to go")
    start_extra("c", 1)
    s = wait_state(lambda x: active_titles(x) == {"Charlie 1"}, "Charlie's new stage")
    check(s is not None and len(s["stages"]) == 3, "a new app's window opens a new active stage")
    consistent("new app")

    say("-- a new window of a parked app's stage brings it forward")
    os.kill(extra_pid("a"), signal.SIGUSR1)
    s = wait_state(lambda x: active_titles(x) == {"Alpha 1", "Alpha 2", "Alpha 3"}, "Alpha 3 in Alpha's stage")
    check(s is not None and len(s["stages"]) == 3, "the new window joins its app's stage, which becomes active")
    consistent("new window of a parked app")

    say("-- a new window of the active app joins it")
    os.kill(extra_pid("a"), signal.SIGUSR1)
    s = wait_state(lambda x: "Alpha 4" in active_titles(x), "Alpha 4 in the active stage")
    check(s is not None and len(s["stages"]) == 3, "the new window joins the active stage")
    consistent("new window of the active app")

    say("-- Shift held while opening an app: its window joins the current stage")
    stop_extra("c")
    wait_state(lambda x: len(x["stages"]) == 2, "Charlie's stage to go")
    time.sleep(0.8)
    pt = topmost_point("Alpha")
    r = ctl("debug-click", pt[0], pt[1], "shift") if pt else {"ok": False, "error": "no Alpha window"}
    check(r.get("ok"), f"shift-clicked an Alpha window ({r.get('error', 'ok')})")
    start_extra("c", 1)
    s = wait_state(lambda x: "Charlie 1" in by_title(x), "Charlie 1 managed")
    check(s is not None and "Charlie 1" in active_titles(s) and len(s["stages"]) == 2,
          f"a Shift launch joins the current stage ({stage_titles(s) if s else None})")
    consistent("Shift launch")

    say("-- Shift held on an activation: the app's windows come here")
    time.sleep(0.8)
    pt = topmost_point("Alpha")
    r = ctl("debug-click", pt[0], pt[1], "shift") if pt else {"ok": False, "error": "no Alpha window"}
    check(r.get("ok"), f"shift-clicked an Alpha window ({r.get('error', 'ok')})")
    activate("b")
    s = wait_state(lambda x: "Bravo 1" in active_titles(x), "Bravo here")
    check(s is not None and len(s["stages"]) == 1, "a Shift activation brings Bravo into the current stage")
    consistent("Shift activation")

    say("-- drag a card into the main area")
    time.sleep(0.8)  # past the activation echo window
    s = state()
    t = by_title(s)
    do("new", t["Bravo 1"]["id"])
    for name in ("Alpha 2", "Alpha 3", "Alpha 4", "Charlie 1"):
        do("send", 2, t[name]["id"])
    s = state()
    check(s["strip"] == [2] and active_titles(s) == {"Alpha 1"}, f"one card, Alpha 1 alone ({stage_titles(s)})")
    vis = s["display"]["visible"]
    cx, cy = vis["x"] + 70, vis["y"] + vis["h"] / 2
    # Right beside the strip column (it scales with the display), so the drag path is ours.
    big = (vis["x"] + s["strip_width"], cy - 260, 1000, 520)
    r = ctl("debug-frame", t["Alpha 1"]["id"], *big)
    check(r.get("ok"), f"Alpha 1 covers the drag path ({r.get('error', 'ok')})")
    time.sleep(1.5)
    front = state()["stages"][1]["windows"][0]
    drop = (vis["x"] + 700, cy)
    r = ctl("debug-drag", cx, cy, cx + 60, cy, *drop)
    check(r.get("ok"), f"dragged the card to {drop[0]:.0f},{drop[1]:.0f} ({r.get('error', 'ok')}{whose(r.get('error'))})")
    s = wait_state(lambda x: len(x["stages"]) == 1, "the card's windows to join")
    check(s is not None and len(active_titles(s)) == 6, "the dropped card's windows join the current stage")
    if s:
        f = next(w for w in s["windows"] if w["id"] == front)["restore"]
        inside = f["x"] <= drop[0] <= f["x"] + f["w"] and f["y"] <= drop[1] <= f["y"] + 30
        check(inside, f"its front window lands with its title bar under the drop point ({fmt(f)})")
    consistent("card drop")

    say("-- drag a window onto the strip")
    s = state()
    t = by_title(s)
    do("new", t["Alpha 2"]["id"])
    for name in ("Alpha 3", "Alpha 4", "Charlie 1", "Bravo 1"):
        do("send", 2, t[name]["id"])
    r = ctl("debug-frame", t["Alpha 1"]["id"], *big)
    time.sleep(1.5)
    s = state()
    check(active_titles(s) == {"Alpha 1"}, f"Alpha 1 alone again ({stage_titles(s)})")
    pre = by_title(s)["Alpha 1"]["restore"]
    grab = (pre["x"] + 500, pre["y"] + 12)
    r = ctl("debug-drag", *grab, grab[0] - 60, grab[1], vis["x"] + 60, grab[1])
    check(r.get("ok"), f"dragged Alpha 1 by its title bar onto the strip ({r.get('error', 'ok')}{whose(r.get('error'))})")
    s = wait_state(lambda x: by_title(x)["Alpha 1"]["parked"], "Alpha 1 to become its own stage")
    if check(s is not None, "a window dropped on the strip gets a stage of its own"):
        a1 = by_title(s)["Alpha 1"]
        check(a1["home"] != s["active"] and stage_titles(s)[a1["home"]] == ["Alpha 1"], "alone in its new stage")
        check(close(a1["restore"], pre), f"its saved frame is the pre-drag frame ({fmt(a1['restore'])})")
        check(a1["home"] in s["strip"], "its card is in the strip")
    consistent("window drop")
    stop_app()
    all_onscreen("native quit")
    stop_all_fixtures()


# ----- motion: strip level, drops into a slot, card reorder, switch animation -----------


OVERLAY_LAYER = 25  # NSStatusWindowLevel: the click-through motion overlay


def our_windows(pid=None):
    """Our own app's onscreen windows (layer, bounds), by our pid (asked when not given)."""
    pid = pid or app_pid()
    if not pid:
        return []
    r = run([BIN, "probe-windows", "dev.eugene.stagemanager"], timeout=10)
    try:
        wins = json.loads(r.stdout)["windows"]
    except (json.JSONDecodeError, KeyError):
        return []
    return [w for w in wins if w["pid"] == pid and w["onscreen"]]


def strip_panel(wins=None):
    """The strip panel: our tall window at the left edge (not the slim tab, not the overlay)."""
    tall = [w for w in (wins if wins is not None else our_windows())
            if w["bounds"]["w"] > 12 and w["bounds"]["h"] > 300 and w["layer"] != OVERLAY_LAYER]
    return tall[0] if tall else None


def drag_sampled(points, what, wait=5.0):
    """A synthetic drag (ctl debug-drag) along `points`, sampling our windows while it runs
    (see `sample_during`). The app refuses a drag, posting nothing, while anything but our
    own or a fixture's window is topmost on the path; a refused drag is tried again until
    `wait` seconds have passed (a window of the phase before still going away). Returns the
    last reply and the samples of the drag that ran."""
    words = [str(round(c, 1)) for p in points for c in p]
    end = time.time() + wait
    say(f"  dragging {what}")
    while True:
        proc = subprocess.Popen([BIN, "ctl", "debug-drag", *words], env=ENV, stdout=subprocess.PIPE, text=True)
        seen = sample_during(proc)
        out, _ = proc.communicate(timeout=20)
        refused = "not ours or a fixture's" in out or "no window at" in out
        if not refused or time.time() >= end:
            return out, seen
        say(f"  the path is not clear yet ({out.strip()[:120]}{whose(out)}); trying again")
        time.sleep(0.5)


def whose(reply):
    """For a ctl reply refusing a click or drag over another process's window: which program
    that is (` = <name>`), while it still runs; empty otherwise."""
    m = re.search(r"not ours or a fixture's \(pid (\d+)\)", reply or "")
    if not m:
        return ""
    r = subprocess.run(["ps", "-o", "comm=", "-p", m.group(1)], capture_output=True, text=True)
    return f" = {r.stdout.strip() or 'gone'}"


def sample_during(proc):
    """Our onscreen windows, sampled back to back for as long as the drag `proc` runs (the
    strip is lifted only while the pointer is over it, under a second; a single timed sample
    missed it). Every sample is a list as `our_windows` gives."""
    seen, pid = [], app_pid()
    while proc.poll() is None:
        seen.append(our_windows(pid))
    return seen


CONFIG_BACKDROP = f"""
[[rule]]
app = "{FIX_ID}.c"
action = "ignore"
"""


def start_backdrop(s):
    """An unmanaged fixture window (Charlie, an `ignore` rule) in front of every other app's
    window, covering the strip column and the main area's left part: whatever else is on the
    screen (an editor over the strip column, say), the synthetic drags pass over our windows
    and the fixtures only. The fixtures are raised in front of it afterwards."""
    vis = s["display"]["visible"]
    start_extra_frames("c", 1, f"0,0,{s['strip_width'] + 900:.0f},{vis['h']:.0f}")
    activate("c")
    time.sleep(0.8)


def reveal_strip(what):
    """Reaches the strip the way a person does when a window covers its column (his editor
    on the 5K display): an active window over the strip column hides it (Apple's auto-hide);
    the pointer resting at the left edge brings it back above every window."""
    s = state()
    vis = s["display"]["visible"]
    edge = vis["x"] + s["strip_width"]
    active = [w for w in s["windows"] if w["home"] == s["active"] and w["role"] == "standard" and not w["parked"]]
    if active and not any(w["restore"]["x"] < edge for w in active):
        r = active[0]["restore"]
        ctl("debug-frame", active[0]["id"], edge - 100, r["y"], r["w"], r["h"])
        time.sleep(0.9)  # past the echo window; the strip hides
    r = ctl("debug-hover", vis["x"] + 1, vis["y"] + vis["h"] / 2, 900)
    panel = wait_until(lambda: (lambda p: p if p and p["layer"] == 3 else None)(strip_panel()), 3, "the strip to be revealed")
    cards = state().get("strip_cards", [])
    return check(r.get("ok") and panel is not None and bool(cards),
                 f"{what}: the pointer resting at the left edge reveals the covered strip above windows "
                 f"({r.get('error', 'ok')}, layer {panel and panel['layer']}, {len(cards)} cards)")


def phase_motion():
    say("\n== motion: strip level, a window dropped into a slot, card reorder, switch animation ==")
    stop_app()
    stop_all_fixtures()
    write_config(CONFIG_BACKDROP)
    start_fixture(5)
    s = start_app(5, extra_env={"STAGE_MANAGER_TEST": "1"})
    if not check(s is not None, "app up with five fixture windows"):
        return
    check(s.get("strip_level") == "BelowWindows" and s.get("animation") is True,
          f"defaults: the strip below windows, switches animated ({s.get('strip_level')}, {s.get('animation')})")
    start_backdrop(s)
    t = by_title(s)
    vis = s["display"]["visible"]
    left = vis["x"] + s["strip_width"] + 60
    # Fixtures 3 and 5 stay, clear of the strip; 1, 2 and 4 get a stage each (three cards).
    ctl("debug-frame", t["Fixture 3"]["id"], left, vis["y"] + 60, 640, 420)
    ctl("debug-frame", t["Fixture 5"]["id"], left + 200, vis["y"] + 260, 640, 420)
    for n in (1, 2, 4):
        do("new", t[f"Fixture {n}"]["id"])
    time.sleep(1.5)
    s = state()
    check(len(s["strip"]) == 3 and len(s.get("strip_cards", [])) == 3, f"three cards ({s['strip']}, {len(s.get('strip_cards', []))} drawn)")
    panel = strip_panel()
    check(panel is not None and panel["layer"] == -1, f"the strip sits below windows (layer {panel and panel['layer']})")

    say("-- a window dragged over the strip lifts it and shows its card; dropped above the first card it takes that slot")
    s = state()
    first = s["strip_cards"][0]["frame"] if s.get("strip_cards") else None
    pre = by_title(s)["Fixture 5"]["restore"]
    grab = (pre["x"] + 300, pre["y"] + 12)
    target = (vis["x"] + 60, (first["y"] - 6) if first else vis["y"] + 40)
    out, seen = drag_sampled([grab, (grab[0] - 60, grab[1]), target], "Fixture 5 onto the strip")
    layers = sorted({p["layer"] for p in (strip_panel(m) for m in seen) if p})
    lifted = [m for m in seen if (lambda p: p is not None and p["layer"] == 3)(strip_panel(m))]
    check(bool(lifted), f"the strip is above windows while a window is over it (layers seen {layers} in {len(seen)} samples)")
    check(any(w["layer"] == OVERLAY_LAYER for m in lifted for w in m),
          "the dragged window's card shows in the motion overlay (while the strip is lifted)")
    check('"ok": true' in out or '"ok":true' in out, f"the drag ran ({out.strip()[:120]}{whose(out)})")
    s = wait_state(lambda x: by_title(x)["Fixture 5"]["parked"], "Fixture 5 to become its own stage")
    if check(s is not None, "a window dropped on the strip gets a stage of its own"):
        f5 = by_title(s)["Fixture 5"]
        check(s["strip"][:1] == [f5["home"]], f"its card took the slot above the first card ({s['strip']}, its stage {f5['home']})")
        check(close(f5["restore"], pre), f"its saved frame is the pre-drag frame ({fmt(f5['restore'])})")
    time.sleep(1.0)
    panel = strip_panel()
    check(panel is not None and panel["layer"] == -1, "the strip is back below windows")
    consistent("window drop into a slot")

    say("-- a card dragged to another slot keeps it, also after a relaunch")
    time.sleep(1.0)
    check(len(state().get("strip_cards", [])) == 4, f"four cards drawn ({len(state().get('strip_cards', []))})")
    reveal_strip("reorder")
    s = state()
    cards = s.get("strip_cards", [])
    if len(cards) == 4:
        last, top = cards[-1], cards[0]["frame"]
        b = last["frame"]
        start = (vis["x"] + 70, b["y"] + b["h"] / 2)
        out, _ = drag_sampled([start, (start[0], start[1] - 30), (vis["x"] + 70, top["y"] + 4)], "the last card to the top")
        check('"ok": true' in out or '"ok":true' in out, f"the drag ran ({out.strip()[:120]}{whose(out)})")
        s = wait_state(lambda x: x["strip"][:1] == [last["number"]], "the card to move to the top")
        check(s is not None, f"the dragged card is first ({state()['strip']})")
        order = state()["strip"]
        stop_app()
        s = start_app(5, extra_env={"STAGE_MANAGER_TEST": "1"}, fresh_log=False)
        check(s is not None and s["strip"] == order, f"the order survives a relaunch ({s and s['strip']} vs {order})")

    say("-- a card click animates; every window ends at its exact frame")
    time.sleep(1.5)
    reveal_strip("card click")
    s = state()
    cards = s.get("strip_cards", [])
    if check(bool(cards), "cards drawn"):
        c = cards[0]
        b = c["frame"]
        r = ctl("debug-click", b["x"] + b["w"] / 2, b["y"] + b["h"] / 2)
        check(r.get("ok"), f"clicked a card ({r.get('error', 'ok')})")
        check(any(w["layer"] == OVERLAY_LAYER for w in our_windows()), "snapshots move in the overlay")
        s = wait_state(lambda x: x["active"] == c["number"], "the click to switch")
        check(s is not None, "the click switched")
        time.sleep(1.0)
        check(not any(w["layer"] == OVERLAY_LAYER for w in our_windows()), "the overlay is gone once the motion ends")
        consistent("animated switch")

    say("-- level = \"above_windows\" keeps the strip on top")
    stop_app()
    with open(os.path.join(HOME, "config.toml"), "w") as f:
        f.write("[strip]\nlevel = \"above_windows\"\n" + CONFIG_BACKDROP)
    s = start_app(5, extra_env={"STAGE_MANAGER_TEST": "1"}, fresh_log=False)
    if check(s is not None, "app up with level = above_windows"):
        time.sleep(1.5)
        panel = strip_panel()
        check(panel is not None and panel["layer"] == 3, f"the strip is above windows (layer {panel and panel['layer']})")
    stop_app()
    all_onscreen("motion quit")
    write_config(None)
    stop_all_fixtures()


# ----- visible: the stage area beside the strip, the edge tab, quit right after a start --


SCRATCH = os.environ.get("SM_SCREENSHOTS") or HOME


def our_layer3_windows():
    """Our own app's windows at layer 3 (the edge tab; the strip when lifted), by our pid."""
    pid = app_pid()
    if not pid:
        return []
    r = run([BIN, "probe-windows", "dev.eugene.stagemanager"], timeout=10)
    try:
        wins = json.loads(r.stdout)["windows"]
    except (json.JSONDecodeError, KeyError):
        return []
    return [w for w in wins if w["pid"] == pid and w["layer"] == 3 and w["onscreen"]]


def edge_tab():
    """The edge tab: our slim layer-3 window (the strip panel is far wider)."""
    tabs = [w for w in our_layer3_windows() if w["bounds"]["w"] <= 12]
    return tabs[0] if tabs else None


def screenshot(name, s):
    vis = s["display"]["visible"]
    path = os.path.join(SCRATCH, name)
    r = subprocess.run(["screencapture", "-x", "-R", f"{vis['x']:.0f},{vis['y']:.0f},260,{vis['h']:.0f}", path],
                       capture_output=True, text=True)
    say(f"  screenshot {path}" if r.returncode == 0 and os.path.exists(path) else f"  screenshot failed: {r.stderr.strip()}")


def live_frame(title):
    return frames_of([title])[title]


def write_config(text):
    for f in ("state.json", "config.toml"):
        p = os.path.join(HOME, f)
        if os.path.exists(p):
            os.remove(p)
    if text:
        with open(os.path.join(HOME, "config.toml"), "w") as f:
            f.write(text)


def phase_visible():
    say("\n== visible: Apple's exact frames by default; with fit, windows clear of the strip; the edge tab; quit ==")
    stop_app()
    stop_all_fixtures()
    write_config(None)
    frames = {"a": "20,60,700,400;10,120,-40,500", "b": "30,90,600,380"}
    start_extra_frames("a", 2, frames["a"])
    start_extra_frames("b", 1, frames["b"])
    activate("a")
    time.sleep(1.0)
    wins, _ = fixture_windows([EXTRA["a"][0], EXTRA["b"][0]])
    s = start_app(3, extra_env={"STAGE_MANAGER_TEST": "1"})
    if not check(s is not None, "defaults: app up with Alpha and Bravo"):
        return
    check(s["fit"] == "off" and s["stage_inset"] == 0, f"defaults: no fitting (fit={s['fit']}, inset {s['stage_inset']})")
    t = by_title(s)
    orig = {n: t[n]["restore"] for n in ("Alpha 1", "Alpha 2", "Bravo 1")}
    check(all(t[n]["original"] is None for n in orig), "defaults: no window was moved or resized at first start")
    consistent("defaults first start")
    st = {st["number"]: st["windows"] for st in s["stages"]}
    alpha = next(n for n, ws in st.items() if t["Alpha 1"]["id"] in ws)
    bravo = next(n for n, ws in st.items() if t["Bravo 1"]["id"] in ws)
    for n in (bravo, alpha, bravo, alpha):
        do("switch", n)
    s = state()
    same = all(close(by_title(s)[n]["restore"], orig[n]) for n in orig)
    check(same, "defaults: switching restores every window at exactly its own frame, even over the strip")
    consistent("defaults switch")
    time.sleep(1.0)
    check(edge_tab() is None, "defaults: no edge tab while a window covers the strip")
    check("showing it for a moment" not in log_text(), "defaults: no launch reveal")
    check("hotkeys registered active=0" in log_text(), "defaults: no global hotkey registered")
    screenshot("strip-defaults.png", s)
    pid = app_pid()
    ctl("quit")
    wait_gone(pid)
    a2 = live_frame_probe("a", "Alpha 2")
    check(a2 is not None and close(a2, orig["Alpha 2"]), f"defaults quit: frames untouched ({fmt(a2) if a2 else None})")
    all_onscreen("defaults quit")

    say("-- [windows] fit = \"narrow\": windows clear of the strip, narrowed when too wide")
    stop_all_fixtures()
    write_config('[windows]\nfit = "narrow"\n')
    # Alpha: one window over the strip column that fits, one too wide for the stage area.
    # Bravo: one over the strip column (its stage is parked at first start).
    start_extra_frames("a", 2, "20,60,700,400;10,120,-40,500")
    start_extra_frames("b", 1, "30,90,600,380")
    activate("a")
    time.sleep(1.0)
    before = {}
    wins, _ = fixture_windows([EXTRA["a"][0], EXTRA["b"][0]])
    s = start_app(3, extra_env={"STAGE_MANAGER_TEST": "1"})
    if not check(s is not None, "app up with Alpha and Bravo"):
        return
    vis, inset = s["display"]["visible"], s["stage_inset"]
    left = vis["x"] + inset
    check(s["fit_windows"] is True and s["strip_width"] == 200,
          f"fitting on (config); strip {s['strip_width']:.0f} pt on every display, stage area from x={left:.0f}")
    t = by_title(s)
    for name in ("Alpha 1", "Alpha 2", "Bravo 1"):
        before[name] = t[name]["original"] or t[name]["restore"]
    time.sleep(0.8)

    def placed(title):
        """Where the window is now (shown) or will be shown (parked: its restore frame)."""
        w = by_title(state())[title]
        return ("parked", w["restore"]) if w["parked"] else ("shown", live_frame(title))

    def narrowed(f):
        return (abs(f["x"] - left) <= 1 and abs(f["x"] + f["w"] - (vis["x"] + vis["w"])) <= 1
                and abs(f["h"] - before["Alpha 2"]["h"]) <= 1 and f["w"] < before["Alpha 2"]["w"] - 1)

    how, a1 = placed("Alpha 1")
    check(abs(a1["x"] - left) <= 1 and abs(a1["w"] - before["Alpha 1"]["w"]) <= 1,
          f"first start: a window over the strip moves right, same size ({how} {fmt(a1)})")
    how, a2 = placed("Alpha 2")
    check(narrowed(a2), f"first start: a too-wide window is narrowed to the stage area, height kept ({how} {fmt(a2)}, was {fmt(before['Alpha 2'])})")
    how, b1 = placed("Bravo 1")
    check(abs(b1["x"] - left) <= 1, f"first start: Bravo's window is clear too ({how} {fmt(b1)})")
    check({placed(n)[0] for n in ("Alpha 1", "Bravo 1")} == {"parked", "shown"}, "one stage shown, the other parked")
    consistent("visible first start")

    say("-- a stage restore keeps it clear; the narrowed window stays narrowed")
    s = state()
    t = by_title(s)
    number = {st["number"]: st["windows"] for st in s["stages"]}
    alpha = next(n for n, ws in number.items() if t["Alpha 2"]["id"] in ws)
    bravo = next(n for n, ws in number.items() if t["Bravo 1"]["id"] in ws)
    for n in (bravo, alpha, bravo, alpha):
        do("switch", n)
    b = by_title(state())["Bravo 1"]["restore"]
    a2 = live_frame("Alpha 2")
    check(abs(b["x"] - left) <= 1, f"Bravo's stage restores clear of the strip ({fmt(b)})")
    check(narrowed(a2), f"back on Alpha: shown narrowed ({fmt(a2)})")
    consistent("visible switch")

    say("-- a new window over the strip moves right")
    start_extra_frames("c", 1, "25,150,640,360")
    st = wait_state(lambda x: "Charlie 1" in active_titles(x), "Charlie's new stage")
    time.sleep(0.6)
    c1 = live_frame("Charlie 1")
    check(st is not None and abs(c1["x"] - left) <= 1, f"a new window over the strip moves right ({fmt(c1)})")
    time.sleep(1.5)
    screenshot("strip-fit.png", state())

    say("-- quit gives every window its exact earlier frame")
    pid = app_pid()
    t0 = time.time()
    ctl("quit")
    wait_gone(pid)
    took = time.time() - t0
    a2 = live_frame_probe("a", "Alpha 2")
    want = before["Alpha 2"]
    check(a2 is not None and close(a2, want), f"quit: the narrowed window is back at its exact frame ({fmt(a2) if a2 else None}, want {fmt(want)})")
    a1 = live_frame_probe("a", "Alpha 1")
    if not check(a1 is not None and close(a1, before["Alpha 1"]), f"quit: the moved window is back where it was ({fmt(a1) if a1 else None})"):
        say(f"  Alpha's windows: {fixture_windows([EXTRA['a'][0]])[0]}")
    check(took < 6, f"quit took {took:.1f} s")
    all_onscreen("visible quit")

    say("-- fit = \"move\" with the launch reveal and the edge tab on: a too-wide window stays put")
    stop_all_fixtures()
    write_config('[windows]\nfit = "move"\n[reveal]\nedge_tab = true\nlaunch_reveal = true\n')
    start_extra_frames("a", 1, "10,120,-40,500")
    activate("a")
    time.sleep(1.0)
    # The largest of the app's windows (an active app also lists its menu bar strip).
    wide = max((w["bounds"] for w in fixture_windows([EXTRA["a"][0]])[0]), key=lambda b: b["w"] * b["h"])
    s = start_app(1, extra_env={"STAGE_MANAGER_TEST": "1"})
    if not check(s is not None, "app up (nudge-only)"):
        return
    a1 = live_frame("Alpha 1")
    check(close(a1, wide), f"nudge-only: the too-wide window stays where it is ({fmt(a1)}, was {fmt(wide)})")
    check(edge_tab() is None, "the strip is shown for a moment at launch (no tab yet)")
    tab = wait_until(edge_tab, 8, "the edge tab")
    check("a window covers the strip; showing it for a moment" in log_text(), "the launch announce ran")
    if check(tab is not None, f"the edge tab appears at layer 3 with our pid ({fmt(tab['bounds']) if tab else None})"):
        b = tab["bounds"]
        mid = s["display"]["visible"]["y"] + s["display"]["visible"]["h"] / 2
        check(b["x"] <= vis["x"] + 1 and abs(b["y"] + b["h"] / 2 - mid) <= 2, "flush left, vertically centered")
        screenshot("edge-tab.png", s)
        r = ctl("debug-click", b["x"] + b["w"] / 2, b["y"] + b["h"] / 2)
        check(r.get("ok"), f"clicked the tab ({r.get('error', 'ok')})")
        gone = wait_until(lambda: edge_tab() is None, 3, "the tab to give way to the strip")
        check(gone is not None, "a click on the tab reveals the strip (the tab hides)")
        back = wait_until(edge_tab, 4, "the tab to come back once the pointer is away")
        check(back is not None, "the strip auto-hides again and the tab returns")
    stop_app()
    all_onscreen("nudge-only quit")

    say("-- quit right after a first start with many windows answers through the WM thread")
    stop_all_fixtures()
    write_config(None)
    start_fixture(12)
    start_extra("a", 3)
    start_extra("b", 2)
    start_extra("c", 2)
    if os.path.exists(LOG):
        os.remove(LOG)
    args = ["open", "-n", APP]
    for k, v in {"STAGE_MANAGER_HOME": HOME, "STAGE_MANAGER_ONLY": ONLY, "STAGE_MANAGER_NO_PROMPT": "1",
                 "STAGE_MANAGER_TEST": "1", "RUST_LOG": "info,stage_manager=debug"}.items():
        args += ["--env", f"{k}={v}"]
    subprocess.run(args, check=True)
    pid = wait_until(app_pid, 15, "the app")
    wait_until(lambda: "first start: one stage per app" in log_text(), 15, "the first start")
    t0 = time.time()
    r = ctl("debug-menu-quit")
    check(r.get("ok"), f"quit through the menu's path ({r.get('error', 'ok')})")
    if pid:
        wait_gone(pid, 35)
    took = time.time() - t0
    text = log_text()
    check("did not answer" not in text and "unparked every window" in text,
          f"the WM thread answered Quit ({took:.1f} s after the first start; no 'did not answer')")
    check(took < 10, f"quit returned within the timeout ({took:.1f} s)")
    all_onscreen("quit right after a first start")
    stop_all_fixtures()


# ----- parity: generated config, Cmd-M to the strip, a card's app icon -----------------


def icon_center(s, number):
    """Where the app icon of stage `number`'s card is, by the strip's layout rules (UX-SPEC):
    cards of 161 pt per visible height (at least 97 pt tall), 29.5 pt apart, the column
    centered on the visible frame; the icon centered 10.5 pt right of and above the front
    window's bottom-left corner. Single-window cards only."""
    vis = s["display"]["visible"]
    by_id = {w["id"]: w for w in s["windows"]}
    stages = {st["number"]: st for st in s["stages"]}
    heights = []
    for n in s["strip"]:
        r = by_id[stages[n]["windows"][0]]["restore"]
        heights.append(max(161.0 * r["h"] / vis["h"], 97.0))
    total = sum(heights) + 29.5 * (len(heights) - 1)
    top = vis["y"] + (vis["h"] - total) / 2
    for n, h in zip(s["strip"], heights):
        if n == number:
            return vis["x"] + 15 + 10.5, top + h - 10.5
        top += h + 29.5
    return None


def phase_parity():
    say("\n== parity: generated config, Cmd-M to the strip, a card's app icon ==")
    stop_app()
    stop_all_fixtures()
    write_config(None)
    start_extra_frames("a", 2, "400,80,600,400;460,140,620,420")
    start_extra_frames("b", 1, "420,100,640,420")
    activate("b")
    time.sleep(1.0)
    s = start_app(3, extra_env={"STAGE_MANAGER_TEST": "1"})
    if not check(s is not None, "app up with Alpha (2 windows) and Bravo"):
        return
    with open(os.path.join(HOME, "config.toml")) as f:
        text = f.read()
    live = [l for l in text.splitlines() if l.strip() and not l.lstrip().startswith("#") and not l.startswith("[")]
    check(not live, f"the generated config.toml holds no live values ({live[:3]})")
    r = run([BIN, "config", "--print-defaults"], timeout=10)
    check(r.stdout == text, "and it is exactly `config --print-defaults`")
    check("hotkeys registered active=0" in log_text(), "no global hotkey registered")

    say("-- Cmd-M sends the window to the strip as a card of its own")
    s = state()
    t = by_title(s)
    alpha = s["active"] if t["Alpha 1"]["home"] == s["active"] else t["Alpha 1"]["home"]
    do("switch", alpha)
    time.sleep(0.9)
    os.kill(extra_pid("a"), signal.SIGUSR2)
    s = wait_state(lambda x: by_title(x)["Alpha 1"]["parked"], "Alpha 1 to go to the strip")
    if check(s is not None, "a minimized window goes to the strip, not the Dock"):
        a1 = by_title(s)["Alpha 1"]
        check(stage_titles(s)[a1["home"]] == ["Alpha 1"] and a1["home"] in s["strip"], "as a card of its own")
        check(s["active"] == alpha and "Alpha 2" in active_titles(s), "the view stays on the current stage")
        check(close(a1["restore"], t["Alpha 1"]["restore"]), "its frame is kept for its return")
    consistent("Cmd-M")
    wins, d = fixture_windows([EXTRA["a"][0]])
    check(len([w for w in wins if w["bounds"]["h"] > 60]) == 2, "the window is not left in the Dock (both Alpha windows listed)")

    say("-- a click on a card's app icon brings every window of that app")
    s = state()
    t = by_title(s)
    bravo = t["Bravo 1"]["home"]
    do("switch", bravo)
    time.sleep(1.5)
    s = state()
    t = by_title(s)
    a1_stage = t["Alpha 1"]["home"]
    pt = icon_center(s, a1_stage)
    r = ctl("debug-click", pt[0], pt[1]) if pt else {"ok": False, "error": "no card"}
    check(r.get("ok"), f"clicked Alpha 1's card icon at {pt} ({r.get('error', 'ok')})")
    s = wait_state(lambda x: active_titles(x) == {"Alpha 1", "Alpha 2"}, "both Alpha windows shown")
    check(s is not None, "the icon click shows the card's stage with every Alpha window")
    consistent("icon click")
    stop_app()
    all_onscreen("parity quit")
    stop_all_fixtures()


def start_extra_frames(k, n, frames):
    bundle, path, pattern, title = EXTRA[k]
    stop_extra(k)
    subprocess.run(["open", "-n", "-g", path, "--args", "--windows", str(n), "--title", title, "--background",
                    "--frames", frames], check=True)
    return wait_until(lambda: len(fixture_windows([bundle])[0]) >= n, 15, f"{n} {title} windows")


def live_frame_probe(k, title):
    """A fixture window's frame by title through AX-free means: the window server's list,
    matched by the app's windows sorted by id (titles are 1..n in creation order)."""
    wins = sorted((w for w in fixture_windows([EXTRA[k][0]])[0] if w["bounds"]["h"] > 60), key=lambda w: w["id"])
    i = int(title.rsplit(" ", 1)[1]) - 1
    return wins[i]["bounds"] if i < len(wins) else None


def frontmost_bundle():
    r = subprocess.run(["lsappinfo", "info", "-only", "bundleid", "front"], capture_output=True, text=True)
    out = r.stdout.strip()
    return out.split("=", 1)[1].strip().strip('"') if "=" in out else None


def main():
    phases = sys.argv[1:] or ["m3", "safety", "m4", "m6", "strip", "native", "visible", "parity", "motion"]
    if not os.path.exists(BIN) or not all(os.path.exists(p) for p in [FIX_APP] + [e[1] for e in EXTRA.values()]):
        say("build first: scripts/test-stages.sh builds the app and the fixture")
        return 2
    if pids_of("Stage Manager.app/Contents/MacOS/stage-manager$") and not app_pid():
        say("note: another Stage Manager is running (not ours); it uses another home and is left alone")
    say(f"home: {HOME}")
    front = frontmost_bundle()
    try:
        for p in phases:
            if owner_app_running():
                say("STOP: another Stage Manager (the owner's) is running; no live phase runs beside it")
                failed.append("the owner's app is running")
                break
            {"visible": phase_visible, "m3": phase_m3, "safety": phase_safety, "m4": phase_m4, "m6": phase_m6, "strip": phase_strip,
             "native": phase_native, "parity": phase_parity, "motion": phase_motion}[p]()
    finally:
        stop_app()
        if pids_of(FIX_EXE) or any(pids_of(e[2]) for e in EXTRA.values()):
            all_onscreen("cleanup")
        stop_all_fixtures()
        if front and front not in ALL_IDS:
            subprocess.run(["open", "-b", front], capture_output=True)
    say(f"\n{len(passed)} passed, {len(failed)} failed (every app run's log: {HOME}/stage-manager*.log)")
    for f in failed:
        say(f"  FAIL {f}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
