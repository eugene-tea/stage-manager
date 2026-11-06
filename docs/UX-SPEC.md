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
| Click wallpaper to reveal desktop | With "Only in Stage Manager", a wallpaper click reveals the desktop. With "Always", macOS moves the windows aside itself and Stage Manager stays out of the way. |
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
stays up under it. It slides away only once a window of the front stage comes within 50 pt of
the screen's left edge, as Apple's does (measured 11-05 on a Studio Display: Apple's stayed up
under VS Code dragged left over the cards from about 308 pt, and went at about 50 pt; a laptop's
windows nearly fill the screen, so there it goes more often). Each screen's strip follows its
own front stage. Resting the pointer at the left edge brings it back. Each card draws the stage's
windows at one scale, turned in perspective toward the middle of the screen, with the app
icon on its bottom-left corner. The number of cards depends on the screen height: 5 on a
laptop, 6 on a 5K screen. A popup open on a window (a popover, a child window sticking out
of it) shows on its card beside the window, at its true size and place, as on Apple's; the
window keeps its own size on the card, and nothing outside a window is drawn. A palette
far from its window (more than the window's own width or height past it, or on another
screen) is left off: the card shows the window alone. So is a popup over a window that
macOS's capture does not place (a calendar's event popup reaching past the window's top):
never a fill around a window on a card.

**Cards let the wallpaper through**, as Apple's do (seen on the owner's screenshots
11-03, measured 11-05): each window's picture is drawn at 78 % over a flat colour of its own,
the wallpaper's average behind the thumbnail at 95 % of its saturation and 78 % of its
brightness (`[cards] window_opacity`, `backdrop_saturation`, `backdrop_brightness`).
`scripts/measure-apple-card-glass.py` read one of Apple's cards with its window flat white,
black and grey: the opacity 0.78 in two runs, the colour behind within a level of the model,
the grey a blend in the display's encoded values; the hovered card read fully solid. Apple's
colour is flat over a card where the wallpaper behind it changes, so it is an average, not a
blur. Not measured yet: dark mode, and what Apple shows through a window behind
another on one card (here each window has the wallpaper behind it). A switch's snapshots
take the card's look as they shrink into it and leave it as they grow into their windows.
The card under the pointer turns solid as it grows (the owner, 11-03: "Apple's SM doing
that"): its pictures ease to `[cards] hover_window_opacity` (1) on the hover's own curve and
time, and back as it shrinks on leave; a card a dragged card would merge into does the same,
as it grows. A click on it hands the snapshot over at the look drawn that moment (solid, or
part way), so nothing jumps; a card the snapshots land in under the pointer shows at rest,
then grows and turns solid together. Not measured on Apple's yet: whether its hovered card
turns fully solid, and on the same timing as the growth (`measure-apple-card-glass.py
--hover` reads it). The wallpaper is drawn from its file, as the clean corner's is; where it cannot be (a
shuffle, an unreadable picture), the cards are opaque.

**Strip order is slot-stable.** A clicked card's stage becomes active, and the stage you
leave takes the slot the clicked card vacated. A stage entering a full strip takes the slot
of the least recently used card. (Keep stages in place, an extra below, orders it otherwise.)

