//! The wallpaper corner patch as the WM thread plans it ([`crate::engine::corner::corner_patch`],
//! `[windows] corner_patch`): one patch a screen over the slivers of its parked windows,
//! planned after every batch, on the tick, and on the fast tick after a focus or through
//! Show Desktop; the plan goes to the main thread, which draws it
//! ([`crate::ui::corner_patch`]).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::super::*;
use super::cover::cover_desktop_forced;
use super::shield::shield_parked;

/// How long after a focus or an activation the corner patch is planned on the fast tick
/// ([`Controller::replan_patch`], the review's L2).
pub(in crate::platform::controller) const PATCH_REPLAN: Duration = Duration::from_millis(500);

/// One screen's corner patch as the WM thread plans it (round displays-2: a patch per
/// screen, each with its own rectangle, order, Show Desktop state and plan).
#[derive(Default)]
pub(in crate::platform::controller) struct PatchTrack {
    /// The last plan sent to the main thread.
    pub(in crate::platform::controller) sent: Option<crate::ui::corner_patch::PatchPlan>,
    /// The rectangle in use ([`crate::engine::corner::corner_patch::steady_rect`]).
    pub(in crate::platform::controller) rect: Option<Rect>,
    /// The rectangle in use on each of the screen's Desktops (the engine's Space ids): each
    /// Desktop has a panel of its own ([`crate::ui::corner_patch`]), steady on its own.
    pub(in crate::platform::controller) rects: HashMap<u64, Rect>,
    /// The order as last checked ([`crate::engine::corner::corner_patch::order_step`]).
    pub(in crate::platform::controller) order: crate::engine::corner::corner_patch::OrderState,
    /// Why the last plan hid the patch (logged once a change).
    pub(in crate::platform::controller) why: Option<&'static str>,
    /// The patch was once found ordered right (logged once).
    pub(in crate::platform::controller) honoured: bool,
    /// The Desktop's windows while Show Desktop is listed: their slide back is its close's
    /// first sign ([`show_desktop::CloseWatch`]).
    pub(in crate::platform::controller) reveal_close: CloseWatch,
    /// Where macOS listed the panel at the last plan during Show Desktop (logged when it
    /// changes; `None` outside Show Desktop).
    pub(in crate::platform::controller) reveal_panel: Option<PanelPlace>,
    /// The rectangle through Show Desktop: grown over the parked windows macOS pushes into
    /// view, until they are back ([`show_desktop::PushedCover`]).
    pub(in crate::platform::controller) reveal_cover: PushedCover,
    /// Since when the list shows this screen's Desktop sliding (the main screen's slide is
    /// the phase's clock, [`phase::Tracker::slide_seen`]; another screen's is kept here).
    pub(in crate::platform::controller) slide_since: Option<Instant>,
}

/// The windows of `parked` (the model's parked windows) that window server list `list` shows
/// hidden (`hidden`: the engine's test of a listed frame on the display,
/// [`WindowEngine::is_hidden`]): parked in fact. One the model has parked but the window server lists elsewhere (a park that did not
/// hold, an app that put its window back) shows there: never a sliver of the corner patch nor
/// of a switch's cover (round 8L6: on the owner's Mac one listed at 533,184 1741x1173 grew both
/// to 473,124 2087x1316 for minutes), and a window that stays shown through a switch.
pub(in crate::platform::controller) fn parked_in_zone(list: &[windows::CgWindow], parked: impl IntoIterator<Item = u32>, hidden: impl Fn(Rect) -> bool) -> HashSet<u32> {
    let parked: HashSet<u32> = parked.into_iter().collect();
    let (inside, elsewhere): (Vec<_>, Vec<_>) = list
        .iter()
        .filter(|w| parked.contains(&w.id))
        .partition(|w| hidden(w.bounds));
    note_parked_elsewhere(&elsewhere);
    inside.iter().map(|w| w.id).collect()
}

