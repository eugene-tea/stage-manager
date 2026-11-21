//! The bindings of the window server's Desktops to the model's Spaces: made at an arrival,
//! checked at the next ones, kept right through a relabel, a prune and a fallback, saved
//! with their uuids. Made-up titles only.

use super::*;
use crate::engine::model::Home;
use crate::engine::persist::{LaunchHints, StateFile};
use crate::engine::recovery::EngineKind;
use crate::engine::testkit::{MAIN, meta, model, w};

fn desk(id: u64, uuid: &str) -> Desk {
    Desk { id, uuid: uuid.into() }
}

/// An arrival on Desktop `d` listing windows `live` (of their own apps), `absent` missing.
fn arrive(m: &mut Model, d: &Desk, live: &[u32], absent: &[u32]) -> Agree {
    let live: Vec<WindowMeta> = live.iter().map(|&id| meta(id, id as i32 * 10, &format!("com.example.app{id}"))).collect();
    let absent: Vec<WindowId> = absent.iter().map(|&id| w(id)).collect();
    m.arrive_on(MAIN, Some(d), None, &live, &absent, &LaunchHints::default());
    m.last_arrival().desk.expect("a listing was given").1
}

#[test]
fn an_arrival_binds_its_desktop_and_the_next_ones_check_it() {
    let mut m = model();
    assert!(!m.keyed());
    assert_eq!(arrive(&mut m, &desk(1, ""), &[1], &[]), Agree::New);
    let one = m.current_space(MAIN);
    assert!(m.keyed());
    assert_eq!((m.space_of_desk(1), m.desk_of_space(one), m.shown_desk(MAIN), m.desk_of(w(1))), (Some(one), Some(1), Some(1), Some(1)));
    assert_eq!(arrive(&mut m, &desk(4, "U4"), &[2], &[1]), Agree::New, "another Desktop, a Space of its own");
    let four = m.current_space(MAIN);
    assert_ne!(one, four);
    assert_eq!(m.desk_of(w(2)), Some(4));
    assert_eq!(arrive(&mut m, &desk(1, ""), &[1], &[2]), Agree::Yes, "back: the guess names the Space bound");
    assert_eq!(m.current_space(MAIN), one);
    // The guess and the listing apart: the model stays on its Space, the bindings as they were.
    assert_eq!(arrive(&mut m, &desk(4, "U4"), &[1], &[]), Agree::No, "Desktop 4 listed, Desktop 1's windows on screen");
    assert_eq!(m.current_space(MAIN), one, "the arrival decided as ever");
    assert_eq!((m.space_of_desk(4), m.space_of_desk(1)), (Some(four), Some(one)));
    assert_eq!(m.desk_agrees(one, 4), Some(false));
    assert_eq!(m.desk_agrees(one, 1), Some(true));
    assert_eq!(m.desk_agrees(SpaceId(99), 77), None, "neither bound: nothing to say");
    // A uuid that changed under the same id is taken.
    assert_eq!(arrive(&mut m, &desk(1, "U1"), &[1], &[]), Agree::Yes);
    assert_eq!(m.desks()[&1].uuid, "U1");
}

#[test]
fn an_arrival_with_no_listing_binds_nothing() {
    let mut m = model();
    m.arrive_on(MAIN, None, None, &[meta(1, 10, "com.example.a")], &[], &LaunchHints::default());
    assert!(!m.keyed());
    assert_eq!(m.last_arrival().desk, None);
}

/// An empty Space is taken for the next Desktop the model never saw (it stands for no
/// Desktop): its binding goes with it, never a disagreement.
#[test]
fn an_empty_desktop_left_for_a_new_one_takes_the_new_desktop() {
    let mut m = model();
    assert_eq!(arrive(&mut m, &desk(1, ""), &[], &[]), Agree::New);
    let space = m.current_space(MAIN);
    assert_eq!(arrive(&mut m, &desk(4, "U4"), &[2], &[]), Agree::New);
    assert_eq!(m.current_space(MAIN), space, "the model's way: the empty Space shown is the new Desktop");
    assert_eq!((m.space_of_desk(4), m.space_of_desk(1)), (Some(space), None));
}

