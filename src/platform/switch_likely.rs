//! Signs that a switch is likely (round picture-wait): the pointer in the Dock's bar, a stage
//! hotkey's modifiers going down, a press on a Dock item.
//!
//! A switch plays pictures of the windows leaving, taken at the switch where none is fresh
//! (`controller/switch_pictures.rs`), and waits for them. A card click and ⌘Tab announce
//! themselves (the pointer on the card and its press, the switcher's key) and the strip takes
//! the pictures then. A Dock click and a hotkey announce themselves too, a little earlier
//! than they act: the pointer comes into the Dock's bar before it clicks there, and a
//! hotkey's modifiers go down before its key. Each such sign is told to the controller
//! ([`Sign`], `WmCommand::SwitchLikely`), which takes the pictures a switch would wait for
//! (`Controller::switch_likely`), so the switch that follows finds them fresh and waits for
//! none.
//!
//! **Events only, never a timer.** The pointer's sign comes from the mouse moves the strip
//! already hears (`ui::strip::pointer_moved`, heard only while a strip may show): once as
//! the pointer comes into the bar, then again no more often than [`POINTER_EVERY`] while it
//! moves there. A pointer resting in the Dock moves nothing and tells nothing. The modifiers'
//! sign comes from the input monitor's modifier changes, when the keys held become exactly
//! those of a binding that moves windows ([`set_hotkey_mods`]), no more often than
//! [`AGAIN_AFTER`]. The press on a Dock item is the controller's own (`dock_pressed`: it reads
//! the item there already).
//!
//! The Dock's bar is the one the controller reads for its hit tests
//! (`Controller::refresh_dock_bar`, [`set_dock_bars`]). An auto-hidden Dock's bar is read
//! where it rests off the screen until the next read finds it shown, so the pointer's sign
//! may miss it; the press's sign does not depend on the bar's frame.

use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use super::clock;
use super::switch_shots::FRESH;
use crate::config::Mods;
use crate::engine::geometry::{Point, Rect};

/// What made a switch likely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    /// The pointer came into the Dock's bar, or moves there.
    DockPointer,
    /// The left button went down on a Dock item.
    DockPress,
    /// The modifiers of a hotkey that moves windows went down.
    HotkeyModifiers,
}

impl Sign {
    /// For the log.
    pub fn as_str(self) -> &'static str {
        match self {
            Sign::DockPointer => "the pointer in the Dock",
            Sign::DockPress => "a press on a Dock item",
            Sign::HotkeyModifiers => "a hotkey's modifiers down",
        }
    }
}

/// While the pointer moves in the Dock's bar its sign is told again after this: a picture
/// taken at one sign is fresh for a switch ([`FRESH`]) until about the next.
pub const POINTER_EVERY: Duration = FRESH;

/// A sign that begins anew (the pointer coming into the bar again, a hotkey's modifiers
/// going down again) is told after this at the earliest: a pointer crossing the bar's edge
/// back and forth, or a modifier used while typing (a binding on ⌥ alone, say), never has
/// the shown windows pictured at each one. Half the freshness, as the controller takes
/// again only a picture older than that (`switch_pictures::AHEAD_OLDER`).
pub const AGAIN_AFTER: Duration = Duration::from_millis(FRESH.as_millis() as u64 / 2);

/// What the signs are told from (main thread, but for the bars, set on the WM thread).
#[derive(Default)]
struct State {
    /// The Dock's bar as the controller last read it (global, top-left).
    bars: Vec<Rect>,
    /// The pointer was in the bar at its last move.
    inside: bool,
    /// When the pointer's sign was last told.
    pointer_told: Option<Instant>,
    /// The modifiers of the bindings that move windows.
    mods: Vec<Mods>,
    /// The modifiers held at the last change.
    held: Mods,
    /// When the modifiers' sign was last told.
    keys_told: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State { bars: Vec::new(), inside: false, pointer_told: None, mods: Vec::new(), held: NO_MODS, keys_told: None });

const NO_MODS: Mods = Mods { ctrl: false, alt: false, shift: false, cmd: false };

