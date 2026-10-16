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
along the whole path, and the pointer is put back; then wallpaper clicks with a Finder
window of the test's own scratch folder: Finder's echo never brings its window back, card
clicks while the desktop shows, a deliberate Finder activation, `[strip]
hide_while_desktop_shown`; skipped while the owner has Finder windows open or Apple's own
wallpaper click would act), visible (by default, Apple's: no window
is moved or resized, ever; with `[windows] fit = "narrow"` windows kept clear of the strip on
first start, switch and creation, a too-wide one narrowed and given back its exact frame on
quit; with `fit = "move"` plus the launch reveal and the edge tab turned on, those; a
menu-path quit right after a first start with many windows), parity (the generated config has
no live values and registers no hotkey; Cmd-M sends a window to the strip; a click on a
card's app icon brings every window of that app), motion (the strip below windows and lifted
while a window is dragged over it, with the dragged window's card in the motion overlay; a
window dropped above the first card takes that slot; a card dragged to another slot keeps it
across a relaunch; a card dropped in a gap reorders and one dropped on another card's middle
merges the two stages (`[strip] merge_on_drop`, on by default); a card click animates and every frame is exact afterwards;
`level = "above_windows"` keeps the strip on top), slots (a dozen card clicks in a mixed order,
one at a time and fast, each a strict swap with every snapshot starting on its window and
landing on the card or window it aimed at; a click on the slot a transition is still landing
in does nothing; a 2 pt and a 7 pt wobble are clicks; a card dragged down stays whole and
above the others; with SM_PIXELS=1, a window dragged over the strip shows its card under the
pointer at the top and bottom quarter and a slowed switch's snapshot lands on its window, read
back from the overlay's pixels), capture (the one phase with window
captures on: real thumbnails and snapshots, and a quit that waits for captures in flight), expose (Mission Control and App Exposé,
typed through the app with the user's own shortcuts, never a Desktop change: every strip pane
is Transient, the window server no longer lists the strip on screen while they show, and after Escape it is back on the same
Desktop, drawn as before, and a card click switches), restore (a restart simulated with
STAGE_MANAGER_SESSION: a layout of three fixture apps saved in session A; in session B the app
starts on an empty screen, keeps every saved stage pending with no card, then the apps reopen
their windows late, one by one, one app in reverse order and retitled, two windows already
minimized when the app starts (no card for a waiting stage unless declared in [stages] names):
every window goes back to its saved stage and frame, parked only on a strong or medium match,
the Desktop takes the saved id, the active stage is never switched away, the strip comes back
in its saved order; a minimized window un-minimized during the restore window goes quietly to
its inactive stage; the login settles with no stage switch and ends by itself once apps are
quiet for 20 s, the saved key window then focused, the restore's line logged and repeated by
`log summary`; one un-minimized after it brings its stage forward; an app with no saved window goes by Apple's rule; the app that never reopens keeps
its stage through a relaunch in the same session and loses it in the next session; then a
logout simulated with the test's own note (STAGE_MANAGER_LOGOUT_NOTE, never Apple's): the head
start at its first signal, repeats ignored, a cancel resuming at once, the apps quitting while
it holds with the file kept, and the next session bringing each window back into its stage at
its saved frame (one reopened clamped against the park corner, one at the park spot); and the
same with no early signal, from the entries kept with `quit_at`. Hands off the Mac during this
phase: a click or key press ends the restore window early).

Tests that need an extension turn it on in their own config.toml.

Window captures (ScreenCaptureKit) are off in every app run (STAGE_MANAGER_NO_CAPTURE=1:
cards and switch ghosts are drawn without pictures) except the capture phase's single
launch, because macOS may answer a capture with a screen-access alert (src/platform/capture.rs,
"Quiet by design"). The run ends with the number of replayd alerts macOS showed meanwhile.

No phase runs while the owner's installed app is up, and only our own app's guardian is
ever signalled.

SM_FIXTURE_ID sets the main fixture's bundle ID (default dev.eugene.stagemanager.fixture.stages);
the native phase's apps are <id>.a, <id>.b and <id>.c.
"""

import json
import os
import re
import signal
import struct
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


def log_path():
    """Today's log in the test home (the app writes one file per local day)."""
    return os.path.join(HOME, time.strftime("%Y-%m-%d") + ".log")


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
    """engine::park::is_parked_in on the main display: its full frame's park zone or, when
    `d` carries it, its visible frame's (macOS keeps a parked window above a bottom Dock)."""
    v = d.get("visible")
    return is_parked_on(b, d) or (bool(v) and v["w"] > 0 and v["h"] > 0 and is_parked_on(b, v))


def is_parked_on(b, d):
    """engine::park::is_parked against one frame."""
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
    a repo's target/ or anywhere else), or another Stage Manager's guardian still waiting for a
    window it left parked: no live phase runs while it is up. Such a guardian brings back any
    window it finds in the park zone, the fixtures' too (10-16: every Dock arrival failed)."""
    ours = app_pid()
    apps = pids_of("Stage Manager.app/Contents/MacOS/stage-manager$")
    guardians = set(pids_of("Stage Manager.app/Contents/MacOS/stage-manager --guardian ")) - set(guardian_pids(ours))
    return any(p != ours for p in apps) or bool(guardians)


def archive_log():
    """Keeps the last app run's log as stage-manager.run-NN.log (renamed, not removed: its
    guardian keeps writing into the same file), so a run leaves the whole story."""
    log = log_path()
    if not os.path.exists(log):
        return None
    n = 1 + max([int(f[len("stage-manager.run-"):-4]) for f in os.listdir(HOME)
                 if f.startswith("stage-manager.run-") and f.endswith(".log") and f[18:-4].isdigit()] or [0])
    path = os.path.join(HOME, f"stage-manager.run-{n:02d}.log")
    os.rename(log, path)
    return path


def start_app(expect_windows, extra_env=None, fresh_log=True, capture=False):
    """Launches the app (window captures off unless `capture`) and waits for its windows."""
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
    if not capture:
        env["STAGE_MANAGER_NO_CAPTURE"] = "1"
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
        with open(log_path(), encoding="utf-8", errors="replace") as f:
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
    if not wins:
        # A fixture that crashed, or a probe that failed, must not pass as "nothing parked".
        return check(False, f"{what}: every fixture window onscreen (the window server lists no fixture window)")
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
            if w.get("bundle_id") in ALL_IDS:
                bad.append(f"{w['title']} managed but not listed by the window server")
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
    back = [x for x in s["windows"] if x["role"] == "standard" and x["home"] == active]
    check(
        s["active"] == active and bool(back) and not any(x["parked"] for x in back),
        "a second reveal brings it back (the stage active again, its windows shown)",
    )
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
    check("windows still parked after unparking" not in log_text(),
          "ctl quit: no false \"still parked\" warning (the list is read after it settles)")
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
    native_wallpaper()


# ----- native: wallpaper clicks and Finder ----------------------------------------------


FINDER = "com.apple.finder"


def wm_default(key):
    """A `com.apple.WindowManager` value as text, or None when absent."""
    r = subprocess.run(["defaults", "read", "com.apple.WindowManager", key], capture_output=True, text=True)
    return r.stdout.strip() if r.returncode == 0 else None


def finder_windows():
    """Finder's standard windows (layer 0; the desktop is below), on any Space."""
    return [w for w in probe(FINDER)["windows"] if w["layer"] == 0]


def finder_parked():
    """Every Finder standard window sits in the park zone (and there is one)."""
    p = probe(FINDER)
    wins = [w for w in p["windows"] if w["layer"] == 0]
    return bool(wins) and all(is_parked(w["bounds"], p["display"]) for w in wins)


def shown(s, title):
    """Window `title` is in the active stage and not parked."""
    w = by_title(s).get(title)
    return w is not None and not w["parked"] and w["home"] == s["active"]


def switched_since_click():
    """Lines since the first wallpaper click of this run that switched stages."""
    text = log_text()
    i = text.find("wallpaper click x=")
    return [ln for ln in text[i:].splitlines() if "; switching" in ln] if i >= 0 else []


def wallpaper_point(s, avoid):
    """A point on the main display where only the wallpaper is (Finder's desktop window
    topmost): right of the strip column, clear of every window in `avoid` (bounds) and of
    the managed windows' saved frames. Found by asking the app to click there: it posts
    nothing while another app's window is topmost, so a refused point costs nothing.
    Returns the point clicked, or None."""
    vis = s["display"]["visible"]
    frames = avoid + [w["restore"] for w in s["windows"]]

    def clear(x, y):
        return not any(f["x"] - 20 <= x <= f["x"] + f["w"] + 20 and f["y"] - 20 <= y <= f["y"] + f["h"] + 20
                       for f in frames)

    x0 = vis["x"] + s["strip_width"] + 80
    for y in range(int(vis["y"] + 120), int(vis["y"] + vis["h"] - 120), 140):
        for x in range(int(vis["x"] + vis["w"] - 120), int(x0), -160):
            if clear(x, y) and ctl("debug-click", x, y).get("ok"):
                return x, y
    return None


def wallpaper_click(pt, what):
    r = ctl("debug-click", *pt)
    check(r.get("ok"), f"{what}: clicked the wallpaper ({r.get('error', 'ok')})")
    time.sleep(1.5)  # past the reveal and Finder's echo
    return state()


def click_card(s, title, what):
    """Clicks the card of the stage holding `title` (its drawn frame); True when posted."""
    home = by_title(s)[title]["home"]
    b = next((c["frame"] for c in s.get("strip_cards", []) if c["number"] == home), None)
    r = ctl("debug-click", b["x"] + b["w"] / 2, b["y"] + b["h"] / 2) if b else {"ok": False, "error": "no card drawn"}
    return check(r.get("ok"), f"clicked {what} ({r.get('error', 'ok')}{whose(r.get('error'))})")


def strip_titles(s):
    """The strip, top to bottom, as each card's sorted window titles."""
    st = stage_titles(s)
    return [sorted(st.get(n, [])) for n in s["strip"]]


