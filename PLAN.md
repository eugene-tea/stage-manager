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

- **Stages per screen and Space.** Every screen has its own strip and stages, as Apple's
  does (round displays, UX-SPEC "Screens"): its own Spaces, park corner (one with no screen
  beyond it, `engine::park::choose_corner`) and strip capacity; a window belongs to the
  screen showing most of it (`engine::displays`), moved to another it starts a stage of its
  own there (`Model::windows_moved_to_display`), and a screen removed hands its stages to the
  main one (`Model::set_displays`). Screens are known by vendor, model and serial
  (`ScreenKey`), never by `CGDirectDisplayID`, in the model and in `state.json` (a Space
  names its screen). `[displays] main_only` keeps the first version's main-screen-only way.
  A stage is a group of windows on one Space (Desktop); every Space
  has its own stages, active stage and strip, as Apple's does. Inactive stages are parked in
  the park corner of their display (macOS keeps a sliver onscreen). Switching = save
  the active stage's live frames and z-order, park it, restore the target's frames, raise
  back to front, focus the remembered key window last. Nothing acts on another Space's
  windows except the safety paths.
- **Spaces by their windows.** No public API names a Space: the engine gives each a
  synthetic id and recognizes it on `activeSpaceDidChangeNotification` from the window
  server's on-screen list. A window is given to the engine only once it shows on screen
  (another Space, minimized, app hidden: it waits). A window wholly off every display (a
  headless browser's) is not the user's: it waits until it shows on one, and a managed one its
  app moves there is let go where it is (`engine::displays`). A once-a-second check catches
  moves between Spaces and missed notifications. No SkyLight/CGS calls.
- **Engine is pure Rust** (`src/engine`, `src/config.rs`): no Objective-C types, fully unit
  tested. Observations in (frames, z-order, focus, the Dock's order), `Action`s out (`Park`,
  `SetFrame`, `Raise`, `Focus`) for the platform layer to execute.
- **Platform layer** (`src/platform`): AX, CGWindowList, ScreenCaptureKit, Carbon hotkeys,
  NSWorkspace events, safety. Public APIs only, plus `_AXUIElementGetWindow`.
- **UI** (`src/ui`): status item and menu, Settings, the strip panel (CALayer cards) and the
  motion overlay. The overlay sits just below floating windows (an always-on-top player stays
  over a switch), and above everything only while a dragged window's card shows.
- **Threads:** AX can block on hung apps, so every AX call runs on the WM thread with a
  0.25 s messaging timeout; the main thread never calls AX. Every frame write runs with its
  app's `AXEnhancedUserInterface` off (`platform::enhanced_ui`: an app with it on animates
  Accessibility's moves), once per app per batch, on again at the batch's end, the WM tick,
  quit, the panic hook and SIGABRT. An activation notice the WM thread decides late
  (under load) acts only if it fired after our latest switch began and its app is still
  frontmost (`engine::stale_activation`). A move whose window handle answers
  `kAXErrorInvalidUIElement` looks the window up afresh by id in its app's `AXWindows` and
  tries once more with the new handle, kept; unlisted, off screen, on the Desktop shown (not
  at a display's size: a full-screen Space's), its app not hidden and no hidden tab's signature, on two looks a second apart, it is closed and
  let go as destroyed; the Space check looks at off-screen managed windows the same way
  (`engine::stale_handle`). Commands go main → WM through a
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
7. **Restore frames are kept within reach** (`Display::reach`): never in a park zone, the title
   bar below the menu bar with 80 pt of it on screen; otherwise exactly the window's own frame.

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
  (`find_named`). The restore's line names those windows; nothing tells the user (1.6.0's
  menu list taken out 11-06: he cannot act on it, macOS moves no window between Desktops).

## Open

