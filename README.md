<p align="center">
  <picture>
    <source srcset="docs/assets/app-icon-dark.png" media="(prefers-color-scheme: dark)">
    <img src="docs/assets/app-icon.png" alt="" width="128" height="128">
  </picture>
</p>

<h1 align="center">Stage Manager</h1>

<p align="center">
  <a href="https://github.com/eugene-tea/stage-manager/releases/latest"><img src="https://img.shields.io/github/v/release/eugene-tea/stage-manager?logo=github&label=release" alt="Latest release"></a>
  <a href="https://github.com/eugene-tea/homebrew-tap"><img src="https://img.shields.io/badge/dynamic/regex?url=https%3A%2F%2Fraw.githubusercontent.com%2Feugene-tea%2Fhomebrew-tap%2Fmain%2FCasks%2Fstage-manager.rb&search=version%20%22(%5B%5E%22%5D%2B)%22&replace=v%241&label=homebrew&logo=homebrew&color=orange" alt="Homebrew cask version"></a>
  <img src="https://img.shields.io/badge/macOS-14%2B-lightgrey?logo=apple" alt="macOS 14 or later">
</p>

<p align="center"><a href="https://github.com/eugene-tea/stage-manager/releases/latest"><b>Download</b></a>
· free and open source · signed and notarized · Apple silicon, macOS 14 Sonoma or later<br>
or <code>brew install eugene-tea/tap/stage-manager</code></p>

<p align="center"><a href="https://eugene-tea.github.io/stage-manager/"><img src="docs/assets/screenshot.jpg" alt="Stage Manager: recent stages as cards on the left, the windows in use in the middle"></a></p>

## What it is

Stage Manager works and looks like the Stage Manager built into macOS. Recent stages wait as
cards on the left, and the windows in use stay in the middle, where they were put. Anyone who
knows Apple's already knows this one.

It also adds what people have asked Apple for since 2022. Apple's clicks, drags and keys work
the same with every addition; left unused, the app behaves exactly like Apple's.

- Stages come back after a restart, in the same order.
- An app opened with Shift held goes into the current stage.
- A card dropped on another combines the two stages.
- Stages can be switched, and windows sent to them, from the keyboard (⌃⌥1–9, turned on in
  Settings).
- Cards can be kept in a custom order or in the Dock's order (Settings).
- Windows can be kept clear of the strip, so the cards stay in view (Settings).

## More in the config file

Everything else is in one plain text file. An AI assistant can edit it, and Settings →
Advanced shows how.

- **Rules for new windows:** by app and title, a named stage, every stage, centred or cascaded.
- **Named stages:** names and numbers on the cards.
- **The strip:** width, size, tilt, an edge tab, above or below windows.
- **Motion:** every animation's timing, measured from Apple's.
- **More shortcuts:** new stage, pin a window, show or hide the strip.

## First launch

Apple's Stage Manager has to be turned off first (the app offers to do it). **Accessibility**
access is required. **Screen Recording** is optional and puts live pictures of the windows on
the cards.

Everything about the windows stays on the Mac. The app goes online only for a daily update
check on GitHub, which can be turned off.

## Learn more

- [Info page](https://eugene-tea.github.io/stage-manager/)
- [User guide](docs/GUIDE.md): install, use, settings, troubleshooting
- [Configuration reference](docs/CONFIG.md): every option
- [Accessibility](.github/ACCESSIBILITY.md): VoiceOver, the keyboard, Reduce Motion, and what's missing
- [Development](docs/DEVELOPMENT.md): build, test, release
- [Notes for AI agents](docs/AGENTS.md): for an assistant changing the settings
- [Changelog](CHANGELOG.md)

Stage Manager is an independent project with no affiliation to Apple. MIT licensed
([LICENSE](LICENSE)).