/// Logs once per window and listed frame a window the model has parked that the window server
/// lists outside the park zone (8L6 review: the owner's 10-16 Fork window, listed at its frame
/// for minutes while parked; the next log settles whether the park held).
pub(in crate::platform::controller) fn note_parked_elsewhere(elsewhere: &[&windows::CgWindow]) {
    static SEEN: Mutex<Vec<(u32, i64, i64)>> = Mutex::new(Vec::new());
    let Ok(mut seen) = SEEN.lock() else { return };
    for w in elsewhere {
        let key = (w.id, w.bounds.x.round() as i64, w.bounds.y.round() as i64);
        if seen.contains(&key) {
            continue;
        }
        if seen.len() >= 256 {
            seen.clear();
        }
        seen.push(key);
        tracing::debug!(id = w.id, pid = w.pid, bounds = %format!("{:.0},{:.0} {:.0}x{:.0}", w.bounds.x, w.bounds.y, w.bounds.w, w.bounds.h), onscreen = w.onscreen,
            "a window the model has parked is listed outside the park zone");
    }
}

/// What the corner patch may cover while Show Desktop pushes the parked windows into view
/// ([`show_desktop::pushed_rect`]): the park corner's square a switch's cover keeps to
/// ([`crate::engine::corner::corner_patch::cover_cap`], the still's block at the bottom-right corner),
/// within the block of the desktop's still kept (`still`, when one is), which the patch's
/// picture is cropped from: no still is taken while an overview shows.
pub(in crate::platform::controller) fn reveal_cap(display: Rect, corner: Corner, still: Option<Rect>) -> Rect {
    let cap = crate::engine::corner::corner_patch::cover_cap(display, corner);
    still.and_then(|s| s.intersect(&cap)).unwrap_or(cap)
}

/// The parked windows (`parked`: id, Space, that Space's screen) of the other Desktops of
/// `screen`, which shows `here`: one of them listed on screen means that screen's Desktop
/// slides. Another screen's parked windows are no such sign: their slivers show in its own
/// corner all the time, and counted here they read as a slide that never ends (no still of
/// the desktop was taken while both screens had a stage parked).
pub(in crate::platform::controller) fn parked_on_other_desktops(parked: impl IntoIterator<Item = (u32, SpaceId, DisplayId)>, here: SpaceId, screen: DisplayId) -> HashSet<u32> {
    parked.into_iter().filter(|(_, space, on)| *on == screen && *space != here).map(|(id, _, _)| id).collect()
}

/// For how long another screen's Desktop has been read sliding (`None`: it does not), its
/// clock `since` kept as the main screen's is ([`crate::engine::phase::Tracker::slide_seen`]):
/// started at the first plan that reads the slide, cleared by one that does not.
pub(in crate::platform::controller) fn slide_clock(since: &mut Option<Instant>, slides: bool, now: Instant) -> Option<Duration> {
    *since = if slides { Some(since.unwrap_or(now)) } else { None };
    since.map(|t| now.saturating_duration_since(t))
}

impl Controller {
    /// Finder's desktop icons for the patch at `rect` as known (`None`: not known, the patch
    /// stays off). Takes a finished read's answer, and starts a read (on its own thread,
    /// [`crate::platform::desktop_items::start_read`]) for a new rectangle or when one is due: the last
    /// good read [`PATCH_ITEMS_FRESH`] old, a failed one [`items_retry`] old. A read
    /// that fails keeps the icons known. None starts while `quiet` (a drag, the fast tick).
    pub(in crate::platform::controller) fn patch_items(&mut self, screen: u32, rect: Option<Rect>, now: Instant, quiet: bool) -> Option<Vec<Rect>> {
        let answer = match self.patch_items.reading.as_ref().map(|r| r.try_recv()) {
            Some(Ok(answer)) => Some(answer),
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => Some(None),
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => None,
        };
        if let Some(answer) = answer {
            self.take_items_answer(answer, now);
        }
        let rect = rect?;
        let p = &mut self.patch_items;
        let due = p.read_for.get(&screen) != Some(&rect) || p.due.is_none_or(|d| now >= d);
        if due && p.reading.is_none() && !quiet {
            p.read_for.insert(screen, rect);
            p.due = Some(now + PATCH_ITEMS_FRESH);
            p.reading = crate::platform::desktop_items::start_read();
        }
        if !self.settings.hide_widgets_standard && !self.corner.patch_widgets_logged {
            self.corner.patch_widgets_logged = true;
            tracing::info!(
                "corner patch: widgets may show on the desktop (StandardHideWidgets off); their places are not public, so the file's picture would cover one in the corner (a still of the desktop shows it)"
            );
        }
        self.patch_items.known.clone()
    }

