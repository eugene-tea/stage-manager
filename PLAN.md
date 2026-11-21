# Stage Manager: plan

What's still to do. What shipped is in [CHANGELOG.md](CHANGELOG.md), how the app is built is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), how it should behave is in [docs/UX-SPEC.md](docs/UX-SPEC.md),
and what users asked for is in [docs/FEATURE-REQUESTS.md](docs/FEATURE-REQUESTS.md).

## 1.x: everything known broken or unchecked

Version 1 is Apple's Stage Manager, copied with nothing left: fixes and checks only; new features
wait for 2.0. Known gaps that can't be closed from our side are in UX-SPEC, "Known gaps".

**When it happens, no session needed:**

- **The next real restart.** In that day's log, check:
  - the launch's `recovery scan` line shows `restored=0` (no window came back hidden)
  - no `app activated … queued_ms` over 100 in the first two minutes, and if there is one, the
    `slow WM task` line names what held the thread
  - the `restore:` summary line
- **A screen plugged in or out (one screen at a time too).** In that day's log, within the
  minute after it: every `display: a hide while a screen gone is watched` line with
  `mid_change: false` has `home` equal to `on` when `on` is set and nothing was kept, and no
  `window left this Space` follows a Dock click or ⌘Tab to another Desktop.
- **Apple's, one screen at a time** (UX-SPEC, Screens): with the lid and the external screen,
  by hand.
- **"macOS isn't reporting windows" shows in the menu again.** Quit Stage Manager, then count windows with
  System Events (`osascript -e 'tell application "System Events" to count windows of every process'`). If
  any are listed, our quit fixed it: the menu line then says "Quit and reopen Stage Manager", and the bug
  is ours. If none are listed, lock and unlock the screen and count again.

## 1.9: closer to Apple's feel

- **A newly opened app's stage comes as its window shows** (Corner engine; worth doing only
  if the Spaces engine does not ship or S6 shows it still matters). After 1.8.5's fixes (the AX ask
  of a just-opened app every 40 ms, a new window looked at every fast tick), the old stage
  still starts to leave 60 to 170 ms after the window is found, and the window sits over it
  meanwhile (Grok Bot, 11-13 09:54); Apple's swaps cleanly (its Stage Manager is in the
  window server, so the window can wait for the swap). Three trims, about 30 to 50 ms left:
  - the outgoing windows' pictures ready ahead (09:54:24: 106 ms building them before the
    motion could start);
  - no park-zone rescan or other AX pass while a new window waits for its stage (09:54:38:
    a rescan of two apps that did not answer held the decision 54 ms);
  - for a switch a new window starts, the outgoing pictures drawn under that window, not in
    the layer above every window: the window stays in front while the old stage shrinks
    away behind it, instead of the old window's picture covering it for a moment. Check
    Apple's order first (a recording at 120 fps).
- **Apple's Stage Manager on and off, live.** The stages now come back from `state.json`
  set aside at the stand-down (CHANGELOG). Left: a live check of the round trip with the
  owner turning Apple's on and off by hand (the suite's toggle writes
  `com.apple.WindowManager`, which is not done on the owner's Mac).
- **Mission Control under the Spaces engine: measure, then decide the default.** Built
  (`[windows] mission_control_stages`, CHANGELOG): the hidden stages' windows are lent to the
  Desktop while Mission Control or App Exposé shows. Run `SM_ENGINE=spaces
  scripts/test-stages.sh mission-probe` (it prints what each outcome means), then `mission`.
  Mission Control takes late windows: keep `"shown"`. Only windows there at the key: make
  `"key"` the default. Neither: `"off"`, and keep hidden windows on the Desktop another way.
  Left after that: a faster sign for a gesture, a hot corner and the Dock's item (up to 2 s
  at rest now, the shown line's `found_within_ms`; 11-20 07:49:14 no notice of the Dock's
  came, observed since 07:48:20. To measure: whether the input monitor's global monitor gets
  `NSEventType::Gesture`/`Swipe` events for a four-finger swipe up, and what one look at the
  on-screen list costs at 4 Hz at rest, `measure_stages.py`); whether a window parked at
  its card at the key (the real key, `"shown"`) and put home once Mission Control shows flies
  from its card (found at the listing it is laid out with no flight, 11-20; the key is heard
  now, by the HID tap: run `mission-probe` again); should the close show them at their cards, move them to the corner
  at the close's key (Escape, the shortcut again), as a pick's frames are set at its
  activation, and measure whether a move while Mission Control shows lays it out again; why
  parks miss (the `a hidden window left hidden` lines: listed at
  its frame after the park is a late listing or a refusal, away from it out of the zone a
  clamp, perhaps above the Dock Mission Control shows; whether the retry takes); a hidden
  window dragged to another Desktop inside Mission Control (it goes back to its stage); hidden dialogs and panels
  (standard windows only are lent); App Exposé lends every hidden window, not only the app
  in front's.
