#!/usr/bin/env python3
"""Live probe: where Apple's strip takes a Shift-click (engine/apple/points.rs, "Only the drawn
card takes a click").

The window list lists a strip thumbnail at the bounding box of its turned plane; the Apple mode
presses INSIDE pt deep in that plane (engine/perspective.rs turns the box back into it), CLEAR
pt off the thumbnails in front grown by HOVER, and off each app icon's drawn middle only. This
probe settles that mapping in one run: it Shift-clicks fixture thumbnails at the point the
model picks, at points along each edge of the model's plane just inside and just outside, at
its corners (inside the rounded corner and on it), in the box's empty corners, on an icon's
drawn middle and in its window's clear part over the card, at the back window of a stage of
two next to the front one (the miss of the live run of 11-17), and the two windows of that
stage in one Shift press. Each click is fast (Shift down, 12 ms, down and up with no hold,
pointer warped back, 12 ms, Shift up: the app's own; no hover dwell, which relays the strip
out) and alone in its Shift press but for the last trial. After each it records whether the
window joined the stage in front (added), Apple's switched to it (switched), the desktop was
revealed (desktop: the stage in front moved aside) or nothing happened, then puts the
arrangement back (the fixture quit and opened again, the stage in front activated).

The arrangement, fixtures only: Bravo (one window) alone in the strip, Alpha (two windows) a
stage of two in the strip, Charlie in front. Stage Manager itself is not run: Apple's Stage
Manager only.

OFF BY DEFAULT: it turns Apple's Stage Manager on on this Mac, so it runs only with
SM_TEST_TOGGLE_APPLE_SM=1, as scripts/test_apple.py does; the original GloballyEnabled value
is restored on every exit (success, failure, Ctrl-C, kill) and the fixtures are quit. About a
minute and a half. Prints a table and writes it as JSON to a temporary file (named at the end).
Only the fixtures' windows are clicked, each click only where WindowManager's own window is
topmost. Window titles are never read.

  SM_TEST_TOGGLE_APPLE_SM=1 scripts/probe_press_quad.py [--dry]

--dry reads the strip and prints the planned points without clicking.
"""

import atexit
import ctypes
import ctypes.util
import json
import math
import os
import signal
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import test_apple as t  # noqa: E402  (the fixtures, Apple's toggle and its restore)

# ----- the model, as engine/perspective.rs and engine/apple/points.rs have it -------------

MEASURED = [(1800.0, 44.2, 1346.0), (2560.0, 47.9, 2260.0)]
EYE_X = 15.0
INSIDE, CLEAR, HOVER, MARGIN = 6.0, 3.0, 1.11, 2.0
ICON = (66.0, 82.0)


def view(visible_x, visible_w, middle):
    (w0, t0, d0), (w1, t1, d1) = MEASURED
    k = min(max((visible_w - w0) / (w1 - w0), 0.0), 1.0)
    return (math.radians(t0 + (t1 - t0) * k), d0 + (d1 - d0) * k, (visible_x + EYE_X, middle))


def far_ratio(v, u):
    tilt, d, _ = v
    return d / (d + u * math.sin(tilt))


def project(v, x, top, u, h):
    tilt, _, (ex, ey) = v
    f = far_ratio(v, u)
    rx = ex + (x + u * math.cos(tilt) - ex) * f
    ry = lambda y: ey + (y - ey) * f  # noqa: E731
    return [(x, top), (rx, ry(top)), (rx, ry(top + h)), (x, top + h)]


def from_bounds(v, b):
    """The turned plane whose bounding box is b = (x, y, w, h), or None."""
    tilt, d, (ex, ey) = v
    x, y, w, h = b
    if w <= 0 or h <= 0:
        return None
    reach = lambda u: ex + (x + u * math.cos(tilt) - ex) * far_ratio(v, u) - x  # noqa: E731
    lo, hi = 0.0, 8.0 * d
    if reach(hi) < w:
        return None
    for _ in range(60):
        mid = (lo + hi) / 2
        lo, hi = (mid, hi) if reach(mid) < w else (lo, mid)
    u = (lo + hi) / 2
    f = far_ratio(v, u)
    top = y if y <= ey else ey + (y - ey) / f
    bottom = y + h if y + h >= ey else ey + (y + h - ey) / f
    return project(v, x, top, u, bottom - top) if bottom > top else None


