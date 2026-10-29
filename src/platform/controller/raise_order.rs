//! The check after a batch's raises: a shown stage's windows end in the order asked, also
//! when two apps answer their `AXRaise` before the window server reorders their windows
//! ([`crate::engine::raise_order`] has the why and the rules).
//!
//! Runs after a batch that raised standard windows of two or more apps: a switch's unveil
//! (card clicks, ⌘Tab, Dock clicks; once the strip has the arrival's note,
//! [`Controller::flush_pending`]), a switch run at once, and `finish_ahead`'s raises
//! ([`Controller::execute_batch`]). The keyboard is not held up: the early focus and the
//! batch's own focus have run before it. It holds the WM thread at most
//! [`order::CHECK_CAP`], mostly [`order::WATCH`] polling a right order (longer after a fix,
//! [`order::WATCH_AFTER_FIX`]: the late app's answer to the fix's raise may land after the
//! key's), and never fights the user: a mouse button pressed since the switch began, or
//! another window brought in front, stops it at once.

use std::collections::HashSet;
use std::time::Instant;

use super::{Controller, ax_focus};
use crate::engine::model::Action;
use crate::engine::raise_order as order;
use crate::platform::observe::{Observer, WindowKind};
use crate::platform::{input, safety, windows};

/// Why a fix stopped short.
const USER_ACTED: &str = "the user clicked or brought another window forward";
const OUT_OF_TIME: &str = "out of time";
const FIXES_SPENT: &str = "fixed twice, still out of order";

/// What batch `actions` raises (in the order sent) and the window it focuses last.
pub(super) fn raises_and_focus(actions: &[Action]) -> (Vec<u32>, Option<u32>) {
    let raises = actions.iter().filter_map(|a| if let Action::Raise { id } = a { Some(id.0) } else { None }).collect();
    let focus = actions.iter().rev().find_map(|a| if let Action::Focus { id } = a { Some(id.0) } else { None });
    (raises, focus)
}

/// The user's acts that stop the check: a mouse button pressed since `since` (the switch's
/// start), or another window in front than at the check's start (`front_then`).
struct UserWatch<'a> {
    since: Instant,
    asked: &'a [u32],
    front_then: Option<u32>,
}

impl UserWatch<'_> {
    fn acted(&self, listed: &[u32]) -> bool {
        input::since_last_click() < self.since.elapsed() || order::front_moved(listed, self.asked, self.front_then)
    }
}

impl Controller {
    /// After a batch that raised `raises` (in the order sent) and focused `focus`, the user's
    /// action counting from `since`: watches the window server's order of those windows for
    /// [`order::WATCH`] ([`order::WATCH_AFTER_FIX`] once fixed), and when it is not the order
    /// asked raises the misplaced ones again, back to front, each waited for, and ends with
    /// the key's raise and focus. One log line when the order needed fixing.
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
        let start = Instant::now();
        let deadline = start + order::CHECK_CAP;
        let listed = listed_ids();
        let user = UserWatch { since, asked: &asked, front_then: listed.first().copied() };
        let mut listed = Some(listed);
        let mut first_wrong: Option<Vec<u32>> = None;
        let mut fixes = 0;
        let mut calm_since = start;
        let mut stopped: Option<&'static str> = None;
        let got = loop {
            let now_listed = listed.take().unwrap_or_else(listed_ids);
            let got = order::listed_order(&asked, &now_listed);
            let again = order::to_raise_again(&asked, &got);
            let now = Instant::now();
            if user.acted(&now_listed) {
                stopped = Some(USER_ACTED);
                break got;
            }
            if again.is_empty() {
                if now.duration_since(calm_since) >= order::calm_needed(fixes) || now >= deadline {
                    break got;
                }
                std::thread::sleep(order::POLL.min(deadline - now));
                continue;
            }
            first_wrong.get_or_insert_with(|| got.clone());
            if fixes == order::MAX_FIXES {
                stopped = Some(FIXES_SPENT);
                break got;
            }
            if now + order::POLL >= deadline {
                stopped = Some(OUT_OF_TIME);
                break got;
            }
            fixes += 1;
            if let Err(why) = self.raise_again(ob, &asked, raises, &again, focus, &user, deadline) {
                stopped = Some(why);
                break order::listed_order(&asked, &listed_ids());
            }
            calm_since = Instant::now();
        };
        let Some(wrong) = first_wrong else { return };
        let fixed = order::to_raise_again(&asked, &got).is_empty();
        tracing::info!(
            asked = ?asked,
            got = ?wrong,
            now = ?got,
            fixed,
            fixes,
            why_not = stopped.unwrap_or(""),
            took_ms = start.elapsed().as_millis() as u64,
            "raise order: a stage's windows landed out of the order asked (back to front)"
        );
    }

    /// Raises `again` (back to front) once more, each with the children the batch raised
    /// right after it (`raises`), each waited for until it shows above the windows `asked`
    /// puts below it ([`order::PER_WINDOW`] at most), then focuses `focus` as the batch did.
    /// Out of time, the windows left are skipped but the key's raise and focus still run (a
    /// window raised again over it must not stay in front); the user acting stops it all.
    #[allow(clippy::too_many_arguments)]
    fn raise_again(
        &mut self,
        ob: &Observer,
        asked: &[u32],
        raises: &[u32],
        again: &[u32],
        focus: Option<u32>,
        user: &UserWatch,
        deadline: Instant,
    ) -> Result<(), &'static str> {
        let mut late = false;
        for &id in again {
            let now = Instant::now();
            let is_key = focus == Some(id);
            if now >= deadline && !is_key {
                late = true;
                continue;
            }
            let Some(w) = ob.window(id) else { continue };
            let budget = deadline.saturating_duration_since(now).clamp(order::POLL, order::PER_WINDOW);
            self.echoes.touch_window(id, now);
            let _ = w.element.bounded(budget.as_secs_f32(), |e| e.raise());
            for child in children_raised_after(raises, id, |c| ob.window(c).is_some_and(|w| w.kind == WindowKind::Child)) {
                if let Some(c) = ob.window(child) {
                    self.echoes.touch_window(child, now);
                    let _ = c.element.bounded(budget.as_secs_f32(), |e| e.raise());
                }
            }
            let until = (now + order::PER_WINDOW).min(deadline);
            loop {
                let listed = listed_ids();
                if user.acted(&listed) {
                    return Err(USER_ACTED);
                }
                let now = Instant::now();
                if order::shows_above(id, order::below(asked, id), &order::listed_order(asked, &listed)) || now >= until {
                    break;
                }
                std::thread::sleep(order::POLL.min(until - now));
            }
        }
        if let Some(key) = focus.filter(|k| again.contains(k))
            && let Some(w) = ob.window(key)
        {
            let now = Instant::now();
            self.echoes.touch_window(key, now);
            self.echoes.touch_app(w.pid, now);
            let _ = ax_focus(&w.element, w.pid);
        }
        if late { Err(OUT_OF_TIME) } else { Ok(()) }
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