- **Mission Control's keys: the real F3.** The posted ⌃↑ reaches the key tap (11-20); press
  the real F3 and ⌃↑ and look for the log's `mission-control: trigger seen ... by="the key
  tap"`; with F3 not heard, `ctl state`'s `mission_control.tap.system_defined_seen` says
  whether it comes as a media key.
- **Video calls in a hidden stage (Spaces engine).** A stage's windows off every Desktop
  count as hidden to their apps: Google Meet in Safari stops drawing other people's video and
  its card shows grey tiles (11-16); Apple's keeps them playing. Option, the owner's call: a
  hidden call window (or the apps the config names) stays on its Desktop, parked in the
  corner as the Corner engine does.
- **Less work while nobody looks: CPU and battery.** 4 to 5 % CPU at rest on 11-13; since
  1.9 the thumbnails come at events only (none at rest: a day's log of 11-19 counts no capture
  in hours with no switch), the Spaces engine takes no full-size picture on a timer, and the
  Dock's order is read at events. Aim: 1 to 2 %, with no card shown stale and no switch slower.
  - Run `RUNS=1 ENGINES=spaces SM_REST_S=300 scripts/measure.sh covered` (and `rest`) and write
    the numbers in UX-SPEC.
  - Full-size switch pictures, the park corner only (every 2 s of the front stage's windows,
    whether the strip shows or not): keep them (a Dock click or ⌘Tab switch has no pointer
    near the strip to warn), but skip a window that has not changed since its last picture if
    that can be told cheaply; measure before deciding.
  - Video calls: check whether our captures make a call's video stutter (it lags at times
    with ours where Apple's is smooth). The call stand-in missed no refresh at rest or
    switching (11-14, 11-18), and at rest only the park corner's 2 s pictures touch a call
    window in front; so a real call, recorded with and without captures, dropped frames
    counted at each capture round; if they show, capture a window that plays video or a call
    less often, or never at full size.
  - The Dock's bar is still read every 2 s for the inert slivers (its lists' frames, a few
    Accessibility calls): read it at a display change, the Dock's preferences file changing
    and an app launch instead, if its auto-hide does not move it.
  - Measure on battery (Activity Monitor's Energy Impact, before and after) and write the
    numbers in UX-SPEC.

## 2.0: new features

- **A right-click menu on cards** (R4): Bring Here, New Stage, Keep on Every Stage, Name Stage…, Close
  Window, Quit App. A named stage shows its name on hover (R7).
- **Stages in the menu bar menu**, click one to switch (R2).
- **Drop a window on a card** to add it to that stage (R3b). Apple's has none: its strip hides
  instead (11-09).
- **"Only the main screen" in Settings** (`[displays] main_only`, off; R8). The setting is read at launch, so
  turning it on or off quits the app (every window comes back) and opens it again, as an update does. Its
  line in Settings says so.
- **Scroll the strip past its capacity**, with a "+N" mark (R5).
- **No stage switches the user didn't ask for** (R6): an activation with no click or key press just before
  it doesn't switch stages, and that app's card glows.
- **⌘M only where it is the app's Minimize.** ⌘M is taken for "minimize to the strip" in any
  managed window with a minimize button, also in apps that bind ⌘M to something else (an
  image editor's Curves, a remote desktop that passes it on). Read each app's menu bar once
  per activation on the WM thread (the item whose `AXMenuItemCmdChar` is M with no other
  modifier must be the Window menu's minimize item, told by position or identifier, never
  by its localized title), publish the pids to the key tap as `Minimizable` is, and take the
  key only for those; an app not read yet keeps its key.
- **Install a ready update at the lock.** A Mac whose screen locks or sleeps within two
  minutes never reaches the automatic install (120 s idle, unlocked and awake); it installs
  at the user's Quit only. Start the install when the lock is announced, before
  Accessibility stops moving windows (every window back first, as at a logout's head
  start), or shorten the idle rule; decide which, and what a lock cancelled mid-install
  leaves on screen.
- **A Dock launch opens on the Dock's screen**, an option, off. A launched app places its first
  window itself, at its remembered frame or on the key window's screen, so an app opened from
  the Dock on the second screen can open on the main one (Reminders and Weather, 11-12; the
  arrival is placed as-is). With the option on, a window that arrives right after a Dock
  click on another screen moves there, sized to fit, before it is shown. Check Apple's first;
  never move a window whose app restores it to a screen the user chose.
- **One stage across all screens** (R8, second half), an option, off. One strip, on the main screen. A
  switch brings a stage's windows back on every screen. Still open: what it means when "Displays have
  separate Spaces" is on, and the lid and restart rules once a window moved to another screen keeps its
  stage.
