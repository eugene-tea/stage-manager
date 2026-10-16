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
2. **Quit** (menu) unparks everything, then reads the window server: every window of an allowed
   app still in a park zone (known to this run or not) is brought back to its registry frame,
   its `state.json` frame, or centered, before it exits; one it cannot reach leaves the exit
   unclean for the guardian. A restore a busy app cannot answer (`CannotComplete`) is retried
   quickly (three tries 75 ms apart, within 1.2 s) on an unpark only (quit, stand-down, the
   safety paths), never in a switch. Every budget is a bound: the leftovers check takes at
   most 4 s (a try starts only when it fits), the registry's quick retries 2 s in all. A
   stand-down (Apple's Stage Manager turned on) ends with the same check; a window it cannot
   reach is looked for again whenever a Desktop shows.
3. **Panic hook** unparks (uses `try_lock`; falls back to a scan, below).
4. **SIGTERM/SIGINT/SIGHUP** go through a self-pipe to a watcher thread that unparks and exits.
5. **Guardian process.** At launch the app spawns itself as `stage-manager --guardian <pid>`.
   When the parent dies without writing the clean-exit marker (SIGKILL, segfault, abort), the
   guardian runs the recovery scan, then waits for every window still parked (those the
   registry's parked file names, and any managed app's in the park zone) and moves each back
   once its Space shows.
6. **Recovery scan** (launch and guardian): every window of every app whose frame lies in a
   park zone is moved back to its saved frame from `state.json`, or centered on the display.
   After a main display change every app is scanned again (macOS puts windows back where
   they were the last time that arrangement was in use, parked ones included), leaving the
   windows we park alone; apps that do not answer, at launch or then, are scanned again for
   30 s. The scans go in slices (two apps a pass; one that does not answer holds the next pass
   off 3 s), so a busy app never holds the WM thread longer than that.
   A thin sliver at the edge (at most 8 pt by 256 pt on screen, past the right or bottom edge)
   counts as parked only for windows we manage or registered, and for apps with a window in
   `state.json`; any other app's window must sit clearly in the park zone.
7. **Restore frames are clamped** onto the visible display before being applied.

Across Spaces: the registry keeps every Space's parked windows (AX elements taken while
their Space showed). Quit is clean only when the window server lists no managed window in
the park zone on any Space; otherwise the guardian keeps watching after its scan and scans
again whenever such a window shows (its Space was visited), retrying while it stays shown,
since AX lists most apps' windows only on the shown Space. A window is done only when seen
back (AX reads it outside the park zone, or the window server lists it on screen outside it
in settled passes a second apart: a sliding Space can list in-between frames) or gone (not
listed on any Space for 3 s), or adopted by Stage Manager running again (`ctl adopted`: in
its model). It never exits while one waits; a new Stage Manager does not end the watch by
itself running. The guardian and the new app moving the same window at once is benign: the
guardian only ever moves a window out of the park zone, never into it, so the worst case is a
window shown that the new app meant to keep parked (until its next switch), never one left
parked; and it lets go of each window at its next pass once the new app reports it adopted.

## Testing hooks

- `cargo test`: engine (rules, transitions, placement, serialization, park-zone math).
- **ctl socket** (`~/.config/stage-manager/stage-manager.sock`, line protocol; not
  `$TMPDIR`, which macOS prunes, like the single-instance lock beside it): `switch N`, `send N`, `new`,
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

## Before public release

The owner runs a trial week on the daily log (`docs/DEVELOPMENT.md`, "Logs")
before the first public release. Before that release:

- [x] Lower the default `[log] level` (to `"info"`, or `"warn"`), and with it hide Mark a
  Problem by default (it shows only while the level is `debug` or `trace`). Done after
  1.1.0: `"info"`.
- [x] Decide which window titles the log keeps (all, none, or hashed) and say so in the guide.
  Hashed (a short FNV-1a code), titles as they are only with `[log] titles = true`; marks'
  state dumps the same; said in the guide's "Updates and privacy".
- [ ] Revisit the caps (`max_mb_per_day` 100, `max_mb_total` 500, `days` 14) against the
  trial's real volume (`stage-manager log summary` for each day).

Volume at the default level, measured 10-12 on the automated suite's logs (fixture apps
only; 0.4 h of app time over 220 launches): about 117 bytes a line; the scripted work
itself (switches, sends, clicks, drags, new windows, Space changes, ~780 focus changes) came
to 12 000 lines and 1.6 MB an hour, about 10 lines per user action and 1-2 per focus change;
each launch adds ~30 lines plus one per window found, each quit ~8. A person's working day
(~300 switches, ~1500 focus changes, ~100 new windows, ~30 Space changes, a launch or two)
is then about 7 000 lines, 1 MB; a heavy day 2-3 MB. Even the suite's machine pace for
8 hours stays near 13 MB, well under the 100 MB day cap.

## Accepted trade-offs (review rounds)

Known gaps a review found and the owner accepted as rare, left as they are:

- 6X review L3: the sliver rule (`src/engine/park.rs`, `Slivers`) still applies to every
  window the model manages, since the model sees only managed apps' windows and must not
  adopt a parked window macOS left a few points above the corner. A user's window pushed
  almost wholly off the right or bottom edge in the display's bottom-right quarter, with at
  most 8 pt by 256 pt on screen, is then taken as parked: its frame there is not recorded, a
  stage switch brings it back to its last frame, and the launch's scan centers it when its
  app has a window in `state.json`. Other apps' windows are judged without the rule.
