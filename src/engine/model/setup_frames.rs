//! Each window's frame per screen setup, kept and put back (`[displays] frames_per_setup`;
//! the rules are [`crate::engine::setup_frames`]'s, the timing the controller's).
//!
//! The frames live beside the windows ([`Model::setup_frames`], by window), go into
//! `state.json` with each saved window ([`crate::engine::persist::SavedWindow::setups`]) and
//! come back with the window matched to it after a restart. A window gone leaves its frames
//! with it.

use std::collections::HashSet;

use super::{Action, Model, WindowId, WindowRole};
use crate::engine::setup_frames::{PutBack, SetupFrame, SetupKey, Settled, absolute, keep, kept, relative};
use crate::engine::stage_area::same_frame;

impl Model {
    /// The setup of the screens the model has now ([`SetupKey::of`]).
    pub fn setup_key(&self) -> Option<SetupKey> {
        SetupKey::of(&self.displays)
    }

    /// True when some window keeps a frame for `setup`: the setup was seen before.
    pub fn knows_setup(&self, setup: &SetupKey) -> bool {
        self.setup_frames.values().any(|list| kept(list, setup).is_some())
    }

    /// Window `id`'s frames, the most recent setup first.
    pub fn setup_frames_of(&self, id: WindowId) -> &[SetupFrame] {
        self.setup_frames.get(&id).map_or(&[], Vec::as_slice)
    }

    /// Every standard window's frame now, for `setup`: the screen it is on and its frame
    /// there (its pre-fit frame when it was fitted clear of the strip, else its restore frame:
    /// the live one when shown, the kept one when hidden). Windows on a screen of no known
    /// identity, or with no usable frame, are left out.
    pub fn setup_frames_now(&self, setup: &SetupKey) -> Vec<(WindowId, SetupFrame)> {
        self.windows
            .values()
            .filter(|w| w.meta.role == WindowRole::Standard)
            .filter_map(|w| {
                let d = self.displays.iter().find(|d| d.id == w.meta.display)?;
                let frame = w.original.unwrap_or(w.restore);
                (d.key.known() && frame.is_finite() && !frame.is_degenerate()).then(|| {
                    (w.meta.id, SetupFrame { setup: setup.text().to_string(), screen: d.key.text(), frame: relative(frame, d.frame) })
                })
            })
            .collect()
    }

    /// Keeps `frames` (from [`Model::setup_frames_now`]) for the windows still known; the
    /// frames of windows gone go. True when anything changed (the file is saved again then).
    pub fn keep_setup_frames(&mut self, frames: Vec<(WindowId, SetupFrame)>) -> bool {
        let before = self.setup_frames.len();
        let windows = &self.windows;
        self.setup_frames.retain(|id, _| windows.contains_key(id));
        let mut changed = before != self.setup_frames.len();
        for (id, entry) in frames {
            if self.windows.contains_key(&id) {
                changed |= keep(self.setup_frames.entry(id).or_default(), entry);
            }
        }
        changed
    }

    /// Takes window `id`'s frames from its saved entry (a window matched after a restart).
    pub(crate) fn take_setup_frames(&mut self, id: WindowId, frames: &[SetupFrame]) {
        if !frames.is_empty() {
            self.setup_frames.insert(id, frames.to_vec());
        }
    }

    /// A display change settled: the windows go back to their frames for the setup now
    /// when it was seen before ([`Model::put_back_setup_frames`], but those in `touched`), and
    /// every window's frame is kept for it from here (macOS's, for a setup not seen before).
    pub fn setup_settled(&mut self, touched: &HashSet<WindowId>) -> (Vec<Action>, Settled) {
        let Some(setup) = self.setup_key() else { return (Vec::new(), Settled::Unknown) };
        let (actions, settled) = if self.knows_setup(&setup) {
            let (actions, note) = self.put_back_setup_frames(&setup, touched);
            (actions, Settled::Known(setup.screens(), note))
        } else {
            (Vec::new(), Settled::New(setup.screens()))
        };
        let frames = self.setup_frames_now(&setup);
        self.keep_setup_frames(frames);
        (actions, settled)
    }

    /// A display change settled into `setup`, one seen before: each standard window with a
    /// frame kept for it goes back there, on the same screen. A shown window is moved (fitted
    /// clear of the strip and brought within reach as any restore is); a hidden one (a hidden
    /// stage's, or an app's hidden with ⌘H) gets it as its restore frame, shown there next
    /// time. Left alone: the windows in `touched` (the person's since the change), one on
    /// another screen than its frame's or on a Desktop not shown (nothing moves a window
    /// between screens, Desktops or stages), and one with no frame for the setup.
    pub fn put_back_setup_frames(&mut self, setup: &SetupKey, touched: &HashSet<WindowId>) -> (Vec<Action>, PutBack) {
        let mut actions = Vec::new();
        let mut note = PutBack::default();
        let ids: Vec<WindowId> = self.windows.values().filter(|w| w.meta.role == WindowRole::Standard).map(|w| w.meta.id).collect();
        for id in ids {
            let Some(entry) = self.setup_frames.get(&id).and_then(|list| kept(list, setup)).cloned() else {
                note.none_kept += 1;
                continue;
            };
            if touched.contains(&id) {
                note.touched += 1;
                continue;
            }
            let Some(w) = self.windows.get(&id) else { continue };
            let screen = self.displays.iter().find(|d| d.id == w.meta.display && d.key.text() == entry.screen).copied();
            // A window of an app hidden now (⌘H) is out of view as a hidden stage's is: its
            // restore frame only, shown there when the app is.
            let parked = w.parked || self.hidden_apps.contains(&w.meta.pid);
            let (shown_space, display) = (self.is_shown_space(w.space), w.meta.display);
            let Some(screen) = screen.filter(|_| parked || shown_space) else {
                note.elsewhere += 1;
                continue;
            };
            let frame = absolute(entry.frame, screen.frame);
            if !frame.is_finite() || frame.is_degenerate() {
                note.none_kept += 1;
                continue;
            }
            if let Some(w) = self.windows.get_mut(&id) {
                w.original = None;
                w.restore = frame;
            }
            self.fit_window(id);
            let Some(w) = self.windows.get(&id) else { continue };
            let to = self.within_reach(display, w.restore);
            let live = w.meta.frame;
            if let Some(w) = self.windows.get_mut(&id) {
                w.restore = to;
            }
            if !parked && !same_frame(to, live) {
                actions.push(Action::SetFrame { id, frame: to });
            }
            note.put_back += 1;
        }
        if note.put_back > 0 {
            actions.push(Action::StateChanged);
        }
        (actions, note)
    }
}