def native_wallpaper():
    """Apple's wallpaper click with Finder in play (the owner's report of 10-12: "Finder comes
    back and leaves, back and leaves, on each desktop click"). macOS gives the desktop to
    Finder, so each click activates Finder and may focus its window; that echo must never
    bring Finder's window back. A second click brings back the stage the first revealed
    (UX-SPEC), never Finder's. A card clicked while the desktop shows brings exactly that
    stage; the other cards keep their order. A deliberate Finder activation (`open -b`: the
    Dock's and the app switcher's path) brings Finder's stage, as Apple's does.

    Finder is managed here (STAGE_MANAGER_ONLY gains com.apple.finder), so the sub-test runs
    only when Finder has no window of its own: the test opens one on a scratch folder in the
    test home, while the app runs, and closes it (through the app) at the end. It also needs
    Apple's Stage Manager off and "Click wallpaper to reveal desktop" set to Only in Stage
    Manager (`EnableStandardClickToShowDesktop = 0`): with Always, also when the key is
    absent (macOS's default), macOS itself moves every window aside on the synthetic click
    and our reveal stays off."""
    say("-- wallpaper clicks with a Finder window (the test's own scratch folder)")
    if wm_default("GloballyEnabled") == "1":
        say("  skipped: Apple's Stage Manager is on; a real wallpaper click would act on every window")
        return
    if wm_default("EnableStandardClickToShowDesktop") != "0":
        say("  skipped: \"Click wallpaper to reveal desktop\" is Always (set, or absent: macOS's default); "
            "macOS would move every window aside and our reveal is off")
        return
    if finder_windows():
        say(f"  skipped: Finder has {len(finder_windows())} window(s) of the owner's open; managing Finder would move them")
        return
    folder = os.path.join(HOME, "finder-scratch")
    os.makedirs(folder, exist_ok=True)
    env = {"STAGE_MANAGER_TEST": "1", "STAGE_MANAGER_ONLY": ONLY + "," + FINDER}
    try:
        write_config(None)
        start_extra("a", 1)
        start_extra("b", 1)
        activate("a")
        time.sleep(1.0)
        if not check(start_app(2, extra_env=env) is not None, "app up with Finder managed"):
            return
        subprocess.run(["open", folder], check=True)
        s = wait_state(lambda x: any(w["bundle_id"] == FINDER for w in x["windows"]), "the scratch Finder window managed", 10)
        if not check(s is not None and len(finder_windows()) == 1, "a Finder window on the scratch folder, managed"):
            return
        finder = next(w["title"] for w in s["windows"] if w["bundle_id"] == FINDER)
        time.sleep(1.0)
        activate("a")
        s = wait_state(lambda x: shown(x, "Alpha 1"), "Alpha's stage")
        check(s is not None and not shown(s, finder) and finder_parked(),
              f"Alpha's stage is shown; Finder's window sits parked in its own stage ({s and stage_titles(s)})")
        alpha_frame = by_title(state())["Alpha 1"]["restore"]
        time.sleep(1.0)
        pt = wallpaper_point(state(), [w["bounds"] for w in finder_windows()])
        if not check(pt is not None, "found a spot where only the wallpaper is (and clicked it)"):
            return
        time.sleep(1.5)

        say("-- first click: the desktop; Finder's echo is ignored")
        s = state()
        check(s["desktop_revealed"] and not active_titles(s), f"the desktop is revealed ({stage_titles(s)})")
        check(finder_parked(), "Finder's window stays parked")
        check(by_title(s)["Alpha 1"]["parked"] and by_title(s)["Bravo 1"]["parked"], "every fixture window is parked")
        check(["Alpha 1"] in strip_titles(s), "the revealed stage's card is in the strip")
        check("after a wallpaper click: ignored" in log_text(), "Finder's activation or focus after the click was ignored")
        check(not switched_since_click(), f"nothing switched after the click ({switched_since_click()})")
        consistent("first wallpaper click")

        say("-- second click: the revealed stage comes back (UX-SPEC), never Finder's")
        s = wallpaper_click(pt, "second click")
        check(shown(s, "Alpha 1"), f"Alpha's stage is back ({stage_titles(s)})")
        check(finder_parked() and not shown(s, finder), "Finder's window stays parked")
        consistent("second wallpaper click")

        say("-- third click: the desktop again")
        s = wallpaper_click(pt, "third click")
        check(s["desktop_revealed"] and finder_parked() and by_title(s)["Alpha 1"]["parked"],
              "the desktop is revealed again; Finder's window stays parked")
        check(not switched_since_click(), f"no wallpaper click or echo switched stages ({switched_since_click()})")
        consistent("third wallpaper click")

        say("-- a card click while the desktop shows: the stage just revealed, as it was")
        others = [c for c in strip_titles(s) if c != ["Alpha 1"]]
        if click_card(s, "Alpha 1", "the revealed stage's card"):
            time.sleep(1.5)  # past Finder's echo
            s = state()
            check(shown(s, "Alpha 1") and close(by_title(s)["Alpha 1"]["restore"], alpha_frame),
                  "exactly that stage, at its frame")
            check(finder_parked() and by_title(s)["Bravo 1"]["parked"], "every other window stays parked")
            check(strip_titles(s) == others, f"the other cards keep their order ({strip_titles(s)} vs {others})")
            consistent("card click while revealed")

        say("-- a card click while the desktop shows: another stage")
        s = wallpaper_click(pt, "fourth click")
        others = [c for c in strip_titles(s) if c != ["Bravo 1"]]
        if click_card(s, "Bravo 1", "Bravo's card"):
            time.sleep(1.5)
            s = state()
            check(shown(s, "Bravo 1"), "exactly Bravo's stage, no bounce")
            check(finder_parked() and by_title(s)["Alpha 1"]["parked"], "every other window stays parked")
            check(strip_titles(s) == others, f"the other cards keep their order ({strip_titles(s)} vs {others})")
            consistent("second card click while revealed")

        say("-- a deliberate Finder activation brings Finder's stage")
        wallpaper_click(pt, "fifth click")
        time.sleep(0.5)  # well past the click's echo
        subprocess.run(["open", "-b", FINDER], check=True)
        s = wait_state(lambda x: shown(x, finder), "Finder's stage after open -b", timeout=4)
        check(s is not None and not finder_parked(),
              "open -b com.apple.finder with the desktop shown: Finder's stage comes (Apple's Dock click)")
        activate("b")
        wait_state(lambda x: shown(x, "Bravo 1"), "Bravo's stage")
        time.sleep(1.0)
        subprocess.run(["open", "-b", FINDER], check=True)
        s = wait_state(lambda x: shown(x, finder), "Finder's stage after open -b", timeout=4)
        check(s is not None and not finder_parked() and by_title(s)["Bravo 1"]["parked"],
              "open -b com.apple.finder from another app: Finder's stage comes (Apple's app switcher)")
        check(len(finder_windows()) == 1, f"no new Finder window ({len(finder_windows())})")

        say("-- [strip] hide_while_desktop_shown = true")
        with open(os.path.join(HOME, "config.toml"), "w") as f:
            f.write("[strip]\nhide_while_desktop_shown = true\n")
        do("reload")
        activate("a")
        wait_state(lambda x: shown(x, "Alpha 1"), "Alpha's stage")
        time.sleep(1.0)
        s = wallpaper_click(pt, "click with the option on")
        check(s["desktop_revealed"] and s["strip_hidden_for_desktop"], "the strip hides while the desktop shows")
        check(finder_parked() and s["strip_hidden_for_desktop"], "Finder's echo changes nothing (the strip stays hidden)")
        activate("b")
        s = wait_state(lambda x: not x["strip_hidden_for_desktop"], "the strip back")
        check(s is not None and shown(s, "Bravo 1"), "an activation brings the strip back with its stage")
    finally:
        # The test's own Finder window goes (through our app, while it runs), then the app.
        for w in finder_windows():
            r = ctl("debug-close", w["id"])
            if not r.get("ok"):
                say(f"  could not close Finder window {w['id']}: {r}")
        wait_until(lambda: not finder_windows(), 5, "the scratch Finder window to close")
        stop_app()
        check(not finder_windows(), "the scratch Finder window is closed")
        all_onscreen("wallpaper quit")
        stop_all_fixtures()
        write_config(None)
        subprocess.run(["rm", "-rf", folder])


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


def card_drag(start, end, what):
    """Drags a card from `start` to `end` (holding there 600 ms), sampling `ctl debug-strip`
    while it runs. Returns the reply and the samples."""
    path = [start, (start[0], start[1] + 12), end]
    words = [str(round(v, 1)) for p in path for v in p] + ["hold=600"]
    say(f"  dragging {what}")
    proc = subprocess.Popen([BIN, "ctl", "debug-drag", *words], env=ENV, stdout=subprocess.PIPE, text=True)
    seen = sample_strip_during(proc)
    out, _ = proc.communicate(timeout=20)
    return out, seen


def merge_sub_test(vis):
    """`[strip] merge_on_drop` (on by default): a card dropped in a gap reorders as before; one
    dropped on another card's middle merges the two stages, and nothing unparks."""
    say("-- a card dropped in a gap reorders; one dropped on a card's middle merges the stages")
    time.sleep(1.0)
    reveal_strip("merge")
    s = state()
    cards = s.get("strip_cards", [])
    if not check(len(cards) >= 3 and [c["number"] for c in cards] == s["strip"],
                 f"three or more cards drawn in the engine's order ({[c['number'] for c in cards]} vs {s['strip']})"):
        return
    x = vis["x"] + 70
    order0 = s["strip"]
    # Into the gap between the second and third cards: the first card goes right above the third.
    f1, f2 = cards[1]["frame"], cards[2]["frame"]
    before_log = len(log_text())
    out, seen = card_drag((x, center(cards[0]["frame"])[1]), (x, (f1["y"] + f1["h"] + f2["y"]) / 2),
                          "the first card into the gap above the third")
    check('"ok": true' in out or '"ok":true' in out, f"the drag ran ({out.strip()[:120]}{whose(out)})")
    want = [order0[1], order0[0]] + order0[2:]
    s = wait_state(lambda x: x["strip"] == want, "the gap drop to reorder")
    check(s is not None, f"a gap drop reorders as before ({order0} -> {state()['strip']}, wanted {want})")
    check(not any(p.get("merge_onto") for p in seen[-3:]), "held in a gap, no card offered to merge")
    text = log_text()[before_log:]
    check("card moved to another slot" in text and "the stages merge" not in text, "logged as a reorder, not a merge")

    time.sleep(1.0)
    reveal_strip("merge, again")
    s = state()
    cards = s.get("strip_cards", [])
    ids = ids_by_number(s)
    if not check(len(cards) >= 3, f"cards drawn ({len(cards)})"):
        return
    src, dst = ids[cards[0]["number"]], ids[cards[2]["number"]]
    members = {st["id"]: list(st["windows"]) for st in s["stages"]}
    moving, staying = members[src], members[dst]
    n0 = len(s["strip"])
    before_log = len(log_text())
    out, seen = card_drag((x, center(cards[0]["frame"])[1]), (x, center(cards[2]["frame"])[1]),
                          "the first card onto the third card's middle")
    check('"ok": true' in out or '"ok":true' in out, f"the drag ran ({out.strip()[:120]}{whose(out)})")
    offered = [p for p in seen if p.get("merge_onto") == dst]
    check(bool(offered), f"held over the third card's middle, it is offered the merge ({len(offered)} of {len(seen)} samples)")
    s = wait_state(lambda x: len(x["strip"]) == n0 - 1, "the merge (one card fewer)")
    if not check(s is not None, f"one card fewer after the drop ({n0} -> {len(state()['strip'])})"):
        return
    time.sleep(1.0)
    s = state()
    after = {st["id"]: list(st["windows"]) for st in s["stages"]}
    check(src not in after, f"the dropped card's stage is gone ({sorted(after)})")
    check(after.get(dst) == staying + moving,
          f"the target's stage holds both, its own in front ({after.get(dst)}, wanted {staying + moving})")
    parked = {w["id"]: w["parked"] for w in s["windows"]}
    check(all(parked.get(i) for i in staying + moving), "every window of both stays parked (nothing moved on screen)")
    num = numbers_by_id(s)
    check(num.get(dst) in s["strip"], f"the target keeps a card ({s['strip']}, target {num.get(dst)})")
    # The strip may have slid away again (a window covers its column): ask it what it has.
    p = strip_probe() or {}
    drawn = [c["stage"] for c in p.get("cards", [])]
    check(p.get("order") == [ids_by_number(s)[n] for n in s["strip"]] and drawn == p.get("order"),
          f"the strip has one card fewer, the dropped one's layers gone (order {p.get('order')}, layers {drawn})")
    check("card dropped on a card: the stages merge" in log_text()[before_log:], "logged as a merge")
    consistent("card merge")


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

    merge_sub_test(vis)

    say("-- a card click animates; every window ends at its exact frame")
    time.sleep(1.5)
    reveal_strip("card click")
    s = state()
    cards = s.get("strip_cards", [])
    if check(bool(cards), "cards drawn"):
        c = cards[0]
        b = c["frame"]
        key = next((st["key"] for st in s["stages"] if st["number"] == c["number"]), None)
        r = ctl("debug-click", b["x"] + b["w"] / 2, b["y"] + b["h"] / 2)
        check(r.get("ok"), f"clicked a card ({r.get('error', 'ok')})")
        # The keyboard goes to the new stage at once (Apple activates the app first): its
        # key window is raised in front while it still sits parked behind its snapshot.
        early = []
        for _ in range(8):
            p = probe()
            mine = [w for w in p["windows"] if w["layer"] == 0 and w["onscreen"] and w["z"] is not None]
            front = min(mine, key=lambda w: w["z"]) if mine else None
            if front and front["id"] == key and is_parked(front["bounds"], p["display"]):
                early.append(front["id"])
                break
        check(any(w["layer"] == OVERLAY_LAYER for w in our_windows()), "snapshots move in the overlay")
        s = wait_state(lambda x: x["active"] == c["number"], "the click to switch")
        check(s is not None, "the click switched")
        check("the keyboard goes to the new stage now" in log_text(),
              "the new stage's key window is focused with the parks, before its window arrives")
        if not early:
            say("  note: no probe caught the key window raised while still parked (it may have arrived first)")
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


