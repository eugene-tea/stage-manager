//! A fake WindowManager for the Apple mode's tests ([`FakeWindowManager`]): stages, the
//! strip's room, Apple's motion and the 650 ms blank of its Accessibility on a test clock, a
//! covered strip, the person's input, and Apple's answers as measured on 11-16: a focus
//! switches to the window's stage; a new app takes a stage of its own, or, hidden at its
//! launch, goes into the strip with the stage in front left as it is; a running app's window
//! reopened while it is hidden (at its activation) shows nowhere, listed off screen, until the
//! app is shown again without activating it, then goes into the strip, the stage in front left
//! as it is (the sixth probe of 11-16), and reopened with no hide switches as a new app's
//! does; a Shift-click on a
//! visible part of a thumbnail adds that window alone to the stage in front, every click of
//! one Shift press landing on what the strip showed at the press (the fourth probe of 11-16),
//! a press mid-motion is ignored, a click on a covered strip lands on nothing of Apple's (with
//! [`FakeWindowManager::relayout`], each click lands on the item as Apple's lays it out again
//! around the thumbnails left by the clicks before); an item draws three thumbnails, a
//! stage's fourth window and on listed off screen within it (live 11-17); a
//! focus of a window in front raises it within its stage; a drag of a title bar onto the strip
//! takes the window out to a stage of its own (covered or not), and is not posted when another
//! window is topmost at the grab point (the gestures' guard). Other Desktops: a Desktop made
//! or shown again keeps each Desktop's stages, the windows of the ones not shown listed off
//! screen; a focus of such a window is counted (macOS would show its Desktop). A second
//! screen is a second fake shifted right ([`FakeWindowManager::on_screen_at`]), its list
//! joined to the first's. Every focus and posted gesture is recorded. It implements [`AppleDesk`], so the mirror, following, the jobs
//! and the routes run against it as against the real desk.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::engine::apple::desk::{AppleDesk, InputNow, Landed, Posted};
use crate::engine::apple::mirror::{Item, Listed, ListedWindow, Reading, SETTLE};
use crate::engine::geometry::{Point, Rect};
use crate::engine::model::Display;

/// WindowManager's pid in the fake.
pub const MANAGER: i32 = 99;

/// A thumbnail's size and the strip's first place (the measured 79x104 at x = 15).
pub const THUMB: Rect = Rect::new(15.0, 100.0, 79.0, 104.0);

/// Where a hidden strip's thumbnails are listed (the measured x = -258).
const HIDDEN_X: f64 = -258.0;

/// How long Apple's Accessibility reads empty after a change (measured: about 650 ms).
const BLANK: Duration = Duration::from_millis(650);

/// The strip's items, one slot apart.
const SLOT: f64 = 150.0;

/// A fake WindowManager and window server.
#[derive(Debug, Clone)]
pub struct FakeWindowManager {
    clock: Instant,
    /// Each window's app and full-size frame.
    windows: BTreeMap<u32, (i32, Rect)>,
    /// The current stage, front to back.
    current: Vec<u32>,
    /// The other stages, in the strip's order top to bottom (those past the room unlisted).
    stages: Vec<Vec<u32>>,
    /// How many items the strip shows.
    room: usize,
    /// The strip hidden (a window over it): its thumbnails off the left edge.
    covered: bool,
    /// The last change, and the bounds every window was listed at before it (the motion's
    /// start).
    changed: Option<(Instant, BTreeMap<u32, Rect>)>,
    /// Every focus asked, in order.
    pub focused: Vec<u32>,
    /// Every gesture posted, in order.
    pub posted: Vec<Posted>,
    /// The person's input as the gate reads it (idle unless a test says otherwise).
    pub input: InputNow,
    /// Apps hidden (at their launch).
    pub hidden: BTreeSet<i32>,
    /// The person's own input ends every drag midway.
    pub interrupt_drags: bool,
    /// Posted clicks Apple's ignored because they came mid-motion.
    pub ignored_mid_motion: u32,
    /// Apple's answers no posted gesture (a macOS that changed).
    pub deaf: bool,
    /// "Show windows from an application: All at Once": a drag of a window whose app keeps
    /// another window in the stage in front only moves it (the fifth probe of 11-16). Off by
    /// default here, so the other tests can split an app's windows.
    pub all_at_once: bool,
    /// How far right each thumbnail of a stage sits from the one in front of it (16 points
    /// measured; 0 for windows at one frame, whose thumbnails stack exactly).
    pub stack: f64,
    /// How many thumbnails an item draws (three, live 11-17): a stage's windows past it are
    /// listed off screen within the item, with no twin.
    pub item_room: usize,
    /// Each click of a press lands on the item as laid out again around the thumbnails the
    /// clicks before it took (live 11-17: the third click of a press missed a thumbnail Apple's
    /// had moved); off, every click lands on what the strip showed at the press.
    pub relayout: bool,
    /// How the last gesture ended, until taken.
    landed: Option<Landed>,
    /// How far right of the main screen's origin this fake's screen is (a second screen).
    shift: f64,
    /// WindowManager's icon windows are numbered from here (apart for a second screen).
    icon_base: u32,
    /// The Desktops not shown: each one's stage in front and its other stages.
    desktops: Vec<(Vec<u32>, Vec<Vec<u32>>)>,
    /// Focuses asked of a window on a Desktop not shown (macOS would have shown it).
    pub desktop_jumps: u32,
    /// Windows a hidden app reopened: in no stage, listed off screen at their frames, until
    /// the app shows again.
    shown_nowhere: Vec<u32>,
    /// Every app shown again by an unhide, in order.
    pub unhidden: Vec<i32>,
    /// Windows the observer does not take as standard (a window reopened while its app is
    /// hidden reads `AXDialog`, live 11-16): listed as any other, never in
    /// [`FakeWindowManager::window_ids`] nor [`FakeWindowManager::pids`].
    pub loose: BTreeSet<u32>,
    /// Windows an app hidden at its launch makes only once it is shown again (Reminders, live
    /// 11-16).
    on_show: BTreeMap<i32, Vec<u32>>,
}

