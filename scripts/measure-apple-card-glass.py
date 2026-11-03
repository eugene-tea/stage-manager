#!/usr/bin/env python3
"""Measures how Apple's Stage Manager strip lets the wallpaper through a card (round
card-glass, 11-03), to set `[cards] window_opacity`, `backdrop_saturation` and
`backdrop_brightness` from it.

RUN ONLY WITH THE OWNER AT THE MAC AND APPLE'S STAGE MANAGER ON. He turns it on (System
Settings, Desktop & Dock); this script only reads `com.apple.WindowManager GloballyEnabled`
and never writes it. It opens two fixture apps of its own (one window each), takes screenshots
of the strip's column (macOS may ask for screen access: the screenshot is the only way to see
Apple's strip), and quits the fixtures at the end. Nothing is written to the repo; the
screenshots stay in a temp folder, deleted at the end unless --keep.

How: fixture A's one window becomes a card on Apple's strip (fixture B is the active stage).
A's window is flat white, then black, then grey, then white again, in place (its
`--color-file`), so the same card over the same wallpaper is read four times:

    white = a + (1 - a) U      black = (1 - a) U      =>  a = white - black,  U = black / (1 - a)

per pixel and channel, in the display's encoded values, `a` the picture's opacity and `U` the
under. Grey (128) tells whether the blend is in encoded values (the model) or in linear light;
the second white tells the reads are steady. The under's spread from the card's top to its
bottom tells whether it is flat (the model: a mean behind the thumbnail) or a local blur.
`stage-manager probe-glass` gives the wallpaper behind the card as the app reads it (from the
picture's file), and the under is fitted to it: U = k (s M + (1 - s) luma(M)).

With --hover (round hover-glass, for `[cards] hover_window_opacity`): after those reads it
asks the owner to put the pointer on A's card and press Return (it never moves the pointer
itself), then reads the hovered card white and black the same way: its opacity there
(a = white - black; 1 when the hovered card is solid) and its size against the card at rest.

Usage: scripts/measure-apple-card-glass.py [--yes] [--keep] [--hover] [--bin PATH]
Needs: python3, macOS's screencapture and sips, swiftc (the fixture is built here), and the
app's binary (`make` first; target/debug/stage-manager, or --bin).
"""

import json
import os
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIX = {
    "a": ("dev.eugene.stagemanager.fixture.glass.a", os.path.join(ROOT, "target", "SM Fixture Glass A.app")),
    "b": ("dev.eugene.stagemanager.fixture.glass.b", os.path.join(ROOT, "target", "SM Fixture Glass B.app")),
}
EXE = "Contents/MacOS/sm-fixture"
LUMA = (0.2126, 0.7152, 0.0722)
# How long Apple's card takes to show a window's new colour (it is live; generous).
SETTLE = 2.5
# The strip's column: this wide from the main display's left edge (points).
COLUMN = 340


def say(msg):
    print(msg, flush=True)


def run(args, **kw):
    return subprocess.run(args, capture_output=True, text=True, **kw)


def apple_stage_manager_on():
    r = run(["defaults", "read", "com.apple.WindowManager", "GloballyEnabled"])
    return r.returncode == 0 and r.stdout.strip() == "1"


def build_fixtures():
    src = os.path.join(ROOT, "scripts", "fixture", "main.swift")
    for bundle_id, app in FIX.values():
        exe = os.path.join(app, EXE)
        if os.path.exists(exe) and os.path.getmtime(exe) >= os.path.getmtime(src):
            continue
        env = dict(os.environ, FIXTURE_ID=bundle_id, FIXTURE_APP=app)
        for cmd in (["scripts/bundle.sh", "fixture"], ["scripts/sign.sh", app]):
            r = subprocess.run(cmd, cwd=ROOT, env=env, capture_output=True, text=True)
            if r.returncode != 0:
                sys.exit(f"cannot build {app}: {r.stdout}{r.stderr}")


