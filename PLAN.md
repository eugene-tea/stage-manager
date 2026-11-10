# Stage Manager: plan

What's still to do. What shipped is in [CHANGELOG.md](CHANGELOG.md), how the app is built is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), how it should behave is in [docs/UX-SPEC.md](docs/UX-SPEC.md),
and what users asked for is in [docs/FEATURE-REQUESTS.md](docs/FEATURE-REQUESTS.md).

## 1.8.0

- **Accessibility** (`.github/ACCESSIBILITY.md`), in progress: VoiceOver labels on the strip's
  cards (which stage, which apps), Reduce Motion worked out from first principles, and a review
  of the whole app.
- **The edge tab across Desktops** goes in `test_spaces.py`.
- **After the release:** look at the repository's tabs on GitHub (README, Accessibility, Security,
  Contributing) once 1.8.0 is live; the files themselves were reviewed before it.

## 1.9

- **A right-click menu on cards** (R4): Bring Here, New Stage, Keep on Every Stage, Name Stage…, Close
  Window, Quit App. A named stage shows its name on hover (R7).
- **Stages in the menu bar menu**, click one to switch (R2).
- **Drop a window on a card** to add it to that stage (R3b).
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
- **Save `state.json` off the window thread** if 1.8's timing shows saves over about 10 ms: a writer thread
  that keeps only the newest state, finished before the quit's save.
- **Reopen after a cancelled logout even when some windows didn't come back.** Today only a clean quit at
  a logout reopens the app on a cancel. The parked file says "logout", and the guardian waits for the
  cancel once its recovery watch is done.

## Live checks

**One session, about 52 minutes.** Use the dev app and fixture windows only, with the 5K and the laptop
screen both attached. The main thread runs it, with Eugene at the Mac for the steps marked (E).

1. **The suite's new checks, 13 min.** `scripts/test-stages.sh safety park capture parity` on today's
   build: `kill -9` with a dialog open, an app answering late at the quit, a fit move then a
   crash, a dialog opened in a hidden stage, a window showing only a piece at the corner, no
   captures while the strip is hidden, and a minimized window's way to the strip sampled every
   8 ms (its places are printed: after the genie only the sink out of the Dock tile, then the
   corner). Hands off the Mac; they have never run.
2. **Both screens, 5 min.** `scripts/test-stages.sh displays` on today's build. All checks pass, including
   each screen's own card glass and corner patch (`displays[i].pictures`, `displays[i].corner_patch.shown`).
3. **⌘-Tab to the other screen, 3 min.** Put fixture B on the laptop screen (`ctl debug-frame`) as a stage of
   its own, then send it to a card there. Bring fixture A's stage to the front on the 5K. Activate B the way
   ⌘-Tab does (`osascript … to activate`). In the log: one `transition` on the laptop screen only. The 5K's
   `displays[0].active_stage` doesn't change.
4. **Full screen on one screen, 4 min.** `ctl debug-fullscreen <B> on` on the laptop screen, then open a new
   fixture window on the 5K. It joins the 5K's stage, the 5K's strip stays, and "a full-screen Space shows"
   is logged for the laptop screen only. Then `debug-fullscreen <B> off`: B comes back to its stage.
5. **Mission Control, 3 min.** `ctl debug-expose mission-control`. A fixture window opened while it's up is
   placed once it ends, and both strips come back on the same Desktop.
6. **A window moved to another Desktop of the laptop screen (E), 4 min.** Eugene drags fixture B in Mission
   Control to the laptop screen's second Desktop. In the log: B leaves its stage, and on that Desktop it
   starts a stage of its own. Back on the first Desktop, its stages are as they were.
7. **Apple's Stage Manager, measured (E), 16 min.** Eugene turns on Apple's Stage Manager and Dark Mode.
   - `scripts/measure-apple-card-glass.py`, in Dark Mode.
   - Then, in Light Mode, on fixture cards:
     - Shift-click on a card of two apps: whole card, or the app under the pointer?
     - Option-click on a card.
     - ⌘H, then ⌘\`.
     - The auto-hide delay and a new card's fade (recordings).
     - A card of three apps.
   - **Minimize, one 30 s recording** (screen recording at 60 fps, the 5K only; round minimize-flow).
     Fixture `--windows 2` as one stage, two cards of other fixtures in the strip. At 0 s the yellow
     button on Fixture 1; at 10 s ⌘M on Fixture 2; at 20 s Reduce Motion on, ⌘M on the window
     brought back from Fixture 1's card. Read frame by frame: does a genie into the Dock play at all;
     where the window shrinks to (the new card's slot: top or bottom of the strip) and in how many
     frames; does it turn and take on the card's glass on the way, or fly flat; does its card appear
     at once, fade or slide; does the Dock show anything (a tile, a bounce); and what Reduce Motion
     makes of it. Constants that may change after it: the flight's spring (today `[animation]
     shrink_*`, a pair of its own if Apple's is faster), where the picture starts and its size
     (`ui::minimize_motion::start`: the Dock tile, at the size the genie left the window), the
     card's fade without a tile and under Reduce Motion (`display_options::FADE_MS`), and the new
     card's slot (the engine's `Target::New`). macOS's own genie stays: Stage Manager hears of a
     minimize only after it.
   - With Reduce Motion on: one switch recorded (for the accessibility round).
   - Eugene turns Apple's Stage Manager and Dark Mode back off.
8. **VoiceOver on the strip (E), 5 min.** The accessibility round's checklist: each card reads its stage and
   apps, and VO-Space switches stages.

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
