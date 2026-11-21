//! Who caused this focus or activation: ours, the user's, the app's own or macOS's.
//!
//! macOS never says who caused a notice. One focus or activation notice passes a chain of
//! gates in `Controller::handle_event`, each added for one incident; what each notice came
//! to is noted here ([`judged`]), so the bench (docs/DEVELOPMENT.md, "The bench") can pin the
//! whole chain's answer for a scripted incident, at every load.

/// The kind of notice judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A window's focus (`AXFocusedWindowChanged`), by window id.
    Focus,
    /// An app's activation (NSWorkspace), by pid.
    Activation,
}

impl Notice {
    /// The word a trail line starts with.
    pub fn word(self) -> &'static str {
        match self {
            Self::Focus => "focus",
            Self::Activation => "activation",
        }
    }
}

/// A notice of `kind` about `id` (a window id, or a pid) came to `verdict`: one trace line,
/// and under test one line of this thread's trail ([`trail`]).
pub fn judged(kind: Notice, id: i64, verdict: &'static str) {
    tracing::trace!(kind = kind.word(), id, verdict, "notice judged");
    #[cfg(test)]
    trail::note(format!("{} {id}: {verdict}", kind.word()));
}

/// Test only: what the notices handled on this thread came to, in order (the bench reads it
/// beside what a person would see).
#[cfg(test)]
pub mod trail {
    use std::cell::RefCell;

    thread_local! {
        static TRAIL: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    /// Starts keeping this thread's trail (dropping what was kept).
    pub fn start() {
        TRAIL.with(|t| *t.borrow_mut() = Some(Vec::new()));
    }

    /// The trail since [`start`], in order, still kept.
    pub fn so_far() -> Vec<String> {
        TRAIL.with(|t| t.borrow().clone().unwrap_or_default())
    }

    /// Forgets the trail so far and keeps on (a scenario's history is not its incident).
    pub fn clear() {
        TRAIL.with(|t| {
            if let Some(kept) = t.borrow_mut().as_mut() {
                kept.clear();
            }
        });
    }

    pub(super) fn note(line: String) {
        TRAIL.with(|t| {
            if let Some(kept) = t.borrow_mut().as_mut() {
                kept.push(line);
            }
        });
    }
}
