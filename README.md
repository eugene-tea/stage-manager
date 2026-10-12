# Stage Manager

A floating-window stage manager for macOS: Apple's Stage Manager strip and floating windows,
plus rules, hotkeys, named stages, pinning, placement and persistence. See `PLAN.md`.

Status: milestones 1–6 (skeleton, observation, stages with hotkeys and safety, rules and
placement, the strip, persistence) plus Apple's default behavior (first start, new windows,
drag and drop, ⌘M, the card's app icon, One at a Time), Shift sugar, and a strip measured
against Apple's on a laptop and a 5K display. **Out of the box it looks and behaves like
Apple's Stage Manager**; every extension is off until `config.toml` turns it on, and every
look and behavior is a config key (below). The app keeps stages of floating windows on every
Space (Desktop), each with its own stages and strip, as Apple's does: switching parks the
stage you leave in the bottom-right corner and restores the target's frames and z-order
exactly; the strip on the left shows the Space's other stages as cards.

Everything works by mouse, the Dock and ⌘Tab, the way Apple's Stage Manager does. Hotkeys
are an optional extra (off by default); nothing needs one.

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

Apple's Stage Manager is the baseline; everything below works without a keyboard shortcut.

- **First start.** With no saved stages, every app with windows becomes its own stage, as
  when Apple's Stage Manager is turned on: with "Show windows from an application: All at
  Once" (System Settings → Desktop & Dock) one stage per app holding all its windows; with
  "One at a Time" one stage per window. The frontmost app's stage is shown; the others go
  to the strip, most recently used at the top. Later launches restore the saved stages
  (below); apps that were not saved get their own stages the same way. (`[reveal]
  launch_reveal = true` shows a strip a window covers for a moment at launch; Apple does
  not.)
- **Windows keep their frames**, as with Apple's: turning Stage Manager on moves or resizes
  nothing, and switching brings every window back at exactly its own frame, even where it
  overlaps the strip; the strip simply stays hidden while a window of the current stage
  covers it, and the pointer resting at the left edge brings it back. `[windows] fit =
  "move"` (or `"narrow"`) keeps placed windows clear of the strip instead; quit then gives
  every window back its exact earlier frame.
- **The strip looks like Apple's on every display**: cards at x = 15 pt, each window drawn
  at one scale (a window as tall as the visible screen becomes a 161 pt card), turned in
  perspective toward the screen's middle, 29.5 pt apart, the app icon on the bottom-left
  corner, 5 cards at 1130 pt of visible height and 6 at 1410 pt. Every number is a key in
  `[strip]`, `[cards]`, `[icons]`, `[motion]` and `[reveal]`.
- **Stages.** Every window belongs to one stage (or is pinned). Only the active stage is
  shown; the others are parked in the bottom-right corner (a sliver stays visible; macOS
  keeps part of every window onscreen). Clicking a card, clicking a parked sliver, ⌘Tab or
  a Dock click on an app whose windows sit in another stage switches to that stage.
- **New windows** follow Apple by default (`action = "native"`): a window of an app already
  on the current stage joins it; a window of an app that sits in another stage joins that
  stage and brings it forward; a newly opened app (or one whose windows were all closed)
  gets a stage of its own, and the stage you leave goes to the strip. With "One at a Time",
  a new window of an app that is not on the current stage opens a stage of its own (the
  app's newest window, shown alone). Rules in `config.toml` (below) can send named apps
  elsewhere. Sheets, dialogs and panels follow their parent.
- **Drag a card into the main area** to bring its windows into the current stage: press a
  card and move; a copy of its front window follows the pointer and grows to the window's
  size as it leaves the strip. Let go anywhere in the main area: the windows join the
  current stage and the front one lands where you dropped it (title bar under the pointer).
  Let go over the strip to cancel. A plain click still switches.
- **Drag a window onto the strip** (by its title bar) to give it a stage of its own: when
  the pointer reaches the strip area the strip slides in (if hidden) and shows a highlighted
  empty card where it will go. Let go there: the window leaves the current stage, its card
  appears in the strip, and it comes back where it was before the drag when you switch to
  it.
- **Hold Shift to bring things here** instead of going there (`[sugar] shift_brings_here`,
  on by default):
  - open an app with Shift held (Dock click, Finder double-click, Spotlight Shift-Return,
    Launchpad): its new window joins the current stage instead of opening a new one;
  - Shift-click an app in the Dock whose windows sit in another stage: its windows come to
    the current stage (all of them, or the most recent one with "One at a Time");
  - Shift-click a card: its windows come to the current stage.

  The app switcher is never affected: ⌘Tab and ⌘⇧Tab (which cycles backward) always switch
  stages, as Apple's do.
- **⌘M** (or the yellow button) sends the window to the strip as a card of its own, as
  Apple's does, instead of the Dock (`[windows] minimize_to_strip`; macOS may show its
  restore animation toward the corner for a moment).
