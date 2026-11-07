# Contributing

Pull requests are welcome; open them against `dev`, the default branch (`master` holds
releases only, see [AGENTS.md](../AGENTS.md)). A few things keep them easy to take.

**Apple's way stays the default.** With an empty config file, Stage Manager looks and
behaves like Apple's. A new behaviour is an option, off by default (or, if it changes
nothing Apple's does, on). [UX-SPEC](../docs/UX-SPEC.md) records how Apple's works and where
we differ; [FEATURE-REQUESTS](../docs/FEATURE-REQUESTS.md) lists what users have asked for.

**Building and testing.** [DEVELOPMENT](../docs/DEVELOPMENT.md) covers the build, the
fixture app and the live tests; [ARCHITECTURE](../docs/ARCHITECTURE.md) how the code is laid
out and the rules it keeps. Before a pull request:

- `make check` passes (unit tests and clippy; CI runs the same).
- Tests use the fixture app's windows, never the windows of apps you are using.
- A line in [CHANGELOG.md](../CHANGELOG.md) under Unreleased, in plain words a user would
  understand.
- A new option goes in [CONFIG.md](../docs/CONFIG.md) with its default.

For anything big, open an issue first so we can agree on the shape before you build it.
