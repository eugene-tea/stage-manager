//! The bindings of the window server's Desktops to the model's Spaces: made at an arrival,
//! checked at the next ones, kept right through a relabel, a prune and a fallback, saved
//! with their uuids. Made-up titles only.

use super::*;
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
    m.arrive_on(MAIN, Some(d), &live, &absent, &LaunchHints::default());
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
    m.arrive_on(MAIN, None, &[meta(1, 10, "com.example.a")], &[], &LaunchHints::default());
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
