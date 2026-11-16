//! What a step of ours does in the Apple mode: an [`Intent`] against the mirror gives the
//! [`Step`]s a route carries out, or a [`Refusal`] with its reason. Apple's answers; after
//! every step the mirror is read again and the model follows what is there, whether the step
//! did what was planned or not.
//!
//! A2 plans the switch: the focus route (raise, main, app frontmost) needs only a known window
//! of the stage, works with the strip hidden and for a stage no longer listed, and moves no
//! pointer. The posted routes (bring here, send out) come with A3.

use serde::Serialize;

use super::desk::Posted;
use super::mirror::Arrangement;

/// What was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Stage N in front (1-based: the strip's slots top to bottom, then the remembered ones).
    Switch(usize),
}

/// One step of a plan.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum Step {
    /// Window focused (the focus route).
    Focus(u32),
    /// A gesture posted (A3).
    Post(Posted),
    /// Wait until the strip shows item `slot` (A3).
    AwaitStrip(usize),
    /// Wait until the arrangement is settled.
    AwaitSettle,
}

/// Why an intent is not carried out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Refusal(pub String);

/// The plan for switching to stage `n` of arrangement `a`: focus its key window (`key`: the
/// one the model remembers of the group's windows, else its front one), once the arrangement
/// is settled. Stage `n` in front already, or none: refused.
pub fn switch(a: &Arrangement, n: usize, key: impl Fn(&[u32]) -> Option<u32>) -> Result<Vec<Step>, Refusal> {
    let Some(group) = a.by_number(n) else {
        let count = a.strip.len() + a.remembered.len();
        return Err(Refusal(format!("no stage {n}: Apple's shows {count} besides the one in front")));
    };
    let Some(window) = key(&group.windows).filter(|w| group.windows.contains(w)).or_else(|| group.windows.first().copied()) else {
        return Err(Refusal(format!("stage {n} has no window")));
    };
    let mut steps = Vec::new();
    if !a.settled {
        steps.push(Step::AwaitSettle);
    }
    steps.push(Step::Focus(window));
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::apple::mirror::Group;
    use crate::engine::geometry::Rect;

    fn arrangement(settled: bool) -> Arrangement {
        let g = |w: &[u32]| Group { windows: w.to_vec(), frame: Rect::new(15.0, 100.0, 79.0, 104.0) };
        Arrangement { current: vec![4], strip: vec![g(&[3]), g(&[1, 2])], remembered: vec![g(&[7])], settled, ..Arrangement::default() }
    }

    #[test]
    fn a_switch_focuses_the_stages_key_window_counting_the_strip_then_the_remembered() {
        let a = arrangement(true);
        assert_eq!(switch(&a, 1, |_| None), Ok(vec![Step::Focus(3)]));
        assert_eq!(switch(&a, 2, |_| Some(2)), Ok(vec![Step::Focus(2)]), "the model's key window");
        assert_eq!(switch(&a, 2, |_| Some(9)), Ok(vec![Step::Focus(1)]), "a key of another group: the front one");
        assert_eq!(switch(&a, 3, |_| None), Ok(vec![Step::Focus(7)]), "a remembered stage: focus needs only a known window");
        assert!(switch(&a, 4, |_| None).is_err());
        assert!(switch(&a, 0, |_| None).is_err());
    }

    #[test]
    fn a_switch_mid_motion_waits_for_the_settle() {
        assert_eq!(switch(&arrangement(false), 1, |_| None), Ok(vec![Step::AwaitSettle, Step::Focus(3)]));
    }
}