def bounds_of(q):
    xs, ys = [p[0] for p in q], [p[1] for p in q]
    return (min(xs), min(ys), max(xs) - min(xs), max(ys) - min(ys))


def rect_quad(r):
    x, y, w, h = r
    return [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]


def depth(q, p):
    """Inside: the distance to the nearest edge; outside: minus at most the distance."""
    area = sum(q[i][0] * q[(i + 1) % 4][1] - q[(i + 1) % 4][0] * q[i][1] for i in range(4))
    if abs(area) < 1e-9:
        return -math.inf
    s = 1 if area > 0 else -1
    out = math.inf
    for i in range(4):
        (ax, ay), (bx, by) = q[i], q[(i + 1) % 4]
        n = math.hypot(bx - ax, by - ay)
        if n > 1e-9:
            out = min(out, s * ((bx - ax) * (p[1] - ay) - (by - ay) * (p[0] - ax)) / n)
    return out


def grown(q, k):
    x, y, w, h = bounds_of(q)
    cx, cy = x + w / 2, y + h / 2
    return [(cx + (a - cx) * k, cy + (b - cy) * k) for a, b in q]


def icon_drawn(r):
    x, y, w, _ = r
    return (x + (w - 44.0) / 2, y + 33.0 - 19.0 - 3.0, 44.0, (41.0 + 19.0 + 3.0) - (33.0 - 19.0 - 3.0))


def drawn(v, kind, r):
    if kind == "thumb":
        q = from_bounds(v, r)
        return grown(q, HOVER) if q else rect_quad(r)
    if kind == "icon":
        return rect_quad(icon_drawn(r))
    return rect_quad(r)


def visible_point(v, screen, box, over):
    own = from_bounds(v, box)
    if not own:
        return None
    covers = [drawn(v, k, r) for k, r in over]
    sq = rect_quad(screen)
    x0, y0, w, h = bounds_of(own)
    mid = (x0 + w / 2, y0 + h / 2)
    best = None
    for i in range(int(w) + 1):
        for j in range(int(h) + 1):
            p = (x0 + i, y0 + j)
            room = min(depth(own, p) - INSIDE, depth(sq, p) - MARGIN)
            if room < 0:
                continue
            for c in covers:
                room = min(room, -depth(c, p) - CLEAR)
            if room < 0:
                continue
            dist = math.hypot(p[0] - mid[0], p[1] - mid[1])
            if best is None or room > best[0] + 0.01 or (abs(room - best[0]) <= 0.01 and dist < best[2]):
                best = (room, p, dist)
    return best[1] if best else None


# ----- CoreGraphics through ctypes --------------------------------------------------------


class CGPoint(ctypes.Structure):
    _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]


class CGSize(ctypes.Structure):
    _fields_ = [("width", ctypes.c_double), ("height", ctypes.c_double)]


class CGRect(ctypes.Structure):
    _fields_ = [("origin", CGPoint), ("size", CGSize)]


