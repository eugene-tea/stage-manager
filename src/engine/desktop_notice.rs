//! The windows a restart brought back on another Desktop (round desktop-notice): what the
//! status menu lists, `ctl state` reports and the restore's line logs.
//!
//! After a restart macOS reopens a non-native app's windows (Electron and the like) on the
//! Desktop shown at login, not on the one they were on (the owner's 11-02 restart: Fork, VS
//! Code, Slack, Chrome and Calendar from Desktop 8 on Desktop 4). The pool (round 8I,
//! `model/pool.rs`) rebuilds their stages where they show; it cannot move a window to another
//! Desktop (no public API), so the user is told: one list for the whole restore, never a
//! notice per window ([`crate::engine::model::Model::desktop_notice`]).
//!
//! An entry leaves the list by itself once its window shows on its saved Desktop (the user
//! dragged it there in Mission Control and went there, or went there with it), when the
//! window closes or leaves the Desktop it was on, and every entry leaves at a dismissal.
//!
//! **Naming a Desktop.** Mission Control numbers Desktops ("Desktop 2"), but macOS has no
//! public way to tell which number the Desktop shown has: `com.apple.spaces` lists the
//! Desktops in order, yet its "Current Space" is not written at a switch (checked 11-03: it
//! stayed put through a dozen switches and a rewrite of the file), and the private Space calls
//! are off limits. So a Desktop is named by its number when one is known ([`DesktopRef::number`];
//! none is today), else "this Desktop" when it is the one shown, else by up to two of the
//! apps on it ("the Desktop with Terminal and Mail"), which is what Mission Control's thumbnails
//! show, else "another Desktop".

use super::model::{Pid, WindowId};

/// How many apps name a Desktop.
pub const APPS_NAMED: usize = 2;

/// The longest window title a menu entry shows, in characters (a longer one ends in "…").
pub const TITLE_MAX: usize = 60;

/// A Desktop as an entry names it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DesktopRef {
    /// The model's id for it (synthetic, as in `state.json`; never shown to the user).
    pub id: u64,
    /// Its number in Mission Control, when known (see the module docs: no public source yet).
    pub number: Option<usize>,
    /// It is the Desktop shown now on the main display.
    pub shown: bool,
    /// Up to [`APPS_NAMED`] bundle ids of apps with windows on it (other than the listed
    /// windows), most recently used first: windows managed there, else saved entries waiting
    /// there.
    pub apps: Vec<String>,
}

/// One window back on another Desktop than its saved one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The window.
    pub window: WindowId,
    /// Its app's process.
    pub pid: Pid,
    /// Its app's bundle id.
    pub bundle_id: String,
    /// Its title (shown in the menu and `ctl state`, never logged: the log takes a hash).
    pub title: String,
    /// The Desktop it was saved on.
    pub saved: DesktopRef,
    /// The Desktop it is on now.
    pub now: DesktopRef,
}

/// `d` in words: "Desktop 2", "this Desktop", "the Desktop with Terminal and Mail", "another
/// Desktop" (see the module docs); `name` gives an app's name from its bundle id.
pub fn desktop_label(d: &DesktopRef, name: impl Fn(&str) -> String) -> String {
    if let Some(n) = d.number {
        return format!("Desktop {n}");
    }
    if d.shown {
        return "this Desktop".to_string();
    }
    let names: Vec<String> = d.apps.iter().take(APPS_NAMED).map(|b| name(b)).collect();
    match names.as_slice() {
        [] => "another Desktop".to_string(),
        [one] => format!("the Desktop with {one}"),
        [one, two, ..] => format!("the Desktop with {one} and {two}"),
    }
}

/// A name for an app that is not running, from its bundle id: its last part
/// ("com.example.Term" → "Term").
pub fn fallback_name(bundle: &str) -> String {
    bundle.rsplit('.').find(|p| !p.is_empty()).unwrap_or(bundle).to_string()
}

/// `title` cut to [`TITLE_MAX`] characters, ending in "…" when cut.
pub fn short_title(title: &str) -> String {
    if title.chars().count() <= TITLE_MAX {
        return title.to_string();
    }
    title.chars().take(TITLE_MAX - 1).collect::<String>() + "…"
}

