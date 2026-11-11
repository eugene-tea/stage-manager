//! Windows left in the park zone after an unpark, decided without touching anything (6X).
//!
//! The owner's report (10-13): "When you quit the app, I still saw some pieces of windows in
//! the corner." Quit unparked every window the model knew, then read the window server's
//! list, found windows still in the park zone and only warned: nothing tracked them (a window
//! macOS put back in a corner after a display change, one an earlier run left there, one
//! whose app answered late), and the exit counted as clean, so the guardian did nothing
//! either. Now every unpark path ends with a look at the list ([`settle`]): each window still
//! parked is brought back ([`leftover_frame`]: its registered frame, else the frame the unpark
//! gave it, else its `state.json` frame, else centered); one that cannot be reached (another Desktop's, an app that does
//! not answer) leaves the exit unclean, so the guardian waits for it.
//!
//! A restore an app is too busy to answer (`kAXErrorCannotComplete`, the 0.25 s messaging
//! timeout) is tried again a few times, quickly ([`retry_quickly`]), and counts as done as
//! soon as the window server lists the window out of the park zone: a busy app often carries
//! out the request a moment after the timeout.

use std::collections::BTreeSet;
use std::time::Duration;

use super::geometry::Rect;
use super::persist::StateFile;
use super::recovery::{FrameSource, LiveWindow, ScanDisplays, plan_sourced, safe_restore};

/// What a quit's unpark and its checks left away, counted ([`quit_end`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuitLeft {
    /// Windows the leftovers check left in a park zone: this app's own (not handed over),
    /// ordered-out children aside.
    pub leftovers: usize,
    /// Windows whose restore failed and that are not seen back, ordered-out children aside.
    pub unrestored: usize,
    /// Windows still in the registry, ordered-out children aside.
    pub registered: usize,
    /// Windows taken from an old guardian still away, ordered-out children aside.
    pub taken: usize,
    /// Child windows their apps ordered out while parked
    /// ([`super::not_ours::ordered_out_child`]) that are still on record.
    pub ordered_out: usize,
}

/// Counts what a quit left: `back` (its leftovers check found no window in a park zone; else
/// `left_not_taken` of them were this app's own, ordered-out children aside), the windows
/// whose restore failed (`unrestored`), the registry's (`registry`) and the taken ones still
/// away (`taken`), each without the children judged `ordered_out`, which are counted apart
/// while any record still holds them.
pub fn quit_left(
    back: bool,
    left_not_taken: usize,
    unrestored: &BTreeSet<u32>,
    registry: &BTreeSet<u32>,
    taken: &BTreeSet<u32>,
    ordered_out: &BTreeSet<u32>,
) -> QuitLeft {
    let besides = |ids: &BTreeSet<u32>| ids.difference(ordered_out).count();
    let on_record = |id: &&u32| unrestored.contains(id) || registry.contains(id) || taken.contains(id);
    QuitLeft {
        leftovers: if back { 0 } else { left_not_taken },
        unrestored: besides(unrestored),
        registered: besides(registry),
        taken: besides(taken),
        ordered_out: ordered_out.iter().filter(on_record).count(),
    }
}

/// How a quit ends for the windows it left ([`quit_end`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitEnd {
    /// Every window is back: the clean-exit marker is written, the guardian has no work.
    Clean,
    /// Only children their apps ordered out while parked are left: nothing can bring one
    /// back, so the quit counts as clean for the user (no error, a ready update installs),
    /// but no clean-exit marker is written: the guardian keeps waiting for each, as for any
    /// registered window.
    Waits,
    /// Only windows handed over by an old guardian are left (ordered-out children aside):
    /// passed on to this app's guardian, a warning.
    PassedOn,
    /// A window this app parked is still away: an error, and the guardian brings it back.
    Unclean,
}

impl QuitEnd {
    /// Whether the clean-exit marker is written (the guardian then does nothing).
    pub fn marks_clean(self) -> bool {
        self == QuitEnd::Clean
    }

    /// Whether the quit is clean in the user's eyes: no error, and the user's own Quit
    /// installs a ready update.
    pub fn clean_for_user(self) -> bool {
        matches!(self, QuitEnd::Clean | QuitEnd::Waits)
    }
}

