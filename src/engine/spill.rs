//! A screen going away under the Spaces engine: its hidden windows stay in their stages, on
//! their own Desktops (the pure half; the calls are
//! [`crate::platform::spaces::engine::SpacesEngine::keep_through_display_change`]'s).
//!
//! **What happens.** A stage Space is made on the screen whose Desktop it serves. When that
//! screen goes (unplugged, the lid closed), macOS moves the screen's Desktops onto the screen
//! left, under the same ids, but the window server takes the windows out of the screen's stage
//! Spaces and puts them on a Desktop of its own choosing, not always theirs (a log, 11-18: the
//! main screen unplugged, two hidden stages of its Desktop shown were listed on its other
//! Desktop a moment later). A hide then read the Desktop each window was on as its home, so a
//! later show would have put it there, and once that Desktop showed, the model saw known
//! windows of another Desktop on screen and filed them into its stages: the Desktop they came
//! from kept 3 windows of its 7.
//!
//! **The rule.** For a while after a screen went ([`WATCH`]), a window this engine holds is
//! kept on the Desktop it was hidden from while that Desktop exists: a window out of its stage
//! Space goes back into a stage Space of that Desktop ([`plan`]), and a hide takes that
//! Desktop as its home, not the one the window shows on ([`hide_home`]). Each stage Space of
//! a screen that is gone, and each stage Space a window came out of, is made again (the old
//! one is not trusted to hold its windows any more) and its windows moved in, hidden; the old
//! one ends once empty. A window whose Desktop is gone too is kept hidden in a stage Space of
//! the Desktop it shows on (else the one shown): its stage still stands, on that Desktop. A
//! window shown since its last hide (in front as the screen went) is hidden from the Desktop
//! its screen shows, not the one the window server lists it on mid-change ([`MID_CHANGE`]: in
//! the change's settle, no click or key since). Later in the watch such a window is hidden
//! from the Desktop it is listed on: a Desktop not shown whose stage in front goes to a card
//! (a Dock click or ⌘-Tab arranges that Desktop before macOS slides to it) keeps it (a log,
//! 11-21: the external screen back after a day on the laptop alone, a Dock click 36 s after
//! the change hid the arriving Desktop's front window into a stage of the Desktop still
//! shown; it was counted gone 3 s later and showed up on the other Desktop).
//!
//! **A Desktop that ends on a screen still there.** The main screen gone, macOS moves its
//! Desktops onto the screen left and ends the Desktop it had made there when the other screen
//! took the main role (a live run on a virtual screen, 11-18): that Desktop's shown windows go
//! onto the Desktop moved in, its stage Spaces stay where they were, on a Desktop no longer
//! listed, and a hidden window there counted gone left its stage, came out as the stage Space
//! was swept, and joined another stage of its app. So a stage Space whose Desktop ended
//! ([`Map::ended`]: unlisted for [`ENDED_AFTER`] while the display list holds still) is made again too, for the Desktop that
//! Desktop's shown windows are on now ([`Map::landed`]), else the one a window of it was let
//! out onto, else the one its screen shows, else the main screen's: its stages join there
//! whole, hidden, as the model takes the Desktop's stages ([`crate::engine::model::Model`]'s
//! adoption of a gone screen's Desktop).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::engine::geometry::Rect;

/// How long after a screen went (or after the unlock, when it went while the screen was
/// locked) hidden windows are kept on their own Desktops ([`plan`], [`hide_home`]): the
/// window server let windows out of stage Spaces as late as the first look at a moved
/// Desktop (a log, 11-18: 20 s after the change).
pub const WATCH: Duration = Duration::from_secs(60);

/// How long after a screen went (or after the unlock) a hide counts as the display change's
/// own, with no click or key since ([`mid_change`]): the settle of a display change, as the
/// controller's (an activation macOS makes then switches nothing).
pub const MID_CHANGE: Duration = Duration::from_secs(10);

/// True when a hide now is part of the display change itself, not the user's doing: the
/// watch began `since_watch` ago ([`WATCH`], restarted at the unlock), within [`MID_CHANGE`],
/// and the user's last click or key press (`since_press` ago) came before it. Only then may
/// the window server list a window in front on another Desktop than the one its screen
/// shows ([`hide_home`]). `None`: no watch.
pub fn mid_change(since_watch: Option<Duration>, since_press: Duration) -> bool {
    crate::engine::displays::macos_after_display_change(since_watch, since_press, MID_CHANGE)
}

/// How often the tick looks for hidden windows let out of their stage Space while
/// [`WATCH`]ing: faster than the model's look for windows of another Desktop on screen
/// ([`crate::engine::overview::FOREIGN_SETTLE`], two sightings a second apart).
pub const KEEP_EVERY: Duration = Duration::from_millis(500);

