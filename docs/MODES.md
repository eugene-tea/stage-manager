# Three ways to keep stages

macOS has no public way to hide a window and keep it ready, and Apple's Stage Manager has no
interface for other apps. Each way of running stages works around that differently, and each
trades something. Three are built: the park corner (the default), macOS's own Spaces (in
testing) and following Apple's Stage Manager (opt-in). This page compares them for anyone
who wants to choose or try one. [STRATEGIES.md](STRATEGIES.md) has every call, cost and
measurement behind them.

## At a glance

| | Park corner | Spaces (in testing) | Following Apple's |
|---|---|---|---|
| **Who draws the strip** | Stage Manager | Stage Manager | Apple's Stage Manager |
| **How a hidden stage is kept** | Its windows are moved into a corner of the screen | Its windows are moved to a hidden Space of the window server | Apple's Stage Manager keeps it |
| **Additions** (restore, rules, pins, card order, merge on drop, looks) | All | All | Hotkeys, merge, send, new stage, new apps in the current stage, restore after a restart |
| **Apple's Stage Manager** | Off | Off | On |
| **Setting** | `[windows] engine = "accessibility"`, or `"auto"` (in this version it also uses the corner) | Not a setting yet | `[windows] with_apple = "augment"` |
| **Pros** | Works on every supported Mac with public calls only; every addition works; the motion matches Apple's | Hidden windows leave the screen entirely: no corner, no slivers; about 2 ms per window, with no app asked to move; a slow app doesn't hold up a switch | Apple's own strip, motion and arrangement, exactly; no strip or hidden windows of Stage Manager's |
| **Cons** | Every hide is a real move, so an app that refuses or limits a move breaks it; macOS keeps a thin edge of each window on screen, covered by a matching piece of the desktop; the motion is played by pictures of the windows | Relies on undocumented window-server calls that a macOS update can change; each launch tests them first, and they fall back to the park corner if they fail | Apple's Stage Manager takes changes only from the pointer: Stage Manager posts those clicks and drags, and the pointer jumps for about a frame per click or a second per drag; placement, pins, rules, card order and looks do nothing; under "All at Once" an app's windows can't be split |

## Park corner

The default. To hide a stage, Stage Manager moves its windows into the bottom-right corner of
the screen (a free corner when another screen sits beyond it). macOS always keeps a thin edge
of a window on screen. Those edges are made to ignore clicks and are covered by a matching
piece of the desktop, so the corner looks empty ("The clean corner" in the
[guide](GUIDE.md)). Switches play Apple's motion with pictures of the windows, then the real
windows are put in place behind them.

Every move goes through Accessibility, the public interface for moving other apps' windows,
so this works on every Mac that runs Stage Manager. The move is the app's own, though. An app
that refuses a move, limits a window's position, or answers slowly shows it.

## Spaces (in testing)

Each hidden stage goes into a Space of its own that the window server keeps out of sight, as
macOS does for the windows of another Desktop. Nothing is left in a corner, and no app is
asked to move its windows, so a switch costs about 2 ms per window whatever the app.

The calls are the window server's own and are undocumented. A macOS update can change them,
so each launch runs a test with a window of its own first. Health checks keep watching, and
the park corner takes over whenever they fail. This way is built and runs in the test suite.
It becomes a choice once its measurements are in, and the log already says whether it would
work on a given Mac.

## Following Apple's Stage Manager

With `[windows] with_apple = "augment"` and Apple's Stage Manager on, Apple's draws the strip
and arranges the windows. Stage Manager reads Apple's arrangement and adds the stage hotkeys,
`ctl` commands, new apps in the current stage, and putting the stages back after a restart
("With Apple's Stage Manager on" in the [guide](GUIDE.md)).

Apple's Stage Manager posts no notices and takes changes only from the pointer. So Stage
Manager reads its arrangement on every event, and a few times a second while windows move.
It moves windows by Shift-clicking thumbnails in Apple's strip and dragging title bars onto
it. Nothing is posted while the user clicks, types or holds a key, or while Apple's motion
runs. These routes were measured on macOS 27. Routes that were tried and didn't work (Apple's
own Accessibility actions on the strip, clicks sent to its process) are rechecked by the live
tests, so a macOS update that changes them shows up.

Without this setting (`"stand_down"`, the default), Stage Manager brings every window back
and arranges nothing while Apple's Stage Manager is on, and it picks up again once Apple's is
off.

## Choosing

- **For every addition, and Apple's look:** the park corner, the default.
- **For Apple's own strip, with hotkeys and restore on top:** following Apple's.
- **If windows misbehave on a stage change:** `[windows] engine = "accessibility"` keeps the
  park corner, whatever later versions choose by default.
