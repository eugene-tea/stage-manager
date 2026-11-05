//! The windows a restart brought back on another Desktop (round desktop-notice), between the
//! model ([`crate::engine::model::Model::desktop_notice`]) and the status menu: the lines sent
//! to the main thread whenever they change, `ctl state`'s view, and the menu's two actions
//! (bring a listed window forward, dismiss the list).
//!
//! The list shows only after the restore window of a restart has ended (never while apps are
//! still opening at the login) and with `[stages] restore_notice` on (the default).

use serde_json::{Value, json};

use super::{Controller, End, UiUpdate, notify_ui};
use crate::engine::desktop_notice::{self, Entry};
use crate::engine::model::WindowId;
use crate::platform::logging;
use crate::platform::observe::Observer;
use crate::ui::status_menu::NoticeLine;

/// What the menu (or `ctl debug-notice`) asks of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeCommand {
    /// Bring the listed window with this id forward (its stage with it).
    Show(u32),
    /// Dismiss the whole list.
    Dismiss,
}

impl Controller {
    /// True when the list may show: `[stages] restore_notice` on, after a restart, the restore
    /// window over.
    fn notice_due(&self) -> bool {
        self.store.config().stages.restore_notice && self.model.as_ref().is_some_and(|m| m.after_restart() && !m.restoring())
    }

    /// Forgets the entries no longer away and sends the menu's lines to the main thread when
    /// they changed (none while the list may not show). App names from the running apps.
    pub(super) fn publish_notice(&mut self, ob: &Observer) {
        let due = self.notice_due();
        let Some(m) = self.model.as_mut() else { return };
        if m.tidy_desktop_notice() {
            tracing::debug!("desktop notice: a window is back on its own Desktop, or gone: off the list");
        }
        let entries = if due { m.desktop_notice() } else { Vec::new() };
        let name = |b: &str| app_name(ob, b);
        let lines: Vec<NoticeLine> = entries
            .iter()
            .map(|e| NoticeLine {
                window: e.window.0,
                title: desktop_notice::entry_title(e, &name(&e.bundle_id)),
                detail: desktop_notice::entry_detail(e, name),
            })
            .collect();
        let before = self.notice_sent.as_deref().unwrap_or_default();
        if before == lines.as_slice() {
            return;
        }
        if before.is_empty() {
            tracing::info!(windows = lines.len(), "desktop notice: windows back on another Desktop listed in the menu");
        } else if lines.is_empty() {
            tracing::info!("desktop notice: the list is empty");
        }
        self.notice_sent = Some(lines.clone());
        notify_ui(UiUpdate::DesktopNotice(lines));
    }

    /// Carries out `cmd` (a menu click, a user action): bring a listed window forward, or
    /// dismiss the list.
    pub fn notice_command(&mut self, ob: &mut Observer, cmd: NoticeCommand) -> Value {
        self.end_restore(ob, "the desktop notice", End::User);
        let reply = match cmd {
            NoticeCommand::Dismiss => {
                if let Some(m) = self.model.as_mut() {
                    m.dismiss_desktop_notice();
                }
                tracing::info!("desktop notice: dismissed");
                json!({ "ok": true })
            }
            NoticeCommand::Show(id) => {
                if let Some(why) = self.frozen_reason() {
                    return json!({ "ok": false, "error": why });
                }
                let obs = self.observe(ob);
                let Some(m) = self.model.as_mut() else { return json!({ "ok": false, "error": "not managing" }) };
                let actions = m.show_noticed(WindowId(id), &obs);
                let count = actions.len();
                tracing::info!(id, actions = count, "desktop notice: a listed window brought forward");
                self.run_batch(ob, actions, true);
                json!({ "ok": true, "actions": count })
            }
        };
        self.publish_notice(ob);
        reply
    }

    /// `ctl state`'s view: whether the list may show, every entry (titles included, as
    /// `ctl state` has them) and the menu's lines as last sent.
    pub(super) fn notice_json(&self) -> Value {
        let entries: Vec<Entry> = self.model.as_ref().map(|m| m.desktop_notice()).unwrap_or_default();
        let fallback = desktop_notice::fallback_name;
        json!({
            "on": self.store.config().stages.restore_notice,
            "due": self.notice_due(),
            "entries": entries.iter().map(|e| json!({
                "id": e.window.0,
                "pid": e.pid,
                "bundle_id": e.bundle_id,
                "title": e.title,
                "saved_desktop": e.saved.id,
                "now_desktop": e.now.id,
                "saved": desktop_notice::desktop_label(&e.saved, fallback),
                "now": desktop_notice::desktop_label(&e.now, fallback),
            })).collect::<Vec<_>>(),
            "menu": self.notice_sent.as_ref().map(|l| l.iter().map(|x| json!({ "id": x.window, "title": x.title, "detail": x.detail })).collect::<Vec<_>>()),
        })
    }

    /// The list for the restore's line ([`desktop_notice::log_list`]), titles hashed unless
    /// `[log] titles` is on.
    pub(super) fn notice_log(&self) -> String {
        let entries = self.model.as_ref().map(|m| m.desktop_notice()).unwrap_or_default();
        desktop_notice::log_list(&entries, logging::title)
    }
}

/// A running app's name, else a name made from its bundle id.
fn app_name(ob: &Observer, bundle: &str) -> String {
    ob.app_name(bundle).map_or_else(|| desktop_notice::fallback_name(bundle), str::to_string)
}
