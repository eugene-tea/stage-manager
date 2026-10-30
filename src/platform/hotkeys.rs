//! Global hotkeys through Carbon's `RegisterEventHotKey` (main thread only).
//!
//! Config key names ([`Key`]) map to US-layout virtual key codes and [`Mods`] to Carbon
//! modifier masks here. A hotkey press calls the sink installed with [`install`], which posts
//! a WM command and returns. [`register`] replaces every binding (config reload).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

use crate::config::{Hotkey, HotkeyCommand, Key, Mods};

type OsStatus = i32;
type EventRef = *mut c_void;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerUpp = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OsStatus;

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerUpp,
        num_types: u32,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OsStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OsStatus;
    fn UnregisterEventHotKey(hotkey: EventHotKeyRef) -> OsStatus;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OsStatus;
}

const fn four_cc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32
}

const EVENT_CLASS_KEYBOARD: u32 = four_cc(b"keyb");
const EVENT_HOTKEY_PRESSED: u32 = 5;
const PARAM_DIRECT_OBJECT: u32 = four_cc(b"----");
const TYPE_HOTKEY_ID: u32 = four_cc(b"hkid");
const SIGNATURE: u32 = four_cc(b"StMg");

/// Carbon modifier masks.
const CMD_KEY: u32 = 0x0100;
const SHIFT_KEY: u32 = 0x0200;
const OPTION_KEY: u32 = 0x0800;
const CONTROL_KEY: u32 = 0x1000;

/// Carbon's modifier mask for `mods`.
pub fn carbon_mods(mods: Mods) -> u32 {
    let mut m = 0;
    if mods.cmd {
        m |= CMD_KEY;
    }
    if mods.shift {
        m |= SHIFT_KEY;
    }
    if mods.alt {
        m |= OPTION_KEY;
    }
    if mods.ctrl {
        m |= CONTROL_KEY;
    }
    m
}

/// The US-layout virtual key code (`kVK_*`) of a key.
pub fn key_code(key: Key) -> Option<u32> {
    const LETTERS: [u32; 26] = [
        0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2E, // a–m
        0x2D, 0x1F, 0x23, 0x0C, 0x0F, 0x01, 0x11, 0x20, 0x09, 0x0D, 0x07, 0x10, 0x06, // n–z
    ];
    const DIGITS: [u32; 10] = [0x1D, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19];
    const F_KEYS: [u32; 20] = [
        0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F, 0x69, 0x6B, 0x71, 0x6A, 0x40, 0x4F,
        0x50, 0x5A,
    ];
    Some(match key {
        Key::Letter(c) if c.is_ascii_lowercase() => LETTERS[(c as u8 - b'a') as usize],
        Key::Letter(_) => return None,
        Key::Digit(d) => *DIGITS.get(usize::from(d))?,
        Key::F(n) => *F_KEYS.get(usize::from(n).checked_sub(1)?)?,
        Key::Space => 0x31,
        Key::Return => 0x24,
        Key::Tab => 0x30,
        Key::Escape => 0x35,
        Key::Delete => 0x33,
        Key::ForwardDelete => 0x75,
        Key::Left => 0x7B,
        Key::Right => 0x7C,
        Key::Up => 0x7E,
        Key::Down => 0x7D,
        Key::Home => 0x73,
        Key::End => 0x77,
        Key::PageUp => 0x74,
        Key::PageDown => 0x79,
        Key::Minus => 0x1B,
        Key::Equal => 0x18,
        Key::LeftBracket => 0x21,
        Key::RightBracket => 0x1E,
        Key::Backslash => 0x2A,
        Key::Semicolon => 0x29,
        Key::Quote => 0x27,
        Key::Comma => 0x2B,
        Key::Period => 0x2F,
        Key::Slash => 0x2C,
        Key::Grave => 0x32,
    })
}

type Sink = Box<dyn Fn(HotkeyCommand)>;

#[derive(Default)]
struct State {
    installed: bool,
    sink: Option<Sink>,
    refs: Vec<EventHotKeyRef>,
    commands: HashMap<u32, HotkeyCommand>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

extern "C" fn on_hotkey(_call: EventHandlerCallRef, event: EventRef, _data: *mut c_void) -> OsStatus {
    let mut id = EventHotKeyId::default();
    // SAFETY: `id` is a valid buffer of the requested size; the event is live for the
    // duration of the callback.
    let status = unsafe {
        GetEventParameter(
            event,
            PARAM_DIRECT_OBJECT,
            TYPE_HOTKEY_ID,
            std::ptr::null_mut(),
            std::mem::size_of::<EventHotKeyId>(),
            std::ptr::null_mut(),
            (&mut id as *mut EventHotKeyId).cast(),
        )
    };
    if status == 0 && id.signature == SIGNATURE {
        // Nothing unwinds into Carbon: a panic is caught here and logged.
        let _ = super::safety::guarded("the hotkey handler", || dispatch(id.id));
    }
    0
}

/// Sends the command bound to hotkey `id` to the sink. A panic is caught where it happens
/// ([`super::safety::guarded`]; a bare `catch_unwind` here was dead, the review's K7: the
/// panic hook unparked everything and quit first) and the sink stays for the next hotkey.
fn dispatch(id: u32) {
    let command = STATE.with(|s| s.borrow().commands.get(&id).copied());
    let Some(command) = command else { return };
    tracing::debug!(?command, "hotkey");
    // Take the sink out while it runs, so a re-entrant call cannot double-borrow.
    let sink = STATE.with(|s| s.borrow_mut().sink.take());
    if let Some(sink) = sink {
        let _ = super::safety::guarded("a hotkey's command", || sink(command));
        STATE.with(|s| {
            s.borrow_mut().sink.get_or_insert(sink);
        });
    }
}

/// Installs the Carbon event handler and the sink that receives hotkey commands (main
/// thread, once).
pub fn install(sink: impl Fn(HotkeyCommand) + 'static) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.sink = Some(Box::new(sink));
        if s.installed {
            return;
        }
        let spec = EventTypeSpec { event_class: EVENT_CLASS_KEYBOARD, event_kind: EVENT_HOTKEY_PRESSED };
        let mut handler: EventHandlerRef = std::ptr::null_mut();
        // SAFETY: valid target, a handler with the Carbon signature, one spec, out pointer.
        let status = unsafe {
            InstallEventHandler(GetApplicationEventTarget(), on_hotkey, 1, &spec, std::ptr::null_mut(), &mut handler)
        };
        if status == 0 {
            s.installed = true;
        } else {
            tracing::error!(status, "cannot install the hotkey handler");
        }
    });
}