#[test]
fn the_bindings_follow_a_relabel_and_go_with_a_pruned_space_and_a_fallback() {
    let mut m = model();
    arrive(&mut m, &desk(1, ""), &[1], &[]);
    let one = m.current_space(MAIN);
    m.relabel_space(one, SpaceId(40));
    assert_eq!(m.space_of_desk(1), Some(SpaceId(40)));
    assert_eq!(m.desk_of(w(1)), Some(1));
    // Desktop 4 with one window, left when the window closed: the empty Space is pruned.
    arrive(&mut m, &desk(4, "U4"), &[2], &[1]);
    let four = m.current_space(MAIN);
    m.remove_window(w(2));
    assert_eq!(arrive(&mut m, &desk(1, ""), &[1], &[]), Agree::Yes);
    assert!(!m.has_space(four), "an empty Space left is forgotten");
    assert_eq!(m.space_of_desk(4), None, "and its Desktop is bound to nothing");
    // A relabel onto a Space bound already: one Desktop per Space.
    arrive(&mut m, &desk(4, "U4"), &[3], &[1]);
    let four = m.current_space(MAIN);
    m.relabel_space(four, SpaceId(40));
    assert_eq!((m.space_of_desk(4), m.space_of_desk(1)), (Some(SpaceId(40)), None));
    // The engine falls back: no listing any more.
    let other = if m.options().engine == EngineKind::Spaces { EngineKind::Corner } else { EngineKind::Spaces };
    m.set_engine(other);
    assert!(!m.keyed());
}

#[test]
fn the_state_file_keeps_the_bindings_uuids_and_reads_a_file_without_them() {
    let mut m = model();
    arrive(&mut m, &desk(1, ""), &[1], &[]);
    arrive(&mut m, &desk(4, "U4"), &[2], &[1]);
    let file = m.to_state();
    let spaces: Vec<u64> = file.spaces.iter().map(|s| s.id).collect();
    let mut want: Vec<SavedDesk> = [(1, ""), (4, "U4")]
        .iter()
        .map(|&(d, uuid)| SavedDesk { space: m.space_of_desk(d).expect("bound").0, uuid: uuid.into() })
        .collect();
    want.sort_by_key(|d| d.space);
    assert_eq!(file.desks, want);
    assert!(want.iter().all(|d| spaces.contains(&d.space)));
    let text = file.to_json().expect("encodes");
    assert!(text.contains("\"desks\""));
    let back = StateFile::from_json(&text).expect("reads back");
    assert_eq!(back.desks, file.desks);
    // A file from before: no `desks` at all, read as none; and none written when none bound.
    let mut old = StateFile::from_json(&text.replace("\"desks\"", "\"ignored\"")).expect("reads");
    assert!(old.desks.is_empty());
    old.desks.clear();
    assert!(!old.to_json().expect("encodes").contains("\"desks\""));
    // Loaded into a model: written again for the saved Spaces not visited yet, by their uuids.
    let mut next = model();
    next.restore_spaces(back.spaces.clone());
    next.restore_desks(back.desks.clone());
    let again = next.to_state();
    assert_eq!(again.desks, back.desks, "the saved Spaces wait unvisited, their Desktops with them");
}

// ----- keyed: the listing decides (design r3, slices 2 and 3) --------------------------------------

/// A keyed arrival on Desktop `d`: `live` are the windows the window server has on it.
fn arrive_keyed(m: &mut Model, d: &Desk, listed: &Listed, live: &[u32]) -> Vec<Action> {
    let live: Vec<WindowMeta> = live.iter().map(|&id| meta(id, id as i32 * 10, &format!("com.example.app{id}"))).collect();
    m.arrive_on(MAIN, Some(d), Some(listed), &live, &[], &LaunchHints::default())
}

