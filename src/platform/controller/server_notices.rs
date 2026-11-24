//! The window server's notices on the WM thread, under the Spaces engine (round r5-notices;
//! [`crate::engine::server_notice`], [`crate::platform::spaces::notices`]). Each brings a look
//! forward that a poll makes today; what the look finds decides, exactly as the poll's look
//! decides, and the poll stays as the cap behind it.
//!
//! - **1508** (Mission Control's or App Exposé's open or close, a Desktop change's end): the
//!   look for an overview now ([`Controller::look_for_overview`], through Mission Control's
//!   step so its lends follow at once), and a short watch of fast-tick looks after it
//!   ([`crate::engine::mission_control::Hint::Server`]). One heard while an overview shows is
//!   noted ([`crate::engine::overview::OverviewWatch::told`]): its close is looked for at every
//!   fast tick, and the look that finds macOS's own window and the Dock's cover both gone ends
//!   the Space-inference freeze with no settle after.
//! - **1325 / 1326** (a window added to or taken out of a Space), not our own: the tick's
//!   check of the model against the window server now ([`Controller::spaces_reconcile`]) for a
//!   window the engine holds, lends or the model counts hidden; for a window nothing knows,
//!   its app's windows in a stage Space that nothing answers for are let out now
//!   ([`engine::SpacesEngine::let_strangers_out`]) in place of at the next tidy. Not while the
//!   two may differ for a reason ([`Controller::spaces_held_back`]): the tick checks once it
//!   may. For a window the model shows, while the window server's listing decides where the
//!   windows are (design r3): the tick's check of the Spaces now, which asks the window for
//!   its Desktop ([`Controller::desk_membership`]: moved to another Desktop by hand).
//! - **1401 / 1329** (a Desktop change's end): NSWorkspace's notice drives the Space-change
//!   path; only when none came within one fast tick of it ([`WORKSPACE_WAIT`]) is the tick's
//!   check of the Spaces brought forward, which looks at the window server's listing first
//!   ([`Controller::sync_desktops`], design r3): a Desktop change with no notice is taken
//!   within a fast tick of its end.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Controller;
use super::spaces::{STRAY_LOOK, TIDY_EVERY};
use crate::engine::margin;
use crate::engine::model::WindowId;
use crate::engine::server_notice::{Burst, Kind};
use crate::platform::clock;
use crate::platform::observe::Observer;
use crate::platform::spaces::engine;
use crate::platform::window_engine::EngineKind;
use crate::platform::windows;

/// How long after a Desktop change's notice (1401) NSWorkspace's may still come before the
/// tick's check of the Spaces is brought forward: one fast tick (the WM thread's
/// `FAST_TICK_SECONDS`). Its notice comes 1-9 ms before the flip (11-21).
pub(super) const WORKSPACE_WAIT: Duration = Duration::from_millis(20);

/// What the notices brought forward, on the WM thread, for `ctl state`.
#[derive(Debug, Default)]
pub(super) struct ServerWatch {
    /// A Desktop change's end heard at the first instant; the check is due at the second
    /// unless NSWorkspace's notice comes first.
    space_due: Option<(Instant, Instant)>,
    /// When NSWorkspace's Space notice was last heard.
    workspace_at: Option<Instant>,
    /// Looks for an overview made at a notice.
    overview_looks: u64,
    /// Checks of the model against the window server made at a membership notice, and those
    /// held back (the tick checks once it may).
    reconciles: u64,
    held_back: u64,
    /// Windows let out at a membership notice.
    strangers_out: u64,
    /// Desktop changes: the tick's check brought forward, or NSWorkspace's notice first.
    space_checks: u64,
    workspace_first: u64,
    /// Checks of the Spaces made at a membership notice about a window the model shows.
    member_checks: u64,
}

impl ServerWatch {
    /// NSWorkspace's Space notice, heard at `heard`: a Desktop change's notice waiting for it
    /// needs no check of its own.
    pub(super) fn workspace_heard(&mut self, heard: Instant) {
        self.workspace_at = Some(heard);
        if self.space_due.take().is_some() {
            self.workspace_first += 1;
        }
    }
}

impl Controller {
    /// The window server's notices of one kind (`burst`): the look each brings forward. The
    /// Spaces engine only (nothing is registered under the others).
    pub fn server_notice(&mut self, ob: &mut Observer, burst: Burst) {
        if self.engine.kind() != EngineKind::Spaces || self.model.is_none() {
            return;
        }
        let now = clock::now();
        // One read of the window list for what this burst looks at, until a write of ours.
        let _wake = windows::wake();
        match burst.kind {
            Kind::Overview => self.overview_noticed(ob, burst.at, now),
            Kind::Added | Kind::TakenOut => self.membership_noticed(ob, &burst, now),
            Kind::DesktopChanged | Kind::DesktopChangedToo => self.desktop_change_noticed(burst.at),
            Kind::Created | Kind::Visible | Kind::Invisible => {}
        }
    }

    /// 1508: the overview looked for now; its close, when one shows, looked for at every
    /// fast tick after.
    fn overview_noticed(&mut self, ob: &mut Observer, at: Instant, now: Instant) {
        if self.locked {
            return;
        }
        self.overview.told(at);
        let lends = self.mission_server_hint(now);
        self.spaces.server.overview_looks += 1;
        self.overview.look_at_notice(true);
        if lends {
            // Mission Control's step looks, and its lends follow what the look finds.
            self.mission_step(ob, true, now);
        } else if let Some(main) = self.model.as_ref().map(|m| *m.main_display()) {
            let list = windows::wake_on_screen();
            self.look_for_overview(&list, main.frame, now);
        }
        self.overview.look_at_notice(false);
    }

