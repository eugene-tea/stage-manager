# Changelog

## Unreleased

- The shadow under the app icons on cards is lighter and tighter, as Apple's is.
- A card dragged quickly up or down the strip stays in one piece: its picture no longer runs
  ahead of its app icons and jumps back onto them.

## 1.8.2 — 2026-11-09

- Closing a notification no longer hides your windows.
- Alerts from Settings, such as Check for Updates…, now open attached to the Settings window.
- A window coming in from the strip no longer shows a darker shadow for a moment as it lands.

## 1.8.1 — 2026-11-09

- Holding the mouse down on the desktop, or dragging from it (selecting icons, moving a
  file), no longer brings back a stage with a Finder window.

## 1.8.0 — 2026-11-09

- A switch moves as Apple's does, matched frame by frame to recordings of both. A window
  coming out of its card moves out over the desktop while still small, as if far behind the
  screen, then comes forward to its place; going back, it shrinks first, then slides into its
  card. It keeps its shape (it no longer stretches wide in flight) and turns about its
  middle. The windows you leave start shrinking on the same frame as the new ones start
  growing, and the new stage's windows no longer jump in early at full size when their app
  has another window out of sight.
- Cards look like Apple's: rounder corners and a shadow you barely see. A card of two apps
  stays about a single card's size, the window you used last in front and the other peeking
  out above it on the side where it sits on your screen, instead of the two side by side. A
  card under the pointer grows a little more and eases in; on a card of two apps each window
  grows on its own and the icons stay put. At launch, a card still waiting for its picture is
  faint glass, not a dark grey block. Nothing on a card is cut off at the strip's right edge.
- In Dark Mode the cards show the apps' dark icons, as the Dock does, and they follow the icon
  style chosen in System Settings → Appearance.
