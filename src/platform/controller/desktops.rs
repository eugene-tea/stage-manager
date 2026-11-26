//! The window server's Desktops as the controller reads them (design r3). One read,
//! [`Controller::listing`]: each managed screen's entry of `SLSCopyManagedDisplaySpaces`,
//! through the window engine ([`WindowEngine::desktops`]) from the reads' own layer
//! ([`crate::platform::desk_reads`]), the same under the Corner and the Spaces engine since
//! round corner-desktops. The layer has its own check ([`Controller::check_reads`], at the
//! tick's look: the listing against the windows on screen) and answers nothing while the
//! reads rest.
//!
//! **While it can be read and a Desktop is bound** ([`Controller::keyed_listing`]) the
//! listing decides, in place of the guesses from the window list (slices 2 and 3):
//!
//! - **Which Desktop arrived, and where** ([`Controller::desk_differences`]): per screen, the
//!   listing's Space shown against the Desktop the model's Space there is bound to, and its
//!   kind against the full screen the controller counts. A difference is that screen's
//!   Space change, and only the screens that differ arrive. The driver runs at the Space
//!   change's notice ([`Controller::flip_step`]: the notice comes 1-9 ms before the listing
//!   flips, so none yet is waited for on the fast tick, [`FLIP_WAIT`] at most, then taken
//!   for no change) and at every look for missed changes ([`Controller::sync_desktops`]: one
//!   found there is taken at once, and its notice, when it comes after, does nothing more,
//!   [`Controller::announced_late`]). The id an arrival uses is read again at the arrival.
//! - **Which windows are on the Desktop** ([`Controller::keyed_live`]): each window of an
//!   arrival is asked for its Spaces (22 µs); one not on the arriving Desktop is left out (a
//!   Desktop still listed while it slides away, a window just shown on another, the Corner
//!   engine's parked windows of the Desktop left), one on two Desktops or more is on every
//!   Desktop. No wait for a list that shows one Desktop only,
//!   no second look, no settle after.
//! - **Where a window went** ([`Controller::desk_membership`], at the tick): a window of the
//!   Desktop shown that is not on screen, or one of another Desktop that is, is asked for
//!   its Desktop once: on its own, nothing; on one other, it moved there
//!   ([`Model::window_on_desk`]); on none (hidden in a stage's Space, in full screen),
//!   nothing. A window hidden in the park corner stays on its Desktop and is asked as a
//!   shown one is (Mission Control shows it, and it can be dragged to another Desktop); one
//!   hidden in a stage Space is on no Desktop and is not asked.
//! - **A window found** ([`Controller::found_on_desk_shown`]) is placed only when it is on
//!   the Desktop the model shows, and an activation whose app's focused window is on another
//!   Desktop belongs to that Desktop ([`Controller::focus_on_another_desk`]).
//!
//! With no listing (the reads missing or resting, the Apple mode, a screen with no entry,
//! an entry naming no Space shown) every one of these runs as before, by the windows on
//! screen: the paths part on what could be read, never on the engine's kind. Where the two
//! engines differ here it is by where a hidden window is (`Model::hidden_on_their_desktop`:
//! the park corner is on its Desktop, a stage Space on none). A guess made so is corrected at the next
//! good read: the model's Space then is bound to no Desktop, or to another, which is a
//! difference.
//!
//! Each arrival and each look also compares the model with the listing through the bindings
//! ([`crate::engine::desk_shadow`]): the Desktop each screen shows, and each held window's
//! home. A disagreement is logged when it begins and when it ends; `ctl state` carries the
//! listing, the bindings and the counts (`desks`).
//!
//! [`WindowEngine::desktops`]: crate::platform::window_engine::WindowEngine::desktops

use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::corner::unrestored::LetGo;
use super::{Controller, launch_hints};
use crate::engine::desk_reads::{self as desk_reads_check, Screen, Verdict};
use crate::engine::desk_shadow::{self, Change, Shadow, Sight, Subject};
use crate::engine::geometry::Rect;
use crate::engine::margin;
use crate::engine::spill;
use crate::engine::model::desks::{Desk, Listed, SavedDesk};
use crate::engine::model::{Display, DisplayId, WindowId, WindowMeta, WindowRole};
use crate::engine::phase::{self, Hold};
use crate::platform::observe::Observer;
use crate::platform::spaces::ffi::ScreenDesks;
use crate::platform::{clock, desk_reads, input, windows};

