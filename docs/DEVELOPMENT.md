# Stage Manager — development

How to build, sign, run and test Stage Manager. The architecture and its safety layers are
in [PLAN.md](../PLAN.md); how the app should behave is in [UX-SPEC.md](UX-SPEC.md);
publishing a release is in [RELEASING.md](RELEASING.md).

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

`scripts/test-stages.sh` runs the scripted checks against the real app, driving it through
its ctl socket and reading the window server's view of the windows:

```sh
scripts/test-stages.sh                    # every phase
scripts/test-stages.sh m3 safety          # some phases: m3 safety m4 m6 restore strip native
                                          # visible parity motion slots expose capture raise park
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
| `STAGE_MANAGER_NO_LOGIN_ITEM` | never register or unregister the login item |
| `STAGE_MANAGER_NO_CAPTURE` | window captures off, whatever the config says |
| `STAGE_MANAGER_TEST` | enables the test-only `ctl debug-*` commands |
| `STAGE_MANAGER_UPDATE_VERSION` | builds with `--features update-test` only: the update check offers this version instead of asking GitHub |
| `STAGE_MANAGER_UPDATE_DMG` | builds with `--features update-test` only: an update installs from this local disk image instead of downloading one (a locally signed app then trusts its own designated requirement) |
| `SM_TEST_TOGGLE_APPLE_SM` | `1` lets `scripts/test-apple-stage-manager.sh` toggle Apple's Stage Manager (it restores the setting on exit) |

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
Control shortcuts, and full-screen toggles for the scripted checks; each refuses to act
outside the app's own and `STAGE_MANAGER_ONLY` windows. See `src/platform/ctl.rs`.

## Other tools

- `stage-manager preview settings|welcome|about|update|… [--dark|--light] [--pane PANE] [--png FILE]`
  opens one of the app's windows alone, or draws it to a PNG, without managing anything.
- `stage-manager strip-demo` runs the strip alone, fed from the live window list, and never
  moves a window. Its variables (`STAGE_MANAGER_DEMO_*`, and an offscreen render with
  `--features debug-render`) are described in `src/ui/strip_demo.rs`.
- `stage-manager probe-windows <bundle-id>…`, `probe-screen` and `probe-patch` print the
  window server's view of apps, of the screen, and what the corner patch would draw.
- `scripts/probe_park.py` measures where macOS leaves a parked window.

## Logs

The app, its guardian process and one-shot commands write one file per local day to
`~/Library/Logs/Stage Manager/YYYY-MM-DD.log`. `[log] level` is `info` by default (the main
events); `debug` records every decision the app makes and shows **Mark a Problem** in the
menu. Day files are capped in size and kept 14 days.

- `stage-manager log summary [DATE|yesterday]` prints a day's counts, runs, marks, and every
  warning and error, deduplicated: short enough to paste.
- `stage-manager mark ["note"]` (or **Mark a Problem** in the menu) writes a marked block
  with the app's state and the last 30 seconds of detailed lines.

Window titles go into the log, and into a mark's state dumps, as a short hash
(`#1a2b3c4d`, the same for the same title in every process on this Mac: it is keyed with a
random key made once and kept owner-only in the config folder, `title-key`, so a guessed
title cannot be confirmed from a log alone) unless `[log] titles = true`;
lines name windows by id, pid and bundle id. `ctl dump` and `ctl state` still answer with
titles. Logs still name your apps and your stages: never commit one.

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
