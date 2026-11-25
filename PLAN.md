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
- **A screen plugged in or out (one screen at a time too), under the Spaces engine.** In
  that day's log, within the minute after it: every `desktops: the model and the window
  server disagree` line has its `... agree again` within about 10 s (`lasted_ms`), and no
  `window left this Space` follows a Dock click or ⌘Tab to another Desktop.
- **Apple's, one screen at a time** (UX-SPEC, Screens): with the lid and the external screen,
  by hand.
- **"macOS isn't reporting windows" shows in the menu again.** Quit Stage Manager, then count windows with
  System Events (`osascript -e 'tell application "System Events" to count windows of every process'`). If
  any are listed, our quit fixed it: the menu line then says "Quit and reopen Stage Manager", and the bug
  is ours. If none are listed, lock and unlock the screen and count again.

**By hand, with someone at the Mac:**

- **The window server's notice for Mission Control opened without a key.** Notice 1508 was
  measured for the shortcut only (UX-SPEC, "The window server's notices"). Open Mission
  Control by a four-finger swipe, a hot corner and the Dock's item under the Spaces engine
  and read `ctl state`'s `engine.notices` and the `mission-control:` lines: if 1508 comes for
  each, the hidden stages' windows come at once whatever opened it, and MODES' "up to two
  seconds later from a gesture, a hot corner or the Dock" goes.
- **Which capture lights the screen-capture indicator** (`scripts/test-stages.sh
  probe-system indicator`, a build with `--features probes`; it raises macOS's
  screen-capture alert, so never unattended). Under the Spaces engine each window's look is
  read from its shadow through ScreenCaptureKit about every 20 s per window in use and
  after every switch; if the window server's own capture leaves the indicator off, read the
  looks that way and keep ScreenCaptureKit for the windows it misses.

## 1.9: closer to Apple's feel

- **The park corner gets a little lighter for the length of a switch** (Corner engine; seen
  11-24 on a smooth dark-red wallpaper). The patch at rest reaches 22 pt left of the slivers,
  one parked window's shadow; with five windows parked at the same spot their shadows add up
  past it (about 2 % darker beside the patch, 3 of 255, fading out by 60 pt). A switch's
  cover reaches 60 pt (`ACTIVE_REACH`: macOS activates the windows waiting there) and shows
  the desktop with no shadow, so the corner lightens for the motion and darkens again. One
  way: the patch at rest as wide as the cover, so nothing changes at a switch; its cost is a
  larger patch where it is drawn from the file (no Screen Recording), off more often for a
  desktop icon under it. Compare before and after from captures of the corner.
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
  Left after that: whether a gesture, a hot corner and the Dock's item are found as fast as
  the shortcut (the window server's notice 1508, below under "By hand"); whether a window parked at
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

## Under the hood: what the rework of 11-21 left

Nothing here changes what a person asks for; each closes a way a slower Mac, or a rarer
order of events, can still go wrong. The bench first, then the live suite
(DEVELOPMENT, "The bench").

- **A screen going with its Desktops kept (the rest of the Desktops design's slice 5).**
  When a screen goes and macOS moves its Desktops to the screen left under the same ids, the
  model still merges the gone screen's Desktop into the one shown there, and `spill`'s watch
  and the clock sort out the rest. Left: `adopt_display` only reparents a screen's Desktops
  while the listing decides (their Spaces, stages and bindings kept); `Model::desks_listed`
  gives each bound Space the screen that lists its Desktop, the same call when the screen
  comes back; a census once two looks read the same listing (every managed window the engine
  does not hold asked for its Desktop, a stage whose windows all moved carried whole);
  `spill::WATCH` only a cap on the brisk pace, ending when nothing names a gone screen; then
  `handed_hidden`, `display_watch` and the model's two `EngineKind::Spaces` branches go.
- **Desktops matched by uuid after a restart (slice 6).** The saved Desktops are matched
  to the listing's by uuid instead of by their windows. Needs first: whether Space uuids last
  over a logout and a reboot (each load logs how many saved uuids are listed now).
- **The cover wait off the WM thread.** A switch still waits for the strip's answer with the
  WM thread held (`COVER_TIMEOUT`, 150 ms), because five callers of `execute_animated` go on
  as if its batch had run (an un-minimize's dive frame and un-minimize, a new window's
  `let_new_out`, an activation's `lent_decided`, the tick's and the fast tick's later duties,
  the next events of the same run). Each needs to become a continuation run at the cover's
  end. On the bench at load 10 every animated switch lands with no pictures, and an app's
  own activation inside the wait leaves its stage in front with the switch's app frontmost.
- **A held focus against Accessibility's silence.** A focus held for its Desktop is dropped
  after 6 s (`FOCUS_REPLAY_CAP`), but a silent Accessibility may hold the arrival for 10 s
  (`AX_SILENT_MAX`), so the focus can be lost after an unlock. And an activation held while
  a Desktop may slide is decided "here" after 500 ms with no Space change
  (`SLIDE_ACTIVATION_HOLD`): the notice, measured 0.35-0.45 s after a Dock click, is past
  that on a Mac three times slower.
  Both want an end by the event, the time as a cap; the bench cannot yet make the observer
  lose its windows as after an unlock.
- **An app that reorders its windows late.** The raise-order check fixes the order and
  watches it for 150 ms; an app whose answer lags 300 ms ends out of order at every load
  (`CHECK_CAP` 400 ms; pinned on the bench, `an app reordering 300 ms late`).
- **The launch watch's notice.** The window-created notice (811) never reached the app in a
  normal run: it seems to come only for windows asked for one by one
  (`SLSRequestNotificationsForWindows`), so the launch watch rests on its 4 ms read. Asking
  for each new app's windows, or another sign, would let it rest.
- **What still decides by time under the Accessibility engine.** The Desktop guess's settles
  (`MIXED_WAIT`, `SPACE_SETTLE`, `FOREIGN_SETTLE`, `SPACE_CHECKS`, `BESIDE_ARRIVALS`,
  `STICKY_HOPS`, `SLIDE_CARRY`) and the corner patch's 22 time values, mostly blind settles:
  each to be ended by what it waits for where a read can say, its value a cap.
- **The strip's switch machine.** `Running` carries a switch in 10 flags and 16 timers with
  3 tests; one explicit state machine, tested on its own, in its place.
- **One `wait_until` and one `Backoff`.** At least 16 hand-written "read until or deadline"
  loops, 5 lock retries and 3 doubling backoffs, each its own; one helper each, on the
  clock a test moves.
- **The Apple mode's second screen and Reduce Motion, unmeasured.** Apple's answers and
  motions were measured on one screen with Reduce Motion off (UX-SPEC, "Measured on
  Apple's").

## One way for every mode: what the audit of 11-25 found

The rule: how windows are hidden is the mode's choice (Accessibility the default while the
Spaces mode is proven; Spaces is to become the default). What the app captures is the same in
every mode: the window server's own call first, wherever a check finds it working, and the
public way as the fallback. Pictures follow the rule since 1.9. The items below are where the
Accessibility mode was left behind for no reason of the mode.

- **The window server's list of Desktops is read under the Spaces engine only.** Under the
  Accessibility engine Desktops are still told apart by the windows on screen. Read there too
  (with its own check that the call answers, and the guess as the fallback), it brings the
  Desktop behaviours UX-SPEC lists for the Spaces engine and ends most of "What still decides
  by time under the Accessibility engine", above. To be agreed first: the rule above is
  stated for pictures, and this is a read.
- **Mission Control's opening and a Desktop change's end are found by a 1 s poll** under the
  Accessibility engine; the triggers that bring the look forward run under Spaces only (one,
  the Dock's notice, is public). `mission_control_key_keeps_stage` is honoured under Spaces
  only for the same reason.
- **The housekeeping rests 0.75 s at most under the Accessibility engine**, 2 s under the
  others, to keep the list the inert slivers' tap trusts fresh; each wake is a full pass.
  Measure what the cap costs at rest, then keep that list fresh by something lighter. A show
  still owed for a window that only waits for its Desktop also keeps the pace brisk there.
- **⌘Tab to another Desktop arranges the stage on arrival**, in both modes, though the chosen
  app is known before ⌘ is released (the Spaces engine's lend already follows it). Arranged
  ahead, as a Dock click's is; UX-SPEC's "known gap" sentence then goes.
- **A Dock click's arrangement is undone after 1 s** under the Accessibility engine, 3 s under
  Spaces; the reason for 3 s (a busy Mac's late notice) is not the mode's. One cap; and
  whether the Accessibility engine can arrange at the press as Spaces does.
- **A hidden window found shown is given up after four tries** under the Accessibility engine
  and then stays in view, counted hidden, with no card; the Spaces engine never gives up.
- **Smaller.** The wait for a switch's pictures (160 ms) is sized for ScreenCaptureKit, not for
  the window server's capture; the landing variants can be tried only with `engine = "spaces"`;
  MODES.md's cost at rest quotes 11-14 and is to be measured again after the picture change.

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
