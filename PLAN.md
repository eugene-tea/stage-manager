# Stage Manager — plan

A floating-window stage manager for macOS: Apple's Stage Manager strip and floating
windows, plus rules, hotkeys, named stages, pinning, placement and persistence.
No tiling, no grids, no snapping; every window keeps the frame the user gives it.
The defaults look and behave like Apple's; every extra is off until configured.

App: `Stage Manager.app`, bundle ID `dev.eugene.stagemanager`, `LSUIElement`, binary
`stage-manager`, crate `stage_manager`. Native behavior and every deviation:
`docs/UX-SPEC.md`; building, testing, the ctl socket and the source layout:
`docs/DEVELOPMENT.md`; the user's view: `docs/GUIDE.md`; requests: `docs/FEATURE-REQUESTS.md`.

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
  asked on threads of their own, so one busy app holds up no other. Frame writes run with
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
   logout it waits for a cancel and reopens the app.
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

## Open

- **What heals Accessibility blindness.** After a WindowServer watchdog kill, Accessibility
  listed no window (System Events neither) until our quit and a lock/unlock, unchecked
  between. Next time: quit Stage Manager, check System Events, then lock and unlock. If the
  quit heals it, the menu line says "Quit and reopen Stage Manager" instead of "Log out",
  and the bug is ours.
- **Logout cancel gaps.** A cancel in the milliseconds before our exit is missed; a logout
  where not every window came back does not relaunch.
- **Medium matches in the restore:** windows shown after the launch get saved frames on a
  medium match too (the saved size only on a strong one). Left as is.
- **The Dock icon after a cancelled logout.** AppKit's NO to the switch back to accessory at
  a real cancel was not reproduced outside a logout (cause unmeasured). The fix retries for
  about 10 s and reads macOS's listing; if its INFO line shows tries > 1 or `done=false` at a
  real cancel, look again.
- **Real-restart check, from the day's log:** no window reopens parked; "logout signal"
  lines vs the first "app terminated" (then trim `SIGNALS`); a slow app in the saved front
  stage; windows placed within ms of being found (no "queued_ms" over ~100). The reopen list
  worked at the 11-06 restart ("reopened by macOS", 7 s after login).
- **Screens, live with the owner:** the `displays` phase, the lid and a restart with both
  screens passed 11-06. Still to try: ⌘-Tab to the other screen's stage; the second screen's
  glass, patch, full screen, Mission Control, a window moved to another Desktop.
- **Still the main screen's, by design:** a Space change settling and the lock gate every
  screen (no notice names the screen); `ctl merge`, `ctl space`; the probes and debug
  hooks; the sliver shield; `ctl state`'s top-level fields (each screen's: `displays[i]`).
- **Card switches drawn wrong once** at a first launch after a login (a relaunch cured it).
  If it returns: a screen recording, and what the overlay and strip panels hold.
- **Log caps** (100 MB a day, 500 total, 14 days) against real days (`stage-manager log
  summary`); the suite's estimate: ~1 MB a day, 2–3 MB heavy.
- **Overlay over a raised strip:** a switch animates just under the strip then, so it may
  cover an always-on-top window.
- **Deferred review findings** (`tmp/fable-review/`): K5, `state.json`'s fsync off the WM
  thread, ordered against the quit's save; L5/U4, the switch cover's Dock carve-out now the
  overlay sits below the Dock (needs a look); C4, a step journal for non-APFS
  swaps.
- **Live checks never written:** an off-edge window, one tucked by the corner, a Dock
  change, a dialog born parked; a late answer at quit, `kill -9` with a dialog, a fitted
  move after a crash, a reopen at the corner frame; a broken config at launch; captures
  stopping with the strip hidden, edge reveal, the tab across Spaces.
- **The suite and NotificationCenter:** a banner's layer-21 window over the strip (or the
  owner's unmanaged windows) makes click checks refuse. Wait for it; name it when it stays.
- **Full-screen windows in the closed-window rule:** a dead handle off screen at about a
  display's size is kept (`stale_handle::on_full_screen_space`); the window's last read
  AXFullScreen would retire the size test.
- **Short-lived windows** (Safari's small dialog, Chrome's address-bar suggestions) are
  taken into a stage and let go a second later, many times a day; Apple's shows neither on
  a card. Decide whether such windows are ever managed.
- **Card glass not measured:** Dark Mode; a window behind another on one card; the hover's
  growth (Apple's ~11 %) against ours.
- **`hide_parked_apps` stays off:** a hidden app ⌘-Tabbed to skips the switch's motion, and
  its live phase left an empty stage after "supersede: Bravo and Charlie together again".
  Look there before offering it.
- **Next version: "Only the main screen" in Settings** (`[displays] main_only`, off; R8).
  Read at launch, so the switch quits (every window back) and reopens the app, as an update
  does; its line says so.
- **Feature requests not built** (`docs/FEATURE-REQUESTS.md` §3): card context menu (R4),
  stage names from the UI (R7), strip overflow scroll with "+N" (R5), quiet activations
  (R6), a stage list in the menu (R2), drop a window on a card (R3b), the parity checks.

## Out of scope

A unified stage across screens (Apple's has none); fullscreen windows (ignored). UX-SPEC's
measurement record of Apple's Stage Manager is that file as of commit 20665a3.
