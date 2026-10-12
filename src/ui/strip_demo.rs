//! `stage-manager strip-demo`: the strip fed by a made-up snapshot, as a test harness.
//!
//! Every second it reads the live window list (no Accessibility, never moves a window),
//! groups windows into fake stages, treats the stage holding the frontmost window as active
//! (its frames drive auto-hide), and presents the rest as cards. Clicks are logged.
//!
//! Grouping (`STAGE_MANAGER_DEMO_LAYOUT`): `native` (default) is Apple's "All at Once", one
//! stage per app, so the strip can be compared like for like with Apple's; `chunks` splits
//! each app's windows into stages of 1, 2, 3 windows and lets the first two apps share a
//! card, to exercise stacks.
//!
//! The look comes from the built-in defaults, or from `$STAGE_MANAGER_HOME/config.toml` when
//! `STAGE_MANAGER_HOME` is set (re-read when it changes; never written). Environment also:
//! `STAGE_MANAGER_ONLY` (comma-separated bundle ids; their offscreen windows count too, so a
//! window moved offscreen still shows), `STAGE_MANAGER_DEMO_NUMBERS=1` (number badges),
//! `STAGE_MANAGER_DEMO_AUTOHIDE=0|1` (overrides Apple's `AutoHide`),
//! `STAGE_MANAGER_THUMBNAIL_MS` (capture interval), `STAGE_MANAGER_DEMO_MOTION=1` (a card
//! click plays the switch animation with snapshots of the windows involved; the real windows
//! never move, so the snapshots land over them).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::SystemTime;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSRunningApplication};
use objc2_foundation::{MainThreadMarker, NSTimer};

use crate::config::{Config, StripStyle};
use crate::engine::geometry::Rect;
use crate::platform::permissions::PermissionState;
use crate::platform::{logging, native_settings, windows};
use crate::ui::status_menu::{self, MenuAction, MenuState, StatusMenu};
use crate::ui::strip;
use crate::ui::strip_model::{self, CardSnapshot, CardWindow, MovingWindow, StripEvent, StripSnapshot, Transition};

/// Most cards the demo shows when the screen is unknown.
const MAX_CARDS: usize = 6;
/// Window list refresh period.
const REFRESH_SECONDS: f64 = 1.0;
/// Windows smaller than this are helpers, not documents.
const MIN_SIDE: f64 = 60.0;

/// A window as the demo sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DemoWindow {
    pub id: u32,
    pub pid: i32,
    pub frame: Rect,
    pub onscreen: bool,
}

/// Flags the demo copies into every snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DemoFlags {
    pub user_visible: bool,
    pub hide_recent_apps: bool,
    /// Apple's grouping (one stage per app) instead of chunks.
    pub native: bool,
    /// Most cards.
    pub capacity: usize,
    /// Card clicks play the switch animation (`STAGE_MANAGER_DEMO_MOTION`).
    pub motion: bool,
}