impl Default for FakeWindowManager {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeWindowManager {
    /// No window yet; a strip of room 4, shown.
    pub fn new() -> Self {
        Self {
            clock: Instant::now(),
            windows: BTreeMap::new(),
            current: Vec::new(),
            stages: Vec::new(),
            room: 4,
            covered: false,
            changed: None,
            focused: Vec::new(),
            posted: Vec::new(),
            input: InputNow::idle(),
            hidden: BTreeSet::new(),
            interrupt_drags: false,
            ignored_mid_motion: 0,
            deaf: false,
            stack: 16.0,
            item_room: 3,
            relayout: false,
            all_at_once: false,
            landed: None,
            shift: 0.0,
            icon_base: 9900,
            desktops: Vec::new(),
            desktop_jumps: 0,
            shown_nowhere: Vec::new(),
            unhidden: Vec::new(),
            loose: BTreeSet::new(),
            on_show: BTreeMap::new(),
        }
    }

    /// The fake's screen `x` points right of the main screen's origin (a second screen; its
    /// windows numbered apart by the caller), its icon windows numbered from `icon_base`.
    pub fn on_screen_at(mut self, x: f64, icon_base: u32) -> Self {
        self.shift = x;
        self.icon_base = icon_base;
        self
    }

    /// A new Desktop shown: the one shown set aside with its stages, the new one empty.
    pub fn new_desktop(&mut self) {
        self.change();
        let left = (std::mem::take(&mut self.current), std::mem::take(&mut self.stages));
        self.desktops.push(left);
    }

    /// Desktop `k` of the ones not shown (0: the first set aside) shown again, the one shown
    /// set aside in its place.
    pub fn show_desktop(&mut self, k: usize) {
        self.change();
        let Some(back) = self.desktops.get_mut(k) else { return };
        let shown = (std::mem::take(&mut self.current), std::mem::take(&mut self.stages));
        let (current, stages) = std::mem::replace(back, shown);
        self.current = current;
        self.stages = stages;
    }

    /// True when window `id` is on a Desktop not shown.
    fn elsewhere(&self, id: u32) -> bool {
        self.desktops.iter().any(|(c, st)| c.contains(&id) || st.iter().any(|s| s.contains(&id)))
    }

    /// The test clock.
    pub fn now(&self) -> Instant {
        self.clock
    }

    /// The clock moves on `ms`.
    pub fn advance(&mut self, ms: u64) {
        self.clock += Duration::from_millis(ms);
    }

    /// Every window the observer takes as standard (the Apple mode's managed ones).
    pub fn window_ids(&self) -> BTreeSet<u32> {
        self.windows.keys().copied().filter(|id| !self.loose.contains(id)).collect()
    }

