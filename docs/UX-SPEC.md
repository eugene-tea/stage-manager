# Stage Manager — design notes

## The baseline

Out of the box, Stage Manager behaves like the Stage Manager built into macOS. Someone who
knows Apple's should notice no difference until they reach for an extra. Every extra is
off, or invisible until used, and every number is a config key whose default is Apple's.

Apple's behavior was learned by observation only: using it, reading its public settings,
watching the window list and taking screenshots. No Apple binary was inspected, and no
Apple artwork is in the repository. Where Apple's documentation is silent and nothing was
measured, the notes below say so. The full measurement record (method, raw numbers and fits)
is this file as of commit 20665a3.

## macOS settings come first

Where macOS already has a setting, Stage Manager reads it and follows it live. The config
file never repeats one.

| System Settings → Desktop & Dock | What Stage Manager does |
|---|---|
| Stage Manager on or off | The two cannot run together. While Apple's is on, Stage Manager steps aside: every window comes back, the strip hides and nothing is arranged. It offers to turn Apple's off, and changes the setting only when you click. |
| Show recent apps | Off hides the strip until the pointer reaches the left edge. |
| Show windows from an application | All at Once or One at a Time, as below. |
| Click wallpaper to reveal desktop | With "Only in Stage Manager", a wallpaper click puts the stage into the strip, as Apple's does. With "Always", macOS itself moves every window aside (Apple's Stage Manager is off while ours runs) and Stage Manager stays out of the way: macOS answers that click before any other app can, and only that setting stops it, which Stage Manager never writes. Settings says so beside its button to System Settings. |
| Show items on desktop | Read, but not acted on (see the differences). |

Settings changes "Show recent apps" and "Show windows from an application" the same way
Apple's own Customize sheet does.

## Where we match Apple

**Windows keep their frames.** Turning Stage Manager on moves and resizes nothing. A switch
brings every window back at exactly its own frame, even where it overlaps the strip, hangs
off an edge or is taller than the screen. Only a window that could not be grabbed moves: its
title bar comes below the menu bar and above the Dock with at least 80 points of it on
screen, and one left in the hiding corner comes back just out of it.

**The strip.** It sits below windows: a window moved over the cards covers them and the strip
stays up under it. It slides away only once a window of the front stage covers the cards'
left edge, its own left edge within 20 pt of the screen's, as Apple's does (recorded on a
Studio Display: Apple's stayed up under a window dragged left over the cards to 52 pt, and
slid away when it was dragged on to 15 pt, its cards starting at 16 pt; the point between
was not measured; a laptop's windows nearly fill the screen, so there it goes more often).
Each screen's strip follows its own front stage. Resting the pointer at the left edge brings
it back: Apple's starts sliding in about 180 ms after the pointer gets there (measured), ours
rests 130 ms and checks the pointer every 30 ms (`[reveal] delay_ms`, `poll_ms`). Each card
draws the stage's windows at one scale, turned in perspective toward the middle of the
screen, with the app icon on its bottom-left corner. The number of cards depends on the
screen height: 5 on a laptop, 6 on a 5K screen.

**A card's look** (measured on the 11-09 stills of a 5K display, 2 px a point). The front
window is the stage's last used, drawn on top. Windows of one app: the one behind peeks out
16 pt on the left (`[cards] stack_dx`; 17 read on the laptop's window list), each at its own
card size (Red 1 and Red 2, 1000 × 706 and 900 × 628, both 97 pt tall, tops level, either
one in front). Several apps: the card stays about a single card's size. Blue (1100 × 748)
behind and Green (1000 × 688, right of it and lower on the screen) in front: every window at
the plain scale, none held at 97 pt (Blue 85 pt tall, Green 78), Green's left edge 42 pt
right of Blue's (`second_app_dx`), Green's top 18.3 pt below Blue's (the two centered on one
line, the window behind 15 pt up: `second_app_dy`), Green's bottom 96 pt below Blue's top
(the whole card is held at `min_height`, its windows grown, their offsets kept), Green's far
edge at 128.5 pt where Blue alone reaches 105. The window behind peeks out on the side it
sits on the screen: two of the owner's cards with the other app's window right of the front
one showed it peeking out on the right (23 and 47 pt; up 34 and 13 pt, so Apple's offsets
vary with the windows; ours are the fixture's). A card of three apps (measured 11-09, Red 1
in front, Green, Blue): the second app's window where a card of two puts it (Green on the
left, its left edge 42 pt out and 15 pt up), the third's behind both at the top, its left
edge 12 pt toward the second's side and 24 pt up (`third_app_dx`, `third_app_dy`: 0.29 and
1.6 times the second's, read on the hovered card against a card of two hovered the same
way); a fourth app's windows join the third's (not measured). The icons sit 10.5 pt right of the card's left edge and 10.5 pt above its
bottom (the lowest window's: 10 pt above Green's bottom, at Blue's), and stay put while the
card grows. On a card of two apps the second icon sits 36 pt right of the first (`[icons]
step`); on a card of three they overlap, 18 pt apart (`step_three`, measured 11-09). They are the variants the Dock shows (macOS 26's icon and widget style: Default,
Dark, Clear or Tinted, light, dark or following the appearance): in Dark Mode Apple's cards
showed Mail's, Finder's, Messages' and Safari's dark icons (11-09). Ours fetches each icon per
look (`ui::app_icons`) and again at an appearance change. Corners 8 pt (`corner_radius`; fitted to the rounding of three cards' top-left
corners, ours read 4 at 4). The card's shadow is barely there: 5 to 8 levels darker in the
first two pixels below a card, gone by 4 (2 pt), 3 to 6 levels for a pixel beside its far
edge, none above it; a Core Animation render of a card fitted to the bottom, top and far
edges of two cards gives `shadow_radius` 1, `shadow_opacity` 0.3, `shadow_offset_y` 1 (the
same render read our old 4 / 0.4 / 2 within a level). The icon's shadow is faint and tight:
measured 11-09 evening against Apple's in Dark Mode on the same wallpaper (the owner's two
screenshots), below the icon over the wallpaper it darkens by about 30 % at the edge, 16 %
three pixels down, and is gone by 8 (4 pt): `[icons] shadow_radius` 2, `shadow_opacity` 0.3,
`shadow_offset_y` 1, within 2 to 3 % of Apple's from three pixels down. The morning's fit on
the card under the icon (3, 0.6, 1.5) read twice as dark and twice as wide on the wallpaper. Hovered, each
window of a card grows by about 11 % about its own middle and the offsets between them stay
(a single card 178 → 199 pt wide; the pair's offsets 42 and 18 pt both ways, the pair's
bounds 7 % wider, 9 % taller; Blue's and Green's middles moved under half a point), and the
app icons neither move nor grow (their boxes the same to the pixel on the 2x stills of a
single card and of the pair). Ours does the same, and grows a card a dragged card would
merge into and shrinks a pressed one the same way (Apple's press not measured). The growth
goes with the card: dragged in the column or gliding to its slot, each window grows about
its own middle where the card is drawn, so the card moves as one piece, its windows on its
icons, as Apple's does (11-09: while the growth from the press back to the hover size still
ran, a card dragged 250 pt down was drawn 27 pt off its icons, then jumped back). A popup open on a window (a popover, a child window sticking out
of it) shows on its card beside the window, at its true size and place, as on Apple's; the
window keeps its own size on the card, and nothing outside a window is drawn. A palette
far from its window (more than the window's own width or height past it, or on another
screen) is left off: the card shows the window alone. So is a popup over a window that
macOS's capture does not place (a calendar's event popup reaching past the window's top):
never a fill around a window on a card.

