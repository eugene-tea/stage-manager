# Three ways to keep stages

macOS has no public way to hide a window and keep it ready, and Apple's Stage Manager has no
interface for other apps. Each way of running stages works around that differently, and each
trades something. Three are built: Accessibility (the default), macOS's own Spaces, and
integrating with Apple's Stage Manager. This page compares them for anyone who wants to choose
or try one. [STRATEGIES.md](STRATEGIES.md) has every call, cost and measurement behind
them.

## Comparison

✓ yes · — no, or not measured · ◐ in part (see the notes below the table)

| | Accessibility | Spaces | Integrated with Apple's |
|---|:---:|:---:|:---:|
| **Status** | Default | Opt-in | Opt-in |
| **Setting** (`[windows]`) | `engine = "accessibility"` | `engine = "spaces"` | `apple_stage_manager_on = "integrate"` |
| **Apple's Stage Manager** | Off | Off | On |
| **Strip drawn by** | Stage Manager | Stage Manager | Apple |
| ***Stages*** | | | |
| Recent stages as cards, Apple's motion | ✓ | ✓ | ✓ |
| Each screen and Desktop with its own stages | ✓ | ✓ | ✓ |
| Stages back after a restart | ✓ | ✓ | ✓ |
| Switch stages from the keyboard (⌃⌥1–9) | ✓ | ✓ | ✓ |
| Send a window to a stage, or to a new one | ✓ | ✓ | ◐ ¹ |
| Combine two stages | ✓ | ✓ | ◐ ² |
| A new app opened into the current stage (Shift, rules) | ✓ | ✓ | ◐ ³ |
| ***Additions on the strip*** | | | |
| Card order: custom, in place, or the Dock's | ✓ | ✓ | — |
| Named stages | ✓ | ✓ | — |
| Windows kept clear of the strip | ✓ | ✓ | — |
| Rules for placing new windows, pinned windows | ✓ | ✓ | — |
| Strip width, size, tilt and edge tab | ✓ | ✓ | — |
| ***How it works*** | | | |
| Hidden windows leave the screen entirely | — ⁴ | ✓ | ✓ |
| Works with apps that refuse or limit a window move | — | ✓ | ✓ |
| Pointer never moved by the app | ✓ | ✓ | — ⁵ |
| Documented macOS interfaces only | ✓ | — ⁶ | ◐ ⁷ |
| ***Cost at rest*** ⁸ | | | |
| **Total, with the system's share** | **about 4.9 %** | **about 1.2 %** | — |
| Stage Manager itself (what Activity Monitor shows) | 1.07 % | 0.35 % | about 1 % ⁹ |
| macOS's screen-capture service | 1.23 % | 0 % | — |
| Window server, above its own load | +2.58 % | +0.86 % | — |
| Time awake per minute | 500 ms | 125 ms | — |
| ***Speed*** | | | |
| A card click until the windows land | about 860 ms | about 850 ms | about 700 ms (Apple's motion) |

1. Apple's Stage Manager takes changes only from the pointer, so the window is moved with a
   posted drag onto its strip. Under "Show windows from an application: All at Once" an app's
   windows stay in one stage, so a window whose app has another window in its stage can't be
   sent.
2. With the hotkeys or `ctl merge N`. Dropping one card on another works only in Stage
   Manager's own strip.
3. Apple's Stage Manager gives every new app a stage of its own, so the app is hidden as it
   opens and its window is then Shift-clicked in from Apple's strip, about 1.7 seconds after
   the launch. When the window shows before the hide takes, Apple's has already switched: the
   stage that was in front comes back with the app's window on top, a visible round trip of
   about two seconds. Finder, and apps that bring themselves forward again once shown, always
   take that round trip. While Apple's strip is covered the window waits a few seconds, then
   stays in a stage of its own. A running app with no window that opens one as it is
   activated is handled the same way.
4. macOS keeps a thin edge of every window on screen. Those edges sit in a corner, ignore
   clicks, and are covered by a matching piece of the desktop.
5. Each move is a posted Shift-click or drag in Apple's strip: the pointer leaves for about a
   frame per click, about a second per drag, then comes back. Nothing is posted while the user
   clicks, types or holds a key.
6. The window server's own Space calls are undocumented. Each launch tests them first, health
   checks keep watching, and Accessibility takes over whenever they fail.
7. Accessibility and posted events are documented. Where to click depends on how Apple's strip
   is drawn, as measured on macOS 27, and a macOS update can change that. The live tests
   recheck it.
8. Percent of one CPU core at rest, test windows only, the median of three runs on an Apple
   silicon Mac with macOS 27 (11-14; method and every figure in [UX-SPEC.md](UX-SPEC.md),
   "Window engines measured"). Most of what Accessibility costs is in macOS's own processes
   (the screen-capture service, for the pictures that play the motion, and the window server),
   so Activity Monitor's line for Stage Manager shows only about a fifth of it.
9. An average over 13 hours of everyday use with Apple's Stage Manager on (11-17 to 11-18),
   before the work this mode doesn't use was switched off. It's an average over work and rest,
   so it isn't directly comparable with the other two columns.

## Settings

Both are in the `[windows]` section of the config file.

- `engine`: how Stage Manager hides windows while Apple's Stage Manager is off.
  `"accessibility"` (the default) or `"spaces"`. It is read at launch, so Stage Manager has to
  be quit and opened again after a change. With `"spaces"`, the log says which one runs and
  why.
- `apple_stage_manager_on`: what happens while Apple's Stage Manager is on. `"pause"` (the
  default) brings every window back and arranges nothing until Apple's is off; `"integrate"`
  follows Apple's stages.

The two combine, for example Spaces while Apple's Stage Manager is off and integrated while it
is on. More on each: "The clean corner", "With Apple's Stage Manager on" and "Troubleshooting"
in the [guide](GUIDE.md); every call, route and its cost in [STRATEGIES.md](STRATEGIES.md).

## Choosing

- **For every addition, with Apple's look:** Accessibility, the default.
- **For the lowest cost at rest and no windows left in a corner:** Spaces.
- **For Apple's own strip, with hotkeys and restore on top:** integrated with Apple's.
