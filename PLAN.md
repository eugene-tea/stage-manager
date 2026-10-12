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

- **One native Space.** A stage is a group of windows. Inactive stages are parked in the
  bottom-right corner of their display (macOS keeps a sliver onscreen). Switching = save the
  active stage's live frames and z-order, park it, restore the target's frames, raise back to
  front, focus the remembered key window last.
- **Engine is pure Rust** (`src/engine`, `src/config.rs`): no Objective-C types, fully unit
  tested. It takes observations (live frames, z-order, focus) and returns a list of
  `Action`s (`Park`, `SetFrame`, `Raise`, `Focus`) that the platform layer executes.
- **Platform layer** (`src/platform`): AX wrappers, CGWindowList, ScreenCaptureKit, Carbon
  hotkeys, NSWorkspace events, safety. Public APIs only, plus `_AXUIElementGetWindow`.
- **UI** (`src/ui`): status item and menu, strip panel (layer-backed NSView, CALayer cards).
- **Threads:** AX can block on hung apps, so every AX call runs on the WM thread with a
  0.25 s messaging timeout. The main thread never calls AX. Commands go main → WM through a
  channel plus `CFRunLoopPerformBlock` + wake-up; strip snapshots go WM → main via GCD.

## Safety: no window stays parked

Defence in depth, because a parked window that never returns is the worst bug:

1. **Registry.** A global registry holds every parked window's AX element and restore frame.
2. **Quit** (menu) unparks everything, then exits.
3. **Panic hook** unparks (uses `try_lock`; falls back to a scan, below).
4. **SIGTERM/SIGINT/SIGHUP** go through a self-pipe to a watcher thread that unparks and exits.
5. **Guardian process.** At launch the app spawns itself as `stage-manager --guardian <pid>`.
   When the parent dies without writing the clean-exit marker (SIGKILL, segfault, abort), the
   guardian runs the recovery scan.
6. **Recovery scan** (launch and guardian): every window of every app whose frame lies in a
   park zone is moved back to its saved frame from `state.json`, or centered on the display.
7. **Restore frames are clamped** onto the visible display before being applied.

## Testing hooks

- `cargo test`: engine (rules, transitions, placement, serialization, park-zone math).
- **ctl socket** (`$TMPDIR/stage-manager.sock`, line protocol): `switch N`, `send N`, `new`,
  `pin`, `dump`, `unpark-all`. The `stage-manager ctl ...` subcommand talks to it. This lets
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

Multi-monitor beyond the main display (the model keys stages by display), animations,
fullscreen windows (ignored), drag and drop between stages.
