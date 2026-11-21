//! The check after a batch's raises: a shown stage's windows end in the order asked, also
//! when two apps answer their `AXRaise` before the window server reorders their windows
//! ([`crate::engine::raise_order`] has the why and the rules).
//!
//! Runs after a batch that raised standard windows of two or more apps: a switch's unveil
//! (card clicks, ⌘Tab, Dock clicks; once the strip has the arrival's note,
//! [`Controller::flush_pending`]), a switch run at once, and `finish_ahead`'s raises
//! ([`Controller::execute_batch`]). The keyboard is not held up: the early focus and the
//! batch's own focus have run before it. A state, not a wait (round r6-waits): the first
//! read at once, then one at each fast tick, [`order::POLL`] apart while it runs
//! ([`Controller::fast_tick_soon`]), the WM thread handling its notices between them; ended
//! within [`order::CHECK_CAP`], mostly after [`order::WATCH`] of a right order (longer after
//! a fix, [`order::WATCH_AFTER_FIX`]: the late app's answer to the fix's raise may land after
//! the key's). It never fights the user: a mouse button pressed since the switch began, or
//! another window brought in front, stops it at its next read, and a later batch that raises
//! or focuses a window, or moves one of its windows, ends it ([`Controller::order_check_meets`]).

use crate::platform::clock;
use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::{Controller, ax_focus};
use crate::engine::model::Action;
use crate::engine::raise_order as order;
use crate::platform::observe::{Observer, WindowKind};
use crate::platform::{input, safety, windows};

/// Why a fix stopped short.
const USER_ACTED: &str = "the user clicked or brought another window forward";
const OUT_OF_TIME: &str = "out of time";
const FIXES_SPENT: &str = "fixed twice, still out of order";
const LATER_BATCH: &str = "a later batch moved its windows or raised others";

/// The rest of the fast tick's duties keep their own pace while the check reads sooner
/// (`wm_thread::FAST_TICK_SECONDS`).
const FAST_REST: Duration = Duration::from_millis(20);

/// What batch `actions` raises (in the order sent) and the window it focuses last.
pub(super) fn raises_and_focus(actions: &[Action]) -> (Vec<u32>, Option<u32>) {
    let raises = actions.iter().filter_map(|a| if let Action::Raise { id } = a { Some(id.0) } else { None }).collect();
    let focus = actions.iter().rev().find_map(|a| if let Action::Focus { id } = a { Some(id.0) } else { None });
    (raises, focus)
}

/// A check under way: what it asked, where it stands, when it reads next.
#[derive(Debug)]
pub(super) struct OrderCheck {
    /// The batch's raises (in the order sent) and its focus.
    raises: Vec<u32>,
    focus: Option<u32>,
    /// The order asked, back to front ([`order::asked_order`]).
    asked: Vec<u32>,
    /// The switch's start: a click since then is the user's.
    since: Instant,
    /// The window in front at the check's start (a pinned window over the stage, say).
    front_then: Option<u32>,
    start: Instant,
    deadline: Instant,
    /// The order as first found wrong, for the log line.
    first_wrong: Option<Vec<u32>>,
    fixes: u32,
    /// Since when the order has been right (the watch's start, or a fix's end).
    calm_since: Instant,
    /// A fix under way: the windows raised again in turn.
    fix: Option<Fix>,
    /// When the next read is due.
    next: Instant,
}

/// The windows raised again, back to front, each waited for until it shows above those
/// below it ([`order::PER_WINDOW`] at most).
#[derive(Debug)]
struct Fix {
    again: Vec<u32>,
    /// The next window to raise.
    at: usize,
    /// The window raised last, waited for until this.
    waiting: Option<(u32, Instant)>,
    /// A window skipped, the cap passed.
    late: bool,
}

