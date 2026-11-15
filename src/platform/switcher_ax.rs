//! ⌘Tab's switcher as the Dock's Accessibility shows it (round cmd-tab): which app it
//! selects, read on a switcher key only, never polled.
//!
//! The ⌘Tab switcher is the Dock's UI. Whether the Dock lists it among its Accessibility
//! children, and how it marks the selection, is not documented: [`read`] looks for a child of
//! the Dock's application element whose role or subrole names a switcher (one public source,
//! the Hammerspoon and AltTab discussions, gives the subrole `AXProcessSwitcherList`), and
//! takes its selection from `AXSelectedChildren`, else the item whose `AXSelected` is true.
//! An item names its app by `AXURL` (as the Dock's own items do), else by its title, matched
//! to one running app's name. [`dump`] (`ctl debug-cmd-tab`'s probe) prints what the Dock
//! shows two levels down, so the owner's run can tell whether any of this holds.
//!
//! Every call runs at [`SWITCHER_AX_SECONDS`]; the first call the Dock leaves unanswered ends
//! the read.

use objc2_app_kit::NSWorkspace;
use serde_json::{Value, json};

use super::ax::{AxElement, AxError, AxResult};
use super::dock;

/// Each Accessibility call to the Dock for the switcher (seconds): the selection is wanted
/// before ⌘'s release, a Dock slower than this is no help.
pub const SWITCHER_AX_SECONDS: f32 = 0.05;

/// `AXSelectedChildren`.
const SELECTED_CHILDREN: &str = "AXSelectedChildren";
/// `AXSelected`.
const SELECTED: &str = "AXSelected";

/// What the Dock's Accessibility showed of the switcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    /// No child of the Dock looks like the switcher (not up yet, gone, or not exposed).
    NoSwitcher,
    /// The Dock left a call unanswered within [`SWITCHER_AX_SECONDS`].
    Unanswered,
    /// The switcher, with `items` apps; `selected` the one it selects, when it says.
    Shown {
        /// How many items it lists.
        items: usize,
        /// The item selected, when the Dock marks one.
        selected: Option<Item>,
    },
}

/// The item the switcher selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Its place in the list (0: the app in front).
    pub index: Option<usize>,
    /// Its app, when its `AXURL` or its title names exactly one running app.
    pub pid: Option<i32>,
    /// How it was matched: "url", "title", or "unmatched".
    pub matched: &'static str,
}

impl Read {
    /// The app selected, when the Dock named one.
    pub fn pid(&self) -> Option<i32> {
        match self {
            Read::Shown { selected: Some(item), .. } => item.pid,
            _ => None,
        }
    }

    /// For the log: what the read found.
    pub fn name(&self) -> &'static str {
        match self {
            Read::NoSwitcher => "no switcher in the Dock's Accessibility",
            Read::Unanswered => "the Dock did not answer",
            Read::Shown { selected: None, .. } => "a switcher with no selection",
            Read::Shown { selected: Some(Item { pid: None, .. }), .. } => "a selection that names no running app",
            Read::Shown { selected: Some(_), .. } => "the switcher's selection",
        }
    }
}

/// True when a Dock child with this `role` and `subrole` is the switcher: either names a
/// switcher (`AXProcessSwitcherList` and the like).
pub fn is_switcher(role: &str, subrole: &str) -> bool {
    let named = |s: &str| s.to_ascii_lowercase().contains("switcher");
    named(role) || named(subrole)
}

/// The switcher's selection (`dock`: the Dock's pid), read now.
pub fn read(dock: i32) -> Read {
    let t = SWITCHER_AX_SECONDS;
    let app = AxElement::application(dock);
    let children = match app.bounded(t, AxElement::children) {
        Ok(c) => c,
        Err(AxError::CANNOT_COMPLETE) => return Read::Unanswered,
        Err(_) => return Read::NoSwitcher,
    };
    for child in children {
        let kind = child.bounded(t, |e| (e.role(), e.subrole()));
        let (role, subrole) = match kind {
            (Err(AxError::CANNOT_COMPLETE), _) | (_, Err(AxError::CANNOT_COMPLETE)) => return Read::Unanswered,
            (role, subrole) => (role.unwrap_or_default(), subrole.unwrap_or_default()),
        };
        if !is_switcher(&role, &subrole) {
            continue;
        }
        return match selection(&child) {
            Ok(read) => read,
            Err(AxError::CANNOT_COMPLETE) => Read::Unanswered,
            Err(_) => Read::Shown { items: 0, selected: None },
        };
    }
    Read::NoSwitcher
}

