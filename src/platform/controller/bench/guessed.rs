//! The fallback, pinned: with the window server's Desktops not read
//! ([`crate::platform::fake::Desk::desk_reads`] false), every scenario that runs the Corner
//! engine reads as it did before the Corner engine read them (1.9.2; `guessed.txt`: its
//! first 50 lines printed from the code as it stood then, a scenario added since printed
//! with the reads off; "another screen's Desktop changes (Corner)" printed again when the
//! desk learned to keep a second screen's parked windows on that screen, `fake::Desk::place`;
//! and the whole file at the merge with round switch-pictures, whose pictures taken at the
//! switch show in the calls of 11 scenarios as `shots [ids]` and nowhere else, move one cause
//! chain at load 3 to its reading at load 10 with the same outcome ("an app focuses its other
//! window after our focus"), and add five scenarios of their own).
//! A Mac where the reads do not work behaves as before. Beside each
//! Desktop scenario's own test (`desktops.rs`, the reads on) the file is also the "before".
//!
//! Print the file again only when a scenario is added or its script changes:
//! `BENCH_PRINT=1 cargo test --lib guessed::print_pins -- --nocapture`.

use std::cell::Cell;
use std::collections::BTreeMap;

use super::scenarios::{LOADS, Scenario};
use super::*;

thread_local! {
    /// While set, a bench under the Corner engine runs on a desk whose reads do not answer.
    static FORCED: Cell<bool> = const { Cell::new(false) };
}

/// True while this thread's Corner benches run without the reads ([`Forced`]).
pub(super) fn forced() -> bool {
    FORCED.with(Cell::get)
}

/// The Corner benches of this thread run without the reads until dropped.
pub(super) struct Forced(());

impl Forced {
    pub(super) fn on() -> Self {
        FORCED.with(|f| f.set(true));
        Self(())
    }
}

impl Drop for Forced {
    fn drop(&mut self) {
        FORCED.with(|f| f.set(false));
    }
}

/// Every scenario of the bench.
fn all() -> impl Iterator<Item = &'static Scenario> {
    super::scenarios::ALL.iter().chain(super::causes::ALL).chain(super::switch::ALL).chain(super::desktops::ALL).chain(super::screens::ALL).chain(super::corner_screens::ALL)
}

/// The pinned outcomes: a scenario's name, then its outcome at each load, tab-separated.
const PINS: &str = include_str!("guessed.txt");

pub(super) fn pins() -> BTreeMap<&'static str, [&'static str; 3]> {
    PINS.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 4, "a pin is a name and three outcomes: {l}");
            (f[0], [f[1], f[2], f[3]])
        })
        .collect()
}

#[test]
fn with_the_reads_off_every_corner_scenario_reads_as_before() {
    let pins = pins();
    let mut seen = 0;
    for (name, _, script) in all() {
        for (i, load) in LOADS.iter().enumerate() {
            let run = {
                let _guess = Forced::on();
                script(*load)
            };
            if run.engine != EngineKind::Corner {
                break;
            }
            let want = pins.get(name).unwrap_or_else(|| panic!("{name}: a Corner scenario with no pin (module docs)"));
            assert_eq!(run.outcome, want[i], "{name} at load {load}, the reads off:\n{}", run.story);
            assert_eq!(run.disagreed, 0, "{name} at load {load}: nothing is read, nothing disagrees");
            seen += usize::from(i == 0);
        }
    }
    assert_eq!(seen, pins.len(), "every pin is a Corner scenario of the bench");
}

/// Prints the pins (module docs).
#[test]
fn print_pins() {
    if std::env::var("BENCH_PRINT").is_err() {
        return;
    }
    let _guess = Forced::on();
    for (name, _, script) in all() {
        let runs = LOADS.map(script);
        if runs[0].engine == EngineKind::Corner {
            println!("PIN\t{name}\t{}\t{}\t{}", runs[0].outcome, runs[1].outcome, runs[2].outcome);
        }
    }
}
