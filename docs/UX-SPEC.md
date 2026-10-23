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
brings every window back at exactly its own frame, even where it overlaps the strip.

**The strip.** It sits below windows: a window moved over it covers it, and the strip slides
away. Resting the pointer at the left edge brings it back. Each card draws the stage's
windows at one scale, turned in perspective toward the middle of the screen, with the app
icon on its bottom-left corner. The number of cards depends on the screen height: 5 on a
laptop, 6 on a 5K display. A popup open on a window (a popover, a child window sticking out
of it) shows on its card beside the window, at its true size and place, as on Apple's; the
window keeps its own size on the card, and nothing outside a window is drawn. A palette
far from its window (more than the window's own width or height past it, or on another
display) is left off: the card shows the window alone. So is a popup over a window that
macOS's capture does not place (round 8M, Calendar's event popup reaching past the window's
top: the capture's content said the window's frame, its image held both squeezed into the
window's shape on a fill): never a fill around a window on a card.

**Strip order is slot-stable.** A clicked card's stage becomes active, and the stage you
leave takes the slot the clicked card vacated. A stage entering a full strip takes the slot
of the least recently used card. (Keep stages in place, an extra below, orders it otherwise.)

**Switching.** The pressed card eases back on mouse-down. On release, the windows you leave
shrink into their card's slot and the new stage grows from its card, on Apple's spring.
Shift-click a card to bring its windows into the current stage; Option-click does nothing.
Clicking a card's app icon brings every window of that app, from all stages.

**Activation.** ⌘-Tab, ⌘⇧-Tab or a Dock click to an app in another stage switches to that
stage, animated as a card click: the stage you leave shrinks into the strip and the app's
windows grow from their card. The keyboard goes to the app's window at once.

**New windows.** A window of an app already on screen joins its stage. A window of an app in
a waiting stage brings that stage forward. A newly opened app gets a stage of its own (an
extra, off by default, puts it in the current stage instead: see Extras).
Sheets and dialogs follow their window.

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
Desktop shows that window's stage. Full-screen apps have no strip.

