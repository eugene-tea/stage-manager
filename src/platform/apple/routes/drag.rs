//! The drag route: a window in the stage in front taken out to a stage of its own by a posted
//! drag of its title bar onto the strip (a probe of 11-16; it works with the strip covered, a
//! window of the stage over the strip's column: the release there took the dragged window
//! into the hidden strip). The gesture is `platform::apple::post`'s; the points are
//! `engine::apple::points`'.

use super::{Cost, Route};
use crate::engine::apple::desk::{AppleDesk, InputNow, Posted};
use crate::engine::apple::gate;
use crate::engine::apple::mirror::{Arrangement, Where};
use crate::engine::apple::plan::{Refusal, Step};

/// The drag route.
#[derive(Debug, Clone, Copy, Default)]
pub struct Drag;

impl Route for Drag {
    fn name(&self) -> &'static str {
        "drag"
    }

    fn cost(&self) -> Cost {
        Cost { ms: 600, pointer: true }
    }

    /// A drag of a window in front with another beside it, through the gate (the strip may
    /// be covered).
    fn can(&self, step: &Step, mirror: &Arrangement, input: &InputNow) -> Result<(), Refusal> {
        let Step::Post(Posted::Drag { window, .. }) = step else { return Err(Refusal("the drag route only drags".into())) };
        if mirror.where_is(*window).map(|(w, _)| w) != Some(Where::Current) {
            return Err(Refusal(format!("window {window} is not in the stage in front")));
        }
        if mirror.current.len() < 2 {
            return Err(Refusal(format!("window {window} is alone in its stage already")));
        }
        gate::gate(input, mirror.settled, false).map(|_| ()).map_err(|h| Refusal(h.why().into()))
    }

    fn run(&self, step: &Step, desk: &mut dyn AppleDesk) -> Result<(), String> {
        match step {
            Step::Post(p @ Posted::Drag { .. }) => desk.post(p),
            _ => Err("the drag route only drags".into()),
        }
    }
}
