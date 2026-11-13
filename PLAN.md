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

- **A newly opened app's stage comes as its window shows.** After 1.8.5's fixes (the AX ask
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
- **Stages kept through Apple's Stage Manager on and off.** Turning Apple's on drops
  `state.json` (so a quit, crash or logout meanwhile leaves no stale file, 10-13), and
  turning it off starts fresh, one stage per app: VS Code and Terminal, one stage before,
  came back as two (11-13 09:54:55 to 09:55:21). Keep the stages in memory through the
  stand-down and put each window still open back in its stage at the resume, matched by
  window id (stable until the logout); a window opened meanwhile gets a stage per app, one
  closed drops out. A quit, crash or logout while standing down still starts the next launch
  fresh. A live-suite case for the on, off, on round trip.

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
