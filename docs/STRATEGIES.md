# Strategies

How Stage Manager gets stages on screen, every way built or tried, in one table per kind with
the same columns: what it does, the calls, what it costs (time, pointer, CPU, what is seen),
what breaks it, where the code is, how it was measured. [MODES.md](MODES.md) compares the
three ways for users in one table; [ARCHITECTURE.md](ARCHITECTURE.md),
"Window engines", keeps the code map; this file keeps the catalog. The Apple mode's rows came
phase by phase (A1 and A2: the mirror and the focus route; A3: the posted routes and their
gate; A4: where a newly opened app goes; A5: the restore; A6: screens, Desktops, full screen
and the strip's order).

## Strategies

| Strategy | What it does | Calls | Cost | What breaks it | Code | Measured |
|---|---|---|---|---|---|---|
| Pause | While Apple's Stage Manager is on (`[windows] apple_stage_manager_on = "pause"`, the default): every window back, nothing arranged, no strip | Accessibility set frame for each window held, once | Nothing while it lasts | Nothing: Apple's arranges | `engine/stand_down.rs` | The live check `scripts/test-apple-stage-manager.sh` |
| Accessibility (the Corner engine) | Inactive stages' windows parked in a corner of their screen, the motion played by pictures | Accessibility set position and frame; ScreenCaptureKit pictures | A park per window (an app's own move); pictures every 2 s while the strip shows; a sliver per window in the corner, covered by the corner patch | An app that refuses or clamps the move; another screen beyond the corner | `engine/corner`, `platform/corner` | The live suite (`scripts/test_stages.py`) |
| Spaces with pictures | Each inactive stage in a hidden window-server Space of its own; the motion played by pictures | SkyLight Space calls, read back; Accessibility set frame on a show | About 2 ms a window, no app asked; pictures taken at the switch | A window server that refuses the calls (the launch probe, the health checks, then the fall back to Corner) | `engine/spaces`, `platform/spaces` | The launch probe; the live suite with `SM_ENGINE=spaces` |
| Spaces live landing | As above, the incoming windows flying the last stretch themselves through their Space's transform | The Space transform per step | As above, no picture over the window at its end | A transform call missing (then pictures) | `platform/spaces/landing.rs` | `STAGE_MANAGER_LANDING=live`, the live suite |
| Integrate | Apple's Stage Manager arranges; Stage Manager reads its arrangement and follows it with its own model, and acts through routes Apple's answers to | The window list (each read); WindowManager's Accessibility (read only, every 2 s); the routes below | A list read per event, every 50 ms while anything moves, every 2 s at rest; nothing of ours hidden, no strip or pictures of ours | Apple's changing what WindowManager lists (its layer-0 windows over the thumbnails, `sm.strip`'s items); see each route | `engine/apple`, `platform/apple`, `platform/conductor.rs`; `[windows] apple_stage_manager_on = "integrate"` | Two probes, 11-16, macOS 27.2; `scripts/test_apple.py` |

## Routes inside Apple mode

| Route | What it does | Calls | Cost | What breaks it | Code | Measured |
|---|---|---|---|---|---|---|
| Focus | Brings a stage in front: its key window raised, made main, its app frontmost | Accessibility `AXRaise`, `AXMain`, the app's `AXFrontmost` | 5-7 ms, no pointer; Apple's own motion follows (about 700 ms); works with the strip hidden and for a stage the strip has no room for | A window not known (never seen in a stage); an app that does not answer | `platform/apple/routes/focus.rs`, `engine/apple/plan.rs` | Probe 11-16; `scripts/test_apple.py switch` |
| Shift-click | Brings windows into the stage in front (`ctl merge N`: every window of stage N; `ctl send N`: a switch to N, the click, a switch back); only an item's front window takes a click (the eighth probe, 11-17: a window behind another of its item, of one app or two, took none, alone or after the front's in one press), so a press clicks the front of each item and the windows behind come in later presses, each at a fresh read once Apple's add motion has settled; each lands on top, so the order in front is then put back by the focus route on the windows out of place, back to front, the one wanted on top last (the eighth probe: a focus of a window in the stage in front puts it on top, the stage whole; AXRaise alone, and with AXMain, did nothing) | A real Shift key-down (none when the user holds Shift), about 12 ms, then a click per item's front 15 ms apart, each with no hold at a visible part of its thumbnail (6 pt inside the plane Apple draws, which the window list lists at its bounding box: a click in the box's corners outside it reveals the desktop or is lost, live 11-17; 3 pt off what is drawn over it, the thumbnails listed in front of it as their planes grown as under the pointer, the app icons at their middle only, any other window whole; the ones of the same press not clicked yet counted, read from the window list at the settle; `engine/perspective.rs` turns a box back into its plane), the pointer warped back once the posted events have moved it (else the warp lands first and the click leaves the pointer at its point: live 11-16), about 12 ms after the last, Shift key-up; on a thread of its own; the topmost window at each point checked to be WindowManager's just before its click | The pointer away 2-15 ms a click (about a frame); one click per window (a Shift-click adds only the window clicked); one press and one Apple motion, about 700 ms, per window of an item (the items' fronts together), then a focus per window out of place, about 700 ms each | A click mid-motion (ignored, or switches back: every click waits for the settle); the strip covered (waited for, at most 4 s, then given up and logged); a thumbnail with no part of its own showing (what covers it and the pointer logged at debug as the wait begins), and a window Apple draws no thumbnail of (an item shows three: the fourth and on are listed off screen within it, live 11-17), each clicked at the next settle, on top; the gate (below) | `platform/apple/routes/shift_click.rs`, `platform/apple/post.rs`, `engine/apple/points.rs`, `engine/apple/job.rs` | Probes 11-16 (second; fourth: a stage of two in one press); `scripts/test_apple.py merge`, `pointer`; `scripts/probe_press_quad.py` (where the strip takes a click: the model's plane edges, corners, icons) |
| Drag | Takes a window out to a stage of its own (`ctl new`, the new-stage hotkey) | The window brought in front by focus if it is not, and raised the same way when its title bar is covered all along (live 11-16: the fixture windows open at one frame); a posted press on its title bar (14 points below its top, at its middle, or the free point along it nearest that, clear of the buttons), 120 ms, 30 moves evenly over about 500 ms to the middle of the strip's items (30 points in at the visible frame's middle height when the strip hides), a 250 ms hold, the release, the pointer warped back (the drag of the first probe, 11-16; a faster one, 60 ms and 24-point steps 6 ms apart dropped at the grab's height above the items, only moved the window, live 11-16; a window a drag moved and did not take out is set back when the job is given up); the topmost window at the press checked to be that window | The pointer away about 0.9 s; the window flies into the strip; works with the strip covered | A title-bar point that is not free (unified toolbars, Electron: probe 6 still open; a window the list does not show over it: the guard refuses, tried once more, then given up with its words); the user's own release, move or button ends it at once: the button up where the pointer is, the window's frame set back through Accessibility, the job given up | `platform/apple/routes/drag.rs`, `platform/apple/post.rs` | Probe 11-16 (third); `scripts/test_apple.py split`, `pointer` |
| Hide at launch | "Open here" without the round trip: a newly opened app the rules want in the stage in front (a rule's `current`, `[stages] new_apps_join_current`, Shift held at the launch) | The will-launch and did-launch notices; on a thread of its own every 2 ms, `hide()` on the app until it is hidden (false until the app is ready), only for a regular app, stopped once a window of it shows at full size; Apple's puts its window in the strip; nothing of it in the strip 275 ms after the hide took (or a window of it known sooner), `unhide()` with no activation (some apps make no window while hidden: Reminders, live 11-16); its thumbnails taken by its pid from the window list, each with a twin and at rest for two reads (25 ms apart while a job runs, never the whole arrangement's settle), Shift-clicked in one press, then its window focused once at rest in front | About 0.5 s to the hide, the window in the strip at about 0.7 s, a Shift-click and Apple's add motion (about 0.6 s); the stage in front never moves; joined about 1.7 s after the launch (live 11-16) | A window shown before the hide takes (then the round trip); no window in 10 s (shown again and activated); the strip covered past the wait (the app is then brought in front by focus, Apple's own result, or shown again) | `platform/apple/launch_hide.rs`, `engine/apple/plan.rs` (`open_here`), `engine/apple/job.rs` | Probe 11-16 (third): hidden at about 536 ms, a single-window launch; `scripts/test_apple.py open-here` |
| Hide at activation | "Open here" for a running app with no window that reopens one at its activation (a Dock click, `open`, ⌘Tab: Notes, Activity Monitor; Finder is left to Apple's, never hidden as it owns the Desktop and the round trip in its place reads as a switch away and back), under the same rules as a launch | The did-activate notice, when the app has no window anywhere (none in a stage, on another Desktop or known to the observer) and was not launched just now; `hide()` every 2 ms as at a launch; 275 ms after the hide took, `unhide()` with no activation (it answers false and still shows the app), the reopened window never waited for by its id (such an app reopens the window it ordered out, an id seen before, and while hidden Accessibility may read it as `AXDialog`: live 11-16, Activity Monitor and Finder); Apple's puts the window in the strip; every window of the app's pid with a WindowManager twin over it in the window list, whatever its id or subrole, at rest for two reads (25 ms apart while a job runs, never the whole arrangement's settle), Shift-clicked in one press, then focused once at rest in front | The hide within about 50 ms of the activation (live 11-16), the unhide 275 ms on, Apple's motion into the strip (about 0.5 s), two reads, a Shift-click and the add motion (about 0.6 s); the stage in front never moves; every wait logged at debug with its reason and length; logged as route `hide_activation` with `unhide_ms` | A window shown before the hide takes, Finder (never hidden: it owns the Desktop, and hidden its stage read empty, live 11-16) and an app that activates itself when shown again (1Password, live 11-16): the round trip, the app's windows taken by its pid and the reason logged; an app that reopens no window (shown again, then activated when no thumbnail of it shows 1.5 s after: a ⌘Tab under a rule costs those two seconds); the strip covered past the wait | `platform/apple/launch_hide.rs` (`Moment::Activation`), `engine/apple/plan.rs` (`reopen_here`), `engine/apple/job.rs` (`Step::Unhide`), `platform/apple/controller.rs` (`app_activated`) | Probe 11-16 (sixth): a fixture copy running with no window, `open` and a window about 50 ms after the activation; `scripts/test_apple.py open-running` (written, not run yet) |
| The round trip | "Open here" when the app came in front: its window shown before the hide took (a window shown at the activation), or the app in front by itself once shown again | Once Apple's switch to the app's stage is at rest (two still reads, not the settle's 700 ms after its last frame), the focus route on the front window of the stage that left: Apple's brings that stage back whole, its order and the windows it drew no thumbnail of with it, and sends the app's stage to the strip; the app's windows then taken by its pid, at rest for two reads, in one Shift press (on top), then focused. A hide called off as too late that took all the same (live 11-17: 35 ms on) is the hide route: shown again at once, its thumbnail taken in ahead of the settle | Three Apple motions (the app's switch, the switch back, the add: about 0.7 s each), one press whatever the stage's size, no point of the stage that left to wait for; visible (the app in front, out, back in on top) | The strip covered past the wait for the app's thumbnail (the app's stage then brought in front by focus, Apple's own result); an app that comes in front by itself again and again (the switch back runs twice, then the job is given up) | `engine/apple/job.rs` (`Goal::Gather`, `Job::pickup`, `Job::hid_late`) | Live 11-17 before (a Shift-click per window of the stage that left: a stage of four took three presses, 7.3 s, and came back reversed); the fake's scenarios; `scripts/test_apple.py roundtrip-order` (written, not run yet) |
| Restore | Puts saved stages back on Apple's (`ctl apple restore`: the stages `state-apple.json` held when the mode started; by itself after a restart, `[stages] restore_with_apple`, on by default) | The restore's own matcher (`engine/persist.rs`, `rematch_on`: the window id within a window server session, else the app, the title's fingerprint and the frame; confident matches only, a strip window's frame counting for nothing); then one goal per saved stage, the saved active one last: a window of it in front by focus (the one whose stage holds the fewest windows of the other saved stages), each window of another saved stage in front with it out by the drag, the rest in one Shift press; a stage that is so already left as it is | Per saved stage that is not so already: a focus and Apple's motion (about 0.7 s), a drag per window to take out (the pointer away about 0.9 s each), one Shift press (about 0.8 s); a visible sequence of a few seconds for a few stages. Estimated from the routes' own costs; `scripts/test_apple.py restore` prints the time | Under All at Once an app's windows saved in two stages (asked in one, said); a window of another saved stage whose app keeps another window in front (it stays, said); a window gone (skipped, said); a window on a Desktop not shown (left there, said); the strip covered (waited for, at most 4 s a step); the gate. A stage that cannot be put back is skipped and the rest go on | `engine/apple/restore.rs`, `engine/apple/job.rs` (`Goal::Stage`), `platform/apple/controller.rs` | Unit tests over the fake; `scripts/test_apple.py restore` (written, not run yet) |
| The edge reveal | Shows a hidden strip | The pointer resting at the screen's left edge | About 2 s of the pointer at the edge | A single posted move or a quick sweep does not reveal | not built (decided 11-16: wait for the strip to show instead) | Probe 11-16 |

**Apple's rule for an app's windows.** With "Show windows from an application: All at Once"
(`AppWindowGroupingBehavior = 1`, the default) Apple's never splits one app's windows into
two stages: a drag of one window while another of its app stays only moves it, from every
drop point tried (the fifth probe, 11-16; the drags that worked took out another app's
window). Send and New for such a window are refused up front with that reason, nothing
posted (`engine/apple/plan.rs`, `apart`); under "One at a Time" they are tried (unprobed).

**The restore after a restart** waits until the restore window's windows have shown: it runs
when the mode starts in another window server session than the saved file's (a restart, a new
login), once every saved window is matched and none came for 3 s, or the login settled (apps
quiet for 20 s and a minute after the login), or five minutes after the start. Until then
nothing of Apple's fresh arrangement is saved over the file. It moves windows with the posted
routes, so it goes through the gate like every step: a person already at work holds each step
back until a still moment, never fights a click or a key.

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

## Apple's rules, as met

| Apple's rule | Measured | What the mode does |
|---|---|---|
| "Show windows from an application: All at Once" keeps an app's windows in one stage | Fifth probe, 11-16: a drag of one window while another of its app stays only moves it | Send and New for such a window refused up front with the reason; the restore asks an app's windows in one saved stage (the one holding most of them) and never drags one whose app keeps another window in front |
| The strip has room for a few stages; the rest are not listed | First probe, 11-16: a stage past the room drops out of `sm.strip` | Such stages are remembered with their last membership; switched to by focus (it needs only a known window); gathered by a focus first, then the stage left Shift-clicked back |
| WindowManager posts no Accessibility notices | Second probe, 11-16: subscriptions taken, nothing posted | The mirror polls: a read at every event, every 50 ms in motion, every 2 s at rest |
| Clicks mid-motion are ignored or switch back | Second probe, 11-16 | Every posted step waits for the settle |
| Apple's orders its own strip | First probe, 11-16: the stage pushed into the strip went back to its old slot; what else orders it is not probed | Nothing reorders it (below, "Tried and rejected"); `[strip] order` does nothing in the mode |
| Each screen has a strip of its own, each Desktop stages of its own | `sm.space` per screen in WindowManager's tree (the first probe saw one screen; a second screen's layout not measured) | One mirror per screen (below) |

### Screens, Desktops and full screen

- **Screens.** One mirror per screen. The window list is one for all screens, so each managed
  window is given to a screen first (`engine/apple/screens.rs`): the screen its middle is on;
  off every screen (a hidden strip's thumbnail), the screen it was on, else the one whose left
  edge is nearest right of it; a thumbnail-sized window with no twin keeps the screen it was
  on (a hidden strip's thumbnails may lie on the screen to the left). Each screen's strip is
  the `sm.space` whose `AXFrame` is the screen's, else, when no `sm.space` has a frame, the
  one at the screen's place (the main one first). A step acts on one screen: send and new on
  the window's, the rest on the keyboard's (the focused window's screen, the main one with
  none); the restore on the main one. The model and `state-apple.json` follow the main screen;
  the others' stages show in `ctl state` (`apple.screens`). Nothing is assumed about a second
  screen's layout: the log says what each screen's mirror sees and what WindowManager lists
  against the screens. Not measured live.
- **Desktops.** Each Desktop has stages of its own, and the window list cannot tell a stage of
  another Desktop from one past the strip's room (both are listed off screen). So at a Desktop
  change every group known goes *away*, tagged with the change: away groups are kept (the
  model keeps them as stages), never counted by `ctl switch N` or the hotkeys, and never acted
  on (a focus of one of their windows would show their Desktop): send, new and a merge of them
  are refused, a gather or a restore leaves them there and says so. Once a window of a
  change's groups is seen again, its Desktop shows again, and the rest of them are that
  Desktop's stages past the strip's room once more.
- **Full screen.** A window on screen covering most of a screen that says it is in full
  screen through Accessibility marks that screen's mirror; no step runs there (a job waits at
  most 4 s, then is given up), and asks for it are refused with the reason.

## Possible next steps

- **Our stages carried into Apple's when the mode starts** (an open decision): when Apple's is
  turned on with `apple_stage_manager_on = "integrate"`, Stage Manager's own stages (`state.json`'s Desktop
  shown) could be put on Apple's by the restore's job, as it puts back `state-apple.json`'s.
  Today they are set aside and come back when Apple's is turned off; Apple's starts from its
  own arrangement. It would cost the restore's visible sequence at every turn on, and asks
  for the owner's word: the default never goes against what Apple's shows.

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
| Reordering Apple's strip (the Dock order, `[strip] order = "dock"`) | Apple's strip in the order of the Dock, or any order of ours | Considered: an Accessibility action on a strip item (only `AXPress` and `AXAddToStage` are listed, both doing nothing); a drag of a strip item up or down (Apple's strip has no reorder: a thumbnail dragged out of it goes to the stage in front); a setting (`com.apple.WindowManager` has none for the order); a chain of focuses, each sending the stage left to a slot of Apple's choosing (the first probe saw it go back to its old slot), to permute the slots | Not built: no call reorders it, and the chain of focuses would play one visible Apple motion per swap, every time the order drifts, against Apple's own rule for its slots | 11-16, 27.2 (the probes); a finding, not a route | none |
