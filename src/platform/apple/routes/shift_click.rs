//! The Shift-click route: one window brought into the stage in front by a posted Shift-click
//! on a visible part of its thumbnail (a probe of 11-16: a Shift-click adds only the window
//! clicked; the pointer away 2-15 ms; clicks wait for the settle). The gesture itself is
//! `platform::apple::post`'s; the point is the mirror's ([`Arrangement::click_at`]).

use super::{Cost, Route};
use crate::engine::apple::desk::{AppleDesk, InputNow, Posted};
use crate::engine::apple::gate;
use crate::engine::apple::mirror::Arrangement;
use crate::engine::apple::plan::{Refusal, Step};

/// The Shift-click route.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShiftClick;

impl Route for ShiftClick {
    fn name(&self) -> &'static str {
        "shift_click"
    }

    fn cost(&self) -> Cost {
        Cost { ms: 30, pointer: true }
    }

    /// A Shift-click on a thumbnail the mirror shows a part of, the strip shown, through the
    /// gate.
    fn can(&self, step: &Step, mirror: &Arrangement, input: &InputNow) -> Result<(), Refusal> {
        let Step::Post(Posted::Click { window, .. }) = step else { return Err(Refusal("the Shift-click route only Shift-clicks".into())) };
        if mirror.strip_shown != Some(true) {
            return Err(Refusal(gate::Hold::StripCovered.why().into()));
        }
        if !mirror.click_at.contains_key(window) {
            return Err(Refusal(format!("window {window}'s thumbnail shows no part of its own")));
        }
        gate::gate(input, mirror.settled, false).map(|_| ()).map_err(|h| Refusal(h.why().into()))
    }

    fn run(&self, step: &Step, desk: &mut dyn AppleDesk) -> Result<(), String> {
        match step {
            Step::Post(p @ Posted::Click { .. }) => desk.post(p),
            _ => Err("the Shift-click route only Shift-clicks".into()),
        }
    }
}
