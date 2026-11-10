# Working on Stage Manager

Notes for anyone changing this code, people and coding agents alike. (Helping a *user* change
their settings is a different job: [docs/AGENTS.md](docs/AGENTS.md).)

## Where things are

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how the app is built and the rules the code
  keeps (threads, safety layers, no private APIs beyond `_AXUIElementGetWindow`).
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): build, the fixture app, the live tests, the
  ctl socket, logs, the source layout.
- [docs/UX-SPEC.md](docs/UX-SPEC.md): how Apple's Stage Manager behaves and where we differ.
- [docs/CONFIG.md](docs/CONFIG.md): every option (generated from `src/config/schema.rs`).
- [PLAN.md](PLAN.md): what is still to do. [CHANGELOG.md](CHANGELOG.md): what shipped.
- [docs/RELEASING.md](docs/RELEASING.md): signing, notarizing and publishing a release.

## Branches

- **`dev`** is the default branch and holds all the work. Pull requests go to `dev`. CI
  (`make check`) runs on every push to it and on every pull request.
- **`master`** holds releases only: at each release it moves forward to the released `dev`
  commit, which gets an annotated tag `vX.Y.Z` and a GitHub release with the DMG. The info
  page is served from `master`'s `docs/`, so a change to the page also moves `master`.
  Nothing else does.
- Versions: a change users can notice bumps the minor (1.7.0 → 1.8.0); a patch is for fixes
  that change no behaviour.

## Rules

- **Apple's way stays the default.** With an empty config the app looks and behaves like
  Apple's Stage Manager; every extra is an option, off unless it changes nothing Apple's does.
- **`make check` is clean** before every commit: unit tests and clippy.
- **Live tests touch fixture windows only** (`STAGE_MANAGER_ONLY`, a separate
  `STAGE_MANAGER_HOME`; DEVELOPMENT, "Testing"). Never move, hide or close the windows of
  apps someone is using, and never leave a window parked.
- **Nothing private in the repo:** no logs, screenshots or window titles; the log itself
  writes titles only as short codes.
- **Accessibility:** anything new on screen gets a VoiceOver label and follows Reduce Motion
  ([.github/ACCESSIBILITY.md](.github/ACCESSIBILITY.md); UX-SPEC, "Accessibility").
- **CHANGELOG** gets a line under Unreleased for every change a user would notice, in plain
  words.