/// How long after a Space change's notice the listing may still name the Space left before
/// the notice is taken for one with no change. Measured 11-21: the notice 1-9 ms before the
/// flip, 24 of 24, never after; this is a cap, and each wait's end is noted against it.
pub(super) const FLIP_WAIT: Duration = Duration::from_millis(300);

/// The window server's entry for each managed screen, read at one moment, and every Desktop
/// it lists on any screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Listing(pub(super) Vec<(DisplayId, ScreenDesks)>, pub(super) BTreeSet<u64>);

/// What the listing says at a look after a Space change's notice ([`Controller::flip_step`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Flip {
    /// It names the Space left still: looked at again on the fast tick.
    Wait,
    /// It never named another ([`FLIP_WAIT`]): no change.
    Unchanged,
    /// These screens show another Space than the model has there.
    Differs(Vec<DisplayId>),
}

impl Listing {
    /// The kind of the Space screen `d` shows: 0 a Desktop, else a full-screen app's.
    pub(super) fn kind(&self, d: DisplayId) -> Option<i64> {
        self.0.iter().find(|(id, _)| *id == d).map(|(_, s)| s.current_kind)
    }

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
/// entry names no Space shown or not its kind.
pub(super) fn match_screens(screens: &[(DisplayId, Option<String>, bool)], list: &[ScreenDesks]) -> Option<Listing> {
    let mut out = Vec::new();
    for (d, uuid, main) in screens {
        let by_uuid = uuid.as_ref().and_then(|u| list.iter().find(|s| s.screen.eq_ignore_ascii_case(u)));
        let entry = by_uuid.or_else(|| main.then(|| list.iter().find(|s| s.screen == "Main")).flatten())?;
        if entry.current == 0 || entry.current_kind < 0 {
            return None;
        }
        out.push((*d, entry.clone()));
    }
    Some(Listing(out, list.iter().flat_map(|s| s.desks.iter().map(|d| d.id)).collect()))
}

/// True when a window at `bounds` lies wholly on the screen at `screen`: a window the
/// reads' check counts for that screen. One that reaches beyond its edge (parked in the
/// corner, dragged partly off, across two screens) says nothing of the Space the screen
/// shows: with a screen beyond that edge, macOS may count it on either.
fn wholly_on(bounds: Rect, screen: Rect) -> bool {
    bounds.w > 0.0 && bounds.h > 0.0 && bounds.x >= screen.x && bounds.y >= screen.y && bounds.max_x() <= screen.max_x() && bounds.max_y() <= screen.max_y()
}

/// Each of `displays` as [`match_screens`] takes them: its id, its uuid from ColorSync, and
/// whether it is the main one (the first).
fn screens_of(displays: &[Display]) -> Vec<(DisplayId, Option<String>, bool)> {
    displays.iter().enumerate().map(|(i, d)| (d.id, crate::platform::desktop_counts::display_uuid(d.id.0), i == 0)).collect()
}

/// The Desktops' state on the WM thread: the shadow, and what the listing decided.
#[derive(Debug, Default)]
pub(super) struct DeskWatch {
    shadow: Shadow,
    /// A Space change found at a look before its notice came: the notice, when it finds no
    /// difference, does nothing more. Cleared by that notice and by the next look.
    taken_unannounced: bool,
    /// Arrivals the listing named.
    arrivals: u64,
    /// Notices that came before the flip and were waited for, and those never followed by one.
    flips_waited: u64,
    flips_capped: u64,
    /// Changes found at a look before their notice, and notices that came after one.
    unannounced: u64,
    announced_late: u64,
    /// Windows left out of an arrival because they were on another Desktop.
    left_out: u64,
    /// Windows found on another Desktop at a look: moved to one shown, or let go.
    moved: u64,
    left: u64,
    /// New windows not placed because they were on another Desktop than the one shown.
    found_elsewhere: u64,
    /// The bound Desktops the looks found unlisted, since when ([`spill::ended_desktops`]),
    /// and how many ended and were taken into the Desktop their windows landed on.
    unlisted: spill::Unlisted,
    ended: u64,
}

impl Controller {
    /// The window server's listing for the managed screens, read now ([`match_screens`]):
    /// `None` when the engine reads none, a managed screen has no entry, or one names no
    /// Space shown.
    ///
    /// A display change is taken from it as any Space change is, under either engine (since
    /// round display-gone; before, under the Corner engine, by the windows on screen for the
    /// change's 10 s settle, its bindings dropped). When a screen goes the model takes the
    /// Desktop it showed into the one shown on the screen left (the adoption) while macOS
    /// may keep that Desktop, or end it and put its windows on another Desktop and show
    /// that one: the arrival there carries the gone screen's stages whole (`carry_stages`),
    /// and the Desktop they leave shows its own most recent stage again. The guess took the
    /// windows of the screen left's own Desktop, no longer shown, for windows gone from it
    /// (live 11-25: they left their stages).
    pub(super) fn listing(&self) -> Option<Listing> {
        self.listing_for(&self.managed_displays())
    }

