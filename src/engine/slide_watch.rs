//! Telemetry for Desktop slides after a Dock click or ⌘Tab (the Spaces engine): one log line
//! per activation, an hourly summary, and counters for `ctl state`. Nothing here changes
//! what the app does.
//!
//! Under the Spaces engine a Dock click (or ⌘Tab) to an app whose windows here wait in stage
//! Spaces puts its stage's window home first, parked at the corner (round dock-put-home), so
//! macOS's "switch to a Space with open windows for the application" keeps the screen here.
//! The known risk is a park or lend that comes too late for a quick click: macOS then slides
//! to another Desktop. Each activation is watched for [`WINDOW`] from its release (the Dock's
//! mouse-up, ⌘'s release) and judged once its outcome is known ([`Watch::due`]), from the
//! Desktop changes the controller already recognizes (no extra polling):
//!
//! - **expected** ([`Change::expected`]): the change goes where this click was meant to go: a
//!   Desktop arranged ahead for it, the Desktop predicted at the press, or a Desktop with a
//!   window of the app when the app had none on the Desktop shown (macOS's own rule, as
//!   Apple's).
//! - **person**: the person moved first: Mission Control or App Exposé showed, or a ⌃-arrow
//!   key was heard, after the release and before the change ([`Person`]). A trackpad swipe
//!   is not heard: one within [`WINDOW`] of a click reads as a slide.
//! - **slid**: any other change of the main screen's Desktop within [`WINDOW`]: one nobody
//!   asked for. `slid_back` says whether a later change returned to the Desktop the slide
//!   left (watched up to [`WAIT_MAX`]), `away_ms` for how long.
//!
//! The line ([`Watch::line`]) and the summary ([`Tally::summary_line`]) are `key=value`
//! words after a fixed prefix (`slide-watch: outcome`, `slide-watch: summary`), for grep.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A Desktop change this soon after the release counts as the activation's.
pub const WINDOW: Duration = Duration::from_millis(1500);

/// The outcome waits this long at most after the release: for a notice whose Desktop is not
/// recognized yet, or for a slide's return.
pub const WAIT_MAX: Duration = Duration::from_millis(3000);

/// A summary is written once an hour while there was activity.
pub const SUMMARY_EVERY: Duration = Duration::from_secs(3600);

/// What started the activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// A click on the app's Dock item.
    Dock,
    /// ⌘Tab's switcher, ⌘ let go on the app.
    CmdTab,
}

impl Via {
    /// The word in the log.
    pub fn name(self) -> &'static str {
        match self {
            Via::Dock => "dock",
            Via::CmdTab => "cmdtab",
        }
    }
}

/// What the person did that explains a Desktop change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Person {
    /// A ⌃-arrow key (Move left or right a space, Mission Control, App Exposé).
    DesktopKey,
    /// Mission Control or App Exposé showed.
    MissionControl,
}

impl Person {
    /// The word in the log.
    pub fn name(self) -> &'static str {
        match self {
            Person::DesktopKey => "desktop-key",
            Person::MissionControl => "mission-control",
        }
    }
}

/// The facts of one activation, known at its release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    /// Dock or ⌘Tab.
    pub via: Via,
    /// The app's bundle id, when known.
    pub bundle: Option<String>,
    /// The app.
    pub pid: i32,
    /// A window of its stage was put on the Desktop shown (lent) for it.
    pub put_home: bool,
    /// Whether that window's park took (`None`: nothing put home, no park asked).
    pub parked: Option<bool>,
    /// What the lend's park took, in milliseconds (`None`: none asked).
    pub park_ms: Option<u64>,
    /// Why the park was skipped or did not take, or the lend was not made.
    pub why: Option<&'static str>,
    /// From the Dock press to its release, or from the first Tab to ⌘'s release.
    pub press_to_release_ms: Option<u64>,
    /// The main screen's Desktop at the release.
    pub desktop_before: u64,
    /// The Desktop predicted for the click at the press (another Desktop), if any.
    pub predicted: Option<u64>,
    /// The app had a window on the Desktop shown at the release.
    pub app_here: bool,
}