/// How a quit ends for what it `left`. A registered window always waits: any window on
/// record, an ordered-out child too, keeps the clean-exit marker unwritten.
pub fn quit_end(left: QuitLeft) -> QuitEnd {
    if left.leftovers > 0 || left.unrestored > 0 || left.registered > 0 {
        QuitEnd::Unclean
    } else if left.taken > 0 {
        QuitEnd::PassedOn
    } else if left.ordered_out > 0 {
        QuitEnd::Waits
    } else {
        QuitEnd::Clean
    }
}

/// After an unpark, how long the window server may take to list the windows in place before
/// one still in the park zone counts as left there (it lists the old frames for a few
/// milliseconds after the moves).
pub const UNPARK_SETTLE: Duration = Duration::from_secs(1);

/// How often the window list is read again during [`UNPARK_SETTLE`].
pub const UNPARK_POLL: Duration = Duration::from_millis(50);

/// The most a quit spends bringing back windows still parked after its unpark (the reads
/// before and after come on top: [`LEFTOVERS_MAX`]).
pub const LEFTOVER_BUDGET: Duration = Duration::from_secs(2);

/// The most the whole leftovers check may add to a quit: a settle, the budget, a settle.
/// Enforced as a deadline: the second settle gets what is left.
pub const LEFTOVERS_MAX: Duration = Duration::from_secs(4);

/// The longest one Accessibility call may take: the messaging timeout the WM thread sets on
/// the system-wide element (`wm_thread::AX_TIMEOUT_SECONDS`, the whole process's).
pub const AX_CALL_MAX: Duration = Duration::from_millis(250);

/// The longest one restore try may take: size, position, size, each to the timeout.
pub const TRY_MAX: Duration = Duration::from_millis(750);

/// A stale element's once-only fresh lookup (the app's window list, reaching the window in
/// it) and the try through what it finds, budgeted apart from the window's
/// [`RESTORE_RETRY`] (the review of 6X2, 1): that budget is for a busy app, and a stale
/// element fails at once, without the app. Its own budget must fit the lookup and a try
/// ([`leftover_fits`]), or the lookup at quit never runs.
pub const REFRESH_BUDGET: Duration = AX_CALL_MAX.saturating_mul(2).saturating_add(TRY_MAX);

/// The most one window's restore may take in an unpark (quit, stand-down): a busy app's
/// quick retries ([`RESTORE_RETRY`]), or a stale element's failed try (one call) and its
/// fresh lookup ([`REFRESH_BUDGET`]), whichever is longer. What quit's timeout allows a
/// window.
pub fn window_restore_max() -> Duration {
    TRY_MAX.max(RESTORE_RETRY.budget).max(AX_CALL_MAX + REFRESH_BUDGET)
}

/// The most the registry's unpark spends trying busy apps again (all its windows together,
/// [`RESTORE_RETRY`] each): once spent, each window left gets its one try, no retry.
pub const REGISTRY_RETRY_BUDGET: Duration = Duration::from_secs(2);

