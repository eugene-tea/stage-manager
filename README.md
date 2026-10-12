# Stage Manager

A floating-window stage manager for macOS: Apple's Stage Manager strip and floating windows,
plus rules, hotkeys, named stages, pinning, placement and persistence. See `PLAN.md`.

Status: milestones 1–6 (skeleton, observation, stages with hotkeys and safety, rules and
placement, the strip, persistence). The app keeps stages of floating windows on one Space:
switching parks the stage you leave in the bottom-right corner and restores the target's
frames and z-order exactly; the strip on the left shows the other stages as cards (click to
switch, shift-click to bring a card's windows into the current stage).

## Build

Requirements: macOS 14+, Rust (edition 2024), Xcode command-line tools (`swiftc` for the
fixture), and the signing identity below.

```sh
scripts/make-cert.sh   # once: creates "Stage Manager Local Signing" in the login keychain
make run               # build, bundle, sign, launch, stream the log; Ctrl-C quits the app
```

The first signing with a new key makes macOS ask whether codesign may use it: choose
**Always Allow**. Until then `make sign` waits on that dialog (it gives up after
`SIGN_TIMEOUT` seconds, default 300).

### Makefile targets

| Target         | What it does |
|----------------|--------------|
| `make build`   | `cargo build --release` |
| `make bundle`  | assembles `target/Stage Manager.app` (`LSUIElement`, bundle ID `dev.eugene.stagemanager`, version from `Cargo.toml`) |
| `make sign`    | signs the bundle by SHA-1 with "Stage Manager Local Signing" (hardened runtime, no timestamp); fails pointing at `scripts/make-cert.sh` when the identity is missing; never ad-hoc |
| `make run`     | `sign`, then launches through LaunchServices (`open -n`) and tails `~/Library/Logs/Stage Manager/stage-manager.log`; Ctrl-C sends `ctl quit` (SIGTERM as a fallback) |
| `make install` | copies the signed app to `~/Applications` |
| `make fixture` | builds and signs `target/SM Fixture.app` (test windows, below) |
| `make check`   | `cargo test` and `cargo clippy --all-targets -- -D warnings` |

`make run` passes `RUST_LOG` (default `info,stage_manager=debug`), `STAGE_MANAGER_NO_PROMPT`,
`STAGE_MANAGER_ONLY`, `STAGE_MANAGER_HOME` and `STAGE_MANAGER_TEST` through to the app when
they are set.

Why LaunchServices: a binary started straight from a terminal has its TCC grants checked
against the terminal. Launched with `open`, macOS checks the bundle's own signature.

## Permissions

- **Accessibility** — observing and moving windows. Without it the app runs, says so in the
  menu, and starts observing within 2 s of the grant (no relaunch).
- **Screen Recording** — strip thumbnails (ScreenCaptureKit). The strip also asks through
  `SCShareableContent`, which is what lists the app in System Settings on recent macOS.

At launch the app asks macOS for whatever is missing (system dialogs), unless
`STAGE_MANAGER_NO_PROMPT` is set. The menu shows "Grant Accessibility…" / "Grant Screen
Recording…" while a grant is missing; each opens the matching Privacy pane.

The stable signature keeps the grants across rebuilds. Its designated requirement is
`identifier "dev.eugene.stagemanager" and certificate leaf = H"<sha1>"`; check with
`codesign -d -r- "target/Stage Manager.app"`.

## macOS settings

Apple's `com.apple.WindowManager` preferences are the source of truth wherever they exist
(`AppWindowGroupingBehavior`, `AutoHide`, `HideDesktop`, `GloballyEnabled`,
`StageManagerHideWidgets`, `StandardHideWidgets`, `EnableStandardClickToShowDesktop`). The
app re-reads them every 2 s and logs changes. Apple's own Stage Manager must be off while
this app runs; the log and the menu warn when `GloballyEnabled` is 1.

## Using it

- **Stages.** Every window belongs to one stage (or is pinned). Only the active stage is
  shown; the others are parked in the bottom-right corner (a sliver stays visible; macOS
  keeps part of every window onscreen). Clicking a parked sliver, ⌘-Tab or a Dock click on
  an app whose windows sit in another stage switches to that stage.
- **New windows** follow the rules in `config.toml` (below); by default they join the
  active stage where the app put them. Sheets, dialogs and panels follow their parent.
- **Click the wallpaper** to reveal the desktop (the active stage goes into the strip);
  click again to bring it back. This follows System Settings → Desktop & Dock → "Click
  wallpaper to reveal desktop": with **Only in Stage Manager** (the default) we reveal; with
  **Always**, macOS itself moves windows aside and we leave the click alone.
- **Quit** from the menu bar item brings every parked window back first.

### Hotkeys

| Default | Does | Config key |
|---------|------|------------|
| ⌃⌥1 … ⌃⌥9 | switch to stage N | `switch` |
| ⌃⌥⇧1 … ⌃⌥⇧9 | send the focused window to stage N (N = count + 1 makes a stage) | `send` |
| ⌃⌥N | send the focused window to a new stage | `new_stage` |
| ⌃⌥P | pin / unpin the focused window (shown on every stage) | `toggle_pin` |
| ⌃⌥S | show / hide the strip | `toggle_strip` |

Hotkeys are Carbon `RegisterEventHotKey` registrations, re-registered when the config
changes; one another app already holds is logged and skipped.

## Configuration

`~/.config/stage-manager/config.toml` (or `$STAGE_MANAGER_HOME/config.toml`), written with
commented defaults on first run and re-read within a second of every save (also Reload
Config in the menu, or `ctl reload`). A bad file keeps the last good config; the error shows
in the menu and the log. Open Config opens the real file. Apple's own Stage Manager settings
are read live from `com.apple.WindowManager` and never repeated here.

The default file, verbatim (`DEFAULT_CONFIG` in `src/config.rs`; a test keeps this copy in
sync):

```toml
# Stage Manager configuration.
#
# macOS settings come first: "Show recent apps", "Show windows from an application",
# "Click wallpaper to reveal desktop" and the rest are read live from
# System Settings → Desktop & Dock. This file only adds what macOS lacks.
# Changes are picked up automatically.

[strip]
# "native": Apple's slot-stable order (the stage you leave takes the clicked card's slot).
# "fixed": creation order, matching the hotkey numbers.
order = "native"
# Also show the active stage as a card (fixed order only; Apple never shows it).
show_active = false
# Show each card's hotkey number.
show_numbers = true
# How often thumbnails refresh, in milliseconds (minimum 250).
thumbnail_interval_ms = 1500

[defaults]
# What a new window does when no rule matches:
#   "current"       join the active stage
#   "new"           open a new stage and switch to it
#   "stage:<name>"  join the named stage (created if missing)
#   "float"         pin it: visible on every stage
#   "ignore"        leave it unmanaged
action = "current"
# Where it goes: "as-is", "center", or "cascade" (offset from the front window).
placement = "as-is"

# Rules for new windows, checked in order; the first match wins. `app` is a bundle ID
# (exact) or "*"; `title` is an optional regular expression. Leave out `action` or
# `placement` to use the defaults above.
#
# [[rule]]
# app = "com.example.chat"
# action = "stage:Chat"
#
# [[rule]]
# app = "com.example.terminal"
# title = "^Scratch"
# action = "float"
# placement = "center"
#
# [[rule]]
# app = "*"
# placement = "cascade"

[hotkeys]
# Modifiers: ctrl, alt (option), shift, cmd. "{n}" stands for the digits 1-9.
# An empty string disables a hotkey.
switch = "ctrl+alt+{n}"
send = "ctrl+alt+shift+{n}"
new_stage = "ctrl+alt+n"
toggle_pin = "ctrl+alt+p"
toggle_strip = "ctrl+alt+s"

[stages]
# Named stages created at launch, in this order (they take hotkey numbers 1, 2, ...)
# and stay even when empty. For example:
# names = ["Work", "Chat"]
names = []
```

Rules are checked in order for each new standard window; existing windows at launch keep
their stage from `state.json` (or join the active stage), except that a matching `ignore`
leaves them unmanaged and `float` pins them.

## Persistence

`state.json` next to the config is written about 500 ms after every change: stages in
order, their windows (bundle ID, title, frame, front-to-back position, key window, pinned),
the active stage and the strip slots. At launch, after the recovery scan, live windows are
matched to it (exact bundle ID + title first, then bundle ID alone), the stages are rebuilt,
the saved active stage is shown and the others are parked. Windows that match nothing join
the active stage.

## Safety: no window stays parked

1. A **registry** of every parked window (AX element + clamped restore frame), synced around
   every batch of window moves.
2. **Quit** (menu, `ctl quit`) unparks through the WM thread, then the registry, then a scan.
3. A **panic hook** unparks from the registry (a scan if it is busy) and exits; panics on the
   WM thread are caught and the registry resynced instead.
4. **SIGTERM / SIGINT / SIGHUP** go through a self-pipe to a watcher that quits cleanly
   (forcing the unpark itself if the main thread hangs); SIGABRT gets a best-effort unpark.
5. A **guardian** process (`stage-manager --guardian <pid>`) waits for the app with kqueue
   and runs the recovery scan unless the app wrote its clean-exit marker (`kill -9`, crash).
6. The **recovery scan** at launch and in the guardian: every window of every managed app
   stranded in the park zone goes back to its `state.json` frame, or is centered.
7. Every restore frame is **clamped** onto the visible screen.

## Environment

| Variable | Effect |
|----------|--------|
| `RUST_LOG` | log filter (default `info,stage_manager=debug`) |
| `STAGE_MANAGER_NO_PROMPT` | never show permission dialogs at launch |
| `STAGE_MANAGER_ONLY` | comma-separated bundle IDs: observe and manage only these apps (the recovery scan too) |
| `STAGE_MANAGER_HOME` | one directory for config, `state.json`, the ctl socket and the log (test runs) |
| `STAGE_MANAGER_TEST` | enables test-only ctl commands (`debug-panic`) |

## ctl socket

The app listens on `$TMPDIR/stage-manager.sock` (or `$STAGE_MANAGER_HOME/stage-manager.sock`;
a home path too long for a socket gets a hashed name in `$TMPDIR`), one command per line, one
line of JSON per reply. The same binary is the client (set the same `STAGE_MANAGER_HOME`):

| Command | Does |
|---------|------|
| `ping` | `{"ok":true,"pong":true,"pid":…,"version":…}` |
| `dump` | observed apps and windows (says so when Accessibility is missing) |
| `state` | stages, windows, restore and live frames, parked flags, strip, config error |
| `switch N` | switch to stage N |
| `send N [ID]` | send a window (default: the focused one) to stage N |
| `new [ID]` | send a window to a new stage |
| `pin [ID]` | pin or unpin a window |
| `merge N` | move stage N's windows into the active stage (a shift-click on its card) |
| `reveal` | reveal the desktop / bring the stage back (a wallpaper click) |
| `unpark-all` | bring every parked window back (the stages stay; a safety command) |
| `reload` | re-read `config.toml` |
| `quit` | unpark everything and quit |
| `debug-panic` | panic on the main thread (only with `STAGE_MANAGER_TEST=1`; a safety drill) |
| `debug-click X Y [shift]` | click our own strip at a global point (only with `STAGE_MANAGER_TEST=1`; refused unless the strip is the topmost window there) |

```sh
B="target/Stage Manager.app/Contents/MacOS/stage-manager"
"$B" ctl state | jq '.stages'
"$B" probe-windows dev.eugene.stagemanager.fixture   # the window server's view of an app
```

Exit status: 0 on `"ok":true`, 1 on an error reply, 2 when the app is not reachable.

## Test script

`scripts/test-stages.sh [m3] [safety] [m4] [m6] [strip]` builds the app and a private fixture
(`target/SM Fixture Stages.app`, bundle ID `dev.eugene.stagemanager.fixture.stages`, so it
never collides with other fixture runs) and checks, against `ctl state` and the window
server (`probe-windows`): stages from `send`, exact restore frames (±1 pt) and z-order across
switches, parked windows in the corner, merge, pin, reveal, child windows following their
parent; the safety drills (`ctl quit`, SIGTERM, SIGINT, a panic, `kill -9` with the guardian,
`kill -9` of app and guardian then a relaunch), each ending with every fixture window
onscreen; rules (`new`, `float` + `center`, `cascade`), hot reload and a bad config; and
persistence across a relaunch of the app and of the fixture; and a click and a shift-click
on a real strip card (switch, merge). Every run uses
`STAGE_MANAGER_HOME` (default a new `/tmp/sm-test.*`, or `TEST_HOME`) and
`STAGE_MANAGER_ONLY` set to the fixture. The fixture comes to the front during switches;
the previously frontmost app is activated again at the end. `SKIP_BUILD=1` skips the build.

## Known limitations

- Main display only; other displays are left alone.
- Windows jump (no animation); parked windows leave a sliver in the bottom-right corner.
- A dialog or panel's parent is a guess: the app's frontmost standard window at the time.
- New windows are matched to rules by their title when they appear; an app that sets the
  title later is matched with the early title.
- Hotkeys use US-layout key positions.
- Wallpaper clicks are detected with a global mouse monitor and a window-list hit test, so
  a click on a Finder desktop icon also counts as a wallpaper click.
- Persistence matches windows by bundle ID and title; two windows with the same title in one
  app may swap stages after a relaunch.

## Strip demo

`stage-manager strip-demo` runs the strip panel alone, fed by a made-up snapshot built from
the live window list every second (no Accessibility; it never moves a window): windows are
grouped by app into fake stages of 1, 2 and 3 windows, the first two apps share one card,
and the stage holding the frontmost window is "active" (its frames drive auto-hide). Clicks
are logged; the menu's Show/Hide Strip toggles it.

```sh
make fixture && open -n -g "target/SM Fixture.app" --args --windows 7 --background
open -n "target/Stage Manager.app" --stderr /tmp/strip-demo.log \
  --env STAGE_MANAGER_ONLY=dev.eugene.stagemanager.fixture --env STAGE_MANAGER_DEMO_NUMBERS=1 \
  --args strip-demo
```

| Variable | Effect |
|----------|--------|
| `STAGE_MANAGER_ONLY` | show only these apps (their offscreen windows too) |
| `STAGE_MANAGER_DEMO_NUMBERS=1` | number badges on cards |
| `STAGE_MANAGER_DEMO_AUTOHIDE=0\|1` | override Apple's `AutoHide` (hide recent apps) |
| `STAGE_MANAGER_THUMBNAIL_MS` | thumbnail refresh interval (default 1500) |

## Fixture app

`make fixture` builds `target/SM Fixture.app` (bundle ID `dev.eugene.stagemanager.fixture`):
N standard windows titled "Fixture 1…N", each a distinct flat color with its title drawn
large, at distinct frames, plus an optional utility panel "Fixture Panel".

```sh
open -n "target/SM Fixture.app" --args --windows 3 --panel
open -n -g "target/SM Fixture.app" --args --background   # no activation, windows ordered back
STAGE_MANAGER_ONLY=dev.eugene.stagemanager.fixture STAGE_MANAGER_NO_PROMPT=1 make run
```

Options also come from the environment: `FIXTURE_WINDOWS`, `FIXTURE_PANEL=1`,
`FIXTURE_BACKGROUND=1`. Automated runs that manage windows must use `STAGE_MANAGER_ONLY`
limited to the fixture.

## Layout

- `src/engine`, `src/config.rs` — pure Rust engine and configuration (unit tested).
- `src/platform` — macOS glue: `ax` (AXUIElement / AXObserver wrappers), `wm_thread` (the
  thread that owns every AX call), `observe` (observed apps and windows), `controller` (the
  engine on the WM thread: observations in, actions out, strip snapshots, `state.json`),
  `safety` (registry, panic hook, signals, guardian, recovery scan), `hotkeys` (Carbon),
  `desktop` (displays, wallpaper clicks), `capture` (ScreenCaptureKit thumbnails on a worker
  thread), `windows` (CGWindowList), `apps` (NSWorkspace), `permissions`,
  `native_settings`, `ctl`, `logging`, `paths`.
- `src/ui` — status item and menu; the strip: `strip` (NSPanel, CALayer cards, hover,
  clicks, slide), `strip_layout` (pure geometry and tilt), `strip_visibility` (pure
  auto-hide and edge reveal), `strip_model` (the controller contract), `strip_demo`.
- `scripts/` — signing identity, bundling, signing, run, fixture, `test-stages.sh`.
