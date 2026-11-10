# Stage Manager: plan

What's still to do. What shipped is in [CHANGELOG.md](CHANGELOG.md), how the app is built is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), how it should behave is in [docs/UX-SPEC.md](docs/UX-SPEC.md),
and what users asked for is in [docs/FEATURE-REQUESTS.md](docs/FEATURE-REQUESTS.md).

## 1.8.x: everything known broken or unchecked

Version 1 is Apple's Stage Manager, copied with nothing left: these are fixes and checks only. New
features wait for 2.0.

**Still open**

- **VoiceOver read aloud on the strip (E), 5 min.** Each card reads its stage and apps, and VO-Space
  switches stages. The labels are in the Accessibility tree (checked 11-09); hearing them needs Eugene.
- **The auto-hide delay on Apple's** (ours: `[reveal] hide_delay_ms` 500): a strip brought back by the
  pointer, then the pointer away from it; how long until Apple's slides out. One recording.
- **How soon Apple's switch starts after the click** (ours: 50 to 90 ms, `motion_ms`). One recording with
  the click on the log's clock.
- **The last frame of the shadow handover: proposed to close.** Since 1.8.2 a landing shows no darker
  frame about half the time, otherwise one (up to 15 levels on Finder's heavier shadow), more under load.
  Two shadows overlapping always read darker unless one goes on the very frame the other comes; macOS
  gives no way to drop another app's window shadow, and every signal we have (the window list, even a
  capture without our windows) comes after the window is already on screen. Eugene's call.

**Done on 11-09 evening** (live on the 5K and the laptop screen; dev, unreleased)

- The two suite checks failing since 1.8.0: the edge tab check left the pointer on the strip (check
  fixed); the icon drift was real, a card's hover growth followed its old slot during a fast drag (app
  fixed). `visible` and `slots`: 288 passed.
- `wallpaper_edge` runs (the desktop lookup missed the desktop's own windows); the edge tab across
  Desktops is in `test_spaces.py` (223 passed).
- A cancelled logout reopens the app after an unclean quit too (`restore`, the frozen-Bravo case).
- Minimize as Apple's: the yellow button and ⌘M fly the window straight into a new card at the top, no
  genie (measured on Apple's 11-09, `parity` 49 passed). Window ▸ Minimize, a title-bar double-click and
  an app's own minimize still play the genie first, then come to the strip.
- From the Apple session: Shift-click takes only the card's front window; a card of three apps (icons
  18 pt apart, the third window behind at the top); a hidden app's card leaves the strip and comes back to
  its slot unhidden, the most recent stage coming in front if the hidden one was.
- Live checks done: the never-run suite checks (`safety park capture parity displays`, 228 passed); ⌘-Tab
  to the other screen, full screen on one screen and Mission Control with two screens (new `screens`
  phase); a window moved to another Desktop in Mission Control (it leaves its stage and carries a stage of
  its own there; the Desktop left keeps its stages).
- Apple's measured, nothing to change: the card glass in Dark Mode (0.78 / 0.93 / 0.82, ours 0.78 / 0.95 /
  0.78, within the fit); Option-click is a plain click; ⌘\` raises the app's other window in its stage;
  no spring loading (a window brought to the strip hides it near the edge, or makes a new card dropped
  low, as ours); the card count by height (4 at a 13-inch Air's, 6 at the Studio Display's); Reduce
  Motion: Apple's switch is a cross-fade of about 80 ms, its minimize a fade out in place of about
  130 ms (ours fades 200 ms, worked out from the accessibility guidance, kept).

**Settled, nothing to do**

- Saving `state.json` off the window thread: 1422 saves on 11-08 and 11-09 took 4.9 ms typically, 9 ms
  at the 99th percentile, 29 ms at most.
- Accessibility (1.8.0) and the GitHub tabs (checked after 1.8.0).

**When it happens, no session needed:**

- **Eugene's next real restart.** In that day's log, check:
  - the launch's `recovery scan` line shows `restored=0` (no window came back hidden)
  - no `app activated … queued_ms` over 100 in the first two minutes, and if there is one, the
    `slow WM task` line names what held the thread
  - the `restore:` summary line
- **"macOS isn't reporting windows" shows in the menu again.** Quit Stage Manager, then count windows with
  System Events (`osascript -e 'tell application "System Events" to count windows of every process'`). If
  any are listed, our quit fixed it: the menu line then says "Quit and reopen Stage Manager", and the bug
  is ours. If none are listed, lock and unlock the screen and count again.

## 2.0: new features

- **A right-click menu on cards** (R4): Bring Here, New Stage, Keep on Every Stage, Name Stage…, Close
  Window, Quit App. A named stage shows its name on hover (R7).
- **Stages in the menu bar menu**, click one to switch (R2).
- **Drop a window on a card** to add it to that stage (R3b). Apple's strip hides instead (11-09); spring
  loading, if Apple's has it, is a 1.8.x check.
- **"Only the main screen" in Settings** (`[displays] main_only`, off; R8). The setting is read at launch, so
  turning it on or off quits the app (every window comes back) and opens it again, as an update does. Its
  line in Settings says so.
- **Scroll the strip past its capacity**, with a "+N" mark (R5).
- **No stage switches the user didn't ask for** (R6): an activation with no click or key press just before
  it doesn't switch stages, and that app's card glows.
- **One stage across all screens** (R8, second half), an option, off. One strip, on the main screen. A
  switch brings a stage's windows back on every screen. Still open: what it means when "Displays have
  separate Spaces" is on, and the lid and restart rules once a window moved to another screen keeps its
  stage.
