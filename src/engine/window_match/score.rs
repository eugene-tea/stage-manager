//! The scored matcher: which saved window a live window is, with how sure we are (the
//! restore design 8D, section 3). One function for both callers: an arrival with many
//! windows ([`crate::engine::persist::rematch_on`]) and one window appearing later
//! ([`crate::engine::model::Model::add_window`] through the model's pending stages).
//!
//! Signals, added up ([`points`]): the same app (required), the same window id (only ever
//! given within one window server session), the exact title, the frame the app reopened the
//! window at, the title's fingerprint ([`super::fingerprint`], [`super::same_fingerprint`]),
//! the size alone. Grades: [`Grade::Strong`] from [`STRONG`] points, [`Grade::Medium`] from
//! [`MEDIUM`], else [`Grade::Low`] (the app alone, maybe its size).
//!
//! **The frame never outweighs two different names.** When both titles have a fingerprint
//! and they differ, the frame counts only as the size: an app opens a new window at its last
//! window's frame (a browser's "Start Page" where the saved page was, Finder's ⌘N where
//! "Downloads" was), and that window is not the saved one.
//!
//! A window with no title yet at the saved frame is a medium match at most
//! ([`points::FRAME_UNTITLED`]): the frame alone, with no name to back it.
//!
//! **A window is parked into an inactive stage only on a strong or medium match** (the
//! callers' rule); a low one goes only to the stage shown ([`LowRule`]), never parked, and
//! leaves the saved window waiting (the callers keep its entry). A pick whose runner-up of
//! another stage scores within [`AMBIGUOUS_WITHIN`] points is ambiguous and counts as low,
//! unless both carry the frame signal and the pick's frame is the nearer one.

use super::fingerprint::same_fingerprint;
use crate::engine::geometry::Rect;

/// Points per signal.
pub mod points {
    /// The same app (bundle id): required for any match.
    pub const BUNDLE: u32 = 10;
    /// The same `CGWindowID` (saved in this window server session).
    pub const ID: u32 = 100;
    /// The same non-empty title.
    pub const TITLE: u32 = 60;
    /// The live frame is the saved one (within [`super::FRAME_TOLERANCE`]).
    pub const FRAME: u32 = 50;
    /// The live frame is the saved one but the window has no title yet: with the app, a
    /// medium match at most (the app may be opening a new window where the saved one was,
    /// its name still to come).
    pub const FRAME_UNTITLED: u32 = 40;
    /// The same fingerprint (when the titles differ; [`super::same_fingerprint`]).
    pub const FINGERPRINT: u32 = 35;
    /// The same size only (when the frames differ, or the frame is the same but the names
    /// are not).
    pub const SIZE: u32 = 15;
}

/// Points for a strong match.
pub const STRONG: u32 = 60;
/// Points for a medium match.
pub const MEDIUM: u32 = 35;
/// A runner-up of another stage this close makes a pick ambiguous.
pub const AMBIGUOUS_WITHIN: u32 = 10;
/// Frames this close (points, each edge and size) are the same frame.
pub const FRAME_TOLERANCE: f64 = 2.0;

/// How sure a match is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Grade {
    /// The app alone (maybe its size): never parks a window.
    Low,
    /// A fingerprint match, or another signal pair below strong.
    Medium,
    /// The same window, its exact title or its exact frame (a titled window).
    Strong,
}

impl Grade {
    /// The grade of `points`.
    pub fn of(points: u32) -> Self {
        if points >= STRONG {
            Self::Strong
        } else if points >= MEDIUM {
            Self::Medium
        } else {
            Self::Low
        }
    }

    /// True for strong and medium: enough to park a window into an inactive stage.
    pub fn confident(self) -> bool {
        self >= Self::Medium
    }

    /// Lower-case name, for the log and `ctl state`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::Strong => "strong",
        }
    }
}

/// A saved window as the scorer sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Saved<'a> {
    /// The app's bundle id (empty: never matches).
    pub bundle: &'a str,
    /// The title when saved.
    pub title: &'a str,
    /// The title's fingerprint.
    pub fp: String,
    /// The window id, only when saved in this window server session.
    pub id: Option<u32>,
    /// Frames the app may reopen it at: the restore frame and the pre-fit frame.
    pub frames: [Option<Rect>; 2],
    /// Which stage (or Desktop) it belongs to: picks are ambiguous only between groups.
    pub group: u64,
}

/// A live window as the scorer sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Live<'a> {
    /// The app's bundle id.
    pub bundle: &'a str,
    /// The title now.
    pub title: &'a str,
    /// The title's fingerprint.
    pub fp: String,
    /// The window id.
    pub id: u32,
    /// The live frame.
    pub frame: Rect,
    /// False when the live frame says nothing (in a park zone, degenerate).
    pub frame_usable: bool,
}

