# Accessibility

Stage Manager should work for everyone who can use Apple's Stage Manager. This page says
where it stands, honestly, and how to tell us about a barrier.

## What works

- **VoiceOver.** Each card in the strip is a button that says which stage it is and which
  apps it holds, for example "Stage 3: Safari, Notes", or the stage's name. Pressing it
  switches to that stage. Its actions show all of one app's windows, as a click on the app's
  icon does. The menu bar icon, its menu, Settings and the welcome window are standard macOS
  controls; switches and pop-up menus in Settings read the name of their row.
- **Keyboard.** Settings, the welcome window and the menu work with Full Keyboard Access.
  With keyboard shortcuts turned on in Settings → Windows, ⌃⌥1–9 switches stages and
  ⌃⌥⇧1–9 sends the front window to one. ⌘-Tab and the Dock also bring an app's stage
  forward, and holding Shift while opening an app keeps it in the stage you are in.
- **Reduce Motion.** Followed by default. Windows fade in and out where they are instead of
  flying to and from the strip, the strip fades instead of sliding, and cards don't grow
  under the pointer or glide to a new place. What you drag still follows the pointer. This
  follows Apple's guidance for the setting (replace movement, scaling and depth with short
  fades) and WCAG 2.3.3. `[animation] reduce_motion` in the config file can set it on or off
  for Stage Manager alone, and `[animation] speed = 0` turns every animation off.
- **Reduce Transparency and Increase Contrast.** The cards turn solid instead of letting the
  wallpaper through, as macOS's own glass does.
- Text in the app's own windows uses the system font and colours, in light and dark mode.

## What doesn't yet

- None of this has been tested by people who rely on VoiceOver, Switch Control or Voice
  Control. Your report is especially welcome.
- VoiceOver doesn't reach the strip while it is hidden (a window covering it, or "Show recent
  apps" off); use the keyboard shortcuts, ⌘-Tab or the Dock then.
- Dragging cards and windows to and from the strip has no keyboard or VoiceOver way yet,
  except the shortcuts that send a window to a stage.
- With both Reduce Motion and Reduce Transparency on, nothing marks the card under the
  pointer.
- The names and numbers drawn on the cards keep their size; they don't follow a larger text
  size.

## Report a barrier

Open an [issue](https://github.com/eugene-tea/stage-manager/issues/new?template=bug.yml)
and say it is about accessibility: what you use (VoiceOver, Switch Control, Reduce Motion, a
larger text size) and what got in the way.
