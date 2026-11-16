//! The Apple mode's routes: each a way of getting Apple's Stage Manager to do one thing, one
//! file each behind [`Route`] (docs/STRATEGIES.md, "Routes inside Apple mode"). A2 has the
//! focus route ([`focus::Focus`]); the posted ones (Shift-click, drag) and the rejected ones
//! kept as tripwires come with A3.

pub mod focus;

use serde::Serialize;

use crate::engine::apple::desk::{AppleDesk, InputNow};
use crate::engine::apple::mirror::Arrangement;
use crate::engine::apple::plan::{Refusal, Step};

/// What a route costs, for `ctl state` and the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Cost {
    /// About how long it takes, in ms (Apple's motion after it not counted).
    pub ms: u64,
    /// It moves the pointer.
    pub pointer: bool,
}

/// A way of getting Apple's to do a step.
pub trait Route {
    /// Its name in logs and `ctl state`.
    fn name(&self) -> &'static str;
    /// What it costs.
    fn cost(&self) -> Cost;
    /// Whether it can run `step` now, against the mirror and the user's input.
    fn can(&self, step: &Step, mirror: &Arrangement, input: &InputNow) -> Result<(), Refusal>;
    /// Runs `step` on `desk`.
    fn run(&self, step: &Step, desk: &mut dyn AppleDesk) -> Result<(), String>;
}

/// The counts of one route, for `ctl state`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RouteStats {
    /// Times it ran.
    pub runs: u64,
    /// Times it was refused or failed.
    pub refused: u64,
    /// What the last run took, in ms.
    pub last_ms: u64,
}