cg = ctypes.cdll.LoadLibrary(ctypes.util.find_library("CoreGraphics"))
cf = ctypes.cdll.LoadLibrary(ctypes.util.find_library("CoreFoundation"))
vp = ctypes.c_void_p
cg.CGWindowListCopyWindowInfo.restype = vp
cg.CGWindowListCopyWindowInfo.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
cg.CGRectMakeWithDictionaryRepresentation.argtypes = [vp, ctypes.POINTER(CGRect)]
cg.CGRectMakeWithDictionaryRepresentation.restype = ctypes.c_bool
cg.CGMainDisplayID.restype = ctypes.c_uint32
cg.CGDisplayBounds.restype = CGRect
cg.CGDisplayBounds.argtypes = [ctypes.c_uint32]
cg.CGEventCreate.restype = vp
cg.CGEventCreate.argtypes = [vp]
cg.CGEventGetLocation.restype = CGPoint
cg.CGEventGetLocation.argtypes = [vp]
cg.CGEventCreateMouseEvent.restype = vp
cg.CGEventCreateMouseEvent.argtypes = [vp, ctypes.c_uint32, CGPoint, ctypes.c_uint32]
cg.CGEventCreateKeyboardEvent.restype = vp
cg.CGEventCreateKeyboardEvent.argtypes = [vp, ctypes.c_uint16, ctypes.c_bool]
cg.CGEventSetFlags.argtypes = [vp, ctypes.c_uint64]
cg.CGEventPost.argtypes = [ctypes.c_uint32, vp]
cg.CGWarpMouseCursorPosition.argtypes = [CGPoint]
cf.CFArrayGetCount.restype = ctypes.c_long
cf.CFArrayGetCount.argtypes = [vp]
cf.CFArrayGetValueAtIndex.restype = vp
cf.CFArrayGetValueAtIndex.argtypes = [vp, ctypes.c_long]
cf.CFDictionaryGetValue.restype = vp
cf.CFDictionaryGetValue.argtypes = [vp, vp]
cf.CFNumberGetValue.argtypes = [vp, ctypes.c_int, vp]
cf.CFBooleanGetValue.argtypes = [vp]
cf.CFBooleanGetValue.restype = ctypes.c_bool
cf.CFStringCreateWithCString.restype = vp
cf.CFStringCreateWithCString.argtypes = [vp, ctypes.c_char_p, ctypes.c_uint32]
cf.CFStringGetCString.argtypes = [vp, ctypes.c_char_p, ctypes.c_long, ctypes.c_uint32]
cf.CFRelease.argtypes = [vp]
UTF8 = 0x08000100
KEYS = {k: cf.CFStringCreateWithCString(None, k.encode(), UTF8) for k in
        ("kCGWindowNumber", "kCGWindowOwnerPID", "kCGWindowLayer", "kCGWindowBounds", "kCGWindowAlpha",
         "kCGWindowIsOnscreen", "kCGWindowOwnerName")}
SHIFT_FLAG, SHIFT_KEY, DOWN, UP, HID = 0x00020000, 56, 1, 2, 0


def num(d, key, double=False):
    v = cf.CFDictionaryGetValue(d, KEYS[key])
    if not v:
        return 0
    out = ctypes.c_double() if double else ctypes.c_int64()
    cf.CFNumberGetValue(v, 13 if double else 4, ctypes.byref(out))
    return out.value


def listed():
    """On-screen windows front to back: dicts of id, pid, layer, bounds, alpha, owner."""
    arr = cg.CGWindowListCopyWindowInfo(1 | 16, 0)  # on screen only, no desktop elements
    out = []
    for i in range(cf.CFArrayGetCount(arr)):
        d = cf.CFArrayGetValueAtIndex(arr, i)
        r = CGRect()
        cg.CGRectMakeWithDictionaryRepresentation(cf.CFDictionaryGetValue(d, KEYS["kCGWindowBounds"]), ctypes.byref(r))
        owner = ctypes.create_string_buffer(256)
        name = cf.CFDictionaryGetValue(d, KEYS["kCGWindowOwnerName"])
        if name:
            cf.CFStringGetCString(name, owner, 256, UTF8)
        on = cf.CFDictionaryGetValue(d, KEYS["kCGWindowIsOnscreen"])
        out.append({"id": num(d, "kCGWindowNumber"), "pid": num(d, "kCGWindowOwnerPID"), "layer": num(d, "kCGWindowLayer"),
                    "bounds": (r.origin.x, r.origin.y, r.size.width, r.size.height),
                    "alpha": num(d, "kCGWindowAlpha", True), "onscreen": bool(on and cf.CFBooleanGetValue(on)),
                    "owner": owner.value.decode(errors="replace")})
    cf.CFRelease(arr)
    return out


def screen_frame():
    r = cg.CGDisplayBounds(cg.CGMainDisplayID())
    return (r.origin.x, r.origin.y, r.size.width, r.size.height)


def pointer():
    e = cg.CGEventCreate(None)
    p = cg.CGEventGetLocation(e)
    cf.CFRelease(e)
    return (p.x, p.y)