- **Click a card's app icon** to switch to that card with every window of the app, also the
  ones sitting in other stages of the Space.
- **Spaces (Desktops).** Every Space has its own Stage Manager, as Apple's does: its own
  stages, active stage and strip, listing only that Space's windows. The same app can have
  stages on several Spaces (VS Code at work on Desktop 1 and at home on Desktop 2), each
  holding only that Space's windows. Clicking a card never changes the Space. Going to a
  Space shows its strip (the first visit after launch groups its windows one stage per app,
  or as saved); a Dock click or ⌘Tab to an app may switch Spaces the way macOS's own
  setting says ("When switching to an application, switch to a Space with open windows for
  the application", Desktop & Dock → Mission Control); we never fight it. A window moved to
  another Space (Mission Control, or dragged across) leaves its stage and joins the current
  stage there. A window on every Space ("Assign To: All Desktops") is treated as pinned.
  Full-screen apps are Spaces of their own: no strip there. Stage numbers (hotkeys, `ctl
  switch N`) count the current Space's stages.
- **One at a Time** (System Settings → Desktop & Dock): an app's waiting windows share one
  card; clicking it again opens the app's next window.
- **Click the wallpaper** to reveal the desktop (the active stage goes into the strip);
  click again to bring it back. This follows System Settings → Desktop & Dock → "Click
  wallpaper to reveal desktop": with **Only in Stage Manager** (the default) we reveal; with
  **Always**, macOS itself moves windows aside and we leave the click alone.
- **Quit** from the menu bar item brings every parked window back first, and every window
  we narrowed or moved clear of the strip back to its earlier frame.

### Hotkeys (optional, off by default)

Nothing needs a hotkey: every action above works by mouse, the Dock and ⌘Tab, and Apple's
Stage Manager has none. They are off by default because global hotkeys take key combinations
other apps may use; `[hotkeys] enabled = true` turns them on. Set any of them to `""` to
leave that one out.

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

`~/.config/stage-manager/config.toml` (or `$STAGE_MANAGER_HOME/config.toml`), re-read within
a second of every save (also Reload Config in the menu, or `ctl reload`). Look and motion
keys apply to the strip live. A bad value keeps the last good config (the error shows in the
menu and the log); unknown keys and out-of-range numbers only warn (numbers are clamped).
Apple's own Stage Manager settings are read live from `com.apple.WindowManager` and never
repeated here.

**The file holds no live values.** The first run writes every key commented out with its
built-in default and a one-line explanation; the defaults themselves live in the app, so a
new default reaches you without touching your file. Uncomment a line to change it. A file
byte-identical to the default file of an earlier version (never edited) is replaced with
today's at launch, and the log says so; any other file is left alone.

```sh
B="target/Stage Manager.app/Contents/MacOS/stage-manager"
"$B" config --print-defaults        # the commented default file
"$B" config --explain cards.tilt    # one key: what it does, type, range, default
"$B" config --check [file]          # validate a file (default: the real one)
"$B" config --reference             # the table below
```

Every key (generated from the same table as the file; a test keeps this copy in sync):

| Key | Default | Values | What it does |
|---|---|---|---|
| `strip.order` | `"native"` | "native", "fixed" | Card order: "native" is Apple's (the stage you leave takes the clicked card's slot); "fixed" is creation order. |
| `strip.show_active` | `false` | true or false | Also show the active stage as a card (fixed order only; Apple never does). |
| `strip.show_numbers` | `false` | true or false | Draw each card's stage number (for the hotkeys). Apple shows none. |
| `strip.show_names` | `false` | true or false | Draw stage names under cards. Apple shows none. |
| `strip.width` | `200.0` | number, 60 to 800 | Width of the strip column: the panel, and the area a window must cover to hide the strip. |
| `strip.capacity` | `0` | whole number, 0 to 30 | Most cards shown; 0 counts them from the screen's visible height (Apple: 5 at 1130 pt, 6 at 1410 pt). |
| `strip.capacity_pitch` | `190.0` | number, 20 to 2000 | With capacity = 0: room per card, in points of visible height. |
| `strip.capacity_margin` | `60.0` | number, 0 to 2000 | With capacity = 0: visible height kept free above and below the cards. |
| `strip.capacity_min` | `3` | whole number, 1 to 30 | With capacity = 0: never fewer cards than this. |
| `strip.capacity_max` | `9` | whole number, 1 to 30 | With capacity = 0: never more cards than this. |
| `strip.scale` | `1.0` | number, 0.25 to 4 | Multiplies every strip length (cards, gaps, icons, width). Apple: 1. |
| `strip.scale_to_display` | `false` | true or false | Also shrink strip lengths on screens smaller than 2560 x 1410 pt (by area, at least half). Apple does not. |
| `strip.thumbnail_interval_ms` | `1500` | whole number, 250 to 60000 | How often thumbnails are refreshed, in milliseconds. |
| `strip.thumbnail_resolution` | `2.0` | number, 0.5 to 4 | Thumbnails are captured at this multiple of their drawn size. |
| `cards.x` | `15.0` | number, -100 to 400 | Left edge of every card, from the screen's left edge. |
| `cards.full_height` | `161.0` | number, 10 to 1000 | A window as tall as the visible screen becomes a card this tall; every window gets the same scale. |
| `cards.min_height` | `97.0` | number, 0 to 1000 | Small windows are drawn at least this tall. |
| `cards.max_width` | `0.0` | number, 0 to 2000 | Widest card before the turn; 0 = as wide as a card of the whole screen. |
| `cards.gap` | `29.5` | number, 0 to 500 | Space between cards. The column is centered on the visible screen. |
| `cards.tilt` | `"auto"` | "auto" or a number, 0 to 85 | Turn of each card in degrees; "auto" is Apple's for the screen (44.2 at 1800 pt wide, 47.9 at 2560). |
| `cards.perspective` | `"auto"` | "auto" or a number, 50 to 50000 | Eye distance in points; "auto" is Apple's for the screen (1346 at 1800 pt wide, 2260 at 2560). |
| `cards.perspective_x` | `15.0` | number, -3000 to 3000 | The eye's horizontal position, from the screen's left edge. |
| `cards.perspective_y` | `0.0` | number, -3000 to 3000 | The eye's height, from the screen's vertical middle (down is positive). |
| `cards.corner_radius` | `4.0` | number, 0 to 60 | Corner radius of each window's thumbnail. |
| `cards.border_width` | `0.5` | number, 0 to 10 | Width of the thin edge around each thumbnail. |
| `cards.border_color` | `"#ffffff2e"` | color "#rrggbb" or "#rrggbbaa" | Color of that edge. |
| `cards.shadow_radius` | `4.0` | number, 0 to 100 | Blur radius of each thumbnail's shadow. |
| `cards.shadow_opacity` | `0.4` | number, 0 to 1 | Opacity of that shadow. |
| `cards.shadow_offset_x` | `0.0` | number, -100 to 100 | Horizontal offset of that shadow. |
| `cards.shadow_offset_y` | `2.0` | number, -100 to 100 | Vertical offset of that shadow (down is positive). |
| `cards.shadow_color` | `"#000000"` | color "#rrggbb" or "#rrggbbaa" | Color of that shadow. |
| `cards.max_windows` | `3` | whole number, 1 to 8 | Most windows drawn on one card. |
| `cards.stack_dx` | `17.0` | number, -200 to 200 | Windows of one app step this far right toward the front one (the one behind peeks out on the left). |
| `cards.behind_scale` | `1.0` | number, 0.3 to 1 | Each window behind another of the same app is drawn this much smaller. |
| `cards.second_app_dx` | `68.0` | number, -300 to 300 | A second app's window sits this far right of the first. |
| `cards.second_app_dy` | `14.0` | number, -300 to 300 | ...and this far up. |
| `cards.hover_scale` | `1.06` | number, 1 to 2 | The card under the pointer grows by this factor, about its center. |
| `cards.hover_ms` | `200` | whole number, 0 to 3000 | How long the hover growth takes, in milliseconds. |
| `cards.fade_ms` | `200` | whole number, 0 to 3000 | How long a new card takes to fade in, in milliseconds. |
| `icons.size` | `38.0` | number, 0 to 300 | Size of the app icon image (macOS icons carry a margin; about 31 pt shows). 0 hides icons. |
| `icons.center_dx` | `10.5` | number, -300 to 300 | The icon's center, right of the card's bottom-left corner (its leftmost window's). |
| `icons.center_dy` | `-10.5` | number, -300 to 300 | The icon's center, below the card's bottom-left corner (up is negative). |
| `icons.step` | `36.0` | number, 0 to 300 | A second app's icon sits this far right of the first. |
| `icons.max` | `3` | whole number, 0 to 10 | Most icons on one card. |
| `icons.shadow_radius` | `4.0` | number, 0 to 100 | Blur radius of the icon's shadow. |
| `icons.shadow_opacity` | `0.3` | number, 0 to 1 | Opacity of the icon's shadow. |
| `icons.shadow_offset_y` | `2.0` | number, -100 to 100 | Vertical offset of the icon's shadow (down is positive). |
| `motion.spring_stiffness` | `240.0` | number, 1 to 10000 | Spring stiffness for cards moving and the strip sliding. |
| `motion.spring_damping` | `31.0` | number, 0 to 1000 | Spring damping (31 with stiffness 240 and mass 1: no overshoot). |
| `motion.spring_mass` | `1.0` | number, 0.01 to 100 | Spring mass. |
| `motion.slide_distance` | `286.0` | number, 0 to 3000 | How far left the cards slide when the strip hides. |
| `reveal.edge_width` | `2.0` | number, 0 to 100 | The pointer within this many points of the left edge counts as at the edge. |
| `reveal.delay_ms` | `250` | whole number, 0 to 10000 | Resting at the left edge this long brings a hidden strip back. |
| `reveal.hide_delay_ms` | `500` | whole number, 0 to 10000 | A strip brought back hides again this long after the pointer leaves it. |
| `reveal.poll_ms` | `60` | whole number, 10 to 1000 | How often the pointer is checked while the strip is hidden. |
| `reveal.edge_tab` | `false` | true or false | Show a slim tab at the left edge while a window hides the strip (an extension). |
| `reveal.launch_reveal` | `false` | true or false | Show a covered strip for a moment at launch (an extension). |
| `reveal.launch_reveal_ms` | `3000` | whole number, 0 to 60000 | How long that launch showing lasts. |
| `drag.threshold` | `4.0` | number, 0 to 100 | A press that moves farther than this (points) drags the card; less is a click. |
| `drag.grow_distance` | `260.0` | number, 1 to 3000 | Past the strip, the dragged card grows to its window's size over this distance. |
| `drag.lifted_opacity` | `0.3` | number, 0 to 1 | Opacity of a card in the strip while it is dragged out. |
| `drag.ghost_opacity` | `0.92` | number, 0 to 1 | Opacity of the dragged copy of the card. |
| `windows.fit` | `"off"` | "off", "move", "narrow" | Keep placed windows clear of the strip: "move" right just enough, "narrow" also too-wide ones. Apple: "off". |
| `windows.fit_margin` | `16.0` | number, 0 to 500 | With fit: room kept between the strip column and the windows. |
| `windows.minimize_to_strip` | `true` | true or false | Cmd-M (minimize) sends the window to the strip as a card of its own, as Apple does, instead of the Dock. |
| `windows.focus_echo_ms` | `700` | whole number, 0 to 10000 | Focus changes this soon after we moved a window are taken as our own and never switch stages. |
| `sugar.shift_brings_here` | `true` | true or false | Hold Shift while opening or activating an app (Dock, Finder, Spotlight Shift-Return, Launchpad) to bring it here. |
| `sugar.launch_window_ms` | `10000` | whole number, 0 to 60000 | A newly opened app's first window counts a Shift held this long before. |
| `sugar.activation_window_ms` | `2000` | whole number, 0 to 60000 | An activation counts a Shift held this long before. |
| `hotkeys.enabled` | `false` | true or false | Register the hotkeys below. |
| `hotkeys.switch` | `"ctrl+alt+{n}"` | hotkey with {n} for the digits 1-9, or "" for none | Switch to stage N. |
| `hotkeys.send` | `"ctrl+alt+shift+{n}"` | hotkey with {n} for the digits 1-9, or "" for none | Send the focused window to stage N (N = count + 1 makes a stage). |
| `hotkeys.new_stage` | `"ctrl+alt+n"` | hotkey such as "ctrl+alt+n", or "" for none | Send the focused window to a new stage. |
| `hotkeys.toggle_pin` | `"ctrl+alt+p"` | hotkey such as "ctrl+alt+n", or "" for none | Pin or unpin the focused window (shown on every stage). |
| `hotkeys.toggle_strip` | `"ctrl+alt+s"` | hotkey such as "ctrl+alt+n", or "" for none | Show or hide the strip. |
| `stages.names` | `[]` | list of names | Named stages created at launch in this order (hotkey numbers 1, 2, ...); they stay when empty. |
| `defaults.action` | `"native"` | "native", "current", "new", "stage:<name>", "float", "ignore" | "native" is Apple's: a window joins its app's stage, a newly opened app gets its own. |
| `defaults.placement` | `"as-is"` | "as-is", "center", "cascade" | Where it goes: "as-is", "center", or "cascade" (offset from the front window). |

`cards.tilt` and `cards.perspective` default to `"auto"`: Apple's values for the display,
measured on a 1800 pt wide laptop (44.2°, 1346 pt) and a 2560 pt wide 5K display (47.9°,
2260 pt), linear between them and held outside. `[[rule]]` tables (new-window rules) are
documented by example at the end of the file:

```toml
[[rule]]
app = "com.example.chat"      # a bundle ID, or "*"
title = "^Scratch"            # optional regular expression
action = "stage:Chat"         # see [defaults] action
placement = "center"          # see [defaults] placement
```

Rules are checked in order for each new standard window; existing windows at launch keep
their stage from `state.json` (or join the active stage), except that a matching `ignore`
leaves them unmanaged and `float` pins them.

## Persistence

`state.json` next to the config is written about 500 ms after every change, per Space (each
with a synthetic id; macOS has no public one): stages in order, their windows (bundle ID,
title, frame, front-to-back position, key window, pinned), the active stage and the strip
slots. At launch, after the recovery scan, the Space shown is recognized by its windows
(exact bundle ID + title; with no match, the Space shown when saved), its windows are matched
to it (exact bundle ID + title first, then bundle ID alone), the stages are rebuilt, the
saved active stage is shown and the others are parked. Windows that match nothing get stages
the way a first start does. Every other saved Space is rebuilt the same way the first time
you go there (macOS lists most apps' windows of a Space only while it shows), and is saved
again untouched until then. Version 1 files (one Space) are read as one Space.

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
   While a window stays stranded on another Space (Accessibility reaches most apps' windows
   only on the Space shown), it keeps watching the window list and scans again when that
   window shows (you went to its Space), until none is left or Stage Manager runs again.
6. The **recovery scan** at launch and in the guardian: every window of every managed app
   stranded in the park zone goes back to its `state.json` frame, or is centered.
7. Every restore frame is **clamped** onto the visible screen.

Every Space counts: the registry holds parked windows of every Space (their AX elements
were taken while their Space showed), and Quit is clean only when the window server lists
no managed app's window in the park zone on any Space; otherwise the guardian takes over.

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
| `dump` | observed apps and windows (says so when Accessibility is missing), and `spaces`: each Space's synthetic id, display, whether it shows, its active stage and stage ids (no titles), plus windows waiting to show and saved Spaces not visited yet |
| `state` | stages (with their Space), windows, restore and live frames, parked flags, strip, the Space shown, config error |
| `switch N` | switch to stage N (of the Space shown) |
| `send N [ID]` | send a window (default: the focused one) to stage N of its Space |
| `new [ID]` | send a window to a new stage |
| `pin [ID]` | pin or unpin a window |
| `merge N` | move stage N's windows into the active stage (a shift-click on its card) |
| `reveal` | reveal the desktop / bring the stage back (a wallpaper click) |
| `unpark-all` | bring every parked window back (the stages stay; a safety command) |
| `reload` | re-read `config.toml` |
| `quit` | unpark everything and quit |
| `debug-panic` | panic on the main thread (only with `STAGE_MANAGER_TEST=1`; a safety drill) |
| `debug-click X Y [shift]` | click at a global point (only with `STAGE_MANAGER_TEST=1`; refused unless our own window or a `STAGE_MANAGER_ONLY` app's window is topmost there) |
| `debug-drag X1 Y1 X2 Y2 [X Y …] [shift]` | a left-button drag through the points (only with `STAGE_MANAGER_TEST=1`; refused unless our own or a `STAGE_MANAGER_ONLY` app's window is topmost at every point of the path); the pointer goes back |
| `debug-frame ID X Y W H` | set a shown managed window's frame and bring it to the front (only with `STAGE_MANAGER_TEST=1`) |

```sh
B="target/Stage Manager.app/Contents/MacOS/stage-manager"
"$B" ctl state | jq '.stages'
"$B" probe-windows dev.eugene.stagemanager.fixture   # the window server's view of an app
```

Exit status: 0 on `"ok":true`, 1 on an error reply, 2 when the app is not reachable.

## Test script

`scripts/test-stages.sh [m3] [safety] [m4] [m6] [strip] [native] [visible] [parity]` builds the app and private
fixtures (`target/SM Fixture Stages.app`, bundle ID `dev.eugene.stagemanager.fixture.stages`,
plus `SM Fixture Stages A/B/C.app` with IDs `….stages.a/.b/.c` for the native phase;
`SM_FIXTURE_ID` picks another base ID, so runs never collide) and checks, against `ctl state` and the window
server (`probe-windows`): stages from `send`, exact restore frames (±1 pt) and z-order across
switches, parked windows in the corner, merge, pin, reveal, child windows following their
parent; the safety drills (`ctl quit`, SIGTERM, SIGINT, a panic, `kill -9` with the guardian,
`kill -9` of app and guardian then a relaunch), each ending with every fixture window
onscreen; rules (`new`, `float` + `center`, `cascade`), hot reload and a bad config; and
persistence across a relaunch of the app and of the fixture; and a click and a shift-click
on a real strip card (switch, merge); and Apple's defaults with three fixture apps (first
start one stage per app, frontmost active, strip in recency order; a new app's window opens a
new stage; a new window of a parked app's stage switches there; a new window of the active
app joins; a Shift launch joins the current stage; a Shift activation brings an app's windows
here; a card dragged into the main area merges and lands at the drop point; a window dragged
onto the strip gets a stage of its own and keeps its pre-drag frame); Apple's frames by
default (nothing moved or resized at first start, on switches or at quit, no edge tab, no
launch reveal, no hotkey) and, with `[windows] fit` turned on in the test's own config,
windows kept clear of the strip and given back their exact frames at quit; the generated
config with no live values, ⌘M sending a window to the strip (the fixture minimizes a
window on SIGUSR2) and a click on a card's app icon bringing every window of the app.
Tests that need an extension turn it on in their own `config.toml`. Synthetic clicks and
drags are posted only where our own or a fixture's windows are topmost along the whole path,
and the pointer is put back. Apple's own Stage Manager must be off for the window-position
checks: while it is on it scales inactive apps' windows into its own strip. Every run uses
`STAGE_MANAGER_HOME` (default a new `/tmp/sm-test.*`, or `TEST_HOME`) and
`STAGE_MANAGER_ONLY` set to the fixtures. The fixture comes to the front during switches;
the previously frontmost app is activated again at the end. `SKIP_BUILD=1` skips the build.

## Known limitations

- Apple's own Stage Manager must be off while this app runs (both move windows).
- Main display only; other displays are left alone.
- Spaces are told apart by their windows (no public API names them): an empty Desktop is
  forgotten when you leave it, and a Desktop whose windows are all hidden or minimized is
  taken for a new one until one of them shows (then it is recognized). Each Space's strip
  appears once the Space has slid in (Apple's slides in with it). The first visit to a
  Space after launch parks its windows into stages, one per app (or as saved).
- A window dragged onto the strip keeps its full size until it is dropped (Apple shrinks it
  into a card); a highlighted placeholder card shows where it will go. A dragged card's
  ghost is a snapshot of its front window, not the live window.
- Shift sugar reads the modifier at the user's triggering click or key press: a newly
  opened app's first window counts within 10 s of a Shift-held click or key press (unless
  another click or key press came after it), an activation within 2 s; or Shift held at the
  moment. A slow app launched with Shift may miss the 10 s window.
- Windows jump (no animation); parked windows leave a sliver in the bottom-right corner.
- ⌘M: the window is parked, then un-minimized there, so macOS may show a short restore
  animation toward the bottom-right corner (Apple's window shrinks into the strip).
- Thumbnails are stills refreshed every 1.5 s (Apple's cards are live).
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
the live window list every second (no Accessibility; it never moves a window): one stage per
app, as Apple's "All at Once" (so it compares like for like with Apple's strip), and the
stage holding the frontmost window is "active" (its frames drive auto-hide). Clicks are
logged; the menu's Show/Hide Strip toggles it. Its look comes from the built-in defaults, or
from `$STAGE_MANAGER_HOME/config.toml` when that is set (re-read when saved, never written).

```sh
make fixture && open -n -g "target/SM Fixture.app" --args --windows 7 --background
open -n "target/Stage Manager.app" --stderr /tmp/strip-demo.log \
  --env STAGE_MANAGER_ONLY=dev.eugene.stagemanager.fixture --env STAGE_MANAGER_DEMO_NUMBERS=1 \
  --args strip-demo
```

| Variable | Effect |
|----------|--------|
| `STAGE_MANAGER_ONLY` | show only these apps (their offscreen windows too) |
| `STAGE_MANAGER_DEMO_LAYOUT=chunks` | stages of 1, 2 and 3 windows per app, the first two apps sharing a card (stacks) |
| `STAGE_MANAGER_DEMO_NUMBERS=1` | number badges on cards |
| `STAGE_MANAGER_DEMO_AUTOHIDE=0\|1` | override Apple's `AutoHide` (hide recent apps) |
| `STAGE_MANAGER_THUMBNAIL_MS` | thumbnail refresh interval (default: the config's) |

Built with `cargo build --release --features debug-render`, the demo can also draw the strip
offscreen with Core Animation's renderer, so the look can be checked while the screen is
locked or without Screen Recording (thumbnails then stay blank):

| Variable | Effect |
|----------|--------|
| `STAGE_MANAGER_DEMO_RENDER=<file>` | after 2.5 s, write the strip (slid in, 2 px per pt) to `<file>` as a `BGRA <w> <h>` line plus raw rows, and quit |
| `STAGE_MANAGER_DEMO_SCREEN=<w>x<h>+<menu bar>` | lay the strip out for another screen, for example `1800x1169+39` |
| `STAGE_MANAGER_DEMO_FAKE=<bundle id>:<w>x<h>;…` | made-up windows instead of the live list, front to back (the first is active) |

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
`FIXTURE_BACKGROUND=1`, `FIXTURE_TITLE` (title prefix, also `--title T`). `kill -USR1` opens
one more window, ordered front without activating the app; `kill -USR2` minimizes one. Automated runs that manage windows must use `STAGE_MANAGER_ONLY`
limited to the fixture.

## Layout

- `src/engine`, `src/config.rs` — pure Rust engine and configuration (unit tested);
  `src/config/schema.rs` lists every key with its type, range, default and doc line.
- `src/platform` — macOS glue: `ax` (AXUIElement / AXObserver wrappers), `wm_thread` (the
  thread that owns every AX call), `observe` (observed apps and windows), `controller` (the
  engine on the WM thread: observations in, actions out, strip snapshots, `state.json`),
  `safety` (registry, panic hook, signals, guardian, recovery scan), `hotkeys` (Carbon),
  `desktop` (displays, wallpaper clicks), `input` (the global mouse and key monitor, live
  modifiers, the Shift sugar's trigger record, test-only synthetic input), `capture` (ScreenCaptureKit thumbnails on a worker
  thread), `windows` (CGWindowList), `apps` (NSWorkspace), `permissions`,
  `native_settings`, `ctl`, `logging`, `paths`.
- `src/ui` — status item and menu; the strip: `strip` (NSPanel, CALayer cards, hover,
  clicks, card drag with its ghost, the drop target for window drags, slide), `strip_layout` (pure geometry and tilt), `strip_visibility` (pure
  auto-hide and edge reveal), `strip_model` (the controller contract), `strip_demo`.
- `scripts/` — signing identity, bundling, signing, run, fixture, `test-stages.sh`.
