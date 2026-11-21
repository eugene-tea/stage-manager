//! The window server's notices to the app, the pure half (round r5-notices; the platform half
//! is [`crate::platform::spaces::notices`], docs/ARCHITECTURE.md "The window server's
//! notices"). A notice is a **trigger for a read**, never a fact: it says "look now", and what
//! the look finds decides, as the tick's look decides without it. Every poll stays as the cap
//! behind it.
//!
//! What was measured (macOS 27.2, 11-21, `probe-spaces cgs`; no notice of these types in 52
//! rests of 3 s): [`Kind::Overview`] once at Mission Control's and App Exposé's open and once
//! at their close, and at a Desktop change's end; [`Kind::Added`] and [`Kind::TakenOut`] name a
//! window and a Space (12 bytes); [`Kind::Created`], [`Kind::Visible`] and [`Kind::Invisible`]
//! name a window (4 bytes); [`Kind::DesktopChanged`] and [`Kind::DesktopChangedToo`] come at
//! a Desktop change's end.
//!
//! - **Layout** ([`Layout`]): which of a membership notice's three words is the window and
//!   which two are the Space was not measured, so it is learned by content: from a notice that
//!   names a window and a Space the app knows (the launch probe's own panel and its Desktop,
//!   or a window the engine just asked onto a Desktop). Until then a membership notice
//!   names nothing, and its use looks at every window.
//! - **Our own** ([`Expected`]): what the engine just asked (a window into a Space) is
//!   expected; its notices trigger nothing. By content, the window and the Space, never by a
//!   time window.
//! - **Bursts** ([`settle`]): the notices of one kind heard in one turn of the main run loop
//!   are one [`Burst`] (Mission Control's open: 53 of one type).
//! - **Health** ([`Health::verdict`]): whether any registered type was heard since the
//!   launch probe (the probe's own panel notices may be lost before the event loop turns:
//!   live 11-21, its creation's never came); which types never were is `ctl state`'s.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// The types used, by their numbers (SkyLight's, as measured; no public names on macOS 27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// 811: a window was created (payload: its id), 3-11 ms after the cause.
    Created,
    /// 815: a window became visible (its id).
    Visible,
    /// 816: a window became invisible (its id).
    Invisible,
    /// 1325: a window was added to a Space (a window and a Space).
    Added,
    /// 1326: a window was taken out of a Space (a window and a Space).
    TakenOut,
    /// 1329: at a Desktop change's end.
    DesktopChangedToo,
    /// 1401: at a Desktop change's end (551 ms after the key).
    DesktopChanged,
    /// 1508: Mission Control's or App Exposé's open or close, a Desktop change's end (no
    /// payload). Not at Show Desktop.
    Overview,
}

impl Kind {
    /// Every kind registered.
    pub const ALL: [Kind; 8] =
        [Kind::Created, Kind::Visible, Kind::Invisible, Kind::Added, Kind::TakenOut, Kind::DesktopChangedToo, Kind::DesktopChanged, Kind::Overview];

    /// Its type number.
    pub fn code(self) -> u32 {
        match self {
            Kind::Created => 811,
            Kind::Visible => 815,
            Kind::Invisible => 816,
            Kind::Added => 1325,
            Kind::TakenOut => 1326,
            Kind::DesktopChangedToo => 1329,
            Kind::DesktopChanged => 1401,
            Kind::Overview => 1508,
        }
    }

    /// The kind of type number `code`.
    pub fn of(code: u32) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.code() == code)
    }

    /// What it says, for the log and `ctl state`.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Created => "window created",
            Kind::Visible => "window visible",
            Kind::Invisible => "window invisible",
            Kind::Added => "window added to a Space",
            Kind::TakenOut => "window taken out of a Space",
            Kind::DesktopChangedToo | Kind::DesktopChanged => "a Desktop change's end",
            Kind::Overview => "Mission Control, App Exposé or a Desktop change",
        }
    }

    /// Its payload names one window in its first word.
    pub fn names_window(self) -> bool {
        matches!(self, Kind::Created | Kind::Visible | Kind::Invisible)
    }

    /// Its payload names a window and a Space ([`Layout`]).
    pub fn membership(self) -> bool {
        matches!(self, Kind::Added | Kind::TakenOut)
    }

    /// The WM thread has a use for it ([`Burst`]); the others are only counted.
    pub fn forwarded(self) -> bool {
        matches!(self, Kind::Added | Kind::TakenOut | Kind::DesktopChanged | Kind::DesktopChangedToo | Kind::Overview)
    }
}