/// A change of the main screen's Desktop seen during a watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    /// Milliseconds from the release to its notice.
    pub at_ms: u64,
    /// The Desktop left.
    pub from: u64,
    /// The Desktop recognized.
    pub to: u64,
    /// Where this activation was meant to go (see the module's notes).
    pub expected: bool,
}

/// How a watch ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The Desktop did not change.
    None,
    /// It changed only where the activation was meant to go.
    Expected,
    /// The person changed it first ([`Person`]).
    Person,
    /// It changed with nobody asking.
    Slid,
}

impl Kind {
    /// The word in the log.
    pub fn name(self) -> &'static str {
        match self {
            Kind::None => "none",
            Kind::Expected => "expected",
            Kind::Person => "person",
            Kind::Slid => "slid",
        }
    }
}

/// A watch's judgement ([`classify`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// What it was.
    pub kind: Kind,
    /// A slide's later return to the Desktop it left.
    pub slid_back: bool,
    /// How long the screen was away before that return, in milliseconds.
    pub away_ms: Option<u64>,
    /// The Desktop shown last.
    pub desktop_after: u64,
    /// What the person did, when that explains the change.
    pub by: Option<Person>,
}

impl Outcome {
    /// True for a slide nobody asked for.
    pub fn slid(&self) -> bool {
        self.kind == Kind::Slid
    }
}

/// Judges the Desktop changes `changes` (in order) after a release on Desktop `before`;
/// `person`: when (ms from the release) and what the person did first, if anything. Changes
/// to the same Desktop are ignored. Only a change within [`WINDOW`] makes a slide; a later
/// change back to the Desktop it left is its return.
pub fn classify(before: u64, changes: &[Change], person: Option<(u64, Person)>) -> Outcome {
    let window = WINDOW.as_millis() as u64;
    let real: Vec<&Change> = changes.iter().filter(|c| c.from != c.to).collect();
    let desktop_after = real.last().map_or(before, |c| c.to);
    let mut out = Outcome { kind: Kind::None, slid_back: false, away_ms: None, desktop_after, by: None };
    for (i, c) in real.iter().enumerate() {
        if c.at_ms > window {
            break;
        }
        if let Some((_, p)) = person.filter(|(at, _)| *at <= c.at_ms) {
            out.kind = Kind::Person;
            out.by = Some(p);
            return out;
        }
        if c.expected {
            out.kind = Kind::Expected;
            continue;
        }
        out.kind = Kind::Slid;
        if let Some(back) = real[i + 1..].iter().find(|b| b.to == c.from) {
            out.slid_back = true;
            out.away_ms = Some(back.at_ms.saturating_sub(c.at_ms));
        }
        return out;
    }
    out
}

/// One activation watched until its outcome is known.
#[derive(Debug, Clone)]
pub struct Watch {
    start: Start,
    at: Instant,
    changes: Vec<Change>,
    person: Option<(u64, Person)>,
    /// Space change notices since the release.
    notices: u32,
    /// The last notice's time (ms from the release) while its Desktop is not recognized.
    noticed: Option<u64>,
    /// The switch's incoming side was already shown, so the rest of its stage showed at once
    /// (the safety net of round dock-put-home).
    already_shown: bool,
    /// An activation of the app was notified.
    activated: bool,
    /// ⌘Tab only: the first app activated, when not the one selected (its pid and bundle):
    /// the switcher's selection was a guess, or a click picked another app.
    other: Option<(i32, Option<String>)>,
}

impl Watch {
    /// A watch from the release at `at`.
    pub fn new(start: Start, at: Instant) -> Self {
        Self { start, at, changes: Vec::new(), person: None, notices: 0, noticed: None, already_shown: false, activated: false, other: None }
    }

    /// The activation's facts.
    pub fn start(&self) -> &Start {
        &self.start
    }