def post_key(down, flags):
    e = cg.CGEventCreateKeyboardEvent(None, SHIFT_KEY, down)
    cg.CGEventSetFlags(e, flags)
    cg.CGEventPost(HID, e)
    cf.CFRelease(e)


def post_mouse(kind, p):
    e = cg.CGEventCreateMouseEvent(None, kind, CGPoint(p[0], p[1]), 0)
    cg.CGEventSetFlags(e, SHIFT_FLAG)
    cg.CGEventPost(HID, e)
    cf.CFRelease(e)


def shift_press(points):
    """One Shift press: a fast click at each of `points`, 15 ms apart, the pointer back."""
    home = pointer()
    post_key(True, SHIFT_FLAG)
    time.sleep(0.012)
    for k, p in enumerate(points):
        if k:
            time.sleep(0.015)
        post_mouse(DOWN, p)
        post_mouse(UP, p)
        end = time.time() + 0.02
        while time.time() < end and math.hypot(pointer()[0] - p[0], pointer()[1] - p[1]) > 1.0:
            time.sleep(0.0005)
        cg.CGWarpMouseCursorPosition(CGPoint(home[0], home[1]))
    time.sleep(0.012)
    post_key(False, 0)


# ----- the strip, read from the list ------------------------------------------------------


def same(a, b, tol=1.0):
    return all(abs(x - y) <= tol for x, y in zip(a, b))


def contains(r, p):
    return r[0] <= p[0] <= r[0] + r[2] and r[1] <= p[1] <= r[1] + r[3]


def manager_pid(ws):
    return next((w["pid"] for w in ws if w["owner"] == "WindowManager"), None)


def fixture_pids():
    return {name: set(t.pids(app)) for name, (_, app, _) in t.FIX.items()}


def read():
    """The fixtures' windows (thumb or full), the icons, the list."""
    ws = listed()
    mgr = manager_pid(ws)
    fx = fixture_pids()
    owner = {pid: name for name, ps in fx.items() for pid in ps}
    wins = []
    for i, w in enumerate(ws):
        if w["pid"] not in owner or w["layer"] != 0:
            continue
        twin = i > 0 and ws[i - 1]["pid"] == mgr and ws[i - 1]["layer"] == 0 and same(ws[i - 1]["bounds"], w["bounds"])
        wins.append({"app": owner[w["pid"]], "id": w["id"], "at": i, "bounds": w["bounds"], "thumb": twin})
    icons = [w["bounds"] for w in ws if w["pid"] == mgr and w["layer"] == 0 and same(w["bounds"][2:], ICON, 2.0)]
    return {"list": ws, "mgr": mgr, "wins": wins, "icons": icons}


def over_of(r, i):
    """What is listed in front of the window at `i` but its own twin, by kind."""
    ws, mgr = r["list"], r["mgr"]
    out = []
    for j, o in enumerate(ws[:i]):
        if j == i - 1 or not o["onscreen"] or o["layer"] < 0:
            continue
        m0 = o["pid"] == mgr and o["layer"] == 0
        if m0 and same(o["bounds"][2:], ICON, 2.0):
            continue
        thumbish = (j > 0 and ws[j - 1]["pid"] == mgr and same(ws[j - 1]["bounds"], o["bounds"])) or \
                   (m0 and j + 1 < len(ws) and same(ws[j + 1]["bounds"], o["bounds"]))
        full_screen_overlay = o["layer"] > 0 and o["bounds"][2] >= 1000 and o["bounds"][3] >= 600
        if full_screen_overlay:
            continue
        out.append(("thumb" if thumbish else "window", o["bounds"]))
    out += [("icon", b) for b in r["icons"]]
    return out


def top_is_manager(r, p):
    """The topmost window at p (passing over full-screen overlays, which let clicks through
    where they draw nothing: Notification Center's, the Dock's) is WindowManager's layer 0."""
    for w in r["list"]:
        if not w["onscreen"] or w["layer"] < 0 or w["alpha"] <= 0 or not contains(w["bounds"], p):
            continue
        if w["layer"] > 0 and w["bounds"][2] >= 1000 and w["bounds"][3] >= 600:
            continue
        return w["pid"] == r["mgr"] and w["layer"] == 0
    return False


