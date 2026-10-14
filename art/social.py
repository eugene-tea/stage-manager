#!/usr/bin/env python3
"""Writes the info page's link-preview card as HTML, for scripts/social-art.sh to render.

    python3 art/social.py <out.html>

A WIDTH × HEIGHT card (Open Graph and Twitter's large summary image): the app icon, the name
and the info page's tagline on a quiet background in the info page's light colours. The
icon is read from docs/assets/app-icon.png. Standard library only.
"""

import argparse
import html
from pathlib import Path

WIDTH, HEIGHT = 1200, 630
ICON_SIZE = 300
NAME = "Stage Manager"
# The info page's promise (docs/index.html, `.promise`).
TAGLINE = (
    "Everything Apple should have done to Stage Manager three or four years ago, "
    "done now, and done the Apple way."
)
FOOTER = "Free and open source · macOS 14 Sonoma or later"
ROOT = Path(__file__).resolve().parent.parent


def page() -> str:
    e = html.escape
    icon = (ROOT / "docs/assets/app-icon.png").as_uri()
    return f"""<!doctype html>
<html><head><meta charset="utf-8">
<style>
  html, body {{ margin: 0; width: {WIDTH}px; height: {HEIGHT}px; overflow: hidden; }}
  body {{
    background: radial-gradient(120% 90% at 22% 30%, #ffffff 0%, #f5f5f7 55%, #ececf0 100%);
    color: #1d1d1f;
    font-family: -apple-system, BlinkMacSystemFont, "SF Pro Display", "Helvetica Neue", Helvetica, Arial, sans-serif;
    -webkit-font-smoothing: antialiased;
    display: flex; align-items: center; box-sizing: border-box; padding: 0 88px;
  }}
  img {{ width: {ICON_SIZE}px; height: {ICON_SIZE}px; flex: none; margin-right: 64px;
         filter: drop-shadow(0 18px 30px rgba(0,0,0,0.12)); }}
  h1 {{ font-size: 76px; line-height: 1.05; font-weight: 600; letter-spacing: -0.01em; margin: 0 0 22px; }}
  p {{ font-size: 31px; line-height: 1.32; font-weight: 500; color: #6e6e73; margin: 0; letter-spacing: 0.004em; }}
  .meta {{ font-size: 22px; font-weight: 400; margin-top: 28px; color: #86868b; }}
</style></head>
<body>
  <img src="{e(icon)}" alt="">
  <div>
    <h1>{e(NAME)}</h1>
    <p>{e(TAGLINE)}</p>
    <p class="meta">{e(FOOTER)}</p>
  </div>
</body></html>
"""


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("out", type=Path)
    args = ap.parse_args()
    args.out.write_text(page(), encoding="utf-8")


if __name__ == "__main__":
    main()