/// One saved window's score for one live window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// The points.
    pub points: u32,
    /// The frame signal counted.
    pub frame: bool,
    /// How far the nearest saved frame is (sum of the four differences; infinite without one).
    pub distance: f64,
}

/// A live window's match.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pick {
    /// The saved window (index into the saved list).
    pub saved: usize,
    /// How sure.
    pub grade: Grade,
    /// The points.
    pub points: u32,
}

/// Which low matches the caller takes: only into the group shown now (the active stage),
/// so a low match never parks a window (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LowRule {
    /// The group shown now: a low match into it is taken; `None` takes none.
    pub shown: Option<u64>,
}

/// The score of `saved` for `live`; `None` for another app.
pub fn score(saved: &Saved, live: &Live) -> Option<Score> {
    if saved.bundle.is_empty() || saved.bundle != live.bundle {
        return None;
    }
    let mut points = points::BUNDLE;
    if saved.id == Some(live.id) {
        points += points::ID;
    }
    let same_title = !saved.title.is_empty() && saved.title == live.title;
    let same_fp = !same_title && same_fingerprint(&saved.fp, &live.fp);
    if same_title {
        points += points::TITLE;
    } else if same_fp {
        points += points::FINGERPRINT;
    }
    // Two different names: the frame says only that the app opened a window where the
    // saved one was.
    let named_apart = !same_title && !same_fp && !saved.fp.is_empty() && !live.fp.is_empty();
    let frames = || saved.frames.iter().flatten().filter(|f| !f.is_degenerate());
    let near = |a: f64, b: f64| (a - b).abs() <= FRAME_TOLERANCE;
    let usable = live.frame_usable && !live.frame.is_degenerate();
    let l = live.frame;
    let frame = usable && !named_apart && frames().any(|f| near(f.x, l.x) && near(f.y, l.y) && near(f.w, l.w) && near(f.h, l.h));
    if frame {
        points += if live.title.is_empty() { points::FRAME_UNTITLED } else { points::FRAME };
    } else if usable && frames().any(|f| near(f.w, l.w) && near(f.h, l.h)) {
        points += points::SIZE;
    }
    let distance = if usable {
        frames()
            .map(|f| (f.x - l.x).abs() + (f.y - l.y).abs() + (f.w - l.w).abs() + (f.h - l.h).abs())
            .fold(f64::INFINITY, f64::min)
    } else {
        f64::INFINITY
    };
    Some(Score { points, frame, distance })
}