type Sink = Box<dyn Fn(Sign) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where the signs go (once, at startup: a WM command). It must not block.
pub fn install(sink: impl Fn(Sign) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

/// The Dock's bar as just read (the WM thread, when it changed); empty when unknown.
pub fn set_dock_bars(bars: &[Rect]) {
    state().bars = bars.to_vec();
}

/// The modifiers of the hotkeys that move windows (the config's bindings as registered;
/// none while hotkeys are off).
pub fn set_hotkey_mods(mods: Vec<Mods>) {
    state().mods = mods;
}

/// True when `p` lies in the Dock's bar.
pub fn in_dock(bars: &[Rect], p: Point) -> bool {
    bars.iter().any(|b| b.contains(p))
}

/// Whether the pointer's sign is told at a move: in the bar now (`inside`), and not told
/// (`told_ago`: since the last one) for [`POINTER_EVERY`] while it stays there (`was_inside`)
/// or for [`AGAIN_AFTER`] when it just came in.
pub fn pointer_tells(was_inside: bool, inside: bool, told_ago: Option<Duration>) -> bool {
    inside && told_ago.is_none_or(|ago| ago >= if was_inside { POINTER_EVERY } else { AGAIN_AFTER })
}

/// Whether a modifier change tells the modifiers' sign: the keys held became exactly those
/// of a watched binding (`now`, from `before`), and none was told within [`AGAIN_AFTER`].
pub fn modifiers_tell(before: Mods, now: Mods, watched: &[Mods], told_ago: Option<Duration>) -> bool {
    now != before && !now.is_empty() && watched.contains(&now) && told_ago.is_none_or(|ago| ago >= AGAIN_AFTER)
}

/// The pointer moved to `p` (global, top-left; main thread, the strip's mouse-move monitor).
pub fn pointer_at(p: Point) {
    let now = clock::now();
    let tells = {
        let mut s = state();
        let inside = in_dock(&s.bars, p);
        let was = std::mem::replace(&mut s.inside, inside);
        let tells = pointer_tells(was, inside, s.pointer_told.map(|t| now.saturating_duration_since(t)));
        if tells {
            s.pointer_told = Some(now);
        }
        tells
    };
    if tells {
        tell(Sign::DockPointer);
    }
}

/// The modifiers held changed to `held` (main thread, the input monitor).
pub fn modifiers(held: Mods) {
    let now = clock::now();
    let tells = {
        let mut s = state();
        let before = std::mem::replace(&mut s.held, held);
        let tells = modifiers_tell(before, held, &s.mods, s.keys_told.map(|t| now.saturating_duration_since(t)));
        if tells {
            s.keys_told = Some(now);
        }
        tells
    };
    if tells {
        tell(Sign::HotkeyModifiers);
    }
}

fn tell(sign: Sign) {
    if let Some(sink) = SINK.get() {
        sink(sign);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAR: Rect = Rect::new(700.0, 1370.0, 1100.0, 66.0);
    const CTRL_ALT: Mods = Mods { ctrl: true, alt: true, shift: false, cmd: false };

    /// The pointer's sign: once as it comes into the bar, then no more often than the
    /// freshness while it moves there; nothing outside, and nothing new for a pointer that
    /// crosses the bar's edge back and forth.
    #[test]
    fn the_pointer_tells_as_it_comes_into_the_dock_then_rarely_while_it_moves_there() {
        assert!(in_dock(&[BAR], Point::new(1200.0, 1400.0)));
        assert!(!in_dock(&[BAR], Point::new(1200.0, 900.0)), "above the bar");
        assert!(!in_dock(&[], Point::new(1200.0, 1400.0)), "the bar unknown");
        assert!(pointer_tells(false, true, None), "came in, never told");
        assert!(!pointer_tells(true, false, None), "left");
        assert!(!pointer_tells(false, false, None), "elsewhere");
        assert!(!pointer_tells(true, true, Some(Duration::from_millis(300))), "moving there, told just now");
        assert!(!pointer_tells(true, true, Some(POINTER_EVERY - Duration::from_millis(1))));
        assert!(pointer_tells(true, true, Some(POINTER_EVERY)), "still moving there: again");
        assert!(!pointer_tells(false, true, Some(Duration::from_millis(200))), "out and back in at once");
        assert!(pointer_tells(false, true, Some(AGAIN_AFTER)), "back in after a while");
    }

    /// The modifiers' sign: when the keys held become exactly a binding's, not on the way
    /// there, not when one is let go, not for a combination no binding has.
    #[test]
    fn a_hotkeys_modifiers_tell_when_they_are_all_down() {
        let ctrl = Mods { ctrl: true, ..NO_MODS };
        let watched = [CTRL_ALT];
        assert!(!modifiers_tell(NO_MODS, ctrl, &watched, None), "⌃ alone, on the way");
        assert!(modifiers_tell(ctrl, CTRL_ALT, &watched, None), "⌃⌥ down");
        assert!(!modifiers_tell(CTRL_ALT, CTRL_ALT, &watched, None), "no change");
        assert!(!modifiers_tell(CTRL_ALT, ctrl, &watched, None), "⌥ let go");
        assert!(!modifiers_tell(CTRL_ALT, NO_MODS, &watched, None), "all let go");
        assert!(!modifiers_tell(ctrl, CTRL_ALT, &[], None), "hotkeys off");
        assert!(!modifiers_tell(ctrl, CTRL_ALT, &watched, Some(Duration::from_millis(100))), "told just now");
        assert!(modifiers_tell(ctrl, CTRL_ALT, &watched, Some(AGAIN_AFTER)));
        let with_shift = Mods { shift: true, ..CTRL_ALT };
        assert!(!modifiers_tell(CTRL_ALT, with_shift, &watched, None), "more keys than the binding's");
    }

    #[test]
    fn every_sign_has_its_word() {
        for s in [Sign::DockPointer, Sign::DockPress, Sign::HotkeyModifiers] {
            assert!(!s.as_str().is_empty());
        }
    }
}