def snapshot():
    return {w["id"]: (w["bounds"], w["thumb"]) for w in read()["wins"]}


def settle(timeout=3.0):
    """Waits for the fixtures' windows to hold still for 300 ms (and 700 ms at the least)."""
    t0 = time.time()
    time.sleep(0.7)
    last, still = snapshot(), time.time()
    while time.time() - t0 < timeout:
        time.sleep(0.1)
        now = snapshot()
        if now != last:
            last, still = now, time.time()
        elif time.time() - still >= 0.3:
            break
    return read()


def arranged(r):
    by = {}
    for w in r["wins"]:
        by.setdefault(w["app"], []).append(w)
    ok = lambda name, n, thumb: len(by.get(name, [])) == n and all(w["thumb"] == thumb for w in by[name])  # noqa: E731
    return ok("Bravo", 1, True) and ok("Alpha", 2, True) and ok("Charlie", 1, False)


def reopen(name, n):
    _, app, _ = t.FIX[name]
    for pid in t.pids(app):
        os.kill(pid, signal.SIGTERM)
    end = time.time() + 5
    while t.pids(app) and time.time() < end:
        time.sleep(0.1)
    t.open_fixture(name, windows=n)


def arrange(full=False):
    """Bravo alone and Alpha's two in the strip, Charlie in front, each by Apple's own answer
    to an app opened (its own stage, in front) or activated."""
    r = read()
    by = {}
    for w in r["wins"]:
        by.setdefault(w["app"], []).append(w)
    for name, n in (("Bravo", 1), ("Alpha", 2)):
        mine = by.get(name, [])
        if full or len(mine) != n or not all(w["thumb"] for w in mine):
            reopen(name, n)
            time.sleep(1.2)
    if full or len(by.get("Charlie", [])) != 1:
        reopen("Charlie", 1)
        time.sleep(1.2)
    subprocess.run(["open", t.FIX["Charlie"][1]], check=True)
    return settle()


# ----- the trials -------------------------------------------------------------------------


def outcome(before, after, target_ids, front_id):
    """added / switched / desktop / nothing, and which targets joined."""
    full = {w["id"]: not w["thumb"] for w in after["wins"]}
    joined = [i for i in target_ids if full.get(i)]
    b = {w["id"]: w["bounds"] for w in before["wins"]}
    a = {w["id"]: (w["bounds"], w["thumb"]) for w in after["wins"]}
    front_moved = front_id not in a or a[front_id][1] or not same(a[front_id][0], b[front_id], 2.0)
    if joined and not front_moved:
        return "added", joined
    if joined:
        return "switched", joined
    if front_moved:
        return "desktop", []
    return "nothing", []


