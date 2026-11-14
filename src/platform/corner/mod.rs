//! The Corner engine's platform half: the engine itself ([`engine::CornerEngine`], hides and
//! shows through Accessibility) and the inert-slivers tap ([`sliver_tap`]: presses on the
//! slivers its hidden windows leave showing). Its pure half is [`crate::engine::corner`];
//! what the controller keeps up around it on the WM thread is `controller/corner/`; bringing
//! everything back is [`super::safety`].

pub mod engine;
pub mod sliver_tap;
