//! The window server's notices, heard under every window engine of ours (round r5-notices
//! for the Spaces engine, round looks-forward for the Corner engine; the pure half is
//! [`crate::engine::server_notice`], docs/ARCHITECTURE.md "The window server's notices").
//! Each is a trigger for a read, never a fact: the WM thread looks, and the look decides. If
//! none comes on some Mac, every poll still runs as before.
//!
//! **Which** ([`chosen`]): the Spaces engine hears every type of [`Kind::ALL`]; the Corner
//! engine only those that bring a look every engine makes ([`Kind::LOOKS`]: Mission
//! Control's or App Exposé's open and close, a Desktop change's end), so nothing wakes this
//! process for a window made, shown or moved between Spaces there. The launch chooses one of
//! the two; while Apple's Stage Manager arranges the windows (the Apple mode) what comes is
//! dropped on the WM thread, no controller of ours looking.
//!
//! **Registered** on the main thread ([`register`]: before the launch's probe when it may
//! run the Spaces engine, so that the probe's own panel is heard; else once the engine is
//! chosen): `SLSRegisterNotifyProc(proc, type, context)` with the callback `(type, data,
//! unsigned int length, context)` (CGSInternal's CGSEvent.h, `CGSRegisterNotifyProc`, MIT).
//! `probe-spaces cgs` (11-21) found this registration and the connection's
//! (`SLSRegisterConnectionNotifyProc`, yabai's extern.h) to bring the same notices at the
//! same moments; this one is kept because it has a public way back out,
//! `SLSRemoveNotifyProc(proc, type, context)` (CGSEvent.h, `CGSRemoveNotifyProc`), taken for
//! the types the engine chosen does not hear. The process hears them only while its main
//! thread runs the AppKit event loop (the app does, from `[NSApp run]` on).
//!
//! **Checked** ([`crate::engine::notice_trust`], as the Desktops' reads are checked,
//! `platform/desk_reads.rs`): the calls there and the registration taken, else off for the
//! run; and while it runs, the notices that bring a look are told against what the looks
//! find ([`told`], [`fact`], from the WM thread). None for the last opens and closes of
//! Mission Control: silent, and the public triggers take over ([`works`]). Many in a row for
//! nothing: they rest ten minutes, none forwarded ([`brings_looks`]). `ctl state`'s
//! `engine.notices.trust` says which.
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
//! **Health** ([`probe_window`], [`health_tick`]): `heard` in `ctl state` as soon as any
//! registered type was heard, `silent` when none came within its wait (not a failure: the
//! polls carry on), with the types never heard (`never_heard`: 811 never heard means the
//! launch watch's nudge never fires). The launch probe's panel only teaches the membership
//! layout (its creation's notice is lost before the event loop turns, live 11-21).

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use block2::RcBlock;
use objc2_core_foundation::{CFRunLoop, kCFRunLoopCommonModes};
use serde_json::{Value, json};

use super::spaces::ffi::{self, SKYLIGHT};
use super::spaces::launch_watch;
use crate::engine::notice_trust::{Change, OFF_FOR, Trust};
use crate::engine::recovery::EngineKind;
use crate::engine::server_notice::{self, Burst, Expected, Health, Kind, Layout, Raw, Verdict, WORDS};
use crate::platform::clock;

type PlainProc = extern "C" fn(u32, *mut c_void, u32, *mut c_void);
type FnRegister = unsafe extern "C" fn(PlainProc, u32, *mut c_void) -> i32;
type FnRemove = unsafe extern "C" fn(PlainProc, u32, *mut c_void) -> i32;

/// Notices kept for one turn at most (a flood past it is counted, not kept).
const TURN_MAX: usize = 4096;

/// The types heard now, a bit each in the order of [`Kind::ALL`] ([`chosen`], [`fell_back`]):
/// none while nothing is forwarded.
static HEARD: AtomicU32 = AtomicU32::new(0);
/// The notices that bring a look rest ([`Trust`]): the flush forwards none of them.
static RESTING: AtomicBool = AtomicBool::new(false);
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
    /// The types whose registration took and was not removed since.
    kinds: Vec<Kind>,
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

