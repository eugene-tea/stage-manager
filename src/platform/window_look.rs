//! Each window's look as macOS draws its shadow, read from a small capture of the window with
//! its shadow (round window-appearance; the classification and the cache are
//! [`crate::ui::shadow_look`]'s).
//!
//! A window's capture ([`super::capture`]) also asks for this reading when it is due
//! ([`due`]): never read, read before the system's appearance last changed
//! ([`appearance_changed`]), or [`REFRESH`] seconds ago. The reading is a second still of the
//! same window with the same filter, its shadow not ignored and its child windows left out, at
//! half a pixel a point ([`probe_size`]: at most 512 px on the longer side, a twentieth of the
//! pixels of a full-size snapshot at 2x); it is read on ScreenCaptureKit's queue and only its
//! alpha along three lines is looked at. Nothing reads it before a switch needs it: a
//! snapshot's shadow asks [`look`], falling back to the system's appearance when there is no
//! reading.
//!
//! [`REFRESH`]: crate::ui::shadow_look::REFRESH
//! [`probe_size`]: crate::ui::shadow_look::probe_size

use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Instant;

use objc2_core_graphics::CGImage;

use crate::ui::shadow_look::{self, Look, LookCache};

static CACHE: LazyLock<Mutex<LookCache>> = LazyLock::new(|| Mutex::new(LookCache::default()));
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

fn cache() -> MutexGuard<'static, LookCache> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

fn now() -> f64 {
    EPOCH.elapsed().as_secs_f64()
}

/// When window `id`'s look is to be read now: the generation to record it under.
pub fn due(id: u32) -> Option<u64> {
    let c = cache();
    c.due(id, now()).then(|| c.generation())
}

/// The system's appearance changed (Light, Dark or Auto): every window is read again at its
/// next capture, and none of the readings before is used meanwhile.
pub fn appearance_changed() {
    cache().appearance_changed();
    tracing::debug!("window look: the system's appearance changed; every window is read again");
}

/// Window `id`'s look and how long ago it was read (seconds), if read since the system's
/// appearance last changed.
pub fn look(id: u32) -> Option<(Look, f64)> {
    cache().look(id, now())
}

/// Reads window `id`'s look from `image` (the window with its shadow, its frame `frame`
/// points) and records it under `generation`; logged at debug when it changes, and when
/// there is no clear evidence.
pub fn read(id: u32, generation: u64, image: &CGImage, frame: (f64, f64)) {
    let Some((bytes, w, h)) = super::pixels::bgra(image, None) else { return };
    let alpha: Vec<u8> = bytes.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
    let fit = shadow_look::profiles(&alpha, w, h, frame).and_then(|p| shadow_look::classify(&p));
    let changed = cache().record(id, generation, fit.map(|f| f.look), now());
    match fit {
        Some(f) if changed => {
            let (look, state) = f.look.words();
            tracing::debug!(window = id, look, state, rms = format!("{:.1}", f.rms), other = format!("{:.1}", f.other), w, h, "window look: its shadow is drawn in this appearance");
        }
        Some(_) => {}
        None => tracing::trace!(window = id, w, h, "window look: no clear reading of its shadow"),
    }
}