/// What a read of the order came to.
enum Flow {
    /// Read again at this time.
    Wait(Instant),
    /// The check ends, the order as read last, with why it stopped short (none: settled).
    Done(Vec<u32>, Option<&'static str>),
}

impl OrderCheck {
    /// The user acted since the switch began: a click, or another window in front.
    fn acted(&self, listed: &[u32]) -> bool {
        input::since_last_click() < clock::since(self.since) || order::front_moved(listed, &self.asked, self.front_then)
    }
}

impl Controller {
    /// After a batch that raised `raises` (in the order sent) and focused `focus`, the user's
    /// action counting from `since`: starts the check of the window server's order of those
    /// windows ([`OrderCheck`]; a check still under way ends, [`LATER_BATCH`]), and reads it
    /// once now.
    pub(super) fn settle_raise_order(&mut self, ob: &Observer, raises: &[u32], focus: Option<u32>, since: Instant) {
        if raises.len() < 2 || safety::quit_requested() {
            return;
        }
        let standard = |id: u32| ob.window(id).is_some_and(|w| w.kind != WindowKind::Child);
        let asked = order::asked_order(raises, focus, standard);
        let apps: HashSet<i32> = asked.iter().filter_map(|&id| ob.window(id).map(|w| w.pid)).collect();
        if asked.len() < 2 || apps.len() < 2 {
            return;
        }
        self.end_order_check(LATER_BATCH);
        let start = clock::now();
        let listed = listed_ids();
        self.order_check = Some(OrderCheck {
            raises: raises.to_vec(),
            focus,
            front_then: listed.first().copied(),
            asked,
            since,
            start,
            deadline: start + order::CHECK_CAP,
            first_wrong: None,
            fixes: 0,
            calm_since: start,
            fix: None,
            next: start,
        });
        self.order_check_read(ob, Some(listed));
    }

    /// A batch about to run (`actions`) meets a check under way: one that raises or focuses
    /// a window, or moves a window the check asked about, ends it (what it stacks is the
    /// batch's now). A batch of other windows' frames leaves it reading.
    pub(super) fn order_check_meets(&mut self, actions: &[Action]) {
        let Some(c) = &self.order_check else { return };
        let touches = actions.iter().any(|a| match a {
            Action::Raise { .. } | Action::Focus { .. } => true,
            Action::SetFrame { id, .. } | Action::Hide { id } => c.asked.contains(&id.0),
            Action::StateChanged => false,
        });
        if touches {
            self.end_order_check(LATER_BATCH);
        }
    }

    /// Ends a check under way (`why` it stopped short), the order as the window server
    /// lists it now.
    fn end_order_check(&mut self, why: &'static str) {
        let Some(c) = &self.order_check else { return };
        let got = order::listed_order(&c.asked, &listed_ids());
        self.finish_order_check(got, Some(why));
    }

    /// The fast tick's share for the check: a read when due. True when the rest of the fast
    /// tick runs now too: always with no check under way; with one, at the rest's own pace
    /// ([`FAST_REST`]) while something else wants the fast tick.
    fn order_check_fast_tick(&mut self, ob: &Observer) -> bool {
        let now = clock::now();
        let Some(next) = self.order_check.as_ref().map(|c| c.next) else { return true };
        if now >= next {
            self.order_check_read(ob, None);
        }
        let due = self.fast_rest_at.is_none_or(|at| now >= at);
        due && self.wants_fast_tick()
    }

    /// The WM thread's fast tick: the check's read ([`Self::order_check_fast_tick`]), then
    /// the rest ([`Self::fast_tick`]).
    pub fn run_fast_tick(&mut self, ob: &mut Observer) {
        if self.order_check_fast_tick(ob) {
            self.fast_rest_at = Some(clock::now() + FAST_REST);
            self.fast_tick(ob);
        }
    }

    /// True while the fast tick has work: the check, or the rest ([`Self::wants_fast_tick`]).
    pub fn fast_tick_wanted(&self) -> bool {
        self.order_check.is_some() || self.wants_fast_tick()
    }

    /// How soon the fast tick should come when sooner than its period: the check's next read.
    pub fn fast_tick_soon(&self) -> Option<Duration> {
        self.order_check.as_ref().map(|c| c.next.saturating_duration_since(clock::now()).max(Duration::from_millis(1)))
    }

    /// Reads the order (`listed`: read already, at the check's start) and goes on as far as
    /// it can without waiting: a fix's raises, each read back at once, the key's focus.
    fn order_check_read(&mut self, ob: &Observer, mut listed: Option<Vec<u32>>) {
        loop {
            let Some(c) = self.order_check.as_ref() else { return };
            let flow = match &c.fix {
                // A fix's next raise goes with no read before it.
                Some(f) if f.waiting.is_none() => self.raise_next(ob),
                Some(_) => {
                    let now_listed = listed.take().unwrap_or_else(listed_ids);
                    self.fix_read(ob, &now_listed)
                }
                None => {
                    let now_listed = listed.take().unwrap_or_else(listed_ids);
                    self.order_check.as_mut().and_then(|c| Self::watch_read(c, &now_listed))
                }
            };
            match flow {
                Some(Flow::Wait(at)) => {
                    if let Some(c) = self.order_check.as_mut() {
                        c.next = at;
                    }
                    return;
                }
                Some(Flow::Done(got, why)) => {
                    self.finish_order_check(got, why);
                    return;
                }
                // On at once.
                None => {}
            }
        }
    }