/// A window the engine holds hidden, as the window server lists it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldNow {
    pub id: u32,
    /// The stage Space it was put in.
    pub space: u64,
    /// The Desktop it was hidden from.
    pub home: u64,
    /// The Spaces the window server lists it on now.
    pub now: Vec<u64>,
}

impl HeldNow {
    /// True when it is out of its stage Space, on another Space (a Desktop): let out by the
    /// window server. A window on no Space (minimized, ordered out) is not.
    pub fn spilled(&self) -> bool {
        !self.now.is_empty() && self.now != [self.space]
    }
}

/// What the engine knows of the screens and its stage Spaces.
#[derive(Debug, Clone, Default)]
pub struct Map {
    /// (Desktop, stage) to its stage Space.
    pub stages: BTreeMap<(u64, u64), u64>,
    /// The screen (the window server's display identifier) each stage Space was made on.
    pub screens: BTreeMap<u64, String>,
    /// The screens there now.
    pub screens_now: BTreeSet<String>,
    /// Every Desktop of every screen now.
    pub desktops: BTreeSet<u64>,
    /// The Desktops shown now, the main screen's first.
    pub shown: Vec<u64>,
    /// Each screen there now, with the Desktop it shows.
    pub screen_shown: BTreeMap<String, u64>,
    /// Desktops that ended (a stage's Desktop unlisted for [`ENDED_AFTER`],
    /// [`ended_desktops`]): their stage Spaces are made again on another Desktop, on any
    /// screen.
    pub ended: BTreeSet<u64>,
    /// An ended Desktop, with the Desktop its shown windows are on now (by the model's word
    /// of which windows showed there): where its stages join.
    pub landed: BTreeMap<u64, u64>,
}

impl Map {
    /// True when stage Space `space` was made on a screen that is gone. A Space with no screen
    /// noted is not (nothing says so).
    fn screen_gone(&self, space: u64) -> bool {
        self.screens.get(&space).is_some_and(|s| !s.is_empty() && !self.screens_now.contains(s))
    }
}

/// One stage Space made again: its windows into a new stage Space of `to`, the old one ended
/// once empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remake {
    /// The stage Space now.
    pub old: u64,
    pub stage: u64,
    /// The Desktop it was made for.
    pub from: u64,
    /// The Desktop the new one is for: `from` while it exists.
    pub to: u64,
    /// Every window held in it, let out or not.
    pub windows: Vec<u32>,
    /// Those the window server let out.
    pub spilled: Vec<u32>,
}

impl Remake {
    /// True when its Desktop is gone and its windows are kept on another one.
    pub fn rehomed(&self) -> bool {
        self.from != self.to
    }
}

/// What a look while watching found to do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub remakes: Vec<Remake>,
    /// Windows let out of a stage Space this engine has no key for (nothing to make again):
    /// hidden again by the model's word instead.
    pub unkeyed: Vec<u32>,
}

impl Plan {
    /// True when nothing is to be done.
    pub fn is_empty(&self) -> bool {
        self.remakes.is_empty() && self.unkeyed.is_empty()
    }
}

/// The stage Spaces to make again (see the module docs): each one of a screen that is gone,
/// and each one a window was let out of. `held`: the windows held (not lent, not waiting as
/// new, not flying, no show owed), as the window server lists them now.
pub fn plan(held: &[HeldNow], map: &Map) -> Plan {
    let key_of: BTreeMap<u64, (u64, u64)> = map.stages.iter().map(|(k, s)| (*s, *k)).collect();
    let mut by_space: BTreeMap<u64, Vec<&HeldNow>> = BTreeMap::new();
    for h in held {
        by_space.entry(h.space).or_default().push(h);
    }
    let mut out = Plan::default();
    for (space, members) in by_space {
        let spilled: Vec<u32> = members.iter().filter(|h| h.spilled()).map(|h| h.id).collect();
        let key = key_of.get(&space).copied();
        let ended = key.is_some_and(|(from, _)| map.ended.contains(&from));
        if spilled.is_empty() && !map.screen_gone(space) && !ended {
            continue;
        }
        let Some((from, stage)) = key else {
            out.unkeyed.extend(spilled);
            continue;
        };
        let surfaced = members.iter().find(|h| h.spilled()).and_then(|h| desktop_of(&h.now, &map.desktops, &map.shown));
        let exists = |d: &u64| map.desktops.contains(d);
        // Its own Desktop while it exists; else where that Desktop's shown windows went, where
        // a window of it surfaced, what its screen shows, the main screen's.
        let screen_shows = map.screens.get(&space).and_then(|s| map.screen_shown.get(s)).copied();
        let to = Some(from)
            .filter(exists)
            .or(map.landed.get(&from).copied().filter(exists))
            .or(surfaced)
            .or(screen_shows)
            .or(map.shown.first().copied());
        let Some(to) = to else {
            out.unkeyed.extend(spilled);
            continue;
        };
        out.remakes.push(Remake { old: space, stage, from, to, windows: members.iter().map(|h| h.id).collect(), spilled });
    }
    out
}

