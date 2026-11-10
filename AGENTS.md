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

- **`dev`** holds all the work, a commit at a time. Pull requests go to `dev`; one opened
  against `master` is moved there (`.github/workflows/pr-to-dev.yml`). CI (`make check`)
  runs on every push to it and on every pull request.
- **`master`** is the default branch, the one GitHub shows and `git clone` checks out: one
  commit per release, with dev's files at that release and its CHANGELOG section as the
  message, tagged `vX.Y.Z` (annotated) with a GitHub release and the DMG. It shares no
  history with `dev`; `scripts/master-commit.sh` writes its commits. GitHub reads the info
  page (`docs/`) and the contributing notes, templates and workflows (`.github/`) from
  `master`, so a change to those also adds a commit to `master` (`master-commit.sh repo`).
  Nothing else does. Only the owner can update `master` (a ruleset).
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
