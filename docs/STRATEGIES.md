# Strategies

How Stage Manager gets stages on screen, every way built or tried, in one table per kind with
the same columns: what it does, the calls, what it costs (time, pointer, CPU, what is seen),
what breaks it, where the code is, how it was measured. [ARCHITECTURE.md](ARCHITECTURE.md),
"Window engines", keeps the code map; this file keeps the catalog. Every phase of the Apple
mode adds its rows (A1 and A2 so far: the focus route; the posted routes and their gate come
with A3).

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
| Shift-click (A3) | Brings one window into the stage in front | A posted Shift key-down, a click with no hold at a visible part of its thumbnail, the pointer warped back, Shift key-up | The pointer away 2-15 ms (about a frame); Apple's motion; one click per window (a Shift-click adds only the window clicked) | A click mid-motion (ignored, or switches back); the strip covered; a thumbnail with no part of its own showing | not built | Probe 11-16 (second) |
| Drag (A3) | Takes a window out to a stage of its own | A posted drag of its title bar onto the strip | The pointer held about 250 ms; the window flies | A title-bar point that is not free (unified toolbars, Electron) | not built | Probe 11-16 |
| The round trip (A4) | "Open here": a new app's stage put back with the one it pushed into the strip | The window list to see the old stage land in the strip, then a Shift-click per window | About 1 s, visible (out, then back in), longer for a stage of several windows | The strip covered; the old stage past the strip's room | not built | Probe 11-16: window at about 285 ms, the old stage in the strip at about 1 s |
| The edge reveal | Shows a hidden strip | The pointer resting at the screen's left edge | About 2 s of the pointer at the edge | A single posted move or a quick sweep does not reveal | not built (decided 11-16: wait for the strip to show instead) | Probe 11-16 |

## Tried and rejected

Each is kept as a tripwire: the live check asserts it still does nothing (A3's `rejected`
phase), so a macOS update that makes one work turns the check red and says so.

| Route | What it was to do | Calls | Result | Date, macOS | Tripwire |
|---|---|---|---|---|---|
| `AXPress` on a strip item | Switch to its stage | Accessibility action on WindowManager's `AXButton` | Returns success, does nothing (with the pointer over the strip too) | 11-16, 27.2 | A3 `rejected` |
| `AXAddToStage` on a strip item | Bring its stage into the one in front | Accessibility action | Returns success, does nothing | 11-16, 27.2 | A3 `rejected` |
| A click posted to WindowManager's pid | Switch with no pointer | `CGEventPostToPid` | Ignored | 11-16, 27.2 | A3 `rejected` |
| Shift flags on the click alone | Bring a stage in | A posted click with the Shift flag, no key event | Switches instead | 11-16, 27.2 | A3 `rejected` |
| Shift held + focus | Bring a stage in with no pointer | A real Shift down, then the focus route | Switches, does not add | 11-16, 27.2 | A3 `rejected` |
| A click mid-motion | Save the wait for the settle | A posted click while Apple's animates | Ignored early; at about 85 % it switched back to the stage leaving | 11-16, 27.2 | A3 (a click held back until settled) |
| A single posted move to the left edge | Reveal a hidden strip | One posted pointer move | Does not reveal (about 2 s at the edge does) | 11-16, 27.2 | none |
| An observer on WindowManager | Hear the strip change instead of polling | `AXObserver` on WindowManager (created, destroyed, layout, selected children, value, moved, resized) | Subscriptions taken, no notice ever posted: the mode polls | 11-16, 27.2 | none |