def plan(r, v, screen):
    """The trials: (name, app, [(window id, point)], model depth of the first point)."""
    wins = {w["app"]: [x for x in r["wins"] if x["app"] == w["app"]] for w in r["wins"]}
    trials = []
    bravo = wins["Bravo"][0]
    box = bravo["bounds"]
    q = from_bounds(v, box)
    over = over_of(r, bravo["at"])
    covers = [(k, drawn(v, k, b)) for k, b in over]

    def add(name, win, p, on_icon=False):
        """A trial at `p` on `win`; Bravo's points on another thumbnail, a window or (but for
        the icon trials) an icon's drawn part are skipped: they would say nothing of Bravo's."""
        if p is None:
            return
        covered = win is bravo and any(depth(c, p) > -1.0 for k, c in covers if not (on_icon and k == "icon"))
        trials.append({"name": name, "app": win["app"], "clicks": [(win["id"], p)],
                       "depth": round(depth(from_bounds(v, win["bounds"]), p), 1), "covered": covered,
                       "manager_on_top": top_is_manager(r, p)})

    add("chosen", bravo, visible_point(v, screen, box, over))
    names = ("top", "right", "bottom", "left")
    for e in range(4):
        (ax, ay), (bx, by) = q[e], q[(e + 1) % 4]
        mx, my = (ax + bx) / 2, (ay + by) / 2
        n = math.hypot(bx - ax, by - ay)
        # Inward normal: toward the plane's middle.
        cx, cy = sum(p[0] for p in q) / 4, sum(p[1] for p in q) / 4
        nx, ny = -(by - ay) / n, (bx - ax) / n
        if (cx - mx) * nx + (cy - my) * ny < 0:
            nx, ny = -nx, -ny
        for d in (3.0, -3.0):
            add(f"{names[e]} edge {'in' if d > 0 else 'out'} {abs(d):.0f}", bravo, (mx + nx * d, my + ny * d))
    corner_names = ("top-left", "top-right", "bottom-right", "bottom-left")
    cx, cy = sum(p[0] for p in q) / 4, sum(p[1] for p in q) / 4
    for k in (1, 3):  # the corners whose box corner is cut off (above the middle: top-right, bottom-left)
        px, py = q[k]
        a, b = q[(k + 1) % 4], q[(k + 3) % 4]
        ux = (a[0] - px) / math.hypot(a[0] - px, a[1] - py) + (b[0] - px) / math.hypot(b[0] - px, b[1] - py)
        uy = (a[1] - py) / math.hypot(a[0] - px, a[1] - py) + (b[1] - py) / math.hypot(b[0] - px, b[1] - py)
        un = math.hypot(ux, uy)
        for d in (3.0, 7.0):
            add(f"{corner_names[k]} corner {d:.0f} in", bravo, (px + ux / un * d, py + uy / un * d))
    bx0, by0, bw, bh = box
    for name, p in (("box top-right", (bx0 + bw - 2, by0 + 2)), ("box bottom-left", (bx0 + 2, by0 + bh - 2)),
                    ("box top-left", (bx0 + 2, by0 + 2)), ("box bottom-right", (bx0 + bw - 2, by0 + bh - 2))):
        if depth(q, p) < 0:
            add(name + " (off the plane)", bravo, p)
    # The icon: its drawn middle over the card, and its window's clear part over the card.
    for ib in r["icons"]:
        dr = icon_drawn(ib)
        mid = (dr[0] + dr[2] / 2, dr[1] + dr[3] / 2)
        if depth(q, mid) > 3:
            add("icon drawn middle", bravo, mid, on_icon=True)
        clear = [(ib[0] + i, ib[1] + j) for i in range(0, 67, 2) for j in range(0, 83, 2)]
        clear = [p for p in clear if -depth(rect_quad(dr), p) >= 3 and depth(q, p) >= 4]
        if clear:
            add("icon window, clear part", bravo, max(clear, key=lambda p: depth(q, p)), on_icon=True)
    # Alpha's stage of two: the back window next to the front one, and the two in one press.
    alpha = sorted(wins["Alpha"], key=lambda w: w["at"])
    if len(alpha) == 2:
        front, back = alpha
        fb = front["bounds"]
        add("back, 5 pt left of the front's box", back, (fb[0] - 5, fb[1] + fb[3] * 0.38))
        pb = visible_point(v, screen, back["bounds"], over_of(r, back["at"]))
        add("back, chosen", back, pb)
        pf = visible_point(v, screen, front["bounds"], over_of(r, front["at"]))
        if pb and pf:
            trials.append({"name": "back then front, one press", "app": "Alpha", "clicks": [(back["id"], pb), (front["id"], pf)],
                           "depth": round(depth(from_bounds(v, back["bounds"]), pb), 1), "covered": False,
                           "manager_on_top": top_is_manager(r, pb) and top_is_manager(r, pf)})
    return trials


