# Stage Manager — architecture

How the app is built and the rules the code keeps. What it should do for the user is in
[UX-SPEC.md](UX-SPEC.md); building and testing in [DEVELOPMENT.md](DEVELOPMENT.md); what is
still to do in [PLAN.md](../PLAN.md).

App: `Stage Manager.app`, bundle ID `dev.eugene.stagemanager`, `LSUIElement`, binary
`stage-manager`, crate `stage_manager`. A floating-window stage manager: Apple's strip and
floating windows, no tiling, no grids, no snapping; every window keeps the frame the user
gives it. The defaults look and behave like Apple's; every extra is off until configured.

## Architecture

```text
main thread (AppKit)          WM thread (CFRunLoop)            helpers
─────────────────────         ──────────────────────           ──────────────────────
NSApplication run loop        AXObservers (all apps)            capture: SCScreenshotManager
status item + menu            AX reads/writes (timeout 0.25s)   config watcher (mtime poll)
strip NSPanels                engine::Model (owns state)        signal watcher (self-pipe)
Carbon hotkeys        ──cmd──▶ executes engine Actions          guardian process (parent death)
NSWorkspace notifications     persists state.json               ctl socket (testing/scripting)
              ◀──snapshot───  StripModel to main via GCD
```

- **Stages per screen and Space**, as Apple's (UX-SPEC "Screens"). Each screen has its own
  strip, Spaces, park corner (one with no screen beyond it, `engine::park::choose_corner`)
  and capacity. A window belongs to the screen showing most of it (`engine::displays`);
  moved to another it starts a stage there (`Model::windows_moved_to_display`); a removed
  screen hands its stages to the main one (`Model::set_displays`). Screens are known by
  vendor, model and serial (`ScreenKey`), never `CGDirectDisplayID`, in the model and in
  `state.json`. `[displays] main_only` (read at launch) keeps the main screen only. Inactive
  stages are parked in their screen's corner. A switch saves the active stage's frames and
  z-order, parks it, restores the target's, raises, focuses the remembered key window last.
  Nothing acts on another Space's windows except the safety paths.
- **Spaces by their windows.** No public API names a Space: each gets a synthetic id,
  recognized on `activeSpaceDidChangeNotification` from the window server's on-screen list.
  A window reaches the engine only once it shows on a display. A once-a-second check
  catches Space moves and missed notices. No SkyLight/CGS calls.
- **Engine is pure Rust** (`src/engine`, `src/config.rs`), unit tested: observations
  in, `Action`s out (`Park`, `SetFrame`, `Raise`, `Focus`).
- **Platform layer** (`src/platform`): AX, CGWindowList, ScreenCaptureKit, Carbon hotkeys,
  NSWorkspace events, safety. Public APIs only, plus `_AXUIElementGetWindow`.