def pids(app):
    r = run(["pgrep", "-f", os.path.join(app, EXE)])
    return [int(p) for p in r.stdout.split()]


def quit_fixtures():
    for _, app in FIX.values():
        for pid in pids(app):
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass


def main_screen_points(binary):
    """The main display's size in points, from the app's own probe."""
    r = run([binary, "probe-glass", "--rect", "0,0,1,1"])
    try:
        v = json.loads(r.stdout.strip().splitlines()[-1])
    except (json.JSONDecodeError, IndexError):
        sys.exit(f"probe-glass did not answer: {r.stdout}{r.stderr}")
    if not v.get("ok"):
        sys.exit(f"the app cannot draw the wallpaper from its file ({v.get('why')}): the under cannot be compared")
    return v["screen"]["w"], v["screen"]["h"]


def read_bmp(path):
    """(width, height, rows of (r, g, b) top first) of a 24- or 32-bit BMP."""
    with open(path, "rb") as f:
        data = f.read()
    offset = struct.unpack_from("<I", data, 10)[0]
    w, h = struct.unpack_from("<ii", data, 18)
    bpp = struct.unpack_from("<H", data, 28)[0]
    if bpp not in (24, 32):
        sys.exit(f"{path}: {bpp} bits a pixel")
    step = bpp // 8
    stride = (w * step + 3) & ~3
    rows = []
    for y in range(abs(h)):
        at = offset + y * stride
        rows.append([(data[at + x * step + 2], data[at + x * step + 1], data[at + x * step]) for x in range(w)])
    if h > 0:
        rows.reverse()
    return w, abs(h), rows


def shoot(folder, name, height):
    png, bmp = os.path.join(folder, name + ".png"), os.path.join(folder, name + ".bmp")
    r = run(["screencapture", "-x", "-R", f"0,0,{COLUMN},{int(height)}", png])
    if r.returncode != 0 or not os.path.exists(png):
        sys.exit(f"screencapture failed: {r.stderr}")
    run(["sips", "-s", "format", "bmp", png, "--out", bmp])
    return read_bmp(bmp)


def card_mask(white, black):
    """The pixels of A's card, where its white and black reads differ a lot in every channel,
    and its inside (3 px in from any edge of that: the turned edges, the border, the corners)."""
    h, w = len(white), len(white[0])
    mask = [[min(white[y][x][i] - black[y][x][i] for i in range(3)) > 80 for x in range(w)] for y in range(h)]
    pts = [(x, y) for y in range(h) for x in range(w) if mask[y][x]]
    inner = [(x, y) for (x, y) in pts if all(mask[y + dy][x + dx] for dx in (-3, 0, 3) for dy in (-3, 0, 3)
                                            if 0 <= x + dx < w and 0 <= y + dy < h)]
    return pts, inner


def hover_read(white, black, rest_px, scale):
    """The hovered card's opacity (white - black, per pixel inside it) and its size against
    the card at rest (`rest_px`: x, y, w, h in pixels)."""
    pts, inner = card_mask(white, black)
    if not inner:
        return {"error": "no card changed with A's window while hovered: was the pointer on A's card?"}
    x0, x1 = min(p[0] for p in pts), max(p[0] for p in pts)
    y0, y1 = min(p[1] for p in pts), max(p[1] for p in pts)
    alphas = [sum(white[y][x][i] - black[y][x][i] for i in range(3)) / (3 * 255) for x, y in inner]
    shows = [[black[y][x][i] for x, y in inner] for i in range(3)]
    return {
        "card_points": [round(x0 / scale, 1), round(y0 / scale, 1), round((x1 - x0 + 1) / scale, 1), round((y1 - y0 + 1) / scale, 1)],
        "pixels": len(alphas),
        "opacity": {"median": round(percentile(alphas, 0.5), 4), "p05": round(percentile(alphas, 0.05), 4), "p95": round(percentile(alphas, 0.95), 4)},
        "black_page_reads": [round(percentile(c, 0.5), 1) for c in shows],
        "size_vs_rest": [round((x1 - x0 + 1) / rest_px[2], 4), round((y1 - y0 + 1) / rest_px[3], 4)],
        "suggested": {"hover_window_opacity": round(min(1.0, percentile(alphas, 0.5)), 2)},
    }


