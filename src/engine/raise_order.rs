//! A shown stage's windows end in the order its card shows, whatever the apps' timing.
//!
//! A switch raises the arriving windows back to front, one `AXRaise` each, the key last and
//! focused ([`crate::engine::model::Model::show_stage`]). Plain raises stack in raise order
//! (a probe with the fixture apps, 10-27), but a real app may answer its `AXRaise` before its
//! window's reorder reaches the window server (on its next run-loop pass, say), so two raises
//! a few ms apart to two different apps can land in either order. On the owner's Mac (10-27,
//! a stage of Mail, Safari and Calendar, Calendar's Dock click) the two inactive apps' windows
//! landed swapped on every other arrival: the card saved the swapped order when he left (the
//! capture sorts by the window server's z-order) and the next arrival swapped them back.
//!
//! So after a batch whose raises reach two or more apps, the platform reads the window
//! server's order of the windows it raised ([`listed_order`]) for a moment, and when it
//! differs from the order asked ([`asked_order`]) raises again the windows above the longest
//! bottom run already stacked right ([`to_raise_again`]), one at a time, back to front, each
//! waited for until it shows above those below it ([`shows_above`]), and ends with the key's
//! raise and focus as the batch did. Bounded ([`CHECK_CAP`]), and given up at once when the
//! user acts meanwhile: a mouse button pressed since the switch began, or another window
//! brought in front ([`front_moved`]: ⌘Tab, ⌘`). Typing into the key window stops nothing.

use std::time::Duration;

/// How long the order is watched after the batch (and after each fix) for a late reorder
/// before it counts as settled.
pub const WATCH: Duration = Duration::from_millis(60);

/// How often the window server's order is read while watching or waiting.
pub const POLL: Duration = Duration::from_millis(4);

/// How long one window raised again is waited for to show above those below it.
pub const PER_WINDOW: Duration = Duration::from_millis(60);

/// The whole check's budget, watching included: the WM thread is never held longer.
pub const CHECK_CAP: Duration = Duration::from_millis(250);

/// At most this many fixes per batch (a late reorder of the batch's own raises may undo the
/// first one).
pub const MAX_FIXES: u32 = 2;

/// The order a batch asks for, back to front: its raises (`raises`, in the order sent; a
/// window raised twice counts where it was raised last), the standard windows only
/// (`standard`: a child's place is its app's business), with the batch's focus (`focus`),
/// which raises its window again, last.
pub fn asked_order(raises: &[u32], focus: Option<u32>, standard: impl Fn(u32) -> bool) -> Vec<u32> {
    let mut order: Vec<u32> = Vec::new();
    for &id in raises.iter().chain(focus.iter()) {
        if !standard(id) {
            continue;
        }
        order.retain(|&o| o != id);
        order.push(id);
    }
    order
}

/// The windows of `asked` as the window server stacks them, back to front, from its list
/// `listed` (window ids, front to back; a window not listed is left out).
pub fn listed_order(asked: &[u32], listed: &[u32]) -> Vec<u32> {
    let mut got: Vec<u32> = listed.iter().copied().filter(|id| asked.contains(id)).collect();
    got.dedup();
    got.reverse();
    got
}

/// The windows to raise again, back to front, for `got` (the window server's order, back to
/// front, [`listed_order`]) to become `asked`: those above the longest bottom run of `asked`
/// already stacked in its order (a window raised again goes above every one not raised
/// again). Empty when the order is right. Windows not listed are left out of both.
pub fn to_raise_again(asked: &[u32], got: &[u32]) -> Vec<u32> {
    let asked: Vec<u32> = asked.iter().copied().filter(|id| got.contains(id)).collect();
    let mut settled = 0;
    for id in got {
        if asked.get(settled) == Some(id) {
            settled += 1;
        }
    }
    asked[settled..].to_vec()
}

/// True when window `id` shows above each of `below` that `got` (back to front) lists; false
/// when `id` itself is not listed.
pub fn shows_above(id: u32, below: &[u32], got: &[u32]) -> bool {
    let Some(at) = got.iter().position(|&g| g == id) else { return false };
    below.iter().filter_map(|b| got.iter().position(|g| g == b)).all(|b| b < at)
}

/// True when the window server's frontmost window (`listed`, front to back) is neither one of
/// `asked` nor the one that was in front when the check began (`front_then`, a pinned window
/// over the stage, say): the user brought another window forward.
pub fn front_moved(listed: &[u32], asked: &[u32], front_then: Option<u32>) -> bool {
    listed.first().is_some_and(|f| !asked.contains(f) && Some(*f) != front_then)
}