/// Asks `probe` until it answers nothing or `max` has passed (`elapsed`: time since the
/// settle began, its reads included), sleeping `step` between asks but never past `max`;
/// returns its last answer (what is still there after the settle).
pub fn settle<T>(
    mut probe: impl FnMut() -> Vec<T>,
    max: Duration,
    step: Duration,
    mut elapsed: impl FnMut() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> Vec<T> {
    loop {
        let found = probe();
        let spent = elapsed();
        if found.is_empty() || spent >= max {
            return found;
        }
        sleep(step.min(max - spent));
    }
}

/// How long a leftovers check lets the window server's list settle before its first read:
/// [`UNPARK_SETTLE`] when windows were moved a moment ago (`moved`: the unpark of a quit, a
/// stand-down or a logout), none for a later look whose caller has waited that long already
/// without holding the window thread (a Desktop change while a window is still away, looked
/// at on a later tick). Slept out here, the wait held the thread a second at every Desktop
/// change for as long as that window stayed away.
pub fn settle_before(moved: bool) -> Duration {
    if moved { UNPARK_SETTLE } else { Duration::ZERO }
}

/// ... and before its second read, after bringing windows back: [`UNPARK_SETTLE`] after an
/// unpark (`moved`, as before: a move asked then may still land) or once the check asked a
/// window to move itself (`tried`: how many); none when it could reach none of them (another
/// Desktop's): nothing was asked, so nothing can land.
pub fn settle_after(moved: bool, tried: usize) -> Duration {
    if moved || tried > 0 { UNPARK_SETTLE } else { Duration::ZERO }
}

/// True when a window still parked may be tried with `left` of the leftovers' budget: its
/// try ([`TRY_MAX`]) and, for one the registry does not know, reaching it through its app's
/// list and reading its title (an Accessibility call each) must fit. Never a try that would
/// end past the budget (the review of 6X, L1).
pub fn leftover_fits(left: Duration, registered: bool) -> bool {
    let reach = if registered { Duration::ZERO } else { AX_CALL_MAX * 2 };
    left >= reach + TRY_MAX
}

/// What this run remembers of a window's frame when the leftovers check finds it still in
/// the park zone, freshest first (S1: the unpark's own frame used to be dropped with its
/// registry entry before the check, so a window whose app carried the move out late, or only
/// its size, went back to an older `state.json` frame or the center).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Remembered {
    /// The safety registry's frame (a window still parked, or one whose restore failed).
    pub registry: Option<Rect>,
    /// The frame the unpark just restored it to (it left the registry as done).
    pub unparked: Option<Rect>,
    /// The frame an old guardian handed it over with (round 8N).
    pub taken: Option<Rect>,
}

impl Remembered {
    /// The freshest frame remembered, with where it comes from.
    pub fn first(&self) -> Option<(Rect, LeftoverSource)> {
        self.registry
            .map(|r| (r, LeftoverSource::Registry))
            .or(self.unparked.map(|r| (r, LeftoverSource::Unparked)))
            .or(self.taken.map(|r| (r, LeftoverSource::Registry)))
    }
}

/// Where a window still parked after the unpark goes back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftoverSource {
    /// The safety registry's frame for it (a window we parked, whose restore failed), or the
    /// frame an old guardian handed it over with.
    Registry,
    /// The frame the unpark restored it to a moment before (its app carried the move out
    /// late, or not at all).
    Unparked,
    /// Its window's frame in `state.json`.
    Saved,
    /// Nothing known: centered on the visible frame.
    Centered,
}

impl LeftoverSource {
    /// A short name for the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::Registry => "registry",
            Self::Unparked => "unpark",
            Self::Saved => "state.json",
            Self::Centered => "centered",
        }
    }
}

/// The frame a window still parked goes back to: the freshest frame this run `remembered`
/// (made safe), else its `state.json` frame (its window id first, then exact bundle id and
/// title, then bundle id when one candidate is left: [`super::window_match`]), else
/// centered; always on the visible frame and never in a park zone.
pub fn leftover_frame(remembered: Remembered, live: &LiveWindow, state: Option<&StateFile>, displays: &ScanDisplays) -> (Rect, LeftoverSource) {
    if let Some((r, source)) = remembered.first() {
        return (safe_restore(r, displays), source);
    }
    match plan_sourced(std::slice::from_ref(live), state, displays).first() {
        // No registered frames are given here: the caller's come first (`remembered`).
        Some((_, frame, FrameSource::Saved | FrameSource::Registered)) => (*frame, LeftoverSource::Saved),
        Some((_, frame, FrameSource::Centered)) => (*frame, LeftoverSource::Centered),
        // The window server's frame and the scan's park test disagree (it was listed in the
        // zone): centered all the same.
        None => (safe_restore(Rect::new(f64::NAN, 0.0, live.frame.w, live.frame.h), displays), LeftoverSource::Centered),
    }
}

/// How a restore is tried again while its app is busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuickRetry {
    /// Tries in all, the first included.
    pub tries: u32,
    /// Wait between two tries (the window server is read after it).
    pub gap: Duration,
    /// No new try that would end after this much time since the first began.
    pub budget: Duration,
}

