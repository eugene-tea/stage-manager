# Stage Manager: plan

What's still to do. What shipped is in [CHANGELOG.md](CHANGELOG.md), how the app is built is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), how it should behave is in [docs/UX-SPEC.md](docs/UX-SPEC.md),
and what users asked for is in [docs/FEATURE-REQUESTS.md](docs/FEATURE-REQUESTS.md).

## 1.8.x: everything known broken or unchecked

Version 1 is Apple's Stage Manager, copied with nothing left: fixes and checks only; new features
wait for 2.0. Nothing is open (1.8.3, 11-09). Known gaps that can't be closed from our side are in
UX-SPEC, "Known gaps".

**When it happens, no session needed:**

- **The next real restart.** In that day's log, check:
  - the launch's `recovery scan` line shows `restored=0` (no window came back hidden)
  - no `app activated … queued_ms` over 100 in the first two minutes, and if there is one, the
    `slow WM task` line names what held the thread
  - the `restore:` summary line
- **"macOS isn't reporting windows" shows in the menu again.** Quit Stage Manager, then count windows with
  System Events (`osascript -e 'tell application "System Events" to count windows of every process'`). If
  any are listed, our quit fixed it: the menu line then says "Quit and reopen Stage Manager", and the bug
  is ours. If none are listed, lock and unlock the screen and count again.

## 1.9: closer to Apple's feel

- **First: two window engines behind one seam.** Internal: users see the same app. A stage
  switch goes through the window server's own Spaces where they work (the Spaces engine) and
  through today's Accessibility corner park where they don't (the Corner engine). Emcee
  (1.1.1) loads SkyLight and WindowManagement and calls their bridged Space operations
  (create, destroy, hide, show, add windows, transform, absolute level:
  `SLSBridgedSpace…Operation`), plus `SLSCopySpacesForWindows` and
  `SLSRegisterConnectionNotifyProc`, with no SIP change. A stage as a hidden Space swaps in
  one window-server step (Apple's clean swap), places a new window in its stage in one call,
  and, if the transform scales live windows, animates without full-size pictures. About a
  quarter of the source (27k of 113k lines: the corner patch, picture motion, park and
  slivers, the park side of safety) moves into the Corner engine and stops running under
  Spaces.
  - **The seam (S1, Corner only, no behaviour change; can ride along in 1.8.x):** one
    `WindowEngine` trait (hide, show, place a new window, is it hidden, animate, bring
    everything back, health) with exactly two implementations. `Action::Park { frame }`
    becomes `Hide`, unparks `Show`; the model's park-zone tests go through `is_hidden`.
    The model, controller, strip, rules, thumbnails, `state.json` and the guardian protocol
    are shared and never ask which engine runs. Done when `make check`, the scenario runner
    and the live suite give today's results and logs.
  - **Spike (S0, a branch, beside S1):** `stage-manager probe-spaces`, fixture windows only.
    Another app's window into our Space, hidden and shown, no SIP change (no: Corner only);
    hidden windows after a kill -9; the transform on a live window at 120 Hz; a new window
    placed before it first shows; Mission Control, Desktops, full screen, ⌘Tab, a Dock click
    on a hidden window; Apple's Stage Manager turned on meanwhile; switch time and CPU
    against Corner. Go or no-go from the numbers.
  - **Then:** tests on both (a fake of each engine in `testkit`, the scenario runner over
    both, `test-stages.sh --engine`; nuance cases per engine: the park zone, slivers,
    patch and pictures; Space cleanup, a kill -9, the probe, a fallback mid-run); the Spaces
    engine (calls loaded at run time, a Space per stage per screen and Desktop, the
    guardian's bring-back); live motion through the transform if the spike holds, else the
    pictures through the same `animate`; probe and fallback; measure (CPU at rest, switch
    time, a new app's overlap, Energy Impact, a call's dropped frames) into UX-SPEC.
  - **Which engine runs:** a probe at every launch (symbols resolve, a trial Space with our
    own window works), the reason logged and in `ctl state`. A check that fails while running
    (a window not where the Space call put it) brings every window back and runs Corner for
    the rest of the run; the next launch probes again. No list of checked macOS versions,
    so nobody has to check each macOS update before users get it. `[windows] engine =
    "spaces"` runs the probe and the Spaces engine; `"accessibility"`, the default, is the
    way out, listed under troubleshooting.
  - Quit, a crash, a logout and a stand-down use the same bring-back, whichever engine runs.
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
  at rest now; the log says whether a notice of the Dock's comes first, and the Dock's
  notices are observed only after the first ⌘ press); a hidden window dragged to another
  Desktop inside Mission Control (it goes back to its stage); hidden dialogs and panels
  (standard windows only are lent); App Exposé lends every hidden window, not only the app
  in front's.
- **Video calls in a hidden stage (Spaces engine).** A stage's windows off every Desktop
  count as hidden to their apps: Google Meet in Safari stops drawing other people's video and
  its card shows grey tiles (11-16); Apple's keeps them playing. Option, the owner's call: a
  hidden call window (or the apps the config names) stays on its Desktop, parked in the
  corner as the Corner engine does.
- **The strip revealed at the edge, flat (Apple's, 11-16).** With the strip hidden (a window
  over its column), a pointer at the left edge brings Apple's strip in with its cards flat,
  facing the viewer, and larger than in the tilted strip; ours slides the tilted strip back.
  Record Apple's (a capture of the reveal: the cards' pose, size, spacing, shadow, the slide
  in and out and its timing, and what a card does when the pointer leaves), then match it,
  pose as well as speed (track the cards' corners frame by frame, not only the timing).
- **Card pictures: spread like Apple's, as switchable variants.** Ours follow the windows'
  real layout, so a window mostly behind another hardly shows (VS Code over Terminal, 11-16);
  Apple's spreads them. Variants for the owner to compare from the menu: the real layout,
  Apple's spread, spread only when a window is mostly covered.
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

- **R&D: an engine mode that augments Apple's Stage Manager** (after 1.9 settles). Apple's
  arranges and draws; the app adds what it lacks: per-app rules for where a new app opens
  (its own stage or the current one), Shift while opening to bring it here, keyboard
  shortcuts, maybe the Dock's order. First a read-only look at what WindowManager exposes
  through Accessibility while Apple's is on (its strip, stages, actions); the whole idea rests
  on adding a window to Apple's current stage without a visible round trip.

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
