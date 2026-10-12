//! The strip: which stages show as cards, in which slots.
//!
//! Apple's strip is slot stable (measured 2026-10-12, `docs/UX-SPEC.md`, "Strip order"):
//!
//! - Switching to a stage whose card is in the strip is a **swap**: the stage being left takes
//!   the slot the clicked card vacated.
//! - A stage that **enters** the strip (the stage left by a desktop reveal or a hotkey switch to
//!   an out-of-view stage, a new inactive stage) takes a free slot at the bottom; with none
//!   free it takes the slot of the **least recently used** card, which drops out of view (its
//!   windows stay parked).
//! - At most `capacity` cards show; the platform computes it from the screen height
//!   (`[strip] capacity*`, Apple: 5 at 1130 pt, 6 at 1410 pt).
//!
//! `StripOrder::Fixed` instead lists stages in creation order, matching hotkey numbers.
//!
//! Every Space has its own strip: slots, order and recency are kept per Space, and a
//! display's strip shows the Space it shows now.

use super::{DisplayId, Model, Pid, SpaceId, StageId, WindowId, WindowRole};
use crate::engine::settings::AppWindows;

/// How cards are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StripOrder {
    /// Apple's slot-stable order with swap and least-recently-used eviction (`"native"`).
    #[default]
    Native,
    /// Creation order, matching hotkey numbers (`"fixed"`).
    Fixed,
}

/// Strip options: ours, since macOS has no setting for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StripOptions {
    /// Card order.
    pub order: StripOrder,
    /// Show the active stage as a card too. Applies to `Fixed` order only; Apple's native
    /// strip never shows the active stage.
    pub show_active: bool,
    /// The most cards shown (Apple: 6 at 1440 pt height); computed by the platform.
    pub capacity: usize,
}

impl Default for StripOptions {
    fn default() -> Self {
        Self {
            order: StripOrder::Native,
            show_active: false,
            capacity: 6,
        }
    }
}

/// One card to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripCard {
    /// The stage.
    pub stage: StageId,
    /// Its 1-based hotkey number.
    pub number: usize,
    /// Its name, if any.
    pub name: Option<String>,
    /// Standard member windows, front to back.
    pub windows: Vec<WindowId>,
    /// Distinct apps in icon order (by their front-most window).
    pub apps: Vec<Pid>,
    /// True for the active stage (only with `show_active` in fixed order).
    pub active: bool,
}

impl Model {
    /// The cards for the main display (the Space it shows now).
    pub fn strip(&self) -> Vec<StripCard> {
        self.strip_for(self.displays[0].id)
    }

    /// True when the strip stays hidden until edge hover (Apple's "Show recent apps" off).
    pub fn strip_auto_hides(&self) -> bool {
        self.options.settings.hide_recent_apps
    }

    /// The cards for one display: those of the Space it shows now.
    pub fn strip_for(&self, display: DisplayId) -> Vec<StripCard> {
        self.strip_in(self.current_space(display))
    }

    /// The cards of one Space.
    pub fn strip_in(&self, space: SpaceId) -> Vec<StripCard> {
        let opts = self.options.strip;
        let active = self.active_stage_in(space);
        let ids: Vec<StageId> = match opts.order {
            StripOrder::Native => self.slots.get(&space).cloned().unwrap_or_default(),
            StripOrder::Fixed => self
                .stages
                .iter()
                .filter(|s| {
                    s.space == space
                        && if Some(s.id) == active {
                            opts.show_active && !s.windows.is_empty()
                        } else {
                            self.strip_eligible(s.id)
                        }
                })
                .map(|s| s.id)
                .take(opts.capacity)
                .collect(),
        };
        let cards: Vec<StripCard> = ids.into_iter().filter_map(|id| self.card(id, Some(id) == active)).collect();
        if self.options.settings.app_windows == AppWindows::OneAtATime {
            self.group_one_at_a_time(space, cards)
        } else {
            cards
        }
    }

    /// With "One at a Time", an app's windows sit in stages of one window each; Apple shows
    /// the app once on the left, and clicking it again opens the next window. So the cards
    /// of single-app stages of one app become one card (in the first one's slot) whose stage
    /// is the app's least recently shown window (clicks cycle through them all), with the
    /// app's other waiting windows stacked behind it.
    fn group_one_at_a_time(&self, space: SpaceId, cards: Vec<StripCard>) -> Vec<StripCard> {
        let single_app = |c: &StripCard| (c.apps.len() == 1).then(|| c.apps[0]);
        let mut out: Vec<StripCard> = Vec::new();
        let mut done: Vec<Pid> = Vec::new();
        for card in cards {
            let Some(pid) = single_app(&card).filter(|_| !card.active) else {
                out.push(card);
                continue;
            };
            if done.contains(&pid) {
                continue;
            }
            done.push(pid);
            // Every inactive single-window-app stage of this app on the Space, also the
            // ones out of view, least recently shown first.
            let mut group: Vec<(u64, StageId)> = self
                .stages
                .iter()
                .filter(|s| s.space == space && !self.is_active(s.id) && !s.windows.is_empty())
                .filter(|s| s.windows.iter().all(|w| self.windows.get(w).is_some_and(|w| w.meta.pid == pid)))
                .map(|s| (s.last_active, s.id))
                .collect();
            group.sort();
            let Some(next) = group.first().map(|g| g.1).and_then(|id| self.card(id, false)) else {
                out.push(card);
                continue;
            };
            let mut windows = next.windows.clone();
            for (_, id) in group.iter().skip(1) {
                if let Some(s) = self.stage(*id) {
                    for w in &s.windows {
                        if !windows.contains(w) {
                            windows.push(*w);
                        }
                    }
                }
            }
            out.push(StripCard { windows, ..next });
        }
        out
    }

