//! Hiding whole apps instead of leaving their parked windows' slivers in the corner
//! (`[windows] hide_parked_apps`): the decision, pure.
//!
//! macOS keeps a strip of every parked window on screen. An app whose every window sits in
//! inactive stages can be hidden instead (`NSRunningApplication.hide`, as ⌘H: no animation,
//! its windows on every Desktop leave the screen), and then nothing of it shows. Once the hide
//! has taken, the windows of the stage an activation of the app would bring
//! ([`AppPark::keep`]) go back to their own frames while hidden, so that ⌘Tab or a Dock click,
//! which unhide the app natively, show them where they belong at once: no corner, no move.
//!
//! An app may be hidden only when ([`may_hide`]):
//! - the feature is on, and it is not Finder (Finder owns the desktop);
//! - its bundle id is known: every unhide of ours checks the pid still runs the bundle we
//!   hid, so an app without one could be hidden but never brought back by us;
//! - the model manages at least one of its windows, and every one of them, on every Desktop,
//!   is parked (none in an active stage, none pinned or on every Desktop): hiding is per
//!   app, across Desktops;
//! - the window server lists no other window of it (a window we do not manage: ignored by
//!   a rule, a full-screen one, one on a Desktop not visited yet) and none of its windows
//!   waits to be placed (minimized, on another Desktop);
//! - it is not hidden already (a hide of the user's stays the user's: never adopted, never
//!   undone), and it is not the active app (hiding the active app activates another);
//! - the shown Desktop's active stage has a window: Desktops are recognized by the windows on
//!   screen, and parked slivers count; with hides, the active stage's windows must carry it;
//! - macOS did not just refuse to hide it.

/// What the model says about an app's windows ([`crate::engine::model::Model::app_park`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppPark {
    /// Windows of the app the model manages, every Desktop.
    pub windows: usize,
    /// Of those, the ones shown: in an active stage, pinned, or on every Desktop.
    pub shown: usize,
    /// The windows of the stage an activation of the app would bring on the shown Desktop
    /// (its most recent inactive stage there with a window of it), with their restore frames:
    /// kept at their own frames while the app is hidden.
    pub keep: Vec<(u32, crate::engine::geometry::Rect)>,
    /// The shown Desktop's active stage has a standard window.
    pub stage_here: bool,
}

/// Everything [`may_hide`] weighs for one app.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HideFacts {
    /// `[windows] hide_parked_apps`.
    pub on: bool,
    /// The app is Finder.
    pub finder: bool,
    /// The app's bundle id is known (our unhides match the pid against it).
    pub bundle_known: bool,
    /// The model's view.
    pub park: AppPark,
    /// Windows of the app the window server lists that the model does not manage.
    pub unmanaged: usize,
    /// Windows of the app waiting to be placed (not on screen when found).
    pub waiting: usize,
    /// The app is hidden now (by the user: ours are not asked again).
    pub hidden: bool,
    /// The app is the active one.
    pub active: bool,
    /// macOS refused a hide of it lately.
    pub refused: bool,
}

/// Whether the app may be hidden, or why not (module docs).
pub fn may_hide(f: &HideFacts) -> Result<(), &'static str> {
    if !f.on {
        return Err("off");
    }
    if f.finder {
        return Err("Finder owns the desktop");
    }
    if !f.bundle_known {
        return Err("no bundle id (our unhide could not match the app)");
    }
    if f.park.windows == 0 {
        return Err("no managed window");
    }
    if f.park.shown > 0 {
        return Err("a window of it is shown");
    }
    if f.unmanaged > 0 {
        return Err("a window of it is not managed");
    }
    if f.waiting > 0 {
        return Err("a window of it waits to be placed");
    }
    if f.hidden {
        return Err("hidden already (the user's)");
    }
    if f.active {
        return Err("the active app");
    }
    if !f.park.stage_here {
        return Err("the active stage is empty (the Desktop is recognized by its windows)");
    }
    if f.refused {
        return Err("macOS refused lately");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::geometry::Rect;

    fn ok() -> HideFacts {
        HideFacts {
            on: true,
            bundle_known: true,
            park: AppPark { windows: 2, shown: 0, keep: vec![(7, Rect::new(100.0, 100.0, 800.0, 600.0))], stage_here: true },
            ..HideFacts::default()
        }
    }

    #[test]
    fn an_app_wholly_in_inactive_stages_may_be_hidden() {
        assert_eq!(may_hide(&ok()), Ok(()));
    }

    #[test]
    fn every_reason_to_keep_parking_holds_on_its_own() {
        let cases: Vec<(&str, HideFacts)> = vec![
            ("off", HideFacts { on: false, ..ok() }),
            ("Finder", HideFacts { finder: true, ..ok() }),
            ("no bundle id (never unhidden by us)", HideFacts { bundle_known: false, ..ok() }),
            ("no window", HideFacts { park: AppPark { windows: 0, ..ok().park }, ..ok() }),
            ("split across the active stage", HideFacts { park: AppPark { shown: 1, ..ok().park }, ..ok() }),
            ("an unmanaged window (full screen, ignored, another Desktop)", HideFacts { unmanaged: 1, ..ok() }),
            ("a window waiting (minimized, another Desktop)", HideFacts { waiting: 1, ..ok() }),
            ("hidden by the user", HideFacts { hidden: true, ..ok() }),
            ("the active app", HideFacts { active: true, ..ok() }),
            ("an empty active stage", HideFacts { park: AppPark { stage_here: false, ..ok().park }, ..ok() }),
            ("refused", HideFacts { refused: true, ..ok() }),
        ];
        for (name, f) in cases {
            assert!(may_hide(&f).is_err(), "{name}");
        }
    }
}