    /// [`Self::listing`] for `displays` (the first the main one), before the model is built.
    pub(super) fn listing_for(&self, displays: &[Display]) -> Option<Listing> {
        let list = self.engine.desktops()?;
        match_screens(&screens_of(displays), &list)
    }

    /// The reads' check (`engine/desk_reads.rs`, 3), at a look while nothing moves: of the
    /// windows the window list shows wholly on each managed screen that shows a Desktop
    /// ([`wholly_on`]), one must be on the Space the listing names there. Read whatever the reads' state (after a
    /// rest they are read for this alone), one window at a time until one agrees, and only
    /// when a look is due. True when the reads rest from now: this look goes on by the guess.
    pub(super) fn check_reads(&mut self, ob: &Observer, list: &[windows::CgWindow], now: Instant) -> bool {
        if !desk_reads::check_due(now) || self.locked || input::left_button_down() || self.displays_settling() {
            return false;
        }
        let displays = self.managed_displays();
        // A pass with no entry for a screen goes by the guess already: nothing to compare.
        let Some(listing) = desk_reads::raw_desktops().and_then(|raw| match_screens(&screens_of(&displays), &raw)) else { return false };
        let rects: Vec<(DisplayId, Rect)> = displays.iter().map(|d| (d.id, d.frame)).collect();
        let mut screens: Vec<Screen> = Vec::new();
        let mut asked: Vec<(u32, u32)> = Vec::new();
        for (d, current) in listing.shown() {
            let mut screen = Screen { current, windows: Vec::new() };
            let Some(frame) = rects.iter().find(|(id, _)| *id == d).map(|(_, r)| *r) else { continue };
            for w in list.iter().filter(|w| w.onscreen && w.layer == 0 && ob.window(w.id).is_some() && wholly_on(w.bounds, frame)) {
                let on = desk_reads::raw_spaces_of(&[w.id]).unwrap_or_default();
                let agrees = on.contains(&current);
                screen.windows.push(on);
                asked.push((d.0, w.id));
                if agrees {
                    break;
                }
            }
            screens.push(screen);
        }
        let verdict = desk_reads_check::cross_check(&screens);
        if let Verdict::Disagree(why) = verdict {
            // Ids only: each screen's Space shown with its windows' Spaces, and the windows asked.
            tracing::debug!(why, ?screens, ?asked, "desktops: the listing and the windows on screen disagree at this look");
        }
        desk_reads::checked(verdict, now)
    }

    /// The Desktop screen `d` shows for an arrival there, read now.
    pub(super) fn arrival_desk(&self, d: DisplayId) -> Option<Desk> {
        self.listing()?.desk(d)
    }

    /// The listing while it decides: readable now, and the model has a Desktop bound (the
    /// launch's arrival binds the first). `None`: every decision is made as before, by the
    /// windows on screen.
    pub(super) fn keyed_listing(&self) -> Option<Listing> {
        if !self.model.as_ref()?.keyed() {
            return None;
        }
        self.listing()
    }

    /// The kind of the Space the managed screen at `frame` shows, while the listing decides.
    pub(super) fn listed_kind(&self, frame: crate::engine::geometry::Rect) -> Option<i64> {
        let d = self.managed_displays().into_iter().find(|d| d.frame == frame)?;
        self.keyed_listing()?.kind(d.id)
    }