/// The bit of `kind` in [`HEARD`].
fn bit(kind: Kind) -> u32 {
    Kind::ALL.iter().position(|k| *k == kind).map_or(0, |i| 1 << i)
}

/// The bits of `kinds`.
fn bits(kinds: &[Kind]) -> u32 {
    kinds.iter().fold(0, |m, k| m | bit(*k))
}

/// True while notices of `kind` are heard.
fn hears(kind: Kind) -> bool {
    HEARD.load(Ordering::Relaxed) & bit(kind) != 0
}

extern "C" fn on_notice(code: u32, data: *mut c_void, len: u32, _ctx: *mut c_void) {
    let at = clock::now();
    // A type the engine that runs does not hear (registered for the probe, or one whose
    // removal is not exported): dropped here, nothing queued, no flush.
    if !Kind::of(code).is_some_and(hears) {
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
    if raws.is_empty() || HEARD.load(Ordering::Relaxed) == 0 {
        return;
    }
    let resting = RESTING.load(Ordering::Relaxed);
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
            if burst.kind.forwarded() && burst.wants_look() && !(resting && burst.kind.brings_look()) {
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
        if burst.kind == Kind::Created && launch_watch::watching() {
            launch_watch::nudge();
        }
        // Resting: none of those that bring a look goes on ([`Trust`]).
        if burst.kind.forwarded()
            && burst.wants_look()
            && !(resting && burst.kind.brings_look())
            && let Some(sink) = SINK.get()
        {
            super::costs::bump(super::costs::Cost::ServerNotice);
            sink(burst);
        }
    }
}

/// Registers the notices of `kinds` not registered yet (main thread, at the launch). True
/// when any type is registered after it; nothing is heard until [`chosen`] says which engine
/// runs.
pub fn register(kinds: &[Kind]) -> bool {
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
    let reg: Option<FnRegister> = unsafe { ffi::load(h, c"SLSRegisterNotifyProc", &mut names) };
    let remove: Option<FnRemove> = unsafe { ffi::load(h, c"SLSRemoveNotifyProc", &mut names) };
    let Some(reg) = reg else {
        tracing::info!("window-server notices: SLSRegisterNotifyProc is not exported on this macOS; the polls carry on alone");
        return false;
    };
    let mut g = book();
    let b = g.get_or_insert_with(Book::default);
    let asked: Vec<Kind> = kinds.iter().copied().filter(|k| !b.kinds.contains(k)).collect();
    if asked.is_empty() {
        return !b.kinds.is_empty();
    }
    // SAFETY: a callback of the stated type that lives as long as the process; no context.
    let took: Vec<Kind> = asked.iter().copied().filter(|k| unsafe { reg(on_notice, k.code(), std::ptr::null_mut()) } == 0).collect();
    tracing::info!(
        types = ?took.iter().map(|k| k.code()).collect::<Vec<_>>(),
        of = asked.len(),
        remove = remove.is_some(),
        "window-server notices: registered (SLSRegisterNotifyProc)"
    );
    b.kinds.extend(took);
    b.registered = Some((b.kinds.len(), "SLSRegisterNotifyProc"));
    b.remove = remove;
    !b.kinds.is_empty()
}

/// The types engine `kind` hears: all of them under the Spaces engine, those that bring a
/// look every engine makes under the Corner engine ([`Kind::LOOKS`]), none for an engine of
/// Apple's (the launch never chooses one).
fn heard_by(kind: EngineKind) -> &'static [Kind] {
    match kind {
        EngineKind::Spaces => &Kind::ALL,
        EngineKind::Corner => &Kind::LOOKS,
        EngineKind::Apple => &[],
    }
}

