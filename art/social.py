#!/usr/bin/env python3
"""Writes the info page's link-preview card as HTML, for scripts/social-art.sh to render.

    python3 art/social.py <out.html>

A WIDTH × HEIGHT card (Open Graph and Twitter's large summary image): the app icon and the
name, centred and large (a link preview is small in chat apps), on a quiet background in the info page's light colours (his word 10-13:
just the icon and the name). The icon is read from docs/assets/app-icon.png. Standard
library only.
"""

import argparse
import html
from pathlib import Path

WIDTH, HEIGHT = 1200, 630
ICON_SIZE = 330
NAME = "Stage Manager"
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
    display: flex; flex-direction: column; align-items: center; justify-content: center;
    box-sizing: border-box;
  }}
  img {{ width: {ICON_SIZE}px; height: {ICON_SIZE}px; flex: none; margin-bottom: 26px;
         filter: drop-shadow(0 18px 30px rgba(0,0,0,0.12)); }}
  h1 {{ font-size: 112px; line-height: 1.0; font-weight: 600; letter-spacing: -0.02em; margin: 0; }}
</style></head>
<body>
  <img src="{e(icon)}" alt="">
  <h1>{e(NAME)}</h1>
</body></html>
"""


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("out", type=Path)
    args = ap.parse_args()
    args.out.write_text(page(), encoding="utf-8")


if __name__ == "__main__":
    main()