    /// The screens whose Space shown is another than the model has there: the main screen
    /// when the listing's kind (a Desktop, a full-screen app's Space) is not the one the
    /// controller counts; any screen showing a Desktop the model's Space there is not bound to.
    pub(super) fn desk_differences(&self, listing: &Listing) -> Vec<DisplayId> {
        let Some(m) = self.model.as_ref() else { return Vec::new() };
        let main = self.main_display_id();
        listing
            .0
            .iter()
            .filter(|(d, s)| {
                let full = s.current_kind != 0;
                (*d == main && full != self.phase.fullscreen()) || (!full && m.shown_desk(*d) != Some(s.current))
            })
            .map(|(d, _)| *d)
            .collect()
    }

    /// A look at the listing while a Space change's notice waits ([`Flip`]). The notice
    /// comes before the flip: none yet is waited for ([`FLIP_WAIT`] from the notice's
    /// handling), each wait's end noted.
    pub(super) fn flip_step(&mut self, listing: &Listing, now: Instant) -> Flip {
        let differs = self.desk_differences(listing);
        let waited = self.phase.since_change(now).unwrap_or_default();
        let held = self.phase.hold() == Some(Hold::Flip);
        if !differs.is_empty() {
            if held {
                self.desks.flips_waited += 1;
                margin::wait("FLIP_WAIT", waited, FLIP_WAIT, margin::End::Met);
                tracing::debug!(waited_ms = ms(waited), "desktops: the listing flipped after the Space change's notice");
            }
            return Flip::Differs(differs);
        }
        if waited < FLIP_WAIT {
            return Flip::Wait;
        }
        self.desks.flips_capped += 1;
        margin::wait("FLIP_WAIT", waited, FLIP_WAIT, margin::End::Cap);
        Flip::Unchanged
    }

    /// True while the fast tick looks at the listing for a flip.
    pub(super) fn flip_awaited(&self) -> bool {
        self.phase.hold() == Some(Hold::Flip)
    }

    /// The fast tick's look for the flip a notice waits for.
    pub(super) fn flip_fast_tick(&mut self, ob: &mut Observer) {
        if self.flip_awaited() && !self.locked {
            self.settle_space(ob);
        }
    }

    /// A Space change's notice heard at `heard` after a look took the change already
    /// ([`Self::sync_desktops`]): when the listing shows nothing more to take, or that change
    /// still waits to be recognized, the notice is that change's and does nothing more.
    pub(super) fn announced_late(&mut self, heard: Instant) -> bool {
        if !std::mem::take(&mut self.desks.taken_unannounced) {
            return false;
        }
        let Some(listing) = self.keyed_listing() else { return false };
        if !self.phase.changing() && !self.desk_differences(&listing).is_empty() {
            return false;
        }
        self.server.workspace_heard(heard);
        self.desks.announced_late += 1;
        tracing::info!("Space change notified after a look had taken it from the window server's listing: nothing more to do");
        true
    }

    /// The look for a Space change no notice told yet (the tick's check of the Spaces, the
    /// window server's own notice of a change's end): the main screen showing another Space
    /// than the model has is its Space change, begun now (true: the look ends here); another
    /// screen showing another Desktop arrives there. `list`: the window list read for the look.
    pub(super) fn sync_desktops(&mut self, ob: &mut Observer, list: &[windows::CgWindow], listing: &Listing, now: Instant) -> bool {
        self.desks.taken_unannounced = false;
        let differs = self.desk_differences(listing);
        if differs.is_empty() {
            return false;
        }
        if differs.contains(&self.main_display_id()) {
            self.desks.unannounced += 1;
            tracing::info!(screens = ?differs.iter().map(|d| d.0).collect::<Vec<_>>(), "desktops: the window server's listing shows another Space than the model, with no notice yet: the change is taken now");
            self.on_space_changed(ob, now);
            self.desks.taken_unannounced = true;
            return true;
        }
        let awaited = self.awaited_elsewhere(now);
        self.arrive_others_for(ob, list, awaited, Some(&differs));
        false
    }