/// The windows `asked` puts below window `id`: what it must show above once raised again.
pub fn below(asked: &[u32], id: u32) -> &[u32] {
    asked.iter().position(|&a| a == id).map_or(&[], |at| &asked[..at])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asked_order_is_the_raises_with_the_focus_last() {
        // show_stage: Mail, Safari, then Calendar (the key) and its sheet, then the focus.
        let asked = asked_order(&[169, 172, 161, 900], Some(161), |id| id != 900);
        assert_eq!(asked, [169, 172, 161]);
        // finish_ahead: the window it focuses raised in its place, then focused.
        assert_eq!(asked_order(&[1, 2, 3], Some(2), |_| true), [1, 3, 2]);
        // A window raised twice counts where it was raised last.
        assert_eq!(asked_order(&[1, 2, 1, 3], None, |_| true), [2, 1, 3]);
        assert!(asked_order(&[], None, |_| true).is_empty());
    }

    #[test]
    fn listed_order_reads_the_window_servers_list_back_to_front() {
        // Front to back: Calendar, Mail, a pinned window, Safari, the Desktop's others.
        let listed = [161, 169, 500, 172, 7, 8];
        assert_eq!(listed_order(&[169, 172, 161], &listed), [172, 169, 161]);
        // A window not listed (closed, minimized) is left out.
        assert_eq!(listed_order(&[169, 172, 161, 4], &listed), [172, 169, 161]);
    }

    #[test]
    fn the_right_order_raises_nothing_again() {
        assert!(to_raise_again(&[169, 172, 161], &[169, 172, 161]).is_empty());
        // A window not listed changes nothing.
        assert!(to_raise_again(&[169, 4, 172, 161], &[169, 172, 161]).is_empty());
        assert!(to_raise_again(&[], &[]).is_empty());
    }

    #[test]
    fn two_apps_swapped_raise_the_upper_one_and_the_key_again() {
        // 10-27: Mail raised first ended above Safari.
        assert_eq!(to_raise_again(&[169, 172, 161], &[172, 169, 161]), [172, 161]);
        // And the other way round at the next arrival.
        assert_eq!(to_raise_again(&[172, 169, 161], &[169, 172, 161]), [169, 161]);
    }

    #[test]
    fn the_key_behind_raises_only_the_key_again() {
        assert_eq!(to_raise_again(&[1, 2, 3], &[1, 3, 2]), [3]);
    }

    #[test]
    fn a_bottom_window_out_of_place_raises_all_above_it() {
        // The bottom window landed in front: everything asked above it goes again.
        assert_eq!(to_raise_again(&[1, 2, 3], &[2, 3, 1]), [2, 3]);
        assert_eq!(to_raise_again(&[1, 2, 3, 4], &[4, 3, 2, 1]), [2, 3, 4]);
    }

    #[test]
    fn raised_again_in_turn_the_order_comes_right() {
        // Each window raised again goes on top of the asked ones: the plan ends right.
        for got in [[1, 2, 3, 4], [2, 1, 3, 4], [4, 3, 2, 1], [3, 1, 4, 2], [1, 4, 2, 3]] {
            let asked = [1, 2, 3, 4];
            let mut stack = got.to_vec();
            for id in to_raise_again(&asked, &got) {
                stack.retain(|&s| s != id);
                stack.push(id);
            }
            assert_eq!(stack, asked, "from {got:?}");
        }
    }

    #[test]
    fn another_window_in_front_is_the_user_acting() {
        let asked = [169, 172, 161];
        // A window of the stage in front, even a late one out of place: not the user.
        assert!(!front_moved(&[161, 172, 169], &asked, Some(161)));
        assert!(!front_moved(&[169, 161, 172], &asked, Some(161)));
        // A pinned window that was in front from the start stays there: not the user.
        assert!(!front_moved(&[500, 161, 172, 169], &asked, Some(500)));
        // ⌘Tab to another app, ⌘` to another window: its window in front.
        assert!(front_moved(&[146, 161, 172, 169], &asked, Some(161)));
        assert!(front_moved(&[146, 500, 161], &asked, Some(500)));
        assert!(!front_moved(&[], &asked, None));
    }

    #[test]
    fn shows_above_those_below_it() {
        let got = [172, 169, 161];
        assert!(shows_above(169, &[172], &got));
        assert!(!shows_above(172, &[169], &got));
        assert!(shows_above(161, below(&[169, 172, 161], 161), &got));
        // Windows below that are not listed do not count; an unlisted window is not above.
        assert!(shows_above(172, &[4], &got));
        assert!(!shows_above(4, &[172], &got));
        // The bottom window asked has nothing to show above.
        assert!(below(&[169, 172, 161], 169).is_empty());
        assert!(below(&[169, 172, 161], 4).is_empty());
    }
}