/// The windows the window server lists in a stage Space being made again (`listed`, its
/// [`Remake::old`]) that go along into the new one: those nothing holds (`held`: every
/// window held, a candidate or not), a held window's companions (an attached panel). A held
/// window left out of the remake (lent, waiting as new, flying, a show owed) is where it is
/// on purpose and stays.
pub fn followers(listed: &[u32], held: &BTreeSet<u32>) -> Vec<u32> {
    listed.iter().copied().filter(|id| !held.contains(id)).collect()
}

/// True when a stage Space made again ([`Remake::old`]) may end: the window server lists no
/// window in it after the moves (`listed`), and no held window names it (`held`: each held
/// window's stage Space, a candidate or not), so a lent window's return never names a Space
/// gone. Else it is kept as retired until it empties.
pub fn old_ends(old: u64, listed: &[u32], held: impl IntoIterator<Item = u64>) -> bool {
    listed.is_empty() && !held.into_iter().any(|s| s == old)
}

/// How long a Desktop a stage is kept for stays unlisted, with the display list unchanged,
/// before it counts as ended ([`ended_desktops`]): a window server slow to list a gone
/// screen's Desktops on the screen left would otherwise have their stages joined onto the
/// Desktop shown, and nothing moves them back.
pub const ENDED_AFTER: Duration = Duration::from_millis(2500);

/// What the looks while watching have seen of unlisted Desktops ([`ended_desktops`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unlisted {
    /// Each Desktop a stage is kept for that is unlisted, with since when (with the display
    /// list as it is now).
    since: BTreeMap<u64, Instant>,
    /// The display list at the last look: each screen with its Desktops.
    displays: Vec<(String, Vec<u64>)>,
}

/// The Desktops ended at this look: those of `homes` (Desktops a stage is kept for) not
/// listed now that have been unlisted for [`ENDED_AFTER`] at least while the display list
/// (`displays`: each screen with its Desktops, as the window server lists them now) stayed
/// as it is: one look mid-change may list a screen's Desktops only in part, and a window
/// server slow to list them may do so for a while. A display list changed since the look
/// before starts every wait again. With what this look saw, for the next.
pub fn ended_desktops(homes: impl IntoIterator<Item = u64>, displays: &[(String, Vec<u64>)], before: &Unlisted, now: Instant) -> (BTreeSet<u64>, Unlisted) {
    let listed: BTreeSet<u64> = displays.iter().flat_map(|(_, d)| d.iter().copied()).collect();
    let same = before.displays == displays;
    let since: BTreeMap<u64, Instant> = homes
        .into_iter()
        .filter(|d| !listed.contains(d))
        .map(|d| (d, before.since.get(&d).copied().filter(|_| same).unwrap_or(now)))
        .collect();
    let ended = since.iter().filter(|(_, t)| now.saturating_duration_since(**t) >= ENDED_AFTER).map(|(d, _)| *d).collect();
    (ended, Unlisted { since, displays: displays.to_vec() })
}

/// The Desktop an ended Desktop's shown windows are on now: of `peers` (each one's Spaces
/// now, the windows the model shows there), the Desktop most are on (a tie: one shown, then
/// the one first met). None when none is on a Desktop.
pub fn landed_on(peers: &[Vec<u64>], desktops: &BTreeSet<u64>, shown: &[u64]) -> Option<u64> {
    let mut count: Vec<(u64, usize)> = Vec::new();
    for now in peers {
        let Some(d) = desktop_of(now, desktops, shown) else { continue };
        match count.iter_mut().find(|(c, _)| *c == d) {
            Some((_, n)) => *n += 1,
            None => count.push((d, 1)),
        }
    }
    let best = count.iter().map(|(_, n)| *n).max()?;
    let tied: Vec<u64> = count.iter().filter(|(_, n)| *n == best).map(|(d, _)| *d).collect();
    tied.iter().copied().find(|d| shown.contains(d)).or(tied.first().copied())
}

/// The Desktop among `now` (a window's Spaces): one shown first, else any Desktop.
fn desktop_of(now: &[u64], desktops: &BTreeSet<u64>, shown: &[u64]) -> Option<u64> {
    now.iter().copied().find(|s| shown.contains(s)).or_else(|| now.iter().copied().find(|s| desktops.contains(s)))
}

