//! macOS keyboard observation via `CGEventTap`, and injection via synthetic
//! `CGEvent`s.
//!
//! Rekey installs a *listen-only* tap: keystrokes are observed and passed
//! straight through, never swallowed. Corrections are applied afterwards as
//! ordinary synthetic backspaces and text, which is why it works in every app
//! without integrating with any of them.
//!
//! Three OS signals matter for not misbehaving:
//!
//! * **Accessibility** must be granted or the tap cannot be created at all.
//! * **Secure input** is on whenever the focused field is a password field.
//!   Injecting there is both futile and alarming, so Rekey stands down.
//! * **The active input source** tells us which layout the user is typing on,
//!   which is the other half of every decision the detector makes.

use crate::{Context, HookError, Injector, Key, KeyEvent, Modifiers};
use core_foundation::base::{CFRelease, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{
    CallbackResult, CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventType, EventField,
};
use core_graphics::sys::CGEventRef;
use foreign_types::ForeignType;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use std::ffi::c_void;

/// Marker written into the `USER_DATA` field of every event Rekey synthesises,
/// so the tap can recognise its own typing and ignore it. Without this the app
/// would react to its own corrections and loop.
const REKEY_SIGNATURE: i64 = 0x52_454B_4559; // "REKEY"

// -- Carbon / ApplicationServices bindings ----------------------------------

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    /// Reads the characters a key event resolves to on the active layout.
    /// `core-graphics` exposes the setter but not the getter.
    fn CGEventKeyboardGetUnicodeString(
        event: CGEventRef,
        max_length: usize,
        actual_length: *mut usize,
        buffer: *mut u16,
    );
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, key: CFStringRef) -> *mut c_void;
    static kTISPropertyInputSourceID: CFStringRef;
    fn IsSecureEventInputEnabled() -> bool;
}

// -- permission -------------------------------------------------------------

/// True when the user has granted Accessibility access.
pub fn has_permission() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Ask macOS to show the Accessibility prompt. Returns the current state; the
/// user granting access happens asynchronously in System Settings, and the app
/// must be relaunched afterwards for the tap to succeed.
pub fn request_permission() -> bool {
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let value = core_foundation::boolean::CFBoolean::true_value();
        let options = CFDictionary::from_CFType_pairs(&[(key, value)]);
        AXIsProcessTrustedWithOptions(options.as_CFTypeRef() as *const c_void)
    }
}

// -- context ----------------------------------------------------------------

/// macOS input source identifiers mapped to Rekey layout ids.
///
/// Matched as a suffix of the full identifier, so regional variants such as
/// `com.apple.keylayout.RussianWin` resolve correctly.
const INPUT_SOURCES: &[(&str, &str)] = &[
    ("US", "us"),
    ("USExtended", "us"),
    ("ABC", "us"),
    ("British", "us"),
    ("Russian", "ru"),
    ("RussianWin", "ru"),
    ("Russian-Phonetic", "ru"),
    ("Ukrainian", "uk"),
    ("Ukrainian-PC", "uk"),
    ("Hebrew", "he"),
    ("Hebrew-PC", "he"),
    ("Arabic", "ar"),
    ("Arabic-AZERTY", "ar"),
    ("Greek", "el"),
    ("GreekPolytonic", "el"),
    ("German", "de"),
    ("German-DIN-2137", "de"),
    ("French", "fr"),
    ("French-numerical", "fr"),
    ("Spanish", "es"),
    ("Spanish-ISO", "es"),
    ("Turkish", "tr"),
    ("Turkish-QWERTY-PC", "tr"),
];

/// Map a macOS input source id such as `com.apple.keylayout.Russian` to a
/// Rekey layout id.
pub fn layout_from_input_source(id: &str) -> Option<&'static str> {
    let tail = id.rsplit('.').next()?;
    // Longest match first, so "Ukrainian-PC" beats "Ukrainian".
    INPUT_SOURCES
        .iter()
        .filter(|(name, _)| *name == tail)
        .map(|(_, layout)| *layout)
        .next()
        .or_else(|| {
            INPUT_SOURCES
                .iter()
                .filter(|(name, _)| tail.starts_with(name))
                .max_by_key(|(name, _)| name.len())
                .map(|(_, layout)| *layout)
        })
}