    /// Of `live` (the windows on a screen the model knows or waits to place), those the
    /// window server has on Desktop `desk` (one read of its Spaces each): the others are on
    /// another Desktop still listed, and are left out. A window the engine holds hidden
    /// counts on the Desktop it was hidden from, unless lent (then it is where it is
    /// listed). With them, which are on two Desktops or more and which are lent.
    pub(super) fn keyed_live(&mut self, live: Vec<WindowMeta>, desk: &Desk, listing: &Listing) -> (Vec<WindowMeta>, Listed) {
        let mut listed = Listed::default();
        let before = live.len();
        let engine = self.engine.clone();
        let live: Vec<WindowMeta> = live
            .into_iter()
            .filter(|meta| {
                let lent = engine.lent(meta.id);
                if lent {
                    listed.lent.push(meta.id);
                } else if let Some(home) = engine.home_of(meta.id) {
                    return home == desk.id;
                }
                let on: Vec<u64> = engine.spaces_of(meta.id).unwrap_or_default().into_iter().filter(|s| listing.1.contains(s)).collect();
                if on.len() >= 2 && meta.role == WindowRole::Standard {
                    listed.every.push(meta.id);
                }
                on.contains(&desk.id)
            })
            .collect();
        listed.lent.retain(|id| live.iter().any(|m| m.id == *id));
        listed.every.retain(|id| live.iter().any(|m| m.id == *id));
        self.desks.arrivals += 1;
        self.desks.left_out += (before - live.len()) as u64;
        if before != live.len() {
            tracing::debug!(desk = desk.id, listed = before, on_it = live.len(), "desktops: windows listed on screen that the window server has on another Desktop are left out of the arrival");
        }
        (live, listed)
    }

    /// Whether a window just found, `id`, on screen on `display`, is on the Desktop the
    /// model shows there: `Some(false)` when the window server has it on another Desktop (one
    /// sliding in or out) or shows another Desktop than the model there (its arrival places
    /// the window); `None` when the listing does not decide.
    pub(super) fn found_on_desk_shown(&mut self, id: u32, display: DisplayId) -> Option<bool> {
        let listing = self.keyed_listing()?;
        let current = listing.desk(display)?.id;
        let on = self.model.as_ref()?.shown_desk(display) == Some(current) && self.engine.spaces_of(WindowId(id))?.contains(&current);
        if !on {
            self.desks.found_elsewhere += 1;
        }
        Some(on)
    }

    /// True when window `focused`, which the model does not know, is on a Desktop no screen
    /// shows in the model: the activation that focused it belongs to that Desktop (a Desktop
    /// never visited sliding in: its windows are known only from its first arrival).
    pub(super) fn focus_on_another_desk(&self, focused: WindowId) -> bool {
        let (Some(listing), Some(m)) = (self.keyed_listing(), self.model.as_ref()) else { return false };
        if m.window(focused).is_some() {
            return false;
        }
        let on: Vec<u64> = self.engine.spaces_of(focused).unwrap_or_default().into_iter().filter(|s| listing.1.contains(s)).collect();
        !on.is_empty() && !m.displays().iter().any(|d| m.shown_desk(d.id).is_some_and(|shown| on.contains(&shown)))
    }

    /// The tick's look for Desktops macOS ended (design r3, section 4): a Desktop bound to a
    /// Space that no screen lists any more, for [`spill::ENDED_AFTER`] with the display list
    /// still (one look mid-change may list a screen's Desktops only in part), ended; its Space
    /// joins the Space of the Desktop its shown windows are on now ([`spill::landed_on`]),
    /// else of the Desktop its screen shows ([`Model::desk_ended`]). The engine's copies of
    /// its hidden windows' Desktop follow at the tick's check (`SpacesEngine::rehome`).
    /// `raw`: every screen's entry, as the window server lists them now.
    pub(super) fn ended_desktops(&mut self, ob: &mut Observer, listing: &Listing, now: Instant) {
        let Some(m) = self.model.as_ref() else { return };
        let Some(raw) = self.engine.desktops() else { return };
        let displays: Vec<(String, Vec<u64>)> = raw.iter().map(|d| (d.screen.clone(), d.every().collect())).collect();
        let bound: Vec<u64> = m.desks().keys().copied().collect();
        let (ended, unlisted) = spill::ended_desktops(bound, &displays, &self.desks.unlisted, now);
        self.desks.unlisted = unlisted;
        let shown: Vec<u64> = raw.iter().map(|d| d.current).filter(|c| *c != 0).collect();
        for from in ended {
            let Some(m) = self.model.as_ref() else { return };
            let Some(space) = m.space_of_desk(from) else { continue };
            // Where its shown windows are now (and its hidden ones, under an engine that
            // keeps them on their Desktop); else what its screen shows.
            let on_desktops = m.hidden_on_their_desktop();
            let peers: Vec<Vec<u64>> = m
                .windows()
                .filter(|w| w.space == space && (!w.parked || on_desktops) && w.meta.role == WindowRole::Standard)
                .filter_map(|w| self.engine.spaces_of(w.meta.id))
                .collect();
            let screen = m.spaces().iter().find(|s| s.id == space).map(|s| s.display);
            let into = spill::landed_on(&peers, &listing.1, &shown).or_else(|| screen.and_then(|d| listing.desk(d)).map(|d| d.id));
            let Some(into) = into.filter(|d| m.space_of_desk(*d).is_some()) else {
                tracing::debug!(desk = from, space = space.0, "desktops: a Desktop ended; where its windows are is bound to no Space yet (its arrival binds it)");
                continue;
            };
            let Some(m) = self.model.as_mut() else { return };
            let actions = m.desk_ended(from, into);
            if actions.is_empty() {
                continue;
            }
            self.desks.ended += 1;
            self.pressed_obs = None;
            tracing::info!(from, into, space = space.0, "desktops: a Desktop ended (listed no more); its stages join the Desktop its windows are on");
            self.execute(ob, actions);
            self.present();
            self.mark_dirty();
        }
    }

