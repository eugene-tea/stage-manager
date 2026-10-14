# Stage Manager — user guide

## What it is

Stage Manager is a replacement for the Stage Manager built into macOS. Out of the box it
looks and works like Apple's: your recent groups of windows wait as cards in a strip on the
left, and the windows you are using float in the middle, at the size and place you gave
them. On top of that it adds a few things Apple's lacks, such as merging stages, rules for
new windows and keyboard shortcuts. Each extra stays out of sight until you use it, and
everything can be changed in one config file.

## Install

You need a Mac with Apple silicon (M1 or later) and macOS 14 Sonoma or later. Intel Macs are
not supported.

1. Download `Stage-Manager.dmg` from the
   [latest release](https://github.com/eugene-tea/stage-manager/releases/latest).
2. Open it and drag **Stage Manager** into **Applications**.
3. Open Stage Manager from Applications.

With Homebrew, in one line:

```sh
brew install eugene-tea/tap/stage-manager
```

Or tap once, and from then on use the short name:

```sh
brew tap eugene-tea/tap
brew install stage-manager
```

The app updates itself, so `brew upgrade` leaves it alone. `brew uninstall --zap stage-manager`
also removes its settings and logs.

## First launch

A welcome window walks you through three things. Each row turns into a green check mark as
soon as it is done, and you never need to restart the app.

1. **Turn off Apple's Stage Manager.** The two cannot run together. Click **Turn Off** and
   Stage Manager does it for you. Until Apple's is off, Stage Manager waits and moves
   nothing.
2. **Accessibility — required.** This lets Stage Manager move, hide and bring back your
   windows. Click **Allow…**, then turn Stage Manager on in System Settings → Privacy &
   Security → Accessibility.
3. **Screen Recording — optional.** This shows live pictures of your windows on the cards
   and while stages switch. Without it, cards show plain window shapes with the app's icon.
   It is in System Settings → Privacy & Security → Screen & System Audio Recording.

Stage Manager never needs Input Monitoring. It watches key presses only to notice ⌘-Tab and
⌘-\`, and records nothing you type.

If you close the welcome window early, the menu bar icon's menu keeps reminding you what is
missing, with an **Allow…** item for each.

Stage Manager also adds itself to your login items the first time it runs from
Applications, so it starts when you log in. You can turn that off in Settings.

## How it works

### Stages

A stage is a group of windows you use together. Only one stage is on screen at a time. The
others wait as cards in the strip on the left. When you first start, every app with windows
gets a stage of its own, just as when you turn on Apple's Stage Manager.

Your windows keep their own size and position. Stage Manager never resizes or tiles them,
and when you come back to a stage, every window is exactly where you left it.

### The strip

Each card shows the stage's windows in miniature, with the app's icon in the corner. The
strip sits below your windows: when a window covers it, it slides away. Rest the pointer at
the left edge of the screen to bring it back.

### Switching

You can switch stages the same ways as with Apple's Stage Manager:

- Click a card.
- Press ⌘-Tab to an app whose windows are in another stage.
- Click the app in the Dock.
- Click a card's app icon to bring every window of that app, from all stages.

The stage you leave takes the card's place in the strip.

### New windows

A new window joins its app's stage. If the app is not on screen yet, it gets a stage of its
own. Sheets and dialogs always stay with their window.

### Desktops

Each Desktop (Space) has its own stages and its own strip, as with Apple's. Clicking a card
never changes the Desktop. When ⌘-Tab or the Dock takes you to another Desktop, its stage is
ready when it arrives. Full-screen apps have no strip. Mission Control hides the strip, just
as it hides Apple's.

A window you move to another Desktop (in Mission Control, say) goes where a new window of
its app would open there: with that app's windows if it has a stage on that Desktop, else in
a stage of its own, shown in front. Move windows of several apps at once and each app gets
its own stage; the front one is shown and the rest wait in the strip.

### Drag and drop

- **Drag a card into the middle** to bring its windows into the stage you are in.
- **Drag a window onto the strip** by its title bar to give it a stage of its own.
- **Drag a card up or down** to move it to another place in the strip.
- **Drop a card onto another card** to merge the two stages. To split them again, open the
  merged stage and drag a window onto the strip.

Shift-click a card to bring its windows into the current stage without leaving it. Hold
Shift while you open an app, or Shift-click it in the Dock, to bring it here too.

### ⌘M and the wallpaper

⌘M (or the yellow window button) sends a window to the strip as a card of its own, not to
the Dock.

Clicking the wallpaper shows the desktop and clicking it again brings your stage back. This
follows macOS's own setting in System Settings → Desktop & Dock → "Click wallpaper to reveal
desktop".

### The clean corner

To hide a stage, Stage Manager moves its windows into the bottom-right corner of the
screen. macOS always keeps a thin edge of each window on screen. Stage Manager makes those
edges ignore clicks and covers them with a matching piece of your wallpaper, so the corner
looks empty. If another display sits beyond that corner, a free corner is used instead.

## Settings

Open Settings from the menu bar icon. If you hid the icon, open Stage Manager again from
Applications and Settings appears.

- **General:** start at login, show the menu bar icon, check for updates, install them automatically.
- **Stage Manager:** the same options as Apple's Stage Manager — show recent apps, and show
  an app's windows all at once or one at a time. These are macOS's own settings, so they
  stay in step with System Settings.
- **Extras:** merging stages by dropping cards, Shift to bring apps here, keeping windows
  clear of the strip, keyboard shortcuts, and animation speed.
- **Advanced:** the config file, and help for asking an AI assistant to change it.

Changes apply at once.

## The config file

Every option lives in one file:

```text
~/.config/stage-manager/config.toml
```

Open it from Settings → Advanced → **Open Config File…**. Every option is listed there,
switched off with a `#` and showing its default value, with a short explanation above it.
To change one, remove the `#` and edit the value. To go back to the default, put the `#`
back. Changes apply within a second of saving.

If you make a mistake, Stage Manager keeps your last working settings and shows the error in
its menu and in Settings.

**The easy way: ask an AI assistant.** In Settings → Advanced, click **Copy Instructions for
Agents** and paste them into Claude Code, Codex or a similar tool, followed by what you want.
The assistant reads [the notes for AI agents](AGENTS.md) and makes the change for you.

A few useful examples:

```toml
[hotkeys]
enabled = true          # ⌃⌥1–9 switch stages, ⌃⌥⇧1–9 send the front window there,
                        # ⌃⌥N new stage, ⌃⌥P pin a window to every stage, ⌃⌥S show/hide the strip

[windows]
fit = "move"            # keep windows clear of the strip ("narrow" also narrows wide ones)

[animation]
speed = 1.5             # faster animations; 0 turns them off

[strip]
show_numbers = true     # number each card (handy with the shortcuts)
```

Rules decide where new windows go. Each rule names an app by its bundle ID. To find an app's
bundle ID, run `osascript -e 'id of app "Notes"'` in Terminal.

```toml
[[rule]]
app = "com.apple.Notes"
action = "float"        # Notes stays on screen in every stage

[[rule]]
app = "com.apple.Terminal"
action = "stage:Work"   # Terminal windows go to a stage called Work
placement = "center"    # and open centered on the screen
```

Every option, with its values and default, is in the
[configuration reference](CONFIG.md).

## Updates and privacy

Stage Manager checks GitHub for a new version once a day. When one is out, it asks:
**Install and Relaunch**, **Later** or **Skip This Version**. Install downloads the new
version from GitHub and checks that it is signed by the same developer and notarized by
Apple before anything is replaced. Then every window comes back from the corner, the new
version takes the old one's place and opens, and your Accessibility and Screen Recording
approvals stay. If any step fails, the old version stays (or is put back) and says why.

Turn on **Install updates automatically** in Settings to skip the question: an update then
installs while you are away from the Mac, never in the middle of a drag or a switch. You
can turn the daily check off, and **Check for Updates…** in the menu works either way. If
Stage Manager can't replace itself where it is (it runs from the disk image, or from a
folder you can't write to), it offers the download instead.

Nothing about you or your Mac is sent: the check asks GitHub for the newest version, and
installing one downloads it from GitHub (macOS checks it with Apple, as it does any app you
download). These are the app's only network use.

Nothing about your windows leaves your Mac. Stage Manager keeps a daily log in
`~/Library/Logs/Stage Manager` to help find problems. It names windows by app and number,
never by title: where a title matters, the log shows a short code made from it instead
(`[log] titles = true` writes titles as they are, for debugging). Only your user account can
read the log, logs are deleted after 14 days, and nothing sends them anywhere. Your saved
stages in `~/.config/stage-manager` do keep window titles, to find the windows again after a
restart, and are likewise readable by your account only.

## Uninstall

1. Turn off **Launch at login** in Settings.
2. Choose **Quit Stage Manager** from its menu. Every window it was hiding comes back first.
3. Drag Stage Manager from Applications to the Trash.
4. Optionally, delete `~/.config/stage-manager` (your settings and saved stages) and
   `~/Library/Logs/Stage Manager`, and remove Stage Manager from the Accessibility and
   Screen Recording lists in System Settings → Privacy & Security.

With Homebrew, `brew uninstall --zap --cask stage-manager` does steps 2 to 4 except the
Privacy & Security lists.

You can then turn Apple's Stage Manager back on from Control Center.

## Troubleshooting

**Nothing happens.** Look at the menu bar icon's menu. Stage Manager says there if it needs
Accessibility, or if Apple's Stage Manager is on. While Apple's is on, Stage Manager steps
aside and leaves your windows alone.

**Cards show plain shapes instead of window pictures.** Screen Recording is off or macOS
refused it. Choose **Allow Screen Recording…** from the menu.

**macOS asks whether Stage Manager may "bypass the system private window picker".** Since
macOS 15, macOS shows this for every app that takes pictures of windows directly. Choose
**Allow**. It comes back about once a month, and once whenever the app is moved to a new
place.

**Thin window edges show in the corner.** The wallpaper patch steps aside when it cannot
match your wallpaper exactly: video and aerial wallpapers, shuffled or Photos wallpapers,
the Center and Tile placements, or a desktop icon in the corner. A desktop widget in that
corner may be covered by the patch. You can turn the patch off with `[windows] corner_patch
= false`.

**A window seems lost.** Quit Stage Manager from its menu: every hidden window comes back.
If the app crashed, a small helper process brings the windows back by itself, and the next
launch checks the corner again.

Some limits to know about:

- Only the main display is managed. Windows on other displays are left alone.
- Moving windows are pictures taken a moment earlier, so content that changed meanwhile can
  jump when the motion ends. Card pictures refresh every 1.5 seconds rather than live.
- With ⌘M, macOS may show a short animation toward the corner.
- After ⌘-Tab to another Desktop, its stage is set up once the Desktop has slid in. (A Dock
  click sets it up before.)
- An empty Desktop is forgotten when you leave it. A Desktop whose windows are all hidden or
  minimized looks new until one of them shows again.
- After you restart your Mac or log in again, and when an app is relaunched, windows are
  matched to their stages by title. Two windows of one app with the same title may swap
  stages.
- Keyboard shortcuts use US key positions.
- With "Click wallpaper to reveal desktop: Only in Stage Manager", a click on a desktop icon
  also counts as a wallpaper click.

**Reporting a problem.** Set `[log] level = "debug"` in the config file, so the log keeps
every decision and the menu shows **Mark a Problem**. Choose it right after something goes
wrong: it writes a note with the app's state into the day's log. Then open an issue on
[GitHub](https://github.com/eugene-tea/stage-manager/issues) and describe what happened.
The log shows window titles only as short codes, but it does name your apps; read it before
attaching it.

## More

- [Configuration reference](CONFIG.md): every option.
- [Design notes](UX-SPEC.md): how Stage Manager matches Apple's and where it differs.
- [Development](DEVELOPMENT.md): building, testing and releasing.