/// Desktop 1 with windows 1 and 2 (a stage each, 2's hidden) and Desktop 4 with window 3,
/// each bound; Desktop 1 shown.
fn two_desktops() -> (Model, SpaceId, SpaceId) {
    let mut m = model();
    arrive(&mut m, &desk(1, ""), &[1, 2], &[]);
    let one = m.current_space(MAIN);
    arrive(&mut m, &desk(4, "U4"), &[3], &[1, 2]);
    let four = m.current_space(MAIN);
    assert_eq!(arrive(&mut m, &desk(1, ""), &[1, 2], &[3]), Agree::Yes);
    assert_ne!(one, four);
    (m, one, four)
}

#[test]
fn keyed_the_space_bound_to_the_desktop_arrives_whatever_is_on_screen() {
    let (mut m, one, four) = two_desktops();
    // Nothing of Desktop 4 listed yet (its windows not on screen, or not known): the vote
    // would stay on Desktop 1; the listing says Desktop 4.
    arrive_keyed(&mut m, &desk(4, "U4"), &Listed::default(), &[]);
    assert_eq!(m.current_space(MAIN), four);
    assert_eq!(m.last_arrival().desk, Some((4, Agree::Yes)));
    assert_eq!(m.last_arrival().beside, Vec::<u64>::new());
    // And back, with Desktop 4's window still among the windows on screen in the guess's
    // eyes: keyed, the platform left it out, so only Desktop 1's are given.
    arrive_keyed(&mut m, &desk(1, ""), &Listed::default(), &[1, 2]);
    assert_eq!(m.current_space(MAIN), one);
    assert_eq!((m.space_of(w(3)), m.last_arrival().moved), (Some(four), 0));
}

#[test]
fn keyed_a_desktop_never_seen_is_a_new_space_and_an_empty_bound_one_is_not_reused() {
    let mut m = model();
    arrive(&mut m, &desk(1, ""), &[], &[]);
    let one = m.current_space(MAIN);
    // Desktop 1 holds nothing, but it is Desktop 1: Desktop 4 is another Space.
    arrive_keyed(&mut m, &desk(4, "U4"), &Listed::default(), &[2]);
    let four = m.current_space(MAIN);
    assert_ne!(one, four);
    assert_eq!(m.last_arrival().desk, Some((4, Agree::New)));
    assert_eq!((m.space_of_desk(4), m.space_of(w(2))), (Some(four), Some(four)));
    // The empty Space left was forgotten with its binding; back there it is made again.
    assert_eq!(m.space_of_desk(1), None);
    arrive_keyed(&mut m, &desk(1, ""), &Listed::default(), &[]);
    assert_eq!(m.shown_desk(MAIN), Some(1));
    assert_eq!(m.space_of(w(2)), Some(four), "Desktop 4's window stays its own");
}

#[test]
fn keyed_a_known_window_listed_here_was_moved_at_once() {
    let (mut m, one, four) = two_desktops();
    // Window 1 dragged to Desktop 4 in Mission Control, then Desktop 4 shown.
    arrive_keyed(&mut m, &desk(4, "U4"), &Listed::default(), &[1, 3]);
    assert_eq!(m.current_space(MAIN), four);
    assert_eq!((m.space_of(w(1)), m.last_arrival().moved), (Some(four), 1), "moved at the first arrival, no second look");
    assert_eq!(m.space_of(w(2)), Some(one));
    assert!(!m.is_sticky(w(1)));
}

#[test]
fn keyed_a_window_listed_on_two_desktops_is_on_every_desktop_at_once() {
    let (mut m, _, four) = two_desktops();
    let every = Listed { every: vec![w(1), w(9)], lent: Vec::new() };
    // Known window 1 and first-seen window 9 are on every Desktop; 3 is Desktop 4's.
    arrive_keyed(&mut m, &desk(4, "U4"), &every, &[1, 3, 9]);
    assert_eq!(m.current_space(MAIN), four);
    assert!(m.is_sticky(w(1)) && m.is_sticky(w(9)), "{:?}", m.last_arrival());
    assert_eq!(m.last_arrival().moved, 0, "never taken for moved");
    assert_eq!(m.window(w(1)).map(|x| x.home), Some(Home::Pinned));
    assert!(!m.is_sticky(w(3)));
    // Back on Desktop 1 they are still everywhere, and nothing moves.
    arrive_keyed(&mut m, &desk(1, ""), &every, &[1, 2, 9]);
    assert_eq!(m.last_arrival().moved, 0);
    assert!(m.is_sticky(w(1)) && m.is_sticky(w(9)));
}

