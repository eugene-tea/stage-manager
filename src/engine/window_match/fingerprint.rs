//! Title fingerprints (`fp` in `state.json`): a window's title with what apps add or change
//! between two runs taken away, so a window reopened after a restart is still recognized when
//! its title says "— Edited", a new counter, or a browser or Terminal suffix (the restore
//! design 8D, section 3.1).
//!
//! The rules are a table ([`TITLE_RULES`]) so they can grow from what real apps show
//! without touching the logic. In order: whitespace collapsed and case folded; leading
//! marks ([`TitleRule::Prefix`]) and trailing words ([`TitleRule::Suffix`]) dropped,
//! repeatedly; only the text before the first separator kept ([`TitleRule::Split`]); then
//! one trailing copy or counter dropped ([`TitleRule::Counter`]). A rule never empties a
//! title: a step that would leave nothing is skipped. An empty title has an empty
//! fingerprint, which never matches anything.

/// What a [`TITLE_RULES`] entry does with its pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleRule {
    /// A leading mark dropped (unsaved-document marks).
    Prefix,
    /// A trailing word dropped (state the app adds to the title).
    Suffix,
    /// A separator: only the text before the first one is kept (Terminal's "name — zsh —
    /// 80×24", a browser's "Page — Browser").
    Split,
    /// A trailing copy or counter: the pattern, optionally followed by a number; the empty
    /// pattern stands for a bare number (" 2", " (2)").
    Counter,
}

/// The title rules, matched case-insensitively (patterns in lower case).
pub const TITLE_RULES: &[(TitleRule, &str)] = &[
    (TitleRule::Prefix, "• "),
    (TitleRule::Prefix, "● "),
    (TitleRule::Prefix, "* "),
    (TitleRule::Suffix, " *"),
    (TitleRule::Suffix, " — edited"),
    (TitleRule::Suffix, " – edited"),
    (TitleRule::Suffix, " - edited"),
    (TitleRule::Suffix, " (edited)"),
    (TitleRule::Suffix, " — locked"),
    (TitleRule::Split, " — "),
    (TitleRule::Split, " – "),
    (TitleRule::Split, " | "),
    (TitleRule::Split, " - "),
    (TitleRule::Counter, " copy"),
    (TitleRule::Counter, ""),
];

/// The fingerprint of `title` (see the module docs).
pub fn fingerprint(title: &str) -> String {
    let mut t = title.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let rules = |kind: TitleRule| TITLE_RULES.iter().filter(move |(k, _)| *k == kind).map(|(_, p)| *p);
    loop {
        let before = t.len();
        for p in rules(TitleRule::Prefix) {
            if let Some(rest) = t.strip_prefix(p).filter(|r| !r.trim().is_empty()) {
                t = rest.trim_start().to_string();
            }
        }
        for p in rules(TitleRule::Suffix) {
            if let Some(rest) = t.strip_suffix(p).filter(|r| !r.trim().is_empty()) {
                t = rest.trim_end().to_string();
            }
        }
        if t.len() == before {
            break;
        }
    }
    let cut = rules(TitleRule::Split).filter_map(|p| t.find(p)).filter(|&i| i > 0).min();
    if let Some(i) = cut {
        t.truncate(i);
    }
    for p in rules(TitleRule::Counter) {
        if let Some(rest) = strip_counter(&t, p) {
            t = rest;
            break;
        }
    }
    t.trim().to_string()
}

/// `t` without a trailing `pattern` and number (" copy", " copy 3"; for the empty pattern
/// " 2" or " (2)"), when that leaves something.
fn strip_counter(t: &str, pattern: &str) -> Option<String> {
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let rest = if pattern.is_empty() {
        let (head, tail) = t.rsplit_once(' ')?;
        let n = tail.strip_prefix('(').and_then(|x| x.strip_suffix(')')).unwrap_or(tail);
        digits(n).then_some(head)?
    } else if let Some(head) = t.strip_suffix(pattern) {
        head
    } else {
        let (head, tail) = t.rsplit_once(' ')?;
        (digits(tail) && head.ends_with(pattern)).then(|| &head[..head.len() - pattern.len()])?
    };
    let rest = rest.trim_end();
    (!rest.is_empty()).then(|| rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_table_on_made_up_titles() {
        let cases = [
            // Whitespace and case.
            ("  Quarterly   Plan ", "quarterly plan"),
            // Unsaved-document marks.
            ("• Notes.txt", "notes.txt"),
            ("● main.rs", "main.rs"),
            ("* scratch", "scratch"),
            ("Draft *", "draft"),
            // State the app adds.
            ("Letter — Edited", "letter"),
            ("Letter – Edited", "letter"),
            ("Letter - Edited", "letter"),
            ("Letter (Edited)", "letter"),
            ("Contract — Locked", "contract"),
            ("• Letter — Edited", "letter"),
            // Copies and counters.
            ("Untitled 2", "untitled"),
            ("Untitled (3)", "untitled"),
            ("Sketch copy", "sketch"),
            ("Sketch copy 4", "sketch"),
            ("Draft 2 — Edited", "draft"),
            // Separators: the first segment only.
            ("project — zsh — 80×24", "project"),
            ("Weather Today — Example Browser", "weather today"),
            ("main.rs — demo — Example Editor", "main.rs"),
            ("Inbox — 3 unread", "inbox"),
            ("Recipes | Example Site", "recipes"),
            ("Page - Browser", "page"),
            // A title with no separator stays.
            ("Downloads", "downloads"),
        ];
        for (title, want) in cases {
            assert_eq!(fingerprint(title), want, "{title:?}");
        }
    }

    #[test]
    fn a_rule_never_empties_a_title() {
        assert_eq!(fingerprint(""), "");
        assert_eq!(fingerprint("   "), "");
        assert_eq!(fingerprint("2"), "2", "a number alone is the title");
        assert_eq!(fingerprint("— Edited"), "— edited");
        assert_eq!(fingerprint("• "), "•");
        assert_eq!(fingerprint(" — zsh"), "— zsh", "a leading separator is no split");
        assert_eq!(fingerprint("copy"), "copy");
    }

    #[test]
    fn titles_that_differ_in_what_apps_add_share_a_fingerprint() {
        assert_eq!(fingerprint("Budget.numbers"), fingerprint("Budget.numbers — Edited"));
        assert_eq!(fingerprint("Inbox — 3 unread"), fingerprint("Inbox — 12 unread"));
        assert_ne!(fingerprint("Budget"), fingerprint("Forecast"));
    }
}
