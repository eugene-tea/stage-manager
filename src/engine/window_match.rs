//! Which saved window a live window is, decided one way for every safety path (the review of
//! 1.1.0, Grok High 2 and architecture 4): the recovery scan, quit's leftovers and the
//! guardian's scan all go through [`match_windows`].
//!
//! In order, each saved window used once:
//!
//! 1. **The same window**: its `CGWindowID` and its app's bundle id (ids outlive our app, as
//!    the windows are other apps'; a saved id is only given when it was saved in this window
//!    server session, [`super::persist::StateFile::for_session`]).
//! 2. **The same app and title**: bundle id and an exact title that is not empty (an untitled
//!    window says nothing about which one it is).
//! 3. **The only candidate**: the bundle id alone, and only when exactly one saved window of
//!    that app is left unmatched and exactly one live window of that app still wants one.
//!    Two windows of one app whose titles changed are never handed each other's frames:
//!    without a match they are centered.
//!
//! Every live window takes part in the first two passes, so a window still on screen keeps
//! its own saved entry from a stranded sibling; only the windows the caller says want a frame
//! take part in the third.
//!
//! Restoring stages ([`super::persist::rematch_on`] at an arrival, the model's pending stages
//! for each window appearing later) uses the scored matcher instead ([`score`]): it weighs
//! the frame and the title's fingerprint ([`fingerprint`]) too, and says how sure it is, since
//! a match there may park a window.

mod fingerprint;
mod score;

pub use fingerprint::{TITLE_RULES, TitleRule, fingerprint};
pub use score::{AMBIGUOUS_WITHIN, FRAME_TOLERANCE, Grade, Live, LowRule, MEDIUM, Pick, STRONG, Saved, Score, assign, points, score};

/// A window as the matcher sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key<'a> {
    /// `CGWindowID`, when known (a saved one only when saved in this session).
    pub id: Option<u32>,
    /// The app's bundle id (empty when unknown: never matches by app).
    pub bundle: &'a str,
    /// The window title.
    pub title: &'a str,
}

/// How a live window was matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// The same window id and app.
    Id,
    /// The same app and exact title.
    Title,
    /// The only saved window of its app left, for the only window of that app wanting one.
    OnlyCandidate,
}

impl Key<'_> {
    fn same_window(&self, other: &Key) -> bool {
        self.id.is_some() && self.id == other.id && !self.bundle.is_empty() && self.bundle == other.bundle
    }

    fn same_title(&self, other: &Key) -> bool {
        !self.bundle.is_empty() && self.bundle == other.bundle && !self.title.is_empty() && self.title == other.title
    }
}

/// For each live window (by index), the saved window it is (by index) and how it was found;
/// `None` when nothing matched. `wants(i)`: live window `i` may take part in the third pass
/// (it needs a frame); see the module docs.
pub fn match_windows(saved: &[Key], live: &[Key], wants: impl Fn(usize) -> bool) -> Vec<Option<(usize, How)>> {
    let mut used = vec![false; saved.len()];
    let mut out: Vec<Option<(usize, How)>> = vec![None; live.len()];
    for how in [How::Id, How::Title] {
        let pass = |s: &Key, l: &Key| if how == How::Id { s.same_window(l) } else { s.same_title(l) };
        for (i, l) in live.iter().enumerate() {
            if out[i].is_some() {
                continue;
            }
            if let Some(k) = saved.iter().enumerate().position(|(k, s)| !used[k] && pass(s, l)) {
                used[k] = true;
                out[i] = Some((k, how));
            }
        }
    }
    for i in 0..live.len() {
        let bundle = live[i].bundle;
        if out[i].is_some() || bundle.is_empty() || !wants(i) {
            continue;
        }
        let left: Vec<usize> = (0..saved.len()).filter(|&k| !used[k] && saved[k].bundle == bundle).collect();
        let wanting = (0..live.len()).filter(|&j| out[j].is_none() && live[j].bundle == bundle && wants(j)).count();
        if let [k] = left[..]
            && wanting == 1
        {
            used[k] = true;
            out[i] = Some((k, How::OnlyCandidate));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key<'a>(id: Option<u32>, bundle: &'a str, title: &'a str) -> Key<'a> {
        Key { id, bundle, title }
    }

    #[test]
    fn the_same_window_is_found_by_id_whatever_its_title() {
        let saved = [key(Some(7), "com.example.a", "Draft"), key(Some(8), "com.example.a", "Other")];
        let live = [key(Some(8), "com.example.a", "Renamed"), key(Some(7), "com.example.a", "Also renamed")];
        let m = match_windows(&saved, &live, |_| true);
        assert_eq!(m, vec![Some((1, How::Id)), Some((0, How::Id))]);
    }

    #[test]
    fn an_id_of_another_app_is_not_the_same_window() {
        let saved = [key(Some(7), "com.example.a", "Draft")];
        let live = [key(Some(7), "com.example.b", "Draft")];
        assert_eq!(match_windows(&saved, &live, |_| true), vec![None]);
    }

    #[test]
    fn then_app_and_exact_title_but_never_an_empty_title() {
        let saved = [key(None, "com.example.a", ""), key(None, "com.example.a", "Draft")];
        let live = [key(Some(3), "com.example.a", "Draft"), key(Some(4), "com.example.a", "")];
        let m = match_windows(&saved, &live, |_| false);
        assert_eq!(m, vec![Some((1, How::Title)), None], "an untitled window is no title match");
    }

    #[test]
    fn app_alone_only_for_the_one_candidate_left() {
        // One saved window of the app left, one stranded window wanting it: matched.
        let saved = [key(None, "com.example.a", "Old title")];
        let live = [key(Some(1), "com.example.a", "New title")];
        assert_eq!(match_windows(&saved, &live, |_| true), vec![Some((0, How::OnlyCandidate))]);
        // Two saved windows left: which one is not known; nothing.
        let saved = [key(None, "com.example.a", "One"), key(None, "com.example.a", "Two")];
        assert_eq!(match_windows(&saved, &live, |_| true), vec![None]);
        // Two stranded windows wanting the one saved frame: nothing for either.
        let saved = [key(None, "com.example.a", "One")];
        let live = [key(Some(1), "com.example.a", "X"), key(Some(2), "com.example.a", "Y")];
        assert_eq!(match_windows(&saved, &live, |_| true), vec![None, None]);
    }

    #[test]
    fn a_window_on_screen_keeps_its_own_saved_entry() {
        // Grok High 2: the on-screen window (by id) holds its entry; the stranded sibling whose
        // title changed then has the one candidate left, its own.
        let saved = [key(Some(1), "com.example.a", "Shown"), key(None, "com.example.a", "Was parked")];
        let live = [key(Some(1), "com.example.a", "Shown, renamed"), key(Some(2), "com.example.a", "Renamed")];
        let m = match_windows(&saved, &live, |i| i == 1);
        assert_eq!(m, vec![Some((0, How::Id)), Some((1, How::OnlyCandidate))]);
        // A window that does not want a frame never takes the third pass.
        let live = [key(Some(5), "com.example.a", "New")];
        assert_eq!(match_windows(&saved[1..], &live, |_| false), vec![None]);
    }
}