    fn ms(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.at).as_millis() as u64
    }

    /// A Space change notice at `now`.
    pub fn notice(&mut self, now: Instant) {
        self.notices += 1;
        self.noticed = Some(self.ms(now));
    }

    /// The main screen's Desktop recognized at `now`, `from` → `to` (the same Desktop when
    /// nothing changed there); timed from its notice when one came.
    pub fn arrived(&mut self, now: Instant, from: u64, to: u64, expected: bool) {
        let at_ms = self.noticed.take().unwrap_or_else(|| self.ms(now));
        if from != to {
            self.changes.push(Change { at_ms, from, to, expected });
        }
    }

    /// The person did `what` at `now` (the first is kept).
    pub fn person(&mut self, now: Instant, what: Person) {
        if self.person.is_none() {
            self.person = Some((self.ms(now), what));
        }
    }

    /// The switch found its incoming side already shown.
    pub fn shown_early(&mut self) {
        self.already_shown = true;
    }

    /// An activation of app `pid` (`bundle`, when known) was notified. After ⌘Tab, the
    /// first activation of another app before the one selected names the app activated
    /// ([`Self::pid`]; the line's `bundle`, with `guessed=` the one selected).
    pub fn activated(&mut self, pid: i32, bundle: Option<&str>) {
        if pid == self.start.pid {
            self.activated = true;
        } else if self.start.via == Via::CmdTab && !self.activated && self.other.is_none() {
            self.other = Some((pid, bundle.map(str::to_string)));
        }
    }

    /// The app this activation went to: the one activated when another than the one selected
    /// was (⌘Tab), else the one selected.
    pub fn pid(&self) -> i32 {
        match &self.other {
            Some((pid, _)) if !self.activated => *pid,
            _ => self.start.pid,
        }
    }

    /// True once the outcome is known at `now`: [`WINDOW`] has passed, no notice waits for its
    /// Desktop and no slide waits for its return; or [`WAIT_MAX`] has passed.
    pub fn due(&self, now: Instant) -> bool {
        let age = now.saturating_duration_since(self.at);
        if age >= WAIT_MAX {
            return true;
        }
        if age < WINDOW {
            return false;
        }
        let o = self.outcome();
        self.noticed.is_none() && !(o.slid() && !o.slid_back)
    }

    /// The judgement so far.
    pub fn outcome(&self) -> Outcome {
        classify(self.start.desktop_before, &self.changes, self.person)
    }

    /// True when the switch's incoming side was already shown.
    pub fn already_shown(&self) -> bool {
        self.already_shown
    }

    /// The outcome line's fields (after `slide-watch: outcome`): `key=value` words, `n/a` for
    /// what does not apply.
    pub fn line(&self) -> String {
        let s = &self.start;
        let o = self.outcome();
        let num = |v: Option<u64>| v.map_or_else(|| "n/a".to_string(), |v| v.to_string());
        let parked = s.parked.map_or("n/a", |p| if p { "true" } else { "false" });
        // The app activated wins over the one selected; `guessed=` says the one selected.
        let (bundle, guessed) = match &self.other {
            Some((_, other)) if !self.activated => (other.as_deref(), Some(s.bundle.as_deref().unwrap_or("n/a"))),
            _ => (s.bundle.as_deref(), None),
        };
        let mut line = format!(
            "via={} bundle={} parked={} park_ms={} press_to_release_ms={} put_home={} already_shown_fallback={} desktop_before={} desktop_after={} slid={} slid_back={} away_ms={} outcome={} by={} predicted={} app_here={} activated={} notices={}",
            s.via.name(),
            bundle.unwrap_or("n/a"),
            parked,
            num(s.park_ms),
            num(s.press_to_release_ms),
            s.put_home,
            self.already_shown,
            s.desktop_before,
            o.desktop_after,
            o.slid(),
            o.slid_back,
            num(o.away_ms),
            o.kind.name(),
            o.by.map_or("n/a", Person::name),
            num(s.predicted),
            s.app_here,
            self.activated || guessed.is_some(),
            self.notices,
        );
        if let Some(guessed) = guessed {
            line.push_str(&format!(" guessed={guessed}"));
        }
        if let Some(why) = s.why {
            line.push_str(&format!(" why=\"{why}\""));
        }
        line
    }
}

/// One `via`'s counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Activations judged.
    pub clicks: u64,
    /// Of them, with the window put home parked.
    pub parked: u64,
    /// Of them, whose switch found its incoming side already shown (the safety net).
    pub fell_back: u64,
    /// Of them, slid.
    pub slid: u64,
    /// Of the slides, back again.
    pub slid_back: u64,
    /// The slowest park, in milliseconds.
    pub slowest_park_ms: Option<u64>,
}

