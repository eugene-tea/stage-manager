# Accessibility

Stage Manager should work for everyone who can use Apple's Stage Manager. This page says
where it stands today, honestly, and how to tell us about a barrier.

## Where it works

- macOS 14 Sonoma or later, Apple silicon.
- The menu bar icon has a VoiceOver label, and its menu, Settings and the welcome window
  are standard macOS controls.
- Everything works with the mouse or trackpad alone. With keyboard shortcuts turned on in
  Settings, ⌃⌥1–9 switches stages and sends windows to them; ⌘-Tab and the Dock also bring
  an app's stage forward.
- `[animation] speed = 0` in the config file turns every animation off.

## Known limitations

- The strip's cards have no VoiceOver labels yet: VoiceOver does not read which stage a
  card holds.
- The system's Reduce Motion setting is not followed yet; use `[animation] speed = 0`.
- Neither has been tested with VoiceOver users. If you use VoiceOver, Switch Control or
  other assistive technology, your report is especially welcome.

## Report a barrier

Open an [issue](https://github.com/eugene-tea/stage-manager/issues/new?template=bug.yml)
and mention it is about accessibility: what you use (VoiceOver, Switch Control, Reduce
Motion, a larger text size) and what got in the way.
