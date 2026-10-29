# Stage Manager — plan

A floating-window stage manager for macOS: Apple's Stage Manager strip and floating
windows, plus rules, hotkeys, named stages, pinning, placement and persistence.
No tiling, no grids, no snapping; every window keeps the frame the user gives it.
The defaults look and behave like Apple's; every extra is off until configured.

App: `Stage Manager.app`, bundle ID `dev.eugene.stagemanager`, `LSUIElement`, binary
`stage-manager`, crate `stage_manager`. Native behavior and every deviation:
`docs/UX-SPEC.md`; user requests and research: `docs/FEATURE-REQUESTS.md`.

## Architecture

```text
main thread (AppKit)          WM thread (CFRunLoop)            helpers
─────────────────────         ──────────────────────           ──────────────────────
NSApplication run loop        AXObservers (all apps)            capture: SCScreenshotManager
status item + menu            AX reads/writes (timeout 0.25s)   config watcher (mtime poll)
strip NSPanel                 engine::Model (owns state)        signal watcher (self-pipe)
Carbon hotkeys        ──cmd──▶ executes engine Actions          guardian process (parent death)
NSWorkspace notifications     persists state.json               ctl socket (testing/scripting)
              ◀──snapshot───  StripModel to main via GCD
```

- **Stages per Space.** A stage is a group of windows on one Space (Desktop); every Space
  has its own stages, active stage and strip, as Apple's does. Inactive stages are parked in
  the bottom-right corner of their display (macOS keeps a sliver onscreen). Switching = save
  the active stage's live frames and z-order, park it, restore the target's frames, raise
  back to front, focus the remembered key window last. Nothing acts on another Space's
  windows except the safety paths.
- **Spaces by their windows.** No public API names a Space: the engine gives each a
  synthetic id and recognizes it on `activeSpaceDidChangeNotification` from the window
  server's on-screen list. A window is given to the engine only once it shows on screen
  (another Space, minimized, app hidden: it waits). A once-a-second check catches moves
  between Spaces and missed notifications. No SkyLight/CGS calls.
- **Engine is pure Rust** (`src/engine`, `src/config.rs`): no Objective-C types, fully unit
  tested. Observations in (frames, z-order, focus, the Dock's order), `Action`s out (`Park`,
  `SetFrame`, `Raise`, `Focus`) for the platform layer to execute.
- **Platform layer** (`src/platform`): AX, CGWindowList, ScreenCaptureKit, Carbon hotkeys,
  NSWorkspace events, safety. Public APIs only, plus `_AXUIElementGetWindow`.
- **UI** (`src/ui`): status item and menu, Settings, the strip panel (CALayer cards) and the
  motion overlay. The overlay sits just below floating windows (an always-on-top player stays
  over a switch), and above everything only while a dragged window's card shows.
- **Threads:** AX can block on hung apps, so every AX call runs on the WM thread with a
  0.25 s messaging timeout; the main thread never calls AX. Commands go main → WM through a
  channel plus `CFRunLoopPerformBlock`; strip snapshots go WM → main via GCD.

## macOS settings drive behavior

Apple's own settings (`com.apple.WindowManager`, read with `CFPreferencesCopyAppValue`,
re-read on change; never written) are the source of truth wherever they exist: All at Once /
One at a Time, hide recent apps, hide desktop items, click wallpaper to reveal desktop, hide
widgets. `config.toml` holds only what macOS lacks and never duplicates a macOS setting.

## Safety: no window stays parked

A parked window that never returns is the worst bug. Defence in depth:

1. **Registry** of every parked window's AX element and restore frame, across Spaces.
2. **Quit** unparks everything, saves the stages, then reads the window server: any allowed
   app's window still in a park zone comes back to its registry frame, the frame the unpark
   gave it, its `state.json` frame (a dialog only by its own id), or centered. A sliver counts
   there only for a window known by its id (registered, managed, in `state.json`). A
   busy app's `CannotComplete` is retried (three tries 75 ms apart) on unparks only, never in
   a switch. Bounds: the leftovers check ≤ 4 s, the registry's retries ≤ 2 s. Quit is clean
   only when no managed window is in the park zone on any Space; otherwise the guardian
   takes over. A stand-down (Apple's Stage Manager turned on) ends with the same check.
3. **Panic hook** unparks (`try_lock`, falls back to the scan).
4. **SIGTERM/SIGINT/SIGHUP** through a self-pipe to a watcher thread that unparks and exits.
5. **Guardian** (`stage-manager --guardian <pid>`): when the parent dies without its
   clean-exit marker, it runs the recovery scan, then waits for every window still parked and
   moves each back once its Space shows. A window is done when seen back (AX, or the window
   server in settled passes a second apart), gone (unlisted 3 s), or adopted by a running
   copy. It only ever moves windows out of the park zone, so racing the new app is benign.
   **Handoff (8N):** once the new copy reports `adopted`, the guardian hands it the rest of
   its watch (`ctl take-watch`) and exits; the copy watches them by the same rules, and its
   own quit and guardian carry them on. An earlier version is offered again a minute later.
   From a sender not signed like the app, only windows the app itself sees parked are taken
   (never fitted, its corners ignored, reached only while listed in the park zone). A pass
   that lists nothing on screen is blind: nothing is reached until one sees again.
6. **Recovery scan** (launch, guardian, after a main display change): every app's window in
   a park zone goes back to the frame a crashed copy registered (its parked file, by id),
   else its saved frame (only on a main display of the saved size), else centered; silent apps are retried for 30 s, in
   slices of two apps a pass so a busy app never holds the WM thread. A sliver (≤ 8 × 256 pt
   past the right or bottom edge) counts as parked only for windows we manage or registered,
   or apps with a window in `state.json`.
7. **Restore frames are clamped** onto the visible display.

Windows that are never waited on (logged at debug): **hidden tabs** (8J; layer 0, off
screen, unlisted by an answering app, vouched for by a same-size sibling; a managed,
registered or saved window is never judged one), and **other apps' helpers** (8M; in the park
zone, never registered, managed or saved, off screen, unlisted by an answering app). A
registered or saved window always waits.

