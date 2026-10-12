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
    fn pane_urls_point_at_privacy() {
        assert!(PrivacyPane::Accessibility.url().ends_with("Privacy_Accessibility"));
        assert!(PrivacyPane::ScreenRecording.url().ends_with("Privacy_ScreenCapture"));
    }
}
