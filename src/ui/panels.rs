//! How our panels behave in Spaces and Exposé (`NSWindowCollectionBehavior`), in one place.
//!
//! AppKit sorts the flags into independent groups, one choice from each (Apple's "Setting
//! Window Collection Behavior"):
//!
//! - **Spaces:** none (the default: the window belongs to one Space at a time, the one it is
//!   first ordered in on), `CanJoinAllSpaces` (every Space, like the menu bar) or
//!   `MoveToActiveSpace` (follows the user to the Space shown when it is made active).
//! - **Exposé** (Mission Control, App Exposé, Show Desktop): `Managed` (laid out with the
//!   windows), `Transient` ("floats in Spaces and is hidden in Exposé"; the default for a
//!   window above or below the normal level) or `Stationary` (unaffected by Exposé: stays
//!   drawn and in place, like the desktop picture).
//! - **Cycling:** `IgnoresCycle` keeps a window out of ⌘` and the Window menu's cycle.
//! - **Full screen:** `FullScreenAuxiliary` lets a window show on a full-screen app's Space
//!   (with `CanJoinAllSpaces`).
//!
//! Apple's Stage Manager strip disappears in Mission Control, App Exposé and Show Desktop
//! (the owner's report of 10-12: "When I press F3 to show all windows, I'd assume the strip
//! should hide"). Until round 6K ours were `Stationary`, so they stayed drawn over Mission
//! Control. Every panel of ours is now `Transient`: hidden by Exposé, and back when it ends.
//! The Spaces group is left as it was, so a Desktop's strip pane still belongs to that
//! Desktop alone (round 6E) and never shows on a full-screen app's Space.

use objc2_app_kit::NSWindowCollectionBehavior as B;

/// A Desktop's strip pane and its edge tab: on the one Desktop it is first ordered in on
/// (no Spaces flag), hidden by Exposé, out of the window cycle, never on a full-screen Space.
pub fn desktop_panel() -> B {
    B::Transient | B::IgnoresCycle
}

/// The ghost of a card dragged out of the strip: wherever the drag goes (every Space, a
/// full-screen one included), hidden by Exposé, out of the window cycle.
pub fn drag_ghost() -> B {
    B::CanJoinAllSpaces | B::Transient | B::IgnoresCycle | B::FullScreenAuxiliary
}

/// The motion overlay: moves to the Space shown when it is shown, hidden by Exposé, out of
/// the window cycle.
pub fn motion_overlay() -> B {
    B::MoveToActiveSpace | B::Transient | B::IgnoresCycle
}

/// A Desktop's wallpaper corner patch (`[windows] corner_patch`): as a Desktop's strip pane,
/// on the one Desktop it is first ordered in on (no Spaces flag), so it slides with that
/// Desktop and is there, drawn, when the Desktop slides back in (it moved to the Space shown
/// before, and was ordered out at every Space change); hidden by Mission Control, App Exposé
/// and Show Desktop, out of ⌘` and the Window menu's cycle; never on a full-screen Space.
/// Its panel is non-activating and click-through, never key or main, so ⌘Tab and switchers
/// that list apps never see it (an accessory app with no window that can be key).
pub fn corner_patch() -> B {
    desktop_panel()
}

/// Our own windows (Settings, Welcome, alerts): shown on the Desktop the user is on. Without
/// it, a window closed (only hidden, kept for the next opening) still belongs to the Desktop it
/// was last on, and opening it again takes the user there (the owner's report of 10-22).
pub fn app_window() -> B {
    B::MoveToActiveSpace
}

/// The names of the flags set in `bits` (for `ctl state` and logs), in a fixed order.
pub fn names(bits: usize) -> Vec<&'static str> {
    const ALL: [(B, &str); 9] = [
        (B::CanJoinAllSpaces, "can_join_all_spaces"),
        (B::MoveToActiveSpace, "move_to_active_space"),
        (B::Managed, "managed"),
        (B::Transient, "transient"),
        (B::Stationary, "stationary"),
        (B::ParticipatesInCycle, "participates_in_cycle"),
        (B::IgnoresCycle, "ignores_cycle"),
        (B::FullScreenPrimary, "full_screen_primary"),
        (B::FullScreenAuxiliary, "full_screen_auxiliary"),
    ];
    ALL.iter().filter(|(flag, _)| bits & flag.0 != 0).map(|(_, name)| *name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACES: [B; 2] = [B::CanJoinAllSpaces, B::MoveToActiveSpace];
    const EXPOSE: [B; 3] = [B::Managed, B::Transient, B::Stationary];

    fn count(b: B, group: &[B]) -> usize {
        group.iter().filter(|f| b.contains(**f)).count()
    }

    #[test]
    fn every_panel_is_hidden_by_expose_and_out_of_the_cycle() {
        for b in [desktop_panel(), drag_ghost(), motion_overlay(), corner_patch()] {
            assert!(b.contains(B::Transient), "{:?}", names(b.0));
            assert_eq!(count(b, &EXPOSE), 1, "one Exposé choice: {:?}", names(b.0));
            assert!(count(b, &SPACES) <= 1, "one Spaces choice: {:?}", names(b.0));
            assert!(b.contains(B::IgnoresCycle) && !b.contains(B::ParticipatesInCycle));
            assert!(!b.contains(B::Stationary), "Stationary stays drawn over Mission Control");
        }
    }

    #[test]
    fn a_desktop_pane_stays_on_its_desktop_and_off_full_screen_spaces() {
        let b = desktop_panel();
        assert_eq!(count(b, &SPACES), 0, "no Spaces flag: one Desktop, the one it is first ordered in on");
        assert!(!b.contains(B::FullScreenAuxiliary) && !b.contains(B::FullScreenPrimary));
        assert_eq!(names(b.0), ["transient", "ignores_cycle"]);
    }

    #[test]
    fn the_ghost_and_the_overlay_keep_their_spaces_choice() {
        assert!(drag_ghost().contains(B::CanJoinAllSpaces | B::FullScreenAuxiliary));
        assert!(motion_overlay().contains(B::MoveToActiveSpace));
    }

    #[test]
    fn the_corner_patch_never_shows_in_switchers_or_overviews() {
        let b = corner_patch();
        assert_eq!(names(b.0), ["transient", "ignores_cycle"], "one Desktop's, as a strip pane");
        assert!(!b.contains(B::Managed) && !b.contains(B::Stationary), "never laid out or kept by Exposé");
        assert!(!b.contains(B::FullScreenAuxiliary) && !b.contains(B::CanJoinAllSpaces));
    }

    #[test]
    fn names_list_only_what_is_set() {
        assert!(names(0).is_empty());
        assert_eq!(names((B::Stationary | B::IgnoresCycle).0), ["stationary", "ignores_cycle"]);
    }
}
