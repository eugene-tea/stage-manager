# Stage Manager — development

How to build, sign, run and test Stage Manager. The architecture and its safety layers are
in [ARCHITECTURE.md](ARCHITECTURE.md); what is still to do in [PLAN.md](../PLAN.md); how the
app should behave is in [UX-SPEC.md](UX-SPEC.md); publishing a release is in
[RELEASING.md](RELEASING.md).

## Requirements

- A Mac with Apple silicon (the app is built for arm64 only), macOS 14 or later
- Rust: `rust-toolchain.toml` pins the version (rustup installs it on the first build)
- Xcode command-line tools (`swiftc` builds the test fixture)
- The local signing identity (below)

## First build

```sh
scripts/make-cert.sh   # once: creates "Stage Manager Local Signing" in the login keychain
make run               # build, bundle, sign, launch, and stream the log; Ctrl-C quits the app
```

The first signing with a new key makes macOS ask whether `codesign` may use it. Choose
**Always Allow**. Until then `make sign` waits on that dialog (up to `SIGN_TIMEOUT` seconds,
default 300).

The app is always signed with this stable self-signed identity, never ad hoc, so macOS keeps
its Accessibility and Screen Recording approvals across rebuilds. Check the designated
requirement with `codesign -d -r- "target/Stage Manager.app"`.

`make run` launches through LaunchServices (`open -n`): a binary started straight from a
terminal has its approvals checked against the terminal, not the app.

## Make targets

| Target | What it does |
|---|---|
| `make build` | `cargo build --release` |
| `make bundle` | assembles `target/Stage Manager.app` (version from `Cargo.toml`) |
| `make sign` | signs the bundle with the local identity (hardened runtime) |
| `make dev` | signs, then copies the app to `~/Library/Application Support/Stage Manager Dev/` |
| `make run` | `dev`, then launches that copy and tails today's log |
| `make install` | copies the signed app to `~/Applications` (refused while that copy runs: quit it first) |
| `make fixture` | builds `target/SM Fixture.app`, the test window app |
| `make check` | `cargo test` and `cargo clippy --all-targets -- -D warnings` |
| `make icon` | regenerates the app icon (needs Xcode 26; outputs are committed) |
| `make dmg-art` | re-renders the DMG window from `art/dmg.py` and `art/dmg/text.json` |
| `make social-art` | re-renders the info page's link-preview card from `art/social.py` |
| `make dmg` | a DMG of the locally signed app, for checking its layout only |
| `make release` | the public, notarized DMG and Homebrew cask ([RELEASING.md](RELEASING.md)) |

Dev runs and tests always launch from the fixed dev copy. macOS approves screen capture per
app location, so one location means one approval for every worktree. A copy running from
there (any home) is never replaced, and `make run` refuses before copying when a copy answers
in its home; the new copy is made beside the old one and swapped in, so a failed copy keeps
the old one (`python3 scripts/test_dev_app_unit.py` checks this offline). `make install` puts
its copy in `~/Applications` the same way. Neither script ever signals or removes a copy
running from anywhere else: `make run` stops only the dev copy's own process.

`make run` passes `RUST_LOG`, `STAGE_MANAGER_NO_PROMPT`, `STAGE_MANAGER_ONLY`,
`STAGE_MANAGER_HOME`, `STAGE_MANAGER_LOG_DIR` and `STAGE_MANAGER_TEST` through to the app.

## Testing

`make check` runs the unit tests and clippy; both must be clean. The engine (`src/engine`,
`src/config.rs`) is pure Rust with no Objective-C types, so most logic is tested there.

The model's tests run once per window engine: a test inside `both_engines! { .. }`
(`src/engine/testkit.rs`) becomes `<name>::corner` and `<name>::spaces`, and the test model
and the fake desk (`Desk`) hide windows the way that engine does. A test of the park
corner's own geometry goes in `corner_nuance! { .. }` and runs on the Corner engine only;
the Spaces engine's own tests are in `src/engine/spaces.rs`.

`scripts/test-stages.sh` runs the scripted checks against the real app, driving it through
its ctl socket and reading the window server's view of the windows:

