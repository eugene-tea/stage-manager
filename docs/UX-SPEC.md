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
| Shift-click card | Not captured in the session (owner's brief: adds the card's windows to the current stage) | Implemented per the brief; to be checked against native |
| Drag card into main area | The window grows to full size as it leaves the strip, follows the pointer, and joins the **current** stage on drop | Out of scope (drag and drop); `⌃⌥⇧N` sends a window to a stage |
| Drag window onto strip | The window shrinks to card size while over the strip; on drop it becomes **its own stage** in a strip slot (settles in about 400 ms) | Out of scope; `⌃⌥N` moves the focused window to a new stage |
| Dock click or ⌘Tab to an app in the strip | Switches to that app's stage; the outgoing stage takes the vacated slot | Same |
| Click wallpaper | **Reveal desktop:** the active stage goes into the strip and the main area is empty. A second wallpaper click brings it back | Same, following `EnableStandardClickToShowDesktop` |
| Window overlapping the strip area | Strip slides out of the way (not captured in the session; Apple's documented behavior) | Strip hides while any managed window intersects the strip area |
| Pointer at the left edge | Strip slides back in (delay not captured) | Reveal after the pointer rests at x ≤ 2 pt for **250 ms** (provisional); hide again 500 ms after the pointer leaves the strip while a window still overlaps |

## Grouping (`AppWindowGroupingBehavior`)

- **All at once (1, his setting):** activating an app shows all of its windows in that
  stage. A new window of an app joins the active stage by default.
- **One at a time (0):** only the app's most recent window is shown; its other windows stay
  in the strip as cards. Ours: activating such an app switches to the stage holding its most
  recently focused window only.

## Intentional deviations

1. **No window animation.** Windows jump to their frames; the Accessibility API cannot
   animate windows smoothly (out of scope for the prototype). The strip's own cards may
   animate on the measured spring.
2. **Thumbnails are snapshots**, refreshed every 1–2 s through ScreenCaptureKit, not live
   window surfaces.
3. **Parked windows leave a sliver** in the display's bottom-right corner (macOS keeps part
   of every window onscreen). Apple moves them off by private means.
4. **One Space.** Stages live on the Space where the app runs; other Spaces are left alone.
5. **Drag and drop** between strip and main area is not implemented; hotkeys cover it.
6. **Desktop items** (`HideDesktop`) are read and logged but not acted on, since hiding
   Finder's desktop icons is outside a window manager's reach through public API.
7. **Extensions** on top, all optional: rules for new windows, placement (center, cascade),
   hotkeys, named stages, pinned windows, persistence, number badges on cards.

## Still to measure

The edge-reveal and auto-hide delays, and shift-click, were not captured. One more short
session with Apple's Stage Manager on (about 1 minute) would pin them down.
