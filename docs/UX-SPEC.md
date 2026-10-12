# UX spec: Apple's Stage Manager, observed, and how we match it

Measured on 2026-10-12, macOS 27.0.1, one 5K display (2560 × 1440 pt, scale 2, menu bar
30 pt), with the owner using Stage Manager live. Method: public settings
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

## Strip geometry

- **Position:** a vertical column of cards on the left of the main display. Card left edge
  at **x = 15 pt**. The widest card reached x ≈ 181 pt, so the strip occupies about
  **0–200 pt**.
- **Cards** keep each window's aspect ratio inside a box of about **150 × 177 pt**: seen
  sizes 85 × 124, 99 × 110, 117 × 153, 135 × 160, 149 × 158, 130 × 173.
- **Tilt:** each card is turned in perspective about the vertical axis, near (left) edge
  toward the viewer. The far (right) edge is about **92 %** of the near edge's height, and
  the top edge falls about 10° left to right. We reproduce this with a `CATransform3D`
  y-rotation and perspective, tuned to that ratio.
- **Size by screen (ours, by formula):** every strip length (column width, card box, gaps,
  icon size and offsets, hover shift, perspective distances) is multiplied by
  `k = (visible width / 2560) × (visible height / 1410)`, kept within 0.5…1 (the 5K
  numbers above at `k = 1`; larger screens keep them; the floor keeps cards legible).
  Capacity is `floor((visible height − 2 × 190 k) / (170 k))`, within 3…9. Apple draws
  smaller cards on laptops; its exact sizes there are not measured yet.

  | Screen (visible frame) | k | Column | Card box | Capacity | Stage area starts at |
  |---|---|---|---|---|---|
  | 5K, 2560 × 1410 pt | 1.00 | 200 pt | 170 × 177 pt | 6 | x = 216 pt |
  | Laptop, 1800 × 1130 pt | 0.56 | 113 pt | 96 × 100 pt | 9 | x = 129 pt |
- **Count:** at most **6 cards** at 1440 pt height. When a seventh stage enters the strip,
  the oldest card drops out of view (its windows stay parked).
- **Vertical layout:** cards are stacked top to bottom with about **12–25 pt** between card
  boxes. Positions shift so the column stays roughly centered in the visible height. Seen
  card tops: 231, 406, 557, 728, 913, 1062 pt.
- **App icon:** each card shows its app's icon (about **32 pt** visible, with shadow) over
  the card's bottom-left corner. The icon's left edge is about 5 pt left of the card and its
  bottom about 10 pt below the card. A second app on the same card gets a second icon
  **36 pt** to the right.
- **Multiple windows on one card:** windows of one app are stacked with about **17 pt**
  horizontal offset, the front one largest or equal. A second app's window sits **68 pt**
  right and **14 pt** up from the first. Up to 3 are shown.
- **Hover:** the card under the pointer grows about **6.5 %** (135 × 161 → 144 × 170) and
  moves 5 pt left, easing in over about 150 ms.

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
  every Space has its own strip.

## Interactions