/// The Rekey layout id for the keyboard the user is currently typing on.
pub fn current_layout() -> Option<String> {
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return None;
        }
        let id_ref = TISGetInputSourceProperty(source, kTISPropertyInputSourceID);
        let result = if id_ref.is_null() {
            None
        } else {
            let id = CFString::wrap_under_get_rule(id_ref as CFStringRef).to_string();
            layout_from_input_source(&id).map(|s| s.to_string())
        };
        CFRelease(source as *const c_void);
        result
    }
}

/// Bundle identifier of the frontmost application.
pub fn frontmost_app() -> Option<String> {
    use objc2_app_kit::NSWorkspace;
    unsafe {
        let workspace = NSWorkspace::sharedWorkspace();
        let app = workspace.frontmostApplication()?;
        app.bundleIdentifier().map(|id| id.to_string())
    }
}

/// True when a password field has taken over keyboard input.
pub fn secure_input_active() -> bool {
    unsafe { IsSecureEventInputEnabled() }
}

pub fn current_context() -> Context {
    Context {
        app: frontmost_app(),
        layout: current_layout(),
        secure_input: secure_input_active(),
    }
}

// -- injection --------------------------------------------------------------

pub struct MacInjector {
    source: CGEventSource,
}

// CGEventSource is a Core Foundation object; it is safe to use from the single
// thread that owns the injector, and Rekey only injects from the hook thread.
unsafe impl Send for MacInjector {}
unsafe impl Sync for MacInjector {}

impl MacInjector {
    pub fn new() -> Result<MacInjector, HookError> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| HookError::Os("cannot create a CGEventSource".into()))?;
        Ok(MacInjector { source })
    }

    fn post(&self, event: CGEvent) {
        event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, REKEY_SIGNATURE);
        event.post(CGEventTapLocation::HID);
    }

    /// Send one key down/up pair for a raw virtual keycode.
    fn tap_key(&self, keycode: u16) {
        for down in [true, false] {
            if let Ok(event) = CGEvent::new_keyboard_event(self.source.clone(), keycode, down) {
                self.post(event);
            }
        }
    }
}

/// Virtual keycode for Delete (backspace) on macOS.
const VK_DELETE: u16 = 51;

impl Injector for MacInjector {
    fn backspace(&self, count: usize) {
        for _ in 0..count {
            self.tap_key(VK_DELETE);
        }
    }

    fn type_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        // Typing the whole string on a single synthetic event keeps the
        // replacement atomic from the receiving app's point of view, and avoids
        // per-character races in fast editors.
        for down in [true, false] {
            let Ok(event) = CGEvent::new_keyboard_event(self.source.clone(), 0, down) else {
                return;
            };
            event.set_string(text);
            self.post(event);
        }
    }
}

// -- the tap ----------------------------------------------------------------

/// Read the character a key event produced, as macOS resolved it for the
/// active layout.
///
/// Only the first character is used: a single keystroke that yields several
/// (an emoji picker insertion, a composed sequence) is not a mistyped word and
/// the buffer is better off ignoring it.
fn event_character(event: &CGEvent) -> Option<char> {
    const MAX: usize = 8;
    let mut buffer = [0u16; MAX];
    let mut actual: usize = 0;
    unsafe {
        CGEventKeyboardGetUnicodeString(
            event.as_ptr(),
            MAX,
            &mut actual as *mut usize,
            buffer.as_mut_ptr(),
        );
    }
    if actual == 0 {
        return None;
    }
    String::from_utf16_lossy(&buffer[..actual.min(MAX)])
        .chars()
        .next()
}

fn modifiers_of(event: &CGEvent) -> Modifiers {
    let flags = event.get_flags();
    Modifiers {
        shift: flags.contains(CGEventFlags::CGEventFlagShift),
        control: flags.contains(CGEventFlags::CGEventFlagControl),
        alt: flags.contains(CGEventFlags::CGEventFlagAlternate),
        meta: flags.contains(CGEventFlags::CGEventFlagCommand),
    }
}

