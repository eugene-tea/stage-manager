//! A fake WindowManager for the Apple mode's tests ([`FakeWindowManager`]): stages, the
//! strip's room, Apple's motion and the 650 ms blank of its Accessibility on a test clock, a
//! covered strip, and Apple's answers (a focus switches to the window's stage, a new app
//! takes a stage of its own); every posted gesture recorded. It implements
//! [`AppleDesk`], so the mirror, following and the routes run against it as against the
//! real desk.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::engine::apple::desk::{AppleDesk, InputNow, Posted};
use crate::engine::apple::mirror::{Item, Listed, ListedWindow, Reading, SETTLE};
use crate::engine::geometry::Rect;
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
}

impl Default for FakeWindowManager {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeWindowManager {
    /// No window yet; a strip of room 4, shown.
    pub fn new() -> Self {
        Self { clock: Instant::now(), windows: BTreeMap::new(), current: Vec::new(), stages: Vec::new(), room: 4, covered: false, changed: None, focused: Vec::new(), posted: Vec::new() }
    }

    /// The test clock.
    pub fn now(&self) -> Instant {
        self.clock
    }

    /// The clock moves on `ms`.
    pub fn advance(&mut self, ms: u64) {
        self.clock += Duration::from_millis(ms);
    }

    /// Every window.
    pub fn window_ids(&self) -> BTreeSet<u32> {
        self.windows.keys().copied().collect()
    }

    /// The current stage, front to back.
    pub fn current(&self) -> &[u32] {
        &self.current
    }

    /// The stages in the strip's order (past the room too).
    pub fn stages(&self) -> &[Vec<u32>] {
        &self.stages
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
            self.windows.insert(*id, (pid, Rect::new(300.0 + 30.0 * k, 120.0 + 30.0 * k, 700.0, 500.0)));
        }
        if !self.current.is_empty() {
            self.stages.insert(0, std::mem::take(&mut self.current));
        }
        self.current = ids.to_vec();
    }

    /// Apple's answer to a focus of window `id`: its stage comes in front, the stage left
    /// takes its slot. Nothing for a window in front or unknown.
    pub fn focus_window(&mut self, id: u32) {
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

    /// Window `id` closes.
    pub fn close(&mut self, id: u32) {
        self.change();
        self.windows.remove(&id);
        self.current.retain(|w| *w != id);
        for st in &mut self.stages {
            st.retain(|w| *w != id);
        }
        self.stages.retain(|st| !st.is_empty());
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
                let r = match slot < self.room {
                    true => self.thumb(slot, k),
                    false => self.frame(*id).unwrap_or(THUMB),
                };
                out.insert(*id, r);
            }
        }
        out
    }

    /// The `k`th thumbnail of slot `slot`.
    fn thumb(&self, slot: usize, k: usize) -> Rect {
        let x = if self.covered { HIDDEN_X } else { THUMB.x };
        Rect::new(x + 4.0 * k as f64, THUMB.y + SLOT * slot as f64 + 6.0 * k as f64, THUMB.w, THUMB.h)
    }

    /// Slot `slot`'s icon window.
    fn icon(&self, slot: usize) -> Rect {
        let t = self.thumb(slot, 0);
        Rect::new(t.x + 40.0, t.y + 50.0, 66.0, 82.0)
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
            for id in st {
                let b = at(*id);
                if moving.is_none() {
                    windows.push(ListedWindow { id: 9000 + id, pid: MANAGER, layer: 0, bounds: b, onscreen: true });
                }
                windows.push(app(*id, b, true));
            }
            if moving.is_none() {
                windows.push(ListedWindow { id: 9900 + slot as u32, pid: MANAGER, layer: 0, bounds: self.icon(slot), onscreen: true });
            }
        }
        for st in self.stages.iter().skip(self.room) {
            for id in st {
                windows.push(app(*id, at(*id), false));
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
        InputNow { pointer_still_ms: 1000, key_idle_ms: 1000, ..InputNow::default() }
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
        self.posted.push(*posted);
        Ok(())
    }
}
