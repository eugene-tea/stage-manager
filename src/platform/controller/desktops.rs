//! The window server's Desktops as the controller reads them (design r3, slice 1: read and
//! shadow). One read, [`Controller::listing`]: each managed screen's entry of
//! `SLSCopyManagedDisplaySpaces`, from the window engine ([`WindowEngine::desktops`]; the
//! Spaces engine only). Each arrival is given its screen's Desktop, read again at the arrival
//! ([`Model::arrive_on`], which binds it to the Space the model chose or checks it), and each
//! arrival and each look for missed changes compares the model with the listing through the
//! bindings ([`crate::engine::desk_shadow`]): the Desktop each screen shows, and each held
//! window's home. A disagreement is logged when it begins and when it ends; `ctl state`
//! carries the listing, the bindings and the counts (`desks`). Nothing here decides
//! anything.
//!
//! [`WindowEngine::desktops`]: crate::platform::window_engine::WindowEngine::desktops

use serde_json::{Value, json};

use super::Controller;
use crate::engine::desk_shadow::{self, Change, Shadow, Sight, Subject};
use crate::engine::model::desks::{Desk, SavedDesk};
use crate::engine::model::{Display, DisplayId, WindowId};
use crate::platform::clock;
use crate::platform::spaces::ffi::ScreenDesks;

/// The window server's entry for each managed screen, read at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Listing(pub(super) Vec<(DisplayId, ScreenDesks)>);

impl Listing {
    /// The Desktop screen `d` shows: `None` when it shows another kind of Space (a
    /// full-screen app's) or is not listed.
    pub(super) fn desk(&self, d: DisplayId) -> Option<Desk> {
        let (_, s) = self.0.iter().find(|(id, _)| *id == d)?;
        (s.current_kind == 0).then(|| s.desks.iter().find(|k| k.id == s.current).cloned().unwrap_or(Desk { id: s.current, uuid: String::new() }))
    }

    /// Each screen showing a Desktop, with that Desktop's id.
    fn shown(&self) -> Vec<(DisplayId, u64)> {
        self.0.iter().filter(|(_, s)| s.current_kind == 0).map(|(d, s)| (*d, s.current)).collect()
    }
}

/// Each of `screens` (id, uuid from ColorSync, main) matched to its entry of `list`: by its
/// uuid, the main screen also by the name `Main`. `None` when a screen has no entry or its
/// entry names no Space shown.
pub(super) fn match_screens(screens: &[(DisplayId, Option<String>, bool)], list: &[ScreenDesks]) -> Option<Listing> {
    let mut out = Vec::new();
    for (d, uuid, main) in screens {
        let by_uuid = uuid.as_ref().and_then(|u| list.iter().find(|s| s.screen.eq_ignore_ascii_case(u)));
        let entry = by_uuid.or_else(|| main.then(|| list.iter().find(|s| s.screen == "Main")).flatten())?;
        if entry.current == 0 {
            return None;
        }
        out.push((*d, entry.clone()));
    }
    Some(Listing(out))
}

/// The shadow's state on the WM thread.
#[derive(Debug, Default)]
pub(super) struct DeskWatch {
    shadow: Shadow,
}

impl Controller {
    /// The window server's listing for the managed screens, read now ([`match_screens`]):
    /// `None` when the engine reads none, a managed screen has no entry, or one names no
    /// Space shown.
    pub(super) fn listing(&self) -> Option<Listing> {
        self.listing_for(&self.managed_displays())
    }

    /// [`Self::listing`] for `displays` (the first the main one), before the model is built.
    pub(super) fn listing_for(&self, displays: &[Display]) -> Option<Listing> {
        let list = self.engine.desktops()?;
        let screens: Vec<(DisplayId, Option<String>, bool)> =
            displays.iter().enumerate().map(|(i, d)| (d.id, crate::platform::desktop_counts::display_uuid(d.id.0), i == 0)).collect();
        match_screens(&screens, &list)
    }

    /// The Desktop screen `d` shows for an arrival there, read now.
    pub(super) fn arrival_desk(&self, d: DisplayId) -> Option<Desk> {
        self.listing()?.desk(d)
    }

    /// The shadow's look (`why`: the arrival or the check): the model against the listing
    /// read now, each disagreement that began or ended logged.
    pub(super) fn look_at_desks(&mut self, why: &'static str) {
        let Some(listing) = self.listing() else { return };
        let Some(m) = self.model.as_ref() else { return };
        let homes: Vec<(WindowId, u64)> = m.windows().filter_map(|w| Some((w.meta.id, self.engine.home_of(w.meta.id)?))).collect();
        let seen = desk_shadow::disagreements(m, &listing.shown(), &homes);
        for change in self.desks.shadow.look(seen, clock::now()) {
            log_change(change, why);
        }
    }

