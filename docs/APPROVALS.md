# Approvals

What Stage Manager asks macOS for, in the words the app uses. For the disk image's art, the
info page and anyone writing about the app. Keep it in step with
`src/platform/permissions.rs` (`Approval`).

## Before you start

**Turn off Apple's Stage Manager.** Stage Manager takes over from it. If it is on, Stage
Manager offers to turn it off for you ("Turn Off Apple's Stage Manager"), and waits, moving
nothing, until it is off.

## Accessibility — required

- **Why:** lets Stage Manager move, hide and bring back your windows.
- **Without it:** Stage Manager cannot arrange windows; it waits until access is allowed.
- **Where:** System Settings → Privacy & Security → Accessibility.

## Screen Recording — optional

- **Why:** shows live pictures of your windows on the cards and in switches.
- **Without it:** cards show plain window shapes with the app's icon.
- **Where:** System Settings → Privacy & Security → Screen & System Audio Recording.

## Not needed

- **Input Monitoring.** Stage Manager installs no keyboard tap. Its standard event monitor
  sees clicks and drags (no approval needed) and, under Accessibility, key presses only to
  notice ⌘Tab and ⌘\`; nothing you type is recorded.

## How the app asks

The first launch opens a small welcome window with one row per approval and an **Allow…**
button for each; each row turns into a green check mark as soon as access is allowed. While
anything is missing, the menu bar icon's menu starts with "Stage Manager needs … access"
and "Allow …".