/// Builds the fake stages from `windows` (front to back).
pub fn demo_snapshot(windows: &[DemoWindow], flags: DemoFlags, style: Arc<StripStyle>) -> StripSnapshot {
    // Group by app, keeping front-to-back order of first appearance.
    let mut order: Vec<i32> = Vec::new();
    let mut by_pid: HashMap<i32, Vec<DemoWindow>> = HashMap::new();
    for w in windows {
        if !by_pid.contains_key(&w.pid) {
            order.push(w.pid);
        }
        by_pid.entry(w.pid).or_default().push(*w);
    }
    // Chunk each app's windows in sizes 1, 2, 3, 1, 2, 3, … (or one stage per app).
    let mut stages: Vec<Vec<DemoWindow>> = Vec::new();
    for pid in order.iter().filter(|_| flags.native) {
        stages.push(by_pid[pid].clone());
    }
    for pid in order.iter().filter(|_| !flags.native) {
        let list = &by_pid[pid];
        let mut i = 0;
        let mut size = 1;
        while i < list.len() {
            let end = (i + size).min(list.len());
            stages.push(list[i..end].to_vec());
            i = end;
            size = size % 3 + 1;
        }
    }
    // The first two apps share a card: the second app's first stage joins the first
    // app's last stage when they fit in three windows together.
    if order.len() >= 2 && !flags.native {
        let last_of_first = stages.iter().rposition(|s| s[0].pid == order[0]);
        let first_of_second = stages.iter().position(|s| s[0].pid == order[1]);
        if let (Some(a), Some(b)) = (last_of_first, first_of_second)
            && stages[a].len() + stages[b].len() <= 3
        {
            let moved = stages.remove(b);
            stages[a].extend(moved);
        }
    }
    let front = windows.iter().find(|w| w.onscreen).map(|w| w.id);
    let active = front.and_then(|id| stages.iter().position(|s| s.iter().any(|w| w.id == id)));
    let active_frames =
        active.map(|i| stages[i].iter().filter(|w| w.onscreen).map(|w| w.frame).collect()).unwrap_or_default();
    let active_windows = match active.filter(|_| flags.motion) {
        Some(i) => stages[i].iter().filter(|w| w.onscreen).map(|w| CardWindow { id: w.id, pid: w.pid, frame: w.frame }).collect(),
        None => Vec::new(),
    };

    let cards = stages
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != active)
        .take(flags.capacity.max(1))
        .map(|(i, s)| {
            let mut apps = Vec::new();
            for w in s {
                if !apps.contains(&w.pid) {
                    apps.push(w.pid);
                }
            }
            CardSnapshot {
                stage: i as u64 + 1,
                number: i + 1,
                name: (!flags.native && i % 3 == 2).then(|| format!("Stage {}", i + 1)),
                windows: s.iter().map(|w| CardWindow { id: w.id, pid: w.pid, frame: w.frame }).collect(),
                apps,
            }
        })
        .collect();
    StripSnapshot {
        cards,
        active_stage: active.map(|i| i as u64 + 1),
        hide_recent_apps: flags.hide_recent_apps,
        user_visible: flags.user_visible,
        style,
        active_frames,
        window_drag: None,
        announce: false,
        active_windows,
        transition: None,
        parked: None,
    }
}

// ---------------------------------------------------------------------------------------
// The running demo

struct Demo {
    menu: StatusMenu,
    flags: DemoFlags,
    /// `$STAGE_MANAGER_HOME/config.toml`, read when it changes.
    config_path: Option<PathBuf>,
    config_mtime: Option<SystemTime>,
    config: Config,
    style: Arc<StripStyle>,
    numbers: Option<bool>,
    only: Option<Vec<String>>,
    bundles: HashMap<i32, Option<String>>,
    last: Option<StripSnapshot>,
    transitions: u64,
    _timer: Option<Retained<NSTimer>>,
}

thread_local! {
    static DEMO: RefCell<Option<Demo>> = const { RefCell::new(None) };
}

fn with_demo(f: impl FnOnce(&mut Demo)) {
    DEMO.with(|d| {
        if let Ok(mut guard) = d.try_borrow_mut()
            && let Some(demo) = guard.as_mut()
        {
            f(demo);
        }
    });
}

impl Demo {
    fn bundle_of(&mut self, pid: i32) -> Option<String> {
        self.bundles
            .entry(pid)
            .or_insert_with(|| {
                NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                    .and_then(|a| a.bundleIdentifier())
                    .map(|s| s.to_string())
            })
            .clone()
    }

    /// The live windows the demo shows, front to back.
    fn live_windows(&mut self) -> Vec<DemoWindow> {
        let own = std::process::id() as i32;
        let mut out = Vec::new();
        for w in windows::list_windows() {
            if w.layer != 0 || w.pid == own || w.bounds.w < MIN_SIDE || w.bounds.h < MIN_SIDE {
                continue;
            }
            let bundle = self.bundle_of(w.pid);
            let listed = match (&self.only, &bundle) {
                (Some(only), Some(b)) => only.iter().any(|o| o == b),
                (Some(_), None) => false,
                (None, Some(b)) => !windows::is_excluded_bundle(b),
                (None, None) => false,
            };
            // Offscreen windows count only for listed apps (a "parked" fixture window).
            if !listed || (!w.onscreen && self.only.is_none()) {
                continue;
            }
            let b = w.bounds;
            out.push(DemoWindow { id: w.id, pid: w.pid, frame: Rect::new(b.x, b.y, b.w, b.h), onscreen: w.onscreen });
        }
        out
    }