/// The menu entry's title: the app's name and the window's title ("Fork — deep-house"), or
/// the app's name alone for an untitled window.
pub fn entry_title(e: &Entry, app: &str) -> String {
    let title = e.title.trim();
    if title.is_empty() { app.to_string() } else { format!("{app} — {}", short_title(title)) }
}

/// The menu entry's second line: where the window belongs and where it is ("Belongs on the
/// Desktop with Terminal; now on this Desktop").
pub fn entry_detail(e: &Entry, name: impl Fn(&str) -> String) -> String {
    format!("Belongs on {}; now on {}", desktop_label(&e.saved, &name), desktop_label(&e.now, &name))
}

/// The list as the restore's line ends it, titles through `title` (a hash unless `[log]
/// titles` is on): "; on another Desktop: com.example.ide #1a2b3c4d (saved 4, now 8), …".
/// Empty when nothing is listed. The Desktops are the model's ids, as every log line has them.
pub fn log_list(entries: &[Entry], title: impl Fn(&str) -> String) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let items: Vec<String> = entries
        .iter()
        .map(|e| format!("{} {} (saved {}, now {})", e.bundle_id, title(&e.title), e.saved.id, e.now.id))
        .collect();
    format!("; on another Desktop: {}", items.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(b: &str) -> String {
        b.rsplit('.').next().unwrap_or(b).to_string()
    }

    fn d(id: u64, shown: bool, apps: &[&str]) -> DesktopRef {
        DesktopRef { id, number: None, shown, apps: apps.iter().map(|s| s.to_string()).collect() }
    }

    fn entry(title: &str) -> Entry {
        Entry {
            window: WindowId(41),
            pid: 20,
            bundle_id: "com.example.Git".into(),
            title: title.into(),
            saved: d(8, false, &["com.example.Term"]),
            now: d(4, true, &["com.example.Mail"]),
        }
    }

    #[test]
    fn a_desktop_is_named_by_its_number_when_known_else_shown_else_by_its_apps() {
        let numbered = DesktopRef { number: Some(2), ..d(8, true, &["com.example.Term"]) };
        assert_eq!(desktop_label(&numbered, name), "Desktop 2", "Mission Control's number first");
        assert_eq!(desktop_label(&d(4, true, &["com.example.Mail"]), name), "this Desktop");
        assert_eq!(desktop_label(&d(8, false, &["com.example.Term"]), name), "the Desktop with Term");
        assert_eq!(
            desktop_label(&d(8, false, &["com.example.Term", "com.example.Mail", "com.example.Web"]), name),
            "the Desktop with Term and Mail",
            "two apps at most"
        );
        assert_eq!(desktop_label(&d(8, false, &[]), name), "another Desktop");
        assert_eq!(fallback_name("com.example.Term"), "Term");
        assert_eq!(fallback_name("Term"), "Term");
    }

    #[test]
    fn an_entry_reads_app_title_then_where_it_belongs_and_where_it_is() {
        let e = entry("deep-house");
        assert_eq!(entry_title(&e, "Git"), "Git — deep-house");
        assert_eq!(entry_title(&entry("  "), "Git"), "Git", "an untitled window: the app alone");
        assert_eq!(entry_detail(&e, name), "Belongs on the Desktop with Term; now on this Desktop");
        let long = entry(&"x".repeat(200));
        let t = entry_title(&long, "Git");
        assert!(t.ends_with('…') && t.chars().count() == "Git — ".chars().count() + TITLE_MAX, "{t}");
    }

    #[test]
    fn the_log_list_hashes_titles_and_names_desktops_by_id() {
        assert_eq!(log_list(&[], |t| t.to_string()), "");
        let line = log_list(&[entry("Secret plans"), entry("Other")], |_| "#1a2b3c4d".to_string());
        assert_eq!(
            line,
            "; on another Desktop: com.example.Git #1a2b3c4d (saved 8, now 4), com.example.Git #1a2b3c4d (saved 8, now 4)"
        );
        assert!(!line.contains("Secret"));
    }
}