    /// Works out the wallpaper corner patch's plan for every screen ([`crate::engine::corner::corner_patch`];
    /// one patch per screen, round displays-2) and sends each to the main thread when it
    /// changed. Reads the window server's on-screen list only while `[windows] corner_patch`
    /// is on, once for every screen; checks each patch's order in it every time (an
    /// activation raising a parked window undoes it), and has it ordered again when it was
    /// right and is not any more. Returns the list when it read one, for the rest of the tick
    /// ([`crate::platform::corner::sliver_tap::refresh`]).
    pub(in crate::platform::controller) fn update_patch(&mut self) -> Option<Rc<Vec<windows::CgWindow>>> {
        use crate::ui::corner_patch::PatchPlan;
        let config = self.store.config();
        let (on, reach, space) = (config.windows.corner_patch, config.windows.patch_reach(), self.space_changes);
        // Mission Control and App Exposé: the panel is Stationary (round show-desktop-patch;
        // Transient before, which macOS hid while one showed), so it stays drawn in place like
        // the desktop picture under their windows, and is there the moment it closes, with the
        // windows. A patch shown when one opened is left as it is (no plan, no reorder during
        // it); hiding it from the plan left the slivers bare until the close was seen (up to a
        // second's poll) and settled (10-13, F3 twice: the corner bare until a window was
        // focused).
        // A Desktop picked inside Mission Control plans as before (hidden until it is
        // recognized): the old Desktop's patch must not come back on the new one.
        // Only Mission Control and App Exposé: Show Desktop pushes every window to the screen's
        // edges, the patch then over a pushed window's edge (10-13: a lighter block in the
        // corner each time), so it is planned through it ([`show_desktop::gate`]).
        let now = Instant::now();
        // While Show Desktop is up the window list read for the plan is looked at for it too:
        // its close is seen at this plan (on the fast tick, within 20 ms; round
        // show-desktop-patch), not at the next once-a-second check.
        let mut early = None;
        if on
            && !self.phase.changing()
            && self.overview.show_desktop_up(now)
            && let Some(display) = self.model.as_ref().map(|m| m.main_display().frame)
        {
            let list = windows::wake_on_screen();
            self.look_for_overview(&list, display, now);
            early = Some(list);
        }
        let any_shown = self.corner.patches.values().any(|t| t.sent.as_ref().is_some_and(|p| p.show.is_some()));
        if on && !self.phase.changing() && self.overview.showing() == Some(Overview::MissionControl) && any_shown {
            return early;
        }
        // A Space change being recognized: no plan. Each Desktop has its own panel, which
        // slides in with its Desktop as it was left; the main thread leaves it so until the
        // first plan for that Desktop (its Space-change count is newer), so nothing meant
        // for the Desktop left reaches it.
        if on && self.phase.changing() && self.model.is_some() {
            return None;
        }
        let displays: Vec<Display> = if on { self.model.as_ref().map(|m| m.displays().to_vec()).unwrap_or_default() } else { Vec::new() };
        if displays.is_empty() {
            // Off, or no model: every screen's patch off (its icons known stay: they do not
            // depend on the patch; a read is due when the patch is wanted again).
            self.phase.forget_slide();
            self.patch_items.read_for.clear();
            let screens: Vec<u32> = self.corner.patches.keys().copied().collect();
            for screen in screens {
                let reorder = self.corner.patches.get(&screen).and_then(|t| t.sent.as_ref()).map_or(0, |p| p.reorder);
                let track = self.corner.patches.entry(screen).or_default();
                *track = PatchTrack { sent: track.sent.take(), ..PatchTrack::default() };
                let plan = PatchPlan { screen, reorder, space, ..PatchPlan::default() };
                self.send_patch(plan);
            }
            return None;
        }
        let list = early.take().unwrap_or_else(windows::wake_on_screen);
        // The screens gone are forgotten (their patches close on the main thread with the
        // display change).
        self.corner.patches.retain(|screen, _| displays.iter().any(|d| d.id.0 == *screen));
        for d in displays {
            if let Some(plan) = self.plan_patch_on(d, &list, now, reach, space) {
                self.send_patch(plan);
            }
        }
        Some(list)
    }

