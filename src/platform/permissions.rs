//! Accessibility and Screen Recording permission checks and requests.
//!
//! `STAGE_MANAGER_NO_PROMPT` (any value) suppresses every system dialog at launch, so
//! automated runs never pop a prompt on the owner's screen.

use objc2_application_services::{AXIsProcessTrusted, AXIsProcessTrustedWithOptions, kAXTrustedCheckOptionPrompt};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFString};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

/// The two TCC grants the app needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PermissionState {
    /// Accessibility: needed to observe and move windows.
    pub accessibility: bool,
    /// Screen Recording: needed for strip thumbnails.
    pub screen_recording: bool,
}

impl PermissionState {
    /// Reads both grants without prompting.
    pub fn current() -> Self {
        Self { accessibility: accessibility_trusted(), screen_recording: screen_recording_granted() }
    }

    /// One line for the menu: what is granted and what is missing.
    pub fn summary(&self) -> String {
        match (self.accessibility, self.screen_recording) {
            (true, true) => "Permissions granted".to_string(),
            (false, false) => "Missing: Accessibility, Screen Recording".to_string(),
            (false, true) => "Missing: Accessibility".to_string(),
            (true, false) => "Missing: Screen Recording".to_string(),
        }
    }
}

/// One approval (TCC grant) the app asks for. The audit (10-13): Accessibility is required
/// (observing and moving windows; the input monitor's key events and the test-only posted
/// events ride on it too); Screen Recording is optional (window pictures for cards and
/// switches; without it cards draw plain window shapes with the app icon); Input Monitoring
/// is not needed (no keyboard event tap and no HID access: mouse events come from an
/// `NSEvent` global monitor, which needs no grant, key-downs from the same monitor under
/// Accessibility, hotkeys from Carbon's `RegisterEventHotKey`, which needs none, and the one
/// event tap, which makes parked windows' slivers ignore presses, filters mouse events only,
/// under Accessibility).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approval {
    Accessibility,
    ScreenRecording,
}

impl Approval {
    /// Both, in the order they are shown.
    pub const ALL: [Approval; 2] = [Approval::Accessibility, Approval::ScreenRecording];

    /// The name System Settings uses.
    pub fn name(self) -> &'static str {
        match self {
            Self::Accessibility => "Accessibility",
            Self::ScreenRecording => "Screen Recording",
        }
    }

    /// What it is for, one sentence (the welcome window's row).
    pub fn why(self) -> &'static str {
        match self {
            Self::Accessibility => "Lets Stage Manager move, hide and bring back your windows.",
            Self::ScreenRecording => "Shows live pictures of your windows on the cards and in switches.",
        }
    }

    /// What happens without it.
    pub fn without(self) -> &'static str {
        match self {
            Self::Accessibility => "Stage Manager cannot arrange windows; it waits until access is allowed.",
            Self::ScreenRecording => "Cards show plain window shapes with the app's icon.",
        }
    }

    /// Whether the app cannot work without it.
    pub fn required(self) -> bool {
        self == Self::Accessibility
    }

    /// The menu's disabled line while it is missing.
    pub fn needs_line(self) -> String {
        format!("Stage Manager needs {} access", self.name())
    }

    /// The menu item and button that ask for it.
    pub fn allow_title(self) -> String {
        format!("Allow {}…", self.name())
    }

    /// An SF Symbol for its row.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Accessibility => "accessibility",
            Self::ScreenRecording => "rectangle.dashed.badge.record",
        }
    }

    /// Its System Settings pane.
    pub fn pane(self) -> PrivacyPane {
        match self {
            Self::Accessibility => PrivacyPane::Accessibility,
            Self::ScreenRecording => PrivacyPane::ScreenRecording,
        }
    }

    /// Whether `state` has it.
    pub fn granted(self, state: &PermissionState) -> bool {
        match self {
            Self::Accessibility => state.accessibility,
            Self::ScreenRecording => state.screen_recording,
        }
    }
}

