#!/usr/bin/env python3
"""Reads recordings of Apple's strip brought back at the left edge (round edge-flat, 11-16)
and prints what `ui::edge_flat` and `docs/UX-SPEC.md` use: when the cards turn flat, the
slide's curve, each frame's pose (flat or turned), the hide's curve and how the turn comes
back, the cards' sizes flat and turned.

Reads files only; runs nothing, moves nothing. A recording is JSON lines, one per change, as
a probe wrote them every 8 ms while Apple's Stage Manager hid its strip behind a window and
the pointer rested at the left edge, then moved away:

    {"t": ms, "pointer": [x, y], "w": [[owner, id, x, y, w, h], ...]}

`w` lists WindowManager's layer-0 windows ("WM") and the fixture apps' windows. A strip
thumbnail is the app's real window drawn small, so a fixture window's listed bounds are its
card's bounding box (turned in 3D or flat). The recordings themselves are never committed
(they list the owner's other windows too).

The model fitted (the same as `ui::edge_flat`): each card's window is a plane the strip's eye
draws turned about its left edge (eye at the cards' left edge, the display's middle, 2260 pt
away on a 2560 pt display; turn 47.9°); between the flat card and the turned one off the
screen it is a pose seen through the flight's eye (the display's middle, 1.17 display widths
away), its size, turn and height at the spring's progress p, its sideways place at
p - k p (1 - p).

Usage: scripts/measure-edge-reveal.py REC.jsonl... [--window NAME=WxH ...] [--top NAME=Y ...]
       [--display WxH] [--turn DEG] [--eye-distance PT]
The windows default to the probe's fixtures (900 x 668, 700 x 528, 1000 x 728 pt with their
title bars) and their cards' tops to the recording's.
Needs: python3 only.
"""

import argparse
import json
import math
import sys

FIXTURES = {"alpha": (900.0, 668.0), "bravo": (700.0, 528.0), "charlie": (1000.0, 728.0)}
CARD_HEIGHT = 97.0  # `[cards] min_height`: the fixtures' windows are drawn at least this tall


def plane_drawn(x, top, u, v, tilt, eye):
    """A plane turned by `tilt` about its left edge at (x, top..top+v), `u` wide before the
    turn, as `eye` (ex, ey, distance) draws it: left, left top, left bottom, right, right top,
    right bottom."""
    ex, ey, d = eye
    f = d / (d + u * math.sin(tilt))
    far = x + u * math.cos(tilt)
    return (x, top, top + v, ex + (far - ex) * f, ey + (top - ey) * f, ey + (top + v - ey) * f)


def pose_of(drawn, eye):
    """The pose (middle x, middle y, w, h, turn about the middle) `eye` draws as `drawn`."""
    ex, ey, dist = eye
    l, lt, lb, r, rt, rb = drawn
    hl, hr = lb - lt, rb - rt
    depth = dist * (hl - hr) / (hl + hr)
    sl, sr = dist / (dist - depth), dist / (dist + depth)
    left, right = ex + (l - ex) / sl, ex + (r - ex) / sr
    across = (right - left) / 2
    return ((left + right) / 2, ey + ((lt + lb) / 2 - ey) / sl, 2 * math.hypot(depth, across), hl / sl, math.atan2(depth, across))


def pose_drawn(pose, eye):
    ex, ey, d = eye
    mx, my, w, h, t = pose

    def side(u):
        s = d / (d + u * math.sin(t))
        return ex + (mx + u * math.cos(t) - ex) * s, ey + (my - h / 2 - ey) * s, ey + (my + h / 2 - ey) * s

    a, b = side(-w / 2), side(w / 2)
    return (a[0], a[1], a[2], b[0], b[1], b[2])


def bounds(dr):
    l, lt, lb, r, rt, rb = dr
    return (min(l, r), min(lt, rt), abs(r - l), max(lb, rb) - min(lt, rt))


def crit(t, w):
    """A critically damped spring's progress from rest after t seconds at ω = w."""
    if t <= 0:
        return 0.0
    return 1 - (1 + w * t) * math.exp(-w * t)


def golden(f, lo, hi, n=60):
    g = (math.sqrt(5) - 1) / 2
    a, b = lo, hi
    c, d = b - g * (b - a), a + g * (b - a)
    fc, fd = f(c), f(d)
    for _ in range(n):
        if fc < fd:
            b, d, fd = d, c, fc
            c = b - g * (b - a)
            fc = f(c)
        else:
            a, c, fc = c, d, fd
            d = a + g * (b - a)
            fd = f(d)
    return (a + b) / 2


def fit_spring(ts, ps):
    """The start (s) and ω of a critically damped spring through (t, progress) pairs."""
    def err_w(w):
        t0 = golden(lambda t0: sum((crit(t - t0, w) - p) ** 2 for t, p in zip(ts, ps)), ts[0] - 0.2, ts[0] + 0.05)
        return sum((crit(t - t0, w) - p) ** 2 for t, p in zip(ts, ps)), t0

    w = golden(lambda w: err_w(w)[0], 5.0, 40.0)
    e, t0 = err_w(w)
    return t0, w, math.sqrt(e / len(ts))


