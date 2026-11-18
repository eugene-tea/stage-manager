//! The mode picker of Settings → Advanced (docs/MODES.md): the three ways of keeping stages,
//! Accessibility · Spaces · With Apple's, as one choice. Pure: what the picker shows for the
//! facts in force ([`ModeFacts::view`]), the words under it ([`ModeView::line`]), and what a
//! choice writes ([`ModeFacts::writes`]); the window draws it, the shell writes.
//!
//! The facts are the config's `[windows] engine` and `apple_stage_manager_on`, Apple's
//! `GloballyEnabled`, and the engine the launch chose (it changes only at a relaunch, or once
//! when Spaces falls back). Apple's Stage Manager on with `"integrate"` is With Apple's; on
//! with `"pause"` nothing is selected (the app is paused); off, the config's engine, with a
//! note when the engine running is another one (a relaunch pending, or Spaces fell back).
//!
//! A choice is the user's ask to change macOS's setting (the app follows macOS settings and
//! changes them only when asked): With Apple's writes `"integrate"` and turns Apple's on;
//! Accessibility and Spaces write `engine` and turn Apple's off when it is on, keeping
//! `apple_stage_manager_on` as it was.

use crate::engine::engine_choice::ConfigEngine;
use crate::engine::recovery::EngineKind;

/// One way of keeping stages, in the picker's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `engine = "accessibility"`, Apple's off.
    Accessibility,
    /// `engine = "spaces"`, Apple's off.
    Spaces,
    /// Apple's on, `apple_stage_manager_on = "integrate"`.
    WithApple,
}

impl Mode {
    /// Every mode, in the picker's order.
    pub const ALL: [Mode; 3] = [Mode::Accessibility, Mode::Spaces, Mode::WithApple];

    /// Its segment's label.
    pub fn title(self) -> &'static str {
        match self {
            Mode::Accessibility => "Accessibility",
            Mode::Spaces => "Spaces",
            Mode::WithApple => "With Apple's",
        }
    }

    /// Its segment.
    pub fn index(self) -> usize {
        Mode::ALL.iter().position(|m| *m == self).unwrap_or(0)
    }

    /// The mode of segment `index` (`None`: no segment, or out of range).
    pub fn of_index(index: isize) -> Option<Mode> {
        usize::try_from(index).ok().and_then(|i| Mode::ALL.get(i).copied())
    }

    /// The mode of a config's engine.
    pub fn of_engine(engine: ConfigEngine) -> Mode {
        match engine {
            ConfigEngine::Accessibility => Mode::Accessibility,
            ConfigEngine::Spaces => Mode::Spaces,
        }
    }

    /// What it is, in a line or two under the picker (docs/MODES.md's figures, 11-14).
    pub fn words(self) -> &'static str {
        match self {
            Mode::Accessibility => "Hidden windows wait in a corner of the screen. Works on every Mac; about 5 % of a CPU core at rest.",
            Mode::Spaces => "Hidden windows leave the screen; about a quarter of the CPU at rest. Undocumented macOS calls, tested at each launch; Accessibility takes over if they fail.",
            Mode::WithApple => "Apple's Stage Manager arranges the windows, with the stage shortcuts and restore on top. The pointer moves for a moment when a window changes stage.",
        }
    }
}

/// The engine a launch runs ([`crate::platform::window_engine::choice`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Running {
    /// The Corner (Accessibility) or Spaces engine.
    pub kind: EngineKind,
    /// The launch asked for Spaces and runs the corner: its probe failed, or a check while
    /// running fell back.
    pub fell_back: bool,
}

/// What decides the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModeFacts {
    /// Apple's Stage Manager is on (`GloballyEnabled`).
    pub apple_on: bool,
    /// `apple_stage_manager_on = "integrate"`.
    pub follows_apple: bool,
    /// `[windows] engine` in the config in force.
    pub engine: ConfigEngine,
    /// The engine the launch runs (`None`: not known, a preview; taken as the config's).
    pub running: Option<Running>,
}

/// What follows the words under the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    /// Nothing: the mode chosen runs.
    None,
    /// Apple's Stage Manager is on and the app is paused (`"pause"`).
    Paused,
    /// The engine chosen is not the one running: it applies after a relaunch.
    Relaunch,
    /// Spaces is chosen and the corner runs: Spaces failed a check in this run.
    FellBack,
}

/// What the picker shows: the segment selected (`None`: none) and the note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeView {
    pub selected: Option<Mode>,
    pub note: Note,
}

/// What a choice writes ([`ModeFacts::writes`]); `None` for what stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModeWrites {
    /// `[windows] engine`.
    pub engine: Option<ConfigEngine>,
    /// `[windows] apple_stage_manager_on = "integrate"`.
    pub integrate: bool,
    /// Apple's Stage Manager on or off (`GloballyEnabled`).
    pub apple: Option<bool>,
}