/// What [`register`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Registered {
    /// Bindings now active.
    pub active: usize,
    /// Bindings macOS refused (taken by another app, or an unmappable key), as text.
    pub failed: Vec<String>,
}

/// Replaces every registered hotkey with `bindings` (main thread).
pub fn register(bindings: &[(Hotkey, HotkeyCommand)]) -> Registered {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        for r in s.refs.drain(..) {
            // SAFETY: each ref came from RegisterEventHotKey and is unregistered once.
            unsafe { UnregisterEventHotKey(r) };
        }
        s.commands.clear();
        let mut out = Registered::default();
        for (i, (hotkey, command)) in bindings.iter().enumerate() {
            let Some(code) = key_code(hotkey.key) else {
                out.failed.push(hotkey.to_string());
                continue;
            };
            let id = EventHotKeyId { signature: SIGNATURE, id: i as u32 + 1 };
            let mut r: EventHotKeyRef = std::ptr::null_mut();
            // SAFETY: plain values and a valid out pointer; the target is the application.
            let status =
                unsafe { RegisterEventHotKey(code, carbon_mods(hotkey.mods), id, GetApplicationEventTarget(), 0, &mut r) };
            if status == 0 && !r.is_null() {
                s.refs.push(r);
                s.commands.insert(id.id, *command);
                out.active += 1;
            } else {
                tracing::warn!(status, hotkey = %hotkey, "hotkey not registered (taken by another app?)");
                out.failed.push(hotkey.to_string());
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotkeyConfig;

    /// K7: a command that panics is caught where it runs, and the next hotkey still reaches
    /// the sink.
    #[test]
    fn a_panicking_hotkey_command_keeps_the_hotkeys() {
        use std::rc::Rc;
        let seen = Rc::new(std::cell::Cell::new(0));
        let count = seen.clone();
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.commands.insert(1, HotkeyCommand::NewStage);
            s.sink = Some(Box::new(move |_| {
                count.set(count.get() + 1);
                assert!(count.get() != 1, "the first command panics");
            }));
        });
        dispatch(1);
        dispatch(1);
        assert_eq!(seen.get(), 2, "the sink stayed after the panic");
        assert!(STATE.with(|s| s.borrow().sink.is_some()));
    }

    #[test]
    fn modifiers_map_to_carbon_masks() {
        let m = Mods { ctrl: true, alt: true, shift: false, cmd: false };
        assert_eq!(carbon_mods(m), CONTROL_KEY | OPTION_KEY);
        assert_eq!(carbon_mods(Mods { shift: true, cmd: true, ..Mods::default() }), SHIFT_KEY | CMD_KEY);
        assert_eq!(carbon_mods(Mods::default()), 0);
    }

    #[test]
    fn keys_map_to_us_virtual_codes() {
        assert_eq!(key_code(Key::Letter('a')), Some(0x00));
        assert_eq!(key_code(Key::Letter('n')), Some(0x2D));
        assert_eq!(key_code(Key::Letter('p')), Some(0x23));
        assert_eq!(key_code(Key::Letter('s')), Some(0x01));
        assert_eq!(key_code(Key::Letter('z')), Some(0x06));
        assert_eq!(key_code(Key::Digit(1)), Some(0x12));
        assert_eq!(key_code(Key::Digit(5)), Some(0x17));
        assert_eq!(key_code(Key::Digit(9)), Some(0x19));
        assert_eq!(key_code(Key::Digit(0)), Some(0x1D));
        assert_eq!(key_code(Key::F(1)), Some(0x7A));
        assert_eq!(key_code(Key::F(20)), Some(0x5A));
        assert_eq!(key_code(Key::F(0)), None);
        assert_eq!(key_code(Key::Letter('Z')), None);
    }

    #[test]
    fn every_default_binding_maps() {
        let b = HotkeyConfig::default().all_bindings();
        assert_eq!(b.len(), 21);
        assert!(b.iter().all(|(h, _)| key_code(h.key).is_some()));
        let mut codes: Vec<(u32, u32)> = b.iter().map(|(h, _)| (key_code(h.key).unwrap_or(0), carbon_mods(h.mods))).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 21, "no two defaults collide");
    }

    #[test]
    fn four_char_codes() {
        assert_eq!(EVENT_CLASS_KEYBOARD, 0x6B65_7962);
        assert_eq!(TYPE_HOTKEY_ID, 0x686B_6964);
    }
}