def load(path, names):
    frames = []
    with open(path) as f:
        for line in f:
            d = json.loads(line)
            cards = {w[0]: tuple(w[2:]) for w in d["w"] if w[0] in names}
            if cards:
                frames.append((d["t"] / 1000.0, tuple(d["pointer"]), cards))
    return frames


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("recordings", nargs="+")
    ap.add_argument("--window", action="append", default=[], help="NAME=WxH, a fixture window's size on screen")
    ap.add_argument("--top", action="append", default=[], help="NAME=Y, its card's top (default: the recording's)")
    ap.add_argument("--display", default="2560x1440")
    ap.add_argument("--turn", type=float, default=47.9)
    ap.add_argument("--eye-distance", type=float, default=2260.0)
    a = ap.parse_args()
    windows = dict(FIXTURES)
    for s in a.window:
        n, wh = s.split("=")
        w, h = wh.lower().split("x")
        windows[n] = (float(w), float(h))
    dw, dh = (float(v) for v in a.display.lower().split("x"))
    strip_eye = (15.0, dh / 2, a.eye_distance)
    flight_eye = (dw / 2, dh / 2, 1.17 * dw)
    turn = math.radians(a.turn)
    u = {n: CARD_HEIGHT * w / h for n, (w, h) in windows.items()}
    print("cards before the turn (97 pt tall): " + ", ".join(f"{n} {u[n]:.1f}" for n in u))
    hides = []
    for path in a.recordings:
        frames = load(path, u)
        if not frames:
            print(f"{path}: no fixture windows listed")
            continue
        tops = {n: float(t) for n, t in (s.split("=") for s in a.top)}
        print(f"\n== {path}: {len(frames)} changes")
        flat_x = None
        reveal, hide = [], []
        for t, p, cards in frames:
            poses = []
            for n, b in sorted(cards.items()):
                aspect = b[2] / b[3]
                poses.append("flat" if abs(b[2] - u[n]) < 4 and abs(b[3] - CARD_HEIGHT) < 0.6 else "turned" if aspect < 1.12 else "between")
                tops.setdefault(n, b[1] + b[3] - CARD_HEIGHT)
            x = next(iter(cards.values()))[0]
            pose = poses[0] if len(set(poses)) == 1 else "/".join(poses)
            print(f"  {t * 1000:7.0f} ms  pointer {p}  x {x:7.1f}  {pose:8s}  " + "  ".join(f"{n} {b[2]:.0f}x{b[3]:.0f}" for n, b in sorted(cards.items())))
            if pose == "flat" and p[0] <= 2:
                reveal.append((t, x))
            # Once the pointer has left a revealed strip: the hide.
            if p[0] > 2 and flat_x is not None:
                hide.append((t, cards))
            elif pose == "flat":
                flat_x = x if flat_x is None else max(flat_x, x)
        if len(reveal) > 3:
            x0, x1 = reveal[0][1], max(x for _, x in reveal)
            moving = [(t, (x - x0) / (x1 - x0)) for t, x in reveal]
            t0, w, rms = fit_spring([t for t, _ in moving], [q for _, q in moving])
            print(f"  reveal: flat at {reveal[0][0] * 1000:.0f} ms at x {x0:.0f}, slides to {x1:.0f}; spring from {t0 * 1000:.0f} ms, "
                  f"ω {w:.2f} (rms {rms * abs(x1 - x0):.1f} pt); 99 % at {(t0 + 6.6384 / w) * 1000:.0f} ms")
        if hide:
            hides.append((hide, tops, flat_x if flat_x is not None else 16.0))
    if not hides:
        return
    # The hide: each frame's progress, and the sideways trail, fitted over every recording.
    end_x = -260.0
    for hide, _, _ in hides:
        for _, cards in hide:
            for b in cards.values():
                if b[2] / b[3] < 1.0:
                    end_x = min(end_x, b[0])

    def draw(n, top, x0, s, k):
        fl = pose_of(plane_drawn(x0, top, u[n], CARD_HEIGHT, 0.0, strip_eye), flight_eye)
        tu = pose_of(plane_drawn(end_x, top, u[n], CARD_HEIGHT, turn, strip_eye), flight_eye)
        side = s - k * s * (1 - s)
        pose = tuple(fl[i] + (tu[i] - fl[i]) * (side if i == 0 else s) for i in range(5))
        return bounds(pose_drawn(pose, flight_eye))

    def frame_err(cards, tops, x0, s, k):
        return max(max(abs(m - r) for m, r in zip(draw(n, tops[n], x0, s, k), b)) for n, b in cards.items())

    def total(k):
        e = []
        for hide, tops, x0 in hides:
            for _, cards in hide:
                s = golden(lambda s: frame_err(cards, tops, x0, s, k), 0.0, 1.0)
                e.append(frame_err(cards, tops, x0, s, k))
        return math.sqrt(sum(v * v for v in e) / len(e)), max(e)

    k = golden(lambda k: total(k)[0], -0.2, 0.8)
    rms, worst = total(k)
    print(f"\nhide: the turned card off the screen at x {end_x:.0f}; sideways trail k = {k:.2f}; "
          f"each frame's worst edge {rms:.2f} pt rms, {worst:.2f} at worst")
    for hide, tops, x0 in hides:
        ts, ss = [], []
        for t, cards in hide:
            s = golden(lambda s: frame_err(cards, tops, x0, s, k), 0.0, 1.0)
            if 0.01 < s < 0.99:
                ts.append(t)
                ss.append(s)
        if len(ts) > 3:
            t0, w, r = fit_spring(ts, ss)
            print(f"  progress on a spring from {t0 * 1000:.0f} ms, ω {w:.2f} (rms {r:.3f}); 99 % at {(t0 + 6.6384 / w) * 1000:.0f} ms")
    for n in u:
        print(f"  {n}: flat {u[n]:.1f} x {CARD_HEIGHT:.0f}; widest while turning "
              f"{max(draw(n, hides[0][1][n], hides[0][2], i / 100, k)[2] for i in range(101)):.1f}; turned off the screen "
              f"{bounds(plane_drawn(end_x, hides[0][1][n], u[n], CARD_HEIGHT, turn, strip_eye))[2]:.1f} wide")


if __name__ == "__main__":
    sys.exit(main())
