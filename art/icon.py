#!/usr/bin/env python3
"""Writes the app icon as an Icon Composer document (`<name>.icon`: `icon.json` + SVG layers).

The icon is the Stage Manager idea as an object: a frosted glass window pane centre right and
small tilted cards stacked at the left, the strip, each with its app's coloured badge at the
lower-left corner. The layers are flat shapes; macOS 26 adds the Liquid Glass (refraction,
specular edge, shadow) and derives the dark, clear and tinted appearances.

    python3 art/icon.py                       # the chosen variant -> art/AppIcon.icon
    python3 art/icon.py --variant graphite --out /tmp/Graphite.icon

Original artwork only: every shape here is drawn by this script; no Apple artwork, no SF
Symbols. Standard library only, so it runs on the system Python.
"""

import argparse
import json
import math
import shutil
from pathlib import Path

# The variant shipped in the app (`make icon`). The other two are kept for comparison.
DEFAULT_VARIANT = "sky"

CANVAS = 1024  # Icon Composer's macOS canvas, in points; the system masks the rounded square.
MIDDLE_Y = 532.0  # the cards stack on this line, and their perspective looks from it


def srgb(hex_colour: str, alpha: float = 1.0) -> str:
    """An Icon Composer colour string from `#rrggbb`."""
    h = hex_colour.lstrip("#")
    r, g, b = (int(h[i : i + 2], 16) / 255 for i in (0, 2, 4))
    return f"extended-srgb:{r:.5f},{g:.5f},{b:.5f},{alpha:.5f}"


VARIANTS = {
    # A calm daylight blue field; a frosted window over another one behind it, as floating
    # windows overlap; white glass; the badges carry the colour.
    "sky": {
        "background": ("#7cc3ff", "#2b63e6"),
        "background_dark": ("#3a3f4b", "#16181d"),
        "pane": "#ffffff",
        "cards": ["#ffffff"] * 3,
        "badges": ["#ff9f0a", "#34c759", "#ff375f"],
        "cards_n": 3,
        "pane_translucency": 0.35,
        "title_dots": False,
        "back_window": True,
    },
    # Graphite field, colour in the cards themselves, a clear pane: a "pro tool" look.
    "graphite": {
        "background": ("#6b7280", "#262a33"),
        "background_dark": ("#3a3d44", "#141518"),
        "pane": "#ffffff",
        "cards": ["#3d9bff", "#34c759", "#ff9f0a"],
        "badges": None,
        "cards_n": 3,
        "pane_translucency": 0.55,
        "title_dots": True,
        "dot_colour": "#3a3f4b",
        "back_window": False,
    },
    # Teal to indigo, two larger cards and a window with a title bar.
    "aurora": {
        "background": ("#5ad8c8", "#5b4fd6"),
        "background_dark": ("#26323f", "#15141f"),
        "pane": "#ffffff",
        "cards": ["#ffffff"] * 2,
        "badges": ["#ffcc00", "#ff6b5a"],
        "cards_n": 2,
        "pane_translucency": 0.3,
        "title_dots": True,
        "back_window": False,
    },
}

# Geometry, in canvas points.
PANE = (372.0, 300.0, 500.0, 472.0)  # x, y, width, height: the window, centre right
PANE_RADIUS = 64.0
BACK_OFFSET = (44.0, -70.0)  # the window behind, up and to the right (variants with one)
CARD_X = 132.0  # the cards' left (near) edge
CARD_W = 168.0  # how far each card reaches to the right, after the turn
CARD_GAP = 34.0
FAR_SCALE = 0.84  # the far (right) edge's height over the near one's, like the strip's turn
CARD_RADIUS = 22.0


def rounded_polygon(points, radius):
    """An SVG path through `points` with each corner rounded by `radius` (quadratic joins)."""
    n = len(points)
    out = []
    for i in range(n):
        px, py = points[i - 1]
        vx, vy = points[i]
        nx, ny = points[(i + 1) % n]
        d1 = math.hypot(px - vx, py - vy)
        d2 = math.hypot(nx - vx, ny - vy)
        r = min(radius, d1 / 2, d2 / 2)
        ax, ay = vx + (px - vx) * r / d1, vy + (py - vy) * r / d1
        bx, by = vx + (nx - vx) * r / d2, vy + (ny - vy) * r / d2
        out.append(f"{'M' if i == 0 else 'L'}{ax:.2f},{ay:.2f} Q{vx:.2f},{vy:.2f} {bx:.2f},{by:.2f}")
    return " ".join(out) + " Z"


def card_quads(count):
    """Each card's corners (near top, far top, far bottom, near bottom), stacked on the middle.

    A card is a plane turned about its left edge, seen from the stack's middle line: the
    far edge is shorter and its ends lean toward the middle, as the strip's cards do.
    """
    height = 150.0 if count == 3 else 196.0
    total = count * height + (count - 1) * CARD_GAP
    top = MIDDLE_Y - total / 2
    quads = []
    for i in range(count):
        y0 = top + i * (height + CARD_GAP)
        y1 = y0 + height
        far = lambda y: MIDDLE_Y + (y - MIDDLE_Y) * FAR_SCALE  # noqa: E731
        x0, x1 = CARD_X, CARD_X + CARD_W
        quads.append([(x0, y0), (x1, far(y0)), (x1, far(y1)), (x0, y1)])
    return quads