**Switching.** The pressed card eases back on mouse-down. On release, the windows you leave
shrink into their card's slot and the new stage grows from its card, on Apple's spring.
Shift-click a card to bring its windows into the current stage; Option-click is a plain click.
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
Sheets and dialogs follow their window. An app that quits takes its windows out of their
stages and an emptied stage leaves the strip, also when the app quits without macOS's notice.
An app that saved a hidden window's corner place when it quit, and reopens the window there
within a day, has the window put back where it was before the quit (else centered).
A window an app keeps wholly off every screen (a headless browser's, an offscreen render
window) is left alone, as Apple's leaves it: no stage, no card, never moved; it is taken in as
a new window if it ever comes on screen.
An app whose `AXEnhancedUserInterface` an assistive client turned on animates the frame
changes made through Accessibility (Chrome's window slid in from the park corner, 11-03).
Apple's Stage Manager is no Accessibility client and never shows this; we turn the flag off
around our moves, once per app per switch, and back on after (Rectangle's and yabai's way), on
by default with no key. A write the app answers with an error is read back (Chromium's setter
acts on the value, then passes it to AppKit's, whose answer may be an error: -25208 from Chrome); an app that refuses is left alone for a while (1 s, doubling to
30 s), not asked at every move.
**Deviation (stale activations, 11-03):** under load an activation notice reaches us seconds
late (14:09:57: 1–2.6 s; each late one switched stages, each switch brought its own late one,
ten switches back and forth). An activation that fired before the latest switch we began, or
whose app is no longer frontmost when we decide, switches nothing (logged once with its
reason and age); a focus that would switch stages switches nothing when its app's focused
window is in another stage by then. His latest ⌘-Tab or Dock click is neither, so it still
switches, late.

**First start.** Every app with windows becomes its own stage, as when you turn Apple's on.
The frontmost app's stage is shown; the others enter the strip, most recently used first.

**All at Once and One at a Time.** With All at Once, a stage holds all of an app's windows.
With One at a Time, only the app's most recent window shows, and clicking its card again
opens its next window.

**⌘M** sends the window to the strip as a card of its own, not to the Dock.

**Desktops.** Each Desktop has its own stages, active stage and strip, listing only its
windows. The same app can have stages on several Desktops. A card click never changes the
Desktop. ⌘-Tab and the Dock switch Desktops as macOS's own setting says ("When switching to
an application, switch to a Space with open windows for the application"), and the arriving
Desktop shows that window's stage. An app macOS activates as a Desktop slides in (a swipe,
⌃-arrow) never switches a stage on the Desktop being left: it is decided once the new
Desktop is recognized, as Apple's never touches the Desktop left. Full-screen apps have no
strip.

