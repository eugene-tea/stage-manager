//! The Shift-click route: windows brought into the stage in front by posted Shift-clicks on a
//! visible part of each one's thumbnail, all within one Shift press (probes of 11-16: a
//! Shift-click adds only the window clicked; the pointer away 2-15 ms a click; a press waits
//! for the settle; the clicks of one press 50 ms apart all land). The gesture itself is
//! `platform::apple::post`'s; the points are the mirror's ([`Arrangement::click_at`]).

use super::Route;
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

    /// Shift-clicks on thumbnails the mirror shows a part of, the strip shown, through the
    /// gate.
    fn can(&self, step: &Step, mirror: &Arrangement, input: &InputNow) -> Result<(), Refusal> {
        let Step::Post(Posted::Clicks { presses, .. }) = step else { return Err(Refusal("the Shift-click route only Shift-clicks".into())) };
        if mirror.strip_shown != Some(true) {
            return Err(Refusal(gate::Hold::StripCovered.why().into()));
        }
        if let Some(p) = presses.iter().find(|p| !mirror.click_at.contains_key(&p.window)) {
            return Err(Refusal(format!("window {}'s thumbnail shows no part of its own", p.window)));
        }
        gate::gate(input, mirror.settled, false).map(|_| ()).map_err(|h| Refusal(h.why().into()))
    }

    fn run(&self, step: &Step, desk: &mut dyn AppleDesk) -> Result<(), String> {
        match step {
            Step::Post(p @ Posted::Clicks { .. }) => desk.post(p),
            _ => Err("the Shift-click route only Shift-clicks".into()),
        }
    }
}
