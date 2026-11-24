//! The focus route: a window raised, made main and its app frontmost through Accessibility;
//! Apple's Stage Manager brings its stage in front (a probe of 11-16: 5-7 ms, no pointer, the
//! strip hidden or not, any stage whose window is known, one no longer listed too).

use super::Route;
use crate::engine::apple::desk::{AppleDesk, InputNow};
use crate::engine::apple::mirror::Arrangement;
use crate::engine::apple::plan::{Refusal, Step};

/// The focus route.
#[derive(Debug, Clone, Copy, Default)]
pub struct Focus;

impl Route for Focus {
    fn name(&self) -> &'static str {
        "focus"
    }

    /// A focus of a window the mirror knows; while the button is down it waits (a focus then
    /// would end the user's drag elsewhere).
    fn can(&self, step: &Step, mirror: &Arrangement, input: &InputNow) -> Result<(), Refusal> {
        let Step::Focus(window) = step else { return Err(Refusal("the focus route only focuses".into())) };
        if mirror.where_is(*window).is_none() {
            return Err(Refusal(format!("window {window} is in no stage Apple's shows or showed")));
        }
        if input.button_down {
            return Err(Refusal("the mouse button is down".into()));
        }
        Ok(())
    }

    fn run(&self, step: &Step, desk: &mut dyn AppleDesk) -> Result<(), String> {
        match step {
            Step::Focus(window) => desk.focus(*window),
            _ => Err("the focus route only focuses".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::apple::desk::InputNow;
    use crate::engine::apple::mirror::{Arrangement, fold};
    use crate::engine::testkit::apple::FakeWindowManager;
    use crate::engine::testkit::display;

    fn settled(f: &mut FakeWindowManager, a: &Arrangement) -> Arrangement {
        let mut a = a.clone();
        for _ in 0..60 {
            f.advance(50);
            a = fold(&a, Some(&f.reading()), &f.listed(), &f.window_ids(), &display(), f.now());
            if a.settled && !f.busy() {
                return a;
            }
        }
        panic!("never settled");
    }

    #[test]
    fn the_focus_route_switches_apples_stage_with_no_pointer_and_no_post() {
        let mut f = FakeWindowManager::new();
        f.open_app(10, &[1, 2]);
        f.open_app(20, &[3]);
        let a = settled(&mut f, &Arrangement::default());
        let step = Step::Focus(1);
        assert_eq!(Focus.can(&step, &a, &InputNow::default()), Ok(()));
        Focus.run(&step, &mut f).expect("focused");
        assert_eq!(f.focused, vec![1]);
        assert!(f.posted.is_empty(), "no gesture posted");
        let b = settled(&mut f, &a);
        assert_eq!(b.current, vec![1, 2], "Apple's brought its stage in front");
    }

    /// The switch end to end over the fake: the plan for stage 2 of Apple's strip, the focus
    /// route, Apple's motion read until settled, the model following; then stage 1, a stage
    /// past the strip's room (remembered), switched to the same way.
    #[test]
    fn a_switch_by_number_runs_the_focus_route_and_the_model_follows_apples_answer() {
        use crate::engine::apple::{follow, plan};
        use crate::engine::model::{ModelOptions, WindowId, WindowMeta};
        use crate::engine::recovery::EngineKind;
        use crate::engine::testkit::{meta, model_with};
        use std::collections::BTreeMap;

        let mut f = FakeWindowManager::new();
        f.open_app(10, &[1, 2]);
        f.open_app(20, &[3]);
        f.open_app(30, &[4]);
        let mut a = settled(&mut f, &Arrangement::default());
        let mut m = model_with(ModelOptions { engine: EngineKind::Apple, ..ModelOptions::default() });
        let metas = |f: &FakeWindowManager| -> BTreeMap<u32, WindowMeta> { f.window_ids().into_iter().map(|id| (id, WindowMeta { frame: f.frame(id).unwrap_or_default(), ..meta(id, 10, "com.example.app") })).collect() };
        follow::follow(&mut m, &a, &metas(&f), Some(4));

        let run = |f: &mut FakeWindowManager, a: &mut Arrangement, m: &mut crate::engine::model::Model, n: usize| {
            let key = |w: &[u32]| m.stage_of(WindowId(*w.first()?)).and_then(|s| m.stage(s)?.key).map(|k| k.0);
            let steps = plan::switch(a, n, key).expect("planned");
            let [Step::Focus(window)] = steps.as_slice() else { panic!("one focus at rest: {steps:?}") };
            Focus.can(&steps[0], a, &InputNow::default()).expect("the route can");
            Focus.run(&steps[0], f).expect("focused");
            a.expect(crate::engine::apple::job::answer(&steps[0], a).expect("a focus is answered"), f.now());
            *a = settled(f, a);
            follow::follow(m, a, &metas(f), Some(*window));
            *window
        };
        let w = run(&mut f, &mut a, &mut m, 2);
        assert!([1, 2].contains(&w), "stage 2 is the app with windows 1 and 2 (its key window focused: {w})");
        let mut front = a.current.clone();
        front.sort_unstable();
        assert_eq!(front, vec![1, 2]);
        assert_eq!(m.main_active_stage(), m.stage_of(WindowId(1)), "the model follows Apple's answer");
        assert!(f.posted.is_empty(), "no pointer, no posted input");

        f.set_room(1);
        a = settled(&mut f, &a);
        follow::follow(&mut m, &a, &metas(&f), Some(1));
        assert_eq!(a.remembered.len(), 1, "a stage past the strip's room");
        let w = run(&mut f, &mut a, &mut m, 2);
        assert!(f.current().contains(&w), "a remembered stage is switched to by focus: window {w}");
        assert_eq!(m.main_active_stage(), m.stage_of(WindowId(w)));
        assert_eq!(f.focused.len(), 2);
    }

    #[test]
    fn the_focus_route_refuses_an_unknown_window_and_waits_out_a_press() {
        let a = Arrangement { current: vec![4], ..Arrangement::default() };
        assert!(Focus.can(&Step::Focus(9), &a, &InputNow::default()).is_err());
        assert!(Focus.can(&Step::Focus(4), &a, &InputNow { button_down: true, ..InputNow::default() }).is_err());
        assert!(Focus.can(&Step::AwaitSettle, &a, &InputNow::default()).is_err());
    }
}