    /// A read while watching: settled, waiting, or a fix begins (`None`: its first raise
    /// goes at once).
    fn watch_read(c: &mut OrderCheck, listed: &[u32]) -> Option<Flow> {
        let got = order::listed_order(&c.asked, listed);
        let again = order::to_raise_again(&c.asked, &got);
        let now = clock::now();
        if c.acted(listed) {
            return Some(Flow::Done(got, Some(USER_ACTED)));
        }
        if again.is_empty() {
            if now.duration_since(c.calm_since) >= order::calm_needed(c.fixes) || now >= c.deadline {
                return Some(Flow::Done(got, None));
            }
            return Some(Flow::Wait(now + order::POLL.min(c.deadline - now)));
        }
        c.first_wrong.get_or_insert_with(|| got.clone());
        if c.fixes == order::MAX_FIXES {
            return Some(Flow::Done(got, Some(FIXES_SPENT)));
        }
        if now + order::POLL >= c.deadline {
            return Some(Flow::Done(got, Some(OUT_OF_TIME)));
        }
        c.fixes += 1;
        c.fix = Some(Fix { again, at: 0, waiting: None, late: false });
        None
    }

    /// A read during a fix (`listed`): the window raised last waited for until it shows
    /// above those below it, then the next one raised ([`Self::raise_next`]).
    fn fix_read(&mut self, ob: &Observer, listed: &[u32]) -> Option<Flow> {
        let c = self.order_check.as_ref()?;
        let (id, until) = c.fix.as_ref()?.waiting?;
        if c.acted(listed) {
            let got = order::listed_order(&c.asked, &listed_ids());
            return Some(Flow::Done(got, Some(USER_ACTED)));
        }
        let now = clock::now();
        if !order::shows_above(id, order::below(&c.asked, id), &order::listed_order(&c.asked, listed)) && now < until {
            return Some(Flow::Wait(now + order::POLL.min(until - now)));
        }
        let fix = self.order_check.as_mut()?.fix.as_mut()?;
        fix.waiting = None;
        fix.at += 1;
        self.raise_next(ob)
    }

    /// Raises the fix's next window once more (back to front), with the children the batch
    /// raised right after it (`raises`), and waits for it to show above the windows `asked`
    /// puts below it; with none left, focuses the batch's focus as the batch did. Out of
    /// time, the windows left are skipped but the key's raise and focus still run (a window
    /// raised again over it must not stay in front).
    fn raise_next(&mut self, ob: &Observer) -> Option<Flow> {
        loop {
            let c = self.order_check.as_ref()?;
            let fix = c.fix.as_ref()?;
            let Some(&id) = fix.again.get(fix.at) else { break };
            let (focus, deadline) = (c.focus, c.deadline);
            let now = clock::now();
            let is_key = focus == Some(id);
            let skip = |this: &mut Self, late: bool| {
                if let Some(f) = this.order_check.as_mut().and_then(|c| c.fix.as_mut()) {
                    f.at += 1;
                    f.late |= late;
                }
            };
            if now >= deadline && !is_key {
                skip(self, true);
                continue;
            }
            let Some(w) = ob.window(id) else {
                skip(self, false);
                continue;
            };
            if self.keyboard_withheld(id, w.pid, "an order check's raise again") {
                skip(self, false);
                continue;
            }
            let budget = deadline.saturating_duration_since(now).clamp(order::POLL, order::PER_WINDOW);
            self.causes.expect_raise(id, w.pid, now);
            // A fresh handle found this batch for a dead one wins (stale-handles).
            let element = ob.element(id).unwrap_or_else(|| w.element.clone());
            let _ = element.bounded(budget.as_secs_f32(), |e| e.raise());
            let raises = self.order_check.as_ref().map(|c| c.raises.clone()).unwrap_or_default();
            for child in children_raised_after(&raises, id, |c| ob.window(c).is_some_and(|w| w.kind == WindowKind::Child)) {
                if let Some(c) = ob.window(child) {
                    self.causes.expect_raise(child, c.pid, now);
                    let _ = c.element.bounded(budget.as_secs_f32(), |e| e.raise());
                }
            }
            if let Some(f) = self.order_check.as_mut().and_then(|c| c.fix.as_mut()) {
                f.waiting = Some((id, (now + order::PER_WINDOW).min(deadline)));
            }
            // Read back at once.
            return None;
        }
        let c = self.order_check.as_mut()?;
        let fix = c.fix.take()?;
        let focus = c.focus;
        // A focus the landing owes waits for it (the key window flies on no Desktop).
        let owed = |k: &u32| self.landing_focus.as_ref().is_some_and(|f| f.owes(*k));
        if let Some(key) = focus.filter(|k| fix.again.contains(k) && !owed(k))
            && let Some(w) = ob.window(key)
            && !self.keyboard_withheld(key, w.pid, "an order check's focus")
        {
            let now = clock::now();
            self.causes.expect_focus(key, w.pid, now);
            let _ = ax_focus(&ob.element(key).unwrap_or_else(|| w.element.clone()), w.pid);
        }
        let c = self.order_check.as_mut()?;
        if fix.late {
            let got = order::listed_order(&c.asked, &listed_ids());
            return Some(Flow::Done(got, Some(OUT_OF_TIME)));
        }
        c.calm_since = clock::now();
        // Watched again from a fresh read.
        None
    }

