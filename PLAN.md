# Stage Manager — plan

A floating-window stage manager for macOS: Apple's Stage Manager strip and floating
windows, plus rules, hotkeys, named stages, pinning, placement and persistence.
No tiling, no grids, no snapping; every window keeps the frame the user gives it.

App: `Stage Manager.app`, bundle ID `dev.eugene.stagemanager`, `LSUIElement`, binary
`stage-manager`, crate `stage_manager`.

## Architecture

```
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
  server's on-screen list (the shown Space's windows only, parked slivers included). A
  window is given to the engine only once it shows on screen; until then it waits (another
  Space, minimized, app hidden). A once-a-second check catches windows moved between
  Spaces and Space changes the notification missed. No SkyLight/CGS calls.
- **Engine is pure Rust** (`src/engine`, `src/config.rs`): no Objective-C types, fully unit
  tested. It takes observations (live frames, z-order, focus) and returns a list of
  `Action`s (`Park`, `SetFrame`, `Raise`, `Focus`) that the platform layer executes.
- **Platform layer** (`src/platform`): AX wrappers, CGWindowList, ScreenCaptureKit, Carbon
  hotkeys, NSWorkspace events, safety. Public APIs only, plus `_AXUIElementGetWindow`.
- **UI** (`src/ui`): status item and menu, strip panel (layer-backed NSView, CALayer cards).
- **Threads:** AX can block on hung apps, so every AX call runs on the WM thread with a
  0.25 s messaging timeout. The main thread never calls AX. Commands go main → WM through a
  channel plus `CFRunLoopPerformBlock` + wake-up; strip snapshots go WM → main via GCD.

## macOS settings drive behavior

The app is a polyfill for Apple's Stage Manager, so Apple's own settings
(`com.apple.WindowManager`, read with `CFPreferencesCopyAppValue` and re-read when they change)
are the source of truth wherever they exist: show an app's windows all at once or one at a
time (`AppWindowGroupingBehavior`), hide recent apps (`AutoHide`), hide desktop items
(`HideDesktop`), click wallpaper to reveal desktop (`EnableStandardClickToShowDesktop`), hide
widgets. `config.toml` holds only what macOS lacks: rules, placement, hotkeys, named stages,
pinning, persistence, strip extensions. It never duplicates a macOS setting.

## Safety: no window stays parked

Defence in depth, because a parked window that never returns is the worst bug:

1. **Registry.** A global registry holds every parked window's AX element and restore frame.
2. **Quit** (menu) unparks everything, then exits.
3. **Panic hook** unparks (uses `try_lock`; falls back to a scan, below).
4. **SIGTERM/SIGINT/SIGHUP** go through a self-pipe to a watcher thread that unparks and exits.
5. **Guardian process.** At launch the app spawns itself as `stage-manager --guardian <pid>`.
   When the parent dies without writing the clean-exit marker (SIGKILL, segfault, abort), the
   guardian runs the recovery scan, then waits for every window still parked (those the
   registry's parked file names, and any managed app's in the park zone) and moves each back
   once its Space shows.
6. **Recovery scan** (launch and guardian): every window of every app whose frame lies in a
   park zone is moved back to its saved frame from `state.json`, or centered on the display.
7. **Restore frames are clamped** onto the visible display before being applied.

Across Spaces: the registry keeps every Space's parked windows (AX elements taken while
their Space showed). Quit is clean only when the window server lists no managed window in
the park zone on any Space; otherwise the guardian keeps watching after its scan and scans
again whenever such a window shows (its Space was visited), retrying while it stays shown,
since AX lists most apps' windows only on the shown Space. A window is done only when seen
back (AX reads it outside the park zone, or the window server lists it on screen outside it
in settled passes a second apart: a sliding Space can list in-between frames) or gone (not
listed on any Space for 3 s). It never exits while one waits unless Stage Manager runs again.

## Testing hooks

- `cargo test`: engine (rules, transitions, placement, serialization, park-zone math).
- **ctl socket** (`$TMPDIR/stage-manager.sock`, line protocol): `switch N`, `send N`, `new`,
  `pin`, `dump` (with a `spaces` section), `unpark-all`. The `stage-manager ctl ...` subcommand talks to it. This lets
  milestones be checked by script, not just by hand.
- `STAGE_MANAGER_ONLY=<bundle ids>` limits management to listed apps, so automated runs touch
  only a fixture app's windows (`scripts/fixture/`: a Swift app that opens titled windows).

## Milestones

1. **Skeleton:** Cargo project, `.app` bundle, stable self-signed identity
   (`scripts/make-cert.sh`), Makefile (`make run`, `make install`), menu bar item,
   permission checks (Accessibility, Screen Recording), Apple Stage Manager detection.
2. **Observation:** enumerate manageable windows (bundle ID, title, frame, z-order, subrole);
   log create/destroy/focus/activate live.
3. **Engine:** stage model, park/restore, hotkeys, safety layers, ctl socket.
4. **Rules + placement:** config TOML with hot reload, new-window rules, center and cascade.
5. **Strip:** `docs/UX-SPEC.md` from the measurement session, then the panel with live
   thumbnails, click to switch, auto-hide and edge reveal.
6. **Persistence:** `state.json` on change, re-match windows on launch.

Each milestone ends with a commit and a manual test checklist; `cargo test` and
`cargo clippy -- -D warnings` stay clean.

## Out of scope (prototype)

Multi-monitor beyond the main display (the model keys stages by display), fullscreen windows
(ignored). (Switch animations, drawn with snapshots, and drag and drop came later; see
`docs/UX-SPEC.md`.)