/// The approvals to ask for: those missing, plus Screen Recording when macOS refuses
/// captures. Screen Recording counts only while window pictures are on (`[strip] capture`).
pub fn missing(state: &PermissionState, capture_on: bool, capture_refused: bool) -> Vec<Approval> {
    Approval::ALL
        .into_iter()
        .filter(|a| match a {
            Approval::Accessibility => !state.accessibility,
            Approval::ScreenRecording => capture_on && (!state.screen_recording || capture_refused),
        })
        .collect()
}

/// Which System Settings privacy pane to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivacyPane {
    Accessibility,
    ScreenRecording,
}

impl PrivacyPane {
    /// The `x-apple.systempreferences:` URL of the pane.
    pub fn url(self) -> &'static str {
        match self {
            Self::Accessibility => "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            Self::ScreenRecording => "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
        }
    }
}

/// True unless `STAGE_MANAGER_NO_PROMPT` is set.
pub fn prompts_allowed() -> bool {
    std::env::var_os("STAGE_MANAGER_NO_PROMPT").is_none()
}

/// Whether this process is a trusted Accessibility client (no prompt).
pub fn accessibility_trusted() -> bool {
    // SAFETY: plain query without arguments.
    unsafe { AXIsProcessTrusted() }
}

/// Checks Accessibility trust, asking macOS to show its prompt (which also lists the app in
/// the Accessibility pane) when `prompt` is true and the process is not trusted yet.
pub fn check_accessibility(prompt: bool) -> bool {
    // SAFETY: the key is the framework's own constant string.
    let key: &CFString = unsafe { kAXTrustedCheckOptionPrompt };
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[CFBoolean::new(prompt)]);
    // SAFETY: `options` is a valid CFDictionary with the documented key and a CFBoolean.
    unsafe { AXIsProcessTrustedWithOptions(Some(options.as_ref())) }
}

/// Whether Screen Recording is granted (no prompt).
pub fn screen_recording_granted() -> bool {
    CGPreflightScreenCaptureAccess()
}

/// Asks for Screen Recording; the first call shows the system dialog and lists the app in
/// the Screen Recording pane.
pub fn request_screen_recording() -> bool {
    CGRequestScreenCaptureAccess()
}

/// Launch-time check: prompts for whatever is missing unless prompts are suppressed.
pub fn check_at_launch() -> PermissionState {
    let prompt = prompts_allowed();
    let accessibility = check_accessibility(prompt);
    let mut screen_recording = screen_recording_granted();
    if !screen_recording && prompt {
        screen_recording = request_screen_recording();
    }
    PermissionState { accessibility, screen_recording }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_names_what_is_missing() {
        let s = |a, r| PermissionState { accessibility: a, screen_recording: r }.summary();
        assert_eq!(s(true, true), "Permissions granted");
        assert_eq!(s(false, true), "Missing: Accessibility");
        assert_eq!(s(true, false), "Missing: Screen Recording");
        assert!(s(false, false).contains("Accessibility, Screen Recording"));
    }

    #[test]
    fn missing_approvals_follow_the_grants_and_the_capture_setting() {
        let p = |a, r| PermissionState { accessibility: a, screen_recording: r };
        assert_eq!(missing(&p(true, true), true, false), vec![]);
        assert_eq!(missing(&p(false, true), true, false), vec![Approval::Accessibility]);
        assert_eq!(missing(&p(false, false), true, false), Approval::ALL.to_vec());
        assert_eq!(missing(&p(true, false), false, false), vec![], "no window pictures, no Screen Recording");
        assert_eq!(missing(&p(true, true), true, true), vec![Approval::ScreenRecording], "captures refused");
        assert!(Approval::Accessibility.required() && !Approval::ScreenRecording.required());
        assert_eq!(Approval::ScreenRecording.needs_line(), "Stage Manager needs Screen Recording access");
        assert_eq!(Approval::Accessibility.allow_title(), "Allow Accessibility…");
    }

    #[test]
    fn pane_urls_point_at_privacy() {
        assert!(PrivacyPane::Accessibility.url().ends_with("Privacy_Accessibility"));
        assert!(PrivacyPane::ScreenRecording.url().ends_with("Privacy_ScreenCapture"));
    }
}