/// The launch chose engine `kind` (main thread): its types are registered when they are not
/// yet, the others removed again (or, where the removal is not exported, dropped in the
/// callback), and its types are heard from now on.
pub fn chosen(kind: EngineKind) {
    let wanted = heard_by(kind);
    if !wanted.is_empty() {
        register(wanted);
    }
    let mut g = book();
    let Some(b) = g.as_mut() else { return };
    let extra: Vec<Kind> = b.kinds.iter().copied().filter(|k| !wanted.contains(k)).collect();
    if let Some(remove) = b.remove {
        for k in &extra {
            // SAFETY: the same callback and type as registered, no context.
            let _ = unsafe { remove(on_notice, k.code(), std::ptr::null_mut()) };
        }
        b.kinds.retain(|k| wanted.contains(k));
        b.registered = Some((b.kinds.len(), "SLSRegisterNotifyProc"));
    }
    let heard: Vec<Kind> = b.kinds.iter().copied().filter(|k| wanted.contains(k)).collect();
    HEARD.store(bits(&heard), Ordering::Relaxed);
    tracing::info!(
        engine = kind.name(),
        heard = ?heard.iter().map(|k| k.code()).collect::<Vec<_>>(),
        removed = ?extra.iter().map(|k| k.code()).collect::<Vec<_>>(),
        removal = b.remove.is_some(),
        "window-server notices: the types the engine that runs hears"
    );
    if b.kinds.is_empty() {
        *g = None;
    }
}

/// The Spaces engine stopped running (a fallback to the Corner engine, any thread): only
/// the types the Corner engine hears go on; the others stay registered and are dropped in
/// the callback.
pub fn fell_back() {
    let heard = book().as_ref().map_or(0, |b| bits(&b.kinds) & bits(&Kind::LOOKS));
    HEARD.store(heard, Ordering::Relaxed);
}

/// True while any notice is heard.
pub fn on() -> bool {
    HEARD.load(Ordering::Relaxed) != 0
}

static TRUST: Mutex<Trust> = Mutex::new(Trust::new());

