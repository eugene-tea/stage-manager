//! What keeps the tick brisk at rest (round rest-cost): duties that only wait must let it
//! rest.

use super::scenarios::{B, C, D1, D2, both_desktops_known};
use super::*;

/// Audit U3 (11-25): a Dock click on Charlie, whose only window is on Desktop 2, arranges
/// Desktop 2 at the press; no activation follows (the click went nowhere), so the
/// arrangement is undone a second later. The window put back on Desktop 2, which does not
/// show, stays in the safety registry until it is seen there
/// (`Controller::keep_until_seen`): nothing can be retried for it meanwhile, and it kept the
/// tick brisk ("shows owed") until Desktop 2 next showed, for hours if need be. It keeps its
/// entry, the tick rests, and the visit to Desktop 2 lets it go.
#[test]
fn a_show_owed_that_only_waits_for_its_desktop_lets_the_tick_rest() {
    let mut b = both_desktops_known(EngineKind::Corner, 1.0, |d| {
        d.add_app(C, "com.example.charlie", "Charlie");
        d.add_window(14, C, frame(4), D2);
        d.raise(13);
        d.raise(15);
        d.raise(11);
        d.raise(12);
        d.apps.get_mut(&C).expect("Charlie").activates_ms = Some(10_000_000.0);
    });
    b.dock_click(C, 90);
    b.advance(1500);
    let owed: Vec<u32> = b.c.corner.unrestored.keys().copied().collect();
    assert!(!owed.is_empty(), "the undo left a window registered until it is seen back:\n{}", b.story());
    b.advance(10_000);
    let (front, shown, app) = (b.front(), b.desktop_shown(), b.front_app());
    assert_eq!((front, shown, app), (Some(vec![12]), D1, Some(B)), "the click went nowhere:\n{}", b.story());
    let waiting: Vec<(u32, bool)> = b.c.corner.unrestored.iter().map(|(id, u)| (*id, u.waits)).collect();
    assert!(!waiting.is_empty() && waiting.iter().all(|(_, waits)| *waits), "still on record, only waiting for Desktop 2: {waiting:?}\n{}", b.story());
    assert_eq!(b.c.tick_waits(clock::now()), None, "nothing keeps the tick brisk:\n{}", b.story());
    // A minute at rest: a tick every 2 s, not every quarter second.
    let before = b.ticks;
    b.advance(60_000);
    let ticks = b.ticks - before;
    assert!((27..=31).contains(&ticks), "{ticks} ticks in a minute at rest (240 while the entry kept it brisk)");
    assert!(!b.c.corner.unrestored.is_empty(), "the entry waits on");
    // Desktop 2 shows: the window is seen where it was put back, and let go.
    b.go_to_desktop(D2, true);
    b.advance(6000);
    assert!(b.c.corner.unrestored.is_empty(), "seen back on its Desktop: {:?}\n{}", b.c.corner.unrestored.keys().collect::<Vec<_>>(), b.story());
}
