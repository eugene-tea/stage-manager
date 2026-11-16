# Strategies

How Stage Manager gets stages on screen, every way built or tried, in one table per kind with
the same columns: what it does, the calls, what it costs (time, pointer, CPU, what is seen),
what breaks it, where the code is, how it was measured. [ARCHITECTURE.md](ARCHITECTURE.md),
"Window engines", keeps the code map; this file keeps the catalog. Every phase of the Apple
mode adds its rows (A1 and A2: the mirror and the focus route; A3: the posted routes and their
gate; A4: where a newly opened app goes).

## Strategies

| Strategy | What it does | Calls | Cost | What breaks it | Code | Measured |
|---|---|---|---|---|---|---|
| Corner | Inactive stages' windows parked in a corner of their screen, the motion played by pictures | Accessibility set position and frame; ScreenCaptureKit pictures | A park per window (an app's own move); pictures every 2 s while the strip shows; a sliver per window in the corner, covered by the corner patch | An app that refuses or clamps the move; another screen beyond the corner | `engine/corner`, `platform/corner` | The live suite (`scripts/test_stages.py`) |
| Spaces with pictures | Each inactive stage in a hidden window-server Space of its own; the motion played by pictures | SkyLight Space calls, read back; Accessibility set frame on a show | About 2 ms a window, no app asked; pictures taken at the switch | A window server that refuses the calls (the launch probe, the health checks, then the fall back to Corner) | `engine/spaces`, `platform/spaces` | The launch probe; the live suite with `SM_ENGINE=spaces` |
| Spaces live landing | As above, the incoming windows flying the last stretch themselves through their Space's transform | The Space transform per step | As above, no picture over the window at its end | A transform call missing (then pictures) | `platform/spaces/landing.rs` | `STAGE_MANAGER_LANDING=live`, the live suite |
| Apple-augmented | Apple's Stage Manager arranges; Stage Manager reads its arrangement and follows it with its own model, and acts through routes Apple's answers to | The window list (each read); WindowManager's Accessibility (read only, every 2 s); the routes below | A list read per event, every 50 ms while anything moves, every 2 s at rest; nothing of ours hidden, no strip or pictures of ours | Apple's changing what WindowManager lists (its layer-0 windows over the thumbnails, `sm.strip`'s items); see each route | `engine/apple`, `platform/apple`, `platform/conductor.rs`; `[windows] with_apple = "augment"` | Two probes, 11-16, macOS 27.2; `scripts/test_apple.py` |

## Routes inside Apple mode

| Route | What it does | Calls | Cost | What breaks it | Code | Measured |
|---|---|---|---|---|---|---|
| Focus | Brings a stage in front: its key window raised, made main, its app frontmost | Accessibility `AXRaise`, `AXMain`, the app's `AXFrontmost` | 5-7 ms, no pointer; Apple's own motion follows (about 700 ms); works with the strip hidden and for a stage the strip has no room for | A window not known (never seen in a stage); an app that does not answer | `platform/apple/routes/focus.rs`, `engine/apple/plan.rs` | Probe 11-16; `scripts/test_apple.py switch` |
| Shift-click | Brings windows into the stage in front (`ctl merge N`: every window of stage N, front to back; `ctl send N`: a switch to N, the click, a switch back) | A real Shift key-down (none when the user holds Shift), about 12 ms, then a click per window 50 ms apart, each with no hold at a visible part of its thumbnail (the part no window listed in front of it and no app icon covers, the ones of the same press not clicked yet counted, read from the window list at the settle), the pointer warped back once the posted events have moved it (else the warp lands first and the click leaves the pointer at its point: live 11-16), about 12 ms after the last, Shift key-up; on a thread of its own; the topmost window at each point checked to be WindowManager's just before its click | The pointer away 2-15 ms a click (about a frame); one click per window (a Shift-click adds only the window clicked), every window that shows a point of its own in one press; one Apple motion, about 700 ms, for the lot (a stage of three took 3.9 s with a settle after each click, live 11-16) | A click mid-motion (ignored, or switches back: every click waits for the settle); the strip covered (waited for, at most 4 s, then given up and logged); a thumbnail with no part of its own showing; the gate (below) | `platform/apple/routes/shift_click.rs`, `platform/apple/post.rs`, `engine/apple/points.rs`, `engine/apple/job.rs` | Probes 11-16 (second; fourth: a stage of two in one press); `scripts/test_apple.py merge`, `pointer` |
| Drag | Takes a window out to a stage of its own (`ctl new`, the new-stage hotkey) | The window brought in front by focus if it is not, and raised the same way when its title bar is covered all along (live 11-16: the fixture windows open at one frame); a posted press on its title bar (14 points below its top, at its middle, or the free point along it nearest that, clear of the buttons), 120 ms, 30 moves evenly over about 500 ms to the middle of the strip's items (30 points in at the visible frame's middle height when the strip hides), a 250 ms hold, the release, the pointer warped back (the drag of the first probe, 11-16; a faster one, 60 ms and 24-point steps 6 ms apart dropped at the grab's height above the items, only moved the window, live 11-16; a window a drag moved and did not take out is set back when the job is given up); the topmost window at the press checked to be that window | The pointer away about 0.9 s; the window flies into the strip; works with the strip covered | A title-bar point that is not free (unified toolbars, Electron: probe 6 still open; a window the list does not show over it: the guard refuses, tried once more, then given up with its words); the user's own release, move or button ends it at once: the button up where the pointer is, the window's frame set back through Accessibility, the job given up | `platform/apple/routes/drag.rs`, `platform/apple/post.rs` | Probe 11-16 (third); `scripts/test_apple.py split`, `pointer` |
| Hide at launch | "Open here" without the round trip: a newly opened app the rules want in the stage in front (a rule's `current`, `[stages] new_apps_join_current`, Shift held at the launch) | The will-launch and did-launch notices; on a thread of its own every 2 ms, `hide()` on the app until it is hidden (false until the app is ready), only for a regular app, stopped once a window of it shows at full size; Apple's puts its window in the strip; one Shift press for its windows, then the app's front window focused as soon as the window list shows them in front | About 0.5 s to the hide, the window in the strip at about 0.7 s, a Shift-click and Apple's add motion (about 0.6 s); the stage in front never moves; joined about 1.7 s after the launch (live 11-16) | A window shown before the hide takes (then the round trip); the strip covered past the wait (the app is then brought in front by focus, Apple's own result, or shown again) | `platform/apple/launch_hide.rs`, `engine/apple/plan.rs` (`open_here`), `engine/apple/job.rs` | Probe 11-16 (third): hidden at about 536 ms, a single-window launch; `scripts/test_apple.py open-here` |
| The round trip | "Open here" when the hide came too late: the stage pushed into the strip put back with the new app's | The window list to see the old stage land in the strip, then a Shift-click per window of it, then the new app's window focused | About 1 s, visible (out, then back in), longer for a stage of several windows | The strip covered; the old stage past the strip's room (brought in front by focus first, then the rest clicked back) | `engine/apple/job.rs` (`Goal::Gather`) | Probe 11-16: window at about 285 ms, the old stage in the strip at about 1 s; `scripts/test_apple.py open-here` says which route ran |
| The edge reveal | Shows a hidden strip | The pointer resting at the screen's left edge | About 2 s of the pointer at the edge | A single posted move or a quick sweep does not reveal | not built (decided 11-16: wait for the strip to show instead) | Probe 11-16 |

**Apple's rule for an app's windows.** With "Show windows from an application: All at Once"
(`AppWindowGroupingBehavior = 1`, the default) Apple's never splits one app's windows into
two stages: a drag of one window while another of its app stays only moves it, from every
drop point tried (the fifth probe, 11-16; the drags that worked took out another app's
window). Send and New for such a window are refused up front with that reason, nothing
posted (`engine/apple/plan.rs`, `apart`); under "One at a Time" they are tried (unprobed).

### The gate

Every posted step waits for the gate (`engine/apple/gate.rs`): no mouse button down; no ⌘, ⌥
or ⌃ held (a hotkey's own modifiers would turn the click into another gesture); the pointer
still for 50 ms and no key event for 150 ms (the hardware's own state: a posted event never
counts); the screen unlocked; no Mission Control, App Exposé or Show Desktop; Apple's motion
settled (the window list still for two reads and 700 ms since the last change); no gesture of
ours in flight. A step held back by the user's input waits at most 1.5 s, by a covered strip
at most 4 s; then the step is given up and logged, never retried blindly. Whether a step's goal is met is
read at every read out of motion (the groups read afresh; a read within 700 ms of a cause
keeps the groups before), settled or not, so a job ends as the window list shows it done; a
focus within the stage in front (the newly opened app's, a raise before a drag) goes at once,
a focus that switches stages waits for the settle. A step that ran and whose window is not
where it should be at the next settle runs once more, then the job is given up. Input events heard within 300 ms of a gesture of ours are its own echoes: they never
count as the user's Shift for "open here".

## Tried and rejected

Each is kept as a tripwire (`platform/apple/routes/rejected.rs`, reachable only through
`ctl apple try ROUTE ID` with `STAGE_MANAGER_TEST=1`): the live check asserts it still does
nothing (`scripts/test_apple.py rejected`), so a macOS update that makes one work turns the
check red and says so.

| Route | What it was to do | Calls | Result | Date, macOS | Tripwire |
|---|---|---|---|---|---|
| `AXPress` on a strip item | Switch to its stage | Accessibility action on WindowManager's `AXButton` | Returns success, does nothing (with the pointer over the strip too) | 11-16, 27.2 | `rejected`: `apple try ax-press ID` |
| `AXAddToStage` on a strip item | Bring its stage into the one in front | Accessibility action | Returns success, does nothing | 11-16, 27.2 | `rejected`: `apple try ax-add ID` |
| A click posted to WindowManager's pid | Switch or add with no pointer | `CGEventPostToPid` (a Shift-click at the thumbnail's point) | Ignored | 11-16, 27.2 | `rejected`: `apple try pid-click ID` |
| Shift flags on the click alone | Bring a stage in | A posted click with the Shift flag, no key event | Switches instead | 11-16, 27.2 | none (the Shift-click route posts the key events) |
| Shift held + focus | Bring a stage in with no pointer | A real Shift down, then the focus route | Switches, does not add | 11-16, 27.2 | `rejected`: `apple try shift-focus ID` |
| A click mid-motion | Save the wait for the settle | A posted click while Apple's animates | Ignored early; at about 85 % it switched back to the stage leaving | 11-16, 27.2 | The unit tests (a click held back until settled) |
| A single posted move to the left edge | Reveal a hidden strip | One posted pointer move | Does not reveal (about 2 s at the edge does) | 11-16, 27.2 | none |
| An observer on WindowManager | Hear the strip change instead of polling | `AXObserver` on WindowManager (created, destroyed, layout, selected children, value, moved, resized) | Subscriptions taken, no notice ever posted: the mode polls | 11-16, 27.2 | none |
