# UX spec: Apple's Stage Manager, observed, and how we match it

Measured on 2026-10-12, macOS 27.0.1, with the owner using Stage Manager live: first on one
5K display (2560 × 1440 pt, scale 2, menu bar 30 pt, visible frame 0,30 2560 × 1410), then on
his MacBook (1800 × 1169 pt, visible frame 0,39 1800 × 1130; window bounds at ~60 Hz and a
still of the left 420 pt every ~0.9 s). Method: public settings
(`defaults read com.apple.WindowManager`), `CGWindowListCopyWindowInfo` sampled at ~60 Hz
(window bounds, z-order, pointer position, button state), and still screenshots. No Apple
binary was inspected and no Apple artwork is in this repo. Coordinates are global points,
top-left origin.

Stage Manager draws its strip with the real windows, scaled. So the window list reports each
strip card's scaled bounds, and each card's app icon is its own small window. That is what
made the geometry and timing measurable without a screen recording.

## Settings (mirrored, read live)

| macOS setting (System Settings → Desktop & Dock) | Key in `com.apple.WindowManager` | His value | Our behavior |
|---|---|---|---|
| Stage Manager on/off | `GloballyEnabled` | 1 | We warn while Apple's is on; both cannot run |
| Show recent apps in Stage Manager | `AutoHide` (1 = hidden) | 0 | 1 hides our strip until edge hover |
| Show windows from an application: All at Once / One at a Time | `AppWindowGroupingBehavior` (1 = all at once) | 1 | See "Grouping" |
| Show items: On Desktop, in Stage Manager | `HideDesktop` (1 = hidden) | 0 | Read; Finder desktop icons are not ours to hide (deviation) |
| Click wallpaper to reveal desktop | `EnableStandardClickToShowDesktop` (1 or absent = Always, macOS's default since Sonoma; 0 = Only in Stage Manager) | absent (= Always) | Only in Stage Manager: our click toggles the desktop reveal. Always: macOS moves every window aside itself and our reveal stays off, so the two never fight |
| Widgets in Stage Manager | `StageManagerHideWidgets` | 1 | Not applicable |

Our `config.toml` never repeats these. It only adds what macOS lacks.

## Native defaults (the owner's rule, 10-12)

Out of the box the app looks and behaves like Apple's Stage Manager; every extension is off
unless it is invisible until deliberately used. Every number below is a `config.toml` key
whose default is Apple's value (`stage-manager config --explain <key>`).

| What | Apple | Our default | Key to change it |
|---|---|---|---|
| Turning Stage Manager on | moves and resizes nothing (a window at x = −6, 1800 wide, stays) | same | — |
| Switching stages | each window back at exactly its own frame, even over the strip | same | `[windows] fit = "move"` / `"narrow"` keeps windows clear of the strip |
| A window over the strip | the strip hides while it overlaps | same | — |
| The strip's level | below windows: a window moved over it covers it (owner, 10-12) | same; above windows only while a window or card is dragged over it or the edge reveals it over a window | `[strip] level = "above_windows"` |
| Switching | windows grow from their card and shrink into their slot on the spring | same, drawn with snapshots (deviation 1) | `[animation]` |
| Drag a card up or down in the strip | nothing (Apple's strip cannot be reordered) | moves the card to that slot (an extension, invisible until used) | `[strip] reorder` |
| Drop a card on another card | nothing (Apple draws a similar stacked card for a stage of several apps, but a drop there does not merge) | merges the two stages (an extension, invisible until used; the owner's ask, 10-12) | `[strip] merge_on_drop`, `[drag] merge_zone` |
| Edge tab, launch reveal | none | off | `[reveal] edge_tab`, `launch_reveal` |
| The strip while a wallpaper click shows the desktop | stays shown (the way back to a stage) | same | `[strip] hide_while_desktop_shown = true` hides it until the reveal ends or any window activation |
| Card numbers, stage names | none | off | `[strip] show_numbers`, `show_names` |
| Global hotkeys | none | off | `[hotkeys] enabled` |
| Strip size by display | the same lengths everywhere | same | `[strip] scale`, `scale_to_display` |
| Cards shown | 5 at 1130 pt visible height, 6 at 1410 | `floor((h − 2·60) / 190)` within 3…9 | `[strip] capacity*` |
| Shift-click a card | merges the card into the current stage | same | — |
| Option-click a card | nothing | nothing | — |
| ⌘M | the window goes to the strip, not the Dock | same (it may flash) | `[windows] minimize_to_strip` |
| Click a card's app icon | all of the app's windows, also from other stages | same | — |
| One at a Time, click the app again | opens its next window | same (one card per app, cycling) | — |
| Shift on opening or activating an app | (no meaning) | brings it here (invisible unless Shift is held) | `[sugar] shift_brings_here` |
| Mission Control (F3, Control-↑) | the strip disappears while it shows and is back when it ends (owner, 10-12) | same: every panel of ours is `Transient`, which Exposé hides; the strip, edge tab, drag ghost and motion overlay are not drawn and come back on the same Desktop (before 10-12 they were `Stationary` and stayed drawn over it). Mission Control lays out every window of the Desktop, parked ones included | — |
| App Exposé (Control-↓) | the strip disappears | same (`Transient`) | — |
| Show Desktop (F11) | the strip leaves with the windows (to observe: an Apple Community report of the strip and windows sliding off the edges) | hidden (`Transient`; to observe) | — |
| Launchpad, the ⌘Tab switcher | drawn over everything; the strip stays under them | same; our panels are never in ⌘Tab or the ⌘\` window cycle (`IgnoresCycle`) | — |

## Strip geometry (measured; every number is a `[cards]` / `[icons]` key)

Apple draws each card as the real windows, scaled and turned; the window list reports each
card's bounding box, and the app icons are 66 × 82 pt windows. Both displays give the same
rules (a 1–2 pt reading error on screenshots):

- **Scale.** Every window is drawn at one scale: a window as tall as the visible frame
  becomes a **161 pt** tall card (laptop 0.1427 × 1130, 5K 0.1142 × 1410), so a card is
  `window × 161 / visible height`. Small windows are drawn at least **97 pt** tall (a 920 ×
  436 Finder window on the laptop and a 1102 × 643 one at 5K both 97 pt). Seen boxes on the
  laptop: 162 × 193 (Safari 1800 × 1130), 159 × 161 (Fork 1753 × 1130), 120 × 144 (Terminal
  1280 × 940), 99 × 180 (Chrome 1040 × 1130), 113 × 109 (Contacts 1196 × 758), 133 × 107
  (Finder 920 × 436).
- **Turn and perspective.** Each window is a plane turned about its own left edge (the right
  side away), seen from one eye at the display's vertical middle (laptop 584.5 pt, 5K 720)
  and at the cards' left edge horizontally. So cards above the middle show their top edge
  sloping down to the right, cards below show their bottom edge sloping up, and the edges of
  every card meet about 1390 pt (laptop) / 2040 pt (5K) to the right. Fitted to every stable
  box of the sessions (rms 0.4 pt laptop, 0.25 pt 5K): **44.2° and an eye distance of
  1346 pt** on the laptop, **47.9° and 2260 pt** at 5K; `"auto"` interpolates by visible
  width. The far edge is `D / (D + w sin θ)` of the near one: about 0.89 for a 160 pt card.
  Sliding out confirms the eye stays put: a card moving 286 pt left widens from 159 to
  191 pt, exactly as the model predicts.
- **Column.** Cards' left edges at **x = 15 pt**; **29.5 pt** from one card's left-edge
  bottom to the next card's top; the column centered on the **visible frame** (laptop:
  189.4 … 1018.4, center 604 = 39 + 1130 / 2; 5K: 281 … 1189, center 735).
- **Stack.** Windows of one app step **17 pt** to the right toward the front one, at the
  same scale: of a pair of Finder windows the front one's box was at x = 32 (104 wide) and
  the one behind at x = 15 (106 wide), peeking out on the left; both 98 tall. A second
  app's window **68 pt** right and **14 pt** up (5K session).
- **Icon.** About **31 pt** of icon shows (a 38 pt image with the standard margin), centered
  **10.5 pt right of and 10.5 pt above the card's bottom-left corner** (its leftmost window's) (icon windows
  at x = −7, 43.5 pt above that corner, on both displays), with a soft shadow; a second app's
  icon 36 pt right. No badge, no name.
- **Thumbnail edge.** Corners about 4 pt; a thin light edge; a soft shadow about 4 pt.
- **Hover.** The card under the pointer grows **6 %** about its center (Fork 159 × 161 →
  168 × 171, both edges moving), in about **210 ms**, eased out (see "Transitions").
- **Slide.** Hiding moves the cards **286 pt** left (x 15 → −271) on a spring close to the
  transitions' (ω ≈ 16.8: 50 % at ~100 ms, 90 % at ~235 ms, 99 % at ~400 ms); showing
  reverses it.
- **Count.** 5 cards at 1130 pt of visible height, 6 at 1410 pt.

Our strip is checked against these numbers in `ui::strip_layout`'s tests (every stable box
of both sessions within 1.5 pt) and by overlaying our geometry on the screenshots (every card
edge within 0.7 % of the card's height on both displays).

## Strip order

Apple's strip is **slot stable, not "most recent first"**. Every switch was a swap:

- **Click a card:** that card's stage becomes active, and the stage you leave takes the
  **slot the clicked card vacated**. The other cards shift a few points (≤ 20 pt) only to
  re-center the column.
- **A stage entering the strip with no slot free** (wallpaper click, new stage) takes the
  slot of the least recently used card, which drops out of view.

We implement exactly this (`strip.order = "native"`). `"fixed"` (creation order, matching
the hotkey numbers) is an option.

**Reorder (an extension, `[strip] reorder = true`).** Apple's strip cannot be reordered by
dragging. Ours: press a card and drag it up or down within the strip column; the card
follows the pointer, the others make room on the spring, and on release it takes the slot
under the pointer (the gap between the cards whose middles are above and below it). The new
order is saved; in Apple's order the swap rule above carries on from it, and in `"fixed"`
order the stage moves in creation order (its hotkey number follows the strip). A card
dragged past the column turns into the drag-out ghost (below); back over the column it
follows the pointer again.

**Merge (an extension, `[strip] merge_on_drop = true`; Apple: nothing).** While a card is
dragged in the column, the middle part of every other card (`[drag] merge_zone` of its
height, default the middle half) means "onto this card"; above and below it, and between
cards, is the reorder gap as before. Both are measured on the cards at rest with the dragged
card's slot left out, the same reference the gap always had, so the cards making room never
move a target. Held over a card's middle, the reorder gap closes (the dragged card keeps its
own slot meanwhile) and that card grows by the hover scale on the hover's easing and draws
the dragged card's windows stacked behind its own, exactly as the merged card will look (a
target that already shows `max_windows` windows only grows); leaving the middle undoes it,
the cards moving on the spring. Released there, the dragged card shrinks into the target and
fades, the dragged stage's windows join the target's stage behind its own (they stay parked
with their saved frames; nothing moves on screen), the target keeps its slot, key window,
name and the more recent of the two stages' recency, and the dragged card's slot frees by
the slot rule (the cards close up; a free slot takes the most recent stage out of view). An
emptied declared stage stays, as with a Shift-click merge. Undo: pick the merged card and
drag a window onto the strip, which makes it a stage of its own again. Zones on his
displays (four cards: a full-height window, 1280 × 940, a 920 × 436 Finder window, a tall
narrow window): onto-card bands 55–91 pt (laptop) and 53–88 pt (5K) tall, the gaps between
them 79–93 pt and 75–90 pt.

## Transitions

Measured 10-12 (round 6R) on the laptop session (`session.jsonl`: every window's bounds and
the pointer at ~120 Hz while the owner used Apple's Stage Manager, 1800 × 1169 pt; 61
incoming and 24 outgoing windows over the strip clicks, 37 incoming fitted cleanly; 198 card
glides, 155 strip slides, 135 hovers, 12 presses). Every number is a config key with this
value as its default (`docs/GUIDE.md`, "Animations"); `[animation] speed` scales them all.

- **Press.** On **mouse-down** the pressed card answers within one to four frames (median
  first change 24 ms after the press): it eases back from its hover growth to a little under
  its resting size (0.987), 50 % at 36 ms, 90 % at 72 ms, done by ~100 ms
  (`[cards] press_scale`, `press_ms`). Ours: the same, from the strip's own mouse-down (no
  round trip), and the incoming snapshots later grow from the card as pressed.
- **Trigger.** The switch itself acts on **mouse-up**: the first frame of motion is one
  frame after the release (median 13 ms incoming, 15 ms outgoing). Activations from the
  Dock or ⌘Tab start about **110–200 ms** after the input, after the app activates. Ours: a
  card click or hotkey makes the new stage's app frontmost and its key window main at once,
  with the parks; the windows themselves arrive behind their growing snapshots, so
  keystrokes typed during the animation already go to the new stage. Typing does not cut the
  animation short. The windows' frames are read through AX at the press (`CardPressed`), so
  the release goes straight to the motion (the owner's log of 10-12: ~10 ms of AX reads
  after the release before any motion); the strip logs how long each transition waited for
  the main thread (`queued_ms`; his Dock click waited 70 ms once, cause not known yet).
- **Curve.** Incoming windows grow from their card to their frame along a **critically
  damped spring, ω ≈ 16.5 rad/s** (stiffness 272, damping 33, mass 1; pooled fit over 37
  windows, rms 0.021; per window ω 16.4–17.0). Median progress after the mouse-up: 13 % at
  50 ms, 40 % at 100 ms, 66 % at 150 ms, 82 % at 200 ms, 91 % at 250 ms, 96 % at 300 ms,
  99 % at 400 ms, 99.8 % at 500 ms (99 % ~405 ms after the motion starts; 99.9 % at ~556 ms
  after the release). No overshoot. Outgoing windows shrink into their slot on the same
  curve (50 % at 89 ms, 90 % at 208 ms from the release). Before 10-12 ours used ω 15.5
  (stiffness 240, damping 31): 99 % ~30 ms later than Apple's, and started ~2 frames later.
- **Other cards** glide to their new slots on a spring of **ω ≈ 17.0** (stiffness 289,
  damping 34; 50 % at 89 ms, 90 % at 228 ms, 99 % at 375 ms), `[motion] spring_*`.
- **Slide.** The strip slides out and back in on **ω ≈ 16.8** (stiffness 282, damping 33.6;
  50 % at 102 ms, 90 % at 237 ms, 99 % at 400 ms), `[motion] slide_*`.
- **Hover.** Grows 6 % in **~210 ms** eased out (median 208 ms growing, 220 ms shrinking;
  50 % at 76 ms), `[cards] hover_ms`.
- Ours vs Apple's for a card click, from the release (expected from the parts; not yet
  measured live, round 6R ran no live phase): the motion starts one to two frames after it
  (Apple's one), 99 % at ~410 ms (Apple's ~405), the incoming window in place behind its
  snapshot at 99 % (`unpark_at`). Before 10-12 the owner's log showed ~450–520 ms from the
  click to the window in place.
- Switching Spaces (not Stage Manager) slides the whole screen 2609 pt in about 230–310 ms;
  every Space has its own strip (see "Spaces").

## Interactions

| Gesture | Native behavior (observed unless noted) | Ours |
|---|---|---|
| Click card | Switch to that stage; swap slots (above) | Same |
| Shift-click card | Adds the card's windows to the current stage (observed 10-12) | Same |
| Option-click card | Nothing (observed 10-12) | Nothing |
| Click a card's app icon | All of the app's windows, also those in other stages (MacMost; Apple's docs are silent) | Switches to the card's stage and brings the app's windows from other stages there |
| ⌘M | The window goes to the strip, not the Dock | Same: parked as a stage of its own, then un-minimized there (deviation 7) |
| Drag card into main area | The window grows to full size as it leaves the strip, follows the pointer, and joins the **current** stage on drop | Same gesture: a press that moves more than 5 pt lifts the card (less, or a drag released over its own slot, is a click); a ghost of its front window follows the pointer and grows from thumbnail to window size over the 260 pt past the strip's edge, the grab point sliding into the title bar. Released in the main area, the stage's windows join the current stage and the front window lands with its title bar under the pointer (clamped onto the screen); the others keep their saved frames. Released over the strip: cancelled. See deviation 5 |
| Drop a card on another card | Nothing: the card goes back to its slot (Apple's strip has no drop target on a card) | The two stages merge: see "Merge" under "Strip order" (`[strip] merge_on_drop`) |
| Drag window onto strip | The window shrinks to card size under the pointer as soon as the pointer is over the strip and the cards make room where it is; on drop it becomes **its own stage** in that slot (settles in about 400 ms) | While a managed window is dragged by its title bar and the pointer is over the strip column, the strip comes above windows (and slides in if hidden), a card-sized snapshot of the window, turned like a card, follows the pointer (every drag event), and the cards open a gap at the slot under the pointer on the spring. Released there, the window becomes its own stage, its snapshot shrinks into the gap and becomes its card, and its pre-drag frame is its saved frame. Leaving the column takes the snapshot away and the strip back below windows. See deviation 5 |
| Dock click or ⌘Tab to an app in the strip | Switches to that app's stage; the outgoing stage takes the vacated slot | Same |
| Shift + Dock click on an app in another stage | (No native meaning) | Sugar: the app's windows come to the current stage instead (all of them; the most recent with One at a Time) |
| Open an app with Shift held (Dock, Finder, Spotlight Shift-Return, Launchpad) | (No native meaning; the app opens in a new stage) | Sugar: its new window joins the current stage |
| ⌘Tab / ⌘⇧Tab | Switches stages (⌘⇧Tab cycles backward) | Same; Shift never changes the app switcher |
| Click wallpaper | **Reveal desktop:** the active stage goes into the strip and the main area is empty. A second wallpaper click brings it back | Same, following `EnableStandardClickToShowDesktop` (ours only with Only in Stage Manager); the revealed stage's card takes its slot at once (a free slot, else the least recently used card's) |
| Click the strip column off the cards (between cards, below the last) | Not measured: Apple's strip draws no backdrop there | The click goes to whatever lies below (the wallpaper, a Finder desktop icon, a window under a strip lifted above windows): the pane takes the mouse only on a card (where it is drawn, grown when hovered) and while a card is pressed or dragged. Until 10-12 the whole 200 pt column swallowed every click (review R1-ui 8) |
| Double-click wallpaper | Not measured (Apple's docs are silent) | One click: the second click of a double click (macOS's click count 2, within the user's double-click interval) toggles nothing, so the stage does not come straight back (the owner's log of 10-12: two clicks 40 ms apart revealed and brought it back) |
| Click wallpaper with Finder windows in a stage of the strip | Finder becomes the frontmost app (the desktop is Finder's) and its windows stay in the strip; they come back only when their card is picked, one of them is clicked, or Finder is activated on purpose (Dock, ⌘Tab) (expected, not measured: Apple's docs are silent) | Same. macOS reports Finder activated and its main window focused 0.1–0.5 s after the click; that echo is ignored for up to 1 s after a wallpaper click, unless the user does something else first (a click off the wallpaper, a key press). Before 10-12 the echo brought Finder's stage, so each further click toggled Finder's windows in and out |
| Click a card while the desktop shows | That stage comes back (Apple's docs: "Click an app on the left side of the screen"); what the other cards do was not measured | Same: exactly that stage; the desktop has no card, so nothing takes the clicked card's slot and the other cards keep their order (the column re-centers); and Finder's late echo of the wallpaper click does not switch again. The card of the stage just revealed brings it back as it was |
| Window overlapping the strip area | The strip hides while it overlaps; Apple never moves or resizes the window (measured 10-12: VS Code at x = −6, Fork at 47, Chrome at 166 stayed, also on every switch) | Same. Optional: `[windows] fit = "move"` moves a placed window right just enough (strip column plus `fit_margin`), `"narrow"` also narrows one too wide; pre-fit frames come back on quit |
| Pointer on the edge tab (optional) | (Apple has no tab) | With `[reveal] edge_tab = true`: while a window hides the strip, a slim translucent tab (10 × 72 pt window, 5 pt pill, light/dark aware) at the left edge, vertically centered; resting on it reveals the strip like the edge, a click at once |
| Pointer at the left edge | Strip slides back in (delay not captured) | Reveal after the pointer rests at x ≤ 2 pt for **250 ms** (provisional, `[reveal] delay_ms`); hide again 500 ms after the pointer leaves the strip while a window still overlaps (`hide_delay_ms`) |

## First start and new windows

- **Turning Stage Manager on** (our first start, no `state.json`): every app with windows
  becomes its own stage. The frontmost app's stage is shown; the others enter the strip most
  recently used first (from the window list's z-order). Later launches restore saved stages
  and treat apps that were not saved the same way.
- **New windows** (`[defaults] action = "native"`): a window of an app already in the
  current stage joins it; a window of an app whose windows sit in a parked stage joins that
  stage and brings it forward (the most recently used one if there are several); a window
  of an app with no managed window (newly opened, or all its windows closed) opens a new
  stage, and the stage you leave enters the strip. Rules (`current`, `new`, `stage:<name>`,
  `float`, `ignore`) still apply to the apps they name.

## Spaces (Desktops)

Native (Apple's Stage Manager guide; the 5K session; the owner's report of 10-12):

- **Each Space has its own Stage Manager**: its own active stage and its own strip, listing
  only that Space's windows. The same app can have stages on several Spaces, each holding
  only that Space's windows (VS Code with a work window on Desktop 1 and a personal one on
  Desktop 2).
- **A card click never changes the Space.**
- **Full-screen apps** are Spaces of their own, with no strip.
- **Dock and ⌘Tab** may switch Spaces according to macOS's own setting ("When switching to
  an application, switch to a Space with open windows for the application", Desktop & Dock
  → Mission Control). Stage Manager follows; it never fights it. A Dock click, ⌘Tab or
  activation that lands on a window of another Desktop shows that Desktop with **that
  window's stage** active.
- Switching Spaces slides the whole screen (2609 pt in about 230–310 ms at 5K); the new
  Space's strip slides in with it.

Ours:

- Stages, the active stage, strip slots, recency and hotkey numbers are kept per Space. Every
  operation acts on the Space shown; nothing ever moves, raises or focuses a window on
  another Space (that is what made macOS jump Desktops on a card click before 10-12).
- macOS has no public id for a Space, so each gets a synthetic id and is recognized by its
  windows: on a Space change (`NSWorkspace` `activeSpaceDidChangeNotification`) the window
  server's on-screen list (`CGWindowListCopyWindowInfo`, on-screen only: the shown Space's
  windows, parked slivers included, never another Space's) is compared with what we know.
  A known Space scores its windows on screen minus its windows missing for no visible
  reason (app not hidden, not minimized); the best positive score wins; else the saved
  Space whose windows match by bundle ID and title; else a new Space.
- `AXWindows` lists only the shown Space's windows for most apps (measured 10-12: Safari,
  Mail, VS Code, Fork, Terminal, Chrome, Messages, Telegram), so a Space's windows are found
  when it is first shown after launch; they are grouped the way turning Stage Manager on
  does (one stage per app) or as `state.json` saved them.
- A window moved to another Space (Mission Control, or dragged across with the pointer)
  leaves its stage and joins the current stage of the Space it shows on. A window shown on
  every Space ("Assign To: All Desktops") is treated as pinned, once it has shown on three
  Space changes in a row, the last back where it came from (a window moved to another
  Desktop and straight back looks the same for two, and stays an ordinary window). A
  window in full screen still belongs to its Desktop: it never counts as missing there.
- Arriving on a Desktop through an app (Dock, ⌘Tab, an activation), its window's stage
  comes at once (no animation: the Desktop's slide just showed it), whatever order macOS's
  events came in. The owner's log of 10-12 (VS Code with a window on each Desktop, his Dock
  click): the window's focus came ~40 ms before the Space change notice and ~0.7 s before
  the Desktop was recognized, and was lost, so the Desktop came up with its previous stage.
  Now a focus or activation that cannot act yet (its window on a Desktop not shown, not
  known yet, or a Space change settling) is held for up to 3 s
  (`engine::focus_replay`); on arrival AX is asked for the focused window now and, when that
  names no window of the Desktop shown, the held one is replayed. A card click, a hotkey or
  a focus that acted forgets it.
- The owner's log of 10-13 (the same case, after 6R): the focus at +0 ms (held, its Desktop
  known), the Space change notice at +4 ms, the Desktop recognized at +64 ms (every app's
  window list read again first), the switch sent at +80–86 ms, not animated (no transition
  ran; the "stage switch" line counts any switch) but after the slide had begun: he saw the
  Desktop slide in with its previous stage, then the switch. Its strip took over first in
  the old order and the stage left faded into its slot 18 ms later. Now (round 6T) the
  held window's stage goes out **at the notice**, as soon as the window server lists the
  window on screen (its Desktop is the one sliding in), before the window lists are read
  again; the recognition then finds it arranged (nothing moves), raises its windows and
  focuses the window (that window counts as the focused one then, whatever a late
  Accessibility answer says; gone meanwhile, the stage that was there comes back); the
  arriving Desktop's strip is redrawn at the notice too, in its final order. The stage
  left is saved as a switch saves it (the window list's order, its front window as key).
  A focus whose event comes after the notice: the app is asked for its focused window at
  the notice (50 ms Accessibility timeout); a focus held while a change settles is
  arranged for as soon as the window server lists its window. Without a held window the arrival's switch is part of the arrival's own
  batch, and the strip gets one snapshot for it. Either way the arriving strip is laid out
  at once (no glide, no fade).
- Measured live on 10-13 (the hands-free Spaces check): macOS carries out moves of windows
  on a Desktop not shown (a window of Desktop 1 moved 40 pt while Desktop 2 showed), and
  every public notice of a Dock arrival comes at the **end** of the slide: from an `open`
  activation the arriving Desktop's windows were on screen from +90 ms and the leaving
  one's until +327 ms; the activation ~+324 ms, the notice +346 ms, the arrangement +363 ms,
  the recognition +399 ms; the stage left showed through the whole slide. The owner's Dock
  click: the focus +419 ms and the notice +423 ms after the mouse-down. So the notice
  arranges too late to be seen, and the arrival is arranged **at the Dock click** (round
  6U): a left mouse-down over the Dock's own window is hit-tested through public
  Accessibility (`AXUIElementCopyElementAtPosition` on the Dock, an `AXDockItem` of
  subrole `AXApplicationDockItem` or one of its parents, its `AXURL` the app; a miss is
  logged with what was found), and the click's prediction is worked out then, at the
  press, so the release only arranges; let go over the same item soon
  (under 0.6 s, the Dock's menu comes on a held press) and without moving (6 pt) or a
  modifier, it is a click. When macOS will switch Desktops for it ("When switching to an
  application, switch to a Space with open windows for the application" on,
  `com.apple.dock` `workspaces-auto-swoosh`, absent = on; the app running, with no window
  on the Desktop shown in the model nor any on screen out of the park zone in the window
  server's list, none unplaced, not hidden: on the safe side, no prediction when unsure;
  the model's gates and the list are looked at before the app is asked anything), the
  window macOS will show (the app's focused window when it waits on another
  Desktop, else its frontmost there in the window server's order) has its Desktop arranged
  at once, through the cached Accessibility elements of that Desktop's windows, and its
  strip pane redrawn in its final order: the Desktop slides in arranged. Its own moves
  make the window's late focus an echo; a focus of another known window of that app still
  counts (macOS showed another window than predicted: undone, arranged for that one at the
  notice). No Space change
  within 1 s of the click undoes it. ⌘Tab keeps the notice's arrangement for now: no
  public notice names the app chosen before its activation; the switcher is drawn by the
  Dock, and whether its Accessibility tree exposes the selected app while ⌘ is held (to
  read at the ⌘ release) has not been looked at live. A wrong
  prediction (another Desktop recognized, the user acting on the Desktop shown, another
  known window or another app focused, a full-screen Space, no Space change within the
  hold, the window not on screen at the notice) gives the Desktop back
  its previous stage, slots and recency; windows brought back on a Desktop not shown stay
  in the safety registry until seen there.
- Card clicks, hotkeys and ⌘Tab focus a window by raising it and making it main through
  Accessibility before bringing its app forward (never all its windows), so macOS has no
  reason to switch Spaces.
- While a Space change settles (a full-screen animation waited out), card clicks, hotkeys,
  wallpaper clicks and ctl stage commands do nothing (ctl answers "a Space change is
  settling"): the model still shows the Desktop being left.
- On a full-screen Space the strip is not shown and no stage changes. Entering or leaving
  full screen, macOS shows a short-lived stand-in window covering the whole display (no
  title, subrole `AXUnknown`) while the app's window animates, and the Space change comes in
  the middle of it (measured 10-12: taken at face value, each toggle minted a new Desktop and
  merged the Desktop's stages). A Space change while a window covers the display without any
  window reporting `AXFullScreen` waits for the animation to settle (at most 2 s); a window
  appearing then is not managed; a full-screen Space is known by a window on screen in full
  screen. The Desktop keeps its id, stages and windows.
- Each Desktop has its own strip panel, made on the Desktop's first visit while it shows;
  it stays on that Desktop and slides with it, with the cards it had, as Apple's does. A
  window that does not join every Space stays on the Space it was first ordered in (ordering
  it out and in again does not move it; measured 10-12: a single panel moved around stayed
  on Desktop 1, so a card click on Desktop 2 found nothing there). Which panel a Desktop has
  is read from the panels (`isOnActiveSpace`), not from our synthetic Space ids; two on one
  Desktop (a Desktop removed in Mission Control hands its windows to another) are cut to
  one. A full-screen app's Space has none.
- Mission Control, App Exposé and Show Desktop hide every panel of ours, as Apple's strip
  hides (owner, 10-12). AppKit's collection behavior has a Spaces choice and an Exposé
  choice, set independently: ours keep the Spaces choice (none for a Desktop's pane and its
  edge tab: the one Desktop they are first ordered in on, never a full-screen app's Space)
  and take `Transient` for Exposé ("hidden in Exposé"), where they were `Stationary`
  ("unaffected by Exposé", so drawn over Mission Control). `ctl state` lists each pane's
  behavior bits; `scripts/test-stages.sh expose` and the Spaces check's Mission Control
  round check it live (Mission Control and App Exposé typed with the user's own shortcuts,
  left with Escape, never a Desktop change).
- While Mission Control, App Exposé or Show Desktop shows, the window server lists windows
  the Desktop does not hold (App Exposé: the app's windows of other Desktops; Show Desktop:
  the Desktop's windows slid off the display), so nothing is inferred from the list then and
  for a second after (`engine::overview`; they are told by macOS's own windows covering the
  display, measured 10-12: the Dock's at layer 20 and WindowManager's at 19 for Mission
  Control and App Exposé, WindowManager's at 18 for Show Desktop). WindowManager's window
  tells them: the Dock's full-display window alone is an auto-hiding Dock brought up by the
  pointer (measured 10-12 on the owner's Mac: listed 2.9 s while the pointer rested on the
  bottom edge, no WindowManager window with it; before round 6R every Dock click to another
  Desktop was read as Mission Control and held the arrival), and it only carries a Mission
  Control already shown through its exit (at most 1.5 s). Windows of another
  Desktop on screen count only when seen at two checks a second apart. A Desktop clicked in
  Mission Control comes with its notification and is recognized once Mission Control has
  closed.

## Grouping (`AppWindowGroupingBehavior`)

- **All at once (1, his setting):** activating an app shows all of its windows in that
  stage. A new window of an app joins its app's stage (above). First start: one stage per
  app holding all its windows.
- **One at a time (0):** only the app's most recent window is shown; its other windows stay
  in the strip, and clicking the app on the left again opens its next window (Apple's
  guide). Ours: the app's waiting windows share one card (in the slot of the first), whose
  click shows the app's least recently shown window, so clicks cycle through them all. Ours: activating such an app switches to the stage holding its most
  recently focused window only. First start: one stage per window. A new window of an app
  in the current stage joins it; a new window of an app that is only in parked stages opens
  a stage of its own (the app's newest window, shown alone, as activating it would). Shift
  on a Dock click brings only the app's most recent window here.

## Intentional deviations

1. **Window motion is drawn with snapshots.** The Accessibility API cannot scale a live
   window, so switches are animated with snapshots in a click-through overlay above every
   window (`[animation]`, on by default). At a switch the outgoing windows' snapshots
   appear exactly over them, the windows park behind them, and the snapshots shrink into
   the slot of their stage's card; the incoming windows' snapshots grow from their card to
   their frames, and the windows come back behind them at 99 % of the motion
   (`unpark_at`, ~400 ms); the snapshots go and the outgoing stage's card appears in the
   same frame once they have landed. The incoming snapshots start within a frame or two of
   the mouse-up; the outgoing ones hold still over their windows until those have parked
   (`cover_ms` plus the parks, about two to four frames), so no real window ever peeks out
   from behind a smaller snapshot. The desktop reveal, ⌘M, a window dropped on the strip
   and a card dragged out land the same way. What remains: snapshots are stills (a full-size
   snapshot of each shown window is kept fresh every `snapshot_interval_ms`, 2 s, and at
   once when the pointer reaches or presses a card or ⌘ goes down for the app switcher, and
   a window's last one before it parks is what grows back), so content that changed since
   jumps when the motion ends; and during the ~400 ms an incoming window is not yet
   clickable (a click or key press brings every incoming window in at once, and a click
   where one is arriving is never taken as a wallpaper click).
2. **Thumbnails are snapshots**, refreshed every 1–2 s through ScreenCaptureKit, not live
   window surfaces.
3. **Parked windows leave a sliver** in the display's bottom-right corner (macOS keeps part
   of every window onscreen; another corner when a second display sits beyond that one). Apple moves them off by private means.
4. **Spaces are recognized, not named.** No public API names a Space, so Spaces are told
   apart by their windows: an empty Desktop is forgotten when left, and a Desktop whose
   windows are all hidden or minimized is taken for a new one until one of them shows. The
   strip of a Desktop visited before slides in with it, as Apple's does, showing the cards
   it had when left until the new snapshot updates them; on a Desktop's first visit its
   strip appears once the Space has slid in (its panel is made then).
5. **Drag and drop stand-ins.** The Accessibility API cannot scale a live window, so a
   window dragged over the strip keeps its size under a card-sized snapshot of it (turned
   like a card, under the pointer, above everything) that stands in for Apple's shrinking
   window; the real window stays visible to the right of the column. A card dragged out of
   the strip is shown by a ghost (a snapshot of its front window in a click-through panel)
   that grows to the window's size, instead of the live window growing. Drop outcomes match
   Apple's, in the slot under the pointer. (Before 10-12 the target felt "fairly far": the
   placeholder always opened below the last card wherever the pointer was, a full strip then
   put the new card in the least recently used card's slot, a third place, and the pointer
   was polled every 60 ms; now the gap and the drop follow the pointer's slot on every drag
   event.)
6. **Desktop items** (`HideDesktop`) are read and logged but not acted on, since hiding
   Finder's desktop icons is outside a window manager's reach through public API.
7. **⌘M** parks the window as a stage of its own and then un-minimizes it there, so macOS
   may show its restore animation toward the bottom-right corner; Apple's window shrinks
   straight into the strip.
8. **Optional extras, all off by default:** windows kept clear of the strip (`[windows]
   fit`, with exact frames back on quit), the edge tab and the launch reveal (`[reveal]`),
   number badges and names, hotkeys.
9. **Extensions** on top, all optional and never replacing a native gesture: rules for new
   windows, placement (center, cascade), hotkeys, named stages, pinned windows, persistence,
   number badges on cards, and Shift sugar (on by default, invisible unless Shift is held) ("hold Shift to bring things here": Shift while
   opening an app, on a Dock click, or on a card click). Shift is read at the user's
   triggering click or key press (a global monitor) and at the moment the window or
   activation arrives; a trigger with ⌘ held (the app switcher) never counts.

## Still to measure

The edge-reveal and auto-hide delays (the laptop session's slide-ins followed window moves,
not the edge), a hover's exact easing (ours: ease-out over the measured duration), a new
card's fade, Shift-click on a card holding several apps (ours
merges the whole card), Apple's thumbnail shadow and corner radius beyond what the
screenshots show (ours: about 4 pt each), and the moving windows' shadow during a switch
(ours: `[animation] shadow_*`, a guess).