    /// 1325 / 1326 not our own: the check of the model against the window server now, and the
    /// strangers of the apps of windows nothing knows let out now.
    fn membership_noticed(&mut self, ob: &mut Observer, burst: &Burst, now: Instant) {
        let spaces = engine::shared();
        let Some(m) = self.model.as_ref() else { return };
        // A notice that names nothing readable may be about any window.
        let every = burst.unnamed > 0;
        let mut hidden = every;
        let mut shown = false;
        let mut unknown: Vec<u32> = Vec::new();
        for (w, _) in &burst.members {
            let counted_hidden = m.window(WindowId(*w)).is_some_and(|x| x.parked);
            if spaces.holds(*w) || spaces.lent(*w) || counted_hidden {
                hidden = true;
            } else if m.window(WindowId(*w)).is_none() {
                unknown.push(*w);
            } else {
                shown = true;
            }
        }
        if shown && self.keyed_listing().is_some() && !self.spaces_held_back(now) {
            // A window the model shows changed Spaces by something else's hand (Mission
            // Control's drag to another Desktop): where it is now is asked at once.
            self.spaces.server.member_checks += 1;
            margin::wait_noticed("SPACE_POLL", now.saturating_duration_since(self.space_checked), super::SPACE_POLL);
            self.check_spaces_now(ob);
        }
        let own = std::process::id() as i32;
        let apps: BTreeSet<i32> = windows::describe(&unknown).into_iter().filter(|w| w.pid != own && w.layer == 0).map(|w| w.pid).collect();
        if !hidden && apps.is_empty() {
            return;
        }
        if self.spaces_held_back(now) {
            self.spaces.server.held_back += 1;
            tracing::debug!(kind = burst.kind.code(), "window-server notice: the model and the window server may differ for a reason now; the tick checks once it may");
            return;
        }
        if hidden {
            self.spaces.server.reconciles += 1;
            self.spaces_reconcile(now, true);
        }
        if !every && apps.is_empty() {
            return;
        }
        let Some(m) = self.model.as_ref() else { return };
        let list = windows::wake_list();
        let list: Vec<windows::CgWindow> = list.iter().filter(|w| every || apps.contains(&w.pid)).cloned().collect();
        let out = spaces.let_strangers_out(&list, |id| m.window(WindowId(id)).is_some());
        if out > 0 {
            self.spaces.server.strangers_out += out as u64;
            margin::wait_noticed("TIDY_EVERY", now.saturating_duration_since(self.spaces_tidied()), TIDY_EVERY);
            tracing::info!(out, apps = ?apps, "window-server notice: windows nothing answered for let out of stage Spaces now, not at the next tidy");
        }
    }

    /// 1401 / 1329: the tick's check of the Spaces due one fast tick from `at`, unless
    /// NSWorkspace's notice came within one before.
    fn desktop_change_noticed(&mut self, at: Instant) {
        let s = &mut self.spaces.server;
        if s.workspace_at.is_some_and(|w| at.saturating_duration_since(w) <= WORKSPACE_WAIT) {
            s.workspace_first += 1;
            return;
        }
        if s.space_due.is_none() {
            s.space_due = Some((at, at + WORKSPACE_WAIT));
        }
    }

    /// True while a notice wants the fast tick: an overview's close told, a Desktop change's
    /// check due.
    pub(super) fn server_wants_fast(&self, now: Instant) -> bool {
        self.engine.kind() == EngineKind::Spaces && (self.overview.closing_told(now) || self.spaces.server.space_due.is_some())
    }

    /// The fast tick's share: the overview's close looked for after its notice; a Desktop
    /// change's check when NSWorkspace's notice did not come.
    pub(super) fn server_fast_tick(&mut self, ob: &mut Observer, now: Instant) {
        if self.engine.kind() != EngineKind::Spaces {
            return;
        }
        if self.overview.closing_told(now)
            && !self.locked
            && let Some(main) = self.model.as_ref().map(|m| *m.main_display())
        {
            let list = windows::wake_on_screen();
            self.overview.look_at_notice(true);
            self.look_for_overview(&list, main.frame, now);
            self.overview.look_at_notice(false);
        }
        if let Some((at, due)) = self.spaces.server.space_due
            && now >= due
        {
            self.spaces.server.space_due = None;
            self.spaces.server.space_checks += 1;
            margin::wait_noticed("SPACE_POLL", now.saturating_duration_since(at), super::SPACE_POLL);
            tracing::debug!("window-server notice: a Desktop change's end with no NSWorkspace notice yet: the Spaces checked now");
            self.check_spaces_now(ob);
        }
    }

    /// `ctl state`'s `engine.notices.uses`: what the notices brought forward.
    pub(super) fn server_status(&self) -> Value {
        let s = &self.spaces.server;
        json!({
            "overview_looks": s.overview_looks,
            "reconciles": s.reconciles,
            "held_back": s.held_back,
            "strangers_out": s.strangers_out,
            "space_checks": s.space_checks,
            "member_checks": s.member_checks,
            "workspace_first": s.workspace_first,
            "stray_look_ms": STRAY_LOOK.as_millis() as u64,
        })
    }
}
