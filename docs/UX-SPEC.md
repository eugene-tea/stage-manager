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
laptop, 6 on a 5K display.

**Strip order is slot-stable.** A clicked card's stage becomes active, and the stage you
leave takes the slot the clicked card vacated. A stage entering a full strip takes the slot
of the least recently used card.

**Switching.** The pressed card eases back on mouse-down. On release, the windows you leave
shrink into their card's slot and the new stage grows from its card, on Apple's spring.
Shift-click a card to bring its windows into the current stage; Option-click does nothing.
Clicking a card's app icon brings every window of that app, from all stages.

**Activation.** ⌘-Tab, ⌘⇧-Tab or a Dock click to an app in another stage switches to that
stage.

**New windows.** A window of an app already on screen joins its stage. A window of an app in
a waiting stage brings that stage forward. A newly opened app gets a stage of its own.
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
again then shows Settings.

**Launch at login.** Apple's is part of macOS. Ours registers as a login item at its first
launch from Applications, and Settings can turn that off.

**Motion is drawn with pictures.** The public Accessibility API cannot scale a live window,
so switches are animated with snapshots in a click-through layer above everything, while
the real windows move behind them. Snapshots are refreshed every 2 seconds and as the pointer
nears a card, so content that changed meanwhile can jump when the motion ends. A click or
key press during the motion brings the windows in at once.

**Card pictures are stills**, refreshed every 1.5 seconds, not live surfaces.

**Hidden stages wait in a corner.** Without private APIs, the only way to hide a window but
keep it ready is to move it almost off screen. macOS keeps a thin edge of each such window
visible in the bottom-right corner (another corner if a display sits beyond it). To keep
the corner as clean as Apple's, those edges ignore clicks and a patch drawn from the
wallpaper's own file covers them, without screen recording. The patch steps aside where it
cannot match exactly.

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

Shift never changes the app switcher: ⌘-Tab and ⌘⇧-Tab always switch stages.

**After a restart.** The restore is invisible when it works and Apple's behaviour when it
cannot match. Saved windows not on screen at launch wait, with no card, in their stages; as
apps reopen them each goes back to its stage, at its saved frame when the main display has
the saved size, parked only on a confident match (a doubtful one goes by Apple's rule and
stays in view or gets a card of its own). While the login settles (until apps have been
quiet for 20 s and the login is a minute old, the first click or key press, or 5 minutes)
launch-time activations and new windows switch nothing and animate nothing, and cards come
in quietly in their saved order; then the stage left in front shows with its key window
focused, unless a window shown there already has the keyboard. At a logout, restart or
shutdown every window comes back before apps save theirs. No apps are relaunched (that is
macOS's "Reopen windows when logging back in") and no window moves between Desktops. Off,
a restart starts every app in a stage of its own, as Apple's does. One log line sums it up
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