/// Where a hide takes a window's home: what [`hide_home`] weighs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HomeHints {
    /// The Desktop the window shows on now.
    pub on: Option<u64>,
    /// The Desktop it was hidden from, while held (or the one its last stretch noted).
    pub held: Option<u64>,
    /// The Desktop it was last hidden from, kept after the engine let it go (a window let out
    /// by the window server is no longer held).
    pub last: Option<u64>,
    /// A Desktop its stage has a stage Space for: weighed only for a window on no Desktop (a
    /// stage's Space of a Desktop it left lingers while the stage lives).
    pub stage: Option<u64>,
    /// The Desktop shown (the main screen's).
    pub shown: Option<u64>,
    /// The Desktop shown on the screen that lists `on` ([`showing_on`]): `on` itself when it
    /// shows.
    pub showing: Option<u64>,
    /// The hide is the display change's own ([`mid_change`]): `showing` is weighed only then.
    pub mid_change: bool,
}

/// The Desktop a hide takes as the window's home. Outside the [`WATCH`] (`keep` false), the
/// Desktop it shows on, else the one it was held on, else the one shown (as ever). While
/// watching, the Desktop it was hidden from comes first while that Desktop exists (held,
/// else last), then the one it shows on, then its stage's: the Desktop the window server put
/// it on as its screen went is not its own. A window shown since it was last hidden has no
/// last Desktop (a show forgets it): within the change itself ([`HomeHints::mid_change`]) it
/// is hidden from the Desktop its screen shows, not the one the window server lists it on,
/// which mid-change may be another Desktop of that screen (a live run, 11-19: the main screen
/// gone, the built-in screen's own Desktop ended, its window in front was listed on Desktop 1
/// while the screen showed Desktop 4; hidden into a Space of Desktop 1, it counted gone from
/// the Desktop shown and lost its stage). After that, from the Desktop it is listed on: the
/// front window of a Desktop not shown, hidden as a Dock click or ⌘-Tab arranges that Desktop
/// ahead of the slide, is on its own Desktop (a log, 11-21; module docs).
pub fn hide_home(h: HomeHints, desktops: &BTreeSet<u64>, keep: bool) -> Option<u64> {
    if !keep {
        return h.on.or(h.held).or(h.shown);
    }
    let exists = |d: &u64| desktops.contains(d);
    let kept = h.held.filter(exists).or(h.last.filter(exists));
    let fresh = h.held.is_none() && h.last.is_none();
    kept.or(h.showing.filter(|_| fresh && h.mid_change)).or(h.on).or(h.stage.filter(exists)).or(h.held).or(h.shown)
}

/// The Desktop shown on the screen that lists Desktop `desktop`, of `listing` (each screen's
/// Desktop shown, 0 when none, and its Desktops): None when no screen lists it or that screen
/// shows none.
pub fn showing_on(listing: &[(u64, Vec<u64>)], desktop: u64) -> Option<u64> {
    listing.iter().find(|(_, spaces)| spaces.contains(&desktop)).map(|(current, _)| *current).filter(|c| *c != 0)
}

/// The Desktop a window goes to when its own is gone or not known: the one shown on its own
/// screen, that is the screen its stage Space was made on (`made_on`), else the screen that
/// holds its frame (`holding`, asked only then); the first screen's only as the last resort.
/// `listing`: each screen's identifier, in the window server's order, with its Desktop shown
/// (0 when none). Before, a window of the second screen whose Desktop went was shown on, or
/// swept onto, the first screen's Desktop.
pub fn shown_on_own_screen(listing: &[(String, u64)], made_on: Option<&str>, holding: impl FnOnce() -> Option<String>) -> Option<u64> {
    let shown_on = |screen: &str| listing.iter().find(|(s, _)| s == screen).map(|(_, current)| *current).filter(|c| *c != 0);
    made_on
        .and_then(shown_on)
        .or_else(|| holding().as_deref().and_then(shown_on))
        .or_else(|| listing.iter().map(|(_, current)| *current).find(|c| *c != 0))
}