/// Words of a payload kept (a membership notice has 3).
pub const WORDS: usize = 4;

/// One notice as its callback had it: its type, when it came (stamped at the callback's
/// entry) and its first words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Raw {
    /// The type number.
    pub code: u32,
    /// When the callback was entered.
    pub at: Instant,
    /// The payload's first words (zero past its length).
    pub words: [u32; WORDS],
    /// The payload's length in bytes.
    pub len: usize,
}

/// Where a membership notice names its window and its Space: the window's word, and the first
/// of the Space's two (a 64-bit id, low word first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The window's word.
    pub window: usize,
    /// The Space's first word.
    pub space: usize,
}

impl Layout {
    /// The layout of `raw` when it names window `window` and Space `space` at one place each
    /// (`None`: it does not, or at more than one).
    pub fn learn(raw: &Raw, window: u32, space: u64) -> Option<Layout> {
        let n = (raw.len / 4).min(WORDS);
        let mut found = None;
        for w in (0..n).filter(|i| raw.words[*i] == window) {
            for s in (0..n.saturating_sub(1)).filter(|s| *s != w && *s + 1 != w) {
                if u64::from(raw.words[s]) | (u64::from(raw.words[s + 1]) << 32) == space {
                    if found.is_some() {
                        return None;
                    }
                    found = Some(Layout { window: w, space: s });
                }
            }
        }
        found
    }

    /// The window and the Space `raw` names.
    pub fn read(self, raw: &Raw) -> Option<(u32, u64)> {
        let n = (raw.len / 4).min(WORDS);
        if self.window >= n || self.space + 1 >= n {
            return None;
        }
        Some((raw.words[self.window], u64::from(raw.words[self.space]) | (u64::from(raw.words[self.space + 1]) << 32)))
    }

    /// As one byte (0 is none), for an atomic.
    pub fn pack(layout: Option<Layout>) -> u8 {
        layout.map_or(0, |l| 1 + (l.window as u8) * 8 + l.space as u8)
    }

    /// Back from [`Self::pack`].
    pub fn unpack(b: u8) -> Option<Layout> {
        (b != 0).then(|| Layout { window: usize::from((b - 1) / 8), space: usize::from((b - 1) % 8) })
    }
}

/// How many of our own asks are kept ([`Expected`]): one per window, the oldest dropped.
pub const EXPECTED_MAX: usize = 1024;

/// One ask of ours for a window: into which Space, and which of its notices came.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ask {
    into: u64,
    added: bool,
    taken_out: bool,
    seq: u64,
}

/// What the engine asked of the window server, by window: its own adds and take-outs, whose
/// notices trigger nothing. The latest ask per window; a notice it explains is taken once.
#[derive(Debug, Default)]
pub struct Expected {
    asks: BTreeMap<u32, Ask>,
    seq: u64,
}

impl Expected {
    /// Windows `ids` asked into Space `into` (and out of the others).
    pub fn asked(&mut self, into: u64, ids: &[u32]) {
        for id in ids {
            self.seq += 1;
            self.asks.insert(*id, Ask { into, added: false, taken_out: false, seq: self.seq });
        }
        while self.asks.len() > EXPECTED_MAX {
            let Some(oldest) = self.asks.iter().min_by_key(|(_, a)| a.seq).map(|(id, _)| *id) else { break };
            self.asks.remove(&oldest);
        }
    }

    /// True when a notice of `kind` for `window` and `space` is one of ours (taken: a second
    /// one is not): added to the Space it was asked into, or taken out of another.
    pub fn take(&mut self, kind: Kind, window: u32, space: u64) -> bool {
        let Some(a) = self.asks.get_mut(&window) else { return false };
        let ours = match kind {
            Kind::Added if a.into == space && !a.added => {
                a.added = true;
                true
            }
            Kind::TakenOut if a.into != space && !a.taken_out => {
                a.taken_out = true;
                true
            }
            _ => false,
        };
        if a.added && a.taken_out {
            self.asks.remove(&window);
        }
        ours
    }

