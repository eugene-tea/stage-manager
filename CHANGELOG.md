# Changelog

## Unreleased

- With Screen Recording allowed, the corner stays clean on any wallpaper, also with desktop
  icons or widgets there, which still open and drag.
- The corner stays clean at dawn and dusk on wallpapers that follow the sun.
- Clicking an app in the Dock, or switching to it with ⌘-Tab, brings that app's window to the
  front with the keyboard, also when it shares its stage with other apps.
- A card shows its front window on top, also when its stage holds two apps, and the windows
  keep that order as they grow from the card.
- A stage's windows land in the order its card shows, also with three apps.
- With named stages set, stages moved together to another Desktop in Mission Control arrive
  with their windows showing, instead of all hidden behind an empty named stage.
- A dialog left in a stage still waiting for its windows after a relaunch comes back on
  screen when a rule places that stage's last awaited window elsewhere.
- An app that was alone on a Desktop you left still comes back after a restart when it quit
  just before it, as apps on other Desktops do.
- After a restart on a display of another size, a window moved clear of the strip still goes
  back to where you had it when Stage Manager quits.
- A config file with a mistake in it no longer brings back the defaults when Stage Manager
  starts: your last working settings stay, and update checks stay off if they can't be read.
- Whole-number settings accept a number written like `50.0`.
- A hotkey of Shift and a key alone is refused (it would take that key from your typing);
  hotkeys need Control, Option or Command, or a function key.
- If Stage Manager is moved while an update waits to install, it opens again afterwards
  instead of staying closed.
- An update you install by hand no longer leaves its disk image in the temporary folder.
- Quitting Stage Manager brings every hidden window back where you left it, also a window
  whose app was slow to move it, or a dialog.
- After a crash, hidden windows come back exactly where they were, and a window you moved
  meanwhile stays where you put it.
- An app that reopens a window in the corner (it saved the hidden window's place when it
  quit) has it put back where the window was, for a day after the app quit.
- Quitting Stage Manager no longer moves a window you pushed almost off the corner, nor one
  already back in place on another Desktop.
- On a main screen of another size than before, a window brought back from the corner opens
  centered instead of at a place from the other screen.
- A hidden window that its app moves back onto the screen by itself goes back into the corner
  within a second; if you are working in it, it stays and joins the stage you are in. Before,
  it sat mid-screen with no card until you changed Desktop.
- A hidden window that could not be moved into the corner (its app busy) is tried again, with
  pauses, rather than counted hidden while it stays on screen.
- A window dragged out of the corner right after you log in stays where you dropped it.
- While a window that did not come back from the corner is tried again, its edge stays
  covered and ignores clicks.
- A hung app no longer freezes Stage Manager while its windows are brought back.
- When you switch apps with ⌘-Tab or the Dock, the corner is covered again at once.
- A window left partly off the screen, or taller than it, comes back exactly where you left
  it, not pulled onto the screen and shrunk.
- A window pushed into the bottom-right corner comes back by the corner, not where it was
  before.
- A window of an app busy at a switch comes back where you last moved it.
- A bigger Dock no longer sends a window kept by the corner to the middle of the screen.
- A dialog opened on a window in another stage comes back on that window.
- After a window is moved here in Mission Control, the stage it puts away comes back where
  you left its windows.
- A card dropped in the main area stays where you dropped it when Stage Manager quits.
- Without Screen Recording, switching stages moves plain window shapes again instead of
  showing nothing.
- With the strip hidden, Stage Manager no longer takes pictures of your windows in the
  background.

## 1.4.0 — 2026-10-23

- New apps can open in the current stage: turn on **Open new apps in the current stage**
  (Settings → Stages) and a newly opened app joins the stage you are in. Hold Shift while
  opening an app to give it a stage of its own. Off by default, as Apple's.
- Stages can keep their places: set **Strip order** to **Keep in Place** (Settings → Stages)
  and each stage stays in its own place in the strip, whichever card you click.
- Or the strip can follow your Dock: **Strip order** → **Follow the Dock** puts the stages in
  the order their apps sit in the Dock. Rearrange the Dock and the cards follow.
- Settings has a toolbar of panes, as Mac apps' settings do: General, Stage Manager, Stages,
  Windows and Advanced.
- Going to another Desktop no longer pulls you back to the one you left.
- The edges of hidden windows no longer show in the corner while a Desktop slides in or a
  switch ends, including on Apple's dynamic wallpapers.
- An app that quits without macOS saying so (Home, which quits with its last window) no
  longer leaves a gray card behind.
- After a restart, an app macOS did not reopen opens where any new app does when you open it
  later, instead of going back into an old stage.
- A config file that can't be read no longer turns Settings back to the defaults: the last
  good config stays.
- An update that cannot put the previous version back no longer removes it: both copies stay,
  and the note at the next launch says where.

## 1.3.0 — 2026-10-21

- Your stages come back after a restart. When you log out, restart or shut down, Stage
  Manager brings every window back first, so macOS saves them where they really are; after
  you log back in, the windows macOS reopens go back into their saved stages, on each Desktop.
  Windows that some apps (VS Code, Slack and other Electron apps) reopen on the wrong Desktop
  join their saved stage where they appear. Turn it off with `[stages] restore_after_restart`.
- ⌘-Tab and Dock clicks grow the app's windows from their card, as Apple's do; the corner
  stays clean while they do.
- A card shows a window without a gray box around it when a popup is open.
- Quitting is quieter and cleaner: hidden tabs and other apps' helper windows no longer count
  as windows left in the corner, and a leftover safety helper hands its windows to the next
  copy of Stage Manager instead of waiting for days.
- A window closed while its app is not responding leaves its stage (no empty gray card).
- Window pictures retry less often after failures and pause while the Mac is locked or
  asleep, which saves battery.
- The menu bar icon is the same height as Apple's own symbols.

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