    /// Updates the strip options; capacity changes evict least recently used cards or refill.
    pub fn set_strip_options(&mut self, options: StripOptions) {
        self.options.strip = options;
        self.normalize_slots();
    }

    /// Sets how many cards fit (the platform computes it from the display height).
    pub fn set_strip_capacity(&mut self, capacity: usize) {
        self.options.strip.capacity = capacity;
        self.normalize_slots();
    }

    fn card(&self, id: StageId, active: bool) -> Option<StripCard> {
        let s = self.stage(id)?;
        let windows: Vec<WindowId> = s
            .windows
            .iter()
            .copied()
            .filter(|w| {
                self.windows
                    .get(w)
                    .is_some_and(|w| w.meta.role == WindowRole::Standard)
            })
            .collect();
        let mut apps = Vec::new();
        for w in &windows {
            if let Some(pid) = self.windows.get(w).map(|w| w.meta.pid)
                && !apps.contains(&pid)
            {
                apps.push(pid);
            }
        }
        Some(StripCard {
            stage: id,
            number: self.stage_number(id)?,
            name: s.name.clone(),
            windows,
            apps,
            active,
        })
    }

    /// An inactive stage that may hold a card: it has windows or is declared in config.
    pub(crate) fn strip_eligible(&self, id: StageId) -> bool {
        let Some(s) = self.stage(id) else {
            return false;
        };
        !self.is_active(id)
            && (!s.windows.is_empty() || s.name.as_deref().is_some_and(|n| self.is_declared(n)))
    }

    /// The stage `leaving` gives up the active spot to `target`: swap slots, or let `leaving`
    /// enter the strip when `target` had no card.
    pub(crate) fn strip_swap(&mut self, space: SpaceId, leaving: StageId, target: StageId) {
        let slots = self.slots.entry(space).or_default();
        if let Some(i) = slots.iter().position(|&s| s == target) {
            slots[i] = leaving;
        } else {
            self.strip_enter(space, leaving);
        }
    }

    /// A stage enters the strip: a free slot at the bottom, else the least recently used
    /// card's slot.
    pub(crate) fn strip_enter(&mut self, space: SpaceId, stage: StageId) {
        let capacity = self.options.strip.capacity;
        if capacity == 0 {
            return;
        }
        let recency: Vec<(StageId, u64)> =
            self.stages.iter().map(|s| (s.id, s.last_active)).collect();
        let slots = self.slots.entry(space).or_default();
        if slots.contains(&stage) {
            return;
        }
        if slots.len() < capacity {
            slots.push(stage);
            return;
        }
        let age = |id: StageId| recency.iter().find(|r| r.0 == id).map_or(0, |r| r.1);
        if let Some((i, _)) = slots.iter().enumerate().min_by_key(|(_, s)| age(**s)) {
            slots[i] = stage;
        }
    }

    /// Keeps the slots consistent: only eligible stages, no duplicates, at most `capacity`
    /// (evicting least recently used), and free room refilled with the most recent stages
    /// that are out of view.
    pub(crate) fn normalize_slots(&mut self) {
        let capacity = self.options.strip.capacity;
        let spaces: Vec<SpaceId> = self.spaces.iter().map(|s| s.id).collect();
        self.slots.retain(|d, _| spaces.contains(d));
        for space in spaces {
            let mut slots = self.slots.remove(&space).unwrap_or_default();
            let mut seen = Vec::new();
            slots.retain(|&s| {
                let keep = !seen.contains(&s)
                    && self.stage(s).is_some_and(|st| st.space == space)
                    && self.strip_eligible(s);
                seen.push(s);
                keep
            });
            while slots.len() > capacity {
                let oldest = slots
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, s)| self.stage(**s).map_or(0, |st| st.last_active))
                    .map(|(i, _)| i);
                match oldest {
                    Some(i) => {
                        slots.remove(i);
                    }
                    None => break,
                }
            }
            let mut out_of_view: Vec<(u64, StageId)> = self
                .stages
                .iter()
                .filter(|s| {
                    s.space == space && !slots.contains(&s.id) && self.strip_eligible(s.id)
                })
                .map(|s| (s.last_active, s.id))
                .collect();
            out_of_view.sort_by(|a, b| b.cmp(a));
            for (_, id) in out_of_view {
                if slots.len() >= capacity {
                    break;
                }
                slots.push(id);
            }
            self.slots.insert(space, slots);
        }
    }

    /// Stage IDs in the strip's slots for a display's current Space (native order), top to
    /// bottom.
    pub fn strip_slots(&self, display: DisplayId) -> Vec<StageId> {
        self.slots.get(&self.current_space(display)).cloned().unwrap_or_default()
    }
}
