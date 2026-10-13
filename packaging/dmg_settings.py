# dmgbuild settings for the Stage Manager DMG (scripts/dmg.sh passes the -D defines).
#
# dmgbuild writes the window layout (.DS_Store) itself, so building the DMG never drives
# Finder through AppleScript (which would ask for an Automation approval).
#
# Defines: app=<path to Stage Manager.app>, root=<repo root>, theme=light|dark.
import os
import sys

root = defines["root"]  # noqa: F821 (dmgbuild injects `defines`)
app = defines["app"]  # noqa: F821
theme = defines.get("theme", "light")  # noqa: F821

sys.path.insert(0, os.path.join(root, "art"))
import dmg as art  # noqa: E402  (the window's geometry, shared with the background art)

app_name = os.path.basename(app)

format = "ULFO"  # lzfse: smaller than zlib, read by every macOS the app supports
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = os.path.join(root, "resources", "AppIcon.icns")  # the mounted volume's icon

background = os.path.join(root, "art", "dmg", "background.png" if theme == "light" else "dark/background.png")
window_rect = ((200, 140), (art.WIDTH, art.HEIGHT))
icon_size = art.ICON_SIZE
text_size = 13
icon_locations = {app_name: art.APP_ICON, "Applications": art.APPS_ICON}

default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
arrange_by = None
label_pos = "bottom"