- **UI** (`src/ui`): status item and menu, Settings, a strip panel per screen (CALayer
  cards), the motion overlay (just below floating windows; above everything only while a
  dragged window's card shows).
- **Threads:** every AX call runs on the WM thread with a 0.25 s timeout; the main thread
  never calls AX. Apps that do not answer yet (a login, the launch's recovery scan) are
  asked on threads of their own, so one busy app holds up no other. The minimize tap
  (`platform::minimize_tap`) asks on its own thread too, at 0.1 s a call and only for a press
  near a managed window's title-bar buttons or ⌘M: a minimize caught there goes to the WM
  thread as a command. Frame writes run with
  the app's `AXEnhancedUserInterface` off (`platform::enhanced_ui`), on again after (also at
  quit, panic and SIGABRT). A late activation notice acts only if it fired after our latest
  switch began and its app is still frontmost (`engine::stale_activation`). A handle answering `kAXErrorInvalidUIElement` is looked up
  afresh by id; one unlisted and off screen on two looks a second apart is let go as closed
  (`engine::stale_handle`). Commands go main → WM by channel plus `CFRunLoopPerformBlock`;
  snapshots WM → main via GCD.

## macOS settings drive behavior

Apple's settings (`com.apple.WindowManager`, re-read on change, never written) are the
source of truth wherever they exist; `config.toml` holds only what macOS lacks.

## Safety: no window stays parked

A parked window that never returns is the worst bug.

1. **Registry** of every parked window's AX element and restore frame, across Spaces.
2. **Quit** unparks everything, saves the stages, then brings back any window still in a
   park zone (registry, unpark or `state.json` frame, else centered). `CannotComplete` is
   retried on unparks only. Bounds: leftovers ≤ 4 s, registry retries ≤ 2 s. Not clean
   while a managed window is parked on any Space: the guardian takes over. A stand-down
   (Apple's Stage Manager turned on) ends with the same check.
3. **Panic hook** unparks (`try_lock`, falls back to the scan).
4. **SIGTERM/SIGINT/SIGHUP** through a self-pipe to a watcher thread that unparks and exits.
5. **Guardian** (`stage-manager --guardian <pid>`), restarted if it dies while the app runs:
   when the parent dies without its clean-exit marker it runs the recovery scan, then waits
   for every window still parked until it is seen back, gone (unlisted 3 s) or adopted. It
   only moves windows out of the park zone. Once a new copy reports `adopted`, it hands over
   the rest (`ctl take-watch`; an unsigned sender's only as the app sees them parked) and
   exits. A pass listing nothing on screen is blind: nothing moves. After a quit at a
   logout it waits for a cancel and reopens the app; after an unclean one (the parked file
   says `logout`) it listens through its watch, opens the app at a cancel heard then, and
   once every window is back waits as after a clean quit.
6. **Recovery scan** (launch, guardian, main display change): a window in a park zone goes
   back to a crashed copy's registered frame, else its saved frame (main display of the
   saved size only), else centered; silent apps retried 30 s, two apps a pass. A sliver
   (≤ 8 × 256 pt past the right or bottom edge) counts only for windows managed, registered
   or of apps in `state.json`.
7. **Restore frames within reach** (`Display::reach`): never in a park zone, the title bar
   below the menu bar with 80 pt on screen.

Never waited on (logged at debug): **hidden tabs** (layer 0, off screen, unlisted by an
answering app, a same-size sibling) and **other apps' helpers** (in the park zone, off
screen, unlisted, never registered, managed or saved). A managed, registered or saved
window always waits.

## Strip order

`[strip] order`: `"native"` (default, Apple's swap with LRU eviction), `"in_place"` (stable
places per Space; `"fixed"` its old name) or `"dock"` (in-place places by each stage's
leftmost Dock app, read through AX, never mid-transition). Details: UX-SPEC and CONFIG.

## Testing hooks

`make check` stays clean. The ctl socket, `STAGE_MANAGER_ONLY` and the fixture app:
`docs/DEVELOPMENT.md`. Live phases touch fixture windows only; none while another copy's
guardian runs or the screen is locked.

## Accepted trade-offs

- **Slivers:** a window of a managed app pushed almost wholly off the bottom-right edge
  (≤ 8 × 256 pt showing) is taken as parked; a switch brings it back to its last frame.
- **Horizontal drag as a slide:** `activation_frame` (`platform/controller.rs`) reads a
  horizontal-only drag of a display's only shown window, of an app posting no move notice,
  as a Desktop slide.
- **Restore window:** up to 5 minutes after a launch (to the first click, key or card, or
  apps quiet 20 s) a window an app opens itself that matches an inactive stage's saved
  window is parked there, not switched to. When it ends, waiting entries of apps not
  running are given up; running apps keep theirs for their own quiet minute; a Desktop not
  visited keeps its own (up to 30 days).
- **Apps that save their own frames:** a window parked when its app quit may reopen at the
  park frame; within a day it gets its old frame back, else centered.
- **Logout head start:** at the first logout signal every window comes back and the stages
  are saved; the app idles until macOS's quit. A cancel resumes; with no cancel signal, two
  minutes after the latest signal or app quit. An app's quit keeps its entries two minutes.
- **One pool after a restart:** Electron apps reopen on the Desktop shown; a confident match
  of another Desktop's entry recreates its stage there. Two Desktops' same-named stages feed
  one stage (`find_named`). Nothing tells the user: macOS moves no window between Desktops.

## Still the main screen's, by design

A Space change settling and the lock gate every screen (no notice names the screen); `ctl
merge`, `ctl space`; the probes and debug hooks; the sliver shield; `ctl state`'s top-level
fields (each screen's: `displays[i]`).