    /// Sends `plan` to the main thread when it differs from the last one sent for its screen
    /// (a first plan that shows nothing is not sent).
    pub(in crate::platform::controller) fn send_patch(&mut self, plan: crate::ui::corner_patch::PatchPlan) {
        let track = self.corner.patches.entry(plan.screen).or_default();
        if track.sent.as_ref() == Some(&plan) || (track.sent.is_none() && plan.show.is_none() && plan.picture.is_none()) {
            return;
        }
        track.sent = Some(plan.clone());
        notify_ui(UiUpdate::CornerPatch(plan));
    }

    /// The corner patch's plan for one screen `d` from the window server's list `list`
    /// ([`Self::update_patch`]); `None` while that screen's Desktop slides (the plan stays as
    /// it was).
    pub(in crate::platform::controller) fn plan_patch_on(&mut self, d: Display, list: &[windows::CgWindow], now: Instant, reach: crate::engine::corner::corner_patch::Reach, space: u64) -> Option<crate::ui::corner_patch::PatchPlan> {
        use crate::engine::corner::corner_patch::{
            Listed, PatchFacts, desktop_slides, order_step, ordered_right, patch_rect, slide_waits, steady_rect, visibility, would_be_right,
        };
        use crate::ui::corner_patch::{self as patch_ui, PatchPlan};
        let screen = d.id.0;
        let m = self.model.as_ref()?;
        let is_main = d.id == m.main_display().id;
        let reorder = self.corner.patches.get(&screen).and_then(|t| t.sent.as_ref()).map_or(0, |p| p.reorder);
        let mut reorder = reorder;
        let own = std::process::id() as i32;
        let display = d.frame;
        // Only those listed in the park zone (8L6): one the model has parked but the
        // window server lists elsewhere shows there, and grew the patch's rectangle
        // (and every switch's cover) to 2087x1316 pt on the owner's Mac (10-16).
        // This Desktop's parked windows only: another Desktop's listed on screen means
        // it slides in (or out) before the Space change's notice.
        let here = m.current_space(d.id);
        // With the windows whose restore failed while they are still listed in the
        // zone (W4): their slivers are there, whatever the model says.
        let model_parked: HashSet<u32> = shield_parked(
            m.windows().filter(|w| w.parked && w.space == here).map(|w| w.meta.id.0),
            self.corner.unrestored.iter().map(|(id, u)| (*id, u.in_zone)),
        );
        let parked_elsewhere = parked_on_other_desktops(m.windows().filter(|w| w.parked).map(|w| (w.meta.id.0, w.space, m.display_of_space(w.space))), here, d.id);
        let parked = parked_in_zone(list, model_parked.iter().copied(), |b| self.engine.is_hidden(&d, b));
        // Show Desktop pushes the parked windows partly into view (round
        // show-desktop-patch, part 2): the patch covers what of them shows, until they
        // are back at their park frames.
        let show_desktop = self.overview.show_desktop_up(now);
        let parked_listed: Vec<(u32, Rect)> =
            list.iter().filter(|w| w.layer == 0 && model_parked.contains(&w.id)).map(|w| (w.id, w.bounds)).collect();
        let track = self.corner.patches.entry(screen).or_default();
        let cover = track.reveal_cover.see(&CoverLook {
            up: show_desktop,
            parked: &parked_listed,
            in_zone: &parked,
            in_use: track.rects.get(&here.0).copied(),
            display,
            reach,
            cap: reveal_cap(display, d.corner, desktop_shot::kept_region(screen)),
            now,
        });
        // This Desktop's windows as listed while Show Desktop is: their slide back is its
        // close's first sign (round show-desktop-patch).
        let listed_sd = self.overview.showing() == Some(Overview::ShowDesktop);
        let ours: Vec<(u32, Rect)> = if listed_sd {
            list.iter()
                .filter(|w| w.layer == 0 && m.window(WindowId(w.id)).is_some_and(|x| x.space == here))
                .map(|w| (w.id, w.bounds))
                .collect()
        } else {
            Vec::new()
        };
        // A Desktop slides before the Space change's notice (10-23: the parked windows
        // listed at x=110, all together; at 11:51 the next Desktop's listed in the zone
        // and planned as this one's): the plan stays as it was, so the panel slides away
        // with its Desktop drawn and is there when it slides back.
        let listed_parked: Vec<Rect> = list.iter().filter(|w| w.layer == 0 && model_parked.contains(&w.id)).map(|w| w.bounds).collect();
        let elsewhere = list.iter().any(|w| w.layer == 0 && w.onscreen && parked_elsewhere.contains(&w.id));
        // Never while Show Desktop is up or its cover held: it moves the parked windows
        // out of the zone too.
        let slides = !show_desktop
            && !track.reveal_cover.holding()
            && (elsewhere || parked.is_empty())
            && desktop_slides(model_parked.len(), &listed_parked, elsewhere, d.frame, d.visible, d.corner);
        // The main screen's slide is the phase's clock (other holds read it); another
        // screen's is the track's.
        let sliding_for = if is_main {
            self.phase.slide_seen(slides, now);
            self.phase.sliding_for(now)
        } else {
            slide_clock(&mut self.corner.patches.entry(screen).or_default().slide_since, slides, now)
        };
        let track = self.corner.patches.entry(screen).or_default();
        let shown_now = track.sent.as_ref().is_some_and(|p| p.show.is_some());
        if slide_waits(sliding_for, shown_now) {
            if track.why != Some("the Desktop slides") {
                tracing::debug!(screen, "corner patch: the Desktop slides; the plan stays as it was");
                track.why = Some("the Desktop slides");
            }
            return None;
        }
        // While the cover is held every parked window listed counts as parked, wherever
        // Show Desktop put it: the patch is ordered just above them and never taken for
        // over a shown window there.
        let parked: HashSet<u32> = match cover {
            Cover::Held(_) => parked.into_iter().chain(parked_listed.iter().map(|(id, _)| *id)).collect(),
            _ => parked,
        };
        let slivers: Vec<Rect> = list.iter().filter(|w| w.layer == 0 && parked.contains(&w.id)).map(|w| w.bounds).collect();
        let usual = patch_rect(&slivers, display, reach);
        let rect = match cover {
            Cover::Held(r) => Some(r),
            // The windows are back: the slivers' rectangle at once, not kept at the
            // bigger one in use.
            Cover::Released => usual,
            Cover::Usual => usual.map(|r| steady_rect(track.rects.get(&here.0).copied(), r, reach)),
        };
        match cover {
            Cover::Held(r) if track.rect != Some(r) => {
                tracing::info!(screen, rect = %r, "Show Desktop: the corner patch covers the parked windows it pushes into view");
            }
            Cover::Released => {
                tracing::info!(screen, rect = %rect.map_or_else(|| "none".to_owned(), |r| r.to_string()), "Show Desktop: the parked windows are back; the corner patch covers their slivers");
            }
            _ => {}
        }
        match rect {
            Some(r) => track.rects.insert(here.0, r),
            None => track.rects.remove(&here.0),
        };
        track.rect = rect;
        let listed: Vec<Listed> =
            list.iter().map(|w| Listed { id: w.id, layer: w.layer, bounds: w.bounds, drawn: w.alpha > 0.0 && w.pid != own }).collect();
        // The topmost parked window under the patch: what it is ordered against.
        let above = rect.and_then(|r| {
            list.iter().find(|w| w.layer == 0 && parked.contains(&w.id) && w.bounds.intersect(&r).is_some()).map(|w| w.id)
        });
        // Whether just above it could be right at all (no shown window under it that
        // reaches the patch); when not, the fallback, without a show to find out.
        let number = patch_ui::window_number(screen);
        let possible = match (rect, above) {
            (Some(r), Some(a)) => would_be_right(&listed, number.unwrap_or(0), a, r, &parked),
            _ => false,
        };
        // Checked only while the panel is on screen ordered against that window with
        // this reorder count, and that order could be right; otherwise not known.
        let check = match (number, rect, above) {
            (Some(n), Some(r), Some(a))
                if possible && patch_ui::ordered_against(screen) == Some((a, reorder)) && list.iter().any(|w| w.id == n) =>
            {
                Some(ordered_right(&listed, n, r, &parked))
            }
            _ => None,
        };
        let was = track.order;
        let step = order_step(&mut track.order, above, check);
        if step.reorder {
            reorder = reorder.wrapping_add(1);
            tracing::debug!(screen, "corner patch: no longer just above the parked windows; ordered again");
        } else if check == Some(true) && !track.honoured {
            track.honoured = true;
            tracing::info!(screen, "corner patch: ordered just above the parked windows (macOS honours the order across apps)");
        } else if track.order.refused.is_some() && track.order.refused != was.refused {
            tracing::info!(
                screen,
                refused_against = track.order.refused,
                "corner patch: macOS did not keep the patch just above the parked window; it shows only where no window reaches (the fallback mode; `ctl state` names it)"
            );
        } else if check.is_some() && check != was.last {
            tracing::debug!(screen, ?check, "corner patch: order checked");
        }
        let ordered = step.ordered;
        let dragging = input::left_button_down() || self.drag.is_some();
        let quiet = dragging || self.wants_fast_tick();
        let items = self.patch_items(screen, rect, now, quiet);
        // The desktop's still: the patch's picture wherever one can be taken (round
        // desktop-still-patch), and a switch's cover where a desktop icon lies (8L3).
        let corner = self.corner_still(&d, rect, items.as_deref(), now);
        // Show Desktop (round show-desktop-patch): where macOS lists the panel, and the
        // close's first sign, this Desktop's windows sliding back.
        let track = self.corner.patches.entry(screen).or_default();
        let closing = track.reveal_close.see(listed_sd, ours, now);
        if closing == show_desktop::Closing::Began {
            tracing::info!(screen, "Show Desktop closes: the windows slide back; the corner patch is planned again");
        }
        let panel = if show_desktop {
            let listed = number.and_then(|n| list.iter().find(|w| w.id == n)).map(|w| w.bounds);
            let asked = track.sent.as_ref().and_then(|p| p.show);
            let place = show_desktop::panel_place(listed, patch_ui::placed_at(screen), asked, display);
            if track.reveal_panel != Some(place) {
                tracing::info!(screen, listed = ?listed, placed = ?patch_ui::placed_at(screen), "Show Desktop: the corner patch's panel is {}", place.name());
                track.reveal_panel = Some(place);
            }
            place
        } else {
            track.reveal_panel = None;
            PanelPlace::Unknown
        };
        let gate = show_desktop::gate(&RevealFacts {
            mission_control: self.overview.showing() == Some(Overview::MissionControl),
            show_desktop,
            closing: closing.is(),
            still: matches!(corner.still, crate::engine::corner::patch_still::Still::Ready(_)),
            panel,
        });
        let facts = PatchFacts {
            on: true,
            rect,
            // Not the settle after an overview (that is for Space inference): the patch
            // comes back with the windows.
            overview: gate.err().or(self.phase.changing().then_some(show_desktop::OVERVIEW_UP)),
            ordered,
            can_order: possible,
            fullscreen: self.phase.fullscreen(),
            // The same filter as the order's check (`Listed::counts`, the rectangle
            // tested by `visibility`).
            shown: listed.iter().filter(|w| w.layer == 0 && w.drawn && !parked.contains(&w.id)).map(|w| w.bounds).collect(),
            dragging,
            transition: self.phase.switching().is_some(),
            items,
            still: corner.still,
        };
        let visible = visibility(&facts);
        let why = visible.err();
        if why != track.why {
            if let Some(why) = why {
                tracing::debug!(screen, why, "corner patch: hidden by the plan");
            }
            track.why = why;
        }
        let show = visible.ok();
        // The test's forced still is compared with the patch's picture (`debug-overlay`).
        let wanted = desktop_shot::needs_wallpaper(screen) || cover_desktop_forced();
        let picture = if show.is_none() && corner.picture && wanted { rect } else { None };
        let clear = rect.is_some_and(|r| crate::engine::corner::corner_patch::icons_clear(facts.items.as_deref(), r).is_ok());
        let source = crate::engine::corner::patch_still::source(facts.still, clear);
        // The icons as last read well, whatever this plan's rectangle (none read for a
        // plan with no sliver): they are the same on every Desktop, and other Desktops'
        // panels are checked against them (10-23: `None` here hid them all).
        Some(PatchPlan {
            screen,
            show,
            above: show.and(above),
            reorder,
            space,
            picture,
            on: true,
            items: self.patch_items.known.clone(),
            still: show.and(source.still),
            hold: show.is_some() && source.hold,
            file: source.file,
        })
    }