impl ModeFacts {
    /// The picker for these facts (module docs).
    pub fn view(&self) -> ModeView {
        if self.apple_on {
            return if self.follows_apple {
                ModeView { selected: Some(Mode::WithApple), note: Note::None }
            } else {
                ModeView { selected: None, note: Note::Paused }
            };
        }
        let note = match self.running {
            None => Note::None,
            Some(r) => match (self.engine, r.kind) {
                (ConfigEngine::Accessibility, EngineKind::Corner) | (ConfigEngine::Spaces, EngineKind::Spaces) => Note::None,
                (ConfigEngine::Spaces, EngineKind::Corner) if r.fell_back => Note::FellBack,
                _ => Note::Relaunch,
            },
        };
        ModeView { selected: Some(Mode::of_engine(self.engine)), note }
    }

    /// What choosing `mode` writes: only what differs from these facts.
    pub fn writes(&self, mode: Mode) -> ModeWrites {
        match mode {
            Mode::WithApple => ModeWrites { engine: None, integrate: !self.follows_apple, apple: (!self.apple_on).then_some(true) },
            Mode::Accessibility | Mode::Spaces => {
                let engine = if mode == Mode::Spaces { ConfigEngine::Spaces } else { ConfigEngine::Accessibility };
                ModeWrites { engine: (self.engine != engine).then_some(engine), integrate: false, apple: self.apple_on.then_some(false) }
            }
        }
    }

    /// The facts once choosing `mode` has taken: what the picker shows while macOS has not yet
    /// read back Apple's Stage Manager going on or off ([`ModeFacts::held`]).
    pub fn after(mut self, mode: Mode) -> ModeFacts {
        let w = self.writes(mode);
        if let Some(e) = w.engine {
            self.engine = e;
        }
        self.follows_apple |= w.integrate;
        if let Some(on) = w.apple {
            self.apple_on = on;
        }
        self
    }

    /// The facts the picker shows for a choice `asked` a moment ago (`None`: none, or it is
    /// old): as if it had taken, until it has. Apple's Stage Manager going on or off reads back
    /// a second or two after the click, and the selection would jump back meanwhile.
    pub fn held(self, asked: Option<Mode>) -> ModeFacts {
        match asked {
            Some(mode) if self.view().selected != Some(mode) => self.after(mode),
            _ => self,
        }
    }
}

impl ModeView {
    /// The words under the picker: the selected mode's, then the note.
    pub fn line(&self) -> String {
        let note = match self.note {
            Note::None => None,
            Note::Paused => Some("Paused while Apple's Stage Manager is on. Accessibility or Spaces turns it off; With Apple's follows it."),
            Note::Relaunch => Some("Applies after a relaunch."),
            Note::FellBack => Some("Accessibility runs instead: Spaces failed a check in this run. A relaunch tries it again."),
        };
        match (self.selected, note) {
            (Some(m), Some(n)) => format!("{} {n}", m.words()),
            (Some(m), None) => m.words().to_string(),
            (None, Some(n)) => n.to_string(),
            (None, None) => String::new(),
        }
    }

