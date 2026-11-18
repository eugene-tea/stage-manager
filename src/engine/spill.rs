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
//! the Desktop it shows on (else the one shown): its stage still stands, on that Desktop.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

/// How long after a screen went (or after the unlock, when it went while the screen was
/// locked) hidden windows are kept on their own Desktops ([`plan`], [`hide_home`]): the
/// window server let windows out of stage Spaces as late as the first look at a moved
/// Desktop (a log, 11-18: 20 s after the change).
pub const WATCH: Duration = Duration::from_secs(60);

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
        if spilled.is_empty() && !map.screen_gone(space) {
            continue;
        }
        let Some(&(from, stage)) = key_of.get(&space) else {
            out.unkeyed.extend(spilled);
            continue;
        };
        let surfaced = members.iter().find(|h| h.spilled()).and_then(|h| desktop_of(&h.now, &map.desktops, &map.shown));
        let Some(to) = Some(from).filter(|d| map.desktops.contains(d)).or(surfaced).or(map.shown.first().copied()) else {
            out.unkeyed.extend(spilled);
            continue;
        };
        out.remakes.push(Remake { old: space, stage, from, to, windows: members.iter().map(|h| h.id).collect(), spilled });
    }
    out
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
    /// The Desktop shown.
    pub shown: Option<u64>,
}

/// The Desktop a hide takes as the window's home. Outside the [`WATCH`] (`keep` false), the
/// Desktop it shows on, else the one it was held on, else the one shown (as ever). While
/// watching, the Desktop it was hidden from comes first while that Desktop exists (held,
/// else last), then the one it shows on, then its stage's: the Desktop the window server put
/// it on as its screen went is not its own. A window shown since it was last hidden has no
/// last Desktop (a show forgets it), so it is hidden from where it shows, as ever.
pub fn hide_home(h: HomeHints, desktops: &BTreeSet<u64>, keep: bool) -> Option<u64> {
    if !keep {
        return h.on.or(h.held).or(h.shown);
    }
    let exists = |d: &u64| desktops.contains(d);
    h.held.filter(exists).or(h.last.filter(exists)).or(h.on).or(h.stage.filter(exists)).or(h.held).or(h.shown)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_hide_while_watching_takes_the_desktop_it_was_hidden_from() {
        let desktops: BTreeSet<u64> = [471, 1, 4].into();
        let h = HomeHints { on: Some(1), held: Some(4), last: None, stage: None, shown: Some(4) };
        assert_eq!(hide_home(h, &desktops, true), Some(4), "watching: its own Desktop");
        assert_eq!(hide_home(h, &desktops, false), Some(1), "as ever: where it shows");
        // Let go as it came out (no longer held): the Desktop it was last hidden from.
        let h = HomeHints { on: Some(1), held: None, last: Some(4), stage: Some(471), shown: Some(4) };
        assert_eq!(hide_home(h, &desktops, true), Some(4));
        // Shown since (nothing kept): where it shows, never a Desktop its stage left.
        let h = HomeHints { on: Some(1), held: None, last: None, stage: Some(471), shown: Some(4) };
        assert_eq!(hide_home(h, &desktops, true), Some(1));
        // On no Desktop and nothing kept: its stage's Desktop.
        let h = HomeHints { on: None, held: None, last: None, stage: Some(471), shown: Some(4) };
        assert_eq!(hide_home(h, &desktops, true), Some(471));
        // Its Desktop gone: where it shows.
        let h = HomeHints { on: Some(1), held: Some(9), last: Some(9), stage: None, shown: Some(4) };
        assert_eq!(hide_home(h, &desktops, true), Some(1));
        // On no Desktop: as ever.
        let h = HomeHints { on: None, held: Some(9), last: None, stage: None, shown: Some(4) };
        assert_eq!(hide_home(h, &desktops, true), Some(9));
    }
}