impl Counts {
    fn add(&mut self, w: &Watch) {
        let o = w.outcome();
        self.clicks += 1;
        self.parked += u64::from(w.start.parked == Some(true));
        self.fell_back += u64::from(w.already_shown);
        self.slid += u64::from(o.slid());
        self.slid_back += u64::from(o.slid() && o.slid_back);
        if let Some(ms) = w.start.park_ms {
            self.slowest_park_ms = Some(self.slowest_park_ms.map_or(ms, |s| s.max(ms)));
        }
    }

    fn words(&self, via: &str) -> String {
        format!(
            "{via}_clicks={} {via}_parked={} {via}_fell_back={} {via}_slid={} {via}_slid_back={} {via}_slowest_park_ms={}",
            self.clicks,
            self.parked,
            self.fell_back,
            self.slid,
            self.slid_back,
            self.slowest_park_ms.map_or_else(|| "n/a".to_string(), |v| v.to_string())
        )
    }

    fn json(&self) -> Value {
        json!({
            "clicks": self.clicks,
            "parked": self.parked,
            "fell_back": self.fell_back,
            "slid": self.slid,
            "slid_back": self.slid_back,
            "slowest_park_ms": self.slowest_park_ms,
        })
    }
}

/// The counts since the launch, by `via`, and when a summary is due.
#[derive(Debug, Clone, Default)]
pub struct Tally {
    /// Dock clicks.
    pub dock: Counts,
    /// ⌘Tab activations.
    pub cmdtab: Counts,
    /// Outcomes since the last summary.
    since_summary: u64,
    /// When the last summary was written (or the first outcome counted).
    last_summary: Option<Instant>,
}

impl Tally {
    /// Counts a judged watch at `now`.
    pub fn add(&mut self, w: &Watch, now: Instant) {
        match w.start.via {
            Via::Dock => self.dock.add(w),
            Via::CmdTab => self.cmdtab.add(w),
        }
        self.since_summary += 1;
        self.last_summary.get_or_insert(now);
    }

    /// True when an hour has passed since the last summary (or the first outcome after it)
    /// and an outcome was counted since.
    pub fn summary_due(&self, now: Instant) -> bool {
        self.since_summary > 0 && self.last_summary.is_some_and(|t| now.saturating_duration_since(t) >= SUMMARY_EVERY)
    }

    /// True when an outcome was counted since the last summary.
    pub fn active(&self) -> bool {
        self.since_summary > 0
    }

    /// The summary line's fields (after `slide-watch: summary`), counts since the launch;
    /// the hour starts again at `now`.
    pub fn summary_line(&mut self, at: &str, now: Instant) -> String {
        let since = self.since_summary;
        self.since_summary = 0;
        self.last_summary = Some(now);
        format!("at={at} new={since} {} {}", self.dock.words("dock"), self.cmdtab.words("cmdtab"))
    }

