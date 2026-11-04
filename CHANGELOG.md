# Changelog

## Unreleased

- A window Stage Manager had lost its grip on (Music's, right after it launched) is found
  again: it now parks with its stage instead of staying over the next one, and once you
  close it, its card goes instead of showing its old picture and flying a ghost in at every
  switch there.
- When the Mac is very busy, windows no longer fly back and forth between two stages after
  you click a card: macOS's late word that an app became active no longer switches stages
  when that app is no longer in front, or when you have switched since. A ⌘-Tab or Dock
  click still brings its stage, only later.
- The card under the pointer turns solid as it grows, as Apple's does, and lets the
  wallpaper through again when the pointer leaves. `[cards] hover_window_opacity` sets how
  solid; the same value as `window_opacity` keeps the hovered card see-through.
- A window coming back from the corner no longer slides in from the bottom right in apps
  where an accessibility tool has turned on that app's enhanced interface (Chrome, for one):
  Stage Manager turns it off for a moment around its own moves and back on right after, as
  other window managers do.
- The cards on the strip let the wallpaper through, as Apple's do: each window's picture is
  drawn slightly see-through over the wallpaper's colour behind it, and a switch's windows
  take that look as they shrink into their card. `[cards] window_opacity = 1` brings back
  opaque cards; `backdrop_saturation` and `backdrop_brightness` tune the colour.
- A window an app keeps off every screen, such as a headless browser's during automated
  tests, is no longer taken into the current stage, so it no longer slides onto the screen
  when you switch back to that stage. Such windows are left alone, as Apple's Stage Manager
  leaves them.
- Following the Dock, a Finder window that joins a stage no longer pulls that stage to the
  top of the strip: the stage keeps the place of its other app (a stage of Finder windows
  alone still sits at the Finder's place). `[strip] dock_ignore` lists the apps left out
  this way; `[]` lets every app count again.
- After a restart, windows that macOS reopened on another Desktop than their own are listed
  in the menu bar menu, with a small dot on the icon, so you can drag them back in Mission
  Control. An entry goes away once its window is on its own Desktop; Dismiss clears the list.
  `[stages] restore_notice = false` turns it off.
- A window arriving in a switch no longer jumps a little as the motion ends: it takes its
  place only once its picture is right over it.
- Moving windows carry the same shadow as the windows themselves, the focused window's
  larger one included, so the shadow no longer wobbles as a switch starts and ends.
- The end of a switch no longer flickers: the arriving window comes to rest without a last
  small snap, shows in its focused look as its picture fades away, and a hidden app's window
  keeps its shadow until it is really on screen.
- When a stage's windows overlap, the window behind no longer shows faintly through the one
  in front at the end of a switch.
- The arriving window's picture keeps its shadow until the window itself is on screen, so the
  shadow no longer lightens for a moment at the end of a switch.
- In Dark Mode, moving windows carry the darker shadow and the thin light edge macOS gives
  windows there, so the edges no longer flicker as a switch ends.
- A window that an app draws dark while the system is light (or light while it is dark), such
  as Messages with a dark chat background, now moves with its own shadow, so its edges no
  longer flicker as a switch ends.
- A hidden app's window arriving in a switch keeps its picture's shadow until it is drawn as
  the focused window, so the shadow no longer lightens for a frame.
- With hidden parked apps, a switch to a hidden app's stage now always gives it the keyboard:
  an app that does not take its activation is asked again.
- When apps are slow to answer (right after a restart of Stage Manager, or under heavy
  load), stages no longer switch by themselves: a window that was on screen all along but
  found late goes quietly to its stage, a window focused in an app behind or a hidden app no
  longer brings its stage, and quick clicks on several cards never let an earlier click's
  window take the keyboard back.
- An automatic update no longer installs while the screen is locked, when hidden windows
  could not come back until you unlocked; it waits until you are back and step away again.
- The patch that keeps the corner clean is smaller: it reaches past the hidden windows' edges
  only as far as their shadow does.
- A desktop icon dropped into the corner is no longer cut in half for a moment: the patch steps
  aside until it has redrawn the corner with the icon.

## 1.5.0 — 2026-10-29

- The corner stays clean on any wallpaper when Screen Recording is allowed, also with desktop
  icons or widgets there (they still open and drag), at dawn and dusk on wallpapers that
  follow the sun, and through Show Desktop and back.
- Clicking an app in the Dock, or ⌘-Tab, brings its window to the front with the keyboard,
  also when it shares its stage with other apps. Cards show their front window on top, and a
  stage's windows land in the order its card shows.
- Hidden windows come back exactly where you left them: when Stage Manager quits or crashes,
  after a restart on a display of another size, and for a window partly off the screen, one
  pushed into the corner, a dialog, or one whose app was busy.
- A hidden window that its app moves back onto the screen goes back into the corner within a
  second; if you are working in it, it joins your stage.
- A window you minimize with ⌘M keeps its card in the strip.
- With named stages, stages moved together to another Desktop in Mission Control arrive with
  their windows showing.
- A hung app no longer freezes Stage Manager while its windows are brought back.
- A config file with a mistake in it keeps your last working settings instead of the defaults.
  Whole-number settings accept `50.0`, and a hotkey of Shift and a key alone is refused.
- Updates: Stage Manager opens again after an update even if it was moved meanwhile, and an
  update installed by hand no longer leaves its disk image behind.
- Without Screen Recording, switching stages shows plain window shapes again. With the strip
  hidden, no pictures of your windows are taken in the background.

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
