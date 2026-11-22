//! What the ⌘Tab paths heard and did, handed to a listener when one is set.
//!
//! The ⌘Tab probe (`ctl debug-cmd-tab`, test only, in a build with the `probes` feature) sets
//! the listener for its report. With none set, as in every normal run, a note is one load and
//! its text is never built.

use std::sync::OnceLock;

static LISTENER: OnceLock<fn(&str, String)> = OnceLock::new();

/// Sets the listener every later [`note`] goes to (the first one set stays).
#[cfg_attr(not(feature = "probes"), allow(dead_code))]
pub(crate) fn listen(f: fn(&str, String)) {
    let _ = LISTENER.set(f);
}

/// Hands what a ⌘Tab path heard or did (`what`, of `kind`) to the listener; nothing (and
/// `what` not called) when none is set.
#[inline]
pub(crate) fn note(kind: &str, what: impl FnOnce() -> String) {
    if let Some(f) = LISTENER.get() {
        f(kind, what());
    }
}