```sh
scripts/test-stages.sh                    # every phase: m3 safety m4 m6 restore strip native
                                          # visible parity motion slots expose mission capture raise park
                                          # offscreen enhanced stale handles displays
scripts/test-stages.sh m3 safety          # some phases
scripts/test-stages.sh still              # by name only, as away, glass and crop (a combined
                                          # card drawn whole, one of its windows lifted off
                                          # alone in the column, dropped in a gap, dragged out)
scripts/test-stages.sh spaces             # the two-Desktop check, guided (you switch Desktops)
scripts/test-stages.sh spaces --auto      # the same, hands-free, on an unlocked idle Mac
SKIP_BUILD=1 TEST_HOME=/tmp/sm scripts/test-stages.sh
```

Rules for every run that manages windows:

- Limit management to fixture apps with `STAGE_MANAGER_ONLY=<bundle ids>`. Never move,
  park or close anyone's real windows.
- Use a separate home with `STAGE_MANAGER_HOME` (the script makes a new `/tmp/sm-test.*`).
  The script builds its own fixtures (`SM Fixture Stages.app`, and A, B and C for the
  `native` phase).
- Turn off Apple's Stage Manager: while it is on it rescales other apps' windows.
- Keep window captures off (`STAGE_MANAGER_NO_CAPTURE=1`). Only the `capture` phase turns
  them on, because a capture can raise macOS's screen-access alert.
- Turn on an extra in the test's own `config.toml`, never the user's.
- No phase runs while the installed Stage Manager (`~/Applications`) is running, and
  `dev-app.sh` never replaces a dev copy that runs.

Synthetic clicks and drags are posted only where the app's own or a fixture's windows are
topmost along the whole path, and the pointer is put back afterwards.

`scripts/test-update-quit.sh` checks an update installed at the user's Quit with a local
fake release: a test build (`--features update-test`) runs from a scratch folder, a copy
relabelled 99.0.0 sits on a local disk image, and the script checks that `ctl quit` installs
nothing, the next launch removes the left-behind copy and owes an early check, and the
menu's Quit (`ctl debug-menu-quit`) installs 99.0.0 without reopening it. It manages no app
and refuses to run beside a running Stage Manager; run it while at the Mac (an automatic
install goes by itself after 120 s with no input).

### Measuring cost

`scripts/measure.sh` measures what the app costs, the same way each time, under each window
engine and with the app not running (the floor), three runs each by default, and prints the
median and spread of every number (`scripts/measure_stages.py` says what each scenario reads):

```sh
scripts/measure.sh                        # rest sample switch call launch; off, corner, spaces
scripts/measure.sh rest sample            # CPU, wakeups and POWER at rest; where the time goes
RUNS=1 ENGINES="corner spaces" scripts/measure.sh switch
scripts/measure.sh --summary /tmp/sm-measure.XXXXXX   # the table again
```

- **rest**: four stages of two fixture windows, a minute untouched: the app's CPU time over
  wall time and wakeups (`proc_pid_rusage`), `top`'s POWER, the window server's and replayd's
  CPU. **sample**: the next minute under `sample`, split by what started the work (a timer
  and the function it calls, an Accessibility notice, Core Animation's commit, a capture
  reply) and by its kind.
- **switch**: card clicks and Dock clicks timed from the command to the last incoming window
  at its frame (`probe-spaces settle` reads the windows every ms), then again unsampled for
  the CPU of each switch.
- **call**: a call stand-in (the fixture's `--frame-clock`: a floating window that draws on
  every display refresh and counts the refreshes it missed) at rest and while other stages
  are activated.
- **launch**: how long a newly opened app's window shows over the stage being left.
- **hidden** (by name only): a window drawing on every display refresh (the fixture's
  `--frame-clock-normal`) hidden in its stage and shown again by `ctl switch`: whether it
  drew while hidden, how soon it draws again after the show, refreshes missed after.

