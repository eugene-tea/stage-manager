//! macOS's accessibility display options the strip follows (System Settings, Accessibility,
//! Display), read at launch and again whenever macOS says they changed
//! (`NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification`):
//!
//! - **Reduce Motion** (`[animation] reduce_motion`, "system" by default): nothing flies,
//!   slides, grows or turns. What animated becomes a fade of [`FADE_MS`]: a switch is a
//!   cross-fade where the windows are, the strip fades in and out, a card changes place with a
//!   fade and does not grow under the pointer. What follows the pointer while it is dragged
//!   still follows it (the user's own movement). Why each case is decided so: UX-SPEC,
//!   "Reduce Motion".
//! - **Reduce Transparency** (also on with Increase Contrast, which macOS ties to it): the
//!   cards' glass goes solid, as macOS's own materials turn opaque; the edge tab too.
//!
//! The values are kept in atomics, read on the main thread by the strip at each decision, so a
//! change applies from the next motion (and the glass is repainted at once).

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification};
use objc2_foundation::{NSNotification, NSObjectProtocol, NSOperationQueue};

use crate::config::AnimationStyle;

/// The fade that stands in for motion under Reduce Motion (milliseconds at `[animation]
/// speed` 1): a switch's cross-fade, the strip's fade in and out, a card fading in at a new
/// slot. Short and steady, inside the 150–250 ms the guidance asks for (no flashing: one
/// fade each way, never repeated). Tune here if it reads too quick or too slow.
pub const FADE_MS: u64 = 200;

/// macOS's Reduce Motion, as last read.
static REDUCE_MOTION: AtomicBool = AtomicBool::new(false);
/// macOS's Reduce Transparency, as last read.
static REDUCE_TRANSPARENCY: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The change observer's token, kept for the life of the app (main thread).
    static OBSERVER: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> = const { RefCell::new(None) };
}

/// macOS's Reduce Motion setting, as last read.
pub fn system_reduce_motion() -> bool {
    REDUCE_MOTION.load(Ordering::Relaxed)
}

/// macOS's Reduce Transparency setting, as last read: the cards are drawn solid.
pub fn reduce_transparency() -> bool {
    REDUCE_TRANSPARENCY.load(Ordering::Relaxed)
}

/// True when `a`'s motion is reduced now (`[animation] reduce_motion` with macOS's setting).
pub fn reduced(a: &AnimationStyle) -> bool {
    a.reduces_motion(system_reduce_motion())
}

/// The fade that stands in for motion, in seconds at `a`'s speed (0 when nothing animates).
pub fn fade_seconds(a: &AnimationStyle) -> f64 {
    a.seconds(FADE_MS)
}

/// Reads the options now and follows their changes; `changed` runs on the main thread after
/// each change. Call once, on the main thread.
pub fn install(changed: impl Fn() + 'static) {
    read();
    let workspace = NSWorkspace::sharedWorkspace();
    let center = workspace.notificationCenter();
    let main_queue = NSOperationQueue::mainQueue();
    let block = RcBlock::new(move |_note: NonNull<NSNotification>| {
        read();
        changed();
    });
    // SAFETY: the notification name is a valid framework static; the block runs on the main
    // queue only, where `changed` lives.
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
            None,
            Some(&main_queue),
            &block,
        )
    };
    OBSERVER.with(|o| *o.borrow_mut() = Some(token));
}

/// Reads macOS's settings into the atomics and logs a change.
fn read() {
    let workspace = NSWorkspace::sharedWorkspace();
    let motion = workspace.accessibilityDisplayShouldReduceMotion();
    let transparency = workspace.accessibilityDisplayShouldReduceTransparency();
    let was = (REDUCE_MOTION.swap(motion, Ordering::Relaxed), REDUCE_TRANSPARENCY.swap(transparency, Ordering::Relaxed));
    if was != (motion, transparency) {
        tracing::info!(reduce_motion = motion, reduce_transparency = transparency, "macOS display options");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ReduceMotion};

    fn style(speed: f64, reduce_motion: ReduceMotion) -> AnimationStyle {
        AnimationStyle { speed, reduce_motion, ..Config::default().style.animation }
    }

    #[test]
    fn the_fade_is_short_and_steady_at_apples_speed() {
        let s = fade_seconds(&style(1.0, ReduceMotion::On));
        assert!((0.15..=0.25).contains(&s), "{s}");
    }

    #[test]
    fn the_fade_follows_the_speed_and_goes_with_no_animation() {
        let one = fade_seconds(&style(1.0, ReduceMotion::On));
        assert!((fade_seconds(&style(2.0, ReduceMotion::On)) - one / 2.0).abs() < 1e-9);
        assert_eq!(fade_seconds(&style(0.0, ReduceMotion::On)), 0.0);
        let off = AnimationStyle { enabled: false, ..style(1.0, ReduceMotion::On) };
        assert_eq!(fade_seconds(&off), 0.0);
    }

    #[test]
    fn on_and_off_ignore_macos_and_system_follows_it() {
        // The atomic starts false (no test sets it): "system" reads macOS's off.
        assert!(!reduced(&style(1.0, ReduceMotion::System)) || system_reduce_motion());
        assert!(reduced(&style(1.0, ReduceMotion::On)));
        assert!(!reduced(&style(1.0, ReduceMotion::Off)));
    }
}