/// Runs `f` on the notices' standing: this process's, or a fake desk's own.
fn with_trust<T>(f: impl FnOnce(&mut Trust) -> T) -> T {
    #[cfg(test)]
    let f = {
        let mut f = Some(f);
        if let Some(out) = super::fake::with(|d| f.take().map(|f| f(&mut d.notice_trust))).flatten() {
            return out;
        }
        let Some(f) = f else { unreachable!("a desk that answered took the call") };
        f
    };
    f(&mut TRUST.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// True when the notices that bring a look are heard in this run: the calls there and the
/// registration taken for each of them ([`Kind::LOOKS`]).
pub fn listening() -> bool {
    #[cfg(test)]
    if let Some(there) = super::fake::with(|d| d.server_calls) {
        return there;
    }
    Kind::LOOKS.iter().all(|k| hears(*k))
}

/// True while the notices are trusted to bring every look forward ([`Trust::works`]): the
/// public triggers rest. False when none is heard in this run, when the last opens and
/// closes of Mission Control came with none, and during a rest and the trial after it.
pub fn works() -> bool {
    listening() && with_trust(|t| t.works(clock::now()))
}

/// True while a notice that brings a look brings it: heard, and not resting.
pub fn brings_looks() -> bool {
    listening() && with_trust(|t| t.brings_looks(clock::now()))
}

/// What `change` says, in the log; the flush follows a rest.
fn said(change: Option<Change>) {
    match change {
        Some(Change::Silent) => tracing::info!(
            misses = crate::engine::notice_trust::MISSES,
            "window-server notices: Mission Control opened or closed with none, several times in a row: its key and the Dock's notice bring the looks forward from now (the public way), until one comes again"
        ),
        Some(Change::Rested { first: true }) => tracing::warn!(
            rest_s = OFF_FOR.as_secs(),
            "window-server notices: many in a row with nothing to find: they rest, the polls and the public triggers carry on"
        ),
        Some(Change::Rested { first: false }) => tracing::debug!("window-server notices: still for nothing after their rest: they rest again"),
        Some(Change::Back) => tracing::info!("window-server notices: one told of Mission Control's open or close: they bring the looks forward again"),
        None => {}
    }
    RESTING.store(!with_trust(|t| t.brings_looks(clock::now())), Ordering::Relaxed);
}

/// A notice that brings a look reached the WM thread at `now` ([`Trust::notice`]).
pub fn told(now: Instant) {
    let change = with_trust(|t| t.notice(now));
    said(change);
}

/// A fact a notice may tell of was found at `now` ([`Trust::fact`]): a Space change, or
/// (`expected`) Mission Control's or App Exposé's open or close.
pub fn fact(expected: bool, now: Instant) {
    if !listening() {
        return;
    }
    let change = with_trust(|t| t.fact(expected, now));
    said(change);
}

/// The WM thread's tick: a rest that is over is heard again.
pub fn trust_tick(now: Instant) {
    if RESTING.load(Ordering::Relaxed) && with_trust(|t| t.brings_looks(now)) {
        RESTING.store(false, Ordering::Relaxed);
        tracing::info!("window-server notices: their rest is over: heard again on trial");
    }
}

/// `ctl state`'s `engine.notices.trust`: whether the notices bring the looks, and why not.
pub fn trust_status() -> Value {
    let now = clock::now();
    let listening = listening();
    let (word, rest_s, told, missed, for_nothing, rests) = with_trust(|t| {
        let (word, rest) = t.word(now);
        (word, rest, t.told, t.missed, t.for_nothing, t.rests)
    });
    let state = if listening { word } else { "off" };
    json!({
        "state": state,
        "way": if state == "working" { "window server" } else { "public triggers" },
        "why": match state {
            "off" => Some("the notices that bring a look are not registered in this run"),
            "silent" => Some("Mission Control opened or closed with no notice, several times in a row"),
            "resting" | "probing" => Some("many notices in a row had nothing to find"),
            _ => None,
        },
        "rest_s": rest_s,
        "told": told,
        "missed": missed,
        "for_nothing": for_nothing,
        "rests": rests,
    })
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
    if !hears(Kind::Added) {
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
    // Said once its wait is over, with the types not heard by then.
    if b.health.verdict(now, false) != Verdict::Silent {
        return;
    }
    let never = never_heard(b);
    match b.health.verdict(now, any_heard(b)) {
        Verdict::Heard => tracing::info!(never_heard = ?never, "window-server notices: heard (types never heard so far listed)"),
        _ => tracing::info!("window-server notices: silent (none came); the polls carry on as before"),
    }
    b.health_said = true;
}

/// True when any registered type was heard.
fn any_heard(b: &Book) -> bool {
    b.stats.values().any(|s| s.heard > 0)
}

/// The registered types never heard so far, by number.
fn never_heard(b: &Book) -> Vec<u32> {
    b.kinds.iter().filter(|k| b.stats.get(k).is_none_or(|s| s.heard == 0)).map(|k| k.code()).collect()
}

/// `ctl state`'s `engine.notices`: the registration, the launch probe's health, whether they
/// are trusted to bring the looks (`trust`), the layout, and per type registered what was
/// heard, when last (ms ago), how many bursts went to the WM thread, how many notices were
/// folded into them, how many were our own.
pub fn status() -> Value {
    let now = clock::now();
    let trust = trust_status();
    let g = book();
    let Some(b) = g.as_ref() else {
        return json!({ "registered": false, "notices": "off", "trust": trust });
    };
    let types: serde_json::Map<String, Value> = b
        .kinds
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
        "forwarded": HEARD.load(Ordering::Relaxed) != 0,
        "heard_types": b.kinds.iter().filter(|k| hears(**k)).map(|k| k.code()).collect::<Vec<_>>(),
        "trust": trust,
        "notices": b.health.verdict(now, any_heard(b)).name(),
        "never_heard": never_heard(b),
        "probe": { "created": b.health.created, "taken_out": b.health.taken_out, "added": b.health.added },
        "layout": Layout::unpack(LAYOUT.load(Ordering::Relaxed)).map(|l| json!({ "window": l.window, "space": l.space })),
        "expected": b.expected.len(),
        "dropped": DROPPED.load(Ordering::Relaxed),
        "types": types,
    })
}