impl QuickRetry {
    /// The same retry with at most `left` of time (a caller's deadline): a shorter budget.
    pub fn within(self, left: Duration) -> Self {
        Self { budget: self.budget.min(left), ..self }
    }
}

/// The restore retry: three tries 75 ms apart, all within 1.2 s (a hung app costs up to 0.5 s
/// a try: size and position, each to the messaging timeout, so it gets two).
pub const RESTORE_RETRY: QuickRetry = QuickRetry { tries: 3, gap: Duration::from_millis(75), budget: Duration::from_millis(1200) };

/// What [`retry_quickly`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retried<E> {
    /// The outcome: `Ok` when a try went through or the window was seen back.
    pub result: Result<(), E>,
    /// Tries made.
    pub tries: u32,
    /// A try failed but the window server listed the window back afterwards (the app carried
    /// out the request late).
    pub landed_late: bool,
}

/// Runs `attempt`; while it fails with an error `busy` accepts (CannotComplete), waits
/// `policy.gap`, asks `landed` (the window server lists the window out of the park zone: the
/// earlier request went through late) and tries again, up to `policy.tries` tries, and never
/// one that would end after `policy.budget` judged by the last try's cost (`elapsed`: time
/// since the first try began), nor a wait that would. Any other error ends at once. The
/// first try is always made: a caller with a deadline decides beforehand whether it fits.
pub fn retry_quickly<E>(
    policy: QuickRetry,
    mut attempt: impl FnMut() -> Result<(), E>,
    busy: impl Fn(&E) -> bool,
    mut landed: impl FnMut() -> bool,
    mut elapsed: impl FnMut() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> Retried<E> {
    let mut tries = 0;
    loop {
        let began = elapsed();
        tries += 1;
        let err = match attempt() {
            Ok(()) => return Retried { result: Ok(()), tries, landed_late: false },
            Err(e) => e,
        };
        let cost = elapsed().saturating_sub(began);
        if !busy(&err) || tries >= policy.tries.max(1) || elapsed() + policy.gap > policy.budget {
            return Retried { result: Err(err), tries, landed_late: false };
        }
        sleep(policy.gap);
        if landed() {
            return Retried { result: Ok(()), tries, landed_late: true };
        }
        // The next try would take about as long as this one did.
        if elapsed() + cost > policy.budget {
            return Retried { result: Err(err), tries, landed_late: false };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child its app ordered out while parked keeps the guardian waiting (no clean-exit
    /// marker) without making the quit an error or stopping an update at the user's Quit.
    #[test]
    fn an_ordered_out_child_keeps_the_guardian_waiting_but_not_the_quit_unclean() {
        let none = QuitLeft::default();
        assert_eq!(quit_end(none), QuitEnd::Clean);
        let waits = quit_end(QuitLeft { ordered_out: 1, ..none });
        assert_eq!(waits, QuitEnd::Waits);
        assert!(!waits.marks_clean(), "a registered window always waits: the guardian stays");
        assert!(waits.clean_for_user(), "no error, and the user's Quit installs a ready update");
        // Any other window left is counted as before, whatever else is ordered out.
        for left in [QuitLeft { leftovers: 1, ..none }, QuitLeft { unrestored: 1, ..none }, QuitLeft { registered: 1, ordered_out: 1, ..none }] {
            assert_eq!(quit_end(left), QuitEnd::Unclean, "{left:?}");
            assert!(!quit_end(left).clean_for_user() && !quit_end(left).marks_clean());
        }
        let passed = quit_end(QuitLeft { taken: 2, ordered_out: 1, ..none });
        assert_eq!(passed, QuitEnd::PassedOn);
        assert!(!passed.clean_for_user() && !passed.marks_clean());
        assert!(QuitEnd::Clean.marks_clean() && QuitEnd::Clean.clean_for_user());
    }

    /// The 11-06 quit, counted: one window (a browser's child panel) unrestored and in the
    /// registry, left by the leftovers check and judged ordered out.
    #[test]
    fn the_quits_counts_leave_ordered_out_children_aside() {
        let set = |ids: &[u32]| ids.iter().copied().collect::<BTreeSet<u32>>();
        let (panel, none) = (set(&[697]), set(&[]));
        let left = quit_left(false, 0, &panel, &panel, &none, &panel);
        assert_eq!(left, QuitLeft { ordered_out: 1, ..QuitLeft::default() });
        assert_eq!(quit_end(left), QuitEnd::Waits);
        // Not judged so (its app did not answer, a live child on another Desktop): as before.
        let as_before = quit_left(false, 1, &panel, &panel, &none, &none);
        assert_eq!(as_before, QuitLeft { leftovers: 1, unrestored: 1, registered: 1, ..QuitLeft::default() });
        assert_eq!(quit_end(as_before), QuitEnd::Unclean);
        // Another window left beside it still makes the quit unclean.
        assert_eq!(quit_end(quit_left(false, 1, &set(&[12, 697]), &set(&[12, 697]), &none, &panel)), QuitEnd::Unclean);
        // Handed over by an old guardian and found the same way again: not passed on as an
        // error either; a live handed-over window beside it is passed on as before.
        assert_eq!(quit_end(quit_left(false, 0, &none, &none, &panel, &panel)), QuitEnd::Waits);
        assert_eq!(quit_end(quit_left(false, 0, &none, &none, &set(&[40, 697]), &panel)), QuitEnd::PassedOn);
        // Judged ordered out, then let go by every record (seen gone since): a clean quit.
        assert_eq!(quit_end(quit_left(true, 0, &none, &none, &none, &panel)), QuitEnd::Clean);
    }
    use crate::engine::park::{Corner, is_parked};
    use crate::engine::persist::{SavedRole, SavedSpace, SavedStage, SavedWindow};
    use crate::engine::recovery::{Known, is_stranded};

    const MAIN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    const VISIBLE: Rect = Rect::new(0.0, 30.0, 2560.0, 1410.0);

    fn displays() -> ScanDisplays {
        ScanDisplays::new(MAIN, VISIBLE, vec![], [Corner::BottomRight])
    }

    /// A later look at windows still away (a Desktop change, its settle waited out on the
    /// ticks) reads the list as it stands: with a window out of reach on another Desktop each
    /// of the two reads used to wait out its whole settle, two seconds of the window thread at
    /// every Desktop change. After an unpark, and once the check itself asked a window to
    /// move, the list still gets its settle.
    #[test]
    fn a_later_look_with_nothing_moved_does_not_wait_for_the_list() {
        assert_eq!(settle_before(true), UNPARK_SETTLE, "after an unpark");
        assert_eq!(settle_before(false), Duration::ZERO, "the caller waited on its ticks");
        assert_eq!(settle_after(true, 0), UNPARK_SETTLE, "after an unpark, as before");
        assert_eq!(settle_after(false, 1), UNPARK_SETTLE, "a window was asked to move");
        assert_eq!(settle_after(false, 0), Duration::ZERO, "none could be reached: nothing to land");
        // A settle of no time is one read.
        let mut reads = 0;
        let found = settle(|| { reads += 1; vec![7] }, settle_before(false), UNPARK_POLL, || Duration::ZERO, |_| panic!("no wait"));
        assert_eq!((found, reads), (vec![7], 1));
    }

    fn live(bundle: &str, title: &str, frame: Rect) -> LiveWindow {
        LiveWindow { id: None, bundle_id: bundle.into(), title: title.into(), frame }
    }

    fn state(windows: Vec<SavedWindow>) -> StateFile {
        StateFile::single(SavedSpace {
            id: 1,
            current: true,
            stages: vec![SavedStage { name: None, windows, ..Default::default() }],
            active_index: Some(0),
            ..Default::default()
        })
    }

    fn saved(bundle: &str, title: &str, frame: Rect) -> SavedWindow {
        SavedWindow { bundle_id: bundle.into(), title: title.into(), frame, z: 0, key: false, pinned: false, role: SavedRole::Standard, original: None, window: None, ..Default::default() }
    }

    // ----- the settle ------------------------------------------------------------------

    /// [`settle`] on a fake clock: each read costs `read`; returns the answer, the reads made
    /// and the time spent.
    fn settled(answers: impl Fn(u32) -> Vec<u32>, max: Duration, read: Duration) -> (Vec<u32>, u32, Duration) {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let reads = std::cell::Cell::new(0);
        let left = settle(
            || {
                reads.set(reads.get() + 1);
                clock.set(clock.get() + read);
                answers(reads.get())
            },
            max,
            UNPARK_POLL,
            || clock.get(),
            |d| clock.set(clock.get() + d),
        );
        (left, reads.get(), clock.get())
    }

    #[test]
    fn windows_listed_in_place_after_a_few_reads_are_not_left() {
        // The window server lists the old frames for the first reads after the moves.
        let (left, reads, took) = settled(|n| if n <= 3 { vec![4551, 4550] } else { Vec::new() }, UNPARK_SETTLE, Duration::ZERO);
        assert!(left.is_empty(), "no false warning");
        assert_eq!(reads, 4);
        assert_eq!(took, UNPARK_POLL * 3);
    }

    #[test]
    fn a_window_really_still_parked_is_reported_after_the_settle() {
        let (left, _, took) = settled(|_| vec![4600], UNPARK_SETTLE, Duration::ZERO);
        assert_eq!(left, [4600]);
        assert_eq!(took, UNPARK_SETTLE, "waited a second, not a poll more");
    }

    /// The review of 6X, L1: the reads count against the settle's time too.
    #[test]
    fn slow_reads_count_against_the_settle() {
        let (left, reads, took) = settled(|_| vec![4600], UNPARK_SETTLE, Duration::from_millis(200));
        assert_eq!(left, [4600]);
        assert!(reads <= 5, "{reads} reads");
        assert!(took <= UNPARK_SETTLE + Duration::from_millis(200), "{took:?}");
    }

    #[test]
    fn nothing_parked_answers_at_once() {
        let (left, reads, took) = settled(|_| Vec::new(), UNPARK_SETTLE, Duration::ZERO);
        assert!(left.is_empty());
        assert_eq!((reads, took), (1, Duration::ZERO));
    }

    #[test]
    fn the_whole_check_stays_within_its_bound() {
        assert!(UNPARK_SETTLE * 2 + LEFTOVER_BUDGET <= LEFTOVERS_MAX);
        assert_eq!(TRY_MAX, AX_CALL_MAX * 3);
    }

    /// The review of 6X, L1: a leftover is tried only when its try (and reaching it, for one
    /// the registry does not know) fits in what is left of the budget.
    #[test]
    fn a_leftover_is_tried_only_when_its_try_fits() {
        assert!(leftover_fits(LEFTOVER_BUDGET, false));
        assert!(leftover_fits(TRY_MAX, true));
        assert!(!leftover_fits(TRY_MAX, false), "reaching it and its title come first");
        assert!(leftover_fits(TRY_MAX + AX_CALL_MAX * 2, false));
        assert!(!leftover_fits(TRY_MAX - Duration::from_millis(1), true));
        assert!(!leftover_fits(Duration::ZERO, true));
    }

    /// The review of 6X2, 1: the quit's fresh lookup for a stale element has a budget of its
    /// own that fits the lookup and its try (the window's busy-app budget, 1.2 s, never did:
    /// the gate wanted 1.25 s), and quit's timeout allows a window that much.
    #[test]
    fn a_stale_elements_lookup_fits_its_own_budget() {
        assert!(leftover_fits(REFRESH_BUDGET, false));
        assert!(!leftover_fits(REFRESH_BUDGET - Duration::from_millis(1), false), "no slack hidden in it");
        assert!(window_restore_max() >= RESTORE_RETRY.budget);
        assert!(window_restore_max() >= AX_CALL_MAX + REFRESH_BUDGET);
        assert!(window_restore_max() >= TRY_MAX);
    }

    // ----- where a leftover goes ---------------------------------------------------------

    /// The owner's log, 14:30:58: Mail and Messages still parked at `2559,1388` after the
    /// unpark, known to no registry.
    #[test]
    fn a_leftover_goes_to_its_registered_frame_else_its_saved_one_else_the_center() {
        let d = displays();
        let parked = Rect::new(2559.0, 1388.0, 1621.0, 1183.0);
        assert!(is_stranded(parked, &d, Known::Unknown));
        let mail = live("com.apple.mail", "Inbox", parked);

        let registered = Rect::new(67.0, 39.0, 1621.0, 1130.0);
        assert_eq!(leftover_frame(reg(registered), &mail, None, &d), (registered, LeftoverSource::Registry));

        let st = state(vec![saved("com.apple.mail", "Inbox", Rect::new(299.0, 39.0, 1401.0, 1130.0))]);
        assert_eq!(leftover_frame(Remembered::default(), &mail, Some(&st), &d), (Rect::new(299.0, 39.0, 1401.0, 1130.0), LeftoverSource::Saved));

        let (f, source) = leftover_frame(Remembered::default(), &live("com.apple.MobileSMS", "Chat", Rect::new(2559.0, 1388.0, 1051.0, 906.0)), Some(&st), &d);
        assert_eq!(source, LeftoverSource::Centered);
        assert!(VISIBLE.contains_rect(&f) && !d.parks(f), "{f:?}");
        assert_eq!((f.w, f.h), (1051.0, 906.0), "its size kept");
    }

    fn reg(r: Rect) -> Remembered {
        Remembered { registry: Some(r), ..Remembered::default() }
    }

    /// S1 (the review of 10-29): at quit the unpark restored Mail to its frame and
    /// dropped its registry entry; Mail carried the move out late, so the check found it in
    /// the zone. It goes back to the frame the unpark gave it, not to an older `state.json`
    /// frame (read before the save) nor the center; a failed restore's registry frame and a
    /// handed-over frame keep their places around it.
    #[test]
    fn a_leftover_the_unpark_restored_goes_back_to_the_unparks_frame() {
        let d = displays();
        let mail = live("com.apple.mail", "Inbox", Rect::new(2559.0, 1388.0, 1621.0, 1183.0));
        let fresh = Rect::new(120.0, 60.0, 1500.0, 1100.0);
        let older = Rect::new(299.0, 39.0, 1401.0, 1130.0);
        let st = state(vec![saved("com.apple.mail", "Inbox", older)]);
        let unparked = Remembered { unparked: Some(fresh), ..Remembered::default() };
        assert_eq!(leftover_frame(unparked, &mail, Some(&st), &d), (fresh, LeftoverSource::Unparked));
        let renamed = live("com.apple.mail", "Inbox (3)", mail.frame);
        assert_eq!(leftover_frame(unparked, &renamed, None, &d), (fresh, LeftoverSource::Unparked), "never centered");
        let failed = Rect::new(80.0, 50.0, 1500.0, 1100.0);
        let both = Remembered { registry: Some(failed), unparked: Some(fresh), taken: Some(older) };
        assert_eq!(leftover_frame(both, &mail, Some(&st), &d), (failed, LeftoverSource::Registry), "the registry first");
        let taken = Remembered { unparked: Some(fresh), taken: Some(older), ..Remembered::default() };
        assert_eq!(taken.first(), Some((fresh, LeftoverSource::Unparked)), "this run's unpark before an old guardian's frame");
    }

    #[test]
    fn a_registered_frame_in_the_park_zone_is_never_used_as_is() {
        let d = displays();
        let w = live("com.example.a", "Draft", Rect::new(2559.0, 1408.0, 800.0, 600.0));
        let (f, source) = leftover_frame(reg(Rect::new(2559.0, 1408.0, 800.0, 600.0)), &w, None, &d);
        assert_eq!(source, LeftoverSource::Registry);
        assert!(!is_parked(f, MAIN) && VISIBLE.contains_rect(&f), "{f:?}");
    }

    #[test]
    fn a_leftover_the_scan_does_not_count_parked_is_centered_all_the_same() {
        let d = displays();
        let w = live("com.example.a", "Draft", Rect::new(400.0, 300.0, 800.0, 600.0));
        let (f, source) = leftover_frame(Remembered::default(), &w, None, &d);
        assert_eq!(source, LeftoverSource::Centered);
        assert!(VISIBLE.contains_rect(&f) && !d.parks(f));
    }

    // ----- the quick retry ---------------------------------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum E {
        Busy,
        Other,
    }

    /// Runs [`retry_quickly`] with `answers` for the tries and `lands` for the window
    /// server's reads; each try costs `cost` of the clock.
    fn run(answers: &[Result<(), E>], lands: &[bool], cost: Duration) -> (Retried<E>, Duration) {
        let mut a = answers.iter().copied();
        let mut l = lands.iter().copied();
        let clock = std::cell::Cell::new(Duration::ZERO);
        let r = retry_quickly(
            RESTORE_RETRY,
            || {
                clock.set(clock.get() + cost);
                a.next().unwrap_or(Err(E::Busy))
            },
            |e| *e == E::Busy,
            || l.next().unwrap_or(false),
            || clock.get(),
            |d| clock.set(clock.get() + d),
        );
        (r, clock.get())
    }

    /// The owner's log, 14:25:29: three restores failed with CannotComplete, the slivers
    /// stayed ~1.5 s until the registry's fallback.
    #[test]
    fn a_busy_app_is_tried_again_quickly() {
        let (r, took) = run(&[Err(E::Busy), Ok(())], &[false], Duration::from_millis(500));
        assert_eq!(r, Retried { result: Ok(()), tries: 2, landed_late: false });
        assert_eq!(took, Duration::from_millis(1075));
    }

    #[test]
    fn a_request_carried_out_late_counts_as_done() {
        let (r, took) = run(&[Err(E::Busy)], &[true], Duration::from_millis(500));
        assert_eq!(r, Retried { result: Ok(()), tries: 1, landed_late: true });
        assert_eq!(took, Duration::from_millis(575), "no second try");
    }

    #[test]
    fn a_hung_app_stops_at_the_budget() {
        let (r, took) = run(&[], &[], Duration::from_millis(500));
        assert_eq!(r.result, Err(E::Busy));
        assert_eq!(r.tries, 2, "the third would end after 1.2 s");
        assert!(took <= RESTORE_RETRY.budget, "{took:?}");
        // A fast-failing busy app gets every try.
        let (r, _) = run(&[], &[], Duration::from_millis(5));
        assert_eq!(r.tries, RESTORE_RETRY.tries);
    }

    /// The review of 6X, L1: a deadline shorter than the retry's budget is a bound: no try
    /// and no wait that would end past it.
    #[test]
    fn a_deadline_bounds_the_retry() {
        let left = Duration::from_millis(600);
        let mut a = std::iter::repeat(Err::<(), E>(E::Busy));
        let clock = std::cell::Cell::new(Duration::ZERO);
        let r = retry_quickly(
            RESTORE_RETRY.within(left),
            || {
                clock.set(clock.get() + Duration::from_millis(250));
                a.next().unwrap_or(Err(E::Busy))
            },
            |e| *e == E::Busy,
            || false,
            || clock.get(),
            |d| clock.set(clock.get() + d),
        );
        assert_eq!(r.tries, 2, "250 + 75 + 250 fits, a third would not");
        assert!(clock.get() <= left, "{:?}", clock.get());
        assert_eq!(RESTORE_RETRY.within(Duration::from_secs(9)), RESTORE_RETRY);
        // No room for the wait: one try only.
        let r = retry_quickly(RESTORE_RETRY.within(Duration::from_millis(300)), || Err(E::Busy), |e| *e == E::Busy, || true, || Duration::from_millis(250), |_| {});
        assert_eq!(r, Retried { result: Err(E::Busy), tries: 1, landed_late: false });
    }

    #[test]
    fn other_errors_and_successes_are_not_retried() {
        let (r, _) = run(&[Err(E::Other)], &[], Duration::from_millis(5));
        assert_eq!(r, Retried { result: Err(E::Other), tries: 1, landed_late: false });
        let (r, took) = run(&[Ok(())], &[], Duration::from_millis(5));
        assert_eq!(r, Retried { result: Ok(()), tries: 1, landed_late: false });
        assert_eq!(took, Duration::from_millis(5));
    }
}