def percentile(v, q):
    v = sorted(v)
    return v[min(len(v) - 1, max(0, int(round(q * (len(v) - 1)))))]


def luma(c):
    return sum(LUMA[i] * c[i] for i in range(3))


def fit_under(wall, under):
    """Least squares k, s for under = k (s M + (1 - s) luma(M)), on a grid."""
    best = None
    y = luma(wall)
    for si in range(0, 201):
        s = si / 100
        base = [s * wall[i] + (1 - s) * y for i in range(3)]
        den = sum(b * b for b in base)
        if den <= 0:
            continue
        k = sum(base[i] * under[i] for i in range(3)) / den
        err = sum((k * base[i] - under[i]) ** 2 for i in range(3))
        if best is None or err < best[0]:
            best = (err, k, s)
    return best


def to_lin(v):
    c = v / 255
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def from_lin(x):
    x = max(0.0, min(1.0, x))
    return 255 * (12.92 * x if x <= 0.0031308 else 1.055 * x ** (1 / 2.4) - 0.055)


def main():
    args = sys.argv[1:]
    keep = "--keep" in args
    hover = "--hover" in args
    binary = os.path.join(ROOT, "target", "debug", "stage-manager")
    if "--bin" in args:
        binary = args[args.index("--bin") + 1]
    if not os.path.exists(binary):
        sys.exit(f"{binary} is missing: run make first, or pass --bin")
    if not apple_stage_manager_on():
        sys.exit("Apple's Stage Manager is off: the owner turns it on (this script never does)")
    if "--yes" not in args:
        input("Apple's Stage Manager is on. With the owner at the Mac, press Return to start (Ctrl-C to stop)... ")
    build_fixtures()
    sw, sh = main_screen_points(binary)
    folder = tempfile.mkdtemp(prefix="sm-apple-glass.")
    color_file = os.path.join(folder, "color")
    with open(color_file, "w") as f:
        f.write("ffffff")
    try:
        # A first (its window flat white, label-less, clear of the strip), then B (dark grey)
        # becomes the active stage: A waits as the top card.
        frame = f"{COLUMN + 120},120,{int(sw * 0.5)},{int(sh * 0.55)}"
        subprocess.run(["open", "-n", FIX["a"][1], "--args", "--windows", "1", "--no-label", "--color-file", color_file,
                        "--title", "Glass A", "--frames", frame], check=True)
        time.sleep(3.0)
        subprocess.run(["open", "-n", FIX["b"][1], "--args", "--windows", "1", "--no-label", "--colors", "404040",
                        "--title", "Glass B", "--frames", frame], check=True)
        time.sleep(3.0)
        shots = {}
        for name, hex_ in (("white", "ffffff"), ("black", "000000"), ("grey", "808080"), ("white2", "ffffff")):
            with open(color_file, "w") as f:
                f.write(hex_)
            time.sleep(SETTLE)
            shots[name] = shoot(folder, name, sh)
            say(f"read the strip with A's window {name}")
        if hover:
            with open(color_file, "w") as f:
                f.write("ffffff")
            input("Put the pointer on Glass A's card (the top card) and keep it there; then press Return... ")
            for name, hex_ in (("hover_white", "ffffff"), ("hover_black", "000000")):
                with open(color_file, "w") as f:
                    f.write(hex_)
                time.sleep(SETTLE)
                shots[name] = shoot(folder, name, sh)
                say(f"read the hovered card with A's window {name[6:]}")
            say("done with the hover: the pointer may move")
    finally:
        quit_fixtures()
    w, h, white = shots["white"]
    black, grey, white2 = shots["black"][2], shots["grey"][2], shots["white2"][2]
    scale = w / COLUMN
    # The card's picture: where white and black differ a lot, every channel.
    pts, inner = card_mask(white, black)
    if not pts:
        sys.exit("no card changed with A's window: was A on the strip? (screenshots kept in " + folder + ")")
    x0, x1 = min(p[0] for p in pts), max(p[0] for p in pts)
    y0, y1 = min(p[1] for p in pts), max(p[1] for p in pts)
    alphas, unders, by_row, grey_off, grey_lin, steady = [], [], {}, [], [], []
    for x, y in inner:
        wv, kv, gv, w2 = white[y][x], black[y][x], grey[y][x], white2[y][x]
        a = sum(wv[i] - kv[i] for i in range(3)) / (3 * 255)
        if not 0.05 < a < 0.999:
            continue
        u = [kv[i] / (1 - a) for i in range(3)]
        alphas.append(a)
        unders.append(u)
        by_row.setdefault((y - y0) * 3 // max(1, y1 - y0 + 1), []).append(u)
        grey_off.append(max(abs(gv[i] - (a * 128 + (1 - a) * u[i])) for i in range(3)))
        grey_lin.append(max(abs(gv[i] - from_lin(a * to_lin(128) + (1 - a) * to_lin(u[i]))) for i in range(3)))
        steady.append(max(abs(w2[i] - wv[i]) for i in range(3)))
    if not alphas:
        sys.exit("no pixel of the card reads as a blend (screenshots kept in " + folder + ")")
    under = [percentile([u[i] for u in unders], 0.5) for i in range(3)]
    thirds = {k: [round(percentile([u[i] for u in v], 0.5), 1) for i in range(3)] for k, v in sorted(by_row.items())}
    rect = [round(x0 / scale, 1), round(y0 / scale, 1), round((x1 - x0 + 1) / scale, 1), round((y1 - y0 + 1) / scale, 1)]
    r = run([binary, "probe-glass", "--rect", ",".join(map(str, rect))])
    probe = json.loads(r.stdout.strip().splitlines()[-1])
    wall = probe["rects"][0]["wallpaper"] if probe.get("ok") else None
    fit = fit_under(wall, under) if wall else None
    out = {
        "card_px": [x0, y0, x1 - x0 + 1, y1 - y0 + 1],
        "card_points": rect,
        "pixels": len(alphas),
        "opacity": {"median": round(percentile(alphas, 0.5), 4), "p05": round(percentile(alphas, 0.05), 4), "p95": round(percentile(alphas, 0.95), 4)},
        "under": [round(v, 1) for v in under],
        "under_spread_p05_p95": [[round(percentile([u[i] for u in unders], q), 1) for i in range(3)] for q in (0.05, 0.95)],
        "under_by_third_top_middle_bottom": thirds,
        "grey_vs_encoded_blend_median_off": round(percentile(grey_off, 0.5), 2),
        "grey_vs_linear_blend_median_off": round(percentile(grey_lin, 0.5), 2),
        "white_again_median_off": round(percentile(steady, 0.5), 2),
        "wallpaper_behind_card_from_file": wall,
        "picture": probe.get("picture"),
        "fit": fit and {"backdrop_brightness": round(fit[1], 3), "backdrop_saturation": round(fit[2], 2), "rms": round((fit[0] / 3) ** 0.5, 2)},
        "suggested": fit and {"window_opacity": round(percentile(alphas, 0.5), 2), "backdrop_saturation": round(fit[2], 2),
                              "backdrop_brightness": round(fit[1], 2)},
    }
    if hover:
        out["hovered"] = hover_read(shots["hover_white"][2], shots["hover_black"][2], out["card_px"], scale)
    print(json.dumps(out, indent=2))
    if keep:
        say(f"screenshots kept in {folder} (never copy them into the repo)")
    else:
        shutil.rmtree(folder, ignore_errors=True)


if __name__ == "__main__":
    main()
