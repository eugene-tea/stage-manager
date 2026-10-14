//! The scored matcher: which saved window a live window is, with how sure we are (the
//! restore design 8D, section 3). One function for both callers: an arrival with many
//! windows ([`crate::engine::persist::rematch_on`]) and one window appearing later
//! ([`crate::engine::model::Model::add_window`] through the model's pending stages).
//!
//! Signals, added up ([`points`]): the same app (required), the same window id (only ever
//! given within one window server session), the exact title, the frame the app reopened the
//! window at, the title's fingerprint ([`super::fingerprint`]), the size alone. Grades:
//! [`Grade::Strong`] from [`STRONG`] points, [`Grade::Medium`] from [`MEDIUM`], else
//! [`Grade::Low`] (the app alone, maybe its size).
//!
//! **A window is parked into an inactive stage only on a strong or medium match** (the
//! callers' rule); a low one goes to its saved stage only when that stage is shown, or, at an
//! arrival, when it is the only candidate. A pick whose runner-up of another stage scores
//! within [`AMBIGUOUS_WITHIN`] points is ambiguous and counts as low, unless both carry the
//! frame signal and the pick's frame is the nearer one.

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
    /// The same non-empty fingerprint (when the titles differ).
    pub const FINGERPRINT: u32 = 35;
    /// The same size only (when the frames differ).
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
    /// The same window, its exact title or its exact frame.
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

/// Which low matches the caller takes (low never parks: see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LowRule {
    /// The group shown now: a low match into it is taken.
    pub shown: Option<u64>,
    /// A low match is also taken when its app has exactly one saved and one live window left
    /// once the confident matches are made (an arrival: Apple's rule parks there too).
    pub only_candidate: bool,
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
    if !saved.title.is_empty() && saved.title == live.title {
        points += points::TITLE;
    } else if !saved.fp.is_empty() && saved.fp == live.fp {
        points += points::FINGERPRINT;
    }
    let frames = || saved.frames.iter().flatten().filter(|f| !f.is_degenerate());
    let near = |a: f64, b: f64| (a - b).abs() <= FRAME_TOLERANCE;
    let usable = live.frame_usable && !live.frame.is_degenerate();
    let l = live.frame;
    let frame = usable && frames().any(|f| near(f.x, l.x) && near(f.y, l.y) && near(f.w, l.w) && near(f.h, l.h));
    if frame {
        points += points::FRAME;
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
/// tested for ambiguity; then the low picks `low` allows. See the module docs.
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
    // Low picks: into the group shown, or the only candidate left.
    let left = |bundle: &str, used: &[bool], out: &[Option<Pick>]| {
        let s = saved.iter().enumerate().filter(|(k, s)| !used[*k] && s.bundle == bundle).count();
        let l = live.iter().enumerate().filter(|(i, l)| out[*i].is_none() && l.bundle == bundle).count();
        (s, l)
    };
    let only: Vec<bool> = live.iter().map(|l| low.only_candidate && left(l.bundle, &used_saved, &out) == (1, 1)).collect();
    for i in 0..live.len() {
        if out[i].is_some() {
            continue;
        }
        let best = pairs
            .iter()
            .filter(|(i2, k, _)| *i2 == i && !used_saved[*k] && (only[i] || low.shown == Some(saved[*k].group)))
            .min_by(|a, b| order(a, b));
        if let Some(&(_, k, sc)) = best {
            used_saved[k] = true;
            out[i] = Some(Pick { saved: k, grade: Grade::Low, points: sc.points });
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
        assert_eq!(pts(live(1, "com.example.a", "Other", Rect { x: 101.5, ..F1 })), Some(60), "within 2 pt");
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
        let shown = LowRule { shown: Some(2), only_candidate: false };
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
    fn low_matches_only_where_the_caller_allows() {
        let s = [saved("com.example.a", "Old", OTHER, 1)];
        let l = [live(1, "com.example.a", "New", F1)];
        assert_eq!(assign(&s, &l, LowRule::default()), vec![None]);
        let only = LowRule { shown: None, only_candidate: true };
        assert_eq!(assign(&s, &l, only)[0].map(|p| p.grade), Some(Grade::Low), "the only candidate");
        // Two saved windows of the app left: no longer the only candidate.
        let s2 = [saved("com.example.a", "Old", OTHER, 1), saved("com.example.a", "Older", OTHER, 2)];
        assert_eq!(assign(&s2, &l, only), vec![None]);
        // The confident picks are made first, then the counts.
        let s3 = [saved("com.example.a", "Kept", F2, 1), saved("com.example.a", "Old", OTHER, 2)];
        let l3 = [live(1, "com.example.a", "New", F1), live(2, "com.example.a", "Kept", F2)];
        let got = assign(&s3, &l3, only);
        assert_eq!(got[1].map(|p| (p.saved, p.grade)), Some((0, Grade::Strong)));
        assert_eq!(got[0].map(|p| (p.saved, p.grade)), Some((1, Grade::Low)));
    }
}
