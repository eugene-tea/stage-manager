//! The window server's notices, registered for the Spaces engine (round r5-notices; the pure
//! half is [`crate::engine::server_notice`], docs/ARCHITECTURE.md "The window server's
//! notices"). Each is a trigger for a read, never a fact: the WM thread looks, and the look
//! decides. If none comes on some Mac, every poll still runs as before.
//!
//! **Registered** when the launch is about to run its probe for the Spaces engine
//! ([`register`]), on the main thread: `SLSRegisterNotifyProc(proc, type, context)` with the
//! callback `(type, data, unsigned int length, context)` (CGSInternal's CGSEvent.h,
//! `CGSRegisterNotifyProc`, MIT), for the types of [`Kind::ALL`]. `probe-spaces cgs` (11-21)
//! found this registration and the connection's (`SLSRegisterConnectionNotifyProc`, yabai's
//! extern.h) to bring the same notices at the same moments; this one is kept because it has a
//! public way back out, `SLSRemoveNotifyProc(proc, type, context)` (CGSEvent.h,
//! `CGSRemoveNotifyProc`), taken when the launch chooses another engine ([`chosen`]). The
//! process hears them only while its main thread runs the AppKit event loop (the app does,
//! from `[NSApp run]` on).
//!
//! **The callback** stamps the notice at its entry ([`clock::now`]), copies the payload's
//! first words (valid only during the call) and queues it; the first of a turn schedules one
//! flush on the main run loop. Nothing else runs in it. **The flush** decodes the turn's
//! notices into one [`Burst`] per kind ([`server_notice::settle`]), drops our own adds and
//! take-outs ([`expect`], recorded at SkyLight's add calls), counts everything for `ctl
//! state` ([`status`]), nudges a running launch watch at a window's creation
//! ([`super::launch_watch::nudge`]) and sends the bursts the WM thread has a use for
//! (`WmCommand::ServerNotice`, [`set_sink`]).
//!
//! **Health** ([`probe_window`], [`health_tick`]): whether the launch probe's own panel
//! brought its creation, its take-out and its add, matched by content; `heard` or `silent`
//! in `ctl state` (silent is not a failure: the polls carry on).

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use block2::RcBlock;
use objc2_core_foundation::{CFRunLoop, kCFRunLoopCommonModes};
use serde_json::{Value, json};

use super::ffi::SKYLIGHT;
use crate::engine::server_notice::{self, Burst, Expected, Health, Kind, Layout, Raw, Verdict, WORDS};
use crate::platform::clock;

type PlainProc = extern "C" fn(u32, *mut c_void, u32, *mut c_void);
type FnRegister = unsafe extern "C" fn(PlainProc, u32, *mut c_void) -> i32;
type FnRemove = unsafe extern "C" fn(PlainProc, u32, *mut c_void) -> i32;

/// Notices kept for one turn at most (a flood past it is counted, not kept).
const TURN_MAX: usize = 4096;

/// Forwarding is on: the Spaces engine runs ([`chosen`], [`stop`]).
static ON: AtomicBool = AtomicBool::new(false);
/// The membership layout learned ([`Layout::pack`]; 0: none yet).
static LAYOUT: AtomicU8 = AtomicU8::new(0);
/// Notices past [`TURN_MAX`] in a turn.
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// What the callback queued in this turn, and whether its flush is scheduled.
struct Turn {
    raws: Vec<Raw>,
    scheduled: bool,
}

static TURN: Mutex<Turn> = Mutex::new(Turn { raws: Vec::new(), scheduled: false });

/// One kind's counts since the launch.
#[derive(Debug, Default, Clone, Copy)]
struct Stat {
    heard: u64,
    last: Option<Instant>,
    /// Bursts sent to the WM thread.
    sent: u64,
    /// Notices folded into a burst with others of their turn.
    coalesced: u64,
    /// Membership notices of our own asks.
    ours: u64,
}