    /// The tick's look at where the windows are, by the window server's word (module docs):
    /// each standard window of a Desktop shown that is not on screen, and each of another
    /// Desktop that is, is asked for its Desktop once. Nothing while a button is down (a
    /// drag to another Desktop goes on), for a window written to a moment ago, or for one the
    /// model counts hidden that was not lent (the engine's own check hides it again).
    pub(super) fn desk_membership(&mut self, ob: &mut Observer, list: &[windows::CgWindow], listing: &Listing, now: Instant) {
        if input::left_button_down() {
            return;
        }
        let Some(m) = self.model.as_ref() else { return };
        let shown: HashSet<u32> = self.present_in(list);
        let mut found: Vec<(u32, u64, u64)> = Vec::new();
        for w in m.windows().filter(|w| w.meta.role == WindowRole::Standard && !w.sticky) {
            let id = w.meta.id.0;
            let Some(own) = m.desk_of_space(w.space) else { continue };
            // In place as far as a look can tell: shown with its Desktop, or away with it.
            if m.is_shown_space(w.space) == shown.contains(&id) {
                continue;
            }
            // Hidden off every Desktop (a stage Space): the engine's own check hides it again
            // wherever it is listed. A hidden window that stays on its Desktop (the park
            // corner) is asked as a shown one is: moved by hand, it is on another Desktop.
            let off_desktops = w.parked && !m.hidden_on_their_desktop() && !self.engine.lent(w.meta.id);
            // A hidden window of a Desktop listed no more waits for that Desktop's end, where
            // its stage joins another whole ([`Self::ended_desktops`]), as one in a stage
            // Space does.
            let ending = w.parked && !listing.1.contains(&own);
            if off_desktops || ending || self.causes.wrote_recently(id, now) || ob.noted_closed(id) {
                continue;
            }
            let Some(spaces) = self.engine.spaces_of(w.meta.id) else { return };
            let on: Vec<u64> = spaces.into_iter().filter(|s| listing.1.contains(s)).collect();
            if let [desk] = on[..]
                && desk != own
            {
                found.push((id, own, desk));
            }
        }
        if found.is_empty() {
            return;
        }
        // What a card press read no longer holds.
        self.pressed_obs = None;
        let hints = launch_hints(list);
        for (id, from, to) in found {
            let Some(m) = self.model.as_mut() else { return };
            let group = m.group_of(WindowId(id));
            let kept = self.parked_entries_of(&group);
            let Some(m) = self.model.as_mut() else { return };
            let (actions, forgotten) = m.window_on_desk(WindowId(id), to, &hints);
            if forgotten.is_empty() {
                self.desks.moved += 1;
                tracing::info!(id, from, to, "window moved to another Desktop that shows (the window server has it there): it joins that Desktop's stages");
                self.execute(ob, actions);
            } else {
                self.desks.left += 1;
                tracing::info!(id, from, to, "window left this Space (the window server has it on another Desktop): it left its stage; placed when that Desktop shows");
                self.execute(ob, actions);
                self.let_go_hidden(ob, kept, LetGo::Other);
                self.unplaced.extend(forgotten.into_iter().map(|w| w.0).filter(|w| ob.window(*w).is_some()));
            }
        }
    }

