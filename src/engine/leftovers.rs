//! Windows left in the park zone after an unpark, decided without touching anything (6X).
//!
//! The owner's report (10-13): "When you quit the app, I still saw some pieces of windows in
//! the corner." Quit unparked every window the model knew, then read the window server's
//! list, found windows still in the park zone and only warned: nothing tracked them (a window
//! macOS put back in a corner after a display change, one an earlier run left there, one
//! whose app answered late), and the exit counted as clean, so the guardian did nothing
//! either. Now every unpark path ends with a look at the list ([`settle`]): each window still
//! parked is brought back ([`leftover_frame`]: its registered frame, else its `state.json`
//! frame, else centered); one that cannot be reached (another Desktop's, an app that does
//! not answer) leaves the exit unclean, so the guardian waits for it.
//!
//! A restore an app is too busy to answer (`kAXErrorCannotComplete`, the 0.25 s messaging
//! timeout) is tried again a few times, quickly ([`retry_quickly`]), and counts as done as
//! soon as the window server lists the window out of the park zone: a busy app often carries
//! out the request a moment after the timeout.

use std::time::Duration;

use super::geometry::Rect;
use super::persist::StateFile;
use super::recovery::{FrameSource, LiveWindow, ScanDisplays, plan_sourced, safe_restore};

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

/// True when a window still parked may be tried with `left` of the leftovers' budget: its
/// try ([`TRY_MAX`]) and, for one the registry does not know, reaching it through its app's
/// list and reading its title (an Accessibility call each) must fit. Never a try that would
/// end past the budget (the review of 6X, L1).
pub fn leftover_fits(left: Duration, registered: bool) -> bool {
    let reach = if registered { Duration::ZERO } else { AX_CALL_MAX * 2 };
    left >= reach + TRY_MAX
}

/// Where a window still parked after the unpark goes back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftoverSource {
    /// The safety registry's frame for it (a window we parked, whose restore failed).
    Registry,
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
            Self::Saved => "state.json",
            Self::Centered => "centered",
        }
    }
}

/// The frame a window still parked goes back to: its `registered` frame (made safe), else
/// its `state.json` frame (its window id first, then exact bundle id and title, then bundle
/// id when one candidate is left: [`super::window_match`]), else centered;
/// always on the visible frame and never in a park zone.
pub fn leftover_frame(registered: Option<Rect>, live: &LiveWindow, state: Option<&StateFile>, displays: &ScanDisplays) -> (Rect, LeftoverSource) {
    if let Some(r) = registered {
        return (safe_restore(r, displays), LeftoverSource::Registry);
    }
    match plan_sourced(std::slice::from_ref(live), state, displays).first() {
        Some((_, frame, FrameSource::Saved)) => (*frame, LeftoverSource::Saved),
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
    use crate::engine::park::{Corner, is_parked};
    use crate::engine::persist::{SavedRole, SavedSpace, SavedStage, SavedWindow};
    use crate::engine::recovery::{Known, is_stranded};

    const MAIN: Rect = Rect::new(0.0, 0.0, 2560.0, 1440.0);
    const VISIBLE: Rect = Rect::new(0.0, 30.0, 2560.0, 1410.0);

    fn displays() -> ScanDisplays {
        ScanDisplays::new(MAIN, VISIBLE, vec![], [Corner::BottomRight])
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
        assert_eq!(leftover_frame(Some(registered), &mail, None, &d), (registered, LeftoverSource::Registry));

        let st = state(vec![saved("com.apple.mail", "Inbox", Rect::new(299.0, 39.0, 1401.0, 1130.0))]);
        assert_eq!(leftover_frame(None, &mail, Some(&st), &d), (Rect::new(299.0, 39.0, 1401.0, 1130.0), LeftoverSource::Saved));

        let (f, source) = leftover_frame(None, &live("com.apple.MobileSMS", "Chat", Rect::new(2559.0, 1388.0, 1051.0, 906.0)), Some(&st), &d);
        assert_eq!(source, LeftoverSource::Centered);
        assert!(VISIBLE.contains_rect(&f) && !d.parks(f), "{f:?}");
        assert_eq!((f.w, f.h), (1051.0, 906.0), "its size kept");
    }

    #[test]
    fn a_registered_frame_in_the_park_zone_is_never_used_as_is() {
        let d = displays();
        let w = live("com.example.a", "Draft", Rect::new(2559.0, 1408.0, 800.0, 600.0));
        let (f, source) = leftover_frame(Some(Rect::new(2559.0, 1408.0, 800.0, 600.0)), &w, None, &d);
        assert_eq!(source, LeftoverSource::Registry);
        assert!(!is_parked(f, MAIN) && VISIBLE.contains_rect(&f), "{f:?}");
    }

    #[test]
    fn a_leftover_the_scan_does_not_count_parked_is_centered_all_the_same() {
        let d = displays();
        let w = live("com.example.a", "Draft", Rect::new(400.0, 300.0, 800.0, 600.0));
        let (f, source) = leftover_frame(None, &w, None, &d);
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
