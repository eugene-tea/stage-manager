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
| `make build` | `cargo build --release` (`FEATURES=probes` adds the probes) |
| `make bundle` | assembles `target/Stage Manager.app` (version from `Cargo.toml`) |
| `make sign` | signs the bundle with the local identity (hardened runtime) |
| `make dev` | builds with the probes, signs, then copies the app to `~/Library/Application Support/Stage Manager Dev/` |
| `make run` | `dev`, then launches that copy and tails today's log |
| `make install` | copies the signed app to `~/Applications` (refused while that copy runs: quit it first) |
| `make fixture` | builds `target/SM Fixture.app`, the test window app |
| `make check` | `cargo test --features probes` and `cargo clippy --all-targets -- -D warnings`, with and without the probes |
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

### The bench

The bench (`src/platform/controller/bench.rs`) runs the real controller, the real observer
and the real window engine (Corner or Spaces) in a unit test, on a fake desk
(`src/platform/fake.rs`) and a frozen clock. The desk stands in for macOS at the platform's
leaves only (the window list, Accessibility, input state, the frontmost app, the Dock, the
screens, the strip, SkyLight's Space calls): each leaf asks the desk first under
`cfg(test)`, and the desk answers as macOS was measured to (an activation and its notices,
a Desktop slide and its notice, a window in a stage Space on no Desktop). Notices are queued
with their measured delays times a **load factor**, and every platform call costs time on
the clock, so a scenario run at load 3 or 10 is the same script on a slower Mac. Files go
to a scratch home.

The incidents are in `src/platform/controller/bench/scenarios.rs`: each scripts what the
person and macOS did, as its log tells it, and its test asserts what came of it at load 1,
3 and 10. An assertion marked `// TIME DECIDES:` names the constant that changed the
outcome under load.

`src/platform/controller/bench/causes.rs` pins the focus and activation chains ("who caused
this notice", `engine/attribution.rs`): one scenario for each gate, from the incident its doc
comment tells. Their outcomes also read what each focus and activation notice came to, in
the order handled (the trail: `attribution::judged` notes the verdict of every notice;
`Bench::trail`), so a change that moves a notice from one gate to another shows even when a
person would see the same. After such a change, print what to assert:
`BENCH_PRINT=1 cargo test --lib causes::print_them -- --nocapture`.

`src/platform/controller/bench/switch.rs` pins a switch's handshake with the strip and the
raise-order check after it: the strip answering in time, late or never (`Strip::covers_ms`,
`Strip::draws`), a second click, a click or an app's activation inside the cover wait or the
check, an app reordering its windows late (`App::reorders_ms`), and the pictures a switch
takes of the windows leaving (`shots` among the calls; `ahead`: those taken when a switch
was likely, waited for by nothing, which come `Desk::shot_ms` later; `corner`: the park
corner's cover sent ahead of the pictures; `Desk::pictured` says the strip took
one itself, as at a press on a card or at ⌘Tab; `Bench::likely` tells a sign the main thread
would, the pointer in the Dock or a hotkey's modifiers; `Desk::captures_off` is a Mac with no
Screen Recording; `Desk::corner_covers` lets an activation grow from its card under the
Corner engine). Each outcome reads what a
person sees, how many switches played with their pictures (`Strip::covered`, `uncovered`)
and the calls in order. What macOS does on its own clock, whatever the app's thread is
doing, the desk plays with `Desk::at` (at a time) and `Desk::on_raise` (a while after the
app's raise of a window). Print what to assert:
`BENCH_PRINT=1 cargo test --lib switch::print_them -- --nocapture`.

- One scenario with its story (the notices handled and the calls made, in order) and the
  app's own log lines:
  `BENCH_ONE="Dock click" BENCH_LOAD=3 BENCH_LOG=debug cargo test --lib bench_one -- --nocapture`.
- Every scenario's outcome at each load, and the time limits crossed only under load:
  `cargo test --lib load_table -- --nocapture`.

To add a scenario: write a function of the load that builds a `Bench` (the engine, the
config text, the desk's apps and windows), scripts the incident with the bench's verbs
(`click`, `card_click`, `dock_click`, `cmd_tab`, `mission_open`, `go_to_desktop`,
`activate`, `advance(ms)`, or the desk's own through `desk(|d| ..)`) and returns the outcome
in a few words (with `Bench::trail` when the scenario is about who caused a notice); cite the doc comment or commit that tells the incident; add it to `ALL` and
assert its three outcomes in a test. Something macOS does that the desk does not play yet
goes in the desk, with where it was measured, never in the scenario.

`scripts/test-stages.sh` runs the scripted checks against the real app, driving it through
its ctl socket and reading the window server's view of the windows:

```sh
scripts/test-stages.sh                    # every phase: m3 safety m4 m6 restore strip native
                                          # visible parity motion slots expose mission looks capture raise park
                                          # offscreen enhanced stale handles displays reads
scripts/test-stages.sh m3 safety          # some phases
scripts/test-stages.sh still              # by name only, as away, glass and crop (a combined
                                          # card drawn whole, one of its windows lifted off
                                          # alone in the column, dropped in a gap, dragged out)
scripts/test-stages.sh rest               # by name only: the housekeeping rests 2 s under every
                                          # engine; a press on a parked window's sliver, judged on
                                          # a list over a second old at rest, is still swallowed
scripts/test-stages.sh spaces             # the two-Desktop check, guided (you switch Desktops)
scripts/test-stages.sh spaces --auto      # the same, hands-free, on an unlocked idle Mac
SM_DESKTOPS=guess scripts/test-stages.sh spaces --auto   # the same with the Desktops guessed
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
  CPU, and the windows captured a minute (none by default; `SM_SNAPSHOT_MS=2000` runs the
  full-size pictures' timer the Accessibility mode had until 11-25, to measure what it
  costs on the same build). **sample**: the next minute under `sample`, split by what started the work (a timer
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
- **covered** (by name only): at rest with the strip following the Dock, the windows clear
  of the strip and then flush left (the strip slid away): captures, Dock reads, window lists
  and LaunchServices lookups a minute. It fails above 5 lookups a minute in a span where no
  app launched or quit (`costs.apps_changed` says whether one did).

`rest` also records, from `ctl state` before and after its minute, every number of `costs` a
minute (window lists, LaunchServices lookups, stills of the desktop, wallpaper reads), the
ticks that ran brisk and at rest (the two reads stir the tick: about 12 brisk ticks are
theirs), the rest's period and how the corner patch shows. What one part of the app costs is
measured by one run with it off or slowed, each in an `OUT` of its own (every row says what
it ran with):

```sh
E="RUNS=3 ENGINES=corner"                                   # add "off spaces" for the floor and the other engine
env $E OUT=/tmp/sm-rest.base scripts/measure.sh rest sample
env $E OUT=/tmp/sm-rest.nocap SM_CAPTURE=0 scripts/measure.sh rest          # no ScreenCaptureKit: no still of the desktop
env $E OUT=/tmp/sm-rest.nopatch SM_CONFIG="windows.corner_patch=false" scripts/measure.sh rest   # no patch panel, still or icons read
env $E OUT=/tmp/sm-rest.notap SM_CONFIG="windows.inert_slivers=false" scripts/measure.sh rest     # no event tap
env $E OUT=/tmp/sm-rest.one SM_STAGES=1 scripts/measure.sh rest             # nothing hidden: nothing in the corner
env $E OUT=/tmp/sm-rest.750 SM_ENV="STAGE_MANAGER_TICK_REST_MS=750" scripts/measure.sh rest       # the tick as before 11-25
env $E OUT=/tmp/sm-rest.10s SM_ENV="STAGE_MANAGER_TICK_REST_MS=10000" scripts/measure.sh rest     # nearly no tick
```

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

Every `STAGE_MANAGER_*` and `SM_*` variable the app or the scripts read. Three different
switches each mean "a test run", and a test switch is honoured only with its gate set:

- **`TEST`**: `STAGE_MANAGER_TEST` is set (the `ctl debug-*` commands and the switches below
  marked so);
- **`ONLY`**: `STAGE_MANAGER_ONLY` is set (a run on fixture apps);
- **`ONLY`/`HOME`**: `STAGE_MANAGER_ONLY` or `STAGE_MANAGER_HOME` is set.

The installed app is never started with any of them, so none acts on a user's copy (but see
`STAGE_MANAGER_LANDING`). The three stay apart for now.

### The app

| Variable | Test only | Effect |
|---|---|---|
| `RUST_LOG` | no | log filter; overrides `[log] level` |
| `STAGE_MANAGER_HOME` | no | one directory for the config, saved state, the ctl socket and the log; also counts as a test run for `NO_CAPTURE` and for the activation policy (`ui/shell.rs`) |
| `STAGE_MANAGER_LOG_DIR` | no | the daily logs' directory alone |
| `STAGE_MANAGER_ONLY` | no | comma-separated bundle IDs: manage only these apps; the gate of the `ONLY` switches |
| `STAGE_MANAGER_NO_PROMPT` | no | no welcome window, permission prompts or automatic update checks |
| `STAGE_MANAGER_NO_LOGIN_ITEM` | no | never register or unregister the login item or the launchd agent |
| `STAGE_MANAGER_TEST` | yes | enables the test-only `ctl debug-*` commands; the gate of the `TEST` switches |
| `STAGE_MANAGER_NO_CAPTURE` | yes, `ONLY`/`HOME` | window captures off, whatever the config says |
| `STAGE_MANAGER_ENGINE` | yes, `ONLY` | `spaces`: the Spaces window engine, whatever the config says (`SM_ENGINE` sets it) |
| `STAGE_MANAGER_PROBE_FAIL` | yes, `ONLY` | `hide` or `show`: that step of the Spaces engine's launch probe is not sent, as on a Mac that refuses it |
| `STAGE_MANAGER_DEBUG_ACTIVATION_DELAY_MS` | yes, `ONLY` | activation notices reach the WM thread this late (as under load; `test-stages.sh stale`) |
| `STAGE_MANAGER_CAPTURE_SCK` | yes, `ONLY` | every window picture through ScreenCaptureKit, the window server's own source off (as before 1.9) |
| `STAGE_MANAGER_CAPTURE_LIST` | yes, `ONLY` | a switch's windows on screen are pictured in one call of the window server's capture instead of one call a window (to be measured: `probe-spaces capture BUNDLE fast list`) |
| `STAGE_MANAGER_DESKTOPS_GUESS` | yes, `ONLY` | the window server's Desktops are not read: Desktops by the windows on screen (as 1.9.2 under the Corner engine) |
| `STAGE_MANAGER_LANDING` | `ONLY`, or any run under `[windows] engine = "spaces"` | the landing variant: `pictures` (the default), `ordered`, `live`, `live-early`, `live-late` (`ui/landing_math.rs`; the only test switch a user's config can open) |
| `STAGE_MANAGER_LANDING_LEVEL` | as `STAGE_MANAGER_LANDING` | `ranked`: the live landing's windows get levels by rank instead of as made |
| `STAGE_MANAGER_TEST_REOPEN_LIST` | yes, `TEST` | the copy joins the reopen list at a logout (turns regular) without a login item, as the live suite's restore phase checks |
| `STAGE_MANAGER_SESSION` | yes, `TEST` | the window server session the app takes itself to run in: a restart simulated without restarting the Mac |
| `STAGE_MANAGER_LOGOUT_NOTE` | yes, `TEST` | the logout signals are this one distributed notification only (the live suite posts it) |
| `STAGE_MANAGER_DROP_AX_DESTROYED` | yes, `TEST` | Accessibility's destroyed notifications ignored, as from an app that sends none: a closed window leaves through the sweep alone |
| `STAGE_MANAGER_COVER` | yes, `TEST` | `desktop`: a switch's cover is always the desktop's still, kept fresh |
| `STAGE_MANAGER_TICK_REST_MS` | yes, `TEST` | the housekeeping rests this long (250 to 60000) in place of 2 s: what a tick costs, measured (750 was the Accessibility engine's until 11-25) |
| `STAGE_MANAGER_UPDATE_VERSION` | builds with `--features update-test` only | the update check offers this version instead of asking GitHub |
| `STAGE_MANAGER_UPDATE_DMG` | builds with `--features update-test` only | an update installs from this local disk image instead of downloading one (a locally signed app then trusts its own designated requirement) |
| `STAGE_MANAGER_UPDATE_AUTO` | builds with `--features update-test` only | `1` makes `ctl debug-update` ready the update as an automatic install (it waits for the user to be away, or installs at the user's Quit) |
| `STAGE_MANAGER_THUMBNAIL_MS` | `strip-demo` only | the demo's capture interval |
| `STAGE_MANAGER_DEMO_LAYOUT` | `strip-demo` only | `chunks`: each app's windows in stages of 1, 2, 3 (default `native`, one stage per app) |
| `STAGE_MANAGER_DEMO_NUMBERS` | `strip-demo` only | `1`: number badges on the cards |
| `STAGE_MANAGER_DEMO_AUTOHIDE` | `strip-demo` only | `0` or `1`: overrides Apple's `AutoHide` |
| `STAGE_MANAGER_DEMO_MOTION` | `strip-demo` only | `1`: a card click plays the switch animation over the real windows |
| `STAGE_MANAGER_DEMO_FAKE` | `strip-demo` with `--features debug-render` | `<bundle id>:<w>x<h>;…` stands in for the window list |
| `STAGE_MANAGER_DEMO_SCREEN` | `strip-demo` with `--features debug-render` | `<w>x<h>+<menu bar>`: the screen the render draws for |
| `STAGE_MANAGER_DEMO_RENDER` | `strip-demo` with `--features debug-render` | draws the strip offscreen into this file (raw BGRA) and quits |

### The scripts (all test only)

| Variable | Read by | Effect |
|---|---|---|
| `SM_APP`, `SM_BIN` | `scripts/bundle.sh` | the bundle to assemble and the binary to put in it (default `target/Stage Manager.app`, `target/release/stage-manager`) |
| `SM_DEV_APP` | `scripts/dev-app.sh`, `test_stages.py` | where the dev copy goes and runs from (`make install` points it at `~/Applications`) |
| `SM_FIXTURE_ID` | `test-stages.sh`, `measure.sh`, `test_stages.py` | the main fixture's bundle id |
| `SM_ENGINE` | `test-stages.sh`, `test_stages.py`, `measure_stages.py` | `spaces` runs the suite under the Spaces engine (sets `STAGE_MANAGER_ENGINE`); default the Corner engine |
| `SM_DESKTOPS` | `test_stages.py`, `test_spaces.py` | `guess` runs every app with the window server's Desktop reads off (sets `STAGE_MANAGER_DESKTOPS_GUESS`); default the reads on |
| `SM_SCREENSHOTS` | `test_stages.py` | a folder: turns on the visible phase's strip screenshots |
| `SM_SHOTS` | `test_stages.py`, `test_spaces.py` | a folder: turns on `spaces --auto`'s by-eye screenshots and the Mission Control pixel checks |
| `SM_PIXELS` | `test_stages.py` | turns on the checks read back from the overlay's pixels |
| `SM_OVERLAY_TRACE` | `test_stages.py` | prints each overlay read back's time and answer |
| `SM_SKIP_LANDING` | `test_stages.py` | skips the landing blend check |
| `SM_STILL_CLICK` | `test_stages.py` | `1`: runs the desktop still's icon click with "Click wallpaper to reveal desktop" on |
| `SM_LAUNCHES` | `test_stages.py`, `measure_stages.py` | how many launches the launch phase makes (20; the measure's 10) |
| `SM_LANDING_SWITCHES`, `SM_LANDING_MID_S`, `SM_LANDING_ORDER_SWITCHES`, `SM_LANDING_MERGE_ROUNDS` | `test_stages.py` | the landing phases' switches (12), the abort's moment (0.5 s), the order phase's and the merge phase's rounds (4 each) |
| `SM_AUTO` | `test_spaces.py` | `1`: the Spaces check hands-free, as `--auto` |
| `SM_DOCK_REPEATS` | `test_spaces.py` | how many Dock arrivals `spaces --auto` makes (3) |
| `SM_CMDTAB_ONLY` | `test_spaces.py` | `N`: the ⌘Tab steps alone, N times |
| `SM_DISPLAY_GONE_WATCH` | `test_display_gone.py` | how long the watch after a display goes lasts (65 s) |
| `SM_TEST_TOGGLE_APPLE_SM` | `test-apple-stage-manager.sh`, `test_apple.py`, `probe_apple.py`, `probe_press_quad.py` | `1` lets the script toggle Apple's Stage Manager (each restores the setting on exit) |
| `SM_PROBE_DOCK_CLICK` | `probe_apple.py` | `real`: a real click on the Dock item instead of `AXPress` |
| `SM_STAGES`, `SM_SETTLE_S`, `SM_REST_S`, `SM_SWITCHES`, `SM_SWITCH_CPU_S`, `SM_SWITCH_WAYS`, `SM_HIDDEN_ROUNDS`, `SM_HIDDEN_S`, `SM_AFTER_S` | `measure_stages.py` | the measure's stages (4), settle (10 s), rest (60 s), switches (20), CPU window per switch (1.5 s), switch ways (`card dock`), hidden rounds (8) and length (5 s), and the watch after (1.5 s) |
| `SM_CAPTURE` | `measure_stages.py` | `0`: the app runs with captures off (`STAGE_MANAGER_NO_CAPTURE`) |
| `SM_CLICK_HOLD_MS` | `measure_stages.py` | `N`: each switch's click holds the button N ms between press and release (`hold=N` on `ctl debug-click` and `ctl debug-dock-click`); not set, the test's own clicks (a card's 60 ms, a Dock item's fed at once). A person's is about 100 |
| `SM_SNAPSHOT_MS` | `measure_stages.py` | `N`: `[animation] snapshot_interval_ms = N` in every scenario's scratch config (the full-size pictures on a timer; none by default) |
| `SM_CONFIG` | `measure_stages.py` | `section.key=value ...` (space-separated): lines added to every scenario's scratch config, merged by section |
| `SM_ENV` | `measure_stages.py` | `STAGE_MANAGER_NAME=value ...`: test switches of the app for the run (never the home, the fixtures, the engine or the captures: the measure's own) |

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
`debug-minimize-button ID` gives a window's minimize button's frame (for `debug-click`), `debug-dock-lend
BUNDLE` feeds a Dock click without pressing the item (what the press put home waits for an
activation that never comes), `debug-dock-hover BUNDLE` moves the pointer onto a fixture's
Dock item and leaves it there (`debug-pointer-back` puts it back), a click takes `hold=MS`
(`debug-click X Y hold=100`, `debug-dock-click BUNDLE hold=100`: the button held as a
person's), and `debug-stale-handle
ID` takes our handle on a window for dead until it is looked up afresh (`test-stages.sh
handles`); each refuses to act outside the app's own and `STAGE_MANAGER_ONLY` windows. See
`src/platform/ctl.rs`.

## Other tools

The probes, the strip demo and the previews below (`probe-spaces`, `probe-shadow`,
`probe-park`, `probe-patch`, `probe-glass`, `probe-icons`, `strip-demo`, `preview`), and
`ctl debug-cmd-tab` and the rejected routes of `ctl apple try`, are in a build with the
`probes` feature only: the dev copy (`make dev`, `make run`) and every live script
(`scripts/test-stages.sh`, `scripts/measure.sh`, `scripts/probe_park.py`) build with it;
`make install`, `make dmg` and `make release` build without it, and such a build answers
each of them with one line and a non-zero exit. The feature adds only the probes' own modules
and their dispatch, so the live suite tests the product code a release ships.
`probe-windows`, `probe-screen` and `debug-post-space` are in every build.

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
- `scripts/probe_apple.py [dock-front|mission-pick|mission-open|mission-close-key|…-ours|frames-setup]`
  measures Apple's Stage Manager on the fixtures (with `SM_TEST_TOGGLE_APPLE_SM=1`; it restores
  `GloballyEnabled` and `AppWindowGroupingBehavior`): a Dock click on the app in front, a
  pick in Mission Control, its opening, its close by key with the pointer over a hidden
  window (each also under the Spaces engine), frames per screen setup.
  `SM_PROBE_DOCK_CLICK=real` posts a real click on the Dock item instead of `AXPress`.
- `scripts/test-stages.sh probe-system [dock|flip|names|indicator] [--rounds N] [--window ID]`
  (`scripts/probe_system.py`, `src/platform/system_probe/`) runs four probes from the dev app
  on fixture windows and prints tables and verdicts: whether the Dock announces Mission
  Control, App Exposé and Show Desktop (`AXExpose…`) and how soon against the window list;
  when the window server's current Desktop flips during a slide against its notice; what
  `SLSCopyManagedDisplaySpaces` calls the screens and Spaces, what an "All Desktops" window
  lists, and the call's cost; which capture lights the purple screen-recording indicator
  (`indicator` by name only: it may raise the screen-capture alert).
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

### Time values that came close

Every decision made by a time window ("this focus came within 700 ms of our own, so it is
ours") and every wait's end is counted by its constant's name (`src/engine/margin.rs`):
how far into its limit it was, as a share (0.93 is 93 % of the limit), and which way it was
judged. Nothing in behaviour changes. A value that is often judged close to its limit, or a
wait that often ends by its cap, is the one a slower or busier Mac may get wrong.

- `log summary` ends with **Time values that came close**, one row per name: `limit`,
  `inside` (the closest share still judged inside), `outside` (the closest judged outside),
  `close` (judged inside at 0.5 to 1 of the limit), `near` (outside, at 1 to 2), `waits`
  and `capped` (waits ended, and ended by their cap rather than by what they waited for),
  `predated` (notices stamped before the action they were compared with: macOS delivered
  them late, or out of order), `noticed` (waits met at a look a window-server notice brought
  forward; the other met ones were met at the poll's look). Only names with a close call, a
  near miss, a cap, a predated notice or a noticed wait are written.
- The rows come from `margin summary:` info lines, one per name that came close, written
  with the hourly and the quit counts; each covers the time since the one before.
- At `debug`, `margin: name=… elapsed_ms=… limit_ms=… inside=…` says each run of close calls
  as it happens (once as it comes close and once as it crosses, not at every check), and
  every wait ended by its cap (`ended=cap`).
- `ctl state`'s `margins` has the whole run's table.
- The cause ledger (`engine/causes.rs`: what our own focus, raise and frame expect of macOS)
  says when its count is off. `EXPECTED_FOCUS` and `EXPECTED_MOVE` are its waits: `capped`
  counts the expectations that ended unanswered (by their 30 s cap, or passed over when a
  later request to the same app was answered first); `FOCUS_ECHO_FALLBACK` counts the focus
  notices judged by the echo window while something was still expected of their app. At
  `debug` each is one line, `cause ledger: …`, with the app's bundle id, the window id and
  the age. `ctl state`'s `causes` has what is expected now: `open` (`open_focus`,
  `open_moves`, `open_activations`) and `oldest_ms`.

Shares are measured to when macOS's notice reached the app, not to when the window thread
got to it (docs/ARCHITECTURE.md, "Time"): a busy thread no longer shrinks a window.

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
- What ended each lend: `dock: the window put home at the press goes back: WHY` and
  `cmd-tab: the window put home for the switcher goes back: WHY` (another app was activated,
  the press was no click, the next press, the switcher was cancelled, or its cap), with
  `since_ms`, `cap` and `activated`; a lend its switch took is a debug line. The margins
  table's `LEND_WAIT`, `ACTIVATION_WAIT`, `UNREAD_WAIT`, `SILENT_ACTIVATION_WAIT`,
  `SILENT_UNREAD_WAIT`, `DOCK_SWITCH_CAP` and `GONE_WAIT` rows count how each wait ended
  (`capped`: by time). `cmd-tab: the key tap heard the switcher's end` says what the HID tap
  fed the switcher; `ctl state` `mission_control.tap` has `switcher_armed`,
  `switcher_escapes_heard` and `command_ups_heard`.

### The window server's notices

Under the Spaces engine (docs/ARCHITECTURE.md, "The window server's notices"):

- At the launch: `window-server notices: registered` (`types`, `remove`), and 5 s after the
  launch probe `window-server notices: heard` (`never_heard`: the types not heard by then) or
  `... silent (none came) ...`. The first notice of each type logs one
  line; `a membership notice's window and Space read by content` once the layout is learned.
- `a window the model counts hidden was out of its stage's Space: hidden again` carries
  `by_notice`; `window-server notice: windows nothing answered for let out ...` says the
  notice let strangers out in place of the tidy; the launch watch's line has
  `held_by_notice`.
- `ctl state`'s `engine.notices`: `registered`, `notices` (`heard` once any type came,
  `silent` when none came within 5 s, `waiting`), `never_heard` (the types never heard: 811
  there means the launch watch's nudge never fires, as on 11-21), `probe` (the panel's own
  notices, which teach the membership layout), `layout`, `expected`, `dropped`, `types` (per type `heard`, `last_ms_ago`, `sent`
  bursts, `coalesced` notices folded into them, `ours`) and `uses` (what was brought forward:
  `overview_looks`, `reconciles`, `held_back`, `strangers_out`, `space_checks`,
  `workspace_first`). `margins` names the waits: `OVERVIEW_LOOK`, `OVERVIEW_SETTLE`,
  `OVERVIEW_ARRIVAL_SETTLE`, `OVERVIEW_EXIT_CARRY`, `STRAY_LOOK`, `TIDY_EVERY`,
  `SPACE_POLL`, `launch_watch::EVERY`.
- Under the Corner engine (round looks-forward) `engine.notices` is there too, with the
  three types it hears, and under both engines `trust`: `state` (`working`, `silent`,
  `resting`, `probing`, or `off` when they are not registered), `way` (`window server` or
  `public triggers`), `why`, `told`, `missed`, `for_nothing`, `rests`; `uses.rested` counts
  the notices that brought no look during a rest. `mission_control` (and `ctl
  debug-mission-control status`) answers under the Corner engine: `up`, `tap`
  (`wanted_by_engine`: on for Mission Control's keys), `keys`, and `looks` (`public_triggers`,
  looks `made` and `found`, and how many openings each of the key, the Dock's notice and the
  window server's announced). `costs` has `server_notices`, `dock_notices`, `trigger_looks`
  and `taps_on.mission_keys`. `ctl debug-mission-control launch` opens Mission Control by
  its launcher, with no key.
- `ctl space`'s `overview_ms_ago`: `found`, `closed`, `told` (the last notice while an
  overview showed) and `settled_by_notice`.
- The live phases `mission`, `expose`, `launch` (`SM_ENGINE=spaces`) and `spaces --auto`
  print, per wait, how many ended at the notice's look, at the poll's and by the cap;
  `mission` checks by order and cause, never by a time limit: Mission Control and App Exposé
  opened by their shortcuts are found at their notice's look (`OVERVIEW_LOOK` ended by the
  notice), the close's notice comes after the closing key and before the list shows the close,
  and the look after it ends the freeze (`OVERVIEW_SETTLE` ended by the notice); the ms are
  printed (skipped when no notice 1508 came).
- The live phase `looks` (the Corner engine, a default phase; the Mac untouched meanwhile):
  nothing that brings a look forward fires in 10 s at rest and the key tap is off; Mission
  Control by its shortcut is known within 400 ms of the ask at a look brought forward
  (`OVERVIEW_LOOK` ended by a trigger's look), its close ends the freeze with no settle when
  its notice came, no hidden window is asked to park again; opened by its launcher it is
  judged only when a notice came for it; and `mission_control_key_keeps_stage` keeps the
  stage at a close by key. With no notice 1508 on a Mac the first rounds are said, and the
  round after the public triggers took over is judged.
- On the bench `bench/looks.rs` has the Corner engine's scenarios: the desk lists the windows
  in Mission Control's layout with `d.mission_lays_out`, sends the Dock's notice at its open
  with `d.dock_tells_mission`, has no notices to register with `d.server_calls = false`, and
  `looks_before.txt` pins each as it read before anything was heard there.
- On the bench the desk sends them only with `d.server_notices = true` (scenarios "a hidden
  window taken out of its stage Space" and "Mission Control with no key", each with and
  without).

### The Desktops: what the listing decides, and the shadow

Under both engines the window server's listing says which Desktop arrived and where each
window is (`src/platform/controller/desktops.rs`, `src/engine/model/desks.rs`; design r3,
slices 2 and 3; ARCHITECTURE, "The window server's Desktops"), and the model is compared
with it at every arrival and look (`src/engine/desk_shadow.rs`). The reads are a layer of
their own with their own check and fallback (`src/platform/desk_reads.rs`,
`src/engine/desk_reads.rs`).

- `ctl state`'s `desks.reads`: `way` (`window server`, or `windows on screen` while the
  reads do not decide), `state` (`working`, `resting`, `probing` after a rest, `off`), `why`
  (the calls missing, the test switch, a disagreement), `rest_s`, and `checks`: `agreed`,
  `disagreed`, `rests`, `last_disagreement`. The log: `desktops: the window server's
  Desktops can be read` once at the first read (or why not, a warning); `desktops: the
  window server's listing disagrees with the windows on screen: its reads rest` (a warning,
  the first rest of a run) and `... agrees with the windows on screen again: it decides
  again`; at debug, each look that disagreed with every screen's Space and the windows
  asked (ids only).
- `STAGE_MANAGER_DESKTOPS_GUESS` (a test run) forces the fallback; `SM_DESKTOPS=guess` sets
  it for every app run of the live suite. `scripts/test-stages.sh reads` checks the reads on
  one Desktop: on and agreeing, then forced off.

- `Space changed` (and `Space changed on another display`) carry `desk` (the window server's
  Desktop on that screen, read at the arrival) and `agree`: `yes` (its Space was bound to it)
  or `new` (bound now) when the listing decided; `no` only for an arrival guessed without the
  listing's word (bound otherwise; the next look corrects it). When it decides, `beside`,
  `held` and `kept_home` stay empty.
- `desktops: the listing flipped after the Space change's notice` (debug, `waited_ms`);
  `Space change settled by the window server's listing with no arrival on the main screen`
  (a notice with no change after `FLIP_WAIT`, or another screen's change);
  `desktops: the window server's listing shows another Space than the model, with no notice
  yet: the change is taken now`; `Space change notified after a look had taken it ...`;
  `desktops: windows listed on screen that the window server has on another Desktop are left
  out of the arrival` (debug); `window left this Space (the window server has it on another
  Desktop)` and `window moved to another Desktop that shows` (`from`, `to`: the window
  server's ids); `the window server has the window on another Desktop than the one shown; the
  window is placed on its arrival` (debug). The margins table has `FLIP_WAIT`; while the
  listing decides `MIXED_WAIT` and `FOREIGN_SETTLE` no longer appear.
- `ctl state`'s `desks.decides` (the listing decides now) and `desks.keyed`: `arrivals` (by
  the listing), `flips_waited` and `flips_capped` (notices that came before the flip and
  were waited for; those never followed by one), `unannounced` and `announced_late` (changes
  taken at a look before their notice; notices that came after one), `left_out` (windows
  left out of an arrival, on another Desktop), `moved` and `left` (windows found on another
  Desktop at a look), `found_elsewhere` (new windows that waited for their Desktop),
  `flip_wait_ms`.
- `desktops: the model and the window server disagree` once when a disagreement begins and
  `... agree again` once when it ends (with `lasted_ms`): `what=shown` (a screen, `id` its
  display) or `what=home` (a hidden window, `id` the window), `space` (the model's),
  `model_desk` (bound to it), `server_desk` (the listing's current one, or the engine's home),
  `why=arrival|check`. Ids only.
- At a load, `desktops: Desktops saved in state.json by uuid that the window server lists
  now`: `saved`, `named` (with a uuid), `listed_now`, `empty_saved`, `empty_listed`.
- `ctl state`'s `desks`: `listing` (per managed screen: `display`, `current`,
  `current_kind`, `desks` with `id` and `uuid`, `others` with their types; `null` when not
  read), `bindings` (`desk`, `space`, `uuid`), and `disagree`: `shown` and `homes` (open
  now), `shown_begun` and `homes_begun` (since the launch), `longest_ms` (an open one
  counted to now), `open` (each with `what`, `id`, `space`, `model`, `server`, `since_ms`).
- `scripts/test-stages.sh spaces --auto` prints them before the first run's quit and at the
  end, with `desks.keyed`, and fails when a disagreement on the Desktop shown is still open
  5 s on; `display-gone` prints them and the log's disagreement lines at each round's end.
  Its last step is the first visit to a Desktop never shown since the launch, by its key,
  with no saved file: one Space change, the Desktop stays, no stage switched on the Desktop
  left. Under both engines it also checks the reads at each of those points (on, never
  rested); `SM_DESKTOPS=guess scripts/test-stages.sh spaces --auto` runs the same with the
  reads forced off, where the first visit's two checks are printed, not failed (1.9.2's
  behaviour under the Corner engine).
- On the bench (`src/platform/controller/bench/desktops.rs`) the desk keeps the listing
  apart from the window list: the "Current Space" flips 5 ms after the notice is sent
  (`flip_leads_notice` for the other order; a load of 3 or 10 handles the notice after the
  flip), the leaving Desktop's windows may stay listed past it (`leaving_lingers_ms`), the
  listing may name no Space shown (`listing_unreadable`), keep naming the Desktop left
  (`listing_stuck`) or not be read at all (`desk_reads` false), a window goes into full screen and
  out (`Bench::fullscreen`), is on every Desktop (`on_every_desktop`) or is moved to another
  (`space_moves`), a second screen has Desktops of its own (`screen_comes_with`). Every
  scenario asserts that the shadow counted no disagreement on the Desktop shown and that the
  reads never rested. Each Desktop scenario runs under both engines, and
  `bench/corner_screens.rs` has a screen going and coming under the Corner engine.
- The fallback is pinned (`bench/guessed.rs`): every scenario that runs the Corner engine is
  also run with the reads off and must read as `bench/guessed.txt` has it, printed from the
  code before the Corner engine read the Desktops. After adding a Corner scenario:
  `BENCH_PRINT=1 cargo test --lib guessed::print_pins -- --nocapture`.

### A report from another Mac

`ctl state` answers with window titles, so ask for four of its fields (and `log summary`)
and read them together. `engine.notices` says whether the window server's notices reach the
app there (`heard`, `silent`, `never_heard`): with none, every look waits for its poll.
`margins` says which time values came near their limit and which waits ended by their cap,
and `noticed` how often a notice beat the poll. `causes` says what the cause ledger still
expects (`open`, `oldest_ms`): many or old expectations mean an app answers late or out of
order, and its focus notices fall to the echo window. `desks` says whether the listing
decides (`decides`) and whether the model and the window server agree on the Desktop shown
and on each hidden window's Desktop (`disagree`: `open`, `longest_ms`, the counts begun).
A value near its limit in the same minute as an open disagreement or an old expectation
names the time value that decided wrong; capped waits with `silent` notices point at the
poll, not the value; a disagreement with nothing near its limit is a fault of order, for the
bench ("The bench", above).

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
  `src/platform/fake.rs` and `src/platform/controller/bench.rs` are test only (the bench).
- `src/ui` — the menu bar item, the strip and its motion, Settings, the welcome window and
  the other windows.
- `scripts/` — signing, bundling, the dev copy, the fixture, the scripted checks, release.
- `art/` — the icon and DMG sources.

## Releasing

See [RELEASING.md](RELEASING.md).
