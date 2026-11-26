//! A hidden window its app keeps showing (`engine/corner/park_reconcile.rs`; the audit of
//! 11-25, H4): Alpha's window waits in a hidden stage, and Alpha puts it back where it had it
//! each time something else moves it ([`fake::MovesBack`]: an app restoring its saved bounds).
//! At the end the model and the screen agree under both engines: no window is counted hidden
//! while a person sees it.

use super::scenarios::{A, Run, Scenario, expect, two_apps};
use super::*;
use crate::platform::fake::MovesBack;

/// How long the scenario watches after the launch.
const WATCH_MS: u64 = 15_000;

/// How long after the move Alpha puts its window back.
const BACK_MS: f64 = 150.0;

/// Two apps, Bravo in front, Alpha's window 11 hidden; Alpha moves it back `times` times
/// (`None`: every time). Watched for [`WATCH_MS`]: what the model counts hidden, what a person
/// sees, the stage in front, how often the app sent 11 to the park corner, when the model
/// stopped counting 11 hidden (how long after the launch, in whole seconds), and the windows
/// the reconcile still follows.
fn kept_out(engine: EngineKind, load: f64, times: Option<u32>) -> Run {
    let mut b = Bench::new(engine, DEFAULTS, load, 1, |d| {
        two_apps(d);
        d.apps.get_mut(&A).expect("Alpha").moves_back = Some(MovesBack { after_ms: BACK_MS, times });
    });
    let mut joined = None;
    let mut waited = 0;
    while waited < WATCH_MS {
        b.advance(50);
        waited += 50;
        if joined.is_none() && !b.hidden().contains(&11) {
            joined = Some(b.ms());
        }
    }
    let display = b.desk(|d| d.screens[0].display);
    let parks = b.calls().iter().filter(|c| matches!(c, fake::Call::Frame(11, f) if display.parks(*f))).count();
    let hidden: Vec<u32> = b.hidden().into_iter().collect();
    let shows: Vec<u32> = b.on_screen().into_iter().collect();
    let front = b.front().map_or("none".to_string(), |w| format!("{w:?}"));
    let joined = joined.map_or("never".to_string(), |ms| format!("after {} s", ms.div_ceil(1000)));
    let strays = b.c.corner.strays.len() + b.c.spaces.strays.len();
    let out = format!("hidden {hidden:?} shows {shows:?} front {front} parks of 11: {parks}; joined the front {joined}; strays {strays}");
    Run::of(&b, out)
}

fn kept_out_corner(load: f64) -> Run {
    kept_out(EngineKind::Corner, load, None)
}

fn kept_out_spaces(load: f64) -> Run {
    kept_out(EngineKind::Spaces, load, None)
}

fn moved_back_twice_corner(load: f64) -> Run {
    kept_out(EngineKind::Corner, load, Some(2))
}

#[test]
fn a_hidden_window_its_app_keeps_showing_is_not_left_counted_hidden() {
    // The launch's park and four tries, then it joins the stage in front where its app keeps
    // it, its card gone, and is not parked again. Before (audit 11-25 H4): an app as quick as
    // the tick was given up after the tries, the window counted hidden while it showed, with
    // no card; a slower one was a new stray at each move back (its park listed in the zone
    // first) and parked again for good, every 0.8 s at load 3 (21 parks in the 15 s).
    let corner = "hidden [] shows [11, 12] front [11, 12] parks of 11: 5; joined the front after 6 s; strays 0";
    // The app answers ten times slower: the same tries, each undone later.
    let slower = "hidden [] shows [11, 12] front [11, 12] parks of 11: 5; joined the front after 11 s; strays 0";
    expect(kept_out_corner, [corner, corner, slower]);
    // Under the Spaces engine a hide moves nothing: the app has nothing to put back.
    let spaces = "hidden [11] shows [12] front [12] parks of 11: 0; joined the front never; strays 0";
    expect(kept_out_spaces, [spaces, spaces, spaces]);
}

/// The third try holds: hidden, as before, and its stray forgotten once it stayed in the
/// park zone ([`crate::engine::corner::park_reconcile::SETTLED_AFTER`]).
#[test]
fn a_hidden_window_its_app_moves_back_twice_stays_hidden() {
    let hidden = "hidden [11] shows [12] front [12] parks of 11: 3; joined the front never; strays 0";
    expect(moved_back_twice_corner, [hidden, hidden, hidden]);
}

/// Every scenario, for the load table.
pub(super) const ALL: &[Scenario] = &[
    ("a hidden window its app keeps putting back (Corner)", "engine/corner/park_reconcile.rs, audit 11-25 H4", kept_out_corner),
    ("a hidden window its app keeps putting back (Spaces)", "controller/spaces.rs Stray, audit 11-25 H4", kept_out_spaces),
    ("a hidden window its app puts back twice (Corner)", "engine/corner/park_reconcile.rs MAX_TRIES", moved_back_twice_corner),
];