`scripts/measure_engines.py` runs it for the park corner and the Spaces engine alone, with
captures off, three stages and `ctl switch` switches, and prints one table, corner against
spaces, with every number in `target/measure-engines.json` (UX-SPEC, "Window engines
measured"); `--dry-run` prints the plan and launches nothing, and
`python3 scripts/test_measure_unit.py` checks its parts offline.

Captures are on in every run, as installed; sign with a Developer ID (`CERT_NAME`) so the dev
copy keeps its Screen Recording grant. The numbers go to `measure.jsonl` in `OUT` (a new
`/tmp/sm-measure.*` by default), with each `sample` file; neither belongs in the repo.

### The fixture app

`make fixture` builds `target/SM Fixture.app` (`dev.eugene.stagemanager.fixture`): numbered
windows titled "Fixture 1…N", each a flat color, plus an optional utility panel.

```sh
open -n "target/SM Fixture.app" --args --windows 3 --panel
open -n -g "target/SM Fixture.app" --args --background   # no activation
STAGE_MANAGER_ONLY=dev.eugene.stagemanager.fixture STAGE_MANAGER_NO_PROMPT=1 make run
```

`kill -USR1` opens one more window; `kill -USR2` minimizes one. Options also come from
`FIXTURE_WINDOWS`, `FIXTURE_PANEL=1`, `FIXTURE_BACKGROUND=1` and `FIXTURE_TITLE`.

## Environment variables

| Variable | Effect |
|---|---|
| `RUST_LOG` | log filter; overrides `[log] level` |
| `STAGE_MANAGER_HOME` | one directory for the config, saved state, the ctl socket and the log |
| `STAGE_MANAGER_LOG_DIR` | the daily logs' directory alone |
| `STAGE_MANAGER_ONLY` | comma-separated bundle IDs: manage only these apps |
| `STAGE_MANAGER_NO_PROMPT` | no welcome window, permission prompts or automatic update checks |
| `STAGE_MANAGER_NO_LOGIN_ITEM` | never register or unregister the login item or the launchd agent |
| `STAGE_MANAGER_NO_CAPTURE` | window captures off, whatever the config says |
| `STAGE_MANAGER_TEST` | enables the test-only `ctl debug-*` commands |
| `STAGE_MANAGER_TEST_REOPEN_LIST` | with `STAGE_MANAGER_TEST` only: the copy joins the reopen list at a logout (turns regular) without a login item, as the live suite's restore phase checks |
| `STAGE_MANAGER_DEBUG_ACTIVATION_DELAY_MS` | with `STAGE_MANAGER_ONLY` set only: activation notices reach the WM thread this late (as under load; `test-stages.sh stale`) |
| `STAGE_MANAGER_UPDATE_VERSION` | builds with `--features update-test` only: the update check offers this version instead of asking GitHub |
| `STAGE_MANAGER_UPDATE_DMG` | builds with `--features update-test` only: an update installs from this local disk image instead of downloading one (a locally signed app then trusts its own designated requirement) |
| `STAGE_MANAGER_UPDATE_AUTO` | builds with `--features update-test` only: `1` makes `ctl debug-update` ready the update as an automatic install (it waits for the user to be away, or installs at the user's Quit) |
| `SM_TEST_TOGGLE_APPLE_SM` | `1` lets `scripts/test-apple-stage-manager.sh` (the stand-down), `scripts/test_apple.py` (the Apple mode: `mirror`, `switch`) and `scripts/probe_apple.py` toggle Apple's Stage Manager (each restores the setting on exit) |

## The ctl socket

The running app listens on `~/.config/stage-manager/stage-manager.sock` (or in
`$STAGE_MANAGER_HOME`): one command per line,
one line of JSON per reply. The running copy's own binary is the client. The socket is the
user's only (`0600`), and the app serves every command only to code signed as it is, judged by
the connection's audit token: its own binary, or another copy of the bundle signed with the
same identity. Any other program, `target/debug/stage-manager` (ad hoc signed) included, gets
`ping` and `settings` only (and a crashed copy's guardian `adopted` and `take-watch`), and the
refusal names the binary that would be served. Use a binary signed like the copy that runs:

```sh
B=stage-manager                                              # the Homebrew install (its symlink)
B="target/Stage Manager.app/Contents/MacOS/stage-manager"    # a dev build (make sign)
"$B" ctl state | jq '.stages'
```

| Command | Does |
|---|---|
| `ping` | whether the app runs, its version, and whether Apple's Stage Manager is on |
| `state` | stages, windows, frames, the strip, the Desktop shown, the config error |
| `dump` | observed apps and windows, and every Desktop's stages |
| `space` | the Desktop shown, as the engine sees it |
| `switch N` | switch to stage N |
| `send N [ID]` / `new [ID]` / `pin [ID]` | move a window (default: the focused one) to stage N, to a new stage, or pin it |
| `merge N` | bring stage N's windows into the active stage |
| `reveal` | reveal the desktop, or bring the stage back |
| `unpark-all` | bring every parked window back |
| `reload` | re-read `config.toml` |
| `settings` | show the Settings window |
| `mark [NOTE]` | write a problem mark into the day's log |
| `quit` | bring every window back and quit |

Exit status is 0 on success, 1 on an error reply and 2 when the app is not running. With
`STAGE_MANAGER_TEST=1` the `debug-*` commands post clicks, drags, Desktop and Mission
Control shortcuts, ⌘M (`debug-cmd-m`), full-screen toggles and the strip shown or hidden for the scripted checks,
`debug-minimize-button ID` gives a window's minimize button's frame (for `debug-click`), and `debug-stale-handle
ID` takes our handle on a window for dead until it is looked up afresh (`test-stages.sh
handles`); each refuses to act outside the app's own and `STAGE_MANAGER_ONLY` windows. See
`src/platform/ctl.rs`.

## Other tools

- `stage-manager preview settings|welcome|about|update|… [--dark|--light] [--pane PANE] [--mode MODE] [--png FILE]`
  opens one of the app's windows alone, or draws it to a PNG, without managing anything.
  `--mode accessibility|spaces|apple|paused|relaunch|fell-back` sets what the Advanced pane's
  mode picker shows.
- `stage-manager strip-demo` runs the strip alone, fed from the live window list, and never
  moves a window. Its variables (`STAGE_MANAGER_DEMO_*`, and an offscreen render with
  `--features debug-render`) are described in `src/ui/strip_demo.rs`.
- `stage-manager probe-windows <bundle-id>…` (or `--all`), `probe-screen` and `probe-patch` print the
  window server's view of apps, of the screen, and what the corner patch would draw.
- `scripts/probe_park.py` measures where macOS leaves a parked window.
- `scripts/probe_apple.py [dock-front|mission-pick|mission-pick-ours|frames-setup]` measures
  Apple's Stage Manager on the fixtures (with `SM_TEST_TOGGLE_APPLE_SM=1`; it restores
  `GloballyEnabled` and `AppWindowGroupingBehavior`): a Dock click on the app in front, a
  pick in Mission Control (and the same under the Spaces engine), frames per screen setup.
  `SM_PROBE_DOCK_CLICK=real` posts a real click on the Dock item instead of `AXPress`.
- `stage-manager probe-shadow` measures how far an inactive window's shadow reaches past
  each side of its frame, from windows of its own (the corner patch's reach). `--profile`
  prints the falloff of a key and an inactive window's shadow and the colour across each
  edge (the inner rim), `--compare` how closely a snapshot drawn as the overlay draws it
  (the window's own picture, its shadow and rim) matches; `--appearance dark|light` draws
  the probe's windows in that appearance (the system's is never changed), `--background
  GREY` fills them with a grey (a rim shows on it), `--corner PT` draws the comparison at
  that corner radius. Each state's `look` is how the captures read the window's appearance
  from its shadow: from the image above (`display`) and, when the process running the probe
  has Screen Recording, from the still the captures take (`captures`, ScreenCaptureKit).

## Logs

The app, its guardian process and one-shot commands write one file per local day to
`~/Library/Logs/Stage Manager/YYYY-MM-DD.log`. `[log] level` is `info` by default (the main
events); `debug` records every decision the app makes and shows **Mark a Problem** in the
menu. Day files are capped in size and kept 14 days.

- `stage-manager log summary [DATE|yesterday]` prints a day's counts, runs, marks, and every
  warning and error, deduplicated: short enough to paste.
- `stage-manager mark ["note"]` (or **Mark a Problem** in the menu) writes a marked block
  with the app's state and the last 30 seconds of detailed lines.
- At `debug`, a task that held the window thread 100 ms or more (a command, a tick, an
  Accessibility callback) logs one `slow WM task` line: its name, how long, the tick's slowest
  part, and how many such lines were held back (one a second per task at most). Each
  `state saved` line says how long the save took (`ms`, `encode_ms`, `write_ms`).

Window titles go into the log, and into a mark's state dumps, as a short hash
(`#1a2b3c4d`, the same for the same title in every process on this Mac: it is keyed with a
random key made once and kept owner-only in the config folder, `title-key`, so a guessed
title cannot be confirmed from a log alone) unless `[log] titles = true`;
lines name windows by id, pid and bundle id. `ctl dump` and `ctl state` still answer with
titles. Logs still name your apps and your stages: never commit one.

### The slide watch

Under the Spaces engine every Dock click on an app's item and every ⌘Tab release is watched
for a Desktop slide nobody asked for (`src/engine/slide_watch.rs`): macOS's "switch to a
Space with open windows for the application" taking the screen to another Desktop although
the stage's window was put home for the click. Nothing in behaviour changes, nothing is
polled, and the Corner engine writes nothing. Read a day with
`grep 'slide-watch' ~/Library/Logs/Stage\ Manager/*.log`.

- `slide-watch: outcome` (one per activation, written at the first tick, input or Desktop
  change once its outcome is known: 1.5 s after the release, up to 3 s while a Desktop
  change waits to be recognized or a slide to come back): `via=dock|cmdtab`, `bundle`,
  `parked=true|false|n/a` (the window put home was parked at the corner first; `n/a`:
  nothing was put home), `park_ms` (what the park took), `press_to_release_ms` (Dock: press
  to release; ⌘Tab: first Tab to ⌘'s release), `put_home`, `already_shown_fallback` (the
  switch found the app's incoming window already on screen and showed its stage at once),
  `desktop_before`, `desktop_after` (the main screen's), `slid`, `slid_back`, `away_ms` (how
  long the screen was away before it came back), `outcome=none|expected|person|slid`, `by`
  (the person's `desktop-key` or `mission-control`), `predicted` (the Desktop predicted at
  the press), `app_here`, `activated` (an activation of the app was notified: a ⌘Tab with
  `activated=false` was most likely cancelled), `notices`, and `why` when the park was
  skipped or did not take, or the window could not be put home.
- A Desktop change within 1.5 s of the release counts as `expected` when it goes to a
  Desktop arranged ahead for the click, the Desktop predicted at the press, or (the app
  having no window on the Desktop left) a Desktop with one of its windows; as `person` when
  Mission Control or App Exposé showed, or a ⌃-arrow key was heard, before it; else as
  `slid`. A trackpad swipe is not heard, and macOS may keep its ⌃-arrow shortcuts from the
  app: a swipe or key right after a click reads as a slide.
- `slide-watch: summary` (each hour with activity, `at=hour`; and `at=quit` when every
  window is brought back: the quit, a stand-down, a logout): counts since the launch,
  `dock_clicks`, `dock_parked`, `dock_fell_back`, `dock_slid`, `dock_slid_back`,
  `dock_slowest_park_ms`, the same for `cmdtab_`, and `new` (outcomes since the last
  summary). `ctl state` has the same counts under `slide_watch`.

## Configuration keys

Every key is defined once, in `src/config/schema.rs`, with its type, range, default and a
one-line explanation. That table drives parsing, the commented default file, `config
--explain` and [CONFIG.md](CONFIG.md). After changing a key:

- Regenerate the reference: replace the table in `docs/CONFIG.md` with the output of
  `stage-manager config --reference`. A unit test fails until it matches.
- If the default file's text changed, add the previous text as the next
  `src/config/legacy/default-N.toml`. A config file identical to an old default was never
  edited, so it is replaced with the new one at launch.

## Source layout

- `src/engine`, `src/config.rs` — the pure engine and configuration: stages, rules,
  placement, Desktops, persistence, safety decisions. Unit tested.
- `src/logbook`, `src/updates.rs`, `src/self_update.rs`, `src/app_state.rs` — the daily log,
  update checks, update installs (what a new version must prove, the swap and its rollback)
  and the app's own memory, also pure. `src/platform/updater.rs` downloads, verifies and
  hands the swap to `stage-manager --update-helper` at quit.
- `src/platform` — macOS glue: Accessibility, the window server, ScreenCaptureKit, hotkeys,
  input, displays, the WM thread, safety (registry, signals, guardian), ctl, login item.
- `src/ui` — the menu bar item, the strip and its motion, Settings, the welcome window and
  the other windows.
- `scripts/` — signing, bundling, the dev copy, the fixture, the scripted checks, release.
- `art/` — the icon and DMG sources.

## Releasing

See [RELEASING.md](RELEASING.md).