    /// Re-reads the config when its file changed (never writes it).
    fn reload_config(&mut self) {
        let Some(path) = &self.config_path else { return };
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if mtime == self.config_mtime {
            return;
        }
        self.config_mtime = mtime;
        let text = std::fs::read_to_string(path).unwrap_or_default();
        match Config::parse(&text) {
            Ok(p) => {
                for w in p.warnings {
                    tracing::warn!("config: {w}");
                }
                tracing::info!("demo: config read from {}", path.display());
                self.config = p.config;
            }
            Err(e) => tracing::error!("demo: {e}; keeping the last good config"),
        }
        let mut style = self.config.style.clone();
        if let Some(n) = self.numbers {
            style.show_numbers = n;
        }
        if *self.style != style {
            self.style = Arc::new(style);
        }
        strip::set_thumbnail_interval(self.config.strip.thumbnail_interval_ms);
    }

    /// With `debug-render`: `STAGE_MANAGER_DEMO_FAKE="<bundle id>:<w>x<h>;..."` stands in for
    /// the live window list (one window per entry, front to back; the first is active).
    #[cfg(feature = "debug-render")]
    fn fake_windows(&mut self) -> Option<Vec<DemoWindow>> {
        let spec = std::env::var("STAGE_MANAGER_DEMO_FAKE").ok()?;
        let mut out = Vec::new();
        for (i, item) in spec.split(';').filter(|s| !s.is_empty()).enumerate() {
            let (bundle, size) = item.rsplit_once(':')?;
            let (w, h) = size.split_once('x')?;
            let pid = objc2_app_kit::NSRunningApplication::runningApplicationsWithBundleIdentifier(
                &objc2_foundation::NSString::from_str(bundle),
            )
            .firstObject()
            .map_or(-(i as i32) - 1, |a| a.processIdentifier());
            let frame = Rect::new(300.0, 100.0, w.parse().ok()?, h.parse().ok()?);
            out.push(DemoWindow { id: 900_000 + i as u32, pid, frame, onscreen: true });
        }
        Some(out)
    }

    fn windows_now(&mut self) -> Vec<DemoWindow> {
        #[cfg(feature = "debug-render")]
        if let Some(fake) = self.fake_windows() {
            return fake;
        }
        self.live_windows()
    }

    fn refresh(&mut self) {
        self.reload_config();
        if let Some(mtm) = MainThreadMarker::new()
            && let Some(d) = crate::platform::desktop::main_display(mtm)
        {
            self.flags.capacity = crate::engine::stage_area::capacity_for(d.visible.size(), &self.config.strip.capacity, &self.style);
        }
        let snapshot = demo_snapshot(&self.windows_now(), self.flags, self.style.clone());
        if self.last.as_ref() != Some(&snapshot) {
            tracing::debug!(
                cards = snapshot.cards.len(),
                active_frames = snapshot.active_frames.len(),
                user_visible = snapshot.user_visible,
                "demo snapshot"
            );
            self.last = Some(snapshot.clone());
            strip_model::present(snapshot);
        }
    }