    /// The layout `raw` (a window added to a Space) shows for an ask of ours it explains.
    pub fn learn(&self, raw: &Raw) -> Option<Layout> {
        if Kind::of(raw.code) != Some(Kind::Added) {
            return None;
        }
        self.asks.iter().find_map(|(w, a)| Layout::learn(raw, *w, a.into))
    }

    /// How many asks are kept.
    pub fn len(&self) -> usize {
        self.asks.len()
    }

    /// True when none is.
    pub fn is_empty(&self) -> bool {
        self.asks.is_empty()
    }
}

/// How long after the launch probe its panel's notices are waited for (they come once the
/// main thread's event loop turns, after the launch's setup).
pub const HEALTH_WAIT: Duration = Duration::from_secs(5);

/// Whether the notices come ([`Health::verdict`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// No probe ran with the notices registered.
    Off,
    /// None heard yet, within [`HEALTH_WAIT`] of the probe.
    Waiting,
    /// A registered type was heard.
    Heard,
    /// None within [`HEALTH_WAIT`]: the polls carry on as before (not a failure).
    Silent,
}

impl Verdict {
    /// Its word in `ctl state`.
    pub fn name(self) -> &'static str {
        match self {
            Verdict::Off => "off",
            Verdict::Waiting => "waiting",
            Verdict::Heard => "heard",
            Verdict::Silent => "silent",
        }
    }
}

/// The launch probe's panel and what it brought: its creation ([`Kind::Created`]), its
/// take-out of its Desktop as it went into the trial Space ([`Kind::TakenOut`]) and its add
/// back ([`Kind::Added`]), matched by content (the panel's id, its Desktop's).
#[derive(Debug, Clone, Copy, Default)]
pub struct Health {
    panel: Option<(u32, u64, Instant)>,
    /// The panel's creation came.
    pub created: bool,
    /// Its take-out came.
    pub taken_out: bool,
    /// Its add came.
    pub added: bool,
}

impl Health {
    /// The probe made panel `panel` on Desktop `home` at `at`.
    pub fn probe(&mut self, panel: u32, home: u64, at: Instant) {
        *self = Health { panel: Some((panel, home, at)), ..Health::default() };
    }

    /// Notes `raw` if it is one of the panel's; the membership layout it shows, if any.
    pub fn heard(&mut self, raw: &Raw) -> Option<Layout> {
        let (panel, home, _) = self.panel?;
        match Kind::of(raw.code)? {
            Kind::Created if raw.len >= 4 && raw.words[0] == panel => {
                self.created = true;
                None
            }
            k @ (Kind::Added | Kind::TakenOut) => {
                let layout = Layout::learn(raw, panel, home)?;
                if k == Kind::Added {
                    self.added = true;
                } else {
                    self.taken_out = true;
                }
                Some(layout)
            }
            _ => None,
        }
    }

    /// The verdict at `now`, `any` registered type heard so far: heard as soon as one was (the
    /// panel's own notices teach the layout, they are not the test: its creation's is lost
    /// before the event loop turns).
    pub fn verdict(&self, now: Instant, any: bool) -> Verdict {
        let Some((_, _, at)) = self.panel else { return Verdict::Off };
        if any {
            Verdict::Heard
        } else if now.saturating_duration_since(at) < HEALTH_WAIT {
            Verdict::Waiting
        } else {
            Verdict::Silent
        }
    }
}

/// Windows (or window and Space pairs) a burst names at most; past it, it names none and its
/// use looks at every window.
pub const NAMED_MAX: usize = 64;