    /// The current stage, front to back.
    pub fn current(&self) -> &[u32] {
        &self.current
    }

    /// The stages in the strip's order (past the room too).
    pub fn stages(&self) -> &[Vec<u32>] {
        &self.stages
    }

    /// Each such window's app.
    pub fn pids(&self) -> BTreeMap<u32, i32> {
        self.windows.iter().filter(|(id, _)| !self.loose.contains(id)).map(|(id, (pid, _))| (*id, *pid)).collect()
    }

    /// Window `id`'s full-size frame.
    pub fn frame(&self, id: u32) -> Option<Rect> {
        self.windows.get(&id).map(|(_, f)| *f)
    }

    /// Notes a change now: the motion starts from where every window is listed now.
    fn change(&mut self) {
        let from = self.targets();
        self.changed = Some((self.clock, from));
    }

    /// App `pid` opens windows `ids`: Apple's gives it a stage of its own, in front; the
    /// stage left goes to the top of the strip.
    pub fn open_app(&mut self, pid: i32, ids: &[u32]) {
        self.change();
        for (k, id) in ids.iter().enumerate() {
            let k = k as f64;
            self.windows.insert(*id, (pid, Rect::new(self.shift + 300.0 + 30.0 * k, 120.0 + 30.0 * k, 700.0, 500.0)));
        }
        if !self.current.is_empty() {
            self.stages.insert(0, std::mem::take(&mut self.current));
        }
        self.current = ids.to_vec();
    }

    /// App `pid` opens windows `ids` and was hidden at its launch (`hide_took`): Apple's
    /// keeps the stage in front and puts the app's in the strip, on top (probe 3, 11-16).
    /// Without the hide it is [`FakeWindowManager::open_app`] (the stage in front pushed into
    /// the strip).
    pub fn launch(&mut self, pid: i32, ids: &[u32], hide_took: bool) {
        if !hide_took || self.current.is_empty() {
            self.open_app(pid, ids);
            return;
        }
        self.change();
        self.hidden.insert(pid);
        for (k, id) in ids.iter().enumerate() {
            let k = k as f64;
            self.windows.insert(*id, (pid, Rect::new(self.shift + 360.0 + 30.0 * k, 160.0 + 30.0 * k, 640.0, 460.0)));
        }
        self.stages.insert(0, ids.to_vec());
    }

    /// App `pid`, hidden at its launch, makes windows `ids` only once it is shown again (none
    /// while hidden: Reminders, live 11-16); they go into the strip as a reopened window's do.
    pub fn launch_windowless(&mut self, pid: i32, ids: &[u32]) {
        self.hidden.insert(pid);
        self.on_show.insert(pid, ids.to_vec());
    }

    /// Running app `pid`, with no window, reopens window `id` at its activation, the app
    /// hidden at that activation (`hide_took`): the window shows nowhere until the app is shown
    /// again ([`FakeWindowManager::show_app`]), the stage in front never moving (the sixth probe
    /// of 11-16). Without the hide Apple's switches to it as to a new app's stage.
    pub fn reopen(&mut self, pid: i32, id: u32, hide_took: bool) {
        if !hide_took || self.current.is_empty() {
            self.open_app(pid, &[id]);
            return;
        }
        self.hidden.insert(pid);
        self.windows.insert(id, (pid, Rect::new(self.shift + 360.0, 160.0, 640.0, 460.0)));
        self.shown_nowhere.push(id);
    }

    /// App `pid` shown again without activating it: the windows it reopened while hidden go
    /// into the strip as a stage of their own, on top; the stage in front stays.
    pub fn show_app(&mut self, pid: i32) {
        if !self.hidden.remove(&pid) {
            return;
        }
        for id in self.on_show.remove(&pid).unwrap_or_default() {
            self.windows.insert(id, (pid, Rect::new(self.shift + 360.0, 160.0, 640.0, 460.0)));
            self.shown_nowhere.push(id);
        }
        let ids: Vec<u32> = self.shown_nowhere.iter().copied().filter(|w| self.windows.get(w).is_some_and(|(p, _)| *p == pid)).collect();
        if ids.is_empty() {
            return;
        }
        self.change();
        self.shown_nowhere.retain(|w| !ids.contains(w));
        if self.current.is_empty() {
            self.current = ids;
        } else {
            self.stages.insert(0, ids);
        }
    }