**Cards let the wallpaper through**, as Apple's do (measured): each window's picture is
drawn at 78 % over a flat colour of its own,
the wallpaper's average behind the thumbnail at 95 % of its saturation and 78 % of its
brightness (`[cards] window_opacity`, `backdrop_saturation`, `backdrop_brightness`).
`scripts/measure-apple-card-glass.py` read one of Apple's cards with its window flat white,
black and grey: the opacity 0.78 in two runs, the colour behind within a level of the model,
the grey a blend in the display's encoded values; the hovered card read fully solid. Apple's
colour is flat over a card where the wallpaper behind it changes, so it is an average, not a
blur. Not measured yet: dark mode, and what Apple shows through a window behind
another on one card (here each window has the wallpaper behind it). A switch's snapshots
take the card's look as they shrink into it and leave it as they grow into their windows.
The card under the pointer turns solid as it grows, as Apple's does: its pictures ease to
`[cards] hover_window_opacity` (1) on the hover's own curve and time, and back as it shrinks
on leave; a card a dragged card would merge into does the same, as it grows. A click on it
hands the snapshot over at the look drawn that moment (solid, or part way), so nothing jumps;
a card the snapshots land in under the pointer shows at rest, then grows and turns solid
together. Apple's turns solid on the growth's own curve (measured: the windows' opacity and the
card's edge fit the same spring, ω 16.8 and 16.6). The wallpaper is drawn from its file,
as the clean corner's is; where it cannot be (a shuffle, an unreadable picture), the cards are
opaque.

**Strip order is slot-stable.** A clicked card's stage becomes active, and the stage you
leave takes the slot the clicked card vacated. A stage entering a full strip takes the slot
of the least recently used card. (Keep stages in place, an extra below, orders it otherwise.)

**Switching.** The pressed card eases back on mouse-down. On release, the windows you leave
shrink into their card's slot and the new stage grows from its card, on Apple's spring and
along Apple's path (measured, below). Both start on the same frame, as Apple's do. The path is
not a straight line: a window growing out of its card goes out sideways, widens and flattens
ahead of the spring while it rises and gains height on it, so it seems to come up from the
card's bottom; one shrinking into its card drops and loses height first, staying wide and
flat, then slides into the card and turns. In flight a window turns about its vertical
middle, so its left side comes toward you as it turns.
Shift-click a card to add its front window (its stage's last used, drawn in front) to the
current stage, in front with the keyboard, whatever part of the card the pointer is on; the
rest of the card stays in the strip in its slot (Apple's, measured 11-09 on a card of two
apps, whichever app's part was clicked, and on a card of one app's two windows). A card of
one window brings its stage in whole. Option-click is a plain click, as on Apple's (checked
11-09: no app is hidden).
Clicking a card's app icon brings every window of that app, from all stages.

**Activation.** ⌘-Tab, ⌘⇧-Tab or a Dock click to an app in another stage switches to that
stage, animated as a card click: the stage you leave shrinks into the strip and the app's
windows grow from their card. The keyboard goes to the app's window at once. In a stage
shared with other apps, the activated app's window comes in front with the keyboard,
whichever window was focused when the stage was left; a click on a waiting window's edge
does the same for that window.

**New windows.** A window of an app already on screen joins its stage. A window of an app in
a waiting stage brings that stage forward. A newly opened app gets a stage of its own (an
extra, off by default, puts it in the current stage instead: see Extras).
An app that opens several windows at once comes forward with the one it opened last in
front, as macOS shows them, and that one is in front on every switch back to its stage
(Apple's, 11-09; the window used last comes in front, and the keyboard goes to it).
Sheets and dialogs follow their window. Short-lived windows (Safari's small dialogs, Chrome's
address-bar suggestions) join the stage shown and leave it a second later, as any window
does: nothing changes on screen, and holding every new window back to see whether it stays
would slow them all, so they stay managed (decided 11-09). One that shows on a card or
switches a stage is a bug. An app that quits takes its windows out of their
stages and an emptied stage leaves the strip, also when the app quits without macOS's notice.
An app that saved a hidden window's corner place when it quit, and reopens the window there
within a day, has the window put back where it was before the quit (else centered).

A window an app keeps wholly off every screen (a headless browser's, an offscreen render
window) is left alone, as Apple's leaves it: no stage, no card, never moved; it is taken in as
a new window if it ever comes on screen.

An app whose `AXEnhancedUserInterface` an assistive client turned on animates the frame
changes made through Accessibility (Chrome's window slides in from the park corner).
Apple's Stage Manager is no Accessibility client and never shows this; we turn the flag off
around our moves, once per app per switch, and back on after (Rectangle's and yabai's way), on
by default with no key. A write the app answers with an error is read back (Chromium acts on
the value, then may answer with AppKit's error); an app that refuses is left alone for a
while (1 s, doubling to 30 s), not asked at every move.

**Deviation (stale activations):** under load an activation notice can reach us seconds late,
and a late one acted on switches stages back and forth. An activation that fired before the
latest switch we began, or whose app is no longer frontmost when we decide, switches nothing
(logged once with its reason and age); a focus that would switch stages switches nothing when
its app's focused window is in another stage by then. The user's latest ⌘-Tab or Dock click is
neither, so it still switches, late.

**First start.** Every app with windows becomes its own stage, as when you turn Apple's on.
The frontmost app's stage is shown; the others enter the strip, most recently used first.

**All at Once and One at a Time.** With All at Once, a stage holds all of an app's windows.
With One at a Time, only the app's most recent window shows, and clicking its card again
opens its next window.

**⌘M** sends the window to the strip as a card of its own, not to the Dock.

**⌘H** (and Hide Others, or an app hiding itself) takes the app's windows off its cards. A
stage of that app alone leaves the strip entirely: the most recent stage out of view takes
its slot. When it was the stage in front, the most recent stage comes in front, as on a
click. Unhidden without an activation, the stage comes back as a card in the slot it left,
not in front, and the stage in front stays (the card that took the slot goes out of view
when the strip is full). Unhidden by an activation (⌘-Tab, a Dock click), it comes in front
as any activation does. A stage shared with other apps stays, showing theirs (Apple's,
measured 11-09 with an app of two windows alone in front; a shared stage not measured).
**⌘\`** brings the app's other window in front in its stage, and nothing else (checked
11-09).

**Desktops.** Each Desktop has its own stages, active stage and strip, listing only its
windows. The same app can have stages on several Desktops. A card click never changes the
Desktop. ⌘-Tab and the Dock switch Desktops as macOS's own setting says ("When switching to
an application, switch to a Space with open windows for the application"), and the arriving
Desktop shows that window's stage. An app macOS activates as a Desktop slides in (a swipe,
⌃-arrow) never switches a stage on the Desktop being left: it is decided once the new
Desktop is recognized, as Apple's never touches the Desktop left. Full-screen apps have no
strip.

**Screens.** Every screen has its own strip and stages, as Apple's (measured with a 5K main
screen and a laptop's screen to its left, "Displays have separate Spaces" on). Each screen's
strip sits on its left edge, its cards sized and counted for that screen (6 on the 5K, 5 on
the laptop), with its own front stage and cards, sliding away under its own windows. A stage
belongs to the screen its windows are on; a window straddling two screens belongs to the one
showing most of it (macOS's rule).

- **A drag.** A window dragged to another screen starts a stage of its own there, in front,
  once it is let go (nothing changes under the hand, however slow the drag across the
  seam). The stage in front there goes to a card; the screen it left, its front now empty,
  brings its most recent card forward. A window dragged onto either screen's strip becomes a
  stage of its own there. A card dragged from one screen's strip onto the other screen opens
  there, as a drop in a screen's own main area does: its windows (or the one window dragged
  out of a card of several) join that screen's current stage, in front with the keyboard, the
  front one's title bar under the pointer, and the card leaves its strip; the screen it came
  from keeps its front (Apple's, 11-09: a card dragged from the 5K's strip onto the laptop
  opened its window there). Decided, not measured: a window dragged out of a stage of several
  leaves the rest in their stage.
- **Clicks and activations.** A card click changes only its own screen, and so does a click
  on a card's app icon: the app's windows come from that screen's stages. Decided, not
  measured: ⌘-Tab or the Dock to an app whose stage is on the other screen switches that
  screen only. A Dock click or ⌘-Tab to a Desktop on either screen is arranged as on one
  screen (the Dock click ahead of the slide; see Known gaps).
- **A new app** opens on the screen with keyboard focus, as its own stage there, by the
  one-screen rules (macOS picks the screen; the window is taken where it shows).
- **The lid closed.** That screen's front stage comes to the remaining screen as its front
  (its windows where macOS puts them, at their size); the remaining screen's front goes to a
  card, and the closed screen's other stages become cards after the remaining screen's own
  (past the capacity, hidden). Nothing joins: the closed screen's strip goes with it, each
  card is one stage, and its front window shows. The closed screen's hidden windows wait at the remaining screen's corner, those
  macOS pushes whole onto that screen at the change too. macOS may put the closed screen's
  windows on another of the remaining screen's Desktops, one not shown since the launch, and
  slide to it: the same rule holds there, that Desktop's own front a card, also when macOS
  announces the Desktop still shown before it slides; the Desktop left shows its own most
  recent stage again when it comes back, never nothing in front.
- **macOS's activations at a display change.** The app macOS activates after the change
  (Mail, 0.4 s after the screen goes) switches nothing: with no click or key press since, an
  activation or focus in the 10 s after a display change (counted from macOS's notice) is
  macOS's, and the closed screen's front stays in front. A window found then (macOS lists
  windows of the remaining screen's other Desktop for a moment) is placed once the change
  settles, if it still shows; one gone by then leaves no stage or card.
- **The lid opened.** The stages whose windows macOS moves back to that screen are its
  stages again (the one in front before, a card; a stage moved whole stays one stage); the
  stage that came over in front stays where it is. macOS puts each window back at the frame
  it had on that screen, a hidden one at its old corner. A window put back in the open shows
  there at that frame (never one carried over from the other screen), its stage that
  screen's front, also when the user had sent it to a card meanwhile. One put back at the
  corner is that screen's card, also when the user had brought it in front on the remaining
  screen, from any of its Desktops; it is never hidden again at the remaining screen's
  corner. A card put back brings nothing forward; the remaining screen, its front gone back,
  brings its own most recent stage forward. None of it counts as the user's drag.
- **The hidden corner.** Each screen's hidden stages wait in a corner of that screen with no
  screen beyond it (the laptop's bottom-left, its right edge touching the 5K). A window
  showing only a piece at its screen's hidden corner (what shows reaches past the corner's
  side and lies within 120 pt of it and 64 pt of the corner's edge) is never a shown window:
  brought back, it comes wholly on screen in that corner, at its size; found there at a
  launch, or put back there as the lid opens, it is a card, its stage not in front (the front
  an empty one). Apple never leaves a window there. A window hung off an edge elsewhere stays
  where the user put it.
- **Each screen its own.** Each screen's strip samples its own desktop picture for the
  cards' glass; each screen's hidden corner has a patch of its own (a still of that screen's
  desktop where Screen Recording is allowed, else its own wallpaper's file); `[windows] fit`
  keeps a window clear of its own screen's strip; a full-screen app or Mission Control on one
  screen stops nothing on the other.
- **The edge reveal.** Each screen's left-edge reveal is its own: a pointer on a screen to
  the left of a strip never reveals or holds that strip.
- **The main screen changing** (the menu bar moved to the other screen, or the lid closed
  with the laptop main) moves no stage: macOS moves every window with its screen, and each
  hidden window comes back where it was on its screen.
- **`[displays] main_only`** (off) manages the main screen only, a window moved off it left
  alone. It is read at launch: turned on mid-session it would have to bring back every hidden
  window of the other screens, on Desktops not shown too, which a quit does safely.

**Mission Control, App Exposé and Show Desktop** hide the strip, as they hide Apple's. It
is back on the same Desktop when they end. Where Show Desktop pushes a window (while it
shows, expected from a wallpaper click, and for a second after it closes, as the windows
slide back) is never taken as the window's frame: a switch then keeps each window's own. The strip never appears in ⌘-Tab or the ⌘\`
window cycle.

**Wallpaper click.** The active stage goes into the strip, and a second click brings it
back (with "Click wallpaper to reveal desktop: Only in Stage Manager"; with "Always" macOS
moves the windows aside itself, see the settings above). A double click counts as one click, and Finder becoming active after the click never
switches stages. The Dock's window spans its whole screen, clear away from its bar: a click
there is on the wallpaper (or the window) below it, and only one on or near the bar (its
magnified icons included) is the Dock's. So with the screenshot tool's window, which macOS
leaves over the whole screen for minutes after a screenshot and which lets every click
through: a click under it is on whatever is below. Notification Center draws its banners
inside one window over the whole main screen (measured 11-09 on macOS 26: no window of its own
before a banner; with one up, that window at layer 23 is its only one, with no banner-sized
window), so a click under it is on whatever is below only where the Accessibility element at
the point is not Notification Center's: a click on a banner, its close button, its panel, an
alert or the widgets' editing is Notification Center's and reveals nothing (1.8.1 took closing
a banner for a wallpaper click). A press on a file or folder on the
desktop, or its name, is Finder's, as on Apple's: it selects it or starts dragging it, and
nothing is revealed. The icons are the ones read for the corner patch (every 5 s); when none
are that fresh the press waits up to 60 ms for a read, and where they cannot be read at all
the press counts as on the wallpaper. The reveal runs on the release, never on the press, as
Apple's does (measured 11-09 at 60 fps, presses held still on bare wallpaper: held 120 to
460 ms the desktop is revealed, the reveal starting about as much later as the press was held
longer; held 480 ms to 2 s nothing happens at all). So a click reveals only when the pointer
is still where it pressed (within 4 pt) and the press lasted under 470 ms; a press that turns
into a drag (a resize, a rubber-band selection) or is held longer reveals nothing, and no
window is ever parked while a press may still hold its edge. A press just outside a window is
its edge, as on Apple's: macOS gives a resizable window a band outside its frame where a
press takes the edge to resize it (measured 11-09 with no Stage Manager running: up to 3 pt
outside the right edge grabs it, 4 pt doesn't; 3 pt out past a corner diagonally doesn't), so
a press within 6 pt of any window's edge (10 pt near a corner), any stage's, reveals nothing:
twice macOS's band, a margin for other windows.

**Drag and drop.** A card dragged into the main area turns into its window the moment the
pointer leaves the strip's column, and joins the current stage where you drop it. As
Apple's, the window appears at its own size, turned as the card is in the strip's
perspective (which draws it about half as wide at first), its left edge about 76 pt right of
the pointer and its top about 38 pt lower than where it is held; it slides under the pointer
in `[drag] appear_ms` (100 ms, a smoothstep: half way at 50 ms) and comes up as it flattens,
on the spring of windows growing out of the strip (99 % at about 355 ms), whatever the
pointer does. The window is drawn from its own full-size snapshot (asked for at the press),
sharp as Apple's; until that snapshot is at hand the card's thumbnail is never stretched to
the window's size: the card's picture follows the pointer at its card size, and turns into
the window when the snapshot comes. Measured on Apple's two drags (11-09, 60 fps, a 5K screen, 03 a card of one
window and 06 one window of a combined card): its first frame showed the window 424 and
472 pt wide of 990, its top edge sloping as the card's turn draws it, its left edge 34 and
118 pt right of the pointer and its top 42 and 34 pt low; its left edge was under the pointer
after 5 and 7 frames (about half way after 3), its turn halved after about 7 frames and was
gone after 18 to 20 (about 300 ms), so the turn follows the time, not the distance. Apple's
window appeared with the pointer 155 pt and 245 pt from the screen's edge (between the frames
at 148 and 160 pt, and at 240 and 248 pt): no fixed distance from the edge, from the press or
from the card explains both, so ours uses its column's edge (200 pt at the default width),
which lies between them. Apple's window is drawn at its full width after about 140 ms, while
its turn still shows; ours widens with its turn alone, up to about a fifth of the width
narrower on the way, its slope matching Apple's (one spring fits both measures best). Back over the column, the card follows the pointer
again (or a window lifted off a combined card is its card-sized picture again), at once.
The window is held with the pointer 96 pt in from its left edge and 100 pt below its top
(`[drag] window_grab_x`, `window_grab_y`; at most half the window's width and height),
wherever the card was pressed: Apple's put two drags pressed at 47 % and at 71 % of a card's
width at the same spot (measured, 96 and 100 pt, the fixture's 990 × 690 pt window). On a card
of several windows each window drags out on its own, as on Apple's: the press picks the
frontmost window drawn under the pointer (its turned plane, as drawn), the stand-in is that
window's picture and grows to its size, and the drop brings that window alone into the current
stage, in front with the keyboard, held the same way. The stage it left keeps its other windows in their order, its slot and its
number, and its card shows them at once (a stage's only window is the whole card's drop).

How Apple's lifts one window off a combined card (06, 11-09: Green pressed in the Blue + Green
card, 60 fps, a 5K screen): the press squeezes the hovered card a little; as the pointer
starts to move, Blue slides in behind Green and Green stays on the card; nothing follows the
pointer inside the strip. After about 135 pt (16 frames, the pointer about 245 pt from the
screen's left edge) Green comes out as its own window, turned, about half its width, just
right of the pointer; within 6 frames (100 ms) it slides under the pointer and keeps
growing and straightening, full size after about 16 frames, while the card shows Blue alone.
Ours lifts the window off from the first move instead (past `[drag] threshold`, owner 11-09:
with the whole card following the pointer inside the column, "both windows move together"):
its picture leaves the card at the card's size, turned as the card and flattening on the same
spring as a card dragged out, held where it was pressed; its plane on the card fades to
`[drag] lifted_opacity`, the card keeping the others where they are; past the column it turns
into its window as above. Inside the column it can go where Apple's has nothing to
offer (extensions, always on, as a window dropped on the strip): into a gap between cards,
which opens under the pointer, as a stage of its own there (`Model::move_window_to_new_stage`;
the stage it left keeps its number and slot); onto another card's middle (`[strip]
merge_on_drop`, `[drag] merge_zone`), which grows and shows it in front of its own windows,
into that card's stage alone, as its front window (`Model::move_window_to_stage`: first in
its order, raised last and focused when that stage shows); back over its own card, nothing
changes (released there after a wobble that went nowhere, it is a click). The press decides
what moves: on one window of a card of several, that window; on the card's app icons (they
are drawn over the windows), or anywhere on a card of one window, the whole card, which
reorders and merges in the column as below and drops whole in the main area. A window dragged onto the strip
becomes a stage of its own in the slot under the pointer. While it is dragged over the cards
they stay under it, as Apple's do; once the pointer is over the strip, a card-sized picture of
it follows the pointer above everything (Apple's window itself turns into a card there,
recorded with the pointer about 280 pt from the edge on a 5K screen). Apple's took the drop
of a window of another app than the stage's (11-09: Green out of Red's stage made a card) and
sent one of the stage's only app back where it was (Red 2 out of Red's stage); ours makes a
card of either, an extension that keeps Apple's own case as it is.

**Nothing on a card is cut off.** The strip's panel reaches past its column on the right,
as far as the widest card can draw: a combined card's other window (`[cards]
second_app_dx` beside the front one), grown under the pointer, or dragged to the column's
edge with its right part past the pointer, shadows included (the widest card's span at
`hover_scale`, past the column; about 466 pt in all on the laptop, 486 on a 5K screen). The
column alone is the strip for everything that measures it (hit tests of drops, window drags
over the strip, auto-hide, `[windows] fit`); the room right of it lets every click and
scroll through, except over a card drawn there, which hovers and clicks as the rest of it. A
hidden strip slides `[motion] slide_distance` (Apple's 286 pt), or as far as takes the
widest card off the screen whole where that is further (the slide is in 3D, so a card's far
edge moves less than its left: the widest card needs 275 pt on the laptop, 286.4 on a 5K
screen).

**Timing.** Every animation uses Apple's measured timing by default:

| Motion | Apple's timing |
|---|---|
| Windows growing out of the strip: a switch's incoming stage, the desktop coming back, a card dropped in the middle | critically damped spring, 50 % at about 99 ms, 99 % at about 390 ms (stiffness 289, damping 34); sideways motion, width and the turn's flattening ahead of it (the path) |
| Windows shrinking into the strip: the stage you leave, the desktop reveal, ⌘M, a window dropped on the strip | the same spring; sideways motion, width and the turn trailing it |
| Cards gliding to new slots | spring, 99 % at about 375 ms |
| The strip sliding out and in | spring, 99 % at about 400 ms, 286 pt |
| The edge reveal | starts about 180 ms after the pointer reaches the left edge |
| Hover | grows 11 % on a spring, half way at about 100 ms, 99 % at about 400 ms, its windows turning solid on the same curve |
| Press | eases to 0.987 of its size in about 100 ms |
| A card dragged out | its window as it leaves the strip: turned as the card at first, flat on the growing spring; slides under the pointer in about 100 ms (smoothstep) |

Measured 11-09 on a 5K screen at 60 frames a second, Apple's and ours running the same
scripted clicks and drags, each edge of a window tracked frame by frame (our own known springs
read back within 1 % the same way). Apple's hover is a spring too: an ease-out of any length
misses its gentle first frames and its long tail. The slide (Apple's read ω 16.3, ours 16.8)
and the press are unchanged.

**The switch's path** (measured 11-09 on two sets of recordings: first the fixture's blue
window growing out of its hovered card, slot 5, to 1100 × 748 pt and the green one shrinking
from 1000 × 692 into a combined card; then, recorded for this, one 1800 × 1100 pt window
growing from and shrinking into the same slot, three runs each way, its four corners tracked
frame by frame to a tenth of a point). Apple's three runs each way were identical to a tenth
of a point.

*What flies.* The window's picture, never stretched, turned about its vertical middle and
seen in perspective from an eye in front of the display's middle, 3000 pt away on the 5K
display (1.17 display widths): that rigid plane draws every frame of the 1800 pt window
within 0.16 pt rms growing and 0.43 shrinking. The strip's own eye (15, 720 at 2260 pt) does
not fit it (6.1 and 4.1 pt rms), and read through that eye the same frames are up to 14 %
wider than the window. Seen from the flight's eye, the card is the window far behind the
screen (0.12 of its size: 21,700 pt back, if the eye is 3000 pt in front) turned 76°.

*How it flies.* Its size, which is its depth, its turn and its height run on one critically
damped spring (ω 16.8 growing, 17.3 shrinking; 289/34 both ways), from the same moment. Its
sideways place runs ahead toward the window's end: 44 % across at 67 ms while its size is
29 % of the way, 66 % against 48 % at 100 ms. Growing, it is out over the desktop while still
small and far back, then comes forward to the viewer; shrinking, it shrinks back in place
first, then slides into the card. The lead depends on the geometry (0.75 · p (1 − p) for the
1800 pt window, 0.3 for the 1100 pt one, p the spring's progress); one rule gives both:
Apple scales the sideways place about a point 205 pt left of the screen (0.08 display widths)
while the place it scales from moves on the spring (each set alone puts that point between
200 and 228 pt left). Our model draws Apple's corners within 5.3 pt rms (22 at worst) growing
and 6.0 (17) shrinking on the 1800 pt window, 6.1 (24) and 5.3 (17) on the 1100 pt one. The
model of the round before (seen from the strip's eye; the middle's sideways place and the
width 0.33 · p (1 − p) ahead, the turn 0.19 ahead; the window stretched up to 16 % wide in
flight, so it widened with its sideways motion and seemed to fill toward the viewer rather
than come from behind) missed the 1800 pt window by 26.5 pt rms (118 at worst) and 27.2
(135), though it fitted the 1100 pt one it was made from (3.4 and 3.5). Ours keeps the lead
between none and 1 · p (1 − p), so a window close to the strip never passes its place; what
Apple's does there is not measured, nor the flight's eye and origin on other displays (ours
scale them with the display's width).

Not matched: Apple's window leaves its card at the card's hovered size and lands on it at
that size; ours drops the hover for a frame at the click and lands on the card at rest, which
grows again a few frames after.

## Deliberate differences

**A menu bar icon.** Apple's Stage Manager has none; its toggle lives in Control Center. Ours
gives a place for Settings, About, updates and Quit. It can be hidden, and opening the app
again then shows Settings. Its glyph is original art, as tall as the SF Symbols beside it at
the menu bar's default size (12 pt of ink, their median).

**Updates.** Apple's Stage Manager updates with macOS; ours updates itself (GUIDE, Updates).
A downloaded update also installs when you quit Stage Manager: the menu's Quit, or ⌘Q in
Settings and About. It then stays closed (no relaunch: the user asked it to stop). Never at a
logout, restart or shutdown, while Apple's Stage Manager is on, or when a window could not
be brought back. A launch that finds a downloaded update an earlier run left (it quit while
the update waited) checks again within its first minute, from scratch, instead of a day
later. An automatic install still waits, after an unlock, for 120 s with no input.
An alert that comes from a Settings control (Check for Updates… and the install it leads
to, a setting that can't be saved, Turn Off) is a sheet on the Settings window while it is
up, as Apple's HIG asks; the menu bar's counts as one from Settings while Settings is
frontmost. The automatic check's notices and the others stand free, as before.

**Launch at login.** Apple's is part of macOS. Ours registers at its first launch from
Applications, and Settings can turn that off. It registers as a login item
(`SMAppService.mainApp`). At login `loginwindow` first relaunches the apps of its reopen
list (about 3 s after the login) and holds the login items, and launchd agents with them,
until every one has checked in (measured: 12 to 27 s): as a login item, or as a launchd
agent, Stage Manager opened 23-35 s after the login, with most windows already up and
piled. The list holds the regular (Dock) apps running at
the logout; ours is an accessory app. So at the logout's first signal it turns regular
(`[app] reopen_with_apps`, on by default with launch at login on: its icon shows in the
Dock for a moment, no window, nothing activated) and macOS reopens it with the other apps;
a cancelled logout turns it back (a cancel signal, or no quit within two minutes of the
logout's latest sign or of the latest app that quit meanwhile). It
launches as an accessory app as ever, and no window of its own is restorable, so nothing
of it reopens but the app. The login item's copy then hands off without a word.
`[app] login_start = "background_item"` keeps the launchd agent
instead (listed under Allow in the Background; when macOS refuses it or it awaits approval,
the login item stands in and Settings says "Starts after your apps until allowed in Login
Items."); a change of the key moves the registration once, and an agent an older version registered
moves back to the login item. Within 90 s of the login, opening the app again (the login item after the
reopened copy) shows no Settings. An early launch waits for the screens a moment: the
displays' first reading (5 s at most) and, within a minute of the login, a screen the
stages were saved on that is not connected yet (10 s at most).

**Motion is drawn with pictures.** The public Accessibility API cannot scale a live window,
so switches are animated with snapshots in a click-through layer above every normal window
(always-on-top windows stay above it), while the real windows move behind them. Snapshots are refreshed every 2 seconds and as the pointer
nears a card, so content that changed meanwhile can jump when the motion ends. A click
during the motion brings the windows in at once; typing never cuts it short (the keyboard is
already the new stage's). An arriving window takes its place under its picture once the
picture as shown on screen is within a point of it on every edge (about 600 ms after the
click for a window growing from its card, 450 ms at the least: `[animation] unpark_at` is the
floor), so the hand-over shows no step. The picture comes to rest on the spring's own curve
(within a tenth of a point, about 750 ms for a 2000-point travel; Core Animation's settling
time stops 1 pt short, a visible jump), and fades out over its
window over 200 ms a moment after the unveil, so a look that differs (a picture taken while
the window was parked and inactive, a window's own late redraw) melts instead of flipping.
The arriving pictures fade as one picture, overlapping as their windows do: a window behind
never shows through the one in front as both fade (each faded on its own would show the one
behind at a quarter at mid-fade).
The incoming key window is captured again about 180 ms into the switch, once it is key, and
that picture cross-fades into its growing snapshot. Each picture
carries macOS's own window shadow, measured on a Studio Display (`stage-manager probe-shadow
--profile`: a hairline rim and a soft drop, the key window's 2.5 times as wide as an inactive
window's): the key window's on the window that is or becomes key, an inactive window's on
the others, turning into the card's shadow on the spring as it nears its card. macOS draws a
window's shadow by the window's appearance: in Dark Mode the drop reaches as far but is
nearly twice as dark and the hairline nearly opaque. The picture takes the shadow of the
appearance macOS draws that window in, read from the window's own shadow: a small capture of
the window with its shadow (half a pixel a point) next to its usual capture, when it was
never read, read before the system's appearance last changed, or 20 seconds ago; the alpha
past three of its sides is compared with the four shadows (light or dark, key or inactive).
An app may draw its window in another appearance than the system's (Messages draws a
conversation with a dark chat background all dark on a light system, and the system's light
shadow would flicker at the handover). With no reading
the system's appearance stands in. The log's `transition ghost` line names the shadow each
picture carries and where its appearance came from. macOS also draws a thin white line just inside
each window's edge, with the shadow (a capture that leaves the shadow out lacks it): a point
wide at a tenth in Dark Mode (0.08 on an inactive window), half a point in light, where it
shows only on dark content. The picture carries it too, over its edge and corner, coming
and going with the window's shadow; without it every edge steps lighter at the handover. The hairline and the rim follow the curve macOS's shadow
follows, measured on the dark hairline (nearly opaque, so a tenth of a point shows): Core
Animation's continuous corner with a longer tail, the hairline a band just outside it, sharp
where the edge runs along the pixels and softer at 45 degrees; the radius is read from how
much of the picture's diagonal is clear (a filled shape grown by a spread would put a darker
notch at every corner). A window's own
shadow appears in one frame, so the picture keeps its shadow until the window server lists
the window on screen at its frame (asked every half frame from the unveil), then fades it
over 50 ms from a frame later, never before 50 ms past the unveil: both shadows for a few
frames read about 2 levels darker, while a shadow faded before its window shows reads 16
lighter for 80 ms (a window can reach the screen a frame after a fade centred on the unveil
has ended). The picture goes only after that, 300 ms past
the unveil at the most; an outgoing picture's shadow comes in over twice
`[animation] cover_ms` as its window leaves. The cards' thumbnails are not captured while a
switch's pictures move (a round of captures is the window server's work, and drops a frame).

**Deviation (superseded switches):** a switch overtaken before it lands (another card
click, a click of yours anywhere, an activation, a focus) still brings its windows, but
never focuses one or activates its app afterwards: its held focus is dropped, so the keyboard is the newer switch's or
where you clicked (else two card clicks half a second apart hand the activation back and
forth between the two apps). A window's focus brings its stage only in the active app (or with the button down
on a sliver), never in a hidden app or an app behind: those are the app's own or a late echo
of ours (with apps slow to answer, notices come up to a second late, past the echo window). A window on screen when Stage Manager started but found late (its
app not answering then) goes to its stage as during the restore window, parked when that
stage is inactive, never bringing it forward.
Arriving windows
wait in the corner behind their pictures, and
that corner is covered with the wallpaper patch's picture until they are in place (macOS
raises an activated app's windows there) and the patch itself shows again (it is drawn a
moment before the cover goes, at most 300 ms after the motion; never where an arriving window
landed over the corner), never over the menu bar, and
never more than the corner itself (a 320-point square). It lies under the Dock, so a
see-through Dock over the corner shows the cover there, not a waiting window's edge. Where
desktop icons lie in that corner, the patch's picture would hide them: a still of the
desktop there (only the desktop's own layers: the picture and its icons, never a window,
even one that appears as it is taken; laid over the patch's picture when macOS leaves the
desktop picture out of it), kept fresh while card pictures are captured and the Mac is in
use (none while the screen is locked or asleep, or after 2 minutes with no input and no
switch; a fresh one at the first input), covers it instead; the parked windows' edges are
under it too for the moment of the motion. A window that stays shown through the switch and
reaches that corner (a pinned window, kept in pictures like the active stage's) is drawn on
the cover from its own picture, so it looks as it is (that corner of it holds still for the
motion's moment, as a ghost does; a video or a busy terminal there may show a seam until the
windows land); the outgoing stage's windows never count. Where it cannot be covered at all
(the patch off, card captures off with icons there,
no picture ready yet, a staying window with no picture of it, such as another app's the
app does not manage, an activated window macOS put outside that square), an activated app's
windows come to their places at once instead of growing.

**Card pictures are stills**, refreshed every 1.5 seconds, not live surfaces. Until a
window's first picture arrives (at launch, a window first seen), its place on the card is
faint glass: the card's wallpaper-toned under at `[cards] empty_opacity` (0.35), with the
card's edge, or with no picture of the wallpaper a black as faint (the wallpaper darkened as
much as that glass would); the picture then fades in over `[cards] fade_ms` (200 ms). A
dragged card's stand-in without a picture is as faint. Apple's cards show live surfaces and
are never drawn without one.

**Hidden stages wait in a corner.** Without private APIs, the only way to hide a window but
keep it ready is to move it almost off screen. macOS keeps a thin edge of each such window
visible in the bottom-right corner (another corner if a second screen sits beyond it). To
keep the corner as clean as Apple's, those edges ignore clicks and a patch covers them,
reaching past each edge only as far as a hidden (inactive) window's shadow does: least above,
most below (`[windows] corner_patch_reach` sets one number for every side).
Where window thumbnails are captured (Screen Recording), the patch is a still of the corner
showing only the desktop's own layers (the picture, Finder's icons, widgets; never a window,
ours included), exact by construction, so nothing on the desktop keeps it off and icons
under it take clicks and drags as usual (an icon dropped, moved or taken away under the patch
hides it until the new still is drawn, so a stale picture never cuts the icon); it is taken
again on arriving at a Desktop once the slide settles, when an icon there changes, after a click on the desktop, a moment after the
picture or its predicted frame changes, and every 10 seconds otherwise (never during a
switch's motion, a slide, Mission Control or Show Desktop; none while the screen is locked
or asleep). The patch stays where it is through Show Desktop, Mission Control and App
Exposé, as the desktop picture does. Drawn from a still, it shows through Show Desktop what
the desktop shows there and grows over the hidden windows' corners that Show Desktop pushes
into view, back to its usual size once they are back in the corner (never over a shown
window's edge); drawn from the file, it hides then and comes back the moment the windows
start sliding back, covering the hidden windows until they are back. A moving
wallpaper may differ there until the next still. Without Screen Recording, the patch is drawn from the wallpaper's own file instead. A hidden window that
its app moves back onto the screen by itself goes back into the corner within about a
second; the one you are working in (focused, or dragged out) stays and joins the stage you
are in. Dynamic pictures (by time of
day, by the sun, light and dark, Apple's own downloaded ones included) are drawn at the
frame macOS shows. For a picture that follows the sun, neither the place (the time zone
covers hundreds of miles) nor Apple's rule is known, so the clock is a model that starts
wide (any place in the zone, either of two ways of reading the sun, macOS a few minutes
late) and narrows with what the screen shows. Near a frame change the frames it allows
differ: the patch draws when they look alike in the corner, or when one still of the desktop
(taken only where window thumbnails already are) matches one of them; each clear still
narrows the clock, and arriving on a Desktop one still checks even a frame the clock is sure
of. When nothing the clock allows fits the screen, only stills decide. The patch's pixels
are still the file's. The file's patch steps aside where it cannot match exactly (see the GUIDE's
troubleshooting). Each Desktop has its own patch, which stays on it and slides with it when
you go to another Desktop and back, drawn as it was (nothing moves on a Desktop while you
are away); it is checked again once the Desktop has arrived, and hidden while away if the
picture's frame changed meanwhile (the sun, the appearance; near a change of the sun's
frame, unless the frames look alike there) or a desktop icon came under it. A Desktop's
first visit still shows its edges for a moment, until its patch is drawn.

**Dragging uses stand-ins.** A window dragged over the strip keeps its size under a
card-sized picture of it, where Apple shrinks the window itself: Stage Manager cannot resize
another app's window while you hold it without fighting the drag. A card dragged out is shown
by a picture of the window it was pressed on (its front window on a card of one), at the
window's size past the strip.
A window of a combined card lifts off as that picture from the first move, inside the strip
too, where Apple's leaves it on the card until the pointer is about 245 pt from the edge
(Drag and drop, above).

**Minimize** sends the window to the strip as a card of its own, as Apple's does. Apple's,
recorded 11-09 on the 5K at 60 fps (the yellow button on a 900×632 window): no genie and
nothing in the Dock. From the first moving frame the window flies straight into a new card at
the top of the strip (the other cards slide down one slot), turning and shrinking as a stage
leaving in a switch does, its app icon on it early; about 450 ms to rest, most of it in the
first 220 ms. With Reduce Motion on the window fades out in place over about 8 frames and the
card appears.

Ours catches the two usual ways before macOS starts its genie, in an event tap
(`platform::minimize_tap`, decided in `engine::minimize_press`):

- **The yellow button** (a plain click; ⌥-click, ⇧-click and ⌃-click stay macOS's): a press
  near the top-left corner of a window in a shown stage, with that window topmost there (not
  under a card, the edge tab or Settings), is hit-tested through its app's Accessibility at the
  press (0.1 s budget each call; our own windows are never asked). On that window's enabled minimize button the press,
  its drags and its release are swallowed; a release on the button minimizes, one dragged off
  does nothing, as the button's own.
- **⌘M** (no other modifier) while the frontmost app's focused window is a managed one with
  an enabled minimize button and a settable `AXMinimized`. Any other window keeps its app's ⌘M.

The window then leaves as a stage does in a switch: its picture flies from its frame into
its new card at the top of the strip, the others gliding one slot down; under Reduce Motion
its picture fades out where it is while the card fades in. In place (`[strip] order`) the
card takes its own place instead.

Everything else is macOS's minimize first: Window → Minimize from the menu bar, a
double-click on the title bar set to minimize, an app minimizing its own window, ⌥⌘M, an
answer that did not come in time, a full-screen Space, Mission Control. Stage Manager hears
of those only once the genie has put the window in the Dock; then one motion: a picture of
the window comes out of its Dock tile (its own tile, or its app's icon with "Minimize windows
into application icon"), the size the genie left it at, and flies into its card. With no
tile on that screen or no picture of the window, the card fades in where it lands; under
Reduce Motion the card's fade is all. No full-size picture ever shows where the window was.

After a genie the window itself has to leave the Dock to be parked, and macOS animates every
un-minimize out of the Dock tile toward the window's frame, whatever that frame (on screen,
off every display, with its app hidden: measured 11-09). So while it is still in the Dock it
is put beyond the screen edge the Dock is on, in line with its park frame (2000 pt past every
display): its un-minimize sinks out of the tile off the screen within the Dock's band (gone
in about 0.2 s) instead of rising and crossing the screen to the corner, macOS sets it down
in the corner, and it is parked again the moment it is down. Where that way would pass
another screen it goes to the park frame as before. macOS's un-minimize focuses the window:
that is never taken for a window you brought out (only a focus or a drag of yours after the
park counts as that).

`[windows] minimize_to_strip = false`: nothing is caught, and every minimize goes into the
Dock.

**Desktop items are left alone.** Hiding Finder's desktop icons is outside a window
manager's reach through public API.

## Extras

These are not in Apple's Stage Manager. Some are on by default because they stay invisible
until used; the rest are off.

| Extra | Default |
|---|---|
| Drag a card up or down to reorder the strip | on |
| Drop a card onto another card to merge the two stages | on |
| Hold Shift while opening or activating an app, or on a Dock click, to bring it here | on |
| Parked windows' edges ignore clicks; the wallpaper patch covers them | on |
| Rules for new windows: a named stage, a new stage, pinned to every stage, or ignored; centered or cascaded | off until a rule is written |
| Keyboard shortcuts to switch, send, pin and show the strip | off |
| Keep windows clear of the strip, by moving or narrowing them | off |
| Card numbers, stage names, an edge tab, a strip above windows | off |
| Stages come back after a restart or a new login (`[stages] restore_after_restart`) | on |
| A window an app reopens after a restart at under half its saved size gets its saved frame back, once (`[restore] saved_size`) | on |
| A newly opened app joins the current stage; Shift then gives it a stage of its own (`[stages] new_apps_join_current`) | off |
| Keep stages in place: a stage's place never depends on which card was clicked (`[strip] order = "in_place"`) | off |
| Follow the Dock: the stages' places in the Dock's order of their apps (`[strip] order = "dock"`) | off |
| Following the Dock, the Finder gives a stage its place only when no other app of the stage is in the Dock (`[strip] dock_ignore`) | on |

Shift never changes the app switcher: ⌘-Tab and ⌘⇧-Tab always switch stages.

**Keep stages in place** (Settings → Stages → Strip order: Keep in Place; `[strip] order =
"in_place"`). Apple's swap leaves the stages in click order, so a stage's place cannot be predicted. In place,
every stage has a place of its own in creation order per Desktop (hotkey numbers follow it;
a new stage goes last, a closed one leaves no gap, a dragged card moves its stage in that
order, a restart brings the order back). The stage you leave shrinks into its own place and
the clicked card grows into its windows, on the same spring and snapshots as Apple's swap;
⌘-Tab, the Dock, the hotkeys and a desktop reveal follow the same rules. `[strip] active`
says how the active stage's own place shows: `"close"` (the default) gives it no card, as
Apple's strip never shows the active stage, and the cards below close up (the cards between
the two places glide on the strip's spring); `"gap"` leaves its slot empty at its card's
size, so no other card ever moves; `"card"` draws it as a card with a light rim. With more
places than the strip holds, the first `capacity − 1` stages keep theirs and the rest share
the bottom place: the most recently used of them shows, so the stage you leave always has a
card and the head never moves (with `"close"`, an active stage in the head frees its place,
and two of the rest show). Cards keep Apple's one size. "One at a Time" shows an app once,
in the place of its first stage, whichever of its windows is shown.

**Follow the Dock** (Settings → Stages → Strip order: Follow the Dock; `[strip] order =
"dock"`). The places of Keep stages in place, ordered by the Dock: the strip's
top-to-bottom order is the Dock's left-to-right order (top to bottom for a Dock on the side).
The Dock's application items are read through Accessibility (its lists' `AXDockItem`
children and their `AXURL`), so running apps that are not kept in the Dock count where their
icons show; its preferences are never read or written. A stage sits at the leftmost Dock
position among its windows' apps (by bundle id, else by pid), the apps of `[strip]
dock_ignore` (default `["com.apple.finder"]`) counting only when no other app of the stage
has a Dock icon: the Finder's windows join stages of every kind and the Finder always sits
first in the Dock, so a stage a Finder window joined would otherwise sit at the top and jump
there whenever it is left (a stage of Finder windows alone still sits at the Finder's place;
a stage of listed apps alone at the leftmost of them; a changed list re-sorts at once, as a
changed Dock does; `[]` lets every app count). Stages at the same position
keep creation order, and stages whose apps have no Dock icon come after all others, in
creation order. Hotkey numbers follow the strip. The Dock is read at launch, half a second
after an app launches or quits, and every two seconds while this order is on (no
Accessibility notification is known for a rearranged Dock); a changed order re-sorts the
strip at the first moment no switch, drag or Desktop change is under way, and the cards glide
to their new places on the strip's spring. `[strip] active`, the overflow rule and the
animations are Keep stages in place's. A card dragged within the strip springs back and
switches nothing (`[strip] reorder` is off in this order); dropping it on a card merges, and
dragging it out still works. A window dropped on the strip makes its stage, which takes its
Dock place, not the drop position.

**After a restart.** The restore is invisible when it works and Apple's behaviour when it
cannot match. Saved windows not on screen at launch wait, with no card, in their stages; as
apps reopen them each goes back to its stage, at its saved frame when the screen it was saved
on has the saved size, parked only on a confident match (a doubtful one goes by Apple's rule
and stays in view or gets a card of its own). While the login settles (until apps have been
quiet for 20 s, none still opening its first window, and the login is a minute old; the
first click or key press; or 5 minutes) launch-time activations and new windows switch
nothing and animate nothing, and cards come in quietly in their saved order; then the stage
left in front shows with its key window focused, unless a window shown there already has
the keyboard. While that stage is still empty (its app slow to open), a window opening a
stage of its own takes the front instead of a card beside an empty screen; the saved stage
then waits, and its windows get a card when they come. Apps that do not answer
Accessibility yet (busy reopening at the login) are asked on threads of their own, every
150 ms while the login settles and every second after, for a minute at most: a window goes
to its stage about as soon as its app answers, and one slow app holds up no other app's
windows. Before its app answers a window shows where macOS reopened it (moving a window
needs its app's answer).

At a logout, restart or shutdown every window comes back at the first sign of it macOS gives
(any of `loginwindow`'s logout notices, `willPowerOff` the last), so apps save their windows
where they are as far as the timing allows (best effort: macOS may ask apps to quit first);
from then on nothing is arranged (no switch, no card added or removed) and the app itself
quits when macOS asks it to. A cancelled logout brings the stages back at once (one macOS
says nothing of: two minutes after its latest sign, or after the latest app that quit
meanwhile, so a slow restart, an app asking about unsaved changes, is not taken for one).

A logout cancelled after macOS quit the app (another app's unsaved document, its Cancel
clicked later) stops there, and apps already quit stay quit; Apple's Stage Manager, part of
the window server, never quits. So after a quit at a logout (the head start held, or the
quit event's reason a logout, restart or shutdown; never the user's own Quit) the guardian
waits up to ten minutes, logging each logout sign it hears: at a cancel
(`com.apple.logoutCancelled` or its notify(3) name) it opens the installed app again in the
background (`open -g -a`), which restores the stages saved at the head start, in the same
session, by window id, as an accessory app (not macOS's reopen at a login: the reopen list's
mark is cleared as at any launch). A logout that goes on ends the session and the guardian
with it, nothing opened; so does the end of the ten minutes, or the app already running
again. The same goes for a quit at a logout that left windows away (an app that did not
answer, a window gone): the parked file says the quit was for a logout, and the guardian
listens for the cancel while it brings those windows back. A cancel heard meanwhile opens the
app at once (it adopts those windows, or takes the rest of the watch, as after a crash); once
every window is back with no cancel yet, the guardian waits as after a clean quit, the ten
minutes counted from the quit. The guardian is no app: `loginwindow` never waits for it.

An app that quits keeps its windows' places in the saved stages for two minutes (no card
meanwhile), so stages whose apps quit before macOS announced the logout still come back after
the restart. A window reopened squeezed into the park corner, or showing only a piece at it,
goes to its saved frame (brought on screen) when it is matched, and is left where it is (and
counted) when it is not.

A window an app reopens on another Desktop than the one it was saved on (non-native apps
reopen on the Desktop shown) goes into its saved stage made again on the Desktop where it
shows. A window reopened on another screen than its saved stage's, that screen connected,
goes back to that screen, at its saved frame there, in its stage, in front or a card as saved
(Apple's keeps no stages over a restart, so nothing of Apple's is changed); a stage saved on
a Desktop of that screen not shown now is made again where the window shows, as above, and a
window reopened on its own screen stays where its app put it. Saved frames come back on the
screen they were saved on, at the same place on it when the screens were rearranged in
between.

The one exception to the app's frame (`[restore] saved_size`, on; VS Code can reopen a window
at a third of its size): after a restart only, a window matched by its title (a strong
match, not by app and size), reopened at under half its saved area on the screen of its saved
stage at the saved size, the saved size fitting that screen, gets its saved frame once, when
it is first shown (in front at the restore, or its card's first click), within reach as every
restore frame; one log line names it (bundle, from, to). A move or resize seen first keeps the
user's frame; a medium match, a relaunch in the same session, a window larger than saved and
another screen size keep the app's frame (`state.json` keeps the saved frame while it is
owed).

A Desktop is found again by its windows; a screen macOS keeps one Desktop for (read from
`com.apple.spaces`, never written) is the Desktop saved on that screen even before any of its
windows show (apps may list none yet when Stage Manager starts), so they go back to their
stages there as their apps reopen them, and one reopened on the other screen comes back to
it. A screen with several Desktops, or two Desktops saved for a screen that keeps one now, is
left to the windows.

A window that went by Apple's rule (no saved window matched it: an Electron app's window
still titled with its app's name, say) or by a doubtful match is looked at again when its
title changes, and goes to its saved stage on a confident match, a card when that stage is
not in front, nothing switching. This holds per window, until two minutes after the login
(after the launch, for Stage Manager started later): a click on one window, which ends the
settle, leaves the others to it, since apps are still opening then. A window the user has
touched stays where it is: one focused by a click or a key press on it, typed in, moved or
resized, or whose stage was switched to (a card click, a Dock click, ⌘-Tab, a shortcut). A
saved window that shows only after the settle still goes to its stage, as above; that wait is
bounded per app, not by the two minutes (its entries go a minute after its app's last new
window, at the settle's end when its app is not running, and at the next session; a session
in which its Desktop never showed does not count, up to 30 days since that Desktop last
showed). An entry of a Desktop not shown yet stays through that minute for the Desktop's
first show, its app's minute starting again there, but from then on a window shown on another
Desktop is a new window by Apple's rule, never a stage of that Desktop's.

A title that only puts something in front of the saved one ("main.rs — proj" for a saved
"proj", an editor showing a file in its workspace) is the same name, so the frame the window
reopened at counts. While the restore window is open (no click or key yet, so every window
that shows is one an app reopens) a window under another name at the exact saved frame of a
saved window of its app is that window, when nothing else can be meant and the app is
reopening its saved windows: the only one of the app waiting at that frame, no other window
at it, and another window of the app back in its saved stage (a browser's first window, a
Start Page where a saved page was, is not that page; a window that showed before its sibling
is looked at again when the sibling comes). So a second window of Fork showing another
repository gets its own stage, not its sibling's. After the restore window the frame never
outweighs two names: a window the user opens (Finder's ⌘N where a saved window was) keeps
Apple's rule. `[stages] new_apps_join_current` never applies while the restore window is
open: an app reopening its windows is not one the user opened.

No apps are relaunched (that is macOS's "Reopen windows when logging back in") and no window
moves between Desktops (no public API does); nothing lists those windows for the user, who
could not act on such a list. When the restore window ends (no sooner than a minute after the
login), the saved windows of apps not running then are given up (one log line names them): an
app opened later goes by the new-window rules, never back to a stage of the last session. A
running app keeps its saved windows until it has opened none for a minute. Off, a restart
starts every app in a stage of its own, as Apple's does. One log line sums it up, ending with
the windows back on another Desktop (bundle id, hashed title, the Desktops' ids;
`stage-manager log summary` repeats the counts).

**When macOS's Accessibility sees no window.** After WindowServer recovers from a hang,
Accessibility can list no window of any app for the whole session while every app answers and
the window server shows them; a logout fixes it. Apple's Stage Manager lives in WindowServer
and never meets this; ours can do nothing then. The line (`engine/ax_blind.rs`):
Accessibility trusted, at least 3 real windows of regular apps on screen, of at least 2 apps
that answer, none listed, for 30 s; not within 30 s of the login, not while the screen is
locked, Mission Control or a full-screen Space shows. One app quiet is not it. Once it holds,
the menu's top line says *"macOS isn't reporting windows. Log out or restart to fix it; your
stages are saved."* (no notification, no dialog; not while Apple's Stage Manager is on),
`ctl state` has `accessibility_blind` and `accessibility_blind_since`, and one log line marks
its start and one its end. It ends the moment any window is listed. Meanwhile every app is
read again every 2 s, nothing waiting is given up (no app's quiet minute runs, apps not
running wait too), and a save marks every Desktop not looked for, so the next login gives up
none of their windows for this session.

While Accessibility lists nothing (from the first look, no hold), and while the window server
shows windows of regular apps before Accessibility has listed any in this run (an empty
screen is not that), a click or a key press does not end the restore window, and its own ends
wait while it lists nothing, so no waiting window is given up for a click on nothing. Windows
listed again in the same run bring the restore back as at a launch: the restore window opens
again (its clocks from then), every saved window still waiting, of the Desktops shown or not,
goes back to its stage by app, title and frame as it is found, parked when its stage is not
in front, and the windows on screen all along bring no stage forward even after it.

## Accessibility

### Reduce Motion

Decided from what the setting is for, not from Apple's Stage Manager (whose behaviour under
it is not measured). Reduce Motion is for people whom large or depth motion makes dizzy or
sick: the vestibular triggers are large surfaces moving across the screen, zooming toward the
viewer, and turns in 3D. Apple's Human Interface Guidelines
([Motion](https://developer.apple.com/design/human-interface-guidelines/motion),
[Accessibility](https://developer.apple.com/design/human-interface-guidelines/accessibility))
ask apps, with it on, to replace large movement, zooming, scaling, depth changes, spinning and
parallax with dissolves and fades, to tighten springs so nothing bounces, and allow motion
that tracks a person's own gesture directly. WCAG 2.3.3
([Animation from Interactions](https://www.w3.org/WAI/WCAG21/Understanding/animation-from-interactions.html))
asks that motion set off by an interaction can be turned off unless it is essential. What
carries meaning stays: where a window went must still be clear.

`[animation] reduce_motion`: "system" (default) follows macOS's setting, read at launch and
whenever macOS says it changed; "on" and "off" ignore it. With it, every motion below is
replaced by one fade of 200 ms (`display_options::FADE_MS`, scaled by `[animation] speed`),
one way, never repeated, so nothing flashes. `[animation] enabled = false` or `speed = 0`
still turns everything off, fades included.

| Case | Full motion | Under Reduce Motion | Why |
|---|---|---|---|
| A window minimized to the strip (the yellow button, ⌘M) | its picture flies from its frame into its new card, as a stage leaving does | its picture fades out where it is while its card fades in at its slot, as Apple's (11-09) | as the switch below; the card fading in says where it went |
| A window minimized to the strip after macOS's genie (the menu, an app's own) | its picture flies from its Dock tile into its card | its card fades in at its slot; no picture | the window is already gone into the Dock (macOS's own animation, which follows Reduce Motion itself); the card fading in says where it went |
| The switch (also the desktop reveal, an activation, a drop) | windows fly between card and frame at full-screen scale, turned and scaled in depth | a cross-fade where the windows are: the outgoing pictures fade out at their frames while their stage's card fades in at its slot; the incoming pictures fade in at their frames while their card fades out | the largest trigger of all: depth, scale and a 3D turn over most of the screen. The card fading in at its slot as the windows fade out says where they went; the same unveil, handover and landing rules hold, with no spring travel |
| The strip sliding out and in | 286 pt sideways, in 3D | fades in at its place; fades out where it is, then leaves the screen unseen | a large surface moving across the screen |
| Hover | the card's windows grow 11 % on a spring | no growth; the glass still turns solid | scaling by itself; the colour change alone still shows which card is under the pointer |
| Press | the card eases to 0.987 | none | scaling |
| A card moving to another slot | glides on a spring | at its new slot at once, fading in there | movement the user did not make; the fade still marks the card that moved |
| A card or window dragged out | appears turned beside the pointer, slides under it and straightens (the 3D settle) | appears flat under the pointer; then follows it | following the pointer is the user's own movement and stays; the slide and the turn settle by themselves |
| A card dropped on a card | shrinks into it and fades | fades where it is | scaling |
| A card dragged in the column, a window's card over the strip | follows the pointer | the same | the user's own movement |
| The park corner's cover and the corner patch | still pictures over the corner while windows wait there | the same | nothing moves; without them a window jumping to the corner would show |
| The landing (pictures fading over the windows), a new card fading in | fades | the same | already fades |

### Reduce Transparency

macOS's own materials turn opaque with Reduce Transparency (and with Increase Contrast, which
turns it on). The cards' glass does too: each window's picture is drawn solid on the cards
(as `[cards] window_opacity = 1`), and the edge tab is solid with a firm edge. Read at launch
and followed live; the strip repaints at once.

### VoiceOver

**Apple's** (observed 11-09 with the Accessibility API, Apple's Stage Manager on): each card
is an `AXButton` of WindowManager, framed where the card is, labelled with its front
window's title alone ("Activity Monitor"; no stage number, no other app's name), with the
actions `AXPress` (a click: the stage comes in front) and `AXAddToStage` (Shift-click's
add). A system-wide hit test at a card's middle (`AXUIElementCopyElementAtPosition`, how
VoiceOver finds what is under the pointer) answers that button wherever the pointer is.

**Ours.** The strip's view is a group, "Stages", holding one button per card, framed where
the card rests, labelled as Apple's: the front window's title; a card of several apps reads
its front window's alone, as Apple's. Where we add to it: an untitled front window reads its
app's name; a stage with a name (`[stages] names`, a `stage:` rule) says it first ("Mail:
Inbox"); with nothing known the card says its ⌃⌥N number ("Stage 3"); the current stage's
card (`[strip] active = "card"`) adds "current stage". A window retitled while it fronts a
card renews the label. Its help says "Switches to this stage."; `AXPress` switches as a
click does, `AXAddToStage` adds the front window to the stage in front as a Shift-click
does (not offered on the current stage's card), and each app has a custom action, "Show all
Safari windows", as a click on its icon. The list follows the cards as they change, keeping
each card's element; it is empty while the strip is out of sight. The edge tab is a button,
"Stages", that reveals the strip.

**Reaching the cards.** The window server passes a hit test over a window that ignores the
mouse, as it does a click, and the strip's pane lets the mouse through except while the
pointer is on a card (one flag per window, set by where the pointer is). So a hit test at a
card with the pointer elsewhere answered Finder's desktop beneath (found 11-09).
While VoiceOver or Switch Control is on (read from macOS and followed live), each card gets
a transparent catching window of its own exactly where it rests, just under the pane:
hit tests there answer the card's button, the pointer's events there go to the strip as
the pane's own would, and the empty column and the gaps between cards still let clicks
through. With neither on, nothing changes. The strip never takes keyboard focus (a
non-activating panel, as Apple's strip never takes it from the window in front), so
VoiceOver reaches the cards through the pointer: the pointer over the strip, or VoiceOver
set to move its cursor with the mouse pointer.

## Measured on Apple's, nothing to change (11-09 evening)

- The card glass in Dark Mode: window opacity 0.78, backdrop saturation 0.93, brightness 0.82
  (`scripts/measure-apple-card-glass.py`); ours, fitted in Light Mode, 0.78 / 0.95 / 0.78, within the
  fit's error.
- The card count follows the visible height: 4 at a 13-inch MacBook Air's (about 925 pt), 6 at the
  Studio Display's, as reviewers counted; ours gives the same.
- Reduce Motion on Apple's: a switch is a cross-fade of about 80 ms, a minimize a fade out in place of
  about 130 ms. Ours fades 200 ms, worked out from the accessibility guidance rather than copied.

## Known gaps

- A window coming in from the strip can show one frame of a darker shadow as it lands
  (about half the landings, up to 15 levels within 45 pt of its edge on Finder's heavier
  shadow; more often when the Mac is busy). Two shadows overlap for that frame: the
  window's own, which macOS shows the moment the window is in place, and its picture's,
  which ours drops as soon as macOS reports the window there but which reaches the screen
  about a frame later. macOS gives no way to drop another app's window shadow, and no
  signal comes before the window is on screen; dropping it earlier by a guess shows a
  lighter edge instead (11-01). Measured 11-09 at 60 fps on VS Code, Safari and Finder.
- Stage hotkeys and `ctl switch N` count and switch the strip of the screen the keyboard is
  on (the focused window's; the main screen's with no focused window); which screen
  Apple's hotkeys address is not measured.
- No public API names a Desktop, so Desktops are told apart by their windows. An empty
  Desktop is forgotten when you leave it, and one whose windows are all hidden or minimized
  looks new until one of them shows.
- A Dock click to another Desktop arranges it before it slides in. ⌘-Tab cannot: no public
  notice names the chosen app early enough, so the Desktop is arranged when it arrives.
- A Desktop's strip appears after the Desktop has slid in on its first visit; Apple's slides
  in with it.
- A dialog's parent window is a guess: the app's frontmost window at the time.
- New windows are matched to rules by the title they have when they appear.
- Windows of a relaunched app are matched to their stages by title and frame; two of one
  app with the same title and no telling frame stay in view rather than guess.
- With "Click wallpaper: Always" (macOS's default), a wallpaper click moves every window
  aside as macOS does outside Stage Manager, where Apple's puts the stage into the strip and
  the strip stays (recorded 11-09). Only "Only in Stage Manager" gives the click to Stage
  Manager.
- Since macOS 15, macOS asks about once a month whether the app may capture windows
  directly. Apple's content picker avoids that alert but cannot serve a strip of every
  window.
- A switch's motion starts once the windows you leave have moved to the hiding corner behind
  their pictures, or they would show around them: about 50 to 90 ms after the click (11-09's
  log; the `transition: the motion starts` line has it as `motion_ms`). Apple's first frame
  comes about 65 to 85 ms after the click (11-09, four clicks of six).
- A card dragged from the main screen's strip onto the laptop's screen landed with the
  pointer 39 pt in and 353 pt down on Apple's (once, measured 11-09): its rule across screens
  is not known; ours holds it as on its own screen.
- Apple's auto-hide comes either about 0.1 s or about 1.1 s after the pointer leaves the
  strip (six runs, 11-09, no difference found between them); ours waits 500 ms.
- Not measured yet on Apple's: a card of four or more apps, which side a third app's window takes when it sits on the
  other side of the front window from the second app's (ours: the second's side), and what
  decides how far the other app's window peeks out (ours: always the fixture's 42 and 15 pt, on its
  side of the screen).
- Where Apple places a window moved to another Desktop is unverified. Ours places it as a
  window of its app opening there: in that app's most recent stage on that Desktop, else a
  stage of its own, shown in front. Several apps moved at once get a stage each; the front
  one shows, the rest wait in the strip.
