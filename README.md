<p align="center">
  <picture>
    <source srcset="docs/assets/app-icon-dark.png" media="(prefers-color-scheme: dark)">
    <img src="docs/assets/app-icon.png" alt="" width="128" height="128">
  </picture>
</p>

<h1 align="center">Stage Manager</h1>

<p align="center"><a href="https://github.com/eugene-tea/stage-manager/releases/latest"><b>Download</b></a>
· signed and notarized · Apple silicon, macOS 14 Sonoma or later<br>
or <code>brew install eugene-tea/tap/stage-manager</code></p>

## Like Apple's. Then more.

Out of the box it looks and works like Apple's Stage Manager, never against it. Improvements
that fit Apple's way are on; the rest are a switch away in Settings.

- **Survives a restart.** Your stages, their order and the stage in front come back after a
  reboot.
- **Merge with a drop.** Drop one card on another to make one stage. Drag a window to the
  strip to split it off.
- **A strip that stays put.** Each stage keeps its place, or follows your Dock's order.
  Windows can keep clear of the strip.
- **New apps join you.** Open an app straight into the stage you are in. Hold Shift to give
  it a stage of its own.
- **Shortcuts if you want them.** ⌃⌥1–9 to switch stages or send a window. Nothing needs one.
- **Never loses a window.** Quit, crash or update: every hidden window comes back.

## More in the config file

Everything else lives in one plain file. Ask an AI assistant to change it; Settings →
Advanced shows how.

- **Rules for new windows:** by app and title, a named stage, every stage, centred or cascaded.
- **Named stages:** names and numbers on the cards.
- **The strip:** width, size, tilt, an edge tab, above or below windows.
- **Motion:** every animation's timing, measured from Apple's.
- **More shortcuts:** new stage, pin a window, show or hide the strip.

## First launch

Turn off Apple's Stage Manager (the app offers to do it), then allow **Accessibility**,
which is required. **Screen Recording** is optional and puts live window pictures on the cards
and keeps the corner clean on any wallpaper.

Nothing about your windows leaves your Mac. The only network use is a daily update check
on GitHub, and downloading an update from there when it installs one.

## Learn more

- [Info page](https://eugene-tea.github.io/stage-manager/)
- [User guide](docs/GUIDE.md): install, use, settings, troubleshooting
- [Configuration reference](docs/CONFIG.md): every option
- [Development](docs/DEVELOPMENT.md): build, test, release
- [Notes for AI agents](docs/AGENTS.md): for an assistant changing your settings
- [Changelog](CHANGELOG.md)

Stage Manager is an independent project, not affiliated with Apple. MIT licensed
([LICENSE](LICENSE)).