# ----- slots: every switch lands where it aims; clicks are clicks; a card drags whole ------


CONFIG_SLOTS = """
[strip]
level = "above_windows"
""" + CONFIG_BACKDROP

# A slow spring (about ten times Apple's), so the overlay can be read back while it moves.
CONFIG_SLOW = CONFIG_SLOTS.replace("[strip]", "[animation]\nspring_stiffness = 6.0\nspring_damping = 4.9\n\n[strip]")

CLICK_RE = re.compile(r"card click in slot stage=(\d+) slot=Some\((\d+)\)")


def ids_by_number(s):
    return {st["number"]: st["id"] for st in s["stages"]}


def numbers_by_id(s):
    return {st["id"]: st["number"] for st in s["stages"]}


def center(f):
    return (f["x"] + f["w"] / 2, f["y"] + f["h"] / 2)


def inside(p, f, pad=0.0):
    return f["x"] - pad <= p[0] <= f["x"] + f["w"] + pad and f["y"] - pad <= p[1] <= f["y"] + f["h"] + pad


def off(a, b):
    return max(abs(a[k] - b[k]) for k in ("x", "y", "w", "h"))


def cg_bounds(wid):
    wins, _ = fixture_windows([FIX_ID])
    return next((w["bounds"] for w in wins if w["id"] == wid), None)


def swap_holds(before, after, slot, leaving):
    """Apple's swap: the stage left took the clicked card's slot; every other card kept its."""
    return (len(before) == len(after) and after[slot] == leaving
            and all(a == b for i, (a, b) in enumerate(zip(before, after)) if i != slot))


def click_slot(slot, what, settle=1.0):
    """Clicks the card in `slot` and checks the swap, the drawn order, and where every
    snapshot of the transition started, aimed and ended. Returns the state after."""
    s0 = state()
    order0, active0, cards = s0["strip"], s0["active"], s0.get("strip_cards", [])
    if not check(len(cards) == len(order0) and [c["number"] for c in cards] == order0,
                 f"{what}: the strip draws the engine's order ({[c['number'] for c in cards]} vs {order0})"):
        return s0
    target = order0[slot]
    t0 = s0.get("last_transition") or {}
    leaving_ids = {w["id"] for w in s0["windows"] if w["home"] == active0 and not w["parked"] and w["role"] == "standard"}
    pre = {wid: cg_bounds(wid) for wid in leaving_ids}
    r = ctl("debug-click", *center(cards[slot]["frame"]))
    check(r.get("ok"), f"{what}: clicked slot {slot} ({r.get('error', 'ok')})")
    if not check(wait_state(lambda x: x["active"] == target, f"{what}: the switch") is not None,
                 f"{what}: slot {slot}'s stage {target} is active"):
        return state()
    time.sleep(settle)
    s = state()
    order = s["strip"]
    check(swap_holds(order0, order, slot, active0),
          f"{what}: swap: stage {active0} took slot {slot}, every other card kept its slot ({order0} -> {order})")
    check([c["number"] for c in s.get("strip_cards", [])] == order,
          f"{what}: the cards are drawn in that order ({[c['number'] for c in s.get('strip_cards', [])]})")
    t = s.get("last_transition") or {}
    if not check(t.get("id") and t.get("id") != t0.get("id"), f"{what}: a transition was drawn"):
        return s
    num = numbers_by_id(s)
    bad = []
    drawn = s["strip_cards"][slot]["frame"]
    for g in t["ghosts"]:
        if g["outgoing"]:
            if g["slot"] != slot or g["aimed_slot"] != slot or num.get(g["stage"]) != active0:
                bad.append(f"outgoing {g['window']} aimed at slot {g['aimed_slot']} (engine {g['slot']}, stage {num.get(g['stage'])})")
            if not inside(center(g["end"]), drawn, 4):
                bad.append(f"outgoing {g['window']} ends at {fmt(g['end'])}, its card is drawn at {fmt(drawn)}")
            b = pre.get(g["window"])
            if b and off(g["start"], b) > 1.0:
                bad.append(f"outgoing {g['window']} starts at {fmt(g['start'])}, the window was at {fmt(b)}")
        else:
            if g["aimed_slot"] != slot or num.get(g["stage"]) != target:
                bad.append(f"incoming {g['window']} grew from slot {g['aimed_slot']} (stage {num.get(g['stage'])})")
            b = cg_bounds(g["window"])
            if not g.get("real") or not b or off(g["real"], b) > 1.0 or off(g["end"], b) > 1.0:
                bad.append(f"incoming {g['window']} ends at {fmt(g['end'])}, the window is at {b and fmt(b)}")
    check(not bad, f"{what}: every snapshot started on its window and landed on its card or window" + (f" ({'; '.join(bad)})" if bad else ""))
    return s


def strip_probe():
    r = ctl("debug-strip")
    return r.get("strip") if r.get("ok") else None


def sample_strip_during(proc, after=0.8):
    """`ctl debug-strip` samples while the drag `proc` runs, and `after` seconds more (the
    card gliding into its slot)."""
    seen = []
    while proc.poll() is None:
        p = strip_probe()
        if p:
            seen.append(p)
    end = time.time() + after
    while time.time() < end:
        p = strip_probe()
        if p:
            seen.append(p)
    return seen