/// For each live window, its saved window, each used once: confident picks first, greedily
/// by descending score (ties: the nearer frame, then saved order, then live order), each
/// tested for ambiguity; then the low picks `low` allows (into the group shown). See the
/// module docs.
pub fn assign(saved: &[Saved], live: &[Live], low: LowRule) -> Vec<Option<Pick>> {
    let mut pairs: Vec<(usize, usize, Score)> = Vec::new();
    for (i, l) in live.iter().enumerate() {
        for (k, s) in saved.iter().enumerate() {
            if let Some(sc) = score(s, l) {
                pairs.push((i, k, sc));
            }
        }
    }
    let order = |a: &(usize, usize, Score), b: &(usize, usize, Score)| {
        b.2.points
            .cmp(&a.2.points)
            .then(a.2.distance.total_cmp(&b.2.distance))
            .then(a.1.cmp(&b.1))
            .then(a.0.cmp(&b.0))
    };
    pairs.sort_by(order);
    let mut used_saved = vec![false; saved.len()];
    let mut out: Vec<Option<Pick>> = vec![None; live.len()];
    for &(i, k, sc) in &pairs {
        if out[i].is_some() || used_saved[k] {
            continue;
        }
        let rival = pairs
            .iter()
            .filter(|(i2, k2, _)| *i2 == i && *k2 != k && !used_saved[*k2] && saved[*k2].group != saved[k].group)
            .map(|(_, _, r)| *r)
            .max_by(|a, b| a.points.cmp(&b.points).then(b.distance.total_cmp(&a.distance)));
        let ambiguous = rival.is_some_and(|r| {
            r.points + AMBIGUOUS_WITHIN >= sc.points && !(sc.frame && r.frame && sc.distance < r.distance)
        });
        let grade = if ambiguous { Grade::Low } else { Grade::of(sc.points) };
        if grade.confident() {
            used_saved[k] = true;
            out[i] = Some(Pick { saved: k, grade, points: sc.points });
        }
    }
    // Low picks: into the group shown only (never parked).
    let Some(shown) = low.shown else { return out };
    for (i, pick) in out.iter_mut().enumerate() {
        if pick.is_some() {
            continue;
        }
        let best = pairs
            .iter()
            .filter(|(i2, k, _)| *i2 == i && !used_saved[*k] && saved[*k].group == shown)
            .min_by(|a, b| order(a, b));
        if let Some(&(_, k, sc)) = best {
            used_saved[k] = true;
            *pick = Some(Pick { saved: k, grade: Grade::Low, points: sc.points });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::window_match::fingerprint;

    const F1: Rect = Rect::new(100.0, 80.0, 600.0, 400.0);
    const F2: Rect = Rect::new(300.0, 200.0, 700.0, 500.0);

    fn saved<'a>(bundle: &'a str, title: &'a str, frame: Rect, group: u64) -> Saved<'a> {
        Saved { bundle, title, fp: fingerprint(title), id: None, frames: [Some(frame), None], group }
    }

    fn live<'a>(id: u32, bundle: &'a str, title: &'a str, frame: Rect) -> Live<'a> {
        Live { bundle, title, fp: fingerprint(title), id, frame, frame_usable: true }
    }

    const OTHER: Rect = Rect::new(500.0, 300.0, 800.0, 450.0);

    #[test]
    fn the_score_table() {
        let s = Saved { id: Some(7), ..saved("com.example.a", "Plan", F1, 1) };
        let pts = |l: Live| score(&s, &l).map(|x| x.points);
        // Another app: no score at all, whatever else matches.
        assert_eq!(pts(live(7, "com.example.b", "Plan", F1)), None);
        // The app alone, its size, its frame, the fingerprint, the title, the id.
        assert_eq!(pts(live(1, "com.example.a", "Other", OTHER)), Some(10));
        assert_eq!(pts(live(1, "com.example.a", "Other", Rect { x: 900.0, ..F1 })), Some(25));
        assert_eq!(pts(live(1, "com.example.a", "", Rect { x: 101.5, ..F1 })), Some(50), "within 2 pt, no title yet: medium at most");
        assert_eq!(Grade::of(50), Grade::Medium);
        assert_eq!(pts(live(7, "com.example.a", "", F1)), Some(150), "no title, but the same window: strong");
        assert_eq!(pts(live(1, "com.example.a", "Plan", F1)), Some(120), "the title and the frame");
        assert_eq!(pts(live(1, "com.example.a", "Other", F1)), Some(25), "another name at the frame: the size only");
        assert_eq!(pts(live(1, "com.example.a", "Plan — Edited", OTHER)), Some(45));
        assert_eq!(pts(live(1, "com.example.a", "Plan", OTHER)), Some(70));
        assert_eq!(pts(live(7, "com.example.a", "Other", OTHER)), Some(110));
        // A frame in the park zone says nothing.
        let mut parked = live(1, "com.example.a", "Other", F1);
        parked.frame_usable = false;
        assert_eq!(pts(parked), Some(10));
    }

    #[test]
    fn grades() {
        assert_eq!(Grade::of(10), Grade::Low);
        assert_eq!(Grade::of(25), Grade::Low);
        assert_eq!(Grade::of(45), Grade::Medium);
        assert_eq!(Grade::of(60), Grade::Strong);
        assert!(Grade::Medium.confident() && Grade::Strong.confident() && !Grade::Low.confident());
    }

    #[test]
    fn an_empty_title_is_no_evidence() {
        // L8: two untitled windows are not the same window by their title.
        let s = saved("com.example.a", "", F1, 1);
        assert_eq!(score(&s, &live(1, "com.example.a", "", OTHER)).map(|x| x.points), Some(10));
    }

    #[test]
    fn confident_picks_are_greedy_by_score_each_used_once() {
        let s = [saved("com.example.a", "Alpha", F1, 1), saved("com.example.a", "Beta", F2, 2)];
        let l = [live(1, "com.example.a", "Beta", F2), live(2, "com.example.a", "Alpha", OTHER)];
        let got = assign(&s, &l, LowRule::default());
        assert_eq!(got[0].map(|p| (p.saved, p.grade)), Some((1, Grade::Strong)));
        assert_eq!(got[1].map(|p| (p.saved, p.grade)), Some((0, Grade::Strong)));
    }

    #[test]
    fn a_close_runner_up_in_another_stage_makes_a_pick_low() {
        // Two windows of the same title in two stages: neither may be parked by title.
        let s = [saved("com.example.a", "Untitled", OTHER, 1), saved("com.example.a", "Untitled", OTHER, 2)];
        let l = [live(1, "com.example.a", "Untitled", F1)];
        assert_eq!(assign(&s, &l, LowRule::default()), vec![None], "ambiguous: Apple's rule");
        // Low into the stage shown is taken.
        let shown = LowRule { shown: Some(2) };
        assert_eq!(assign(&s, &l, shown)[0].map(|p| (p.saved, p.grade)), Some((1, Grade::Low)));
        // In the same stage there is nothing to confuse.
        let same = [saved("com.example.a", "Untitled", OTHER, 1), saved("com.example.a", "Untitled", OTHER, 1)];
        assert_eq!(assign(&same, &l, LowRule::default())[0].map(|p| p.grade), Some(Grade::Strong));
    }

    #[test]
    fn the_frame_settles_a_tie_between_stages_when_both_have_it() {
        // Same title in two stages, both within 2 pt: the nearer frame wins and stays strong.
        let near = Rect { x: 100.5, ..F1 };
        let s = [saved("com.example.a", "Untitled", Rect { x: 101.8, ..F1 }, 1), saved("com.example.a", "Untitled", near, 2)];
        let l = [live(1, "com.example.a", "Untitled", F1)];
        assert_eq!(assign(&s, &l, LowRule::default())[0].map(|p| (p.saved, p.grade)), Some((1, Grade::Strong)));
        // Only one of them has the frame: 120 against 85, no tie at all.
        let s = [saved("com.example.a", "Untitled", OTHER, 1), saved("com.example.a", "Untitled", F1, 2)];
        assert_eq!(assign(&s, &l, LowRule::default())[0].map(|p| (p.saved, p.grade)), Some((1, Grade::Strong)));
    }

    #[test]
    fn low_matches_only_into_the_stage_shown() {
        let s = [saved("com.example.a", "Old", OTHER, 1)];
        let l = [live(1, "com.example.a", "New", F1)];
        assert_eq!(assign(&s, &l, LowRule::default()), vec![None]);
        // M3: the app's only saved window, in a stage not shown: never a low pick (it would
        // be parked on the app alone).
        assert_eq!(assign(&s, &l, LowRule { shown: Some(2) }), vec![None]);
        assert_eq!(assign(&s, &l, LowRule { shown: Some(1) })[0].map(|p| p.grade), Some(Grade::Low), "the stage shown");
        // The confident picks are made first, then the low ones.
        let s3 = [saved("com.example.a", "Kept", F2, 2), saved("com.example.a", "Old", OTHER, 1)];
        let l3 = [live(1, "com.example.a", "New", F1), live(2, "com.example.a", "Kept", F2)];
        let got = assign(&s3, &l3, LowRule { shown: Some(1) });
        assert_eq!(got[1].map(|p| (p.saved, p.grade)), Some((0, Grade::Strong)));
        assert_eq!(got[0].map(|p| (p.saved, p.grade)), Some((1, Grade::Low)));
    }

    #[test]
    fn the_frame_never_outweighs_two_different_names() {
        // H2: a browser opens "Start Page" at the frame of the saved page's window.
        let s = [saved("com.example.browser", "Example Domain — Example Browser", F1, 2)];
        let start = live(1, "com.example.browser", "Start Page", F1);
        let sc = score(&s[0], &start).expect("same app");
        assert_eq!((sc.points, sc.frame), (25, false), "the frame counts as the size only");
        assert_eq!(assign(&s, std::slice::from_ref(&start), LowRule { shown: Some(1) }), vec![None], "low: Apple's rule, the entry kept");
        // Finder's ⌘N where the pending "Downloads" window was.
        let s = [saved("com.example.finder", "Downloads", F2, 3)];
        let recents = live(2, "com.example.finder", "Recents", F2);
        assert_eq!(score(&s[0], &recents).map(|x| x.points), Some(25));
        assert_eq!(assign(&s, &[recents], LowRule::default()), vec![None]);
        // A title not known yet (empty): the frame counts, medium at most; the same name:
        // in full.
        assert_eq!(score(&s[0], &live(3, "com.example.finder", "", F2)).map(|x| x.points), Some(50));
        assert_eq!(score(&s[0], &live(3, "com.example.finder", "Downloads — Edited", F2)).map(|x| x.points), Some(95));
        // A counter of the same name ("Untitled" and "Untitled 2") is the same name.
        let s = saved("com.example.editor", "Untitled", F1, 1);
        assert_eq!(score(&s, &live(4, "com.example.editor", "Untitled 2", F1)).map(|x| x.points), Some(95));
        // A number that names the window is another name.
        let s = saved("com.example.books", "Invoice 101", F1, 1);
        assert_eq!(score(&s, &live(5, "com.example.books", "Invoice 102", F1)).map(|x| x.points), Some(25));
    }
}