- R15: `activation_frame` (`src/platform/controller.rs`), on a display whose only non-sticky
  shown window belongs to an app that posts no move notice, reads a horizontal-only user drag
  of that window as a Desktop slide and keeps its stale tracked frame.
- 8E3 restore: an app that opens a window by itself, with no user action, during the first
  5 minutes after a launch (the restore window, `src/engine/restore.rs`; it ends sooner at
  the first click, key press, card or Dock click) has a window matching a saved window of an
  inactive stage placed into that stage (a card, parked) rather than switched to: the
  restore cannot tell it from the app reopening its saved windows. 8F: it ends sooner too,
  once apps have been quiet 20 s and the login is a minute old.
- 8F settling: after a restart, while the login settles, a window matching no saved one
  (or a doubtful match) goes by Apple's rule without the switch: into its app's stage, or a
  stage of its own as a card, never brought forward, except that while the saved active
  stage is still empty (its app slow to open) a new stage takes the front and the saved
  stage waits inactive, its windows parked there with a card when they come (8F2, M4); a
  launch-time activation or focus switches nothing. Without the input monitor (no clicks or keys seen) settling ends only
  when apps go quiet or after 5 minutes; the end's focus never takes the keyboard from a
  window shown in the front stage.
- 8F logout head start (8F2, M3): at `willPowerOffNotification` every window comes back,
  the leftovers check runs and the stages are saved, but the app keeps running (strip
  hidden, input ignored) until macOS's own quit, which then only exits. A logout another
  app cancels sends no quit: a minute later (`LOGOUT_WAIT`) the app starts afresh from the
  saved stages, as at a launch; meanwhile every window shows, with no strip.
- 8H: the head start runs at the first logout signal (`loginwindow`'s distributed
  `com.apple.logoutInitiated` / `restartInitiated` / `shutdownInitiated` /
  `logoutContinued`, its notify(3) names, `willPowerOff` last); `logoutCancelled` resumes at
  once; the hold waits two minutes after the latest signal (an early signal may come with the
  confirmation dialog's one-minute countdown). An app's quit keeps its windows' entries in
  `state.json` (`quit_at`) for two minutes, so the stages survive a logout whose early signal
  never comes; a window reopened clamped against the park corner is matched by title and
  goes to its saved frame, an unmatched one is left and counted ("N at the corner").
- 8I: one pool after a restart. Native apps reopen their windows on their own saved
  Desktops, non-native ones (Electron) on the Desktop shown at login (10-16). Each Desktop
  still becomes its saved Desktop by its own windows (adoption unchanged); a stray, a window
  matching another saved Desktop's waiting entry with confidence, recreates its saved stage on
  the Desktop where it showed (a part; a stage split across two Desktops has a part on each),
  and the entry leaves the other Desktop. Strip: the adopted Desktop's saved slots, then the
  parts in their saved order. At the restore window's end by itself, the Desktop shown when
  saved (if shown) brings its saved active stage forward. The line: "K from other Desktops".
  8I2 (the review): with another saved Desktop waiting, the Desktop shown when saved also
  takes two windows while the restore window is open (one, as before, when it is the only
  saved Desktop or after the window); voters that can adopt nothing any more (another
  Desktop taken, or the window's end) join their saved stages where they are, else stay and
  their entries are forgotten; a saved Desktop's stages the pool emptied are never built at
  its visit (its most recent stage left takes the front); own-Desktop entries are scored
  first, other Desktops' only for a window nothing there takes with confidence.
  8I3 (the review): the card pressed or the number key that ends the restore window keeps
  its target: a voter alone in a stage of its own makes that stage its part (same id, number
  and card); a voter whose stage still goes leaves the stage's id and number to follow its
  window. Known, left: a part finds an existing stage by its saved stage's name
  (`find_named`); two Desktops saved with stages of the same name both feed the one stage of
  that name on the Desktop shown (the strays of two same-named stages share a part there).

## 8G: the real-restart check

With the owner, from the day's log (titles never leave it), besides the design's list:

- "Reopen windows when logging back in" on: Stage Manager itself comes back at the login.
  8F quit it at `willPowerOffNotification`, before loginwindow asked, which may have kept
  macOS from reopening it; 8F2 waits for macOS's quit. The log shows "logout, restart or
  shutdown coming: every window back …", then "asked to quit by the system" and "quitting
  after the logout's head start", with no "the quit could not run at once".
- Each app's windows reopen at their real frames (none in the park zone: no `warn` of a
  window brought back from it at the login).
- A logout cancelled by an app with an unsaved document: the stages are back at its cancel
  signal, else two minutes after the latest one ("the logout was cancelled after its head
  start").
- 8H: the order of the logout signals: every "logout signal" line (name, ms since the first,
  what it did). Which name comes first, and whether it comes before the first "app
  terminated"; then trim `SIGNALS` to what arrives. The restore line's "N at the corner".
- 8I: which apps' windows came back on another Desktop than their saved one (the "back in
  its saved stage" lines with `from_other=true`) and the line's "K from other Desktops".
- A slow app (Xcode, an Electron app) in the saved front stage: settling waits for its first
  window (8F2, M2), or a new app's window takes the front meanwhile (M4).

## Out of scope (prototype)

Multi-monitor beyond the main display (the model keys stages by display), fullscreen windows
(ignored). (Switch animations, drawn with snapshots, and drag and drop came later; see
`docs/UX-SPEC.md`; its measurement record of Apple's Stage Manager is that file as of
commit 20665a3.)
