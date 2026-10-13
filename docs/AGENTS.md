# Stage Manager — notes for AI agents

You are configuring **Stage Manager**, a macOS app (by eugene-tea) that replaces Apple's
Stage Manager: a strip of stage cards on the left and floating windows, plus extras. The
user asked you to change how it behaves. Everything is one TOML file; the app reloads it by
itself.

## The binary

```sh
B="/Applications/Stage Manager.app/Contents/MacOS/stage-manager"
# or ~/Applications/Stage Manager.app/…; the user's prompt names the one that runs.
```

## The config file

- `~/.config/stage-manager/config.toml` (or `$STAGE_MANAGER_HOME/config.toml` when that
  variable is set).
- Written on first run with **every key commented out** (`# key = default`) under its
  `[section]`, each with a one-line explanation above it. The defaults live in the app, so a
  commented line means "the built-in default".
- **To change a key, uncomment its line and set the value.** To go back to the default,
  comment it out again (or delete it). Keep the user's other lines and comments as they are.
- New-window rules are `[[rule]]` tables (`app` = bundle ID or `"*"`, optional `title`
  regex, `action`, `placement`); the file ends with commented examples.
- Every key with its type, range and default: `"$B" config --print-defaults` (the
  commented file), `"$B" config --explain section.key` (one key), or the table in
  [GUIDE.md](GUIDE.md#configuration) (generated from the same source).

## How changes apply

- Saved changes apply **within a second** (the app polls the file's modification time).
  Do not restart the app. `"$B" ctl reload` forces a reload.
- A value of the wrong type keeps the **last good config** in force; the error shows in the
  menu, in Settings and in the log. Unknown keys and out-of-range numbers only warn
  (numbers are clamped).
- The Settings window writes the same file (it uncomments a changed key in place and
  comments it out again at the default); your edits show there at once.

## Check your edit

```sh
"$B" config check            # the real file; or: "$B" config check path/to/file.toml
```

Prints `…: ok` and any warnings (exit 0), or the error with its line number (exit 1). Fix
every error and warning you caused.

## Read the app's state

```sh
"$B" ctl ping     # running? version, and whether Apple's Stage Manager is on
"$B" ctl state    # stages, windows, frames, the strip, the config error
"$B" ctl dump     # observed apps and windows, Spaces
```

Exit status 2 means the app is not running. Window titles may be private: do not copy them
anywhere outside this conversation.

## Read the logs

- One file per local day: `~/Library/Logs/Stage Manager/YYYY-MM-DD.log`.
- `"$B" log summary` (or `log summary yesterday`) gives the day's counts, warnings and
  errors, short enough to read whole; `"$B" log path` prints today's file.
- Config problems appear as `config: …` lines.

## Settings that are not in the file

- **macOS's own Stage Manager settings** ("Show recent apps", "Show windows from an
  application", "Click wallpaper to reveal desktop") live in System Settings → Desktop &
  Dock and are read live; the config file never repeats them. Ask the user to change them
  there (or in Stage Manager's Settings window).
- **Approvals** (Accessibility, Screen Recording) are the user's to give in System Settings
  → Privacy & Security.

## Never

- Never edit `state.json` or `app-state.json` beside the config: they are the app's own
  memory (stages, first-launch facts) and are rewritten while it runs.
- Never move, resize, hide or close the user's windows, or script clicks, to "test" a
  change; never call the `ctl debug-*` commands.
- Never write `com.apple.WindowManager` (`defaults write …`) or turn Apple's Stage Manager
  on or off; never add or remove login items yourself (`[app] launch_at_login` does it).
- Never quit or relaunch the app unless the user asks; `ctl quit` brings every hidden
  window back first if they do.