    /// With `STAGE_MANAGER_DEMO_MOTION`: plays a switch to `stage`'s card. The active windows'
    /// snapshots shrink into the card and the card's windows' snapshots grow to their frames;
    /// no window moves.
    fn play_switch(&mut self, stage: u64) {
        let Some(mut s) = self.last.clone().filter(|_| self.flags.motion) else { return };
        let Some(card) = s.cards.iter().find(|c| c.stage == stage).cloned() else { return };
        self.transitions += 1;
        let id = self.transitions;
        let moving = |w: &CardWindow| MovingWindow { window: *w, stage: Some(stage) };
        s.transition = Some(Transition {
            id,
            outgoing: s.active_windows.iter().rev().map(moving).collect(),
            incoming: card.windows.iter().rev().map(moving).collect(),
        });
        tracing::info!(id, stage, "demo: playing a switch");
        strip_model::present(s.clone());
        s.transition = None;
        s.parked = Some(id);
        let parked = RcBlock::new(move |_t: NonNull<NSTimer>| strip_model::present(s.clone()));
        // SAFETY: the block runs on the main run loop only; the run loop keeps the timer.
        let _once = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.04, false, &parked) };
    }

    fn on_menu(&mut self, action: MenuAction) {
        match action {
            MenuAction::ToggleStrip => {
                self.flags.user_visible = !self.flags.user_visible;
                tracing::info!(visible = self.flags.user_visible, "demo: strip toggled");
                self.menu.update(&self.menu_state());
                self.refresh();
            }
            MenuAction::Quit => {
                tracing::info!("demo: quitting");
                std::process::exit(0);
            }
            other => tracing::info!(?other, "demo: menu item ignored"),
        }
    }

    fn menu_state(&self) -> MenuState {
        MenuState {
            permissions: PermissionState::current(),
            apple_stage_manager_on: false,
            strip_visible: self.flags.user_visible,
            config_error: None,
        }
    }
}

fn env_flag(name: &str) -> Option<bool> {
    std::env::var(name).ok().map(|v| v != "0" && !v.is_empty())
}