/// The selection of the switcher's list `list`.
fn selection(list: &AxElement) -> AxResult<Read> {
    let t = SWITCHER_AX_SECONDS;
    let items = list.bounded(t, AxElement::children)?;
    let chosen = match list.bounded(t, |e| e.elements(SELECTED_CHILDREN)) {
        Ok(sel) if !sel.is_empty() => sel.into_iter().next(),
        Err(AxError::CANNOT_COMPLETE) => return Err(AxError::CANNOT_COMPLETE),
        _ => {
            let mut found = None;
            for item in &items {
                match item.bounded(t, |e| e.flag(SELECTED)) {
                    Ok(true) => {
                        found = Some(item.clone());
                        break;
                    }
                    Err(AxError::CANNOT_COMPLETE) => return Err(AxError::CANNOT_COMPLETE),
                    _ => {}
                }
            }
            found
        }
    };
    let Some(chosen) = chosen else { return Ok(Read::Shown { items: items.len(), selected: None }) };
    let index = items.iter().position(|i| *i == chosen);
    let path = chosen.bounded(t, AxElement::url_path).ok();
    let title = chosen.bounded(t, AxElement::title).ok();
    let (pid, matched) = app_of(path.as_deref(), title.as_deref());
    Ok(Read::Shown { items: items.len(), selected: Some(Item { index, pid, matched }) })
}

/// The running app an item names: its bundle path (`AXURL`), else its title as exactly one
/// running app's name.
fn app_of(path: Option<&str>, title: Option<&str>) -> (Option<i32>, &'static str) {
    if let Some(path) = path
        && let Some(pid) = dock::running_by_path().get(path.trim_end_matches('/')).copied()
    {
        return (Some(pid), "url");
    }
    if let Some(title) = title {
        let names: Vec<(String, i32)> = NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .filter_map(|a| Some((a.localizedName()?.to_string(), a.processIdentifier())))
            .collect();
        if let Some(pid) = only_named(&names, title) {
            return (Some(pid), "title");
        }
    }
    (None, "unmatched")
}

/// The one app named `title` among `names` (`None` for none or several).
fn only_named(names: &[(String, i32)], title: &str) -> Option<i32> {
    let mut found = names.iter().filter(|(n, _)| n == title).map(|(_, pid)| *pid);
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// What the Dock shows to Accessibility, two levels down (the probe's dump): each child's
/// role, subrole, title and attribute names; for each, up to `items` of its own children
/// with role, subrole, title, `AXSelected` and `AXURL`. Titles are app names; the dump is
/// printed by the probe, never logged.
pub fn dump(dock: i32, items: usize) -> Value {
    let t = 0.1;
    let app = AxElement::application(dock);
    let children = match app.bounded(t, AxElement::children) {
        Ok(c) => c,
        Err(e) => return json!({ "error": e.to_string() }),
    };
    let describe = |e: &AxElement| {
        e.bounded(t, |e| {
            json!({
                "role": e.role().ok(),
                "subrole": e.subrole().ok(),
                "title": e.title().ok(),
            })
        })
    };
    let lists: Vec<Value> = children
        .iter()
        .map(|child| {
            let mut v = describe(child);
            let attributes = child.bounded(t, AxElement::attribute_names).unwrap_or_default();
            let selected_children = child.bounded(t, |e| e.elements(SELECTED_CHILDREN)).map(|s| s.len()).ok();
            let kids = child.bounded(t, AxElement::children).unwrap_or_default();
            let first_attributes = kids.first().map(|k| k.bounded(t, AxElement::attribute_names).unwrap_or_default());
            let shown: Vec<Value> = kids
                .iter()
                .take(items)
                .map(|k| {
                    let mut kv = describe(k);
                    kv["selected"] = json!(k.bounded(t, |e| e.flag(SELECTED)).ok());
                    kv["url"] = json!(k.bounded(t, AxElement::url_path).ok());
                    kv
                })
                .collect();
            v["attributes"] = json!(attributes);
            v["selected_children"] = json!(selected_children);
            v["children"] = json!(kids.len());
            v["first_child_attributes"] = json!(first_attributes);
            v["items"] = json!(shown);
            v
        })
        .collect();
    json!({ "children": lists.len(), "lists": lists })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_naming_a_switcher_is_the_switcher() {
        assert!(is_switcher("AXList", "AXProcessSwitcherList"));
        assert!(is_switcher("AXProcessSwitcher", ""));
        assert!(is_switcher("AXGroup", "AXAppSwitcher"));
        assert!(!is_switcher("AXList", ""), "the Dock's own bar");
        assert!(!is_switcher("AXList", "AXSectionList"));
    }

    #[test]
    fn a_title_names_an_app_only_when_one_app_has_it() {
        let names = vec![("Mail".to_string(), 10), ("Notes".to_string(), 20), ("Notes".to_string(), 21)];
        assert_eq!(only_named(&names, "Mail"), Some(10));
        assert_eq!(only_named(&names, "Notes"), None, "two apps of that name");
        assert_eq!(only_named(&names, "Chat"), None);
    }

    #[test]
    fn a_read_names_its_app_only_with_a_selection() {
        let item = |pid| Some(Item { index: Some(1), pid, matched: "url" });
        assert_eq!(Read::Shown { items: 4, selected: item(Some(7)) }.pid(), Some(7));
        assert_eq!(Read::Shown { items: 4, selected: item(None) }.pid(), None);
        assert_eq!(Read::Shown { items: 4, selected: None }.pid(), None);
        assert_eq!(Read::NoSwitcher.pid(), None);
        assert_eq!(Read::Unanswered.pid(), None);
        assert_eq!(Read::Shown { items: 4, selected: item(None) }.name(), "a selection that names no running app");
    }
}
