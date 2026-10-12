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
//! - At most `capacity` cards show; the platform computes it from the screen height.
//!
//! `StripOrder::Fixed` instead lists stages in creation order, matching hotkey numbers.

use super::{DisplayId, Model, Pid, StageId, WindowId, WindowRole};

/// Vertical room per card in points: UX-SPEC's measured card pitch (card tops 231, 406, 557,
/// 728, 913, 1062 pt, so about 166 pt) plus a little air.
pub const CARD_PITCH: f64 = 170.0;

/// Room kept free above and below the column of cards, in points. Measured: six cards at
/// 1410 pt of visible height left about 200 pt at each end.
pub const STRIP_MARGIN: f64 = 190.0;

/// The fewest and the most cards the strip shows, whatever the screen.
pub const CAPACITY_RANGE: (usize, usize) = (3, 9);

/// How many cards fit in a display's visible height (Apple: 6 at 1410 pt).
pub fn capacity_for_height(visible_height: f64) -> usize {
    let (lo, hi) = CAPACITY_RANGE;
    if !visible_height.is_finite() {
        return lo;
    }
    let n = ((visible_height - 2.0 * STRIP_MARGIN) / CARD_PITCH).floor();
    if n.is_nan() || n < lo as f64 {
        lo
    } else {
        (n as usize).min(hi)
    }
}

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
    /// The cards for the main display.
    pub fn strip(&self) -> Vec<StripCard> {
        self.strip_for(self.displays[0].id)
    }

    /// True when the strip stays hidden until edge hover (Apple's "Show recent apps" off).
    pub fn strip_auto_hides(&self) -> bool {
        self.options.settings.hide_recent_apps
    }

    /// The cards for one display.
    pub fn strip_for(&self, display: DisplayId) -> Vec<StripCard> {
        let opts = self.options.strip;
        let active = self.active_stage(display);
        let ids: Vec<StageId> = match opts.order {
            StripOrder::Native => self.slots.get(&display).cloned().unwrap_or_default(),
            StripOrder::Fixed => self
                .stages
                .iter()
                .filter(|s| {
                    s.display == display
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
        ids.into_iter()
            .filter_map(|id| self.card(id, Some(id) == active))
            .collect()
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
    pub(crate) fn strip_swap(&mut self, display: DisplayId, leaving: StageId, target: StageId) {
        let slots = self.slots.entry(display).or_default();
        if let Some(i) = slots.iter().position(|&s| s == target) {
            slots[i] = leaving;
        } else {
            self.strip_enter(display, leaving);
        }
    }

    /// A stage enters the strip: a free slot at the bottom, else the least recently used
    /// card's slot.
    pub(crate) fn strip_enter(&mut self, display: DisplayId, stage: StageId) {
        let capacity = self.options.strip.capacity;
        if capacity == 0 {
            return;
        }
        let recency: Vec<(StageId, u64)> =
            self.stages.iter().map(|s| (s.id, s.last_active)).collect();
        let slots = self.slots.entry(display).or_default();
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
        let displays: Vec<DisplayId> = self.displays.iter().map(|d| d.id).collect();
        self.slots.retain(|d, _| displays.contains(d));
        for display in displays {
            let mut slots = self.slots.remove(&display).unwrap_or_default();
            let mut seen = Vec::new();
            slots.retain(|&s| {
                let keep = !seen.contains(&s)
                    && self.stage(s).is_some_and(|st| st.display == display)
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
                    s.display == display && !slots.contains(&s.id) && self.strip_eligible(s.id)
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
            self.slots.insert(display, slots);
        }
    }

    /// Stage IDs in the strip's slots for a display (native order), top to bottom.
    pub fn strip_slots(&self, display: DisplayId) -> Vec<StageId> {
        self.slots.get(&display).cloned().unwrap_or_default()
    }
}
