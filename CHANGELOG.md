# Changelog

## 1.2.0 — 2026-10-13

- Automatic updates: Stage Manager downloads a new version from GitHub, checks that it is
  signed by its developer and notarized by Apple, brings your windows back, replaces itself
  and relaunches. It asks first; Settings can make it install on its own.
- Only one copy manages your windows; launching it again opens the running copy's Settings.
- Safer quits and crashes: a crash can no longer leave the app hanging with windows in the
  corner, a busy app no longer slows switching, and the saved stages are never lost to a
  failed save.
- Privacy: the log no longer records window titles (only a short code), and Stage Manager's
  files are readable by you only.
- The clean corner stays inert after you type a password.
- Apple silicon Macs only (said on the page and in Homebrew).

## 1.1.0 — 2026-10-13

- Desktops keep their stages across restarts, and when you move windows between Desktops.
  A window moved to another Desktop goes where a new window of its app would open there.
- ⌘-Tab, Dock and click switches animate again: the stage you leave shrinks into the strip
  behind the app you switch to.
- Quitting brings back every window from the corner, even from an app that is busy at that
  moment; a window that cannot be reached yet is brought back as soon as it can be.
- The strip stays in front, and stays put under the pointer, while you click through its
  cards with windows covering it.
- The clean corner hides during Show Desktop and comes back with Mission Control.
- New documentation: a short user guide, a configuration reference and development notes.

## 1.0.0

- **2026-10-12** — Day one of daily use. The strip matches Apple's down to the card tilt,
  and each Desktop gets its own stages and strip. Switches animate. Cards can be reordered,
  and dropping one card on another merges the two stages. The strip hides during Mission
  Control. Safety layers mean a window is never left hidden, not even after a crash. Logs
  are kept per day, with a "Mark a Problem" menu item.
- **2026-10-13** — Version 1.0.0, signed and notarized. A menu bar app with Settings, About,
  update checks and launch at login; it stands down while Apple's Stage Manager is on. A
  Dock click to an app on another Desktop arranges that Desktop before it slides in. The
  parked windows' corner is clean: their slivers ignore clicks, and a patch drawn from the
  wallpaper's own file covers them, with no screen recording.