    /// App `pid` opens one more window `id`: into the stage in front when the app has a
    /// window there (or a stage of the app's own, else a new stage in front).
    pub fn open_window(&mut self, pid: i32, id: u32) {
        let of_app = |ws: &[u32], windows: &BTreeMap<u32, (i32, Rect)>| ws.iter().any(|w| windows.get(w).is_some_and(|(p, _)| *p == pid));
        if of_app(&self.current, &self.windows) || self.current.is_empty() {
            self.change();
            self.windows.insert(id, (pid, Rect::new(self.shift + 420.0, 200.0, 600.0, 400.0)));
            self.current.insert(0, id);
            return;
        }
        if let Some(s) = self.stages.iter().position(|st| of_app(st, &self.windows)) {
            self.change();
            self.windows.insert(id, (pid, Rect::new(self.shift + 420.0, 200.0, 600.0, 400.0)));
            self.stages[s].insert(0, id);
            return;
        }
        self.open_app(pid, &[id]);
    }

    /// Apple's answer to a focus of window `id`: its stage comes in front, the stage left
    /// takes its slot, and its app shows again when hidden. Nothing for a window in front or
    /// unknown.
    pub fn focus_window(&mut self, id: u32) {
        if self.elsewhere(id) {
            self.desktop_jumps += 1;
            return;
        }
        if let Some((pid, _)) = self.windows.get(&id) {
            let pid = *pid;
            self.show_app(pid);
        }
        // A window in front is raised within its stage: no motion.
        if let Some(k) = self.current.iter().position(|w| *w == id) {
            let w = self.current.remove(k);
            self.current.insert(0, w);
            return;
        }
        let Some(s) = self.stages.iter().position(|st| st.contains(&id)) else { return };
        self.change();
        let mut target = self.stages.remove(s);
        target.retain(|w| *w != id);
        target.insert(0, id);
        if !self.current.is_empty() {
            self.stages.insert(s, std::mem::take(&mut self.current));
        }
        self.current = target;
    }

    /// Window `id`'s full-size frame set to `frame` (windows opened at one frame, as the
    /// live fixtures do).
    pub fn set_frame(&mut self, id: u32, frame: Rect) {
        if let Some((_, f)) = self.windows.get_mut(&id) {
            *f = frame;
        }
    }

    /// Window `id` closes.
    pub fn close(&mut self, id: u32) {
        self.change();
        self.windows.remove(&id);
        self.shown_nowhere.retain(|w| *w != id);
        self.current.retain(|w| *w != id);
        for st in &mut self.stages {
            st.retain(|w| *w != id);
        }
        self.stages.retain(|st| !st.is_empty());
        for (c, st) in &mut self.desktops {
            c.retain(|w| *w != id);
            st.iter_mut().for_each(|s| s.retain(|w| *w != id));
            st.retain(|s| !s.is_empty());
        }
    }

    /// The strip shows `room` items; the rest are unlisted.
    pub fn set_room(&mut self, room: usize) {
        self.change();
        self.room = room;
    }

    /// A window over the strip hides it (`true`), or it shows again.
    pub fn cover(&mut self, covered: bool) {
        self.change();
        self.covered = covered;
    }

    /// Where each window is listed once the motion ends, and whether on screen.
    fn targets(&self) -> BTreeMap<u32, Rect> {
        let mut out = BTreeMap::new();
        for id in &self.current {
            if let Some(f) = self.frame(*id) {
                out.insert(*id, f);
            }
        }
        for (slot, st) in self.stages.iter().enumerate() {
            for (k, id) in st.iter().enumerate() {
                let r = match (slot < self.room, k < self.item_room) {
                    (true, true) => self.thumb(slot, k),
                    (true, false) => self.undrawn(slot),
                    (false, _) => self.frame(*id).unwrap_or(THUMB),
                };
                out.insert(*id, r);
            }
        }
        out
    }

    /// The `k`th thumbnail of slot `slot` (the front one first; the next 16 points right, as a
    /// live capture of 11-16 had a stage of two).
    fn thumb(&self, slot: usize, k: usize) -> Rect {
        let x = self.shift + if self.covered { HIDDEN_X } else { THUMB.x };
        let k = k as f64;
        Rect::new(x + self.stack * k, THUMB.y + SLOT * slot as f64 + self.stack / 4.0 * k, THUMB.w, THUMB.h)
    }

