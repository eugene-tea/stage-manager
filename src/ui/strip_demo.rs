//! `stage-manager strip-demo`: the strip fed by a made-up snapshot, as a test harness.
//!
//! Every second it reads the live window list (no Accessibility, never moves a window),
//! groups windows by app into fake stages (chunks of 1, 2, 3 windows; the first two apps
//! share one card), treats the stage holding the frontmost window as active (its frames
//! drive auto-hide), and presents the rest as cards. Clicks are logged.
//!
//! Environment: `STAGE_MANAGER_ONLY` (comma-separated bundle ids; their offscreen windows
//! count too, so a window moved offscreen still shows), `STAGE_MANAGER_DEMO_NUMBERS=1`
//! (number badges), `STAGE_MANAGER_DEMO_AUTOHIDE=0|1` (overrides Apple's `AutoHide`),
//! `STAGE_MANAGER_THUMBNAIL_MS` (capture interval, default 1500).

use std::cell::RefCell;
use std::collections::HashMap;
use std::process::ExitCode;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSRunningApplication};
use objc2_foundation::{MainThreadMarker, NSTimer};

use crate::engine::geometry::Rect;
use crate::platform::permissions::PermissionState;
use crate::platform::{logging, native_settings, windows};
use crate::ui::status_menu::{self, MenuAction, MenuState, StatusMenu};
use crate::ui::strip;
use crate::ui::strip_model::{self, CardSnapshot, CardWindow, StripSnapshot};

/// Most cards the demo shows (Apple shows six at 1440 pt).
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
    pub show_numbers: bool,
}

/// Builds the fake stages from `windows` (front to back).
pub fn demo_snapshot(windows: &[DemoWindow], flags: DemoFlags) -> StripSnapshot {
    // Group by app, keeping front-to-back order of first appearance.
    let mut order: Vec<i32> = Vec::new();
    let mut by_pid: HashMap<i32, Vec<DemoWindow>> = HashMap::new();
    for w in windows {
        if !by_pid.contains_key(&w.pid) {
            order.push(w.pid);
        }
        by_pid.entry(w.pid).or_default().push(*w);
    }
    // Chunk each app's windows in sizes 1, 2, 3, 1, 2, 3, …
    let mut stages: Vec<Vec<DemoWindow>> = Vec::new();
    for pid in &order {
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
    if order.len() >= 2 {
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

    let cards = stages
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != active)
        .take(MAX_CARDS)
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
                name: (i % 3 == 2).then(|| format!("Stage {}", i + 1)),
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
        show_numbers: flags.show_numbers,
        active_frames,
    }
}

// ---------------------------------------------------------------------------------------
// The running demo

struct Demo {
    menu: StatusMenu,
    flags: DemoFlags,
    only: Option<Vec<String>>,
    bundles: HashMap<i32, Option<String>>,
    last: Option<StripSnapshot>,
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

    fn refresh(&mut self) {
        let snapshot = demo_snapshot(&self.live_windows(), self.flags);
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

    let interval = std::env::var("STAGE_MANAGER_THUMBNAIL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
    strip::install(mtm, interval);
    strip_model::set_event_sink(|event| tracing::info!(?event, "demo: strip event"));

    let only = std::env::var("STAGE_MANAGER_ONLY")
        .ok()
        .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>())
        .filter(|v| !v.is_empty());
    let hide_recent_apps = env_flag("STAGE_MANAGER_DEMO_AUTOHIDE")
        .unwrap_or_else(|| native_settings::read().get("AutoHide").copied().unwrap_or(0) != 0);
    let flags = DemoFlags {
        user_visible: true,
        hide_recent_apps,
        show_numbers: env_flag("STAGE_MANAGER_DEMO_NUMBERS").unwrap_or(false),
    };
    tracing::info!(?only, ?flags, interval, "demo settings");

    let menu = StatusMenu::new(mtm);
    let demo = Demo { menu, flags, only, bundles: HashMap::new(), last: None, _timer: None };
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

    app.run();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: u32, pid: i32, onscreen: bool) -> DemoWindow {
        DemoWindow { id, pid, frame: Rect::new(300.0 + id as f64, 100.0, 500.0, 400.0), onscreen }
    }

    const FLAGS: DemoFlags = DemoFlags { user_visible: true, hide_recent_apps: false, show_numbers: true };

    #[test]
    fn one_app_chunks_into_stages_and_the_front_one_is_active() {
        let list: Vec<_> = (1..=6).map(|i| w(i, 7, true)).collect();
        let s = demo_snapshot(&list, FLAGS);
        // Stages [1], [2,3], [4,5,6]; [1] holds the front window and is active.
        assert_eq!(s.active_stage, Some(1));
        assert_eq!(s.active_frames.len(), 1);
        let ids: Vec<Vec<u32>> = s.cards.iter().map(|c| c.windows.iter().map(|w| w.id).collect()).collect();
        assert_eq!(ids, vec![vec![2, 3], vec![4, 5, 6]]);
        assert!(s.show_numbers && s.user_visible);
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
}