    /// Whether the Relaunch button shows (the engine chosen is not the one running).
    pub fn relaunch(&self) -> bool {
        matches!(self.note, Note::Relaunch | Note::FellBack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORNER: Option<Running> = Some(Running { kind: EngineKind::Corner, fell_back: false });
    const SPACES: Option<Running> = Some(Running { kind: EngineKind::Spaces, fell_back: false });
    const FELL_BACK: Option<Running> = Some(Running { kind: EngineKind::Corner, fell_back: true });

    fn facts(apple_on: bool, follows_apple: bool, engine: ConfigEngine, running: Option<Running>) -> ModeFacts {
        ModeFacts { apple_on, follows_apple, engine, running }
    }

    fn view(apple_on: bool, follows_apple: bool, engine: ConfigEngine, running: Option<Running>) -> (Option<Mode>, Note) {
        let v = facts(apple_on, follows_apple, engine, running).view();
        (v.selected, v.note)
    }

    use ConfigEngine::{Accessibility as Acc, Spaces as Sp};

    #[test]
    fn the_segments_in_order_with_their_names() {
        let titles: Vec<_> = Mode::ALL.iter().map(|m| m.title()).collect();
        assert_eq!(titles, ["Accessibility", "Spaces", "With Apple's"]);
        for m in Mode::ALL {
            assert_eq!(Mode::of_index(m.index() as isize), Some(m));
        }
        assert_eq!(Mode::of_index(-1), None, "no segment selected");
        assert_eq!(Mode::of_index(3), None);
    }

    #[test]
    fn apples_on_with_integrate_is_with_apples_and_with_pause_selects_nothing() {
        for engine in [Acc, Sp] {
            for running in [None, CORNER, SPACES, FELL_BACK] {
                assert_eq!(view(true, true, engine, running), (Some(Mode::WithApple), Note::None));
                assert_eq!(view(true, false, engine, running), (None, Note::Paused));
            }
        }
        let paused = facts(true, false, Acc, CORNER).view();
        assert!(paused.line().starts_with("Paused while Apple's Stage Manager is on."));
        assert!(!paused.relaunch());
    }

    #[test]
    fn apples_off_shows_the_config_engine_and_says_when_another_runs() {
        assert_eq!(view(false, false, Acc, CORNER), (Some(Mode::Accessibility), Note::None));
        assert_eq!(view(false, true, Acc, CORNER), (Some(Mode::Accessibility), Note::None), "integrate matters only while Apple's is on");
        assert_eq!(view(false, false, Sp, SPACES), (Some(Mode::Spaces), Note::None));
        assert_eq!(view(false, false, Sp, FELL_BACK), (Some(Mode::Spaces), Note::FellBack), "the probe failed, or a check fell back");
        assert_eq!(view(false, false, Sp, CORNER), (Some(Mode::Spaces), Note::Relaunch), "chosen since the launch");
        assert_eq!(view(false, false, Acc, SPACES), (Some(Mode::Accessibility), Note::Relaunch));
        assert_eq!(view(false, false, Acc, FELL_BACK), (Some(Mode::Accessibility), Note::None), "the corner runs, as chosen");
        assert_eq!(view(false, false, Sp, None), (Some(Mode::Spaces), Note::None), "a preview: taken as running");
    }

    #[test]
    fn the_line_is_the_modes_words_then_the_note() {
        let v = facts(false, false, Sp, CORNER).view();
        assert_eq!(v.line(), format!("{} Applies after a relaunch.", Mode::Spaces.words()));
        assert!(v.relaunch());
        let v = facts(false, false, Sp, FELL_BACK).view();
        assert!(v.line().contains("Accessibility runs instead") && v.relaunch(), "a relaunch tries Spaces again");
        let v = facts(false, false, Acc, CORNER).view();
        assert_eq!(v.line(), Mode::Accessibility.words());
        assert!(!v.relaunch());
        assert!(Mode::Accessibility.words().contains("5 %"), "MODES.md: about 4.9 % at rest");
        assert!(Mode::Spaces.words().contains("a quarter"), "MODES.md: about 1.2 % against 4.9 %");
        for m in Mode::ALL {
            let w = m.words().to_lowercase();
            assert!(!w.contains("you") && !w.contains(" we "), "{m:?}: no you or we");
        }
    }

    #[test]
    fn with_apples_writes_integrate_and_turns_apples_on() {
        assert_eq!(facts(false, false, Acc, CORNER).writes(Mode::WithApple), ModeWrites { engine: None, integrate: true, apple: Some(true) });
        assert_eq!(facts(true, false, Sp, SPACES).writes(Mode::WithApple), ModeWrites { engine: None, integrate: true, apple: None }, "paused: integrate only");
        assert_eq!(facts(true, true, Acc, CORNER).writes(Mode::WithApple), ModeWrites::default(), "already so");
    }

    #[test]
    fn an_engine_writes_engine_and_turns_apples_off_keeping_integrate() {
        let w = facts(true, true, Acc, CORNER).writes(Mode::Spaces);
        assert_eq!(w, ModeWrites { engine: Some(Sp), integrate: false, apple: Some(false) });
        let w = facts(true, true, Acc, CORNER).writes(Mode::Accessibility);
        assert_eq!(w, ModeWrites { engine: None, integrate: false, apple: Some(false) }, "the engine as it was");
        assert_eq!(facts(true, false, Sp, SPACES).writes(Mode::Accessibility), ModeWrites { engine: Some(Acc), integrate: false, apple: Some(false) });
        assert_eq!(facts(false, false, Acc, CORNER).writes(Mode::Spaces), ModeWrites { engine: Some(Sp), integrate: false, apple: None });
        assert_eq!(facts(false, false, Sp, SPACES).writes(Mode::Spaces), ModeWrites::default(), "already so");
        let after = facts(true, true, Acc, CORNER).after(Mode::Spaces);
        assert!(after.follows_apple, "apple_stage_manager_on keeps what it was");
    }

    #[test]
    fn every_choice_shows_itself_once_it_has_taken() {
        for apple_on in [false, true] {
            for follows in [false, true] {
                for engine in [Acc, Sp] {
                    for running in [None, CORNER, SPACES, FELL_BACK] {
                        let f = facts(apple_on, follows, engine, running);
                        for m in Mode::ALL {
                            assert_eq!(f.after(m).view().selected, Some(m), "{f:?} then {m:?}");
                            assert_eq!(f.after(m).writes(m), ModeWrites::default(), "nothing left to write");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_choice_is_held_until_macos_reads_it_back() {
        let shown = facts(true, true, Acc, CORNER);
        // Accessibility clicked: Apple's still reads on for a second.
        assert_eq!(shown.held(Some(Mode::Accessibility)).view().selected, Some(Mode::Accessibility));
        assert_eq!(shown.held(None), shown, "nothing asked, or the ask is old");
        let taken = facts(false, true, Acc, CORNER);
        assert_eq!(taken.held(Some(Mode::Accessibility)), taken, "taken: the facts as they are");
        // With Apple's clicked: the config says integrate, Apple's not on yet.
        let half = facts(false, true, Acc, CORNER);
        assert_eq!(half.held(Some(Mode::WithApple)).view(), ModeView { selected: Some(Mode::WithApple), note: Note::None });
    }
}