    /// Plans the corner patch again now, and on the fast tick for [`PATCH_REPLAN`] (the
    /// Review review's L2): macOS raises an activated app's windows, its parked ones too,
    /// above the patch; until the next 250 ms tick ordered it again the slivers showed on
    /// every ⌘-Tab, Dock click or focus of an app with parked windows on this Desktop.
    pub(in crate::platform::controller) fn replan_patch(&mut self, now: Instant) {
        if self.model.is_none() || !self.store.config().windows.corner_patch {
            return;
        }
        self.corner.patch_fast_until = Some(now + PATCH_REPLAN);
        self.update_patch();
    }

    /// True while the patch is planned on the fast tick after a focus or activation.
    pub(in crate::platform::controller) fn patch_replans(&self, now: Instant) -> bool {
        self.corner.patch_fast_until.is_some_and(|t| now < t)
    }

    /// The fast tick's share of the patch: a plan while [`Self::patch_replans`], and one
    /// last plan when the window is over. True when it planned.
    pub(in crate::platform::controller) fn patch_fast_step(&mut self, now: Instant) -> bool {
        let Some(until) = self.corner.patch_fast_until else { return false };
        if now >= until {
            self.corner.patch_fast_until = None;
        }
        self.update_patch();
        true
    }

    /// True while the corner patch is planned on the fast tick for Show Desktop: listed, or
    /// expected from a wallpaper click, with the patch on ([`show_desktop::fast_look`]).
    pub(in crate::platform::controller) fn reveal_fast(&self, now: Instant) -> bool {
        let patch_on = self.model.is_some() && self.store.config().windows.corner_patch;
        let holding = self.corner.patches.values().any(|t| t.reveal_cover.holding());
        show_desktop::fast_look(self.overview.show_desktop_up(now) || holding, patch_on)
    }