    /// The listing took the Space change with no arrival on the main screen: the phase is
    /// steady again and nothing settles (`why` for the log).
    pub(super) fn settled_by_listing(&mut self, why: &'static str, now: Instant) {
        self.phase.feed(phase::Event::ArrivedRead, now);
        tracing::info!(why, "Space change settled by the window server's listing with no arrival on the main screen");
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
        let k = &self.desks;
        json!({
            "reads": desk_reads::status(),
            "listing": listing,
            "bindings": bindings,
            "decides": self.keyed_listing().is_some(),
            "keyed": {
                "arrivals": k.arrivals,
                "flips_waited": k.flips_waited,
                "flips_capped": k.flips_capped,
                "unannounced": k.unannounced,
                "announced_late": k.announced_late,
                "left_out": k.left_out,
                "moved": k.moved,
                "left": k.left,
                "found_elsewhere": k.found_elsewhere,
                "ended": k.ended,
                "flip_wait_ms": ms(FLIP_WAIT),
            },
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
    fn a_window_counts_for_the_screen_it_lies_wholly_on() {
        let main = Rect::new(0.0, 0.0, 1440.0, 900.0);
        assert!(wholly_on(Rect::new(200.0, 100.0, 700.0, 450.0), main));
        assert!(wholly_on(main, main), "a window filling the screen");
        // Parked in the bottom-right corner, a sliver on the screen: not counted.
        assert!(!wholly_on(Rect::new(1436.0, 836.0, 700.0, 450.0), main));
        // Reaching past an edge, towards a screen beyond or not.
        assert!(!wholly_on(Rect::new(1000.0, 100.0, 700.0, 450.0), main));
        assert!(!wholly_on(Rect::new(200.0, 600.0, 700.0, 450.0), main));
        assert!(!wholly_on(Rect::new(0.0, 0.0, 0.0, 0.0), main));
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

    /// On the bench (the real controller on the fake desk, each engine): Desktop keys
    /// between two Desktops (two stages on the first, one hidden) bind each Desktop to its own
    /// Space and the shadow finds nothing, at every load; the reads' check looked and agreed.
    #[test]
    fn desktop_keys_bind_each_desktop_and_nothing_disagrees() {
        use super::super::bench::{Bench, DEFAULTS, frame};
        use crate::engine::recovery::EngineKind;
        use crate::platform::fake::FIRST_DESKTOP as D1;
        let d2 = D1 + 1;
        for (engine, load) in [EngineKind::Spaces, EngineKind::Corner].into_iter().flat_map(|e| [1.0, 3.0, 10.0].map(|l| (e, l))) {
            let mut b = Bench::new(engine, DEFAULTS, load, 2, |d| {
                d.add_app(101, "com.example.alpha", "Alpha");
                d.add_app(102, "com.example.bravo", "Bravo");
                d.add_window(11, 101, frame(1), D1);
                d.add_window(12, 102, frame(2), D1);
                d.add_window(13, 101, frame(3), d2);
                d.front = Some(102);
            });
            for to in [d2, D1, d2, D1] {
                b.go_to_desktop(to, true);
                b.advance(4000);
            }
            let desks = b.c.desks_json();
            let bound: Vec<(u64, u64)> = desks["bindings"].as_array().expect("bindings").iter().map(|x| (x["desk"].as_u64().unwrap_or(0), x["space"].as_u64().unwrap_or(0))).collect();
            let spaces: Vec<u64> = [11, 13].iter().filter_map(|w| b.space_of(*w)).map(|s| s.0).collect();
            assert_eq!(bound, vec![(D1, spaces[0]), (d2, spaces[1])], "load {load}: each Desktop its own Space\n{}", b.story());
            assert!(!b.hidden().is_empty(), "load {load}: a stage hidden, its home compared");
            let g = &desks["disagree"];
            assert_eq!((g["shown_begun"].as_u64(), g["homes_begun"].as_u64()), (Some(0), Some(0)), "load {load}: {desks}\n{}", b.story());
            let r = &desks["reads"];
            assert_eq!((r["way"].as_str(), r["state"].as_str(), r["checks"]["disagreed"].as_u64()), (Some("window server"), Some("working"), Some(0)), "{engine:?} at load {load}: {r}");
            assert!(r["checks"]["agreed"].as_u64().unwrap_or(0) >= 2, "{engine:?} at load {load}: the check looked: {r}");
        }
    }
}