#[test]
fn keyed_a_hidden_window_let_out_on_another_desktop_is_hidden_again_unless_lent() {
    let (mut m, one, four) = two_desktops();
    let hidden = [w(1), w(2)].into_iter().find(|id| m.window(*id).is_some_and(|x| x.parked)).expect("one of Desktop 1's stages is hidden");
    // The window server let it out onto Desktop 4 (a screen went): hidden again, its stage kept.
    let actions = arrive_keyed(&mut m, &desk(4, "U4"), &Listed::default(), &[hidden.0, 3]);
    assert!(actions.contains(&Action::Hide { id: hidden }), "{actions:?}");
    assert_eq!((m.space_of(hidden), m.last_arrival().moved), (Some(one), 0));
    assert_eq!(m.last_arrival().kept_hidden, vec![hidden]);
    // Lent there by the window engine and found on Desktop 4: the user moved it (Mission
    // Control shows a hidden stage's windows, and they can be dragged).
    arrive_keyed(&mut m, &desk(1, ""), &Listed::default(), &[1, 2]);
    let lent = Listed { every: Vec::new(), lent: vec![hidden] };
    arrive_keyed(&mut m, &desk(4, "U4"), &lent, &[hidden.0, 3]);
    assert_eq!((m.space_of(hidden), m.last_arrival().moved), (Some(four), 1));
}

#[test]
fn keyed_a_guess_made_without_the_listing_is_corrected_at_the_next_read() {
    let (mut m, one, four) = two_desktops();
    // The listing could not be read as Desktop 4 arrived and the windows on screen did not
    // say (window 3 minimized, a new window 5 on screen): a new Space was made.
    m.arrive_on(MAIN, None, None, &[meta(5, 50, "com.example.app5")], &[w(1), w(2)], &LaunchHints::default());
    let guessed = m.current_space(MAIN);
    assert!(guessed != one && guessed != four);
    assert_eq!(m.shown_desk(MAIN), None, "bound to no Desktop: the platform sees the difference");
    // The next good read: Desktop 4 is shown; its Space arrives and takes the window in.
    arrive_keyed(&mut m, &desk(4, "U4"), &Listed::default(), &[5]);
    assert_eq!((m.current_space(MAIN), m.space_of(w(5)), m.space_of(w(3))), (four, Some(four), Some(four)));
    assert!(!m.has_space(guessed), "the Space guessed is forgotten, empty");
}

#[test]
fn a_window_found_on_another_desktop_moves_there_or_leaves() {
    let (mut m, one, four) = two_desktops();
    let hints = LaunchHints::default();
    // On its own Desktop: nothing.
    assert_eq!(m.window_on_desk(w(1), 1, &hints), (Vec::new(), Vec::new()));
    // On Desktop 4, not shown: it leaves its stage and is forgotten until it shows.
    let (_, forgotten) = m.window_on_desk(w(1), 4, &hints);
    assert_eq!(forgotten, vec![w(1)]);
    assert!(m.window(w(1)).is_none());
    // Desktop 4 shows: window 2, found on it, joins it at once.
    arrive_keyed(&mut m, &desk(4, "U4"), &Listed::default(), &[3]);
    let (actions, forgotten) = m.window_on_desk(w(2), 4, &hints);
    assert!(forgotten.is_empty() && actions.contains(&Action::StateChanged));
    assert_eq!(m.space_of(w(2)), Some(four));
    assert!(!m.has_space(one), "Desktop 1's Space holds nothing now");
    // A Desktop bound to nothing: it leaves, as to any Desktop not shown.
    let (_, forgotten) = m.window_on_desk(w(3), 77, &hints);
    assert_eq!(forgotten, vec![w(3)]);
}