/// Everything the flush and the status share.
#[derive(Default)]
struct Book {
    expected: Expected,
    health: Health,
    health_said: bool,
    stats: BTreeMap<Kind, Stat>,
    /// The registration: how many types took, and with which call.
    registered: Option<(usize, &'static str)>,
    remove: Option<FnRemove>,
}

static BOOK: Mutex<Option<Book>> = Mutex::new(None);

fn book() -> MutexGuard<'static, Option<Book>> {
    BOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn turn() -> MutexGuard<'static, Turn> {
    TURN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Where the bursts go (the WM thread).
type Sink = Box<dyn Fn(Burst) + Send + Sync>;
static SINK: OnceLock<Sink> = OnceLock::new();

/// The bursts go to `sink` from now on (`WmCommand::ServerNotice`): set once the WM thread runs.
pub fn set_sink(sink: impl Fn(Burst) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

extern "C" fn on_notice(code: u32, data: *mut c_void, len: u32, _ctx: *mut c_void) {
    let at = clock::now();
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let len = len as usize;
    let mut words = [0u32; WORDS];
    if !data.is_null() {
        for (i, w) in words.iter_mut().enumerate().take((len / 4).min(WORDS)) {
            // SAFETY: the window server hands `len` bytes at `data` for the call; `i * 4 + 4 <=
            // len`. Read unaligned: the payload's alignment is not documented.
            *w = unsafe { std::ptr::read_unaligned((data as *const u8).add(i * 4).cast::<u32>()) };
        }
    }
    let first = {
        let mut t = turn();
        if t.raws.len() < TURN_MAX {
            t.raws.push(Raw { code, at, words, len });
        } else {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
        !std::mem::replace(&mut t.scheduled, true)
    };
    if first {
        schedule_flush();
    }
}

/// One flush on the main run loop's next pass, in every common mode (a menu's tracking too).
fn schedule_flush() {
    let Some(main) = CFRunLoop::main() else {
        turn().scheduled = false;
        return;
    };
    let block = RcBlock::new(flush);
    // SAFETY: the common-modes constant is a valid static; the block is 'static and is copied
    // by CoreFoundation.
    unsafe { main.perform_block(kCFRunLoopCommonModes.map(|m| m.as_ref()), Some(&block)) };
    main.wake_up();
}

/// The turn's notices: decoded, ours dropped, counted, sent.
fn flush() {
    let raws = {
        let mut t = turn();
        t.scheduled = false;
        std::mem::take(&mut t.raws)
    };
    if raws.is_empty() || !ON.load(Ordering::Relaxed) {
        return;
    }
    let mut layout = Layout::unpack(LAYOUT.load(Ordering::Relaxed));
    let had = layout;
    let bursts = {
        let mut g = book();
        let Some(b) = g.as_mut() else { return };
        let bursts = server_notice::settle(&raws, &mut layout, &mut b.expected, &mut b.health);
        for burst in &bursts {
            let s = b.stats.entry(burst.kind).or_default();
            if s.heard == 0 {
                tracing::info!(kind = burst.kind.code(), "window-server notices: the first of type {} ({}) heard", burst.kind.code(), burst.kind.name());
            }
            s.heard += u64::from(burst.count);
            s.last = raws.iter().rev().find(|r| r.code == burst.kind.code()).map(|r| r.at);
            s.ours += u64::from(burst.ours);
            if burst.kind.forwarded() && burst.wants_look() {
                s.sent += 1;
                s.coalesced += u64::from(burst.count.saturating_sub(1));
            }
        }
        bursts
    };
    if layout != had {
        LAYOUT.store(Layout::pack(layout), Ordering::Relaxed);
        tracing::info!(layout = ?layout, "window-server notices: a membership notice's window and Space read by content");
    }
    for burst in bursts {
        if burst.kind == Kind::Created && super::launch_watch::watching() {
            super::launch_watch::nudge();
        }
        if burst.kind.forwarded()
            && burst.wants_look()
            && let Some(sink) = SINK.get()
        {
            sink(burst);
        }
    }
}

/// Registers the notices (main thread, at the launch, before the probe for the Spaces
/// engine). True when any type took; nothing is forwarded until [`chosen`] says the Spaces
/// engine runs.
pub fn register() -> bool {
    if cfg!(test) {
        return false;
    }
    // SAFETY: a system framework path; RTLD_NOW resolves everything at load.
    let h = unsafe { libc::dlopen(SKYLIGHT.as_ptr(), libc::RTLD_NOW) };
    if h.is_null() {
        tracing::info!("window-server notices: SkyLight did not load; the polls carry on alone");
        return false;
    }
    let mut names = Vec::new();
    // SAFETY (both): the signatures the module docs name (CGSInternal's CGSEvent.h).
    let reg: Option<FnRegister> = unsafe { super::ffi::load(h, c"SLSRegisterNotifyProc", &mut names) };
    let remove: Option<FnRemove> = unsafe { super::ffi::load(h, c"SLSRemoveNotifyProc", &mut names) };
    let Some(reg) = reg else {
        tracing::info!("window-server notices: SLSRegisterNotifyProc is not exported on this macOS; the polls carry on alone");
        return false;
    };
    // SAFETY: a callback of the stated type that lives as long as the process; no context.
    let ok = Kind::ALL.iter().filter(|k| unsafe { reg(on_notice, k.code(), std::ptr::null_mut()) } == 0).count();
    tracing::info!(types = ok, of = Kind::ALL.len(), remove = remove.is_some(), "window-server notices: registered (SLSRegisterNotifyProc)");
    *book() = Some(Book { registered: Some((ok, "SLSRegisterNotifyProc")), remove, ..Book::default() });
    ok > 0
}

/// The launch chose: with the Spaces engine the notices are forwarded from now on; else they
/// are removed again (or, where the removal is not exported, never forwarded).
pub fn chosen(spaces: bool) {
    if spaces {
        ON.store(book().is_some(), Ordering::Relaxed);
        return;
    }
    let mut g = book();
    let Some(b) = g.take() else { return };
    if let Some(remove) = b.remove {
        for k in Kind::ALL {
            // SAFETY: the same callback and type as registered, no context.
            let _ = unsafe { remove(on_notice, k.code(), std::ptr::null_mut()) };
        }
    }
    tracing::info!(removed = b.remove.is_some(), "window-server notices: another engine runs; unregistered");
}

/// The Spaces engine stopped running (a fallback): nothing is forwarded any more.
pub fn stop() {
    ON.store(false, Ordering::Relaxed);
}

/// True while notices are forwarded.
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// The launch probe made panel `panel` on Desktop `home`: its notices are looked for
/// ([`Health`]).
pub fn probe_window(panel: u32, home: u64) {
    if let Some(b) = book().as_mut() {
        b.health.probe(panel, home, clock::now());
    }
}

/// Windows `ids` asked into Space `sid` by this process (SkyLight's add calls): their
/// notices are ours ([`Expected`]). Nothing while no notice is forwarded.
pub fn expect(sid: u64, ids: &[u32]) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    if let Some(b) = book().as_mut() {
        b.expected.asked(sid, ids);
    }
}

/// The health's verdict once it is in, logged once (the WM tick).
pub fn health_tick(now: Instant) {
    let mut g = book();
    let Some(b) = g.as_mut() else { return };
    if b.health_said {
        return;
    }
    let v = b.health.verdict(now);
    match v {
        Verdict::Heard => tracing::info!("window-server notices: heard (the launch probe's panel brought its creation, take-out and add)"),
        Verdict::Silent => tracing::info!(
            created = b.health.created,
            taken_out = b.health.taken_out,
            added = b.health.added,
            "window-server notices: silent for the launch probe's panel; the polls carry on as before"
        ),
        Verdict::Off | Verdict::Waiting => return,
    }
    b.health_said = true;
}

/// `ctl state`'s `engine.notices`: the registration, the health, the layout, and per type
/// what was heard, when last (ms ago), how many bursts went to the WM thread, how many
/// notices were folded into them, how many were our own.
pub fn status() -> Value {
    let now = clock::now();
    let g = book();
    let Some(b) = g.as_ref() else {
        return json!({ "registered": false, "notices": "off" });
    };
    let types: serde_json::Map<String, Value> = Kind::ALL
        .iter()
        .map(|k| {
            let s = b.stats.get(k).copied().unwrap_or_default();
            (
                k.code().to_string(),
                json!({
                    "heard": s.heard,
                    "last_ms_ago": s.last.map(|t| now.saturating_duration_since(t).as_millis() as u64),
                    "sent": s.sent,
                    "coalesced": s.coalesced,
                    "ours": s.ours,
                }),
            )
        })
        .collect();
    json!({
        "registered": b.registered.map(|(n, _)| n),
        "call": b.registered.map(|(_, c)| c),
        "forwarded": ON.load(Ordering::Relaxed),
        "notices": b.health.verdict(now).name(),
        "probe": { "created": b.health.created, "taken_out": b.health.taken_out, "added": b.health.added },
        "layout": Layout::unpack(LAYOUT.load(Ordering::Relaxed)).map(|l| json!({ "window": l.window, "space": l.space })),
        "expected": b.expected.len(),
        "dropped": DROPPED.load(Ordering::Relaxed),
        "types": types,
    })
}