    /// The check ends with `got` (the order as read last) and `stopped` (why it stopped
    /// short): its wait noted ([`crate::engine::margin::wait`]), one log line when the order
    /// needed fixing.
    fn finish_order_check(&mut self, got: Vec<u32>, stopped: Option<&'static str>) {
        let Some(c) = self.order_check.take() else { return };
        let elapsed = clock::now().saturating_duration_since(c.start);
        let how = match stopped {
            Some(USER_ACTED) | Some(LATER_BATCH) => crate::engine::margin::End::Refuted,
            Some(OUT_OF_TIME) => crate::engine::margin::End::Cap,
            _ if elapsed >= order::CHECK_CAP => crate::engine::margin::End::Cap,
            _ => crate::engine::margin::End::Met,
        };
        crate::engine::margin::wait("CHECK_CAP", elapsed, order::CHECK_CAP, how);
        let Some(wrong) = c.first_wrong else { return };
        let fixed = order::to_raise_again(&c.asked, &got).is_empty();
        tracing::info!(
            asked = ?c.asked,
            got = ?wrong,
            now = ?got,
            fixed,
            fixes = c.fixes,
            why_not = stopped.unwrap_or(""),
            took_ms = elapsed.as_millis() as u64,
            "raise order: a stage's windows landed out of the order asked (back to front)"
        );
    }
}

/// The window server's on-screen normal windows, front to back, by id: layer 0, drawn, not
/// ours.
fn listed_ids() -> Vec<u32> {
    let own = std::process::id() as i32;
    windows::on_screen().into_iter().filter(|w| w.layer == 0 && w.alpha > 0.0 && w.pid != own).map(|w| w.id).collect()
}

/// The windows `raises` raised right after window `id` (where it was raised last) that are
/// children (`child`), up to the next window that is not: a window's children follow it in a
/// batch's raises ([`crate::engine::model::Model::show_stage`]).
fn children_raised_after(raises: &[u32], id: u32, child: impl Fn(u32) -> bool) -> Vec<u32> {
    let Some(at) = raises.iter().rposition(|&r| r == id) else { return Vec::new() };
    raises[at + 1..].iter().copied().take_while(|&r| child(r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::model::WindowId;

    #[test]
    fn a_windows_children_are_the_raises_right_after_it() {
        // show_stage: Mail, Safari and its sheet, Calendar (the key) and its dialog.
        let raises = [169, 172, 900, 161, 901];
        let child = |id: u32| id >= 900;
        assert_eq!(children_raised_after(&raises, 172, child), [900]);
        assert_eq!(children_raised_after(&raises, 161, child), [901]);
        assert!(children_raised_after(&raises, 169, child).is_empty());
        assert!(children_raised_after(&raises, 4, child).is_empty());
    }

    #[test]
    fn a_batch_gives_its_raises_in_order_and_its_last_focus() {
        let w = WindowId;
        let batch = [
            Action::Raise { id: w(169) },
            Action::Focus { id: w(5) },
            Action::Raise { id: w(172) },
            Action::Raise { id: w(161) },
            Action::Focus { id: w(161) },
            Action::StateChanged,
        ];
        assert_eq!(raises_and_focus(&batch), (vec![169, 172, 161], Some(161)));
        assert_eq!(raises_and_focus(&[]), (vec![], None));
    }
}