**Screens.** Every screen has its own strip and stages, as Apple's (measured 11-05 on the
owner's Mac: a 5K Studio Display main, the MacBook's screen to its left, "Displays have
separate Spaces" on): a strip on the left edge of each screen, its cards sized and counted
for that screen (6 on the 5K, 5 on the laptop), each screen with its own front stage and
cards, each strip sliding away under its own windows. A window dragged to another screen
starts a stage of its own there, in front, once it is let go (nothing changes under the
hand, however slow the drag across the seam); the stage that was in front there goes to a
card; the screen it left, its front stage now empty, brings its most recent card forward. A
card click changes only its own screen, and so does a click on a card's app icon: the app's
windows come from that screen's stages. A window dragged onto either screen's strip becomes
a stage of its own there. A new app opens on the screen with keyboard focus,
as its own stage there, by the one-screen rules (macOS picks the screen; the window is
taken where it shows). A window straddling two screens belongs to the one showing most of
it (macOS's rule). The lid closed: that screen's front stage comes to the remaining screen
as its front stage (its windows where macOS puts them, at their size), the remaining
screen's front goes to a card, and the removed screen's other stages become cards there
too, after the remaining screen's own (past the capacity, hidden); its hidden windows wait
at the remaining screen's corner. macOS may put the closed screen's windows on another of
the remaining screen's Desktops and slide to it (the owner's 11-05: the 5K went to its
second Desktop, not shown since the launch): the same rule holds there, the closed screen's
stages whole, that Desktop's own front a card (round displays-lid), also when macOS
announces the Desktop still shown before it slides. Nothing joins: the
closed screen's strip goes with it, each card is one stage, its front window shows, and
the hidden windows macOS pushes whole onto the remaining screen at the change go back to its
corner (round displays-lid-2). The app macOS activates after the change (Mail, 0.4 s
after the screen went) switches nothing: with no click or key press since, an activation
or focus in the 10 s after a display change (heard as macOS announces it, not at the next
second's look) is macOS's, and the closed screen's front stays
in front. So is a window found then (macOS lists windows of the remaining screen's other
Desktop for a moment): it is placed once the change settles, if it still shows, and one
gone by then leaves no stage or card (round displays-lid-4). A Desktop the closed screen's front left for the one macOS slid to shows its own
most recent stage again when it comes back, never nothing in front (round displays-lid-3).
The lid opened: the
stages whose windows macOS moves back to that screen are its stages again (the one in front
before, a card; a stage moved whole stays one stage); the stage that came over in front
stays where it is. macOS puts each window back at the frame it had on that screen, a hidden
one at its old corner: a window put back in the open shows there, at that frame (never
moved to a frame carried over from the other screen), one put back at the corner is a card
still, that screen's card again even from another Desktop of the remaining screen, never
hidden again at the remaining screen's corner (round displays-lid-3), and a card put back
brings nothing forward; none of it is the user's drag. A stage
belongs to the screen its windows are on. Decided, not measured:
a window dragged out of a stage of several leaves the rest in their stage; ⌘-Tab or the
Dock to an app whose stage is on the other screen switches that screen only. Each screen's
hidden stages wait in a corner of that screen with no screen beyond it (the laptop's
bottom-left, its right edge touching the 5K). Each screen's strip samples its own desktop
picture for the cards' glass, each screen's hidden corner has a patch of its own (a still of
that screen's desktop where Screen Recording is allowed, else its own wallpaper's file),
`[windows] fit` keeps a window clear of its own screen's strip, a ⌘-Tab or Dock click to a
Desktop on either screen arranges that Desktop ahead of its slide, and a full-screen app or
Mission Control on one screen stops nothing on the other (round displays-2). The main
screen changing (the menu bar moved to the other screen, or the lid closed with the laptop
main) moves no stage: macOS moves every window with its screen, and each hidden window
comes back where it was on its screen. `[displays]
main_only` (off) is the first version's behaviour: only the main screen, a window moved off
it left alone.

**Mission Control, App Exposé and Show Desktop** hide the strip, as they hide Apple's. It
is back on the same Desktop when they end. The strip never appears in ⌘-Tab or the ⌘\`
window cycle.

**Wallpaper click.** The active stage goes into the strip, and a second click brings it
back. A double click counts as one click, and Finder becoming active after the click never
switches stages. The Dock's window spans its whole screen, clear away from its bar: a click
there is on the wallpaper (or the window) below it, and only one on or near the bar (its
magnified icons included) is the Dock's.

**Drag and drop.** A card dragged into the main area grows to window size and joins the
current stage where you drop it. A window dragged onto the strip becomes a stage of its own
in the slot under the pointer.

**Timing.** Every animation uses Apple's measured timing by default:

| Motion | Apple's timing |
|---|---|
| Windows in a switch, the desktop reveal, ⌘M and drops | critically damped spring, 99 % at about 400 ms |
| Cards gliding to new slots | spring, 99 % at about 375 ms |
| The strip sliding out and in | spring, 99 % at about 400 ms, 286 pt |
| Hover | grows 6 % in about 210 ms, its windows turning solid as it grows |
| Press | eases to 0.987 of its size in about 100 ms |

## Deliberate differences

**A menu bar icon.** Apple's Stage Manager has none; its toggle lives in Control Center. Ours
gives a place for Settings, About, updates and Quit. It can be hidden, and opening the app
again then shows Settings. Its glyph is original art, as tall as the SF Symbols beside it at
the menu bar's default size (12 pt of ink, their median).

**Launch at login.** Apple's is part of macOS. Ours registers at its first launch from
Applications, and Settings can turn that off. It registers as a login item
(`SMAppService.mainApp`). At login `loginwindow` first relaunches the apps of its reopen
list (about 3 s after the login) and holds the login items, and launchd agents with them,
until every one has checked in (12, 27 and 17 s on his Mac, 11-06): as a login item, or as
the launchd agent round early-start tried, Stage Manager opened 23-35 s after the login,
with most windows already up and piled. The list holds the regular (Dock) apps running at
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
Items."); a change of the key moves the registration once, and 1.6.x's agent moves back to
the login item. Within 90 s of the login, opening the app again (the login item after the
reopened copy) shows no Settings. An early launch waits for the screens a moment: the
displays' first reading (5 s at most) and, within a minute of the login, a screen the
stages were saved on that is not connected yet (10 s at most).

**Motion is drawn with pictures.** The public Accessibility API cannot scale a live window,
so switches are animated with snapshots in a click-through layer above everything, while
the real windows move behind them. Snapshots are refreshed every 2 seconds and as the pointer
nears a card, so content that changed meanwhile can jump when the motion ends. A click
during the motion brings the windows in at once; typing never cuts it short (the keyboard is
already the new stage's). An arriving window takes its place under its picture once the
picture as shown on screen is within a point of it on every edge (about 600 ms after the
click for a window growing from its card, 450 ms at the least: `[animation] unpark_at` is the
floor), so the hand-over shows no step. The picture comes to rest on the spring's own curve
(within a tenth of a point, about 750 ms for a 2000-point travel; Core Animation's settling
time stopped it 1 pt short and jumped, the owner's recording 11-01), and fades out over its
window over 200 ms a moment after the unveil, so a look that differs (a picture taken while
the window was parked and inactive, a window's own late redraw) melts instead of flipping.
The arriving pictures fade as one picture, overlapping as their windows do: a window behind
never shows through the one in front as both fade (each faded on its own showed the one
behind at a quarter at mid-fade, 11-02).
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
An app may draw its window in another appearance than the system's (11-02: Messages, a
conversation with a dark chat background, drew its whole window dark on a light system, and
the light shadow the system's appearance picked flickered at the handover). With no reading
the system's appearance stands in. The log's `transition ghost` line names the shadow each
picture carries and where its appearance came from. macOS also draws a thin white line just inside
each window's edge, with the shadow (a capture that leaves the shadow out lacks it): a point
wide at a tenth in Dark Mode (0.08 on an inactive window), half a point in light, where it
shows only on dark content. The picture carries it too, over its edge and corner, coming
and going with the window's shadow; without it every edge stepped lighter at the handover
(the owner, 11-01, Dark Mode). The hairline and the rim follow the curve macOS's shadow
follows, measured on the dark hairline (nearly opaque, so a tenth of a point shows): Core
Animation's continuous corner with a longer tail, the hairline a band just outside it, sharp
where the edge runs along the pixels and softer at 45 degrees; the radius is read from how
much of the picture's diagonal is clear. A filled shape grown by a spread put a darker notch
at every corner (up to 126 alpha levels). A window's own
shadow appears in one frame, so the picture keeps its shadow until the window server lists
the window on screen at its frame (asked every half frame from the unveil), then fades it
over 50 ms from a frame later, never before 50 ms past the unveil: both shadows for a few
frames read about 2 levels darker, while a shadow faded before its window showed read 16
lighter for 80 ms (the owner's third recording, 11-01; the window reached the screen a frame
after a fade centred on the unveil had ended). The picture goes only after that, 300 ms past
the unveil at the most; an outgoing picture's shadow comes in over twice
`[animation] cover_ms` as its window leaves. The cards' thumbnails are not captured while a
switch's pictures move (a round of captures is the window server's work, and dropped a frame).
With `[windows] hide_parked_apps`, a window of an app hidden in place shows only once our
unhide after the unveil has taken (about 100 ms later), when the window server lists it on
screen: its picture's shadow hands over then, except the key window's: it shows inactive
first, so its picture keeps the key shadow until the app is active and has focused it again
(drawn key then; 11-02, Telegram: handed over as it showed, a frame of the inactive shadow
alone read 4 levels lighter before it turned key, and the key shadow over the inactive one is
the smaller error, as the inactive one lies mostly within it). The picture stays until the
app has taken the activation of its key window focused again, so the window is never seen in
its inactive look (11-01: Messages flipped to its key look 180 ms after it showed); 400 ms
past the unveil at the most. An app that has not taken that activation 60 ms after its key window was focused
again is focused once more, twice at most (11-01: Messages's window took the focus, the app
never became active, and the keyboard stayed with the app before).
**Deviation (superseded switches, 11-02):** a switch overtaken before it lands (another card
click, a click of yours anywhere, an activation, a focus) still brings its windows, but
never focuses one or activates its app afterwards: its held focus, the focus again after its
app's unhide and the retries above are dropped, so the keyboard is the newer switch's or
where you clicked (11-02: two card clicks 0.56 s apart on two hidden apps' stages; the first
app's window was focused 120 ms into the second switch and the two apps took the activation
in turn). A window's focus brings its stage only in the active app (or with the button down
on a sliver), never in a hidden app or an app behind: those are the app's own or a late echo
of ours (with apps slow to answer, notices came up to a second late, past the echo window,
and each switched stages). A window on screen when Stage Manager started but found late (its
app not answering then) goes to its stage as during the restore window, parked when that
stage is inactive, never bringing it forward.
Arriving windows
wait in the corner behind their pictures, and
that corner is covered with the wallpaper patch's picture until they are in place (macOS
raises an activated app's windows there) and the patch itself shows again (it is drawn a
moment before the cover goes, at most 300 ms after the motion; never where an arriving window
landed over the corner), never over the menu bar or the Dock's bar, and
never more than the corner itself (a 320-point square). Where
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

**Card pictures are stills**, refreshed every 1.5 seconds, not live surfaces.

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
card-sized picture of it, where Apple shrinks the window itself. A card dragged out is shown
by a growing picture of its front window.

**⌘M** parks the window and then un-minimizes it there, so macOS may show its restore
animation toward the corner. Apple's window shrinks straight into the strip. macOS's
un-minimize focuses the window and may set it down short of the corner: it is parked again
where it rests, never taken for a window you brought out (only a focus or a drag of yours
after the park counts as that).

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
apps reopen them each goes back to its stage, at its saved frame when the main screen has
the saved size, parked only on a confident match (a doubtful one goes by Apple's rule and
stays in view or gets a card of its own). While the login settles (until apps have been
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
needs its app's answer). At a logout, restart or shutdown
every window comes back at the first sign of it macOS gives (any of `loginwindow`'s logout
notices, `willPowerOff` the last), so apps save their windows where they are as far as the
timing allows (best effort: macOS may ask apps to quit first); from then on nothing is
arranged (no switch, no card added or removed) and the app itself quits when macOS asks it
to; a cancelled logout brings the stages back at once (one macOS says nothing of: two minutes
after its latest sign, or after the latest app that quit meanwhile, so a slow restart, an app
asking about unsaved changes, is not taken for one). An app that quits keeps its windows'
places in the saved stages for two minutes (no card meanwhile), so stages whose apps quit
before macOS announced the logout still come back after the restart; a window reopened
squeezed into the park corner goes to its saved frame when it is matched, and is left where
it is (and counted) when it is not. A window an app reopens on another Desktop than the one
it was saved on (non-native apps reopen on the Desktop shown) goes into its saved stage made
again on the Desktop where it shows. A window reopened on another screen than its saved
stage's, that screen connected, goes back to that screen, at its saved frame there, in its
stage, in front or a card as saved (11-06: the laptop's front window, reopened by its app
on the 5K; Apple's keeps no stages over a restart, so this decides nothing of Apple's); a
stage saved on a Desktop of that screen not shown now is made again where the window
shows, as above, and a window reopened on its own screen stays where its app put it. A
Desktop is found again by its windows; a screen macOS keeps one Desktop for (read from
`com.apple.spaces`, never written) is the Desktop saved on that screen even before any of
its windows show, so they go back to their stages there as their apps reopen them, and one
reopened on the other screen comes back to it (11-06 08:27: the apps listed no window yet
when Stage Manager started; the laptop's windows counted as from another Desktop and its
front stayed on the 5K). A screen with several Desktops, or two Desktops saved for a
screen that keeps one now, is left to the windows. A
window that went by Apple's rule (no saved window matched it: an Electron app's window
still titled with its app's name, say) or by a doubtful match is looked at again when its
title changes, and goes to its saved stage on a confident match, a card when that stage is
not in front, nothing switching. This holds per window, until two minutes after the login
(after the launch, for Stage Manager started later): a click on one window, which ends the
settle, leaves the others to it, since apps are still opening then. A window the user has
touched stays where it is: one focused by a click or a key press on it, typed in, moved or
resized, or whose stage was switched to (a card click, a Dock click, ⌘-Tab, a shortcut). A saved
window that shows only after the settle still goes to its stage, as above; that wait is
bounded per app, not by the two minutes (its entries go a minute after its app's last new
window, at the settle's end when its app is not running, and at the next session; a session
in which its Desktop never showed does not count, up to 30 days since that Desktop last
showed). An entry of a Desktop not shown yet stays through that minute for the Desktop's
first show, its app's minute starting again there, but from then on a window shown on
another Desktop is a new window by Apple's rule, never a stage of that Desktop's. Saved
frames come back on the screen they were saved on, at the same place on it when the screens
were rearranged in between. A title that only puts
something in front of the saved one ("main.rs — proj" for a saved "proj", an editor showing
a file in its workspace) is the same name, so the frame the window reopened at counts. While
the restore window is open (no click or key yet, so every window that shows is one an app
reopens) a window under another name at the exact saved frame of a saved window of its app
is that window, when nothing else can be meant and the app is reopening its saved
windows: the only one of the app waiting at that frame, no other window at it, and another
window of the app back in its saved stage (a browser's first window, a Start Page where a
saved page was, is not that page; a window that showed before its sibling is looked at again
when the sibling comes) (11-06: Fork's two windows, saved as two stages on two
Desktops, reopened on the one shown, the second showing another repository: it joined the
first's stage by Apple's rule; now it gets its own stage there). After the restore window
the frame never outweighs two names: a window the user opens (Finder's ⌘N where a saved
window was) keeps Apple's rule. `[stages] new_apps_join_current` never applies while the
restore window is open: an app reopening its windows is not one the user opened. No
apps are relaunched (that is macOS's
"Reopen windows when logging back in") and no window moves between Desktops (no public API
does); nothing lists those windows for the user, who could not act on such a list (his
word 11-06; 1.6.0's menu list is gone). When the restore window ends (no sooner than a minute after the login), the saved windows of apps
not running then are given up (one log line names them): an app opened later goes by the
new-window rules, never back to a stage of the last session. A running app keeps its saved
windows until it has opened none for a minute. Off, a restart
starts every app in a stage of its own, as Apple's does. One log line sums it up, ending
with the windows back on another Desktop (bundle id, hashed title, the Desktops' ids;
`stage-manager log summary` repeats the counts).

## Known gaps

- The lid closed and opened (above) is what Apple's does, built and unit tested, not yet
  seen live with ours; so are a second screen's own card glass, corner patch and desktop
  still, and a full-screen app on one screen while windows arrive on the other (round
  displays-2, built from the one-screen code, checked by the suite's `displays` phase).
- Stage hotkeys and `ctl switch N` count and switch the main screen's strip, whichever
  screen has the keyboard; which screen Apple's hotkeys address is not measured.
- No public API names a Desktop, so Desktops are told apart by their windows. An empty
  Desktop is forgotten when you leave it, and one whose windows are all hidden or minimized
  looks new until one of them shows.
- A Dock click to another Desktop arranges it before it slides in. ⌘-Tab cannot do the same
  yet: no public notice names the chosen app early enough, so the Desktop is arranged when
  it arrives.
- A Desktop's strip appears after the Desktop has slid in on its first visit; Apple's slides
  in with it.
- A dialog's parent window is a guess: the app's frontmost window at the time.
- New windows are matched to rules by the title they have when they appear.
- Windows of a relaunched app are matched to their stages by title and frame; two of one
  app with the same title and no telling frame stay in view rather than guess.
- With "Click wallpaper: Only in Stage Manager", a click on a desktop icon also counts as a
  wallpaper click.
- Since macOS 15, macOS asks about once a month whether the app may capture windows
  directly. Apple's content picker avoids that alert but cannot serve a strip of every
  window.
- Not measured yet on Apple's: the edge-reveal and auto-hide delays (ours: 250 ms and
  500 ms), the hover's exact easing, a new card's fade, a Shift-click on a card holding
  several apps, and the cards' shadow and corner radius.
- Where Apple places a window moved to another Desktop is unverified. Ours places it as a
  window of its app opening there: in that app's most recent stage on that Desktop, else a
  stage of its own, shown in front. Several apps moved at once get a stage each; the front
  one shows, the rest wait in the strip.
