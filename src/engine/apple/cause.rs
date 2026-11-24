//! Who caused an app's activation in the Apple mode ([`Causes`]), by the order of what was
//! heard, time only as a cap (review 11-21, rule 1: "the first X after Y with no Z between").
//! The platform stamps every notice and input at its receipt; nothing here reads a clock.
//!
//! - **Ours**: the first activation notice of an app we activated ourselves (a give-up's
//!   activation given back), with no input of the person's between our ask and the notice,
//!   within [`OWN_ACTIVATION`]. Live 11-17: Contacts opened here, its hide handed the focus to
//!   Finder, the job gave up by activating Finder, whose activation started the job again, six
//!   times over until the person clicked another app.
//! - **Handed over**: the first activation of another app after a hide of ours took, with no
//!   input of the person's between, within [`HANDED_OVER`]: macOS gives the focus on when the
//!   app in front hides (live 11-17: Finder, 13 ms after Contacts was hidden).
//! - Anything else is the person's (or the app's own): the platform judges it as such.
//!
//! What stays a plain time in the mode's controller, as nothing heard ties it to its cause: an
//! app's own activations while it launches (`LAUNCH_ACTIVATION`: an app may activate more than
//! once as it starts, and only time ties them to the launch; a job or hiding for the pid
//! refuses a second "open here" anyway), and a posted gesture's echoes in the input monitor
//! (`post::ECHO`: the person's Shift and ours read the same unless the posted events are
//! tagged, which no probe has checked yet).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::engine::margin::{self, End};

/// How long after our activation of an app its notice is still taken as ours, at most (the
/// first notice after the ask decides; this only bounds a notice that never came).
pub const OWN_ACTIVATION: Duration = Duration::from_secs(5);

/// How long after a hide of ours took another app's first activation is still taken as macOS
/// handing it the focus, at most (live 11-17: 13 ms).
pub const HANDED_OVER: Duration = Duration::from_secs(2);

/// What caused an activation ([`Causes::activation`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Our own activation of the app.
    Ours,
    /// macOS handing the focus on as a hide of ours took.
    HandedOver,
    /// Nothing of ours: the person's, or the app's own.
    Other,
}

/// The causes of ours waiting for their activation notice (module docs).
#[derive(Debug, Clone, Default)]
pub struct Causes {
    /// Apps we activated, and when, each until its first activation notice.
    asked: BTreeMap<i32, Instant>,
    /// Hides of ours that took, by the app hidden, and when; `None` once another app's
    /// activation was taken as handed over.
    hides: BTreeMap<i32, Option<Instant>>,
    /// The person's latest input, as stamped at its receipt.
    person: Option<Instant>,
}

impl Causes {
    /// We activated app `pid` at `at`.
    pub fn activated_by_us(&mut self, pid: i32, at: Instant) {
        self.asked.insert(pid, at);
    }

    /// A hide of ours of app `pid` took at `at` (once per hide: told again, kept as first told).
    pub fn hide_took(&mut self, pid: i32, at: Instant) {
        self.hides.entry(pid).or_insert(Some(at));
    }

    /// Only the hides of the apps `keep` says are still hidden by us are kept.
    pub fn keep_hides(&mut self, keep: impl Fn(i32) -> bool) {
        self.hides.retain(|pid, _| keep(*pid));
    }

    /// The person's input heard at `at` (a gesture of ours and its echoes never count).
    pub fn person(&mut self, at: Instant) {
        self.person = Some(self.person.map_or(at, |p| p.max(at)));
    }

    /// True when the person's input came after `since` and no later than `fired`.
    fn input_between(&self, since: Instant, fired: Instant) -> bool {
        self.person.is_some_and(|p| p > since && p <= fired)
    }