def phase_slots():
    say("\n== slots: a switch's snapshots land where they aim; clicks are clicks; a card drags whole ==")
    stop_app()
    stop_all_fixtures()
    write_config(CONFIG_SLOTS)
    start_fixture(7)
    s = start_app(7, extra_env={"STAGE_MANAGER_TEST": "1"})
    if not check(s is not None, "app up with seven fixture windows"):
        return
    start_backdrop(s)
    vis = s["display"]["visible"]
    left = vis["x"] + s["strip_width"] + 60
    t = by_title(s)
    # All the same size, clear of the strip: the slots keep their places, only the order moves.
    for n in range(1, 8):
        ctl("debug-frame", t[f"Fixture {n}"]["id"], left, vis["y"] + 60, 640, 420)
    for n in range(1, 7):
        do("new", t[f"Fixture {n}"]["id"])
    time.sleep(1.5)
    s = state()
    n_cards = len(s.get("strip_cards", []))
    if not check(n_cards >= 4, f"cards drawn ({s['strip']}, {n_cards} drawn, capacity {s['strip_capacity']})"):
        return
    overlay = ctl("debug-strip").get("strip") or {}
    check(overlay.get("overlay_top_down") is True, "the motion overlay draws top-down")

    say("-- clicking around, one click at a time, in a mixed order")
    picks = [p % n_cards for p in (3, 0, 4, 1, 2, 4, 0, 3, 1, 2, 0, 5)]
    for i, slot in enumerate(picks):
        click_slot(slot, f"click {i + 1} (slot {slot})")

    say("-- clicking around fast: each click while the last one still moves")
    s = state()
    frames = [c["frame"] for c in s["strip_cards"]]
    order, active = list(s["strip"]), s["active"]
    ids = ids_by_number(s)
    expected = []
    before_log = len(log_text())
    for slot in [p % n_cards for p in (2, 0, 3, 1, 4, 2, 0, 1, 3, 0)]:
        target = order[slot]
        order[slot], active = active, target
        expected.append((ids[target], slot))
        ctl("debug-click", *center(frames[slot]))
        time.sleep(0.15)
    s = wait_state(lambda x: x["active"] == active, "the last fast click to switch", timeout=8)
    time.sleep(1.5)
    s = state()
    check(s["strip"] == order and s["active"] == active,
          f"fast clicks: the order is exactly the swaps' ({s['strip']}, active {s['active']}; expected {order}, active {active})")
    clicks = [(int(a), int(b)) for a, b in CLICK_RE.findall(log_text()[before_log:])]
    check(clicks == expected, f"fast clicks: each click hit the card drawn under the pointer ({clicks} vs {expected})")
    check([c["number"] for c in s.get("strip_cards", [])] == s["strip"], "fast clicks: the cards are drawn in the engine's order")

    say("-- a second click on the slot a transition is still landing in does nothing")
    s0 = state()
    slot = 2 % n_cards
    c = center(s0["strip_cards"][slot]["frame"])
    target, active0, order0 = s0["strip"][slot], s0["active"], s0["strip"]
    ctl("debug-click", *c)
    time.sleep(0.1)
    ctl("debug-click", *c)
    time.sleep(1.5)
    s = state()
    check(s["active"] == target and swap_holds(order0, s["strip"], slot, active0),
          f"the second click hit the landing (invisible) card and did nothing (active {s['active']}, wanted {target}; {order0} -> {s['strip']})")

    say("-- a press that wobbles is a click, never a reorder")
    for wobble in (2, 7):
        s0 = state()
        slot = 1 % n_cards
        c = center(s0["strip_cards"][slot]["frame"])
        target, active0, order0 = s0["strip"][slot], s0["active"], s0["strip"]
        before_log = len(log_text())
        out, _ = drag_sampled([c, (c[0], c[1] + wobble)], f"a {wobble} pt wobble on slot {slot}")
        s = wait_state(lambda x: x["active"] == target, f"the {wobble} pt wobble to switch")
        check(s is not None, f"a press that moves {wobble} pt and lets go switches like a click ({out.strip()[:80]})")
        if s:
            time.sleep(1.0)
            check(swap_holds(order0, state()["strip"], slot, active0), f"{wobble} pt wobble: the swap, nothing reordered ({order0} -> {state()['strip']})")
        check("card moved to another slot" not in log_text()[before_log:], f"{wobble} pt wobble: no reorder")

    say("-- a card dragged down passes over the others whole (thumbnail and icons together)")
    s = state()
    if n_cards >= 4:
        a, b = s["strip_cards"][0]["frame"], s["strip_cards"][3]["frame"]
        dragged = ids_by_number(s)[s["strip"][0]]
        start = center(a)
        # Below the fourth card's middle half (`[drag] merge_zone`): its gap, not a merge.
        path = [start, (start[0], start[1] + 12), (start[0], b["y"] + b["h"] * 0.9)]
        words = [str(round(v, 1)) for p in path for v in p] + ["hold=600"]
        proc = subprocess.Popen([BIN, "ctl", "debug-drag", *words], env=ENV, stdout=subprocess.PIPE, text=True)
        seen = sample_strip_during(proc)
        proc.communicate(timeout=20)
        lifted = [p for p in seen if p.get("lifted") == dragged]
        on_top = all(
            next((c["z"] for c in p["cards"] if c["stage"] == dragged), 0) > max([c["z"] for c in p["cards"] if c["stage"] != dragged] or [0])
            for p in lifted)
        check(bool(lifted) and on_top, f"the dragged card is above every other card while dragged and gliding ({len(lifted)} of {len(seen)} samples)")
        # The icon against the front thumbnail's left edge (its turn's axis, which the
        # perspective leaves in place), in units of that edge's length (the hover scale
        # grows both alike).
        rel = []
        for p in lifted:
            c = next((c for c in p["cards"] if c["stage"] == dragged), None)
            if c and c.get("thumb_left_edge") and c.get("icons"):
                (tx, ty), (bx, by) = [(q["x"], q["y"]) for q in c["thumb_left_edge"]]
                ic, h = c["icons"][0], max(by - ty, 1.0)
                rel.append(((ic["x"] - bx) / h, (ic["y"] - by) / h, h))
        if check(len(rel) >= 3, f"the dragged card's thumbnail and icon were sampled ({len(rel)} samples)"):
            dev = max(max(abs(r[0] - rel[0][0]), abs(r[1] - rel[0][1])) * r[2] for r in rel)
            check(dev <= 1.0, f"its icon keeps its place on the thumbnail through the drag and the glide (largest drift {dev:.2f} pt)")
        time.sleep(0.5)
        st = state()
        n = numbers_by_id(st).get(dragged)
        check(n in st["strip"] and st["strip"].index(n) == 3, f"it took slot 3 ({st['strip']}, the dragged card is {n})")

    pixels = bool(os.environ.get("SM_PIXELS"))
    if not pixels:
        say("-- (SM_PIXELS unset: the checks read back from the overlay's pixels are skipped; they capture our own overlay window)")
    say("-- a window dragged over the strip: its card is under the pointer (top and bottom quarter)")
    for frac in ((0.25, 0.75) if pixels else ()):
        s = state()
        shown = [w for w in s["windows"] if w["home"] == s["active"] and not w["parked"] and w["role"] == "standard"]
        if not shown:
            do("switch", s["strip"][0])
            time.sleep(1.0)
            s = state()
            shown = [w for w in s["windows"] if w["home"] == s["active"] and not w["parked"] and w["role"] == "standard"]
        if not check(bool(shown), f"a shown window to drag ({frac})"):
            continue
        f = shown[0]["restore"]
        grab = (f["x"] + 300, f["y"] + 12)
        target = (vis["x"] + 60, vis["y"] + vis["h"] * frac)
        words = [str(round(v, 1)) for p in (grab, (grab[0] - 60, grab[1]), target) for v in p] + ["hold=1500"]
        proc = subprocess.Popen([BIN, "ctl", "debug-drag", *words], env=ENV, stdout=subprocess.PIPE, text=True)
        # The pointer moves 4 pt every 8 ms after a 120 ms press: wait until it is there.
        travel = abs(grab[0] - 60 - target[0]) + abs(grab[1] - target[1]) + 60
        time.sleep(0.12 + travel / 4 * 0.009 + 0.25)
        region = [vis["x"], vis["y"], s["strip_width"] + 120, vis["h"]]
        got = {}
        while proc.poll() is None:
            g = ctl("debug-overlay", *region)
            if g.get("bounds"):
                got = g
        proc.communicate(timeout=20)
        b = got.get("bounds")
        # Under the pointer, held near its top (grabbed by the title bar). Below the display's
        # middle the turned card's far corner rises toward the eye, so its box starts a little
        # above the held edge: the top quarter, not an exact point.
        ok = bool(b) and inside(target, b, 4) and 0 <= (target[1] - b["y"]) / b["h"] <= 0.25
        check(ok, f"{int(frac * 100)} % down: the dragged window's card is drawn under the pointer "
                  f"(pointer {target[0]:.0f},{target[1]:.0f}; card {b and fmt(b)}; {got.get('error', '')})")
        time.sleep(1.0)

    say("-- slowed down: a window's snapshot lands exactly on the window")
    with open(os.path.join(HOME, "config.toml"), "w") as fh:
        fh.write(CONFIG_SLOW)
    ctl("reload")
    time.sleep(1.0)
    s = state()
    if pixels and s.get("strip_cards"):
        slot = 0
        target = s["strip"][slot]
        incoming = [w for w in s["windows"] if w["home"] == target and w["role"] == "standard"]
        if check(bool(incoming), "a card with a window to bring back"):
            win = incoming[0]
            r = win["restore"]
            region = [r["x"] - 40, r["y"] - 40, r["w"] + 80, r["h"] + 80]
            ctl("debug-click", *center(s["strip_cards"][slot]["frame"]))
            last, end = None, time.time() + 8
            while time.time() < end:
                got = ctl("debug-overlay", *region)
                if got.get("bounds"):
                    last = got["bounds"]
                elif last and got.get("ok"):
                    break
                time.sleep(0.05)
            b = cg_bounds(win["id"])
            check(bool(last) and b is not None and off(last, b) <= 3.0,
                  f"the growing snapshot's last frame drawn is the window's frame (drawn {last and fmt(last)}, window {b and fmt(b)})")
            t = state().get("last_transition") or {}
            g = next((g for g in t.get("ghosts", []) if g["window"] == win["id"]), None)
            check(g is not None and g.get("real") and b and off(g["end"], b) <= 1.0,
                  f"its last frame is within 1 pt of the window's (end {g and fmt(g['end'])}, window {b and fmt(b)})")
    stop_app()
    all_onscreen("slots quit")
    write_config(None)
    stop_all_fixtures()
# ----- capture: the one launch with window captures on -----------------------------------


def phase_capture():
    """The only app run with window captures on: thumbnails and switch snapshots are real
    pictures, and a quit with captures running waits for those in flight (an exit with one
    in flight makes macOS alert about screen access). The only phase that may raise that
    alert: on the first run from a new app path, which macOS approves once."""
    say("\n== capture: real thumbnails and snapshots (the only app run with captures on) ==")
    stop_app()
    stop_fixture()
    p = os.path.join(HOME, "state.json")
    if os.path.exists(p):
        os.remove(p)
    start_fixture(3)
    s = start_app(3, extra_env={"STAGE_MANAGER_TEST": "1"}, capture=True)
    if not check(s is not None, "app up with captures on"):
        return
    check(s.get("capture", {}).get("mode") == "on", f"captures are on ({s.get('capture')})")
    t = by_title(s)
    # Fixture 3 stays (clear of the strip, so it shows); 1 and 2 make the one card.
    for n in (1, 2):
        do("send", 2, t[f"Fixture {n}"]["id"])
    s = wait_until(lambda: (lambda x: x if x.get("capture", {}).get("images", 0) >= 2 else None)(state()),
                   10, "window pictures")
    cap = state().get("capture", {})
    check(s is not None and cap.get("gate") == "open",
          f"window pictures arrive and captures stay open ({cap})")
    for n in (2, 1):
        do("switch", n)
        time.sleep(1.2)
        consistent(f"a switch to stage {n} with real snapshots")
    cap = state().get("capture", {})
    check(cap.get("images", 0) > (s or {}).get("capture", {}).get("images", 0),
          f"pictures keep coming ({cap.get('images')} so far)")
    stop_app()
    text = log_text()
    check("bye" in text and "window captures on" in text, "a clean quit after captures ran")
    check("refused window captures" not in text, "macOS did not refuse a capture")
    all_onscreen("capture quit")


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


def captures_on():
    """Screen captures (`screencapture`) only when $SM_SHOTS or $SM_SCREENSHOTS is set: a
    run otherwise raises macOS's screen-capture alert on the owner's Mac."""
    return bool(os.environ.get("SM_SHOTS") or os.environ.get("SM_SCREENSHOTS"))


def screenshot(name, s):
    if not captures_on():
        say(f"  (screenshots off: {name} not taken; SM_SCREENSHOTS=<dir> takes it)")
        return
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
    # A long hide delay: the strip a tab click reveals stays while the polls (each a probe
    # subprocess) look, and while a click goes through its empty column.
    write_config('[windows]\nfit = "move"\n[reveal]\nedge_tab = true\nlaunch_reveal = true\nhide_delay_ms = 4000\n')
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
        clicked = wait_until(lambda: "edge tab clicked" in log_text(), 2, "the tab's click in the log")
        check(clicked is not None, "the tab got the click (the log says so)")
        gone = wait_until(lambda: edge_tab() is None, 3, "the tab to give way to the strip")
        check(gone is not None, "a click on the tab reveals the strip (the tab hides)")
        shown = (state().get("strip_panel") or {}).get("shown")
        say(f"  strip after the tab click: shown {shown}")
        if not shown:
            # The click-through check needs the strip shown over Alpha 1: reveal it the way
            # the motion phase does (the pointer resting at the left edge), not through the
            # tab whose check just failed.
            vis_now = state()["display"]["visible"]
            ctl("debug-hover", vis_now["x"] + 1, vis_now["y"] + vis_now["h"] / 2, 900)
            shown = wait_until(lambda: (state().get("strip_panel") or {}).get("shown"), 3, "the strip revealed at the edge")
            say(f"  strip revealed at the edge for the click-through check: shown {bool(shown)}")
        # R1-ui 8: the strip, lifted over Alpha 1 (which covers the column), has no card (one
        # stage); a click in its empty column goes through to Alpha 1, never to the strip.
        a1 = live_frame("Alpha 1")
        pt = (vis["x"] + 100, min(a1["y"] + a1["h"] - 40, vis["y"] + vis["h"] - 60))
        if shown and a1["x"] <= pt[0] - 20 and a1["y"] <= pt[1] - 20:
            mark = len(log_text())
            r = ctl("debug-click", *pt)
            through = r.get("ok") and wait_until(lambda: "mouse down in another app" in log_text()[mark:], 2, "the click to reach Alpha 1")
            check(bool(through), f"a click on the strip's empty column goes through to the window below ({r.get('error', 'ok')})")
        else:
            check(False, f"the strip shows over Alpha 1 for the click-through check (shown {shown}, Alpha 1 {fmt(a1)})")
        back = wait_until(edge_tab, 8, "the tab to come back once the pointer is away")
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
    archive_log()
    args = ["open", "-n", APP]
    for k, v in {"STAGE_MANAGER_HOME": HOME, "STAGE_MANAGER_ONLY": ONLY, "STAGE_MANAGER_NO_PROMPT": "1",
                 "STAGE_MANAGER_NO_CAPTURE": "1", "STAGE_MANAGER_TEST": "1",
                 "RUST_LOG": "info,stage_manager=debug"}.items():
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


# ----- expose: the strip hides in Mission Control and App Exposé -------------------------

# NSWindowCollectionBehavior bits (src/ui/panels.rs).
BEHAVIOR = {"can_join_all_spaces": 1, "move_to_active_space": 2, "managed": 4, "transient": 8, "stationary": 16}
# A card pixel counts as changed when a channel moved by more than this (0-255).
PIXEL_STEP = 32
# Mission Control's own animation (about half a second), with room.
EXPOSE_SETTLE = 1.2


def behavior_names(bits):
    return [n for n, b in BEHAVIOR.items() if bits & b]


def panes_hide_in_expose(s):
    """Every strip pane is Transient (hidden by Mission Control, App Exposé and Show Desktop),
    never Stationary, and joins no other Space (no Spaces flag): (ok, description)."""
    panes = s.get("strip_panels") or []
    bad = [p for p in panes if not p["behavior"] & BEHAVIOR["transient"]
           or p["behavior"] & (BEHAVIOR["stationary"] | BEHAVIOR["can_join_all_spaces"] | BEHAVIOR["move_to_active_space"])]
    told = [f"space {p['space']}: {behavior_names(p['behavior'])}" for p in panes]
    return bool(panes) and not bad, "; ".join(told) or "no pane"