/// The screen of `screens` (identifier, bounds) that holds the most of `frame`; None when
/// none holds any of it.
pub fn screen_holding(screens: &[(String, Rect)], frame: Rect) -> Option<String> {
    screens
        .iter()
        .filter_map(|(s, bounds)| bounds.intersect(&frame).map(|r| (s, r.area())))
        .filter(|(_, area)| *area > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(s, _)| s.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window of the second screen whose Desktop is gone: the Desktop shown on its stage
    /// Space's screen, else on the screen holding its frame; the first screen's only when
    /// neither is known.
    #[test]
    fn a_window_whose_desktop_is_gone_goes_to_the_desktop_shown_on_its_own_screen() {
        let listing = vec![(LAPTOP.to_string(), 4), (GONE.to_string(), 0), ("studio".to_string(), 9)];
        let never = || -> Option<String> { panic!("the frame is not asked when the stage's screen tells") };
        assert_eq!(shown_on_own_screen(&listing, Some("studio"), never), Some(9), "its stage Space's screen");
        assert_eq!(shown_on_own_screen(&listing, Some(GONE), || Some("studio".into())), Some(9), "that screen shows none: the frame's");
        assert_eq!(shown_on_own_screen(&listing, None, || Some("studio".into())), Some(9), "no stage Space: the frame's");
        assert_eq!(shown_on_own_screen(&listing, Some("unplugged"), || None), Some(4), "neither known: the first screen's");
        assert_eq!(shown_on_own_screen(&[(GONE.to_string(), 0)], None, || None), None, "no screen shows a Desktop");
        let screens = vec![(LAPTOP.to_string(), Rect::new(0.0, 0.0, 1512.0, 982.0)), ("studio".to_string(), Rect::new(1512.0, -400.0, 2560.0, 1440.0))];
        assert_eq!(screen_holding(&screens, Rect::new(1400.0, 100.0, 800.0, 600.0)).as_deref(), Some("studio"), "most of it");
        assert_eq!(screen_holding(&screens, Rect::new(100.0, 100.0, 800.0, 600.0)).as_deref(), Some(LAPTOP));
        assert_eq!(screen_holding(&screens, Rect::new(-5000.0, 0.0, 10.0, 10.0)), None, "off every screen");
    }

    const GONE: &str = "external";
    const LAPTOP: &str = "laptop";

    /// The 11-18 shape: the external screen (main) had Desktops 1 and 4, Desktop 4 shown,
    /// its stages 5 and 6 hidden in Spaces 995 and 996, stage 3 in 993; the laptop had
    /// Desktop 471, its stage 17 hidden in 1004. The external screen is unplugged: macOS
    /// moves Desktops 1 and 4 to the laptop and shows 4.
    fn unplugged() -> Map {
        Map {
            stages: [((4, 5), 995), ((4, 6), 996), ((4, 3), 993), ((471, 17), 1004)].into(),
            screens: [(995, GONE.into()), (996, GONE.into()), (993, GONE.into()), (1004, LAPTOP.into())].into(),
            screens_now: [LAPTOP.to_string()].into(),
            desktops: [471, 1, 4].into(),
            shown: vec![4],
            ..Map::default()
        }
    }

    fn held(id: u32, space: u64, home: u64, now: &[u64]) -> HeldNow {
        HeldNow { id, space, home, now: now.to_vec() }
    }

    #[test]
    fn a_window_out_of_its_stage_space_has_spilled_one_on_no_space_has_not() {
        assert!(held(97, 996, 4, &[1]).spilled(), "on another Desktop");
        assert!(held(97, 996, 4, &[996, 1]).spilled(), "listed on a Desktop too");
        assert!(!held(97, 996, 4, &[996]).spilled());
        assert!(!held(97, 996, 4, &[]).spilled(), "minimized or ordered out");
    }

    #[test]
    fn a_gone_screens_stage_spaces_are_made_again_for_their_own_desktops() {
        // Stages 5 and 6 let out onto Desktop 1 (not theirs); stage 3 still hidden; the
        // laptop's own stage untouched.
        let held = [held(97, 996, 4, &[1]), held(124, 996, 4, &[1]), held(143, 995, 4, &[1]), held(151, 993, 4, &[993]), held(200, 1004, 471, &[1004])];
        let p = plan(&held, &unplugged());
        assert!(p.unkeyed.is_empty());
        let by_old: BTreeMap<u64, &Remake> = p.remakes.iter().map(|r| (r.old, r)).collect();
        assert_eq!(by_old.keys().copied().collect::<Vec<_>>(), vec![993, 995, 996], "every Space of the gone screen, none of the laptop's");
        let six = by_old[&996];
        assert_eq!((six.stage, six.from, six.to), (6, 4, 4), "back to Desktop 4, never Desktop 1 where they showed");
        assert_eq!(six.windows, vec![97, 124]);
        assert_eq!(six.spilled, vec![97, 124]);
        let three = by_old[&993];
        assert_eq!((three.to, three.spilled.len()), (4, 0), "still hidden: made again ahead of a spill");
        assert!(!six.rehomed());
    }

    #[test]
    fn a_window_let_out_on_a_screen_still_there_has_its_space_made_again() {
        let held = [held(200, 1004, 471, &[471]), held(201, 1004, 471, &[1004])];
        let p = plan(&held, &unplugged());
        assert_eq!(p.remakes.len(), 1);
        assert_eq!((p.remakes[0].to, p.remakes[0].windows.clone(), p.remakes[0].spilled.clone()), (471, vec![200, 201], vec![200]));
    }

    #[test]
    fn nothing_is_made_again_when_every_window_is_where_it_was_put() {
        let mut map = unplugged();
        map.screens_now.insert(GONE.into());
        let held = [held(97, 996, 4, &[996]), held(200, 1004, 471, &[1004]), held(201, 1004, 471, &[])];
        assert!(plan(&held, &map).is_empty());
    }

    #[test]
    fn a_window_whose_desktop_is_gone_too_is_kept_where_it_shows() {
        let mut map = unplugged();
        map.desktops.remove(&4);
        map.shown = vec![471];
        let p = plan(&[held(97, 996, 4, &[1])], &map);
        assert_eq!((p.remakes[0].from, p.remakes[0].to), (4, 1));
        assert!(p.remakes[0].rehomed());
        // Nothing let out and its Desktop gone: the Desktop shown.
        let p = plan(&[held(151, 993, 4, &[993])], &map);
        assert_eq!(p.remakes[0].to, 471);
    }

    #[test]
    fn a_window_let_out_of_a_space_with_no_key_is_left_to_the_model() {
        let p = plan(&[held(5, 777, 4, &[4])], &unplugged());
        assert_eq!((p.remakes.len(), p.unkeyed.clone()), (0, vec![5]));
    }

    /// The 11-18 live shape: a virtual screen (main) had Desktop 1, shown, its stages 3-5
    /// hidden in Spaces 1139-1141; the built-in screen had Desktop 1135 (made when the virtual
    /// screen took the main role), shown, its stage 6 hidden in 1142. The virtual screen goes:
    /// macOS moves Desktop 1 to the built-in screen, shows it, and ends Desktop 1135.
    fn main_gone() -> Map {
        Map {
            stages: [((1, 3), 1139), ((1, 4), 1140), ((1, 5), 1141), ((1135, 6), 1142)].into(),
            screens: [(1139, GONE.into()), (1140, GONE.into()), (1141, GONE.into()), (1142, LAPTOP.into())].into(),
            screens_now: [LAPTOP.to_string()].into(),
            desktops: [1].into(),
            shown: vec![1],
            screen_shown: [(LAPTOP.to_string(), 1)].into(),
            ended: [1135].into(),
            landed: BTreeMap::new(),
        }
    }

    #[test]
    fn an_ended_desktops_stage_space_on_the_screen_left_is_made_again_where_its_windows_went() {
        // Charlie 2 (16) still hidden in 1142, on no Desktop listed: its Space is made again
        // for Desktop 1, where the built-in screen's shown windows went.
        let list = [held(11, 1139, 1, &[1139]), held(16, 1142, 1135, &[1142])];
        let mut map = main_gone();
        map.landed = [(1135, 1)].into();
        let p = plan(&list, &map);
        let by_old: BTreeMap<u64, &Remake> = p.remakes.iter().map(|r| (r.old, r)).collect();
        let r = by_old[&1142];
        assert_eq!((r.stage, r.from, r.to, r.windows.clone()), (6, 1135, 1, vec![16]));
        assert!(r.rehomed() && r.spilled.is_empty());
        assert_eq!(by_old[&1139].to, 1, "the gone screen's own, on their own Desktop");
        // Nothing told of its shown windows: the Desktop its screen shows.
        let p = plan(&[held(16, 1142, 1135, &[1142])], &main_gone());
        assert_eq!(p.remakes[0].to, 1);
    }

    #[test]
    fn an_ended_desktop_joins_where_its_shown_windows_went_before_its_screens_desktop() {
        // Two screens left: the ended Desktop's shown windows went onto Desktop 7 of the
        // other screen; the Desktop its own screen shows (1) is not where its stages go.
        let mut map = main_gone();
        map.desktops = [1, 7].into();
        map.shown = vec![7, 1];
        map.screens_now.insert("other".into());
        map.screen_shown.insert("other".into(), 7);
        map.landed = [(1135, 7)].into();
        let p = plan(&[held(16, 1142, 1135, &[1142])], &map);
        assert_eq!(p.remakes[0].to, 7);
        // Landed on a Desktop gone already: not taken.
        map.landed = [(1135, 99)].into();
        let p = plan(&[held(16, 1142, 1135, &[1142])], &map);
        assert_eq!(p.remakes[0].to, 1, "the Desktop its screen shows");
    }

    #[test]
    fn a_desktop_unlisted_but_not_ended_yet_is_left_as_it_is() {
        let mut map = main_gone();
        map.ended.clear();
        assert!(plan(&[held(16, 1142, 1135, &[1142])], &map).is_empty());
    }

    #[test]
    fn a_desktop_ends_only_after_a_while_unlisted_with_the_display_list_still() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let one: Vec<(String, Vec<u64>)> = vec![(LAPTOP.into(), vec![1])];
        let (ended, u) = ended_desktops([1, 1135], &one, &Unlisted::default(), at(0));
        assert!(ended.is_empty(), "the look inside the display change");
        let (ended, u) = ended_desktops([1, 1135], &one, &u, at(500));
        assert!(ended.is_empty(), "two looks 500 ms apart are not enough");
        let (ended, u2) = ended_desktops([1, 1135], &one, &u, at(2000));
        assert!(ended.is_empty());
        let (ended, _) = ended_desktops([1, 1135], &one, &u2, at(2500));
        assert_eq!(ended, [1135].into(), "unlisted for the whole wait");
        // Listed again in between: the wait starts over.
        let both: Vec<(String, Vec<u64>)> = vec![(LAPTOP.into(), vec![1, 1135])];
        let (ended, u) = ended_desktops([1, 1135], &both, &u2, at(2200));
        assert!(ended.is_empty());
        let (ended, _) = ended_desktops([1, 1135], &one, &u, at(4000));
        assert!(ended.is_empty(), "unlisted again only since 4 s");
    }

    #[test]
    fn a_display_list_still_changing_holds_an_ended_desktop_back() {
        // A slow window server: the gone screen's Desktops come onto the screen left one at
        // a time. Desktop 4 is unlisted at first, then listed: never ended.
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let first: Vec<(String, Vec<u64>)> = vec![(LAPTOP.into(), vec![471])];
        let second: Vec<(String, Vec<u64>)> = vec![(LAPTOP.into(), vec![471, 1])];
        let (_, u) = ended_desktops([4, 1135], &first, &Unlisted::default(), at(0));
        let (ended, u) = ended_desktops([4, 1135], &second, &u, at(3000));
        assert!(ended.is_empty(), "the display list changed: every wait starts over");
        let (ended, _) = ended_desktops([4, 1135], &second, &u, at(5500));
        assert_eq!(ended, [4, 1135].into(), "still unlisted, the list unchanged for the wait");
    }

    #[test]
    fn an_ended_desktops_windows_land_where_most_of_them_are() {
        let desktops: BTreeSet<u64> = [1, 7].into();
        assert_eq!(landed_on(&[vec![7], vec![1], vec![1]], &desktops, &[7, 1]), Some(1));
        assert_eq!(landed_on(&[vec![7], vec![1]], &desktops, &[1]), Some(1), "a tie: the one shown");
        assert_eq!(landed_on(&[vec![1135], vec![]], &desktops, &[1]), None, "on no Desktop listed");
        assert_eq!(landed_on(&[], &desktops, &[1]), None);
    }

    #[test]
    fn a_hide_while_watching_takes_the_desktop_it_was_hidden_from() {
        let desktops: BTreeSet<u64> = [471, 1, 4].into();
        // Desktop 1 shows on a screen of its own (the main one shows 4).
        let h = HomeHints { on: Some(1), held: Some(4), last: None, stage: None, shown: Some(4), showing: Some(1), mid_change: false };
        assert_eq!(hide_home(h, &desktops, true), Some(4), "watching: its own Desktop");
        assert_eq!(hide_home(h, &desktops, false), Some(1), "as ever: where it shows");
        // Let go as it came out (no longer held): the Desktop it was last hidden from.
        let h = HomeHints { on: Some(1), held: None, last: Some(4), stage: Some(471), shown: Some(4), showing: Some(1), mid_change: false };
        assert_eq!(hide_home(h, &desktops, true), Some(4));
        // Shown since (nothing kept): where it shows, never a Desktop its stage left.
        let h = HomeHints { on: Some(1), held: None, last: None, stage: Some(471), shown: Some(4), showing: Some(1), mid_change: false };
        assert_eq!(hide_home(h, &desktops, true), Some(1));
        // On no Desktop and nothing kept: its stage's Desktop.
        let h = HomeHints { on: None, held: None, last: None, stage: Some(471), shown: Some(4), showing: None, mid_change: false };
        assert_eq!(hide_home(h, &desktops, true), Some(471));
        // Its Desktop gone: where it shows.
        let h = HomeHints { on: Some(1), held: Some(9), last: Some(9), stage: None, shown: Some(4), showing: Some(1), mid_change: false };
        assert_eq!(hide_home(h, &desktops, true), Some(1));
        // On no Desktop: as ever.
        let h = HomeHints { on: None, held: Some(9), last: None, stage: None, shown: Some(4), showing: None, mid_change: false };
        assert_eq!(hide_home(h, &desktops, true), Some(9));
    }

    /// The 11-19 live shape: a virtual screen (main) had Desktops 1, 4 and 471, 4 shown, its
    /// stages hidden on 4; the built-in screen had Desktop 1554 (made when the virtual screen
    /// took the main role), its stage 2 in front, never hidden. The virtual screen goes:
    /// macOS moves 1, 4 and 471 to the built-in screen, shows 4 and ends 1554; the window
    /// server lists stage 2's window on Desktop 1 as the model hides it (one stage in front
    /// per screen).
    #[test]
    fn a_window_in_front_hidden_as_its_desktop_ends_goes_to_the_desktop_its_screen_shows() {
        let listing = [(4, vec![1, 4, 471])];
        let desktops: BTreeSet<u64> = [1, 4, 471].into();
        let h = HomeHints { on: Some(1), held: None, last: None, stage: None, shown: Some(4), showing: showing_on(&listing, 1), mid_change: true };
        assert_eq!(hide_home(h, &desktops, true), Some(4), "the Desktop shown, where present_now counts it");
        assert_eq!(hide_home(h, &desktops, false), Some(1), "outside the watch: where it is listed, as ever");
        // Still listed on the ended Desktop (no screen lists it): the Desktop shown.
        let h = HomeHints { on: None, held: None, last: None, stage: None, shown: Some(4), showing: None, mid_change: true };
        assert_eq!(hide_home(h, &desktops, true), Some(4));
        // A window kept from an earlier hide keeps its own Desktop (the 11-18 rule).
        let h = HomeHints { on: Some(1), held: None, last: Some(471), stage: None, shown: Some(4), showing: Some(4), mid_change: true };
        assert_eq!(hide_home(h, &desktops, true), Some(471));
        let h = HomeHints { on: Some(1), held: Some(471), last: Some(471), stage: None, shown: Some(4), showing: Some(4), mid_change: true };
        assert_eq!(hide_home(h, &desktops, true), Some(471));
        // Two screens: the Desktop shown on the window's own screen, not the main one's.
        let listing = [(4, vec![1, 4, 471]), (9, vec![8, 9])];
        let desktops: BTreeSet<u64> = [1, 4, 471, 8, 9].into();
        let h = HomeHints { on: Some(8), held: None, last: None, stage: None, shown: Some(4), showing: showing_on(&listing, 8), mid_change: true };
        assert_eq!(hide_home(h, &desktops, true), Some(9));
    }

    /// The 11-21 shape: one screen at a time (a laptop closed on an external screen), Desktops
    /// 1 and 4 moved from the laptop to the external screen under the same ids, 1 shown. The
    /// watch begins as the laptop goes (t 0) and again at the unlock (6.5 s); every hidden
    /// window stays in its stage on its own Desktop. Then the user's Dock clicks, each
    /// arranging a Desktop not shown before macOS slides to it.
    #[test]
    fn a_dock_click_past_the_change_hides_the_arriving_desktops_front_on_its_own_desktop() {
        let s = Duration::from_secs_f64;
        let desktops: BTreeSet<u64> = [1, 4, 9].into();
        let listing = [(1, vec![1, 4, 9])];
        // 36 s: a Dock click (40 ms ago) to Terminal, whose window waits on Desktop 4: the
        // front window of 4 (an editor's, shown there since the laptop) goes to a card while 1
        // still shows. It is hidden on 4, its own, not on the Desktop shown.
        let watch = Some(s(36.0 - 6.5));
        let h = HomeHints {
            on: Some(4),
            held: None,
            last: None,
            stage: Some(4),
            shown: Some(1),
            showing: showing_on(&listing, 4),
            mid_change: mid_change(watch, s(0.04)),
        };
        assert!(!h.mid_change, "the user's click, 29 s into the watch");
        assert_eq!(hide_home(h, &desktops, true), Some(4));
        // 61 s: back to Desktop 1 by another Dock click, 4 showing now: Terminal's other
        // window, in front on 1, is hidden on 1.
        let listing = [(4, vec![1, 4, 9])];
        let h = HomeHints {
            on: Some(1),
            held: None,
            last: None,
            stage: Some(1),
            shown: Some(4),
            showing: showing_on(&listing, 1),
            mid_change: mid_change(Some(s(61.0 - 6.5)), s(0.02)),
        };
        assert_eq!(hide_home(h, &desktops, true), Some(1));
        // The same hide within the change, nothing pressed since: the 11-19 rule, the
        // Desktop shown.
        let h = HomeHints { mid_change: mid_change(Some(s(0.1)), s(5.0)), ..h };
        assert!(h.mid_change);
        assert_eq!(hide_home(h, &desktops, true), Some(4));
    }

    #[test]
    fn a_hide_is_the_changes_own_only_in_its_settle_with_nothing_pressed_since() {
        let s = Duration::from_secs_f64;
        assert!(mid_change(Some(s(0.1)), s(5.0)), "the model's own hide as the screen goes");
        assert!(mid_change(Some(s(9.0)), s(30.0)));
        assert!(!mid_change(Some(s(10.0)), s(30.0)), "past the settle");
        assert!(!mid_change(Some(s(3.0)), s(1.0)), "a click or key since: the user's");
        assert!(!mid_change(None, s(30.0)), "no watch");
    }

    #[test]
    fn the_desktop_shown_on_a_desktops_screen() {
        let listing = [(4, vec![1, 4, 471]), (0, vec![8, 9])];
        assert_eq!(showing_on(&listing, 1), Some(4));
        assert_eq!(showing_on(&listing, 4), Some(4), "itself when it shows");
        assert_eq!(showing_on(&listing, 8), None, "a screen that shows none");
        assert_eq!(showing_on(&listing, 1554), None, "listed by no screen");
    }
}