- The strip comes back sooner when you rest the pointer at the left edge (after about 180 ms,
  as Apple's, not 320), and slides away only once a window covers the cards' left edge
  (about 20 points from the screen's edge, not 50).
- A card or window dragged out of the strip turns into its window as it leaves the strip, as
  with Apple's: at its own size and tilted as the card, just right of the pointer, it slides
  under the pointer in about a tenth of a second and straightens. Before, a card-sized
  picture grew as you went. A window you drag over the strip stays above the cards, and a
  card dropped on your other screen opens its windows there.
- One window of a card that holds several comes off alone as soon as you drag it, also
  inside the strip: let it go in the middle to join your stage, between two cards for a
  stage of its own, or on another card to join that stage. Drag a card by its app icons to
  move the whole card. A window or card dropped onto a card now goes in front of that
  stage's windows, not behind them.
- VoiceOver reads the strip: each card is a button that says its stage and apps ("Stage 3:
  Safari, Notes") and switches to it; its actions show all of one app's windows. In Settings,
  VoiceOver reads each control by its row's name, and Tab reaches every control with Full
  Keyboard Access; in the welcome window each Allow button says what it allows.
- Stage Manager follows Reduce Motion: windows fade in and out where they are instead of
  flying, the strip fades instead of sliding, and cards no longer grow, glide or slide. With
  Reduce Transparency or Increase Contrast on, the cards are solid.
- A downloaded update also installs when you quit Stage Manager; the new version starts the
  next time you open it. Logging out, restarting or shutting down never installs it. Before,
  an update that waited for you to step away was thrown away when you quit. One downloaded
  earlier but never installed is checked again within a minute of opening, not a day later.
- With "Click wallpaper to reveal desktop: Only in Stage Manager", a press a hair outside a
  window is its edge: resizing no longer sends your stage to the strip mid-drag (VS Code came
  back with its side panels folded), and a click on the wallpaper reveals the desktop when
  you let go, as with Apple's, never for a drag or a press held about half a second. Pressing a file or folder on the desktop no longer
  reveals it either: you can select it or drag it onto a window, as with Apple's.
- Clicks on the wallpaper and the Dock work after a screenshot, and while Notification Center
  keeps an invisible window over the screen: macOS leaves such windows for minutes, and
  clicks under them were taken for clicks on a window.
- A window no longer ends up off the edge of the screen after macOS's Show Desktop (a
  wallpaper click with "Always"): the place macOS pushes it to is no longer taken as its own.
- Minimizing a window ends in one motion after macOS's genie: a small picture of it flies
  from its Dock tile into its card. A full-size picture used to fly from where the window had
  been, and the window came back out of the Dock and crossed the screen to the corner.
- An app's activation by macOS or ⌘Tab soon after a switch to its stage is no longer taken
  for Stage Manager's own and ignored, which left its stage where it was.
- Settings' panes are plain, as in Finder's settings, without the rounded box around the
  rows, and say what each "Click wallpaper to reveal desktop" value does.
- `[animation] reduce_motion` ("system", "on" or "off") decides Reduce Motion for Stage
  Manager alone. `[drag] grow_distance` gives way to `[drag] appear_ms`, and
  `[windows] hide_parked_apps` is gone (off by default and never in Settings).
- In a switch, a see-through Dock no longer shows the edge of a window waiting in the corner,
  and a window kept on top of all others stays on top while the strip shows over windows.
- An app that opens two windows at once comes forward with the one it opened last in front.
- A closed window about the size of another screen is no longer taken for a full-screen one.
- With `[log] level = "debug"`, the log names any task that holds the app's window thread
  for 100 ms or more, and how long each save of your stages takes.

## 1.7.0 — 2026-11-06

- After a restart, a window its app reopens at a fraction of the size it had (VS Code, say)
  gets its size and place back the first time it shows, instead of you resizing it by
  hand. Only a window recognised by its name, on the screen it was on, and only once: a
  window you move or resize first keeps yours. `[restore] saved_size = false` keeps the
  size the app gives it.
- A logout or restart that is cancelled after macOS has already quit Stage Manager (another
  app asking about an unsaved document, and you click Cancel) no longer leaves every window
  in one pile with no stages: Stage Manager opens again by itself and brings back the stages
  it saved at the logout. A logout that goes on opens nothing, and macOS never waits for it.
- When macOS stops reporting windows to apps (it can happen after it recovers from a hang,
  until you log out), the menu says so instead of Stage Manager just seeming broken. Your
  saved stages are kept meanwhile, a click no longer throws them away, and if macOS starts
  reporting windows again they come back by themselves.
- After a restart, windows go to their stages as soon as their apps answer, not seconds
  later: apps still busy reopening at the login are asked, and checked for windows a crash
  left hidden, on threads of their own, so one that does not answer yet no longer holds up
  the others' windows or anything else the app does.
- Every screen has its own strip and stages, as Apple's Stage Manager does: a window
  dragged to another screen starts a stage of its own there once you let it go, a card
  click changes only its screen, each screen hides its stages in a corner of its own, and
  closing or opening the lid moves stages between screens the way Apple's does. After a
  restart each screen's stages come back on that screen, their windows in their places on
  it, also when the screens were rearranged in Displays settings in between.
  `[displays] main_only` keeps the
  old way: the main screen only.
- A second screen looks and behaves as the main one: its cards let its own wallpaper
  through, the corner where its hidden windows wait is covered by a patch of its own
  desktop, windows stay clear of its own strip, a Dock click to a Desktop there arranges
  it before it slides in, and a full-screen app on one screen no longer holds up
  windows opening on the other. After a restart the log says when a Desktop came back on
  another screen than it was saved on.
- After a restart with two screens, a window its app reopens on the other screen goes back
  to the screen its stage was on, in its place there. Two windows of one app (two VS Code
  workspaces, say) come back as the two stages they were, even when an editor shows a file
  in its title now, or gets its title only a moment after it opens.
- After a restart, a screen with one Desktop (a laptop's, say) gets its stages back even when
  its apps open their windows only after Stage Manager has started: they no longer count as
  from another Desktop, and its front window, reopened on the other screen, goes back to it.
- After a restart, a window its app reopens under another name at its saved place (Fork
  showing another repository) goes back to its own stage instead of joining the app's other
  window, and "new apps join the current stage" leaves the windows apps reopen at the login
  to the restore.
- When you restart or log out with "Reopen windows when logging back in" on, macOS reopens
  Stage Manager with your other apps, instead of opening it after them as a login item:
  their windows go straight to their stages rather than showing piled up first. For that it
  shows in the Dock for a moment as you log out; `[app] reopen_with_apps` turns it off.
- The strip stays up under a window that covers its cards, as Apple's does, and slides away
  only once the window comes within 50 pt of the screen's left edge.
- The menu no longer lists windows a restart reopened on another Desktop.
- A slow logout or restart (an app asking about unsaved changes) is no longer taken for a
  cancelled one after two minutes: while apps still quit, Stage Manager keeps its windows
  where they are and stays on the list of apps macOS reopens. After a logout that was
  cancelled, an app slow to answer gets its windows arranged once it answers, without a
  click on it first.
- The helper that brings windows back if Stage Manager crashes is started again if it
  stops, and a crash is never mistaken for a clean quit. Opening Stage Manager while an
  earlier copy hangs in its quit gives up after 90 s instead of waiting with nothing
  running, and launch at login is tried again at the next launch when macOS refused it the
  first time.
- A Desktop you did not visit between two restarts keeps its stages: they no longer
  disappear at the second restart (up to 30 days since you last visited it).
- After a restart, a window an app opens once it has settled (Finder's window for a folder
  an hour later, say) is a new window, as Apple's Stage Manager places it, not a window
  pulled into a stage of a Desktop you have not visited yet. That Desktop still gets its
  own windows back when you go there.
- With two screens, a window dragged onto the second screen's strip becomes a card there,
  a click on a card's app icon on the second screen gathers that app's windows from that
  screen's stages only, and the stage shortcuts (and `ctl switch`) count and switch the
  stages of the screen you are typing on.
- With two screens, a full-screen app on one no longer stops the other: its cards, Shift-
  clicks, wallpaper clicks and the stage shortcuts typed there work as usual. A pointer on a
  screen to the left of a hidden strip no longer reveals that strip or keeps it open.
- `[displays] main_only` takes effect at the next launch, and says so: changing it while
  Stage Manager runs used to leave the two screens half managed.
- A wallpaper click reveals the desktop on the Dock's screen too: the Dock's invisible
  screen-wide window no longer counts as a window over the wallpaper, and a click away from
  the Dock no longer asks the Dock what was clicked.
- When the main display changes (the menu bar moved to another screen, or a laptop that was
  the main display closed onto an external screen), hidden windows come back where they
  were on their screen, instead of about a screen away.
- When saving your stages fails for a moment (a full disk), the save is tried again until
  it works, instead of waiting for your next change.
- A window an earlier version left with only a sliver showing at a screen's corner comes
  back as a card, and a window brought back there comes wholly on screen in that corner.

## 1.6.0 — 2026-11-05

- The cards on the strip let the wallpaper through as Apple's do, and the card under the
  pointer turns solid as it grows.
- After a restart, windows that macOS reopened on another Desktop than their own are listed
  in the menu bar menu, with a dot on the icon, so you can move them back.
- Switches end cleanly: no last jump or flicker as the windows land, steady shadows (in Dark
  Mode and in apps drawn dark too), and overlapping windows no longer show through each other.
- When the Mac is very busy, stages no longer switch by themselves or bounce between two
  stages after you click a card. A ⌘-Tab or Dock click still brings its stage, only later.
- In Chrome and other apps whose enhanced accessibility interface is on, windows no longer
  slide in from the bottom right.
- A window Stage Manager had lost track of (Music's, right after it launched) goes into the
  corner with its stage again, and once you close it, its card goes too.
- A Finder window that joins a stage no longer pulls that stage to the top when the strip
  follows the Dock order.
- Windows an app keeps off every screen, such as a headless browser's during automated tests,
  are left alone instead of sliding onto the screen.
- An automatic update waits until the screen is unlocked, so hidden windows can always come
  back.
- The patch that keeps the corner clean is smaller, and a desktop icon dropped into the
  corner is no longer cut in half for a moment.

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