## Strip order

`[strip] order`: `"native"` (default, Apple's swap with LRU eviction), `"in_place"` (stable
places in creation order per Space; `"fixed"` is its old name) or `"dock"` (the in-place
places sorted by each stage's leftmost Dock app, by bundle id then pid; apps without a Dock
icon last; read through AX at launch, 0.5 s after an app launches or quits and every 2 s,
never applied mid-transition; card drags spring back, merges still work). In place: past the
capacity the first capacity−1 stages keep their places and the rest share the bottom slot by
recency; `[strip] active` = close | gap | card.

## Testing hooks

- `make check`: `cargo test` and clippy, clean.
- **ctl socket** (`~/.config/stage-manager/stage-manager.sock`, beside the single-instance
  lock; not `$TMPDIR`, which macOS prunes): `switch N`, `send N`, `new`, `pin`, `dump`,
  `state`, `probe`, `unpark-all`, via `stage-manager ctl …`.
- `STAGE_MANAGER_ONLY=<bundle ids>` limits management to fixture apps (`scripts/fixture/`),
  so live phases (`scripts/test-stages.sh`) never touch real windows. No live phase while
  another copy's guardian runs, or while the screen is locked.

## Accepted trade-offs

Known gaps the owner accepted as rare:

- **Slivers (6X L3):** a user's window pushed almost wholly off the bottom-right edge
  (≤ 8 × 256 pt on screen) of a managed app is taken as parked: a switch brings it back to its
  last frame.
- **R15:** `activation_frame` (`src/platform/controller.rs`), when the display's only shown
  window belongs to an app that posts no move notice, reads a horizontal-only drag of it as a
  Desktop slide.
- **Restore window (8E3, 8F):** for up to 5 minutes after a launch (ending at the first
  click, key, card or Dock click, or once apps are quiet 20 s and the login is a minute old),
  a window an app opens by itself that matches a saved window of an inactive stage is placed
  there, parked, not switched to. While settling, an unmatched window goes by Apple's rule
  without the switch, except into an empty saved front stage's place (8F2). When it ends
  (no sooner than a minute after the login), the waiting entries of apps not running then are
  given up (in stages and on saved Desktops not visited); running apps keep theirs under their
  own quiet minute.
- **Apps that save their own frames (W2):** a window parked when its app quit may be saved
  by the app at the park frame and reopened there; within a day of the quit it takes the
  frame it had (by app and title, else the app's only one), else it is centered.
- **Logout head start (8F, 8H):** at the first logout signal every window comes back, the
  leftovers check runs and stages are saved; the app idles until macOS's quit. A cancelled
  logout resumes at `logoutCancelled`, else two minutes after the latest signal. An app's quit
  keeps its entries in `state.json` two minutes (`quit_at`).
- **One pool after a restart (8I):** Electron apps reopen on the Desktop shown at login; a
  window matching another Desktop's saved entry with confidence recreates its stage as a part
  where it shows. Left: two Desktops' same-named stages both feed the one stage of that name
  (`find_named`).

## Open

- **Screens (after 1.4.0):** follow-up update for external and several screens.
  - A strip and stages per screen, as Apple's (today only the main screen is managed; a
    window on a second monitor is left alone). FEATURE-REQUESTS R8: medium.
  - A login on another screen (10-23: laptop screen at login, then the lid closed onto the
    external display): macOS reopens every window on one Desktop, and the other Desktop's
    saved stages wait for windows that will not come. Let those saved stages form where
    their windows are, and say so in the restore summary.
  - The first launch after that login drew card switches wrong (the incoming window showed
    at its place before its snapshot reached it; the log identical to a good run); a
    relaunch cured it. Catch it with a screen recording if it comes back; check what the
    overlay and the strip panels hold from a launch while the screens settle.
- **Real-restart check (8G)**, with the owner, from the day's log:
  - Stage Manager comes back at login ("Reopen windows" on): "logout, restart or shutdown
    coming …", "asked to quit by the system", "quitting after the logout's head start".
  - No window reopens in the park zone; a cancelled logout restores at its signal.
  - The order of "logout signal" lines vs the first "app terminated", then trim `SIGNALS`;
    "N at the corner"; `from_other=true` lines and "K from other Desktops".
  - A slow app (Xcode, Electron) in the saved front stage.
- **Log caps** (`max_mb_per_day` 100, `max_mb_total` 500, `days` 14) against real days
  (`stage-manager log summary`); the suite's estimate is ~1 MB a normal day, 2–3 MB heavy.
- **Overlay over a raised strip:** a switch while the strip is raised animates just under the
  strip, the floating level, so an always-on-top window may still be covered then.
- **Feature requests not built** (`docs/FEATURE-REQUESTS.md` §3): card context menu (R4),
  stage names from the UI (R7), strip overflow scroll with "+N" (R5), quiet activations
  (R6), a stage list in the menu (R2), the parity checks of §1.

## Out of scope

Multi-monitor beyond the main display (the model keys stages by display), fullscreen
windows (ignored). UX-SPEC's measurement record of Apple's Stage Manager is that file as of
commit 20665a3.
