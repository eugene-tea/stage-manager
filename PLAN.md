# Stage Manager — plan

What is still to do. What shipped is in [CHANGELOG.md](CHANGELOG.md); how the app is built in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md); how it should behave in
[docs/UX-SPEC.md](docs/UX-SPEC.md); what users asked for in
[docs/FEATURE-REQUESTS.md](docs/FEATURE-REQUESTS.md).

## Next version

- **"Only the main screen" in Settings** (`[displays] main_only`, off; R8). Read at launch,
  so the switch quits (every window back) and reopens the app, as an update does; its line
  says so.
- **The corner patch's Settings text** still says "until checked live" (`schema.rs`; a new
  legacy default file).

## Feature requests not built

From FEATURE-REQUESTS §2: a right-click menu on cards (R4), naming stages from the UI (R7),
the strip scrolling past its capacity with "+N" (R5), no unasked stage switches (R6), a
stage list in the menu (R2), dropping a window on a card (R3b), the parity checks.

## Check live with the owner

- **Screens:** ⌘-Tab to the other screen's stage; the second screen's card glass and corner
  patch; a full-screen app and Mission Control on one screen; a window moved to another
  Desktop there.
- **Windows clear of the strip** (`[windows] fit`), with the owner, who has not tried it.
- **After a real restart, from the day's log:** no window reopens parked; "logout signal"
  lines against the first "app terminated" (then trim `SIGNALS`); a slow app in the saved
  front stage; windows placed within ms of being found (no "queued_ms" over ~100).
- **Log caps** (100 MB a day, 500 total, 14 days) against real days (`stage-manager log
  summary`); the suite's estimate is ~1 MB a day, 2–3 MB heavy.
- **Card glass not measured:** Dark Mode; a window behind another on one card; the hover's
  growth (Apple's ~11 %) against ours.

## Find out

- **What heals Accessibility blindness.** After a WindowServer watchdog kill, Accessibility
  listed no window (System Events neither) until our quit and a lock/unlock, unchecked
  between. Next time: quit Stage Manager, check System Events, then lock and unlock. If the
  quit heals it, the menu line says "Quit and reopen Stage Manager" instead of "Log out",
  and the bug is ours.
- **Logout cancel gaps.** A cancel in the milliseconds before our exit is missed; a logout
  where not every window came back does not relaunch.
- **The Dock icon after a cancelled logout.** AppKit's NO to the switch back to accessory
  at a real cancel was not reproduced outside a logout. The fix retries for about 10 s; if
  its INFO line shows tries > 1 or `done=false` at a real cancel, look again.
- **Card switches drawn wrong once** at a first launch after a login (a relaunch cured it).
  If it returns: a screen recording, and what the overlay and strip panels hold.
- **Overlay over a raised strip:** a switch animates just under the strip then, so it may
  cover an always-on-top window.
- **Full-screen windows in the closed-window rule:** a dead handle off screen at about a
  display's size is kept (`stale_handle::on_full_screen_space`); the window's last read
  AXFullScreen would retire the size test.
- **Saving stages off the window thread.** `state.json` is flushed to disk on the WM thread,
  up to every 3 s while windows move (`controller.rs`, `durable.rs`). Move the flush to its
  own thread, still finished before the quit's save.
- **The switch cover gives up too often.** During a switch, the still painted over the
  hiding corner refuses when a window not ours without a picture reaches the corner, a
  window lands outside its 320 pt square, or the strip is hidden (`corner_patch.rs`). It
  also still leaves the Dock's area out, from before the overlay moved below the Dock, so a
  translucent Dock can show a hidden window's sliver.
- **An update on a non-APFS disk.** A forced kill between the updater's two renames leaves
  no app at the install path (`updater.rs`). A step journal, or an APFS check, would close it.

## To decide

- **Short-lived windows** (Safari's small dialog, Chrome's address-bar suggestions) are
  taken into a stage and let go a second later, many times a day; Apple's shows neither on
  a card. Are such windows ever managed?
- **`hide_parked_apps` stays off:** a hidden app ⌘-Tabbed to skips the switch's motion, and
  its live phase left an empty stage. Look there before offering it.

## Tests to write

- **Live checks never written:** an off-edge window, one tucked by the corner, a Dock change,
  a dialog born parked; a late answer at quit, `kill -9` with a dialog, a fitted move after
  a crash, a reopen at the corner frame; a broken config at launch; captures stopping with
  the strip hidden, edge reveal, the tab across Spaces.
- **The suite and NotificationCenter:** a banner's window over the strip (or the owner's
  unmanaged windows) makes click checks refuse. Wait for it; name it when it stays.

## Decided, left as is

- **Medium matches in the restore:** windows shown after the launch get saved frames on a
  medium match too (the saved size only on a strong one).

## Out of scope

A unified stage across screens (Apple's has none); full-screen windows (ignored). UX-SPEC's
measurement record of Apple's Stage Manager is that file as of commit 20665a3.
