//! The shadow (design r3, slice 1): where the model's guess of a Desktop and the window
//! server's listing disagree, and since when. It decides nothing; the disagreements it finds
//! in real use are the incidents the later slices remove.
//!
//! Two things are compared, through the model's bindings ([`crate::engine::model::desks`]):
//! the Desktop the model says each screen shows against the listing's current one
//! ([`Subject::Shown`]), and, for each window the window engine holds hidden, the Desktop the
//! engine will show it on against the model's Desktop for it ([`Subject::Home`]). A
//! disagreement is said once when it begins and once when it ends ([`Change`]), never once
//! per look; the counts and the longest one go to `ctl state`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::engine::model::{DisplayId, Model, SpaceId, WindowId};

/// What is compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Subject {
    /// The Desktop a screen shows.
    Shown(DisplayId),
    /// The Desktop a hidden window belongs to.
    Home(WindowId),
}

/// One disagreement as a look saw it: the model's Space and the Desktop bound to it (if
/// any), and the window server's Desktop (the listing's current one, or the engine's home).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sight {
    pub subject: Subject,
    pub space: SpaceId,
    pub model: Option<u64>,
    pub server: u64,
}

/// What a look found changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A disagreement began.
    Began(Sight),
    /// One ended (they agree now, or it is not compared any more: the screen shows no
    /// Desktop, the window is not held), after `lasted`; `sight` as last seen.
    Ended { sight: Sight, lasted: Duration },
}

/// The disagreements of one look: `shown` is each screen's Desktop shown, as the listing
/// says (screens showing no Desktop left out); `homes` each window the engine holds, with
/// the Desktop it was hidden from. Only pairs where the model's Space and the Desktop are
/// bound otherwise ([`Model::desk_agrees`] false) are kept: neither bound says nothing.
pub fn disagreements(m: &Model, shown: &[(DisplayId, u64)], homes: &[(WindowId, u64)]) -> Vec<Sight> {
    let shown = shown.iter().map(|&(d, desk)| (Subject::Shown(d), Some(m.current_space(d)), desk));
    let homes = homes.iter().map(|&(id, desk)| (Subject::Home(id), m.space_of(id), desk));
    shown
        .chain(homes)
        .filter_map(|(subject, space, server)| {
            let space = space?;
            (m.desk_agrees(space, server) == Some(false)).then(|| Sight { subject, space, model: m.desk_of_space(space), server })
        })
        .collect()
}

/// The disagreements open now and what has been seen since the launch.
#[derive(Debug, Clone, Default)]
pub struct Shadow {
    open: BTreeMap<Subject, (Sight, Instant)>,
    /// Disagreements on the Desktop shown begun since the launch.
    pub shown_begun: u32,
    /// Disagreements on a window's Desktop begun since the launch.
    pub homes_begun: u32,
    /// The longest one ended.
    ended_longest: Duration,
}

impl Shadow {
    /// A look at `now` that found `seen` (every disagreement there is now,
    /// [`disagreements`]): what began and what ended.
    pub fn look(&mut self, seen: Vec<Sight>, now: Instant) -> Vec<Change> {
        let mut changes = Vec::new();
        let gone: Vec<Subject> = self.open.keys().copied().filter(|s| !seen.iter().any(|x| x.subject == *s)).collect();
        for subject in gone {
            if let Some((sight, since)) = self.open.remove(&subject) {
                let lasted = now.saturating_duration_since(since);
                self.ended_longest = self.ended_longest.max(lasted);
                changes.push(Change::Ended { sight, lasted });
            }
        }
        for sight in seen {
            match self.open.get_mut(&sight.subject) {
                Some(open) => open.0 = sight,
                None => {
                    match sight.subject {
                        Subject::Shown(_) => self.shown_begun += 1,
                        Subject::Home(_) => self.homes_begun += 1,
                    }
                    self.open.insert(sight.subject, (sight, now));
                    changes.push(Change::Began(sight));
                }
            }
        }
        changes
    }

    /// Disagreements on the Desktop shown open now.
    pub fn shown_open(&self) -> usize {
        self.open.keys().filter(|s| matches!(s, Subject::Shown(_))).count()
    }

    /// Disagreements on a window's Desktop open now.
    pub fn homes_open(&self) -> usize {
        self.open.keys().filter(|s| matches!(s, Subject::Home(_))).count()
    }

    /// The longest disagreement so far, an open one counted to `now`.
    pub fn longest(&self, now: Instant) -> Duration {
        self.open.values().map(|(_, since)| now.saturating_duration_since(*since)).fold(self.ended_longest, Duration::max)
    }

    /// The disagreements open now, with when each began.
    pub fn open(&self) -> impl Iterator<Item = &(Sight, Instant)> {
        self.open.values()
    }
}

#[cfg(test)]
mod tests;