| Gesture | Native behavior (observed unless noted) | Ours |
|---|---|---|
| Click card | Switch to that stage; swap slots (above) | Same |
| Shift-click card | Not captured in the session (owner's brief: adds the card's windows to the current stage) | Implemented per the brief (sugar: "Shift brings things here"); to be checked against native |
| Drag card into main area | The window grows to full size as it leaves the strip, follows the pointer, and joins the **current** stage on drop | Same gesture: a press that moves more than 4 pt lifts the card; a ghost of its front window follows the pointer and grows from thumbnail to window size over the 260 pt past the strip's edge, the grab point sliding into the title bar. Released in the main area, the stage's windows join the current stage and the front window lands with its title bar under the pointer (clamped onto the screen); the others keep their saved frames. Released over the strip: cancelled. See deviation 5 |
| Drag window onto strip | The window shrinks to card size while over the strip; on drop it becomes **its own stage** in a strip slot (settles in about 400 ms) | Same outcome: while a managed window is dragged by its title bar, the strip slides in once the pointer is over the strip area and shows a highlighted placeholder card in the next slot (the other cards make room). Released there, the window becomes its own stage, its card takes the slot, and its pre-drag frame is its saved frame. See deviation 5 |
| Dock click or ⌘Tab to an app in the strip | Switches to that app's stage; the outgoing stage takes the vacated slot | Same |
| Shift + Dock click on an app in another stage | (No native meaning) | Sugar: the app's windows come to the current stage instead (all of them; the most recent with One at a Time) |
| Open an app with Shift held (Dock, Finder, Spotlight Shift-Return, Launchpad) | (No native meaning; the app opens in a new stage) | Sugar: its new window joins the current stage |
| ⌘Tab / ⌘⇧Tab | Switches stages (⌘⇧Tab cycles backward) | Same; Shift never changes the app switcher |
| Click wallpaper | **Reveal desktop:** the active stage goes into the strip and the main area is empty. A second wallpaper click brings it back | Same, following `EnableStandardClickToShowDesktop` |
| Window overlapping the strip area | Strip slides out of the way (not captured in the session; Apple's documented behavior). Apple keeps the stage's windows clear of the strip when it places them | When we place a window (first start, a stage shown, a new window, a card dropped in) that overlaps the strip column plus a 16 pt margin, it moves right just enough; one too wide is also narrowed (height kept; `[stages] fit_windows = false` only moves). Pre-fit frames come back on quit. The strip hides while any window of the active stage intersects the strip area (a window the user drags there stays) |
| Pointer on the edge tab (ours) | (Apple has no tab) | While a window hides the strip, a slim translucent tab (10 × 72 pt window, 5 pt pill, light/dark aware) sits at the left edge, vertically centered: resting on it reveals the strip after 250 ms like the edge, a click reveals it at once. Hidden when the strip shows or the user turned the strip off |
| Pointer at the left edge | Strip slides back in (delay not captured) | Reveal after the pointer rests at x ≤ 2 pt for **250 ms** (provisional); hide again 500 ms after the pointer leaves the strip while a window still overlaps |

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

## Grouping (`AppWindowGroupingBehavior`)

- **All at once (1, his setting):** activating an app shows all of its windows in that
  stage. A new window of an app joins its app's stage (above). First start: one stage per
  app holding all its windows.
- **One at a time (0):** only the app's most recent window is shown; its other windows stay
  in the strip as cards. Ours: activating such an app switches to the stage holding its most
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
4. **One Space.** Stages live on the Space where the app runs; other Spaces are left alone.
5. **Drag and drop stand-ins.** The Accessibility API cannot scale a live window, so a
   window dragged over the strip keeps its size; a highlighted placeholder card in the slot
   where its card will go stands in for Apple's shrinking window. A card dragged out of the
   strip is shown by a ghost (a snapshot of its front window in a click-through panel) that
   grows to the window's size, instead of the live window growing. Drop outcomes match
   Apple's.
6. **Desktop items** (`HideDesktop`) are read and logged but not acted on, since hiding
   Finder's desktop icons is outside a window manager's reach through public API.
7. **Windows narrowed to fit.** Apple keeps windows clear of the strip; we do the same by
   moving them, and by narrowing a window too wide for the room beside the strip (Apple's
   exact behavior for such windows is not measured). Every narrowed window gets its exact
   earlier frame back on quit or after a crash (`state.json` keeps the pre-fit frame).
8. **Edge tab and launch announce.** A slim tab at the left edge while a window hides the
   strip, and a 3 s showing of a covered strip right after launch, so the strip is never
   invisible without a hint. Apple has neither.
9. **Extensions** on top, all optional and never replacing a native gesture: rules for new
   windows, placement (center, cascade), hotkeys, named stages, pinned windows, persistence,
   number badges on cards, and Shift sugar ("hold Shift to bring things here": Shift while
   opening an app, on a Dock click, or on a card click). Shift is read at the user's
   triggering click or key press (a global monitor) and at the moment the window or
   activation arrives; a trigger with ⌘ held (the app switcher) never counts.

## Still to measure

The edge-reveal and auto-hide delays, and shift-click, were not captured. One more short
session with Apple's Stage Manager on (about 1 minute) would pin them down.