    /// The counters for `ctl state` (`slide_watch`).
    pub fn json(&self) -> Value {
        json!({ "dock": self.dock.json(), "cmdtab": self.cmdtab.json() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(at_ms: u64, from: u64, to: u64, expected: bool) -> Change {
        Change { at_ms, from, to, expected }
    }

    fn start(via: Via) -> Start {
        Start {
            via,
            bundle: Some("com.example.editor".into()),
            pid: 42,
            put_home: true,
            parked: Some(true),
            park_ms: Some(12),
            why: None,
            press_to_release_ms: Some(80),
            desktop_before: 1,
            predicted: None,
            app_here: true,
        }
    }

    #[test]
    fn no_change_is_none() {
        let o = classify(1, &[], None);
        assert_eq!(o.kind, Kind::None);
        assert_eq!(o.desktop_after, 1);
        assert!(!o.slid());
        // A notice recognized as the same Desktop is no change.
        assert_eq!(classify(1, &[change(400, 1, 1, false)], None).kind, Kind::None);
    }

    #[test]
    fn a_change_nobody_asked_for_is_a_slide_and_its_return_is_timed() {
        let o = classify(1, &[change(420, 1, 2, false)], None);
        assert_eq!((o.kind, o.slid_back, o.away_ms, o.desktop_after), (Kind::Slid, false, None, 2));
        let o = classify(1, &[change(420, 1, 2, false), change(1900, 2, 1, false)], None);
        assert_eq!((o.kind, o.slid_back, o.away_ms, o.desktop_after), (Kind::Slid, true, Some(1480), 1));
        // A later change elsewhere is no return.
        let o = classify(1, &[change(420, 1, 2, false), change(900, 2, 3, false)], None);
        assert!(o.slid() && !o.slid_back);
    }

    #[test]
    fn a_change_where_the_click_went_is_expected_and_a_slide_after_it_still_counts() {
        assert_eq!(classify(1, &[change(380, 1, 2, true)], None).kind, Kind::Expected);
        let o = classify(1, &[change(380, 1, 2, true), change(1000, 2, 3, false)], None);
        assert_eq!(o.kind, Kind::Slid);
        assert_eq!(o.desktop_after, 3);
    }

    #[test]
    fn the_persons_own_move_first_is_theirs() {
        let o = classify(1, &[change(700, 1, 2, false)], Some((300, Person::DesktopKey)));
        assert_eq!((o.kind, o.by), (Kind::Person, Some(Person::DesktopKey)));
        // Mission Control after the change does not excuse it.
        let o = classify(1, &[change(400, 1, 2, false)], Some((900, Person::MissionControl)));
        assert_eq!(o.kind, Kind::Slid);
    }

    #[test]
    fn a_change_past_the_window_is_not_the_activations() {
        let o = classify(1, &[change(1600, 1, 2, false)], None);
        assert_eq!(o.kind, Kind::None);
        assert_eq!(o.desktop_after, 2, "the Desktop shown last is still said");
    }

    #[test]
    fn the_watch_waits_for_a_notices_desktop_and_a_slides_return_within_its_cap() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut w = Watch::new(start(Via::Dock), t0);
        assert!(!w.due(at(1000)));
        assert!(w.due(at(1500)), "nothing happened: due at the window's end");
        w.notice(at(1400));
        assert!(!w.due(at(1600)), "a notice waits for its Desktop");
        w.arrived(at(1650), 1, 2, false);
        assert_eq!(w.changes[0].at_ms, 1400, "timed from its notice");
        assert!(!w.due(at(1700)), "a slide waits for its return");
        assert!(w.due(at(3000)), "not past the cap");
        w.notice(at(2000));
        w.arrived(at(2050), 2, 1, false);
        assert!(w.due(at(2100)));
        let o = w.outcome();
        assert!(o.slid() && o.slid_back);
        assert_eq!(o.away_ms, Some(600));
    }

    #[test]
    fn the_line_has_every_field_with_n_a_for_what_does_not_apply() {
        let t0 = Instant::now();
        let mut s = start(Via::CmdTab);
        s.parked = None;
        s.park_ms = None;
        s.put_home = false;
        s.bundle = None;
        let mut w = Watch::new(s, t0);
        w.activated(42, None);
        assert_eq!(
            w.line(),
            "via=cmdtab bundle=n/a parked=n/a park_ms=n/a press_to_release_ms=80 put_home=false already_shown_fallback=false desktop_before=1 desktop_after=1 slid=false slid_back=false away_ms=n/a outcome=none by=n/a predicted=n/a app_here=true activated=true notices=0"
        );
        let mut s = start(Via::Dock);
        s.parked = Some(false);
        s.why = Some("the park did not take in time");
        let mut w = Watch::new(s, t0);
        w.shown_early();
        w.notice(t0 + Duration::from_millis(400));
        w.arrived(t0 + Duration::from_millis(460), 1, 2, false);
        let line = w.line();
        assert!(line.starts_with("via=dock bundle=com.example.editor parked=false park_ms=12 "), "{line}");
        assert!(line.contains(" already_shown_fallback=true desktop_before=1 desktop_after=2 slid=true slid_back=false "), "{line}");
        assert!(line.ends_with(" notices=1 why=\"the park did not take in time\""), "{line}");
    }

    /// Live 11-18: ⌘Tab's selection was a guess (Mail) while a click activated VS Code; the
    /// line named Mail. The app activated wins, the guess is said after it.
    #[test]
    fn after_cmd_tab_the_line_names_the_app_activated() {
        let t0 = Instant::now();
        let mut s = start(Via::CmdTab);
        s.bundle = Some("com.example.mail".into());
        let mut w = Watch::new(s.clone(), t0);
        w.activated(7, Some("com.example.editor"));
        w.activated(42, None);
        assert_eq!(w.pid(), 42, "its app activated after all: the selection stands");
        assert!(w.line().starts_with("via=cmdtab bundle=com.example.mail "), "{}", w.line());
        assert!(!w.line().contains("guessed="));
        let mut w = Watch::new(s.clone(), t0);
        w.activated(7, Some("com.example.editor"));
        w.activated(8, Some("com.example.notes"));
        assert_eq!(w.pid(), 7, "the first other app activated");
        let line = w.line();
        assert!(line.starts_with("via=cmdtab bundle=com.example.editor "), "{line}");
        assert!(line.contains(" activated=true ") && line.ends_with(" guessed=com.example.mail"), "{line}");
        // The same activation is its app's: no guess said.
        let mut w = Watch::new(s.clone(), t0);
        w.activated(42, None);
        assert!(w.line().contains(" activated=true notices=0") && !w.line().contains("guessed="));
        // A Dock click names the app clicked, whatever else is activated.
        let mut w = Watch::new(start(Via::Dock), t0);
        w.activated(7, Some("com.example.mail"));
        assert_eq!(w.pid(), 42);
        assert!(w.line().starts_with("via=dock bundle=com.example.editor ") && w.line().contains(" activated=false "));
    }

    #[test]
    fn the_tally_counts_by_via_and_is_summarized_hourly_while_active() {
        let t0 = Instant::now();
        let mut tally = Tally::default();
        assert!(!tally.active());
        assert!(!tally.summary_due(t0 + SUMMARY_EVERY));
        let mut slid = Watch::new(start(Via::Dock), t0);
        slid.notice(t0 + Duration::from_millis(400));
        slid.arrived(t0 + Duration::from_millis(450), 1, 2, false);
        slid.notice(t0 + Duration::from_millis(900));
        slid.arrived(t0 + Duration::from_millis(950), 2, 1, false);
        tally.add(&slid, t0);
        let mut fell = start(Via::Dock);
        fell.parked = Some(false);
        fell.park_ms = Some(31);
        let mut fell = Watch::new(fell, t0);
        fell.shown_early();
        tally.add(&fell, t0);
        let mut tab = start(Via::CmdTab);
        tab.park_ms = Some(7);
        tally.add(&Watch::new(tab, t0), t0);
        assert_eq!(tally.dock, Counts { clicks: 2, parked: 1, fell_back: 1, slid: 1, slid_back: 1, slowest_park_ms: Some(31) });
        assert_eq!(tally.cmdtab, Counts { clicks: 1, parked: 1, fell_back: 0, slid: 0, slid_back: 0, slowest_park_ms: Some(7) });
        assert!(!tally.summary_due(t0 + Duration::from_secs(60)));
        assert!(tally.summary_due(t0 + SUMMARY_EVERY));
        let line = tally.summary_line("hour", t0 + SUMMARY_EVERY);
        assert_eq!(
            line,
            "at=hour new=3 dock_clicks=2 dock_parked=1 dock_fell_back=1 dock_slid=1 dock_slid_back=1 dock_slowest_park_ms=31 cmdtab_clicks=1 cmdtab_parked=1 cmdtab_fell_back=0 cmdtab_slid=0 cmdtab_slid_back=0 cmdtab_slowest_park_ms=7"
        );
        assert!(!tally.active(), "counted again from the summary");
        assert!(!tally.summary_due(t0 + SUMMARY_EVERY * 3), "no activity since: no summary");
        assert_eq!(tally.json()["dock"]["slid"], 1);
        assert_eq!(tally.json()["cmdtab"]["slowest_park_ms"], 7);
    }
}