    /// Where slot `slot`'s windows past [`FakeWindowManager::item_room`] are listed, off screen:
    /// thumbnail-sized, within the item's front thumbnail (live 11-17: 69x63 inside the item).
    fn undrawn(&self, slot: usize) -> Rect {
        let t = self.thumb(slot, 0);
        Rect::new(t.x, t.y + t.h / 3.0, 60.0, 60.0)
    }

    /// What a click at `at` lands on with the strip as laid out now, its motion not waited for
    /// ([`FakeWindowManager::relayout`]): the drawn thumbnails of each listed slot front to
    /// back, then its icon, as [`FakeWindowManager::listed`] orders them.
    fn thumbnail_at_rest(&self, at: Point) -> Option<u32> {
        if self.covered {
            return None;
        }
        for (slot, st) in self.stages.iter().enumerate().take(self.room) {
            if let Some(id) = st.iter().take(self.item_room).enumerate().find(|(k, _)| self.thumb(slot, *k).contains(at)).map(|(_, id)| *id) {
                return Some(id);
            }
            if self.icon(slot).contains(at) {
                return None;
            }
        }
        None
    }

    /// Slot `slot`'s icon window.
    fn icon(&self, slot: usize) -> Rect {
        let t = self.thumb(slot, 0);
        Rect::new(t.x + 40.0, t.y + 50.0, 66.0, 82.0)
    }

    /// What a press at `at` lands on in the list now: the strip window whose thumbnail (or
    /// its twin) is topmost there, `None` for anything else (a window in front, an icon,
    /// nothing).
    fn thumbnail_at(&self, at: Point) -> Option<u32> {
        let top = self.listed().windows.into_iter().find(|w| w.onscreen && w.bounds.contains(at))?;
        let id = if top.pid == MANAGER { top.id.checked_sub(9000).filter(|id| self.windows.contains_key(id))? } else { top.id };
        self.stages.iter().take(self.room).any(|st| st.contains(&id)).then_some(id)
    }

    /// Apple's answer to a Shift-click on window `id`'s thumbnail: that window alone joins the
    /// stage in front (its app shown again), its stage keeps the rest.
    fn add_to_front(&mut self, id: u32) {
        let Some(s) = self.stages.iter().position(|st| st.contains(&id)) else { return };
        self.change();
        self.stages[s].retain(|w| *w != id);
        self.stages.retain(|st| !st.is_empty());
        self.current.insert(0, id);
        if let Some((pid, _)) = self.windows.get(&id) {
            self.hidden.remove(pid);
        }
    }

    /// Apple's answer to a drag of `id` (in front) onto the strip: it leaves for a stage of
    /// its own, on top of the strip.
    fn take_out(&mut self, id: u32) {
        if !self.current.contains(&id) {
            return;
        }
        self.change();
        self.current.retain(|w| *w != id);
        self.stages.insert(0, vec![id]);
    }

    /// Apple's answer to gesture `posted` (see the module docs).
    fn answer(&mut self, posted: &Posted) -> Landed {
        if self.deaf {
            return Landed::Posted;
        }
        match posted {
            Posted::Clicks { presses, .. } => {
                if self.moving().is_some() {
                    self.ignored_mid_motion += presses.len() as u32;
                    return Landed::Posted;
                }
                if self.relayout {
                    for p in presses {
                        if let Some(id) = self.thumbnail_at_rest(p.at) {
                            self.add_to_front(id);
                        }
                    }
                    return Landed::Posted;
                }
                // Every click of the press lands on what the strip showed at its start: Apple's
                // motion of the first starts after the last (50 ms apart).
                let hit: Vec<u32> = presses.iter().filter_map(|p| self.thumbnail_at(p.at)).collect();
                for id in hit {
                    self.add_to_front(id);
                }
                Landed::Posted
            }
            &Posted::Drag { window, from, to, .. } => {
                if self.interrupt_drags {
                    return Landed::Interrupted("a left mouse-up came".into());
                }
                let top = self.listed().windows.into_iter().find(|w| w.onscreen && w.bounds.contains(from)).map(|w| w.id);
                if top != Some(window) {
                    return Landed::Refused(format!("the topmost window at {:.0},{:.0} is not the one meant", from.x, from.y));
                }
                let grabbed = self.frame(window).is_some_and(|f| from.y >= f.y && from.y < f.y + 28.0 && from.x >= f.x && from.x < f.max_x());
                let pid = self.windows.get(&window).map(|(p, _)| *p);
                let sibling = self.current.iter().any(|w| *w != window && self.windows.get(w).map(|(p, _)| *p) == pid);
                if grabbed && to.x < THUMB.max_x() && self.moving().is_none() && !(self.all_at_once && sibling) {
                    self.take_out(window);
                }
                Landed::Posted
            }
        }
    }

