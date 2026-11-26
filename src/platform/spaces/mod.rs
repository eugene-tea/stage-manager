//! The Spaces engine's platform half: a stage's windows hidden in a window-server Space of
//! their own (docs/ARCHITECTURE.md, "Window engines"). [`ffi`]: SkyLight's calls, loaded at
//! run time; [`engine`]: the engine; [`probe`]: the launch probe that decides whether it runs;
//! [`launch_watch`]: a just-opened app's new windows held before they show. Its pure half is
//! [`crate::engine::spaces`]. (The window server's notices, once here, are every engine's:
//! [`crate::platform::server_notices`].)

pub mod engine;
pub(crate) mod ffi;
pub(crate) mod launch_watch;
pub(crate) mod probe;