    /// App `pid`'s activation notice, stamped `fired`: what caused it (module docs). Our ask or
    /// a hide it answers is used up.
    pub fn activation(&mut self, pid: i32, fired: Instant) -> Cause {
        if let Some(t) = self.asked.remove(&pid) {
            let age = margin::age("apple::OWN_ACTIVATION", fired, t);
            let how = if self.input_between(t, fired) {
                End::Refuted
            } else if age < OWN_ACTIVATION {
                End::Met
            } else {
                End::Cap
            };
            margin::wait("apple::OWN_ACTIVATION", age, OWN_ACTIVATION, how);
            if how == End::Met {
                return Cause::Ours;
            }
        }
        let person = self.person;
        let mut handed = false;
        for (hidden, took) in self.hides.iter_mut().filter(|(h, _)| **h != pid) {
            let Some(t) = *took else { continue };
            if t > fired {
                // Stamped before the hide took: not its answer.
                continue;
            }
            // The first activation of another app after the hide answers it, either way.
            *took = None;
            let age = fired.saturating_duration_since(t);
            let how = if person.is_some_and(|p| p > t && p <= fired) {
                End::Refuted
            } else if age < HANDED_OVER {
                End::Met
            } else {
                End::Cap
            };
            margin::wait("apple::HANDED_OVER", age, HANDED_OVER, how);
            tracing::trace!(hidden, ?how, "apple mode: an activation after a hide of ours");
            handed |= how == End::Met;
        }
        if handed { Cause::HandedOver } else { Cause::Other }
    }

    /// Forgets our asks no notice answered within [`OWN_ACTIVATION`] of `now`.
    pub fn prune(&mut self, now: Instant) {
        self.asked.retain(|_, t| now.saturating_duration_since(*t) < OWN_ACTIVATION);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn the_first_activation_after_ours_is_ours_and_only_the_first() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        c.activated_by_us(7, t0);
        // However late the notice is handled: its order decides, its stamp within the cap.
        assert_eq!(c.activation(7, ms(t0, 2400)), Cause::Ours);
        assert_eq!(c.activation(7, ms(t0, 2500)), Cause::Other, "used up");
        // Another app's activation is not ours.
        c.activated_by_us(7, ms(t0, 3000));
        assert_eq!(c.activation(8, ms(t0, 3010)), Cause::Other);
        assert_eq!(c.activation(7, ms(t0, 3020)), Cause::Ours);
    }

    #[test]
    fn the_persons_input_between_makes_it_theirs() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        c.activated_by_us(7, t0);
        c.person(ms(t0, 40));
        assert_eq!(c.activation(7, ms(t0, 50)), Cause::Other, "a click between our ask and the notice");
        // Input before our ask does not count.
        c.activated_by_us(7, ms(t0, 100));
        assert_eq!(c.activation(7, ms(t0, 150)), Cause::Ours);
    }

    #[test]
    fn a_notice_past_the_cap_is_not_ours() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        c.activated_by_us(7, t0);
        assert_eq!(c.activation(7, t0 + OWN_ACTIVATION), Cause::Other);
        c.activated_by_us(7, t0);
        c.prune(t0 + OWN_ACTIVATION);
        assert_eq!(c.activation(7, ms(t0, 10)), Cause::Other, "pruned");
    }

    #[test]
    fn the_first_other_activation_after_a_hide_took_is_handed_over() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        c.hide_took(40, t0);
        assert_eq!(c.activation(40, ms(t0, 5)), Cause::Other, "the app hidden itself: not handed over");
        // Live 11-17: Finder 13 ms after Contacts hid; a slow Mac much later, still the first.
        assert_eq!(c.activation(50, ms(t0, 1300)), Cause::HandedOver);
        assert_eq!(c.activation(60, ms(t0, 1400)), Cause::Other, "used up");
        c.hide_took(40, ms(t0, 2000));
        assert_eq!(c.activation(60, ms(t0, 2100)), Cause::Other, "told again: kept as first told");
    }

    #[test]
    fn the_persons_input_after_the_hide_makes_the_next_activation_theirs() {
        let t0 = Instant::now();
        let mut c = Causes::default();
        c.hide_took(40, t0);
        c.person(ms(t0, 30));
        assert_eq!(c.activation(50, ms(t0, 60)), Cause::Other);
        // An activation stamped before the hide took is not its answer, nor used up by it.
        let mut c = Causes::default();
        c.hide_took(40, ms(t0, 100));
        assert_eq!(c.activation(50, ms(t0, 90)), Cause::Other);
        assert_eq!(c.activation(50, ms(t0, 110)), Cause::HandedOver);
        // A hide no longer ours is forgotten.
        let mut c = Causes::default();
        c.hide_took(40, t0);
        c.keep_hides(|_| false);
        assert_eq!(c.activation(50, ms(t0, 10)), Cause::Other);
    }
}
