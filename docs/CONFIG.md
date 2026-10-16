# Stage Manager — configuration reference

Every option lives in `~/.config/stage-manager/config.toml`. The file lists every key below,
commented out with its default; remove the `#` to change one. Saved changes apply within a
second, and a mistake keeps the last working settings. Settings already in System Settings →
Desktop & Dock (show recent apps, show an app's windows all at once or one at a time, click
wallpaper to reveal desktop) are read from there and are not repeated here. The
[user guide](GUIDE.md#the-config-file) shows how to edit the file, with examples.

From Terminal (the app's binary is `/Applications/Stage Manager.app/Contents/MacOS/stage-manager`,
or `stage-manager` when installed with Homebrew):

```sh
stage-manager config check                 # check the file; errors name their line
stage-manager config --explain cards.tilt  # one key in detail
stage-manager config --print-defaults      # the default file
stage-manager config --reference           # the table below
```

Rules for new windows are `[[rule]]` tables, checked in order; the first match wins. `app` is
a bundle ID or `"*"`, `title` an optional regular expression, and `action` and `placement`
take the same values as `[defaults]` below:

```toml
[[rule]]
app = "com.apple.Terminal"
title = "^Scratch"
action = "float"
placement = "center"
```

`[animation] speed` multiplies every animation's timing in `[animation]`, `[motion]`, `[cards]`
and `[drag]`. Their defaults are Apple's, measured.

| Key | Default | Values | What it does |
|---|---|---|---|
| `app.launch_at_login` | `true` | true or false | Open Stage Manager when you log in (a login item; System Settings → General → Login Items can also turn it off). Only an installed copy (in Applications) registers. |
| `app.show_menu_bar_icon` | `true` | true or false | Show the menu bar icon (Apple's Stage Manager has none). Without it, open Stage Manager again from Applications to reach Settings. |
| `app.check_for_updates` | `true` | true or false | Check GitHub for a newer release at most once a day and offer it; nothing about you or this Mac is sent. Check for Updates… in the menu works either way. |
| `app.install_updates` | `false` | true or false | Install a newer release without asking: downloaded and verified in the background, installed while you are away from the Mac (windows come back first, then the new version opens). Off: the app asks first. |
| `strip.order` | `"native"` | "native", "fixed" | Card order: "native" is Apple's (the stage you leave takes the clicked card's slot); "fixed" is creation order. |
| `strip.level` | `"below_windows"` | "below_windows", "above_windows" | Where the strip sits: "below_windows" is Apple's (a window moved over it covers it); "above_windows" keeps it over windows. |
| `strip.reorder` | `true` | true or false | Drag a card up or down within the strip to give it another slot (an extension; Apple's cannot, invisible until used). |
| `strip.merge_on_drop` | `true` | true or false | Drop a card onto another card to merge the two stages (an extension; Apple's draws no such drop, invisible until used). |
| `strip.show_active` | `false` | true or false | Also show the active stage as a card (fixed order only; Apple never does). |
| `strip.hide_while_desktop_shown` | `false` | true or false | Hide the strip while a wallpaper click shows the desktop; it comes back on any window activation. Apple keeps it shown. |
| `strip.show_numbers` | `false` | true or false | Draw each card's stage number (for the hotkeys). Apple shows none. |
| `strip.show_names` | `false` | true or false | Draw stage names under cards. Apple shows none. |
| `strip.width` | `200.0` | number, 60 to 800 | Width of the strip column: the panel, and the area a window must cover to hide the strip. |
| `strip.capacity` | `0` | whole number, 0 to 30 | Most cards shown; 0 counts them from the screen's visible height (Apple: 5 at 1130 pt, 6 at 1410 pt). |
| `strip.capacity_pitch` | `190.0` | number, 20 to 2000 | With capacity = 0: room per card, in points of visible height. |
| `strip.capacity_margin` | `60.0` | number, 0 to 2000 | With capacity = 0: visible height kept free above and below the cards. |
| `strip.capacity_min` | `3` | whole number, 1 to 30 | With capacity = 0: never fewer cards than this. |
| `strip.capacity_max` | `9` | whole number, 1 to 30 | With capacity = 0: never more cards than this. |
| `strip.scale` | `1.0` | number, 0.25 to 4 | Multiplies every strip length (cards, gaps, icons, width). Apple: 1. |
| `strip.scale_to_display` | `false` | true or false | Also shrink strip lengths on screens smaller than 2560 x 1410 pt (by area, at least half). Apple does not. |
| `strip.thumbnail_interval_ms` | `1500` | whole number, 250 to 60000 | How often thumbnails are refreshed, in milliseconds. |
| `strip.thumbnail_resolution` | `2.0` | number, 0.5 to 4 | Thumbnails are captured at this multiple of their drawn size. |
| `strip.capture` | `true` | true or false | Capture pictures of windows (Screen Recording) for card thumbnails and switch animations; false draws plain cards and ghosts and never uses screen capture. |
| `cards.x` | `15.0` | number, -100 to 400 | Left edge of every card, from the screen's left edge. |
| `cards.full_height` | `161.0` | number, 10 to 1000 | A window as tall as the visible screen becomes a card this tall; every window gets the same scale. |
| `cards.min_height` | `97.0` | number, 0 to 1000 | Small windows are drawn at least this tall. |
| `cards.max_width` | `0.0` | number, 0 to 2000 | Widest card before the turn; 0 = as wide as a card of the whole screen. |
| `cards.gap` | `29.5` | number, 0 to 500 | Space between cards. The column is centered on the visible screen. |
| `cards.tilt` | `"auto"` | "auto" or a number, 0 to 85 | Turn of each card in degrees; "auto" is Apple's for the screen (44.2 at 1800 pt wide, 47.9 at 2560). |
| `cards.perspective` | `"auto"` | "auto" or a number, 50 to 50000 | Eye distance in points; "auto" is Apple's for the screen (1346 at 1800 pt wide, 2260 at 2560). |
| `cards.perspective_x` | `15.0` | number, -3000 to 3000 | The eye's horizontal position, from the screen's left edge. |
| `cards.perspective_y` | `0.0` | number, -3000 to 3000 | The eye's height, from the screen's vertical middle (down is positive). |
| `cards.corner_radius` | `4.0` | number, 0 to 60 | Corner radius of each window's thumbnail. |
| `cards.border_width` | `0.5` | number, 0 to 10 | Width of the thin edge around each thumbnail. |
| `cards.border_color` | `"#ffffff2e"` | color "#rrggbb" or "#rrggbbaa" | Color of that edge. |
| `cards.shadow_radius` | `4.0` | number, 0 to 100 | Blur radius of each thumbnail's shadow. |
| `cards.shadow_opacity` | `0.4` | number, 0 to 1 | Opacity of that shadow. |
| `cards.shadow_offset_x` | `0.0` | number, -100 to 100 | Horizontal offset of that shadow. |
| `cards.shadow_offset_y` | `2.0` | number, -100 to 100 | Vertical offset of that shadow (down is positive). |
| `cards.shadow_color` | `"#000000"` | color "#rrggbb" or "#rrggbbaa" | Color of that shadow. |
| `cards.max_windows` | `3` | whole number, 1 to 8 | Most windows drawn on one card. |
| `cards.stack_dx` | `17.0` | number, -200 to 200 | Windows of one app step this far right toward the front one (the one behind peeks out on the left). |
| `cards.behind_scale` | `1.0` | number, 0.3 to 1 | Each window behind another of the same app is drawn this much smaller. |
| `cards.second_app_dx` | `68.0` | number, -300 to 300 | A second app's window sits this far right of the first. |
| `cards.second_app_dy` | `14.0` | number, -300 to 300 | ...and this far up. |
| `cards.hover_scale` | `1.06` | number, 1 to 2 | The card under the pointer grows by this factor, about its center. |
| `cards.hover_ms` | `210` | whole number, 0 to 3000 | How long the hover growth (and its undoing) takes, eased out, in milliseconds (Apple's, measured: 208 growing, 220 shrinking). |
| `cards.press_scale` | `0.987` | number, 0.5 to 1 | A pressed card eases to this size of its resting one, from the mouse-down (Apple's, measured: 0.987, the hover growth undone and a little more). |
| `cards.press_ms` | `100` | whole number, 0 to 3000 | How long the press takes, eased out, in milliseconds (Apple's, measured: 90 % at 72 ms, done by 100). |
| `cards.fade_ms` | `200` | whole number, 0 to 3000 | How long a new card takes to fade in, in milliseconds (not measured on Apple's). |
| `icons.size` | `38.0` | number, 0 to 300 | Size of the app icon image (macOS icons carry a margin; about 31 pt shows). 0 hides icons. |
| `icons.center_dx` | `10.5` | number, -300 to 300 | The icon's center, right of the card's bottom-left corner (its leftmost window's). |
| `icons.center_dy` | `-10.5` | number, -300 to 300 | The icon's center, below the card's bottom-left corner (up is negative). |
| `icons.step` | `36.0` | number, 0 to 300 | A second app's icon sits this far right of the first. |
| `icons.max` | `3` | whole number, 0 to 10 | Most icons on one card. |
| `icons.shadow_radius` | `4.0` | number, 0 to 100 | Blur radius of the icon's shadow. |
| `icons.shadow_opacity` | `0.3` | number, 0 to 1 | Opacity of the icon's shadow. |
| `icons.shadow_offset_y` | `2.0` | number, -100 to 100 | Vertical offset of the icon's shadow (down is positive). |
| `motion.spring_stiffness` | `289.0` | number, 1 to 10000 | Spring stiffness of cards gliding to new slots (a switch's swap, a reorder, a drop making room). Apple's, measured: 289 (99 % by about 375 ms). |
| `motion.spring_damping` | `34.0` | number, 0 to 1000 | Spring damping of the cards' glide (34 with stiffness 289 and mass 1: critically damped, no overshoot). |
| `motion.spring_mass` | `1.0` | number, 0.01 to 100 | Spring mass of the cards' glide. |
| `motion.slide_stiffness` | `282.0` | number, 1 to 10000 | Spring stiffness of the strip sliding out and back in (auto-hide, a window over it). Apple's, measured: 282 (99 % by about 400 ms). |
| `motion.slide_damping` | `33.6` | number, 0 to 1000 | Spring damping of the slide (33.6 with stiffness 282 and mass 1: critically damped). |
| `motion.slide_mass` | `1.0` | number, 0.01 to 100 | Spring mass of the slide. |
| `motion.slide_distance` | `286.0` | number, 0 to 3000 | How far left the cards slide when the strip hides. |
| `reveal.edge_width` | `2.0` | number, 0 to 100 | The pointer within this many points of the left edge counts as at the edge. |
| `reveal.delay_ms` | `250` | whole number, 0 to 10000 | Resting at the left edge this long brings a hidden strip back. |
| `reveal.hide_delay_ms` | `500` | whole number, 0 to 10000 | A strip brought back hides again this long after the pointer leaves it. |
| `reveal.poll_ms` | `60` | whole number, 10 to 1000 | How often the pointer is checked while the strip is hidden. |
| `reveal.edge_tab` | `false` | true or false | Show a slim tab at the left edge while a window hides the strip (an extension). |
| `reveal.launch_reveal` | `false` | true or false | Show a covered strip for a moment at launch (an extension). |
| `reveal.launch_reveal_ms` | `3000` | whole number, 0 to 60000 | How long that launch showing lasts. |
| `drag.threshold` | `5.0` | number, 0 to 100 | A press that moves farther than this (points, at least 5) drags the card; less is a click. |
| `drag.merge_zone` | `0.5` | number, 0 to 1 | With merge_on_drop: the middle part of a card's height (0.5 = the middle half) where a dragged card drops onto it; above and below, it goes between cards. |
| `drag.grow_distance` | `260.0` | number, 1 to 3000 | Past the strip, the dragged card grows to its window's size over this distance. |
| `drag.lifted_opacity` | `0.3` | number, 0 to 1 | Opacity of a card in the strip while it is dragged out. |
| `drag.ghost_opacity` | `0.92` | number, 0 to 1 | Opacity of the dragged copy of the card, and of a window's card while it is dragged over the strip. |
| `drag.merge_ms` | `200` | whole number, 0 to 3000 | How long a card dropped onto another takes to shrink into it and fade, in milliseconds (an extension; Apple's has no merge). |
| `animation.enabled` | `true` | true or false | Animate as Apple does: switches, the desktop reveal, Cmd-M and drops, and the strip's slides, glides, hover and press. false: everything jumps. |
| `animation.speed` | `1.0` | number, 0 to 4 | Speed of every animation: 1 is Apple's, 2 twice as fast, 0.5 half as fast (0.25 to 4); 0 is no animation, as enabled = false. It multiplies every spring and duration of [animation], [motion], [cards] and [drag]. |
| `animation.spring_stiffness` | `272.0` | number, 1 to 10000 | Spring stiffness of the windows' motion in a switch, the desktop reveal, Cmd-M and drops. Apple's, measured: 272 (99 % by about 400 ms). |
| `animation.spring_damping` | `33.0` | number, 0 to 1000 | Spring damping of the windows' motion (33 with stiffness 272 and mass 1: critically damped, no overshoot). |
| `animation.spring_mass` | `1.0` | number, 0.01 to 100 | Spring mass of the windows' motion. |
| `animation.unpark_at` | `0.99` | number, 0.5 to 1 | An incoming window takes its place behind its growing snapshot once the motion is this far along (0.99 = 99 %, about 400 ms). |
| `animation.cover_ms` | `20` | whole number, 0 to 500 | Outgoing windows leave this long after their snapshots are drawn over them, so the snapshots are on screen first. |
| `animation.corner_radius` | `0.0` | number, 0 to 60 | Corner radius of a snapshot at full size; 0 keeps the window's own shape (the card's radius is [cards] corner_radius). |
| `animation.shadow_radius` | `24.0` | number, 0 to 100 | Blur radius of a moving snapshot's shadow. |
| `animation.shadow_opacity` | `0.35` | number, 0 to 1 | Opacity of that shadow. |
| `animation.shadow_offset_y` | `10.0` | number, -100 to 100 | Vertical offset of that shadow (down is positive). |
| `animation.snapshot_interval_ms` | `2000` | whole number, 0 to 60000 | How often full-size snapshots of the shown windows are refreshed for the next switch; 0 = only as the pointer nears the strip. |
| `animation.snapshot_cache_mb` | `512` | whole number, 16 to 8192 | Most memory kept for full-size window snapshots; the oldest go first. |
| `animation.activation_grows` | `false` | true or false | Cmd-Tab, a Dock click or a click on a parked sliver to an app in another stage: true grows its windows from their card like a card click; false brings them to their frames at once (macOS has already raised them in the park corner, so waiting shows them there). |
| `windows.fit` | `"off"` | "off", "move", "narrow" | Keep placed windows clear of the strip: "move" right just enough, "narrow" also too-wide ones. Apple: "off". |
| `windows.fit_margin` | `16.0` | number, 0 to 500 | With fit: room kept between the strip column and the windows. |
| `windows.minimize_to_strip` | `true` | true or false | Cmd-M (minimize) sends the window to the strip as a card of its own, as Apple does, instead of the Dock. |
| `windows.focus_echo_ms` | `700` | whole number, 0 to 10000 | Focus changes this soon after we moved a window are taken as our own and never switch stages. |
| `windows.park_inside` | `1.0` | number, -10000 to 32 | Points of a parked window asked to stay inside the display's side (0: none; negative: asked beyond it). macOS keeps what it keeps; scripts/probe_park.py measures it. |
| `windows.park_drop` | `-1.0` | number, -32 to 10000 | How far below the display's bottom edge a parked window's top is asked (-1: one point above it). macOS keeps each app's title strip on screen; scripts/probe_park.py measures it. |
| `windows.hide_parked_apps` | `false` | true or false | An app whose every window (every Desktop) sits in inactive stages is hidden, as Cmd-H does, instead of leaving slivers in the corner; Cmd-Tab and the Dock show its stage in place. A prototype: off by default. |
| `windows.corner_patch` | `true` | true or false | Cover the parked windows' slivers with the desktop picture under them, drawn from the wallpaper's file (no screen capture), so the corner is clean as Apple's; it steps aside while it cannot be drawn exactly (videos, aerials, shuffles, Photos; Center and Tile, and Fit with no fill colour, until checked live) or a desktop icon lies under it. |
| `windows.corner_patch_reach` | `60.0` | number, 0 to 400 | How far past a sliver the corner patch reaches, in points, to cover its shadow. |
| `windows.inert_slivers` | `true` | true or false | A press on the strip macOS keeps on screen of a parked window (the bottom-right corner) does nothing, as Apple's parked windows cannot be grabbed; false lets it reach the app (its stage comes on release). |
| `sugar.shift_brings_here` | `true` | true or false | Hold Shift while opening or activating an app (Dock, Finder, Spotlight Shift-Return, Launchpad) to bring it here. |
| `sugar.launch_window_ms` | `10000` | whole number, 0 to 60000 | A newly opened app's first window counts a Shift held this long before. |
| `sugar.activation_window_ms` | `2000` | whole number, 0 to 60000 | An activation counts a Shift held this long before. |
| `hotkeys.enabled` | `false` | true or false | Register the hotkeys below. |
| `hotkeys.switch` | `"ctrl+alt+{n}"` | hotkey with {n} for the digits 1-9, or "" for none | Switch to stage N. |
| `hotkeys.send` | `"ctrl+alt+shift+{n}"` | hotkey with {n} for the digits 1-9, or "" for none | Send the focused window to stage N (N = count + 1 makes a stage). |
| `hotkeys.new_stage` | `"ctrl+alt+n"` | hotkey such as "ctrl+alt+n", or "" for none | Send the focused window to a new stage. |
| `hotkeys.toggle_pin` | `"ctrl+alt+p"` | hotkey such as "ctrl+alt+n", or "" for none | Pin or unpin the focused window (shown on every stage). |
| `hotkeys.toggle_strip` | `"ctrl+alt+s"` | hotkey such as "ctrl+alt+n", or "" for none | Show or hide the strip. |
| `stages.names` | `[]` | list of names | Named stages created at launch in this order (hotkey numbers 1, 2, ...); they stay when empty. |
| `stages.restore_after_restart` | `true` | true or false | After a restart or a new login, windows go back to the stages and places they had, as their apps reopen them (with macOS's "Reopen windows when logging back in"). Off: saved windows not on screen at launch are forgotten and every app starts in a stage of its own, as Apple's does. Read at launch. |
| `defaults.action` | `"native"` | "native", "current", "new", "stage:<name>", "float", "ignore" | "native" is Apple's: a window joins its app's stage, a newly opened app gets its own. |
| `defaults.placement` | `"as-is"` | "as-is", "center", "cascade" | Where it goes: "as-is", "center", or "cascade" (offset from the front window). |
| `log.level` | `"info"` | "error", "warn", "info", "debug", "trace" | How much the log keeps: "info" the main events, "debug" every decision (and shows Mark a Problem in the menu). |
| `log.titles` | `false` | true or false | Write window titles into the log and problem marks, for debugging; off, a short hash stands in for each. |
| `log.days` | `14` | whole number, 1 to 365 | Day files older than this many days are deleted. |
| `log.max_mb_per_day` | `100` | whole number, 1 to 10000 | A day's file this big takes only warnings, errors, marks and counts from then on. |
| `log.max_mb_total` | `500` | whole number, 1 to 100000 | The log folder's cap: the oldest days go first, never today's. |
