//! The Spaces engine's platform half: a stage's windows hidden in a window-server Space of
//! their own (docs/ARCHITECTURE.md, "Window engines"). [`ffi`]: SkyLight's calls, loaded at
//! run time; [`engine`]: the engine. Its pure half is [`crate::engine::spaces`].

pub mod engine;
pub(crate) mod ffi;