/// macOS virtual keycodes Rekey needs to recognise by identity rather than by
/// the character they produce.
fn classify(keycode: u16, character: Option<char>) -> Key {
    match keycode {
        51 => Key::Backspace,
        49 => Key::Space,
        36 | 76 => Key::Enter,
        48 => Key::Tab,
        53 => Key::Escape,
        // Arrows, and the Home/End/PageUp/PageDown cluster.
        123..=126 | 115 | 116 | 119 | 121 => Key::Navigation,
        _ => {
            if character.is_some_and(|c| !c.is_control()) {
                Key::Character
            } else {
                Key::Other
            }
        }
    }
}

/// Install the tap and run the event loop on the calling thread.
///
/// This never returns while the hook is running, so callers put it on a
/// dedicated thread.
pub fn run<F>(on_key: F) -> Result<(), HookError>
where
    F: FnMut(KeyEvent) + Send + 'static,
{
    if !has_permission() {
        return Err(HookError::PermissionDenied);
    }

    // The tap callback is `Fn`, but the handler needs to mutate the typing
    // buffer. The tap only ever fires on this one thread, so the lock is
    // uncontended and exists purely to satisfy the signature.
    let on_key = std::sync::Mutex::new(on_key);

    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        // Listen-only: the user's keystroke is never delayed or swallowed.
        CGEventTapOptions::ListenOnly,
        vec![CGEventType::KeyDown, CGEventType::FlagsChanged],
        move |_proxy, event_type, event| {
            if matches!(event_type, CGEventType::KeyDown) {
                let synthetic = event
                    .get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
                    == REKEY_SIGNATURE;
                let keycode =
                    event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
                let character = event_character(event);
                if let Ok(mut handler) = on_key.lock() {
                    handler(KeyEvent {
                        character,
                        key: classify(keycode, character),
                        modifiers: modifiers_of(event),
                        synthetic,
                    });
                }
            }
            // Listen-only: every keystroke continues to its destination
            // untouched. Rekey corrects afterwards, it never intercepts.
            CallbackResult::Keep
        },
    )
    .map_err(|_| HookError::PermissionDenied)?;

    let loop_source = tap
        .mach_port()
        .create_runloop_source(0)
        .map_err(|_| HookError::Os("cannot create a run loop source".into()))?;

    let run_loop = CFRunLoop::get_current();
    unsafe {
        run_loop.add_source(&loop_source, kCFRunLoopCommonModes);
    }
    tap.enable();
    CFRunLoop::run_current();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_apple_input_sources_to_layouts() {
        assert_eq!(
            layout_from_input_source("com.apple.keylayout.US"),
            Some("us")
        );
        assert_eq!(
            layout_from_input_source("com.apple.keylayout.Russian"),
            Some("ru")
        );
        assert_eq!(
            layout_from_input_source("com.apple.keylayout.RussianWin"),
            Some("ru")
        );
        assert_eq!(
            layout_from_input_source("com.apple.keylayout.Hebrew-PC"),
            Some("he")
        );
        assert_eq!(
            layout_from_input_source("com.apple.keylayout.Ukrainian-PC"),
            Some("uk")
        );
    }

    #[test]
    fn unknown_input_sources_are_not_guessed() {
        // Better to do nothing than to assume a layout we do not model.
        assert_eq!(layout_from_input_source("com.apple.inputmethod.Kotoeri"), None);
        assert_eq!(layout_from_input_source("com.apple.keylayout.Dvorak"), None);
    }

    #[test]
    fn classifies_editing_keys() {
        assert_eq!(classify(51, None), Key::Backspace);
        assert_eq!(classify(49, Some(' ')), Key::Space);
        assert_eq!(classify(123, None), Key::Navigation);
        assert_eq!(classify(0, Some('a')), Key::Character);
    }

    #[test]
    fn shortcuts_are_distinguished_from_typing() {
        assert!(Modifiers {
            meta: true,
            ..Default::default()
        }
        .is_shortcut());
        // AltGr and Shift produce ordinary characters on many layouts.
        assert!(!Modifiers {
            alt: true,
            shift: true,
            ..Default::default()
        }
        .is_shortcut());
    }
}