    /// At a load: how many of the Desktops `state.json` saved (`saved`, by uuid) the listing
    /// lists now (design r3, open question 2: do uuids last over a restart). Decides
    /// nothing.
    pub(super) fn note_saved_desks(saved: &[SavedDesk], listing: Option<&Listing>) {
        if saved.is_empty() {
            return;
        }
        let Some(listing) = listing else {
            tracing::info!(saved = saved.len(), "desktops: state.json names Desktops by uuid; the window server's listing is not read here");
            return;
        };
        let listed: Vec<&str> = listing.0.iter().flat_map(|(_, s)| s.desks.iter().map(|d| d.uuid.as_str())).collect();
        let named = saved.iter().filter(|d| !d.uuid.is_empty()).count();
        let found = saved.iter().filter(|d| !d.uuid.is_empty() && listed.contains(&d.uuid.as_str())).count();
        tracing::info!(
            saved = saved.len(),
            named,
            listed_now = found,
            empty_saved = saved.len() - named,
            empty_listed = listed.iter().filter(|u| u.is_empty()).count(),
            "desktops: Desktops saved in state.json by uuid that the window server lists now"
        );
    }

    /// `ctl state`'s `desks`: the listing per screen, the bindings, the disagreements.
    pub(super) fn desks_json(&self) -> Value {
        let now = clock::now();
        let listing = self.listing().map(|l| {
            l.0.iter()
                .map(|(d, s)| {
                    json!({
                        "display": d.0,
                        "current": s.current,
                        "current_kind": s.current_kind,
                        "desks": s.desks.iter().map(|k| json!({ "id": k.id, "uuid": k.uuid })).collect::<Vec<_>>(),
                        "others": s.others,
                    })
                })
                .collect::<Vec<_>>()
        });
        let bindings: Vec<Value> = self
            .model
            .as_ref()
            .map(|m| m.desks().iter().map(|(id, b)| json!({ "desk": id, "space": b.space.0, "uuid": b.uuid })).collect())
            .unwrap_or_default();
        let s = &self.desks.shadow;
        let open: Vec<Value> = s
            .open()
            .map(|(sight, since)| {
                let (what, id) = subject(sight.subject);
                json!({ "what": what, "id": id, "space": sight.space.0, "model": sight.model, "server": sight.server, "since_ms": ms(now.saturating_duration_since(*since)) })
            })
            .collect();
        json!({
            "listing": listing,
            "bindings": bindings,
            "disagree": {
                "shown": s.shown_open(),
                "shown_begun": s.shown_begun,
                "homes": s.homes_open(),
                "homes_begun": s.homes_begun,
                "longest_ms": ms(s.longest(now)),
                "open": open,
            },
        })
    }
}

fn ms(d: std::time::Duration) -> u64 {
    d.as_millis() as u64
}

/// The subject's kind and id, for the log and `ctl state`.
fn subject(s: Subject) -> (&'static str, u32) {
    match s {
        Subject::Shown(d) => ("shown", d.0),
        Subject::Home(w) => ("home", w.0),
    }
}

/// One line per disagreement begun or ended: ids only.
fn log_change(change: Change, why: &'static str) {
    let line = |s: &Sight| (subject(s.subject), s.space.0, s.model, s.server);
    match change {
        Change::Began(s) => {
            let ((what, id), space, model, server) = line(&s);
            tracing::info!(what, id, space, model_desk = ?model, server_desk = server, why, "desktops: the model and the window server disagree");
        }
        Change::Ended { sight, lasted } => {
            let ((what, id), space, model, server) = line(&sight);
            tracing::info!(what, id, space, model_desk = ?model, server_desk = server, lasted_ms = ms(lasted), why, "desktops: the model and the window server agree again");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(screen: &str, current: u64, kind: i64, desks: &[u64]) -> ScreenDesks {
        ScreenDesks {
            screen: screen.into(),
            current,
            current_kind: kind,
            desks: desks.iter().map(|&id| Desk { id, uuid: format!("D{id}") }).collect(),
            others: Vec::new(),
        }
    }

    #[test]
    fn each_managed_screen_takes_its_entry_or_there_is_no_listing() {
        let (main, side) = (DisplayId(1), DisplayId(2));
        let list = [entry("Main", 4, 0, &[1, 4]), entry("AB-CD", 9, 4, &[8])];
        let screens = [(main, Some("11-22".to_string()), true), (side, Some("ab-cd".to_string()), false)];
        let l = match_screens(&screens, &list).expect("both listed");
        assert_eq!(l.desk(main), Some(Desk { id: 4, uuid: "D4".into() }), "the main screen by its name");
        assert_eq!(l.desk(side), None, "a full-screen app's Space shows there");
        assert_eq!(l.shown(), vec![(main, 4)]);
        // A managed screen the listing does not name, or an entry with nothing shown: none.
        assert_eq!(match_screens(&[(side, Some("EF".into()), false)], &list), None);
        assert_eq!(match_screens(&[(main, None, true)], &[entry("Main", 0, -1, &[1])]), None);
        // By uuid before the name.
        let list = [entry("Main", 4, 0, &[1, 4]), entry("11-22", 7, 0, &[7])];
        assert_eq!(match_screens(&[(main, Some("11-22".into()), true)], &list).and_then(|l| l.desk(main)).map(|d| d.id), Some(7));
    }
}