/// The notices of one kind heard in one turn of the main run loop, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct Burst {
    /// Their kind.
    pub kind: Kind,
    /// When the first came (stamped at its callback's entry).
    pub at: Instant,
    /// How many came.
    pub count: u32,
    /// The windows the window notices named ([`Kind::names_window`]).
    pub windows: Vec<u32>,
    /// The windows and Spaces the membership notices named, but ours ([`Expected`]).
    pub members: Vec<(u32, u64)>,
    /// Membership notices that named nothing readable (no layout learned yet), or more than
    /// [`NAMED_MAX`]: their use looks at every window.
    pub unnamed: u32,
    /// Membership notices that were our own asks' (nothing to do for them).
    pub ours: u32,
}

impl Burst {
    /// True when the WM thread has something to look at for it.
    pub fn wants_look(&self) -> bool {
        match self.kind {
            Kind::Added | Kind::TakenOut => !self.members.is_empty() || self.unnamed > 0,
            k => k.forwarded(),
        }
    }
}

/// The notices `raws` of one turn, in the order heard, as bursts by kind (in the order each
/// kind first came). Learns the membership layout from the probe's panel (`health`) or from
/// an ask of ours (`expected`) when none is known (`layout`), and drops the notices of our own
/// asks. Unknown types are left out.
pub fn settle(raws: &[Raw], layout: &mut Option<Layout>, expected: &mut Expected, health: &mut Health) -> Vec<Burst> {
    let mut out: Vec<Burst> = Vec::new();
    for raw in raws {
        let Some(kind) = Kind::of(raw.code) else { continue };
        if let Some(l) = health.heard(raw) {
            layout.get_or_insert(l);
        }
        if kind.membership() && layout.is_none() {
            *layout = expected.learn(raw);
        }
        let i = match out.iter().position(|b| b.kind == kind) {
            Some(i) => i,
            None => {
                out.push(Burst { kind, at: raw.at, count: 0, windows: Vec::new(), members: Vec::new(), unnamed: 0, ours: 0 });
                out.len() - 1
            }
        };
        let b = &mut out[i];
        b.count += 1;
        if kind.names_window() && raw.len >= 4 {
            if b.windows.len() < NAMED_MAX && !b.windows.contains(&raw.words[0]) {
                b.windows.push(raw.words[0]);
            }
        } else if kind.membership() {
            match layout.and_then(|l| l.read(raw)) {
                Some((w, s)) if expected.take(kind, w, s) => b.ours += 1,
                Some(m) if b.members.len() < NAMED_MAX => {
                    if !b.members.contains(&m) {
                        b.members.push(m);
                    }
                }
                _ => b.unnamed += 1,
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(code: u32, words: &[u32]) -> Raw {
        let mut w = [0; WORDS];
        w[..words.len()].copy_from_slice(words);
        Raw { code, at: Instant::now(), words: w, len: words.len() * 4 }
    }

    #[test]
    fn a_layout_is_learned_from_a_window_and_a_space_named_once() {
        // Either order: the Space's 64 bits first, or the window first.
        assert_eq!(Layout::learn(&raw(1325, &[7, 0, 4242]), 4242, 7), Some(Layout { window: 2, space: 0 }));
        assert_eq!(Layout::learn(&raw(1325, &[4242, 7, 0]), 4242, 7), Some(Layout { window: 0, space: 1 }));
        assert_eq!(Layout::learn(&raw(1325, &[4242, 8, 0]), 4242, 7), None, "another Space");
        assert_eq!(Layout::learn(&raw(1325, &[7, 0, 7]), 7, 7), Some(Layout { window: 2, space: 0 }), "one reading only");
        assert_eq!(Layout::learn(&raw(1325, &[7, 0, 7, 0]), 7, 7), None, "two readings: not told apart");
        let l = Layout { window: 2, space: 0 };
        assert_eq!(l.read(&raw(1326, &[9, 0, 77])), Some((77, 9)));
        assert_eq!(l.read(&raw(1326, &[9])), None, "too short");
        assert_eq!(Layout::unpack(Layout::pack(Some(l))), Some(l));
        assert_eq!(Layout::unpack(Layout::pack(None)), None);
    }

    /// Our own hide (into the stage Space: taken out of its Desktop) and show (added to its
    /// Desktop) are expected by content; a take-out of the Space the window was put into, or a
    /// second notice of the same kind, is not ours.
    #[test]
    fn our_own_asks_are_told_by_the_window_and_the_space() {
        let mut e = Expected::default();
        e.asked(900, &[5]);
        assert!(e.take(Kind::TakenOut, 5, 1), "out of its Desktop as it went into the stage Space");
        assert!(!e.take(Kind::TakenOut, 5, 1), "once");
        assert!(!e.take(Kind::TakenOut, 5, 900), "taken out of the stage Space it was put into: not ours");
        assert!(!e.take(Kind::Added, 5, 1), "added to another Space");
        assert!(e.take(Kind::Added, 5, 900));
        assert!(e.is_empty(), "both came: forgotten");
        assert!(!e.take(Kind::Added, 6, 1), "never asked");
        for id in 0..(EXPECTED_MAX as u32 + 10) {
            e.asked(1, &[id]);
        }
        assert_eq!(e.len(), EXPECTED_MAX, "bounded");
        assert!(!e.take(Kind::Added, 0, 1), "the oldest dropped");
    }

    /// A burst of 53 notices of one kind is one; our own are counted apart; before a layout is
    /// known a membership notice names nothing, and one of the probe's panel teaches it.
    #[test]
    fn a_turn_settles_into_one_burst_per_kind() {
        let mut layout = None;
        let mut expected = Expected::default();
        let mut health = Health::default();
        let t = Instant::now();
        health.probe(42, 1, t);
        let mut raws: Vec<Raw> = (0..53).map(|i| raw(1325, &[1, 0, 100 + i])).collect();
        raws.push(raw(1508, &[]));
        raws.push(raw(816, &[3]));
        raws.push(raw(9999, &[1]));
        let b = settle(&raws, &mut layout, &mut expected, &mut health);
        assert_eq!(b.iter().map(|b| (b.kind, b.count)).collect::<Vec<_>>(), vec![(Kind::Added, 53), (Kind::Overview, 1), (Kind::Invisible, 1)]);
        assert_eq!(b[0].unnamed, 53, "no layout yet");
        assert!(b[0].wants_look() && b[1].wants_look() && !b[2].wants_look());
        // The probe's panel: created, out of its Desktop, back.
        let raws = [raw(811, &[42]), raw(1326, &[1, 0, 42]), raw(1325, &[1, 0, 42])];
        let _ = settle(&raws, &mut layout, &mut expected, &mut health);
        assert_eq!(layout, Some(Layout { window: 2, space: 0 }));
        assert_eq!(health.verdict(t, true), Verdict::Heard);
        expected.asked(900, &[7]);
        let raws = [raw(1326, &[1, 0, 7]), raw(1326, &[900, 0, 7]), raw(1326, &[900, 0, 7])];
        let b = settle(&raws, &mut layout, &mut expected, &mut health);
        assert_eq!((b[0].ours, b[0].members.clone(), b[0].unnamed), (1, vec![(7, 900)], 0), "ours dropped, the rest named once");
    }

    #[test]
    fn a_layout_is_learned_from_a_show_of_ours_too() {
        let mut layout = None;
        let mut expected = Expected::default();
        let mut health = Health::default();
        expected.asked(3, &[77]);
        let b = settle(&[raw(1325, &[77, 3, 0])], &mut layout, &mut expected, &mut health);
        assert_eq!(layout, Some(Layout { window: 0, space: 1 }));
        assert_eq!((b[0].ours, b[0].members.len()), (1, 0));
    }

    #[test]
    fn the_health_waits_then_says_silent() {
        let t = Instant::now();
        let mut h = Health::default();
        assert_eq!(h.verdict(t, true), Verdict::Off);
        h.probe(42, 1, t);
        assert_eq!(h.verdict(t + Duration::from_secs(1), false), Verdict::Waiting);
        assert_eq!(h.verdict(t + HEALTH_WAIT, false), Verdict::Silent, "none came");
        // Live 11-21: the panel's take-out and add came, its creation's did not; others came.
        let _ = h.heard(&raw(1326, &[1, 0, 42]));
        assert!(!h.created && h.taken_out);
        assert_eq!(h.verdict(t + Duration::from_secs(1), true), Verdict::Heard, "any type heard: heard at once");
    }
}