    /// The corner patch as it runs, for `ctl state` (the review's L3): `mode` is
    /// `ordered` (macOS keeps it just above the parked windows), `fallback` (it refused:
    /// the patch shows only where no shown window reaches), `unknown` (not checked yet) or
    /// `off`; `refused_against` the parked window macOS would not keep it above.
    pub(in crate::platform::controller) fn patch_status(&self, screen: u32) -> Value {
        let on = self.store.config().windows.corner_patch && self.model.is_some();
        let track = self.corner.patches.get(&screen);
        let order = track.map(|t| t.order).unwrap_or_default();
        let mode = if !on {
            "off"
        } else if order.refused.is_some() {
            "fallback"
        } else if order.last == Some(true) || track.is_some_and(|t| t.honoured) {
            "ordered"
        } else {
            "unknown"
        };
        json!({
            "on": on,
            "mode": mode,
            "shown": track.and_then(|t| t.sent.as_ref()).is_some_and(|p| p.show.is_some()),
            "rect": track.and_then(|t| t.rect),
            "refused_against": order.refused,
            "hidden_why": track.and_then(|t| t.why),
        })
    }

    /// The corner patch's last plan on `screen` for `ctl state`: where it shows, and from
    /// which picture (round desktop-still-patch: the still's serial, held while one comes,
    /// the file's).
    pub(in crate::platform::controller) fn patch_plan_status(&self, screen: u32) -> Value {
        json!(self.corner.patches.get(&screen).and_then(|t| t.sent.as_ref()).map(|p| json!({ "show": p.show, "still": p.still, "hold": p.hold, "file": p.file })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two screens, a stage parked on each: the other screen's parked windows, listed in its
    /// corner all the time, are no sign of this screen's Desktop sliding.
    #[test]
    fn another_screens_parked_windows_are_not_another_desktops() {
        let (main, laptop) = (DisplayId(1), DisplayId(2));
        let parked = [(10, SpaceId(1), main), (11, SpaceId(2), main), (20, SpaceId(3), laptop)];
        assert_eq!(parked_on_other_desktops(parked, SpaceId(1), main), HashSet::from([11]));
        assert_eq!(parked_on_other_desktops(parked, SpaceId(3), laptop), HashSet::new());
    }

    /// Another screen's slide clock starts at the first plan that reads the slide, not at
    /// the plan before it.
    #[test]
    fn another_screens_slide_clock_starts_with_the_slide() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut since = None;
        assert_eq!(slide_clock(&mut since, false, t0), None);
        assert_eq!(since, None, "no slide: no clock");
        assert_eq!(slide_clock(&mut since, true, at(250)), Some(Duration::ZERO));
        assert_eq!(slide_clock(&mut since, true, at(500)), Some(Duration::from_millis(250)));
        assert_eq!(slide_clock(&mut since, false, at(750)), None);
        assert_eq!(slide_clock(&mut since, true, at(1000)), Some(Duration::ZERO), "a new slide counts afresh");
    }

    #[test]
    fn show_desktops_cover_keeps_to_the_corner_square_within_the_still() {
        let display = Rect::new(0.0, 0.0, 2560.0, 1440.0);
        let square = Rect::new(2240.0, 1120.0, 320.0, 320.0);
        assert_eq!(reveal_cap(display, Corner::BottomRight, None), square, "no still: the square a switch's cover keeps to");
        let bigger = Rect::new(2000.0, 1000.0, 560.0, 440.0);
        assert_eq!(reveal_cap(display, Corner::BottomRight, Some(bigger)), square, "a bigger still: still the square");
        let smaller = Rect::new(2300.0, 1200.0, 260.0, 240.0);
        assert_eq!(reveal_cap(display, Corner::BottomRight, Some(smaller)), smaller, "only what the still holds");
        assert_eq!(reveal_cap(display, Corner::BottomRight, Some(Rect::new(0.0, 0.0, 100.0, 100.0))), square, "a still elsewhere: the square");
    }
}
