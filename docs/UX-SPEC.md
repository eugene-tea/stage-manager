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
| Click wallpaper to reveal desktop | `EnableStandardClickToShowDesktop` | absent (default) | Click wallpaper toggles desktop reveal |
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
| Edge tab, launch reveal | none | off | `[reveal] edge_tab`, `launch_reveal` |
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
  168 × 171, both edges moving), in about **200 ms**, eased out.
- **Slide.** Hiding moves the cards **286 pt** left (x 15 → −271) on the same spring as the
  transitions (50 % at ~100 ms, 90 % at ~220 ms, settled by ~500 ms); showing reverses it.
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

## Transitions

- **Trigger:** a card click acts on **mouse-up**. Motion starts within one frame.
  Activations from the Dock or ⌘Tab start about **110–200 ms** after the input, after the
  app activates.
- **Curve:** incoming windows grow from their card to their saved frame along a
  **critically damped spring, ω ≈ 15.5 rad/s**. Progress is about 2 % at 20 ms, 14 % at
  55 ms, 50 % at 110–120 ms, 90 % at 250 ms and 99 % at 400–430 ms. It is fully settled by
  about 500 ms, with no overshoot. Outgoing windows shrink into their slot on the same curve.
  As a `CASpringAnimation`: mass 1, stiffness 240, damping 31.
- **Other cards** glide to their new slots on the same curve.
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
| Drag card into main area | The window grows to full size as it leaves the strip, follows the pointer, and joins the **current** stage on drop | Same gesture: a press that moves more than 4 pt lifts the card; a ghost of its front window follows the pointer and grows from thumbnail to window size over the 260 pt past the strip's edge, the grab point sliding into the title bar. Released in the main area, the stage's windows join the current stage and the front window lands with its title bar under the pointer (clamped onto the screen); the others keep their saved frames. Released over the strip: cancelled. See deviation 5 |
| Drag window onto strip | The window shrinks to card size while over the strip; on drop it becomes **its own stage** in a strip slot (settles in about 400 ms) | Same outcome: while a managed window is dragged by its title bar, the strip slides in once the pointer is over the strip area and shows a highlighted placeholder card in the next slot (the other cards make room). Released there, the window becomes its own stage, its card takes the slot, and its pre-drag frame is its saved frame. See deviation 5 |
| Dock click or ⌘Tab to an app in the strip | Switches to that app's stage; the outgoing stage takes the vacated slot | Same |
| Shift + Dock click on an app in another stage | (No native meaning) | Sugar: the app's windows come to the current stage instead (all of them; the most recent with One at a Time) |
| Open an app with Shift held (Dock, Finder, Spotlight Shift-Return, Launchpad) | (No native meaning; the app opens in a new stage) | Sugar: its new window joins the current stage |
| ⌘Tab / ⌘⇧Tab | Switches stages (⌘⇧Tab cycles backward) | Same; Shift never changes the app switcher |
| Click wallpaper | **Reveal desktop:** the active stage goes into the strip and the main area is empty. A second wallpaper click brings it back | Same, following `EnableStandardClickToShowDesktop` |
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
  → Mission Control). Stage Manager follows; it never fights it.
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
  every Space ("Assign To: All Desktops") is treated as pinned.
- Card clicks, hotkeys and ⌘Tab focus a window by raising it and making it main through
  Accessibility before bringing its app forward (never all its windows), so macOS has no
  reason to switch Spaces.
- On a full-screen Space the strip is not shown and no stage changes.
- The strip belongs to one Space at a time: on a Space change it is taken off, its cards
  replaced without animation, and put on the Space shown (deviation 4).

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

1. **No window animation.** Windows jump to their frames; the Accessibility API cannot
   animate windows smoothly (out of scope for the prototype). The strip's own cards may
   animate on the measured spring.
2. **Thumbnails are snapshots**, refreshed every 1–2 s through ScreenCaptureKit, not live
   window surfaces.
3. **Parked windows leave a sliver** in the display's bottom-right corner (macOS keeps part
   of every window onscreen). Apple moves them off by private means.
4. **Spaces are recognized, not named.** No public API names a Space, so Spaces are told
   apart by their windows: an empty Desktop is forgotten when left, and a Desktop whose
   windows are all hidden or minimized is taken for a new one until one of them shows. The
   new Space's strip appears once the Space has slid in (Apple's slides in with it); the
   old Space's strip slides away with its Space, as Apple's does.
5. **Drag and drop stand-ins.** The Accessibility API cannot scale a live window, so a
   window dragged over the strip keeps its size; a highlighted placeholder card in the slot
   where its card will go stands in for Apple's shrinking window. A card dragged out of the
   strip is shown by a ghost (a snapshot of its front window in a click-through panel) that
   grows to the window's size, instead of the live window growing. Drop outcomes match
   Apple's.
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
not the edge), a hover's exact easing, Shift-click on a card holding several apps (ours
merges the whole card), and Apple's thumbnail shadow and corner radius beyond what the
screenshots show (ours: about 4 pt each).