**Mission Control, App Exposé and Show Desktop** hide the strip, as they hide Apple's. It
is back on the same Desktop when they end. The strip never appears in ⌘-Tab or the ⌘\`
window cycle.

**Wallpaper click.** The active stage goes into the strip, and a second click brings it
back. A double click counts as one click, and Finder becoming active after the click never
switches stages.

**Drag and drop.** A card dragged into the main area grows to window size and joins the
current stage where you drop it. A window dragged onto the strip becomes a stage of its own
in the slot under the pointer.

**Timing.** Every animation uses Apple's measured timing by default:

| Motion | Apple's timing |
|---|---|
| Windows in a switch, the desktop reveal, ⌘M and drops | critically damped spring, 99 % at about 400 ms |
| Cards gliding to new slots | spring, 99 % at about 375 ms |
| The strip sliding out and in | spring, 99 % at about 400 ms, 286 pt |
| Hover | grows 6 % in about 210 ms |
| Press | eases to 0.987 of its size in about 100 ms |

## Deliberate differences

**A menu bar icon.** Apple's Stage Manager has none; its toggle lives in Control Center. Ours
gives a place for Settings, About, updates and Quit. It can be hidden, and opening the app
again then shows Settings. Its glyph is original art, as tall as the SF Symbols beside it at
the menu bar's default size (12 pt of ink, their median; round 8N).

**Launch at login.** Apple's is part of macOS. Ours registers as a login item at its first
launch from Applications, and Settings can turn that off.

**Motion is drawn with pictures.** The public Accessibility API cannot scale a live window,
so switches are animated with snapshots in a click-through layer above everything, while
the real windows move behind them. Snapshots are refreshed every 2 seconds and as the pointer
nears a card, so content that changed meanwhile can jump when the motion ends. A click
during the motion brings the windows in at once; typing never cuts it short (the keyboard is
already the new stage's). Arriving windows wait in the corner behind their pictures, and
that corner is covered with the wallpaper patch's picture until they are in place (macOS
raises an activated app's windows there) and the patch itself shows again (it is drawn a
moment before the cover goes, at most 300 ms after the motion; never where an arriving window
landed over the corner), never over the menu bar or the Dock's bar, and
never more than the corner itself (a 320-point square; 10-16, covers of most of the screen
painted the desktop over the owner's windows at every switch). Where
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
windows land); the outgoing stage's windows never count. Where it cannot be covered at all (the patch off, card captures off with icons there,
no picture ready yet, a staying window with no picture of it, such as another app's the
app does not manage, an activated window macOS put outside that square), an activated app's
windows come to their places at once instead of growing.

**Card pictures are stills**, refreshed every 1.5 seconds, not live surfaces.

**Hidden stages wait in a corner.** Without private APIs, the only way to hide a window but
keep it ready is to move it almost off screen. macOS keeps a thin edge of each such window
visible in the bottom-right corner (another corner if a display sits beyond it). To keep
the corner as clean as Apple's, those edges ignore clicks and a patch drawn from the
wallpaper's own file covers them, without screen recording. The patch steps aside where it
cannot match exactly. Each Desktop has its own patch, which stays on it and slides with it
when you go to another Desktop and back, drawn as it was (nothing moves on a Desktop while
you are away); it is checked again once the Desktop has arrived, and hidden while away if
the picture's frame changed meanwhile (the sun, the appearance) or a desktop icon came
under it. A Desktop's first visit still shows its edges for a moment, until its patch is
drawn.

**Dragging uses stand-ins.** A window dragged over the strip keeps its size under a
card-sized picture of it, where Apple shrinks the window itself. A card dragged out is shown
by a growing picture of its front window.

**⌘M** parks the window and then un-minimizes it there, so macOS may show its restore
animation toward the corner. Apple's window shrinks straight into the strip.

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

Shift never changes the app switcher: ⌘-Tab and ⌘⇧-Tab always switch stages.

**Keep stages in place** (Settings → Stages → Strip order: Keep in Place; `[strip] order =
"in_place"`, round 9B). Apple's
swap leaves the stages in click order, so a stage's place cannot be predicted. In place,
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
"dock"`, round 9D). The places of Keep stages in place, ordered by the Dock: the strip's
top-to-bottom order is the Dock's left-to-right order (top to bottom for a Dock on the side).
The Dock's application items are read through Accessibility (its lists' `AXDockItem`
children and their `AXURL`), so running apps that are not kept in the Dock count where their
icons show; its preferences are never read or written. A stage sits at the leftmost Dock
position among its windows' apps (by bundle id, else by pid); stages at the same position
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
apps reopen them each goes back to its stage, at its saved frame when the main display has
the saved size, parked only on a confident match (a doubtful one goes by Apple's rule and
stays in view or gets a card of its own). While the login settles (until apps have been
quiet for 20 s, none still opening its first window, and the login is a minute old; the
first click or key press; or 5 minutes) launch-time activations and new windows switch
nothing and animate nothing, and cards come in quietly in their saved order; then the stage
left in front shows with its key window focused, unless a window shown there already has
the keyboard. While that stage is still empty (its app slow to open), a window opening a
stage of its own takes the front instead of a card beside an empty screen; the saved stage
then waits, and its windows get a card when they come. At a logout, restart or shutdown
every window comes back at the first sign of it macOS gives (any of `loginwindow`'s logout
notices, `willPowerOff` the last), so apps save their windows where they are as far as the
timing allows (best effort: macOS may ask apps to quit first); from then on nothing is
arranged (no switch, no card added or removed) and the app itself quits when macOS asks it
to; a cancelled logout brings the stages back at once. An app that quits keeps its windows'
places in the saved stages for two minutes (no card meanwhile), so stages whose apps quit
before macOS announced the logout still come back after the restart; a window reopened
squeezed into the park corner goes to its saved frame when it is matched, and is left where
it is (and counted) when it is not. A window an app reopens on another Desktop than the one
it was saved on (non-native apps reopen on the Desktop shown) goes into its saved stage made
again on the Desktop where it shows. No apps are relaunched (that is macOS's
"Reopen windows when logging back in") and no window moves between Desktops. Off, a restart
starts every app in a stage of its own, as Apple's does. One log line sums it up
(`stage-manager log summary` repeats it).

## Known gaps

- Only the main display is managed.
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
- Windows of a relaunched app are matched to their stages by title, so two with the same
  title may swap.
- With "Click wallpaper: Only in Stage Manager", a click on a desktop icon also counts as a
  wallpaper click.
- Since macOS 15, macOS asks about once a month whether the app may capture windows
  directly. Apple's content picker avoids that alert but cannot serve a strip of every
  window.
- Not measured yet on Apple's: the edge-reveal and auto-hide delays (ours: 250 ms and
  500 ms), the hover's exact easing, a new card's fade, a Shift-click on a card holding
  several apps, the cards' shadow and corner radius, and the shadow of moving windows.
- Where Apple places a window moved to another Desktop is unverified. Ours places it as a
  window of its app opening there: in that app's most recent stage on that Desktop, else a
  stage of its own, shown in front. Several apps moved at once get a stage each; the front
  one shows, the rest wait in the strip.
