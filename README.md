# Stage Manager

A floating-window stage manager for macOS: Apple's Stage Manager strip and floating windows,
plus rules, hotkeys, named stages, pinning, placement and persistence. See `PLAN.md`.

Status: milestones 1 (skeleton) and 2 (observation). The app shows a menu bar item, checks
permissions and Apple's Stage Manager settings, and logs every manageable window live. It
does not move windows yet.

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

`make run` passes `RUST_LOG` (default `info,stage_manager=debug`), `STAGE_MANAGER_NO_PROMPT`
and `STAGE_MANAGER_ONLY` through to the app when they are set.

Why LaunchServices: a binary started straight from a terminal has its TCC grants checked
against the terminal. Launched with `open`, macOS checks the bundle's own signature.

## Permissions

- **Accessibility** — observing and moving windows. Without it the app runs, says so in the
  menu, and starts observing within 2 s of the grant (no relaunch).
- **Screen Recording** — strip thumbnails (later milestone).

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

## Environment

| Variable | Effect |
|----------|--------|
| `RUST_LOG` | log filter (default `info,stage_manager=debug`) |
| `STAGE_MANAGER_NO_PROMPT` | never show permission dialogs at launch |
| `STAGE_MANAGER_ONLY` | comma-separated bundle IDs: observe (later: manage) only these apps |

## ctl socket

The app listens on `$TMPDIR/stage-manager.sock` (one command per line, one line of JSON per
reply). The same binary is the client:

```sh
B="target/Stage Manager.app/Contents/MacOS/stage-manager"
"$B" ctl ping    # {"ok":true,"pong":true,"pid":…,"version":…}
"$B" ctl dump    # observed apps and windows; says so when Accessibility is missing
"$B" ctl quit    # quits cleanly
```

Exit status: 0 on `"ok":true`, 1 on an error reply, 2 when the app is not reachable.

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
  thread that owns every AX call), `observe` (observed apps and windows), `windows`
  (CGWindowList), `apps` (NSWorkspace), `permissions`, `native_settings`, `ctl`, `logging`,
  `paths`.
- `src/ui` — status item and menu (strip later).
- `scripts/` — signing identity, bundling, signing, run, fixture.