def main():
    if os.environ.get("SM_TEST_TOGGLE_APPLE_SM") != "1":
        print("skipped: set SM_TEST_TOGGLE_APPLE_SM=1 to toggle Apple's Stage Manager on this Mac (the owner's call)")
        return 0
    dry = "--dry" in sys.argv[1:]
    if subprocess.run(["pgrep", "-f", "Contents/MacOS/stage-manager"], capture_output=True).returncode == 0:
        sys.exit("a Stage Manager is running; quit it first (it would follow Apple's or stand down)")
    t.build_fixtures()
    t.original = t.read_apple()

    def cleanup():
        t.quit_fixtures()
        t.restore_apple()
        try:
            os.rmdir(t.HOME)
        except OSError:
            pass

    atexit.register(cleanup)
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, lambda s, _f: sys.exit(128 + s))
    t.quit_fixtures()
    t.set_apple(True)
    time.sleep(1.5)
    screen = screen_frame()
    # The visible frame's left and width: the Dock at the bottom (PROBE_VISIBLE_X/W otherwise).
    v = view(float(os.environ.get("PROBE_VISIBLE_X", screen[0])), float(os.environ.get("PROBE_VISIBLE_W", screen[2])),
             screen[1] + screen[3] / 2)
    r = arrange(full=True)
    if not arranged(r):
        r = arrange(full=True)
    if not arranged(r):
        sys.exit("could not set up Bravo alone and Alpha's two in the strip, Charlie in front")
    print(f"screen {screen}, tilt {math.degrees(v[0]):.1f}, eye distance {v[1]:.0f}, eye {v[2]}")
    for w in r["wins"]:
        print(f"  {w['app']:8s} {'thumb' if w['thumb'] else 'full '} {tuple(round(x, 1) for x in w['bounds'])}")
    print(f"  icons {[tuple(round(x, 1) for x in b) for b in r['icons']]}")
    trials = plan(r, v, screen)
    results = []
    print(f"\n{'trial':38s} {'point':>16s} {'depth':>6s}  outcome")
    for k, tr in enumerate(trials):
        if k:
            r = read()
            if not arranged(r):
                r = arrange()
                if not arranged(r):
                    r = arrange(full=True)
            # The windows were opened again: the plan's ids and points go stale; plan again.
            fresh = {x["name"]: x for x in plan(r, v, screen)}
            if tr["name"] not in fresh:
                print(f"{tr['name']:38s} skipped: not in the plan of the arrangement set up again", flush=True)
                continue
            tr = fresh[tr["name"]]
        p0 = tr["clicks"][0][1]
        line = f"{tr['name']:38s} {f'({p0[0]:.1f}, {p0[1]:.1f})':>16s} {tr['depth']:>6.1f}  "
        if not tr["manager_on_top"] or tr["covered"]:
            res = ("skipped: " + ("covered by another" if tr["covered"] else "not WindowManager's on top"), [])
        elif dry:
            res = ("planned", [])
        else:
            front = next(w["id"] for w in r["wins"] if w["app"] == "Charlie")
            shift_press([p for _, p in tr["clicks"]])
            after = settle()
            res = outcome(r, after, [i for i, _ in tr["clicks"]], front)
        joined = "" if len(tr["clicks"]) < 2 else f" (joined {len(res[1])} of {len(tr['clicks'])}: " + \
            ", ".join("back" if i == tr["clicks"][0][0] else "front" for i in res[1]) + ")"
        print(line + res[0] + joined, flush=True)
        results.append({"trial": tr["name"], "app": tr["app"], "points": [p for _, p in tr["clicks"]], "depth": tr["depth"],
                        "outcome": res[0], "joined": len(res[1])})
    fd, path = tempfile.mkstemp(prefix="probe-press-quad.", suffix=".json")
    with os.fdopen(fd, "w") as f:
        json.dump({"screen": screen, "tilt_deg": math.degrees(v[0]), "distance": v[1], "eye": v[2],
                   "arrangement": [{"app": w["app"], "bounds": w["bounds"], "thumb": w["thumb"]} for w in read()["wins"]],
                   "icons": r["icons"], "results": results}, f, indent=1)
    print(f"\nwritten to {path}")
    print("Read: 'in' edges and corners 7 in should add, 'out' edges, corners 3 in and the box corners off the plane\n"
          "should not (desktop = the click fell through to the wallpaper); the icon's clear part adding says icons\n"
          "cover only where drawn; the back 5 pt left adding says the front window does not grow under the click;\n"
          "the one-press pair joining 2 of 2 says the first click of a press is not lost.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