- **Screens, after round displays** (built: a strip and stages per screen, Apple's rules
  as measured 11-05; a saved Desktop of a screen not connected forms where its windows are,
  the 10-23 login):
  - **Live with the owner:** the `displays` phase of the suite (fixtures on both screens;
    its first run), then the lid, by hand (his first run 11-05 16:27, fixed in round
    displays-lid; run it again): (0) two card clicks on the laptop: no "not in the engine's
    slot" warning, the log's "transition: strip order" with `display=1` and the laptop's
    stages; (1) on the laptop a stage in front and two cards, on the 5K a stage in front and
    a card; close the lid: the laptop's front stage comes to the 5K in front, the 5K's front
    to a card, the laptop's cards after the 5K's own, every window on screen (`ctl state`:
    one display, `displays[0].strip`); if macOS slides the 5K to another Desktop with the
    laptop's windows (11-05: its second Desktop, the log's "Space changed" with `rebuilt`
    and `carried=2`), the same there: the laptop's front in front, its card a card of its
    own, not joined to its app's card; while it is closed (his second run 11-05 20:01, fixed
    in round displays-lid-2), on both Desktops: one strip on the 5K, never a second one over
    it (after the log's "the strips of displays no longer there closed", never "a strip for
    another display"; "a snapshot for a display no longer there is not drawn" may show), each card
    one stage's windows, every card click switching (never "card click: nothing to switch
    to"), the laptop's front window shown, not left at the 5K's corner, and the 5K's cards at
    its corner, not pushed whole onto it; no switch from the app macOS activates after the
    close (round displays-lid-3: the log's "an activation macOS made after a display
    change", never "activated app lives in another stage" before his first click; a window
    listed for a moment then: "a window found after a display change", never "new window"
    and a switch, and "Space changed" with `held` and no new stage, round displays-lid-4), the 5K
    staying on the Desktop macOS slid to; back on the first Desktop its own front shows; (2)
    open the lid (his third run 11-05 20:26 left the laptop's cards on the 5K's second
    Desktop, fixed in round displays-lid-3): the stages macOS moves back are the
    laptop's again (the log's "window macOS put on a display that changed", never "moved to
    another display", and for a card never "a parked window is listed out of the park zone:
    parked again"), the shown one in front at the frame macOS gave it (no move after),
    the hidden ones cards at the laptop's corner (`card=true`; `ctl state`: the laptop's
    `strip` has them), nothing of the 5K's moving (`displays[1]`); then a card click on the laptop brings the card back at its old frame
    there; (3) a restart with both
    screens: each screen's stages on its screen (the log's "Space changed on another
    display" with `rebuilt`; a Desktop saved on a screen not connected: `other_screen`, and
    the restore line's "N Desktops on another screen"); his first, 11-06 04:23 (fixed in
    round displays-restart; run it again): a window its app reopened on the 5K whose stage
    was saved on the laptop goes back to the laptop, at its saved frame, its stage in front
    or a card as saved (the log's "a window shown on another screen than its saved stage's
    goes back to that screen", the restore line's "N windows back to their screen", never
    "on another Desktop: … (saved 17, now 2)" for the laptop's Desktop; `ctl state`: its
    `display` the laptop's, in `displays[1].active_stage` or `strip`); the "stage saved on
    another screen" line now names its saved Desktop and display; two windows of one app
    saved as two stages (VS Code's two workspaces, each its own stage on its own Desktop of
    the 5K, 77 and 78 then) come back as two stages, the one whose title gained the file in
    front of its workspace's name matched by its frame (a "strong" match, never "2 by
    Apple's rule" for it), and one titled late goes to its stage when its title comes ("matches its
    saved one by its new title"), even after his first click, unless he touched that window,
    and never past two minutes after the login ("no longer matched again by a new title"); Fork and Chrome stay
    as saved (they did, 11-06 04:23; at 06:29 Fork's second window, saved as its own stage
    on the other Desktop, reopened at its frame under another repository's name and joined
    the first's stage by Apple's rule: fixed in round restart-two-windows, run it again:
    the log's "a window at a saved window's frame under another name, the only one" and
    "grade=\"medium\"" for it, two Fork stages in `ctl state`, no "asked to join the current
    stage" line while the restore window is open); (4) ⌘-Tab to an app whose stage is
    on the other screen: that screen switches, the other does not, and the Desktop there is
    arranged ahead of its slide (the log's "arranged ahead" for a window on `displays[1]`);
    (5) round displays-2, on the laptop's screen: its cards' glass takes its own wallpaper
    (set a different picture there), its corner patch covers the slivers at its bottom-left
    corner (`displays[1].corner_patch.shown`, from a still: `displays[1].desktop_still.patch`
    "ready"), a window opened there with the 5K in a full-screen app is placed at once (the
    log's "the window's screen shows no Desktop now" never for it), a full-screen app on the
    laptop (`displays[1].fullscreen`) leaves the 5K's windows arriving, Mission Control on
    either screen, and a window moved by hand to another Desktop on the laptop's screen
    (the log's "window left its Space on another screen").
  - **Still the main screen's, by design (round displays-2):** the phase (a Space change
    settling, a switch under way) and the slide hold, which gate every screen while a
    change settles (no notice names the screen); the stage hotkeys, `ctl switch`, `ctl
    merge` and `ctl space`, which count the main screen's strip; the park, patch, glass
    and shadow probes and the debug hooks (`debug-overlay`, `debug-dock-press`); the sliver
    shield's zone for the main screen, always on; `ctl state`'s top-level `space`,
    `display`, `strip_width`, `strip_panel`, `corner_patch` and `desktop_still` (every
    screen's are under `displays[i]`).
  - The first launch after the 10-23 login drew card switches wrong (the incoming window
    showed at its place before its snapshot reached it; the log identical to a good run); a
    relaunch cured it. Catch it with a screen recording if it comes back; check what the
    overlay and the strip panels hold from a launch while the screens settle.
- **Reopen list (round reopen-list), at his next restarts** (installed copy; nothing live
  before). First login after the install: 1.6.x's agent starts the app ("launched" with
  `started="launchd agent"`), which moves to the login item ("moving to the way [app]
  login_start names", "registered" for the login item, "unregistered at the quit" for the
  agent: this copy is its job); Login Items lists Stage Manager once after the next quit. At
  the logout: "logout signal" then "reopen list: activation policy" `policy=Regular
  done=true` within a few ms of the first signal; the Dock icon shows until the quit. After
  the login: "Stage Manager starting" about 3 s after the login (macOS's `TALLaunchStart`),
  "launched: as macOS opened it" with `started="reopened by macOS"`, `listed_at_logout=true`
  and `login_item_event` (whether macOS marks its own reopen as a login item: unknown, the
  line says); no Dock icon, no Settings or other window; then the login item's copy, 10-30 s
  later, "already running … leaving without showing its Settings" (or "opened again moments
  after the login … no Settings" when macOS hands it over as a reopen). Not known before a
  restart: whether `loginwindow` reads the activation policy when the logout begins (our
  switch comes 1.3-1.7 s before `logoutContinued` and 9-18 s before the quit) or the
  `LSUIElement` key at the relaunch; whether the persistent-apps file lists Stage Manager
  (read in place only). A logout cancelled in its dialog: "policy=Accessory", the icon gone.
  `[app] login_start = "background_item"` moves back to the agent (the early-start checks).
- **Flaky live check (displays):** "one of our windows is listed at it" for the other screen's
  corner patch failed once (11-06 07:55, listed 0) and passed on the rerun (listed 1): the
  window server's listing races the patch's ordering in. Wait for it in the check, or find
  what orders it in late.
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
- **After the 10-29 Fable review** (merged as `review-1029`; review files in `tmp/fable-review/`):
  - **Deferred findings:**
    - K5: `state.json`'s fsync off the WM thread, ordered against the quit's save.
    - L5/U4: the switch cover's Dock carve-out, now that the overlay sits below the Dock;
      needs a look at the screen.
    - C4: a step journal for non-APFS swaps.
  - **Skipped, with reasons in the rounds' reports:** E6, W6, the S11 peer rule.
  - **Live checks the rounds named but never wrote:**
    - restore-frames: an off-edge window, a window tucked by the corner, a Dock change,
      a dialog born parked;
    - quit-frames: a late answer at quit, `kill -9` with a dialog, a fitted move after a
      crash, a reopen at the corner frame;
    - config-tools: a broken config at launch, with and without the last good copy;
    - ui-perf: the strip hidden stops captures, edge reveal, the tab across Spaces.
  - **The suite and macOS's notification layer:** a banner (iCloud, 10-29) puts
    NotificationCenter's full-screen layer-21 window over the strip for minutes, and every
    click check then refuses. Have the suite wait for it to go, and name it when it
    doesn't. The owner's own unmanaged windows do the same while his app is quit (Messages
    at the left edge, 1.5.0's run: 849/13, 12 of them refused clicks).
- **After 1.6.0:**
  - **Full-screen windows in the closed-window rule:** a window whose handle died is kept
    when the window server lists it off screen at about a display's size
    (`engine::stale_handle::on_full_screen_space`), since no public API names a window's
    Space. It only ever keeps a window; a surer source (the window's last read AXFullScreen)
    would retire the size test.
  - **Short-lived windows taken in and let go:** Safari's 53x48 dialog and Chrome's
    address-bar suggestion list (AXUnknown, 89 pt tall) are placed in a stage, then let go
    as closed a second later, many times a day (11-05 log). Apple's Stage Manager shows
    neither on a card: decide whether such windows are ever managed.
  - **Card glass, not measured:** Dark Mode; a window behind another on one card; the
    hovered card's growth (Apple's read 11 % wider, 11-05) against ours. The script's
    reading beside the card is in the display's colours, the fit's from the picture file.
  - **`hide_parked_apps` stays off** (its trial ended 11-04: a hidden app ⌘-Tabbed to
    skips the switch's motion). Its live phase 11-05 was 88 of 89: "supersede: Bravo and
    Charlie together again" left an empty stage 3. Look there before it is ever offered again.
- **Feature requests not built** (`docs/FEATURE-REQUESTS.md` §3): card context menu (R4),
  stage names from the UI (R7), strip overflow scroll with "+N" (R5), quiet activations
  (R6), a stage list in the menu (R2), the parity checks of §1.

## Out of scope

A unified stage across screens (FEATURE-REQUESTS R8: hard; Apple's has none), fullscreen
windows (ignored). UX-SPEC's measurement record of Apple's Stage Manager is that file as of
commit 20665a3.
