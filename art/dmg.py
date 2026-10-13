#!/usr/bin/env python3
"""Writes the DMG window's background as HTML, for scripts/dmg-art.sh to render to PNG.

    python3 art/dmg.py [--theme light|dark] <out.html>

The window (WIDTH × HEIGHT points) shows the app icon at APP_ICON, an arrow, Applications at
APPS_ICON (Finder draws both icons and their names; `scripts/dmg-settings.py` places them on
these same points), then the approvals the app will ask for and why. The words live in
`art/dmg/text.json`. Original artwork only: the small approval pictures are drawn here.
Standard library only.
"""

import argparse
import html
import json
from pathlib import Path

WIDTH, HEIGHT = 680, 480
ICON_SIZE = 112
APP_ICON = (190, 146)  # icon centres, in window points (y down)
APPS_ICON = (490, 146)

THEMES = {
    "light": {
        "page": "linear-gradient(180deg, #fbfbfd 0%, #eef0f5 100%)",
        "text": "#1d1d1f",
        "secondary": "#5e5e66",
        "arrow": "#a1a1aa",
        "panel": "rgba(255,255,255,0.75)",
        "panel_edge": "rgba(0,0,0,0.08)",
        "accent": "#0a6cff",
    },
    "dark": {
        "page": "linear-gradient(180deg, #2a2a2e 0%, #1c1c1f 100%)",
        "text": "#f5f5f7",
        "secondary": "#a1a1aa",
        "arrow": "#6e6e76",
        "panel": "rgba(255,255,255,0.06)",
        "panel_edge": "rgba(255,255,255,0.10)",
        "accent": "#4c9bff",
    },
}

# Small line pictures for the approval rows (24 × 24 view box), stroked in the accent colour.
PICTURES = {
    # A window with a four-way move mark: arranging windows.
    "arrange": (
        '<rect x="2.5" y="4" width="19" height="16" rx="3.5"/>'
        '<path d="M2.5 8.5h19"/>'
        '<path d="M12 11v7M8.5 14.5h7M10.5 12.5 12 11l1.5 1.5M10.5 16.5 12 18l1.5-1.5'
        'M10 13l-1.5 1.5L10 16M14 13l1.5 1.5L14 16"/>'
    ),
    # A window with a filled dot: a picture of the screen.
    "capture": (
        '<rect x="2.5" y="4" width="19" height="16" rx="3.5"/>'
        '<path d="M2.5 8.5h19"/>'
        '<circle cx="12" cy="14.25" r="3" fill="currentColor" stroke="none"/>'
    ),
}


def page(theme: str, text: dict) -> str:
    t = THEMES[theme]
    e = html.escape
    rows = "\n".join(
        f"""      <div class="row">
        <svg class="pic" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"
             stroke-linecap="round" stroke-linejoin="round">{PICTURES[a["icon"]]}</svg>
        <div><div class="name">{e(a["name"])}</div><div class="why">{e(a["why"])}</div></div>
      </div>"""
        for a in text["approvals"]
    )
    ax, ay = APP_ICON
    bx, _ = APPS_ICON
    arrow_from, arrow_to = ax + ICON_SIZE / 2 + 34, bx - ICON_SIZE / 2 - 34
    return f"""<!doctype html>
<html><head><meta charset="utf-8"><style>
  html, body {{ margin: 0; width: {WIDTH}px; height: {HEIGHT}px; overflow: hidden; }}
  body {{ background: {t["page"]}; color: {t["text"]};
         font: 13px/1.38 -apple-system, BlinkMacSystemFont, "Helvetica Neue", sans-serif;
         -webkit-font-smoothing: antialiased; position: relative; }}
  .title {{ position: absolute; top: 26px; width: 100%; text-align: center;
           font-size: 17px; font-weight: 600; letter-spacing: -0.01em; }}
  .arrow {{ position: absolute; left: {arrow_from}px; top: {ay - 14}px; }}
  .panel {{ position: absolute; left: 36px; right: 36px; top: 254px; padding: 15px 20px 13px;
           background: {t["panel"]}; border: 1px solid {t["panel_edge"]}; border-radius: 14px; }}
  .lead {{ font-weight: 600; margin-bottom: 10px; }}
  .row {{ display: flex; gap: 12px; align-items: flex-start; margin: 0 0 9px; }}
  .pic {{ flex: none; width: 24px; height: 24px; color: {t["accent"]}; margin-top: 1px; }}
  .name {{ font-weight: 600; }}
  .why {{ color: {t["secondary"]}; }}
  .privacy {{ margin-top: 10px; padding-top: 9px; border-top: 1px solid {t["panel_edge"]};
             color: {t["secondary"]}; font-size: 12px; }}
</style></head><body>
  <div class="title">{e(text["title"])}</div>
  <svg class="arrow" width="{arrow_to - arrow_from}" height="28" viewBox="0 0 {arrow_to - arrow_from} 28"
       fill="none" stroke="{t["arrow"]}" stroke-width="3" stroke-linecap="round" stroke-linejoin="round">
    <path d="M2 14H{arrow_to - arrow_from - 4}M{arrow_to - arrow_from - 14} 4l10 10-10 10"/>
  </svg>
  <div class="panel">
    <div class="lead">{e(text["approvals_heading"])}</div>
{rows}
    <div class="privacy">{e(text["privacy"])}</div>
  </div>
</body></html>
"""


def main() -> None:
    here = Path(__file__).resolve().parent
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--theme", choices=sorted(THEMES), default="light")
    ap.add_argument("--text", type=Path, default=here / "dmg" / "text.json")
    ap.add_argument("out", type=Path)
    args = ap.parse_args()
    text = json.loads(args.text.read_text())
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(page(args.theme, text))


if __name__ == "__main__":
    main()