/// Runs the demo until Quit (menu) or a signal.
pub fn run() -> ExitCode {
    logging::init();
    tracing::info!(pid = std::process::id(), "strip demo starting");
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::error!("not on the main thread");
        return ExitCode::FAILURE;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let config_path = crate::platform::paths::custom_home().map(|h| h.join("config.toml"));
    let interval = std::env::var("STAGE_MANAGER_THUMBNAIL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(Config::default().strip.thumbnail_interval_ms);
    strip::install(mtm, interval);
    strip_model::set_event_sink(|event| {
        tracing::info!(?event, "demo: strip event");
        if let StripEvent::CardClicked { stage, shift: false } = event {
            with_demo(|d| d.play_switch(stage));
        }
    });

    let only = std::env::var("STAGE_MANAGER_ONLY")
        .ok()
        .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>())
        .filter(|v| !v.is_empty());
    let hide_recent_apps = env_flag("STAGE_MANAGER_DEMO_AUTOHIDE")
        .unwrap_or_else(|| native_settings::read().get("AutoHide").copied().unwrap_or(0) != 0);
    let flags = DemoFlags {
        user_visible: true,
        hide_recent_apps,
        native: std::env::var("STAGE_MANAGER_DEMO_LAYOUT").map_or(true, |v| v != "chunks"),
        capacity: MAX_CARDS,
        motion: env_flag("STAGE_MANAGER_DEMO_MOTION").unwrap_or(false),
    };
    let numbers = env_flag("STAGE_MANAGER_DEMO_NUMBERS");
    tracing::info!(?only, ?flags, interval, "demo settings");

    let menu = StatusMenu::new(mtm);
    let config = Config::default();
    let mut style = config.style.clone();
    if let Some(n) = numbers {
        style.show_numbers = n;
    }
    let demo = Demo {
        menu,
        flags,
        config_path,
        config_mtime: None,
        config,
        style: Arc::new(style),
        numbers,
        only,
        bundles: HashMap::new(),
        last: None,
        transitions: 0,
        _timer: None,
    };
    demo.menu.update(&demo.menu_state());
    DEMO.with(|d| *d.borrow_mut() = Some(demo));
    status_menu::set_action_handler(|action| with_demo(|d| d.on_menu(action)));

    let tick = RcBlock::new(|_t: NonNull<NSTimer>| with_demo(Demo::refresh));
    // SAFETY: the block runs on the main run loop only.
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(REFRESH_SECONDS, true, &tick) };
    with_demo(|d| {
        d._timer = Some(timer);
        d.refresh();
    });
    #[cfg(feature = "debug-render")]
    if let Some((w, h, top)) = std::env::var("STAGE_MANAGER_DEMO_SCREEN").ok().and_then(|v| {
        // "<width>x<height>+<menu bar height>", for example "1800x1169+39".
        let (size, top) = v.split_once('+')?;
        let (w, h) = size.split_once('x')?;
        Some((w.parse::<f64>().ok()?, h.parse::<f64>().ok()?, top.parse::<f64>().ok()?))
    }) {
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        strip::set_screen_override(
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h - top)),
        );
        with_demo(Demo::refresh);
    }
    #[cfg(feature = "debug-render")]
    if let Some(path) = std::env::var_os("STAGE_MANAGER_DEMO_RENDER") {
        let render = RcBlock::new(move |_t: NonNull<NSTimer>| {
            let r = strip::debug_render(std::path::Path::new(&path));
            tracing::info!(?r, "demo: rendered the strip offscreen");
            std::process::exit(if r.is_ok() { 0 } else { 1 });
        });
        // SAFETY: the block runs on the main run loop only.
        let _once = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(2.5, false, &render) };
    }

    app.run();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: u32, pid: i32, onscreen: bool) -> DemoWindow {
        DemoWindow { id, pid, frame: Rect::new(300.0 + id as f64, 100.0, 500.0, 400.0), onscreen }
    }

    const FLAGS: DemoFlags = DemoFlags { user_visible: true, hide_recent_apps: false, native: false, capacity: 6, motion: false };

    fn demo_snapshot(windows: &[DemoWindow], flags: DemoFlags) -> StripSnapshot {
        super::demo_snapshot(windows, flags, Arc::new(StripStyle::default()))
    }

    #[test]
    fn one_app_chunks_into_stages_and_the_front_one_is_active() {
        let list: Vec<_> = (1..=6).map(|i| w(i, 7, true)).collect();
        let s = demo_snapshot(&list, FLAGS);
        // Stages [1], [2,3], [4,5,6]; [1] holds the front window and is active.
        assert_eq!(s.active_stage, Some(1));
        assert_eq!(s.active_frames.len(), 1);
        let ids: Vec<Vec<u32>> = s.cards.iter().map(|c| c.windows.iter().map(|w| w.id).collect()).collect();
        assert_eq!(ids, vec![vec![2, 3], vec![4, 5, 6]]);
        assert!(s.user_visible);
    }

    #[test]
    fn the_first_two_apps_share_a_card() {
        let list = vec![w(1, 7, true), w(2, 7, true), w(3, 7, true), w(10, 8, true), w(11, 8, true)];
        let s = demo_snapshot(&list, FLAGS);
        // App 7: [1], [2,3]; app 8: [10], [11]. [2,3] + [10] share a card.
        let mixed = s.cards.iter().find(|c| c.apps.len() == 2).map(|c| c.windows.iter().map(|w| w.id).collect::<Vec<_>>());
        assert_eq!(mixed, Some(vec![2, 3, 10]));
    }

    #[test]
    fn offscreen_windows_are_cards_but_never_active() {
        let list = vec![w(1, 7, false), w(2, 7, true), w(3, 7, true)];
        let s = demo_snapshot(&list, FLAGS);
        assert_eq!(s.active_stage, Some(2));
        assert!(s.cards.iter().any(|c| c.windows.iter().any(|w| w.id == 1)));
        assert!(demo_snapshot(&[], FLAGS).cards.is_empty());
    }

    #[test]
    fn the_native_layout_is_one_card_per_app() {
        let list = vec![w(1, 7, true), w(2, 7, true), w(10, 8, true), w(11, 8, true), w(20, 9, true)];
        let s = demo_snapshot(&list, DemoFlags { native: true, ..FLAGS });
        let ids: Vec<Vec<u32>> = s.cards.iter().map(|c| c.windows.iter().map(|w| w.id).collect()).collect();
        assert_eq!(ids, vec![vec![10, 11], vec![20]]);
        assert!(s.cards.iter().all(|c| c.name.is_none()));
        assert!(s.active_windows.is_empty(), "no snapshots of real windows unless asked");
        let s = demo_snapshot(&list, DemoFlags { native: true, motion: true, ..FLAGS });
        assert_eq!(s.active_windows.iter().map(|w| w.id).collect::<Vec<_>>(), [1, 2]);
    }
}