    /// True while Apple's motion of the last change runs.
    fn moving(&self) -> Option<f64> {
        let (at, _) = self.changed.as_ref()?;
        let t = self.clock.saturating_duration_since(*at).as_secs_f64() / SETTLE.as_secs_f64();
        (t < 1.0).then_some(t)
    }

    /// The window server's list now: the current stage on screen, then the strip's thumbnails
    /// with WindowManager's windows over them and its icons (once the motion ends), then the
    /// windows of stages past the room, off screen.
    pub fn listed(&self) -> Listed {
        let targets = self.targets();
        let moving = self.moving();
        let at = |id: u32| {
            let to = targets[&id];
            match (moving, self.changed.as_ref().and_then(|(_, from)| from.get(&id))) {
                (Some(t), Some(from)) => Rect::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t, from.w + (to.w - from.w) * t, from.h + (to.h - from.h) * t),
                _ => to,
            }
        };
        let mut windows = Vec::new();
        let app = |id: u32, bounds: Rect, onscreen: bool| ListedWindow { id, pid: self.windows[&id].0, layer: 0, bounds, onscreen };
        for id in &self.current {
            windows.push(app(*id, at(*id), true));
        }
        for (slot, st) in self.stages.iter().enumerate().filter(|(s, _)| *s < self.room) {
            for (k, id) in st.iter().enumerate() {
                let b = at(*id);
                if k >= self.item_room {
                    // No thumbnail drawn: listed off screen within the item, no twin.
                    windows.push(app(*id, b, false));
                    continue;
                }
                if moving.is_none() {
                    windows.push(ListedWindow { id: 9000 + id, pid: MANAGER, layer: 0, bounds: b, onscreen: true });
                }
                windows.push(app(*id, b, true));
            }
            if moving.is_none() {
                windows.push(ListedWindow { id: self.icon_base + slot as u32, pid: MANAGER, layer: 0, bounds: self.icon(slot), onscreen: true });
            }
        }
        for st in self.stages.iter().skip(self.room) {
            for id in st {
                windows.push(app(*id, at(*id), false));
            }
        }
        // A hidden app's reopened windows: listed off screen at their frames.
        for id in &self.shown_nowhere {
            if let Some(f) = self.frame(*id) {
                windows.push(app(*id, f, false));
            }
        }
        // The Desktops not shown: listed off screen at their frames.
        for (current, stages) in &self.desktops {
            for id in current.iter().chain(stages.iter().flatten()) {
                if let Some(f) = self.frame(*id) {
                    windows.push(app(*id, f, false));
                }
            }
        }
        Listed { windows, manager: Some(MANAGER) }
    }

    /// WindowManager's Accessibility now: blank for [`BLANK`] after a change, else one item
    /// per listed slot.
    pub fn reading(&self) -> Reading {
        if self.changed.as_ref().is_some_and(|(at, _)| self.clock.saturating_duration_since(*at) < BLANK) {
            return Reading::default();
        }
        let items = self
            .stages
            .iter()
            .enumerate()
            .filter(|(s, _)| *s < self.room)
            .map(|(slot, st)| Item { frame: self.icon(slot).union(&self.thumb(slot, 0)), windows: st.clone() })
            .collect();
        Reading { items }
    }
}

impl AppleDesk for FakeWindowManager {
    fn strip(&mut self, _display: &Display) -> Option<Reading> {
        Some(self.reading())
    }

    fn listed(&mut self) -> Listed {
        FakeWindowManager::listed(self)
    }

    fn input(&mut self) -> InputNow {
        self.input
    }

    fn focus(&mut self, window: u32) -> Result<(), String> {
        if !self.windows.contains_key(&window) {
            return Err(format!("no window {window}"));
        }
        self.focused.push(window);
        self.focus_window(window);
        Ok(())
    }

    fn post(&mut self, posted: &Posted) -> Result<(), String> {
        self.posted.push(posted.clone());
        let landed = self.answer(posted);
        self.landed = Some(landed);
        Ok(())
    }

    fn landed(&mut self) -> Option<Landed> {
        self.landed.take()
    }

    fn unhide(&mut self, pid: i32) -> Result<(), String> {
        self.unhidden.push(pid);
        self.show_app(pid);
        Ok(())
    }
}