def screen_overlays():
    """On-screen windows above or below the windows' layer that cover the main display
    (`probe-screen`): Mission Control and App Exposé show one of the Dock's."""
    r = run([BIN, "probe-screen"], timeout=10)
    try:
        return json.loads(r.stdout).get("overlays", [])
    except (json.JSONDecodeError, AttributeError):
        return []


def read_bmp(path):
    """(width, height, top-down rows, bytes per pixel) of an uncompressed or bitfields BMP,
    as `screencapture -t bmp` writes it (32 bits a pixel)."""
    with open(path, "rb") as f:
        data = f.read()
    if data[:2] != b"BM":
        raise ValueError(f"{path} is not a BMP")
    offset, = struct.unpack_from("<I", data, 10)
    width, height = struct.unpack_from("<ii", data, 18)
    bpp, = struct.unpack_from("<H", data, 28)
    if bpp not in (24, 32):
        raise ValueError(f"{path}: {bpp} bits a pixel")
    stride = ((bpp * width + 31) // 32) * 4
    rows = [data[offset + i * stride: offset + i * stride + width * bpp // 8] for i in range(abs(height))]
    if height > 0:  # bottom-up
        rows.reverse()
    return width, abs(height), rows, bpp // 8


def grab(region, name):
    """The screen in `region` (global points, x y w h) as pixels; None when it fails or
    captures are off (`captures_on`: the pixel checks are skipped then). The file goes to
    the test home (never the repo) and is removed once read."""
    if not captures_on():
        return None
    path = os.path.join(HOME, f"expose-{name}.bmp")
    args = ["screencapture", "-x", "-t", "bmp", "-R", ",".join(f"{v:.0f}" for v in region), path]
    r = subprocess.run(args, capture_output=True, text=True)
    if r.returncode != 0 or not os.path.exists(path):
        say(f"  screen capture failed: {r.stderr.strip()}")
        return None
    try:
        return (region, read_bmp(path))
    except (OSError, ValueError, struct.error) as e:
        say(f"  screen capture unreadable: {e}")
        return None
    finally:
        os.remove(path)


def card_region(s):
    """The cards' frames (global points) and the box around them."""
    cards = [c["frame"] for c in s.get("strip_cards") or []]
    if not cards:
        return [], None
    x0, y0 = min(c["x"] for c in cards), min(c["y"] for c in cards)
    x1, y1 = max(c["x"] + c["w"] for c in cards), max(c["y"] + c["h"] for c in cards)
    return cards, (x0, y0, x1 - x0, y1 - y0)


def changed_fraction(a, b, cards):
    """The share of the cards' pixels (every 2nd pixel each way) that differ between two
    grabs of the same region."""
    region, (w, h, rows_a, bp) = a
    _, (w2, h2, rows_b, _) = b
    if (w, h) != (w2, h2):
        return 1.0
    scale = w / region[2]
    total = changed = 0
    for c in cards:
        px0, px1 = int((c["x"] - region[0]) * scale), int((c["x"] + c["w"] - region[0]) * scale)
        py0, py1 = int((c["y"] - region[1]) * scale), int((c["y"] + c["h"] - region[1]) * scale)
        for y in range(max(py0, 0), min(py1, h), 2):
            ra, rb = rows_a[y], rows_b[y]
            for x in range(max(px0, 0), min(px1, w), 2):
                i = x * bp
                total += 1
                if any(abs(ra[i + k] - rb[i + k]) > PIXEL_STEP for k in range(3)):
                    changed += 1
    return changed / total if total else 0.0


def make_fixture_front(s):
    """A shown fixture window raised and its app made frontmost (through Accessibility), so a
    stray Escape could only ever reach a fixture. True when a fixture is frontmost."""
    if frontmost_bundle() in ALL_IDS:
        return True
    shown = [w for w in s["windows"] if not w["parked"] and w["role"] == "standard"]
    if shown:
        r = shown[0]["restore"]
        ctl("debug-frame", shown[0]["id"], r["x"], r["y"], r["w"], r["h"])
        time.sleep(0.6)
    return frontmost_bundle() in ALL_IDS


def expose_round(which, label, shot=None):
    """Mission Control (`which` = mission-control) or App Exposé (app-windows) through the
    app (`ctl debug-expose`, the user's own shortcut), never a Desktop change: our panes are
    not drawn while it shows (the window server no longer lists them on screen, or lists them
    transparent), and after Escape the strip is back on the same Desktop, drawn as before (the
    cards' pixels), and a card click switches. The cards' pixels while it shows are only
    reported: Mission Control dims the desktop, so they change about 90 % whether the strip
    is hidden or not (measured 10-12 with the old Stationary panes: listed on screen, alpha 1,
    88 % changed); the window server's listing is what tells. `shot`
    (name -> path or None) takes a by-eye screenshot while it shows. Returns True when the
    round ran to the end."""
    say(f"\n  -- {label} --")
    s = state()
    sp_before = ctl("space")
    space_before = sp_before.get("space")
    mark = len(log_text())
    panes_before = [(p["space"], p["active"], p["on_active_space"]) for p in s.get("strip_panels") or []]
    panel = strip_panel()
    if not check(panel is not None and (s.get("strip_panel") or {}).get("shown"), f"{label}: the strip shows first"):
        return False
    cards, region = card_region(s)
    if not make_fixture_front(s):
        check(False, f"{label}: a fixture is frontmost (so Escape reaches only a fixture); front is {frontmost_bundle()}")
        return False
    before = grab(region, "before") if region else None
    time.sleep(0.5)
    again = grab(region, "again") if region else None
    base = changed_fraction(before, again, cards) if before and again else 0.0
    overlays_before = {o["id"] for o in screen_overlays()}
    r = ctl("debug-expose", which)
    if not r.get("ok") and which == "mission-control" and "turned off" in str(r.get("error")):
        # The shortcut is off in Keyboard Shortcuts: Mission Control's own launcher.
        say(f"  {r.get('error')}; opening Mission Control (com.apple.exposelauncher) instead")
        r = {"ok": subprocess.run(["open", "-b", "com.apple.exposelauncher"]).returncode == 0, "key": "exposelauncher"}
    if not check(r.get("ok"), f"{label}: shortcut typed ({r.get('key', r.get('error'))})"):
        return False
    seen = wait_until(lambda: [o for o in screen_overlays() if o["id"] not in overlays_before], 3, f"{label} to show")
    say(f"  {label} {'seen: ' + str([(o['owner'], o['layer']) for o in seen]) if seen else 'not seen in the window list; going on after the wait'}")
    time.sleep(EXPOSE_SETTLE)
    listed = strip_panel()
    during = grab(region, "during") if region else None
    if shot:
        shot(which)
    moved = changed_fraction(before, during, cards) if before and during else None
    say(f"  strip while it shows: listed on screen {listed is not None}"
        + (f" (alpha {listed.get('alpha')})" if listed else "")
        + (f"; cards' pixels changed {moved:.0%} (still screen {base:.0%})" if moved is not None else "; no pixel check (" + ("no cards" if captures_on() else "screen captures off; SM_SHOTS=<dir> turns them on") + ")"))
    hidden = listed is None or listed.get("alpha", 1.0) == 0
    check(hidden, f"{label}: the strip is not drawn while it shows (the window server "
          f"{'no longer lists it on screen' if listed is None else 'lists it on screen, alpha ' + str(listed.get('alpha'))})")
    during_sp = ctl("space")
    check(during_sp.get("inference_frozen") is True,
          f"{label}: the app sees it and infers no Space meanwhile (overview {during_sp.get('overview')!r})")
    ctl("debug-expose", "escape")
    if seen:
        wait_until(lambda: not [o for o in screen_overlays() if o["id"] not in overlays_before], 5, f"{label} to end")
    time.sleep(EXPOSE_SETTLE)
    sp_after = ctl("space")
    check(sp_after.get("space") == space_before, f"{label}: the same Desktop after Escape (Space {space_before})")
    # Two once-a-second checks past the freeze's settle: a re-recognition would be in by now.
    time.sleep(1.5)
    text = log_text()[mark:]
    sp_after = ctl("space")
    check(sp_after.get("space") == space_before and sp_after.get("changes") == sp_before.get("changes")
          and "recognizing the Space again" not in text and not sp_after.get("inference_frozen"),
          f"{label}: the Desktop is never recognized again, and inference resumes after it (Space {space_before} "
          f"then {sp_after.get('space')}, changes {sp_before.get('changes')} then {sp_after.get('changes')}, "
          f"frozen {sp_after.get('inference_frozen')})")
    back = wait_until(strip_panel, 3, "the strip listed on screen again")
    s = state()
    p = s.get("strip_panel") or {}
    check(back is not None and p.get("on_active_space") and p.get("shown"),
          f"{label}: the strip is back on the Desktop shown ({p})")
    panes_after = [(q["space"], q["active"], q["on_active_space"]) for q in s.get("strip_panels") or []]
    check(panes_after == panes_before, f"{label}: every Desktop's pane where it was ({panes_before} then {panes_after})")
    if before:
        after = grab(region, "after")
        diff = changed_fraction(before, after, cards) if after else 1.0
        check(diff <= base + 0.15, f"{label}: the cards are drawn as before ({diff:.0%} changed, still screen {base:.0%})")
    if cards:
        target = s["strip_cards"][0]
        f = target["frame"]
        was = s["active"]
        # Leaving Mission Control, macOS may keep another process's window (the desktop
        # widgets') over the card for a moment; the app refuses to click there, so try again.
        end, refused = time.time() + 4, []
        while True:
            r = ctl("debug-click", f["x"] + f["w"] / 2, f["y"] + f["h"] / 2)
            if r.get("ok") or "not ours" not in str(r.get("error")) or time.time() >= end:
                break
            refused.append(round(time.time() - end + 4, 1))
            time.sleep(0.4)
        if refused:
            say(f"  the card was covered by another process's window for a moment (refused at {refused} s): {r.get('error', 'then clear')}")
        s2 = wait_state(lambda x: x["active"] != was, f"{label}: the card click to switch", timeout=4)
        check(r.get("ok") and s2 is not None, f"{label}: a card click switches afterwards ({r.get('error', 'ok')})")
        if s2:
            ctl("switch", was)
            time.sleep(1.0)
    return True


def show_desktop_round():
    """macOS's own Show Desktop (`ctl debug-expose show-desktop`, the user's shortcut; the same
    reveal as a wallpaper click with "Click wallpaper to reveal desktop: Always", his default)
    slides every window aside. Reports what the window server lists for the shown fixture
    windows meanwhile (`kCGWindowIsOnscreen` and frames, review R2 V8), and checks the Desktop
    is never recognized again (no Space change counted, the same Space id, no re-recognition
    in the log) while it lasts, the same shortcut ends it, and every window is back."""
    label = "Show Desktop"
    say(f"\n  -- {label} (macOS's own reveal) --")
    s = state()
    sp = ctl("space")
    shown = [w for w in s["windows"] if not w["parked"] and w["role"] == "standard"]
    if not make_fixture_front(s):
        check(False, f"{label}: a fixture is frontmost; front is {frontmost_bundle()}")
        return
    mark = len(log_text())
    r = ctl("debug-expose", "show-desktop")
    if not r.get("ok") and "turned off" in str(r.get("error")):
        say(f"  skipped: {r.get('error')}")
        return
    if not check(r.get("ok"), f"{label}: shortcut typed ({r.get('key', r.get('error'))})"):
        return
    time.sleep(1.5)
    wins, _ = fixture_windows([FIX_ID])
    by_id = {w["id"]: w for w in wins}
    for w in shown:
        cg = by_id.get(w["id"])
        say(f"  {w['title']} during {label}: " + (f"listed on screen {cg['onscreen']}, at {fmt(cg['bounds'])} (restore {fmt(w['restore'])})" if cg else "not listed"))
    say(f"  strip pane during {label}: listed on screen {strip_panel() is not None}")
    during_sp = ctl("space")
    check(during_sp.get("inference_frozen") is True and during_sp.get("overview") == "Show Desktop",
          f"{label}: the app sees it and infers no Space meanwhile (overview {during_sp.get('overview')!r})")
    # Two once-a-second checks and the settle: a missed Space change would be counted by now.
    time.sleep(3.0)
    after = ctl("space")
    text = log_text()[mark:]
    check(after.get("space") == sp.get("space") and after.get("changes") == sp.get("changes")
          and "recognizing the Space again" not in text and "windows left this Desktop" not in text,
          f"{label}: the Desktop is never recognized again while it shows (Space {sp.get('space')} then {after.get('space')}, changes {sp.get('changes')} then {after.get('changes')})")
    ctl("debug-expose", "show-desktop")
    time.sleep(1.5)
    def all_back():
        now = {w["id"]: w for w in fixture_windows([FIX_ID])[0]}
        return all(now.get(w["id"], {}).get("onscreen") and close(now[w["id"]]["bounds"], w["restore"]) for w in shown)

    back = wait_until(all_back, 5, "the windows back")
    if not check(back is not None, f"{label}: the same shortcut ends it, every shown window back at its frame"):
        ctl("debug-expose", "show-desktop")
        time.sleep(1.5)
    check(ctl("space").get("space") == sp.get("space"), f"{label}: the same Desktop afterwards")
    consistent(f"after {label}")


def phase_expose():
    say("\n== expose: the strip hides in Mission Control and App Exposé, and comes back ==")
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
    # Stage 1 keeps only Fixture 3 (clear of the strip, so the strip stays shown); the others
    # go to stage 2, the single card.
    for n in (1, 2, 4):
        do("send", 2, t[f"Fixture {n}"]["id"])
    s = wait_state(lambda x: (x.get("strip_panel") or {}).get("shown") and x.get("strip_cards"), "the strip with its card", 4)
    if not check(s is not None, "the strip shows one card"):
        return
    ok, told = panes_hide_in_expose(s)
    check(ok, f"every strip pane is Transient, not Stationary, on one Desktop ({told})")
    for which, label in (("mission-control", "Mission Control"), ("app-windows", "App Exposé")):
        if not expose_round(which, label):
            break
    consistent("after Mission Control and App Exposé")
    show_desktop_round()
    stop_app()
    all_onscreen("expose quit")


def frontmost_bundle():
    """The frontmost app's bundle ID. `lsappinfo info … front` prints nothing on this macOS,
    so the front app's ASN is asked first; the reply is `bundleID="…"` (or `"CFBundleIdentifier"="…"`)."""
    asn = subprocess.run(["lsappinfo", "front"], capture_output=True, text=True).stdout.strip()
    if not asn:
        return None
    out = subprocess.run(["lsappinfo", "info", "-only", "bundleid", asn], capture_output=True, text=True).stdout
    m = re.search(r'(?:bundleID|"CFBundleIdentifier")\s*=\s*"([^"]+)"', out)
    return m.group(1) if m else None


def replayd_alerts(since):
    """How many screen-access alerts macOS (replayd) showed since `since` (any app), or
    None when the log cannot be read."""
    r = subprocess.run(["log", "show", "--style", "compact", "--start", since, "--predicate",
                        'process == "replayd" AND eventMessage CONTAINS "showAlertWithTitle"'],
                       capture_output=True, text=True)
    if r.returncode != 0:
        return None
    return sum(1 for line in r.stdout.splitlines() if line.rstrip().endswith("]:140"))


# ----- restore after a restart (design 8D, round 8E's phase) ------------------------------


def start_extra_args(k, n, *args, wait=True):
    """Starts fixture app `k` with `n` windows and extra fixture arguments (`--delay-ms`,
    `--stagger-ms`, `--reverse`, `--retitle`...); waits for its windows unless `wait` is off."""
    bundle, path, pattern, title = EXTRA[k]
    stop_extra(k)
    subprocess.run(["open", "-n", "-g", path, "--args", "--windows", str(n), "--title", title, "--background", *args], check=True)
    if wait:
        return wait_until(lambda: len(fixture_windows([bundle])[0]) >= n, 15, f"{n} {title} windows")
    return True


def read_state_file():
    try:
        with open(os.path.join(HOME, "state.json"), encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def saved_windows(f):
    """(title, pending since or None) of every saved window in a state.json."""
    return [(w["title"], (w.get("pending") or {}).get("since"))
            for sp in (f or {}).get("spaces", []) for st in sp["stages"] for w in st["windows"]]


def base_title(t):
    """A title without the fixture's retitle suffix."""
    return t.replace(" — Edited", "")


def number_of(s, title):
    for w in s["windows"]:
        if base_title(w["title"]) == title:
            return w["home"]
    return None


def stage_sets_restore(s, n):
    """The base titles of stage `n`'s windows."""
    return {base_title(x) for x in stage_titles(s).get(n, [])}


def phase_restore():
    say("\n== restore after a restart (simulated: STAGE_MANAGER_SESSION) ==")
    stop_app()
    stop_all_fixtures()
    write_config('[stages]\nnames = ["Mail"]\n')
    try:
        os.remove(os.path.join(HOME, "state.json"))  # the test home's: no other phase's layout
    except FileNotFoundError:
        pass
    run_id = f"{os.getpid()}-{int(time.time())}"
    session = {k: {"STAGE_MANAGER_TEST": "1", "STAGE_MANAGER_SESSION": f"restore {k} {run_id}"} for k in "ABC"}

    say("-- session A: a layout of three apps")
    start_extra("a", 3)
    start_extra("b", 2)
    start_extra("c", 1)
    s = start_app(6, extra_env=session["A"])
    if not check(s is not None, "app up with Alpha (3), Bravo (2) and Charlie (1)"):
        return
    t = by_title(s)
    do("send", 1, t["Alpha 1"]["id"])
    do("send", 1, t["Alpha 2"]["id"])
    do("send", number_of(state(), "Alpha 3"), t["Bravo 1"]["id"])
    do("pin", t["Bravo 2"]["id"])
    do("switch", number_of(state(), "Alpha 3"))
    s = state()
    want = {n: set(v) for n, v in stage_titles(s).items() if v}
    want_active = set(stage_titles(s)[s["active"]])
    want_restore = {w["title"]: w["restore"] for w in s["windows"] if w["role"] == "standard"}
    want_strip = [set(stage_titles(s)[n]) for n in s["strip"]]
    want_strip_n = list(s["strip"])
    active_a = next(st for st in s["stages"] if st["number"] == s["active"])
    want_key = next((w["title"] for w in s["windows"] if w["id"] == active_a["key"]), None)
    state_a = s
    say(f"  stages: {want}, active {sorted(want_active)}, strip {want_strip}")
    check(by_title(s)["Bravo 2"]["home"] == "pinned" and want.get(1) == {"Alpha 1", "Alpha 2"},
          "layout: Mail holds Alpha 1 and 2, Bravo 2 is pinned")
    time.sleep(1.0)
    stop_app()
    all_onscreen("session A quit")
    f = read_state_file()
    saved_space = next((sp["id"] for sp in (f or {}).get("spaces", []) if sp.get("current")), None)
    check(f is not None and f.get("session") == session["A"]["STAGE_MANAGER_SESSION"] and saved_space is not None,
          "state.json written in session A, with the Desktop shown")
    check(all(p is None for _, p in saved_windows(f)) and len(saved_windows(f)) == 6, "nothing pending in session A")

    say("-- session B: the app starts on an empty screen; Alpha already reopened two windows minimized")
    stop_all_fixtures()
    # Minimized before the app starts: AppKit minimizes only a window that has been on screen,
    # so the fixture shows each a moment first, and that moment must not be a new window to
    # the app. Alpha's third window opens later (SIGUSR1: "Alpha 3", its session A frame).
    start_extra_args("a", 2, "--minimized", "Alpha 1;Alpha 2", wait=False)
    a_bundle = EXTRA["a"][0]
    def minimized_both():
        wins = fixture_windows([a_bundle])[0]
        return len(wins) == 2 and not any(w.get("onscreen") for w in wins)
    wait_until(minimized_both, 10, "Alpha 1 and 2 in the Dock")
    time.sleep(1.5)  # past the moment on screen and the minimize animation
    check(wait_until(minimized_both, 5, "Alpha 1 and 2 in the Dock, settled") is not None,
          "Alpha 1 and 2 reopened minimized (the window server lists them off screen)")
    s = start_app(0, extra_env=session["B"])
    if not check(s is not None, "app up in session B"):
        return
    r = s.get("restore") or {}
    check(r.get("restoring") is True and r.get("after_restart") is True, "restore after a restart (state.json from another session)")
    check(s["space"] in r.get("provisional", []), "the empty Desktop is provisional")
    check(s["strip"] == [] and not s["windows"], "no card, no window")
    s = wait_state(lambda x: len(x["unplaced"]) == 2, "the two minimized windows known, off screen", timeout=5)
    check(s is not None, "the app knows Alpha 1 and 2 (Accessibility lists them) and leaves them in the Dock (unplaced, unmanaged)")
    check(r.get("pending_unvisited") == 6, f"every saved window waits ({r})")
    f = wait_until(lambda: (lambda x: x if x and x.get("session") == session["B"]["STAGE_MANAGER_SESSION"] else None)(read_state_file()),
                   10, "state.json saved in session B")
    pend = saved_windows(f)
    check(len(pend) == 6 and all(p == session["A"]["STAGE_MANAGER_SESSION"] for _, p in pend),
          f"state.json keeps every saved window, pending since session A ({pend})")

    say("-- the apps reopen their windows late: Alpha 3, then Bravo one by one, reversed and retitled")
    say("   (hands off the Mac: a click or key press would end the restore window early)")
    mail = number_of(state_a, "Alpha 1")
    charlie_n = number_of(state_a, "Charlie 1")
    os.kill(extra_pid("a"), signal.SIGUSR1)
    start_extra_args("b", 2, "--delay-ms", "8000", "--stagger-ms", "1500", "--reverse", "--retitle-after-ms", "500",
                     "--retitle", "Bravo 1=Bravo 1 — Edited", wait=False)
    shown_seen = []
    end = time.time() + 40
    s = None
    while time.time() < end:
        s = state()
        shown_seen.append({base_title(x) for x in active_titles(s)})
        # Alpha 3, Bravo 1 and 2 managed; the two minimized Alpha windows wait off screen.
        if len([w for w in s["windows"] if w["role"] == "standard"]) >= 3 and len(s["unplaced"]) <= 2 \
                and any(w["title"] == "Bravo 1 — Edited" for w in s["windows"]):
            time.sleep(1.5)  # the last moves
            s = state()
            break
        time.sleep(0.3)
    if not check(s is not None and len(s["windows"]) >= 3, "the three reopened windows shown are managed"):
        return
    got = {n: {base_title(x) for x in v} for n, v in stage_titles(s).items() if v}
    back = {n: v for n, v in want.items() if n not in (mail, charlie_n)}
    check(got == back, f"every shown window is back in its saved stage, under its saved number ({got}, saved {want})")
    check(s["space"] == saved_space, f"the Desktop took the saved Desktop's id ({s['space']}, saved {saved_space})")
    check(not (s.get("restore") or {}).get("provisional"), "no Desktop is provisional any more")
    bad_shown = [x for x in shown_seen if x and not x <= want_active]
    check(not bad_shown, f"the active stage was never switched away while windows came back ({bad_shown[:3]})")
    check({base_title(x) for x in active_titles(s)} == want_active, "the saved active stage is active")
    pinned = by_title(s).get("Bravo 2") or {}
    check(pinned.get("home") == "pinned", "Bravo 2 is pinned again")
    off = [w["title"] for w in s["windows"] if w["role"] == "standard"
           and not close(w["restore"], want_restore.get(base_title(w["title"]), w["restore"]), 2.0)]
    check(not off, f"every window has its saved frame ({off})")
    consistent("restored")
    check(any(w["title"] == "Bravo 1 — Edited" for w in s["windows"]), "Bravo 1 was retitled after it was matched")
    waiting = {st["number"]: st for st in s["stages"] if st["pending"] and not st["windows"]}
    check(set(waiting) == {mail, charlie_n}, f"Mail (minimized windows) and Charlie's stage wait, pending ({sorted(waiting)})")
    # A waiting stage has no card (a card is a picture of windows; a minimized window is in the
    # Dock, as at any launch), except a declared one: `[stages] names` cards show with no
    # window, as in v1.2.0 (the 10-15 run's "failure": Mail is declared above).
    check(charlie_n not in s["strip"] and mail in s["strip"],
          f"Charlie's waiting stage has no card; Mail's, declared, has its card ({s['strip']})")
    check((s.get("restore") or {}).get("restoring") is True, "the restore window is still open")

    say("-- un-minimized during the restore window: back to its inactive stage, quietly")
    active_before = s["active"]
    os.kill(extra_pid("a"), signal.SIGHUP)
    s = wait_state(lambda x: "Alpha 1" in by_title(x), "Alpha 1 un-minimized and managed", timeout=8)
    if check(s is not None, "Alpha 1 is managed once un-minimized"):
        time.sleep(1.0)
        s = state()
        a1 = by_title(s)["Alpha 1"]
        # Read off screen, a minimized window answers AXDialog: once shown it is read again.
        check(a1["role"] == "standard", f"a standard window, not taken for a dialog ({a1['role']})")
        check(a1["home"] == mail and a1["parked"], f"in its saved stage (Mail), parked ({a1['home']}, parked {a1['parked']})")
        check(a1.get("restored") in ("strong", "medium"), f"matched with confidence ({a1.get('restored')})")
        check(re.search(rf"restore: back in its saved stage id={a1['id']} .*parked=true switched=false", log_text()) is not None,
              "placed quietly: parked, no switch (the log)")
        check(s["active"] == active_before and {base_title(x) for x in active_titles(s)} == want_active,
              "no switch during the restore window (the login settles: a focus macOS gives the un-minimized window switches nothing)")
        strip_n = [n for n in want_strip_n if n != charlie_n]
        check(s["strip"] == strip_n, f"the strip in its saved order ({s['strip']}, saved {want_strip_n})")
        consistent("un-minimized during")
    s = state()  # also when Alpha 1 never came: the invariants below hold on any state
    wrong = [w["title"] for w in s["windows"] if w["parked"] and w.get("restored") not in ("strong", "medium")]
    check(not wrong, f"parked => matched strong or medium ({wrong})")
    inactive = [w["title"] for w in s["windows"] if w["role"] == "standard" and w["home"] not in ("pinned", s["active"])]
    check(all(by_title(s)[x]["parked"] for x in inactive), f"the inactive stages' windows are parked ({inactive})")

    say("-- settling ends by itself once apps are quiet for 20 s (8F): the saved key window has the keyboard")
    check((s.get("restore") or {}).get("settling") is True, "the login is settling (a restart)")
    # 75 s: quiet 20 s after the last window back (Bravo's stagger, Alpha 1) and a minute after
    # the launch at least, with room for a slow machine.
    s = wait_state(lambda x: (x.get("restore") or {}).get("restoring") is False, "the restore window ends by itself", timeout=75)
    if check(s is not None, "the restore window ended with no user action (apps quiet 20 s)"):
        time.sleep(1.5)  # the end's focus, at the next tick
        s = state()
        text = log_text()
        ends = re.search(r'restore: the restore window ends why="apps went quiet"', text)
        check(ends is not None, "it ended because apps went quiet (the log)")
        # Only session B's restore window counts: from its opening line to its end (the day's
        # log also holds session A, whose `do("switch", …)` switched stages).
        opens = [o for o in re.finditer(r"restore: the restore window opens after_restart=true", text)
                 if ends is not None and o.start() < ends.start()]
        check(bool(opens), "session B's restore window opened after a restart (the log)")
        switches_during = text[opens[-1].end():ends.start()].count("stage switch") if ends and opens else -1
        check(switches_during == 0, f"no stage switch while the login settled ({switches_during} in session B's restore window)")
        check(re.search(r"restore: \d+ windows? into \d+ stages? on \d+ Desktops? in \d+ s; \d+ pending; \d+ by Apple's rule", text) is not None,
              "the restore's one line is logged, no titles")
        summary = run([BIN, "log", "summary"], timeout=15).stdout
        check("last restore after a restart:" in summary, "`log summary` repeats the restore's line")
        check({base_title(x) for x in active_titles(s)} == want_active and s["active"] == active_before, "the saved active stage is in front at the end")
        key_ids = [w["id"] for w in s["windows"] if base_title(w["title"]) == want_key]
        front = next((st for st in s["stages"] if st["number"] == s["active"]), {})
        check(front.get("key") in key_ids, f"its saved key window ({want_key}) is the stage's key")
        check(s.get("focused") in key_ids, f"the saved key window has the keyboard (focused {s.get('focused')}, key {key_ids})")
        consistent("settled")

    say("-- after the restore window: an un-minimized window brings its stage forward")
    do("switch", s["active"])
    s = state()
    check((s.get("restore") or {}).get("restoring") is False, "the restore window is over")
    os.kill(extra_pid("a"), signal.SIGHUP)
    s = wait_state(lambda x: "Alpha 2" in by_title(x), "Alpha 2 un-minimized and managed", timeout=8)
    if check(s is not None, "Alpha 2 is managed once un-minimized"):
        time.sleep(1.0)
        s = state()
        t = by_title(s)
        check(t["Alpha 2"]["role"] == "standard", f"a standard window, not taken for a dialog ({t['Alpha 2']['role']})")
        check(t["Alpha 2"]["home"] == mail and s["active"] == mail, f"its stage (Mail) came forward (active {s['active']})")
        check(not t["Alpha 2"]["parked"] and not t["Alpha 1"]["parked"], "Mail's windows are shown")
        check(t["Alpha 3"]["parked"] and t["Bravo 1 — Edited"]["parked"], "the stage left is parked")
        check(t["Alpha 2"].get("restored") in ("strong", "medium"), f"matched with confidence ({t['Alpha 2'].get('restored')})")
        check(re.search(rf"restore: back in its saved stage id={t['Alpha 2']['id']} .*switched=true", log_text()) is not None,
              "its stage came forward with it (the log)")
        consistent("un-minimized after")

    say("-- an app with no saved window: Apple's rule")
    start_fixture(1)
    s = wait_state(lambda x: "Fixture 1" in by_title(x), "Fixture 1 managed", timeout=8)
    if check(s is not None, "the unmatched app's window is managed"):
        time.sleep(1.0)
        s = state()
        f1 = by_title(s)["Fixture 1"]
        check(f1.get("restored") is None, "matched to no saved window")
        check(stage_titles(s).get(f1["home"]) == ["Fixture 1"] and f1["home"] not in want, "a stage of its own")
        check(s["active"] == f1["home"] and not f1["parked"], "shown, its stage in front, as Apple's does")
        check(stage_sets_restore(s, mail) == {"Alpha 1", "Alpha 2"}, "the restored stages untouched")
        consistent("unmatched app")
    do("switch", mail)
    stop_fixture()
    wait_state(lambda x: "Fixture 1" not in by_title(x), "Fixture 1 gone")

    say("-- give up: Charlie never reopens")
    stop_app()
    all_onscreen("session B quit")
    s = start_app(5, extra_env=session["B"])
    if not check(s is not None, "relaunch in session B"):
        return
    check((s.get("restore") or {}).get("after_restart") is False, "the same session: no restore after a restart")
    def stage_sets_now(x):
        return sorted(sorted(base_title(t) for t in v) for v in stage_titles(x).values() if v)

    want_sets = sorted(sorted(v) for v in want.values() if v != {"Charlie 1"})
    check(stage_sets_now(s) == want_sets, f"the same-session relaunch keeps the stages ({stage_sets_now(s)})")
    check(any(st["pending"] and not st["windows"] for st in s["stages"]), "Charlie's stage still waits")
    time.sleep(1.5)
    stop_app()
    f = read_state_file()
    check(("Charlie 1", session["A"]["STAGE_MANAGER_SESSION"]) in saved_windows(f),
          "Charlie's window is kept through the relaunch, pending since session A")
    s = start_app(5, extra_env=session["C"])
    if not check(s is not None, "launch in session C"):
        return
    f = wait_until(lambda: (lambda x: x if x and x.get("session") == session["C"]["STAGE_MANAGER_SESSION"] else None)(read_state_file()),
                   10, "state.json saved in session C")
    titles = [base_title(x) for x, _ in saved_windows(f)]
    check("Charlie 1" not in titles, "after two sessions without Charlie its window is given up")
    check(sorted(titles) == ["Alpha 1", "Alpha 2", "Alpha 3", "Bravo 1", "Bravo 2"], f"the others are kept ({titles})")
    check(stage_sets_now(s) == want_sets, f"session C: the stages matched again ({stage_sets_now(s)})")
    check(not any(st["pending"] for st in s["stages"]), "session C: nothing pending")
    consistent("session C")
    stop_app()
    all_onscreen("restore phase quit")
    stop_all_fixtures()
    write_config(None)
    restore_after_logout(run_id)


# ----- a logout, simulated (round 8H) -----------------------------------------------------


def post_note(name):
    """Posts the distributed notification `name`: the test's own logout note
    (STAGE_MANAGER_LOGOUT_NOTE), never one of Apple's. The object and userInfo are `$()`
    (nil): JXA's `null` there makes the post silently go nowhere (seen 10-16)."""
    if name.startswith("com.apple."):
        raise ValueError(f"not posting Apple's notification {name}")
    js = ('ObjC.import("Foundation"); $.NSDistributedNotificationCenter.defaultCenter'
          f'.postNotificationNameObjectUserInfoDeliverImmediately({json.dumps(name)}, $(), $(), true);')
    r = subprocess.run(["osascript", "-l", "JavaScript", "-e", js], capture_output=True, text=True)
    return r.returncode == 0


def signal_line(note, then, first=None):
    """The log line of logout signal `note` that led to `then` ("head start", "repeat",
    "resume"); `first`: True for 0 ms since the logout's first signal, False for later."""
    since = r"0" if first else (r"[1-9]\d*" if first is False else r"\d+")
    return re.search(rf'logout signal signal="{re.escape(note)}" cancel=(true|false) since_first_ms={since} .*then="{then}"',
                     log_text())


def quit_entries(f):
    """(title, quit_at) of every saved window in a state.json."""
    return [(w["title"], w.get("quit_at"))
            for sp in (f or {}).get("spaces", []) for st in sp["stages"] for w in st["windows"]]


def layout_of(s):
    """Stage sets (base titles), the active stage's set, and every standard window's frame."""
    sets = sorted(sorted(base_title(t) for t in v) for v in stage_titles(s).values() if v)
    active = sorted(base_title(t) for t in active_titles(s))
    frames = {base_title(w["title"]): w["restore"] for w in s["windows"] if w["role"] == "standard"}
    return sets, active, frames


def same_frames(s, want, tol=2.0):
    """Titles whose frame differs from the one in `want`."""
    return [t for t, f in layout_of(s)[2].items() if t in want and not close(f, want[t], tol)]


def restore_after_logout(run_id):
    """Round 8H: a logout simulated with the test's own note (STAGE_MANAGER_LOGOUT_NOTE): the
    head start at its first signal (every window back, the stages saved), repeats ignored, a
    cancel resuming at once; the apps quit while the logout holds and the file keeps the stages;
    the next session brings each window back into its stage at its saved frame, one reopened
    clamped against the park corner and one at the park spot included. Then the fallback with
    no early signal: the apps quit first, their windows' entries stay in state.json with
    `quit_at`, and the next session restores their stages; in the same session they are gone."""
    say("\n-- a logout, simulated (STAGE_MANAGER_LOGOUT_NOTE): the head start at its first signal")
    note = f"dev.eugene.stagemanager.test.logout.{run_id}"
    env = {k: {"STAGE_MANAGER_TEST": "1", "STAGE_MANAGER_SESSION": f"logout {k} {run_id}", "STAGE_MANAGER_LOGOUT_NOTE": note}
           for k in "DEFG"}
    stop_app()
    stop_all_fixtures()
    write_config(None)
    try:
        os.remove(os.path.join(HOME, "state.json"))
    except FileNotFoundError:
        pass

    # Session D: Alpha (2), Bravo and Charlie, each app in a stage of its own; Alpha's in front.
    start_extra("a", 2)
    start_extra("b", 1)
    start_extra("c", 1)
    s = start_app(4, extra_env=env["D"])
    if not check(s is not None, "session D: app up with Alpha (2), Bravo and Charlie"):
        return
    do("switch", number_of(s, "Alpha 1"))
    s = state()
    want_sets, want_active, want_frames = layout_of(s)
    saved_space = s["space"]
    say(f"  stages {want_sets}, active {want_active}")
    check("logout signals: the test's own note only" in log_text(), "the app listens for the test's note instead of Apple's signals")

    check(post_note(note), "the logout note is posted")
    check(wait_until(lambda: signal_line(note, "head start", first=True), 5, "the head start") is not None,
          "its first signal starts the head start (the log: its name, 0 ms since the first)")
    time.sleep(1.5)
    all_onscreen("the logout's head start")
    held = read_state_file()
    check(held is not None and held.get("session") == env["D"]["STAGE_MANAGER_SESSION"] and len(quit_entries(held)) == 4
          and all(q is None for _, q in quit_entries(held)), "the stages are saved at the head start, every window in them")
    check(post_note(note), "the note again")
    check(wait_until(lambda: signal_line(note, "repeat", first=False), 5, "the repeat") is not None,
          "a repeat is ignored, logged with the ms since the first")

    say("-- a cancel resumes at once: the windows go back to their stages (parked again)")
    check(post_note(note + ".cancelled"), "the cancel note is posted")
    check(wait_until(lambda: signal_line(note + ".cancelled", "resume"), 5, "the resume") is not None, "the cancel resumes at once")
    s = wait_state(lambda x: len(x.get("windows", [])) >= 4 and layout_of(x)[0] == want_sets, "the stages back after the cancel", timeout=10)
    if check(s is not None, "after the cancel the stages are managed again, as saved"):
        time.sleep(1.0)
        check(layout_of(state())[1] == want_active, "the same stage in front")
        consistent("after the cancel")

    say("-- the logout again; the apps quit while it holds")
    check(post_note(note), "the logout note is posted again")
    check(wait_until(lambda: len(re.findall(rf'signal="{re.escape(note)}" cancel=false since_first_ms=0 .*then="head start"', log_text())) >= 2,
                     5, "a second head start") is not None, "a logout after a cancel is a new one: its own head start")
    time.sleep(1.5)
    all_onscreen("the second head start")
    held = read_state_file()
    stop_extra("b")
    stop_extra("c")
    stop_extra("a")
    time.sleep(1.5)
    check(read_state_file() == held, "the apps quit while the logout holds: the file keeps the stages (no save, no stage dropped)")
    stop_app()
    check("quitting after the logout's head start" in log_text(), "macOS's quit (here ctl quit) only exits after the head start")
    check(read_state_file() == held, "the quit writes nothing over it")

    say("-- session E (a restart): the apps reopen, Bravo clamped against the park corner, Charlie at the park spot")
    s = start_app(0, extra_env=env["E"])
    if not check(s is not None, "session E: app up on an empty screen"):
        return
    vis = s["display"]["visible"]
    disp = s["display"]["frame"] if "frame" in s["display"] else vis
    fb, fc = want_frames["Bravo 1"], want_frames["Charlie 1"]
    corner = f"{vis['x'] + vis['w'] - fb['w']:.0f},{vis['y'] + vis['h'] - fb['h']:.0f},{fb['w']:.0f},{fb['h']:.0f}"
    park = f"{disp['x'] + disp['w'] - 1:.0f},{disp['y'] + disp['h'] - 52:.0f},{fc['w']:.0f},{fc['h']:.0f}"
    start_extra_args("a", 2, "--delay-ms", "2000", wait=False)
    start_extra_args("b", 1, "--delay-ms", "3000", "--frames", corner, wait=False)
    start_extra_args("c", 1, "--delay-ms", "4000", "--frames", park, wait=False)
    s = wait_state(lambda x: len([w for w in x["windows"] if w["role"] == "standard"]) >= 4, "the four windows back", timeout=25)
    if check(s is not None, "every reopened window is managed"):
        time.sleep(1.5)
        s = state()
        sets, active, _ = layout_of(s)
        check(sets == want_sets, f"every window back in its stage ({sets}, saved {want_sets})")
        check(active == want_active, f"the saved front stage in front ({active})")
        check(s["space"] == saved_space, "the Desktop took the saved id")
        off = same_frames(s, want_frames)
        check(not off, f"every window at its saved frame, the clamped and the parked one included ({off})")
        wrong = [w["title"] for w in s["windows"] if w["role"] == "standard" and w.get("restored") not in ("strong", "medium")]
        check(not wrong, f"each matched by title and app, whatever its frame ({wrong})")
        consistent("session E")
    stop_app()
    all_onscreen("session E quit")

    say("-- no early signal (the 10-16 restart): the apps quit first, then the app")
    stop_all_fixtures()
    try:
        os.remove(os.path.join(HOME, "state.json"))
    except FileNotFoundError:
        pass
    start_extra("a", 2)
    start_extra("b", 1)
    start_extra("c", 1)
    s = start_app(4, extra_env=env["F"])
    if not check(s is not None, "session F: app up"):
        return
    do("switch", number_of(s, "Alpha 1"))
    s = state()
    want_sets, want_active, want_frames = layout_of(s)
    stop_extra("b")
    stop_extra("c")
    stop_extra("a")
    time.sleep(2.0)  # the debounced save
    f = read_state_file()
    kept = quit_entries(f)
    check(len(kept) == 4 and all(q is not None for _, q in kept), f"each quit app's windows stay in state.json with quit_at ({kept})")
    check(sum(1 for sp in f["spaces"] for st in sp["stages"] if st["windows"]) == 3, "and their three stages")
    check("an app quit: its windows' saved entries stay in state.json" in log_text(), "the log says so")
    stop_app()

    say("-- session G (a restart): the stages come back from the quit entries")
    s = start_app(0, extra_env=env["G"])
    if not check(s is not None, "session G: app up"):
        return
    check(re.search(r"restore: saved windows of apps that quit before the save.* kept=4 dropped=0|kept=4 dropped=0.*restore: saved windows of apps that quit",
                    log_text()) is not None, "the four that quit just before the save wait")
    start_extra_args("a", 2, "--delay-ms", "1500", wait=False)
    start_extra_args("b", 1, "--delay-ms", "2500", wait=False)
    start_extra_args("c", 1, "--delay-ms", "3500", wait=False)
    s = wait_state(lambda x: len([w for w in x["windows"] if w["role"] == "standard"]) >= 4, "the four windows back", timeout=25)
    if check(s is not None, "every reopened window is managed"):
        time.sleep(1.5)
        s = state()
        sets, active, _ = layout_of(s)
        check(sets == want_sets, f"every window back in its stage ({sets}, saved {want_sets})")
        check(active == want_active, f"the saved front stage in front ({active})")
        off = same_frames(s, want_frames)
        check(not off, f"every window at its saved frame ({off})")
        consistent("session G")

    say("-- the same session: an app the user quits and opens again goes by the usual rules")
    stop_extra("b")
    time.sleep(2.0)
    check(("Bravo 1" in [t for t, q in quit_entries(read_state_file()) if q is not None]), "Bravo's entry kept with quit_at")
    stop_app()
    s = start_app(3, extra_env=env["G"])
    if check(s is not None, "relaunch in session G"):
        check(re.search(r"kept=0 dropped=1", log_text()) is not None, "the same session: the quit entry is dropped at the load")
        check(not any(st["pending"] for st in s["stages"]), "nothing waits for Bravo")
        start_extra("b", 1)
        s = wait_state(lambda x: "Bravo 1" in by_title(x), "Bravo 1 managed", timeout=8)
        if check(s is not None, "Bravo reopened"):
            check(by_title(s)["Bravo 1"].get("restored") is None, "matched to no saved window: Apple's rule")
    stop_app()
    all_onscreen("logout sub-phase quit")
    stop_all_fixtures()


def main():
    phases = sys.argv[1:] or ["m3", "safety", "m4", "m6", "restore", "strip", "native", "visible", "parity", "motion", "slots", "expose", "capture"]
    if not os.path.exists(BIN) or not all(os.path.exists(p) for p in [FIX_APP] + [e[1] for e in EXTRA.values()]):
        say("build first: scripts/test-stages.sh builds the app and the fixture")
        return 2
    if pids_of("Stage Manager.app/Contents/MacOS/stage-manager$") and not app_pid():
        say("note: another Stage Manager is running (not ours); it uses another home and is left alone")
    say(f"home: {HOME}")
    since = time.strftime("%Y-%m-%d %H:%M:%S")
    front = frontmost_bundle()
    try:
        for p in phases:
            if owner_app_running():
                say("STOP: another Stage Manager (the owner's) is running; no live phase runs beside it")
                failed.append("the owner's app is running")
                break
            {"visible": phase_visible, "m3": phase_m3, "safety": phase_safety, "m4": phase_m4, "m6": phase_m6, "strip": phase_strip,
             "native": phase_native, "parity": phase_parity, "motion": phase_motion, "slots": phase_slots,
             "expose": phase_expose, "capture": phase_capture, "restore": phase_restore}[p]()
    finally:
        stop_app()
        if pids_of(FIX_EXE) or any(pids_of(e[2]) for e in EXTRA.values()):
            all_onscreen("cleanup")
        stop_all_fixtures()
        if front and front not in ALL_IDS:
            subprocess.run(["open", "-b", front], capture_output=True)
    time.sleep(1.0)
    alerts = replayd_alerts(since)
    say(f"\nscreen-access alerts macOS showed during this run (any app): {'unknown' if alerts is None else alerts}")
    say(f"{len(passed)} passed, {len(failed)} failed (every app run's log: {HOME}/stage-manager.run-*.log and today's {HOME}/YYYY-MM-DD.log)")
    for f in failed:
        say(f"  FAIL {f}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