def svg(body: str) -> str:
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{CANVAS}" height="{CANVAS}" '
        f'viewBox="0 0 {CANVAS} {CANVAS}">\n{body}\n</svg>\n'
    )


def pane_svg(v) -> str:
    x, y, w, h = PANE
    return svg(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{PANE_RADIUS}" fill="{v["pane"]}"/>')


def back_window_svg(v) -> str:
    x, y, w, h = PANE
    dx, dy = BACK_OFFSET
    return svg(f'<rect x="{x + dx}" y="{y + dy}" width="{w}" height="{h}" rx="{PANE_RADIUS}" fill="{v["pane"]}"/>')


def title_dots_svg(v) -> str:
    x, y, _, _ = PANE
    colour = v.get("dot_colour", v["background"][1])
    dots = []
    for i in range(3):
        dots.append(f'<circle cx="{x + 62 + i * 52}" cy="{y + 58}" r="17" fill="{colour}"/>')
    return svg("\n".join(dots))


def cards_svg(v) -> str:
    paths = []
    for quad, colour in zip(card_quads(v["cards_n"]), v["cards"]):
        paths.append(f'<path d="{rounded_polygon(quad, CARD_RADIUS)}" fill="{colour}"/>')
    return svg("\n".join(paths))


def badges_svg(v) -> str:
    """The app badges: a small rounded square on each card's lower-left corner."""
    size = 72.0 if v["cards_n"] == 3 else 84.0
    shapes = []
    for quad, colour in zip(card_quads(v["cards_n"]), v["badges"]):
        cx, cy = quad[3][0] + 14, quad[3][1] - 14  # a little in from the near bottom corner
        shapes.append(
            f'<rect x="{cx - size / 2:.2f}" y="{cy - size / 2:.2f}" width="{size}" height="{size}" '
            f'rx="{size * 0.26:.2f}" fill="{colour}"/>'
        )
    return svg("\n".join(shapes))


def gradient(colours):
    return {"linear-gradient": [srgb(colours[0]), srgb(colours[1])]}


def layer(name, glass=True, opacity=None):
    out = {"glass": glass, "image-name": f"{name}.svg", "name": name}
    if opacity is not None:
        out["opacity"] = opacity
    return out


def document(v):
    """`icon.json`: groups front to back; the system lights each group as one glass piece."""
    front = [layer("pane")]
    if v["title_dots"]:
        front.insert(0, layer("title-dots", opacity=0.55))
    groups = [
        {
            "layers": front,
            "shadow": {"kind": "neutral", "opacity": 0.5},
            "translucency": {"enabled": True, "value": v["pane_translucency"]},
        }
    ]
    if v["back_window"]:
        groups.append(
            {
                "layers": [layer("back-window", opacity=0.7)],
                "shadow": {"kind": "neutral", "opacity": 0.3},
                "translucency": {"enabled": True, "value": 0.4},
            }
        )
    if v["badges"]:
        groups.append(
            {
                "layers": [layer("badges")],
                "shadow": {"kind": "layer-color", "opacity": 0.5},
                "translucency": {"enabled": False, "value": 0.5},
            }
        )
    groups.append(
        {
            "layers": [layer("cards", opacity=0.92)],
            "shadow": {"kind": "neutral", "opacity": 0.35},
            "translucency": {"enabled": True, "value": 0.2},
        }
    )
    return {
        "fill-specializations": [
            {"value": gradient(v["background"])},
            {"appearance": "dark", "value": gradient(v["background_dark"])},
        ],
        "groups": groups,
        "supported-platforms": {"squares": ["macOS"]},
    }


def write(variant: str, out: Path) -> None:
    v = VARIANTS[variant]
    if out.exists():
        shutil.rmtree(out)
    assets = out / "Assets"
    assets.mkdir(parents=True)
    (assets / "pane.svg").write_text(pane_svg(v))
    (assets / "cards.svg").write_text(cards_svg(v))
    if v["badges"]:
        (assets / "badges.svg").write_text(badges_svg(v))
    if v["back_window"]:
        (assets / "back-window.svg").write_text(back_window_svg(v))
    if v["title_dots"]:
        (assets / "title-dots.svg").write_text(title_dots_svg(v))
    (out / "icon.json").write_text(json.dumps(document(v), indent=2, sort_keys=True) + "\n")


def main() -> None:
    here = Path(__file__).resolve().parent
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--variant", choices=sorted(VARIANTS), default=DEFAULT_VARIANT)
    ap.add_argument("--out", type=Path, default=here / "AppIcon.icon")
    args = ap.parse_args()
    write(args.variant, args.out)
    print(f"Wrote {args.out} ({args.variant})")


if __name__ == "__main__":
    main()
