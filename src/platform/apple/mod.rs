//! The Apple mode's platform half (docs/STRATEGIES.md, "Apple-augmented"): while Apple's
//! Stage Manager is on and `[windows] with_apple = "augment"`, the app hides nothing
//! ([`engine::AppleEngine`]), reads Apple's strip ([`strip_ax`], the window list) into the
//! pure mirror ([`crate::engine::apple::mirror`]), keeps its own model in step
//! ([`crate::engine::apple::follow`]) and acts through routes Apple's answers to
//! ([`routes`]). [`controller::AppleController`] is the conductor of this mode, beside the
//! app's own [`super::controller::Controller`] (`platform::conductor`).

pub mod controller;
pub mod desk;
pub mod engine;
pub mod launch_hide;
pub mod post;
pub mod routes;
pub mod strip_ax;
