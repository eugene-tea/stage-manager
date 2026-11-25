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
| Mission Control shows every stage's windows | ✓ | ◐ ⁵ | ✓ |
| Pointer never moved by the app | ✓ | ✓ | — ⁶ |
| Windows moved and hidden through documented macOS interfaces only | ✓ | — ⁷ | ◐ ⁸ |
| Window pictures: macOS's faster undocumented capture first, the documented one wherever it fails | ✓ | ✓ | — |
| Desktops: read from macOS's own undocumented list first, told apart by their windows wherever it fails | ✓ | ✓ | — |
| ***Cost at rest*** (test windows, nobody at the Mac) ⁹ | | | |
| **Total, with the system's share** | **about 5.7 %** | **about 0.5 %** | — |
| Stage Manager itself (what Activity Monitor shows) | 0.83 % | 0.50 % | — |
| macOS's screen-capture service | 0.25 % | 0 % | — |
| Window server, above its own load | +4.6 % | about 0 | — |
| Time awake per minute | 392 ms | 216 ms | — |
| ***Cost in use*** ¹⁰ | | | |
| Stage Manager itself, while working | 1–2 % | — | about 1 % |
| ***Speed*** | | | |
| A card click until the windows land | about 860 ms | about 850 ms | about 700 ms (Apple's motion) |

1. Apple's Stage Manager takes changes only from the pointer, so the window is dragged onto
   its strip. Under "Show windows from an application: All at Once", a window can't leave a
   stage that holds another window of its app.
2. With the hotkeys or `ctl merge N`. Dropping one card on another works only in Stage
   Manager's own strip.
3. Apple's Stage Manager gives every new app its own stage, so the app is hidden as it opens
   and then Shift-clicked into the current stage, about 1.7 seconds after launch. When its
   window shows first, the switch and the way back are visible (about two seconds); Finder and
   apps that bring themselves forward always take that round trip.
4. macOS keeps a thin edge of every window on screen. Those edges sit in a corner, ignore
   clicks, and are covered by a matching piece of the desktop.
5. Hidden stages' windows join Mission Control and App Exposé once they open: at once from the
   keyboard shortcut, up to two seconds later from a gesture, a hot corner or the Dock
   (`mission_control_stages`, below).
6. Each move is a posted Shift-click or drag on Apple's strip: the pointer leaves for a frame
   per click, about a second per drag. Nothing is posted while the user clicks, types or holds
   a key.
7. The window server's own Space calls are undocumented. Each launch tests them first, health
   checks keep watching, and Accessibility takes over whenever they fail.
8. Accessibility and posted events are documented. Where to click depends on how Apple's strip
   is drawn, as measured on macOS 27, and a macOS update can change that. The live tests
   recheck it.
9. Percent of one CPU core at rest, test windows only, the median of three runs (Apple
   silicon, macOS 27, a 5K display, 11-25; details in [UX-SPEC.md](UX-SPEC.md), "Window engines
   measured"). Most of Accessibility's cost is in macOS's window server, so Activity Monitor's
   line for Stage Manager shows only a small part of it. Until 1.9.2 Accessibility also took
   pictures of the front windows every two seconds, and the total read about 16 % on the same
   Mac.
10. Activity Monitor's figure for Stage Manager in everyday use (11-18), without the system's
   share. Accessibility: about 1.9 % over a minute of work (0.9 to 3.6 %), rising with every
   click and switch. Integrated with Apple's: the average over 13 hours, overnight included.

## Settings

All three are in the `[windows]` section of the config file; `engine` and
`apple_stage_manager_on` are also in Settings → Advanced → Mode.

- `engine`: how Stage Manager hides windows while Apple's Stage Manager is off.
  `"accessibility"` (the default) or `"spaces"`. It is read at launch, so Stage Manager has to
  be quit and opened again after a change. With `"spaces"`, the log says which one runs and
  why.
- `mission_control_stages`: with `engine = "spaces"`, whether Mission Control and App Exposé
  show the hidden stages' windows. `"shown"` (the default) puts them on the Desktop once
  Mission Control shows; `"key"` already when its keyboard shortcut is pressed; `"off"` leaves
  them hidden. Read when the config file changes.
- `mission_control_key_keeps_stage`: with `engine = "spaces"`, closing Mission Control with its
  key or Escape keeps the stage in front. Off (the default) is Apple's: the app whose window is
  under the pointer comes in front with its stage.
- `apple_stage_manager_on`: what happens while Apple's Stage Manager is on. `"pause"` (the
  default) brings every window back and arranges nothing until Apple's is off; `"integrate"`
  follows Apple's stages.

`engine` and `apple_stage_manager_on` combine, for example Spaces while Apple's Stage Manager is off and integrated while it
is on. More on each: "The clean corner", "With Apple's Stage Manager on" and "Troubleshooting"
in the [guide](GUIDE.md); every call, route and its cost in [STRATEGIES.md](STRATEGIES.md).

## Choosing

- **For every addition, with Apple's look:** Accessibility, the default.
- **For the lowest cost at rest and no windows left in a corner:** Spaces.
- **For Apple's own strip, with hotkeys and restore on top:** integrated with Apple's.
