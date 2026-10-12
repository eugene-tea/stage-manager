# Feature requests: what Stage Manager users have asked for since 2022

Research of 2026-10-12, from public sources only: Apple's help pages, Apple Community, Apple
Developer Forums, Mac Power Users, Hacker News, MacStories, Michael Tsai's link roundups,
Six Colors, Eclectic Light, MacSales, Setapp, MacMost, and the pages of competing apps.
Reddit was **not reachable** (it blocks our fetcher), and MacRumors returned 403, so those
two appear only through search excerpts or not at all. Years are those of the post cited.

The baseline does not change: Apple's Stage Manager behaves as it does now. Everything
below is optional sugar on top of it. Where a gesture already exists (a modifier on a
click, a drag, the Dock, ⌘Tab, a menu, the strip), it is preferred over a new shortcut.

## 1. Apple's documented behavior (corrects or confirms UX-SPEC)

| Behavior | What Apple (or the best source) says | UX-SPEC today |
|---|---|---|
| **Shift-click a strip card** | Apple's original article *Use Stage Manager to organize apps and windows on Mac* (HT213315, 2022–23): "To create a group of windows in the center, drag a thumbnail to the center window, or **press the Shift key while clicking the thumbnail**" (quoted in [Apple Community, 2023](https://discussions.apple.com/thread/255102446)). The current guide ([support.apple.com/102355](https://support.apple.com/en-us/102355)) no longer mentions Shift. It still works: in [MPU, Dec 2025](https://talk.macpowerusers.com/t/how-to-get-one-app-at-a-time-focus-using-stage-manger-without-the-screenclutter/43776) a user says to "hold Shift when clicking an app to keep several in view". [MacStories 2022](https://www.macstories.net/stories/macos-ventura-the-macstories-review/2/) agrees: "click on one while holding down Shift to add it to the current stage". It works only for apps already in the strip, not for launching an app ([MPU 2024](https://talk.macpowerusers.com/t/is-anyone-using-stage-manager/32549?page=3)). | "Not captured". **Confirmed: it adds the card to the current stage.** Still open: on a card holding several apps, does Shift-click add the whole card or only the app under the pointer? Our merge takes the whole card. One live check settles it. |
| **Option-click a strip card** | Neither Apple nor any third party documents it. The only Option-click related to Stage Manager is on its Control Center button, which turns it off and on again ([MacMost 2022](https://macmost.com/the-comprehensive-guide-to-mac-stage-manager.html)). | Unknown. It looks free for sugar, but check live that it does nothing. |
| **Drag a card into the center** | Apple: "Drag an app from the left side of the screen to add it to a group of apps in the center of the screen" ([Apple guide](https://support.apple.com/guide/mac-help/use-stage-manager-mchl534ba392/mac)). From a card holding several apps, "you can only drag the top app in the pile" ([MacStories 2022](https://www.macstories.net/stories/macos-ventura-the-macstories-review/2/)). | Matches what was observed. |
| **Drag a window to the strip** | Apple: "Drag an app to the left side of the screen to remove it from the group" ([Apple guide](https://support.apple.com/guide/mac-help/use-stage-manager-mchl534ba392/mac)). A dragged item can also hover over a card until that stage comes forward, then be dropped there (spring loading, [Setapp, updated 2026](https://setapp.com/how-to/stage-manager-on-mac)). | We observed "becomes its own stage". **Spring loading onto a card was not captured.** |
| **One at a Time** | "Show only the most recently used window for an app when you switch to it … **click the app on the left again to open the next available window**" ([Apple guide](https://support.apple.com/en-us/102355); [Desktop & Dock settings](https://support.apple.com/guide/mac-help/change-desktop-dock-settings-mchlp1119/mac)). | **Correction:** clicking the same app's card again cycles to its next window. UX-SPEC only covers activation. |
| **Click the wallpaper** | "Always": "Clicking the wallpaper moves all windows out of the way to show your desktop items and widgets." "Only in Stage Manager": "When Stage Manager is turned on, clicking the wallpaper moves all windows out of the way" ([Desktop & Dock settings](https://support.apple.com/guide/mac-help/change-desktop-dock-settings-mchlp1119/mac)). | Confirmed. |
| **How many recent apps** | Apple gives no number ("Your recently used apps are neatly arranged along the left side"). Reviewers counted 4 on an M1 MacBook Air and 6 on a Studio Display ([MacStories 2022](https://www.macstories.net/stories/macos-ventura-the-macstories-review/2/)), 5 on a 13″ Air with up to 6 ([Intego 2022](https://www.intego.com/mac-security-blog/stage-manager-offers-a-new-way-to-work-with-windows-in-macos-ventura-and-ipados-16/)), and "up to six … you can't rearrange or resize" ([Setapp 2026](https://setapp.com/how-to/stage-manager-on-mac)). | Confirms 6 at 1440 pt. **Check that the count drops on shorter displays (4 on a laptop).** |
| **Recent apps hidden** | "recently used apps are hidden—move the pointer to the left edge of the screen to briefly show them" ([Apple guide](https://support.apple.com/en-us/102355)). The strip also hides while a window is dragged over it ([MacStories 2022](https://www.macstories.net/stories/macos-ventura-the-macstories-review/2/)) and when two windows sit side by side ([MacSales 2025](https://eshop.macsales.com/blog/97454-on-the-eve-of-macos-tahoe-has-stage-manager-improved/)). | Confirmed. |
| Native behaviors not in Apple's docs | **⌘M** sends the window to the strip within its set, not to the Dock. **⌘H** removes the app from the strip until it is unhidden. **Clicking a card's app icon** spreads out all of that app's windows, including those in other sets. **⌘`** cycles the windows of the current set ([MacMost 2022](https://macmost.com/the-comprehensive-guide-to-mac-stage-manager.html)). A Stage Manager on/off hotkey can be assigned under Keyboard Shortcuts → Mission Control ([Setapp 2026](https://setapp.com/how-to/stage-manager-on-mac)). | **Not in UX-SPEC.** These are parity items. Check them live before matching. |

## 2. Requests, ranked by how often and for how long people have asked

Feasibility assumes our architecture: one Space, parked windows, AX, ScreenCaptureKit, our
own `NSPanel` strip.

### R1. Put an app or window into the current stage, not a new one (2022 → 2025, the most common request)
- **Sources:** new Finder window or IB library "throws me into a new stage" (ObjC4Life on [Tsai 2022](https://mjtsai.com/blog/2022/10/25/stage-manager-in-macos-13-0/)); "a shortcut for opening an app in the current stage" ([MacStories 2022](https://www.macstories.net/stories/macos-ventura-the-macstories-review/2/)); developers cannot open a helper window into the caller's stage ([Apple Dev Forums 2022](https://developer.apple.com/forums/thread/719749)); Calculator from the Dock pushes Safari aside ([Apple Community 2023](https://discussions.apple.com/thread/255102446)); adding to a set is the "Achilles heel" ([Tao of Mac 2023](https://taoofmac.com/space/blog/2023/05/20/1130)); Shift-click should launch apps and work from Spotlight ([MPU 2024](https://talk.macpowerusers.com/t/is-anyone-using-stage-manager/32549?page=3)). A MacRumors search excerpt suggests "Option-click when opening an app to place it in the current stage".
- **Gap:** Stage Manager offers no "here" when launching or activating. Shift-click only works on cards already in the strip.
- **Sugar (in progress):** Shift held means "bring here" wherever an app is opened or activated: a Dock click, a Finder double-click, Spotlight or Launchpad Return, a new window appearing, and Shift-click on a card (native). A plain click keeps Apple's behavior.
- **Watch out:** ⌘⇧Tab already means "cycle backward", so "Shift on ⌘Tab" is ambiguous. Either leave ⌘Tab out, or read Shift only at the moment ⌘ is released.
- **Feasibility: medium.** Read `CGEventSourceFlagsState` when the activation or `kAXWindowCreated` arrives. A cold launch needs a short window (about 2–5 s) that remembers "Shift was down at launch" per PID.

### R2. Switch between stages from the keyboard (2022 → 2024)
- **Sources:** "desperate need of keyboard shortcuts" ([MacStories 2022](https://www.macstories.net/stories/macos-ventura-the-macstories-review/2/)); "lack of proper keyboard shortcuts to switch between groups", Rui Carmo ([Tsai, 2023 update](https://mjtsai.com/blog/2022/10/25/stage-manager-in-macos-13-0/)); feedback FB12194002 ([Tao of Mac 2023](https://taoofmac.com/space/blog/2023/05/20/1130)); iPad has more shortcuts than the Mac ([MPU 2024](https://talk.macpowerusers.com/t/is-anyone-using-stage-manager/32549?page=3)); competitors fill the gap ([rcmd Stages](https://lowtechguys.com/rcmd/)).
- **Gap:** the only ways to switch are a mouse click on a card, the Dock, and ⌘Tab by app.
- **Sugar:** the ⌃⌥1…9 hotkeys already exist. For the owner, without new keys: scroll or two-finger swipe over the strip to step through stages, and a stage list in the menu-bar menu (click to switch).
- **Feasibility: easy** (scroll events on our own panel; `NSMenu`). A global trackpad swipe anywhere on screen is **hard** (no public API for global gestures).

### R3. Move a window between stages, or take it out, without drag gymnastics (2022 → 2025)
- **Sources:** only the top app of a pile can be dragged ([MacStories 2022](https://www.macstories.net/stories/macos-ventura-the-macstories-review/2/)); changing a group needs "awkward workarounds" (Rui Carmo, [Tsai 2023](https://mjtsai.com/blog/2022/10/25/stage-manager-in-macos-13-0/)); "is there a way to dismiss the app from the stage?" and "Moving app windows between stages seems a little clumsy" ([MPU 2024](https://talk.macpowerusers.com/t/is-anyone-using-stage-manager/32549?page=3), [page 2](https://talk.macpowerusers.com/t/is-anyone-using-stage-manager/32549?page=2)); "It would be amazing … to drag items from the strip from one set to the other" ([MacSales 2025](https://eshop.macsales.com/blog/97454-on-the-eve-of-macos-tahoe-has-stage-manager-improved/)).
- **Gap:** membership changes only by dragging through the center, one top app at a time.
- **Sugar:** (a) Apple's own drags, for parity: drag a window onto the strip to make its own stage, and drag a card into the center to join. (b) Drop a dragged window on a card to join that stage (Apple's spring loading, made direct). (c) Drag one card onto another to merge the two stages. (d) Context-menu items (R4) for anyone who would rather not drag.
- **Feasibility:** card onto card is **easy** (it all happens inside our panel). A window dropped on the strip or a card is **medium**: AX move notifications plus a global mouse-up monitor, hit-tested against the card rectangles. A card dragged into the center is **medium**: an `NSDraggingSession` from the panel, then restore at the drop point. SPEC lists drag and drop as out of scope, so this needs the owner's word.

### R4. A right-click menu on cards: close, quit, peek (2022 → 2026)
- **Sources:** "a right-click menu" ([MacStories 2022](https://www.macstories.net/stories/macos-ventura-the-macstories-review/2/)); right-clicking a thumbnail "shows no options; you can't close a window or quit an app" ([Setapp 2026](https://setapp.com/how-to/stage-manager-on-mac)); a paid add-on, Stage View, sells exactly previews plus close and minimize from them ([ifun 2022](https://www.ifun.de/stage-manager-am-mac-zusatz-app-will-apples-werksfunktion-verbessern-198529/)).
- **Gap:** cards only accept a click or a drag.
- **Sugar:** right-click or Ctrl-click a card for: Bring Here (the same as Shift-click), Move Front Window to New Stage, Keep on Every Stage (pin), Name Stage…, Close Window ▸ (one entry per window), Quit App ▸. A left click is unchanged.
- **Feasibility: easy.** `NSMenu` on our panel; close by pressing the AX close button (`AXCloseButton`); quit with `NSRunningApplication.terminate`.

### R5. A strip that shows more, and in the user's order (2022 → 2026)
- **Sources:** thumbnails "too small" ([HN 2022](https://news.ycombinator.com/item?id=33337549)); a seventh app drops out of sight ([Intego 2022](https://www.intego.com/mac-security-blog/stage-manager-offers-a-new-way-to-work-with-windows-in-macos-ventura-and-ipados-16/)); "takes up a lot of real estate", wants bigger icons and smaller previews ([MPU 2024](https://talk.macpowerusers.com/t/is-anyone-using-stage-manager/32549?page=3)); icons can't be made "smaller and less intrusive" ([MPU Dec 2025](https://talk.macpowerusers.com/t/how-to-get-one-app-at-a-time-focus-using-stage-manger-without-the-screenclutter/43776)); "you can't rearrange or resize them" ([Setapp 2026](https://setapp.com/how-to/stage-manager-on-mac)).
- **Gap:** stages beyond the sixth (fourth on a laptop) are reachable only through the Dock or ⌘Tab, and the order is fixed by recency.
- **Sugar:** keep Apple's count and slot order. Scrolling over the strip reveals older stages, and a "+N" mark shows that more exist. Drag a card to reorder (it keeps that slot). An optional `strip.scale` in `config.toml`, since macOS has no such setting.
- **Feasibility: easy** (all inside our panel and layout code).

### R6. No unasked stage switches (2022 → 2023, and still in 2026 reports)
- **Sources:** Jason Snell is "whisked away to a new stage" within minutes ([Tsai 2022](https://mjtsai.com/blog/2022/10/25/stage-manager-in-macos-13-0/)); Open dialogs make it "a frenetic, hyperactive mess" ([MacStories 2022](https://www.macstories.net/stories/macos-ventura-the-macstories-review/2/)); Archive Utility and emptying the Trash pull the stage away ([Eclectic Light 2023](https://eclecticlight.co/2023/01/17/stage-manager-for-the-unimpressed-3-apps-and-windows/)); "things will disappear, leaving me with just the new app" ([irritatedsysadmin 2022](https://irritatedsysadmin.substack.com/p/impression-a-week-with-stage-manager)).
- **Gap:** any app that activates itself or opens a panel takes the screen.
- **Sugar:** child windows follow their parent (already in SPEC). An activation with no user input in the last ~0.5 s does not switch stage; that app's card glows instead. Utility and helper windows (Archive Utility, panels) never switch.
- **Feasibility: medium.** Use `CGEventSourceSecondsSinceLastEventType`. The heuristic needs tuning against real apps.

### R7. Named stages that survive restarts and relaunches (2022 → 2024)
- **Sources:** SwiftUI apps "don't persist at all" (Howard Oakley, [Tsai 2023](https://mjtsai.com/blog/2022/10/25/stage-manager-in-macos-13-0/)); closing a window breaks its grouping ([Eclectic Light 2023](https://eclecticlight.co/2023/01/17/stage-manager-for-the-unimpressed-3-apps-and-windows/)); a MacRumors excerpt asks Stage Manager to "remember that two apps were paired"; a free app, Stage Label, exists only to label stages ([ifun 2024](https://www.ifun.de/fensterverwaltung-am-mac-stage-label-beschriftet-den-stage-manager-227393/)); rcmd sells saved, lettered Stages ([rcmd](https://lowtechguys.com/rcmd/)).
- **Gap:** stages are anonymous and fragile.
- **Sugar:** persistence and named stages from config already exist. Add naming from the card's menu (R4), the name shown under the card on hover, and an app that relaunches rejoining its remembered stage (already done by bundle ID).
- **Feasibility:** naming is **easy**. "Reopen this stage's apps" is **medium** (launch by bundle ID). Reopening specific documents is **hard** (AX gives `AXDocument` for some apps only).

### R8. Multiple displays: leave the others alone (2022 → 2024)
- **Sources:** two displays make it "in the way of any productivity" (Philip on [Tsai 2022](https://mjtsai.com/blog/2022/10/25/stage-manager-in-macos-13-0/)); moving a window to another display "creates a new group" ([Tao of Mac 2023](https://taoofmac.com/space/blog/2023/05/20/1130)); "I wish the Stage Manager will only work with my main monitor" (Topre) and "treated as a single, unified stage" (iPersuade), both in [MPU 2024](https://talk.macpowerusers.com/t/is-anyone-using-stage-manager/32549?page=2).
- **Gap:** Apple's per-display stages cannot be turned off.
- **Sugar:** we are main-display-only already, which is exactly Topre's wish. A window dragged to another display leaves its stage instead of spawning one. Confirm what ours does now.
- **Feasibility: easy** for "leave alone". Per-display strips are **medium**. A unified stage across displays is **hard**.

### R9. Minimize to the Dock when I mean the Dock (2022 → 2025)
- **Sources:** it "sends things to the strip more often than I'd like" ([MacStories 2022](https://www.macstories.net/stories/macos-ventura-the-macstories-review/2/)); how do I minimize to the Dock with Stage Manager on? Answer: you can't ([Apple Community 2023](https://discussions.apple.com/thread/254954825)); minimized windows can't be moved between sets ([MacSales 2025](https://eshop.macsales.com/blog/97454-on-the-eve-of-macos-tahoe-has-stage-manager-improved/)).
- **Gap:** with Stage Manager on, ⌘M never reaches the Dock.
- **Sugar:** the default matches Apple: on `AXWindowMiniaturized`, unminimize the window and send it to the strip. "Minimize to Dock" stays in the card menu (R4). Avoid Option on the yellow button: it already means "minimize all of this app's windows".
- **Feasibility: medium.** Expect a visible minimize-then-restore flash.

### R10. Faster switching (2022 → 2023)
- **Sources:** "the animations could be made faster" (Peter Ammon, [Tsai 2022](https://mjtsai.com/blog/2022/10/25/stage-manager-in-macos-13-0/)); "I wish I could make the transition faster", worked around with Reduce Motion ([HN 2023](https://news.ycombinator.com/item?id=35026126)); "whooshing" windows ([Tao of Mac 2023](https://taoofmac.com/space/blog/2023/05/20/1130)).
- **Gap and sugar:** our windows already jump (UX-SPEC deviation 1), which is what these users want. Only the strip's card glide is animated; skip it when Reduce Motion is on.
- **Feasibility: easy.**

### R11. Automation through Shortcuts or AppleScript (2022 → 2026)
- **Sources:** no AppleScript or Shortcuts actions (Jack Wellborn, [Tsai 2022](https://mjtsai.com/blog/2022/10/25/stage-manager-in-macos-13-0/)); MacStories wants actions that build and tear down sets ([2022](https://www.macstories.net/stories/macos-ventura-the-macstories-review/2/)); a user asks for a supported way to script a window layout ([Apple Community, Feb 2026](https://communities.apple.com/de/thread/256236489)).
- **Sugar:** the ctl socket already works from a Shortcuts "Run Shell Script" action. Document that.
- **Feasibility:** documenting it is **easy**. An AppleScript dictionary (`sdef`) is **medium**. Native App Intents actions are **hard**, since they need Swift intent metadata in the bundle.

## 3. Recommended first batch

1. **Shift = bring here, everywhere (R1).** Finish the work in progress: Dock, Finder, Spotlight and Launchpad, new windows, card Shift-click. Settle ⌘Tab (watch out for ⌘⇧Tab).
2. **Card context menu (R4).** Bring Here, New Stage, Keep on Every Stage, Name…, Close Window, Quit App. One easy item that also covers parts of R3, R7, R9 and pinning, without a single shortcut.
3. **Stage names (R7).** Set from the menu, shown on hover, persisted.
4. **Strip overflow and reorder (R5).** Scroll over the strip to reach older stages, with a "+N" mark; drag a card to reorder.
5. **Merge by dragging a card onto a card (R3c).** Easy, and needs no change to the scope rule for windows.
6. **Quiet activations (R6).** No stage switch without recent user input; the card glows instead.
7. **Stage list in the menu-bar menu, plus scroll over the strip (R2).** Switching without keys.
8. *(Parity, before any sugar.)* Live checks of the gaps found in §1: One at a Time re-click cycling, ⌘M to the strip, clicking a card's app icon, Shift-click on a card with several apps, Option-click, and the strip count on a shorter display.

Next once the owner lifts "no drag and drop": Apple's own window ↔ strip drags and dropping a window on a card (R3a, R3b, medium).
