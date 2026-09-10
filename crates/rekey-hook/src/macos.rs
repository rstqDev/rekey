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
use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation::base::{CFRelease, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::sys::CGEventRef;
use foreign_types::ForeignType;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
    /// Re-arms an event tap that the system has switched off.
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);

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
    fn TISCreateInputSourceList(properties: *const c_void, include_all: bool) -> CFArrayRef;
    fn TISSelectInputSource(source: *mut c_void) -> i32;
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
        AXIsProcessTrustedWithOptions(options.as_CFTypeRef())
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
///
/// # Must be called on the main thread
///
/// Text Input Services asserts the dispatch queue internally. Calling this
/// from any other thread — the event tap callback in particular — aborts the
/// process with SIGILL inside `dispatch_assert_queue`, on the very first
/// keystroke. Sample it on the main thread and share the result.
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

/// Switch the system keyboard to the layout Rekey just corrected into.
///
/// Correcting `ghbdtn` to `привет` without this leaves the user still on the
/// English layout, so the very next word they type is wrong again. Fixing the
/// text but not the layout solves half the problem.
///
/// # Must be called on the main thread
///
/// Same Text Input Services constraint as [`current_layout`].
///
/// Returns true when the layout was found among the user's enabled input
/// sources and selected. A layout the user has not enabled is not an error:
/// Rekey simply leaves the keyboard alone.
pub fn select_layout(layout_id: &str) -> bool {
    unsafe {
        // A null filter returns the input sources the user has actually
        // enabled, which is what we want: never switch to a layout they have
        // not installed.
        let list = TISCreateInputSourceList(std::ptr::null(), false);
        if list.is_null() {
            return false;
        }
        let mut selected = false;
        let count = CFArrayGetCount(list);
        for i in 0..count {
            let source = CFArrayGetValueAtIndex(list, i) as *mut c_void;
            if source.is_null() {
                continue;
            }
            let id_ref = TISGetInputSourceProperty(source, kTISPropertyInputSourceID);
            if id_ref.is_null() {
                continue;
            }
            let id = CFString::wrap_under_get_rule(id_ref as CFStringRef).to_string();
            if layout_from_input_source(&id) == Some(layout_id) {
                selected = TISSelectInputSource(source) == 0;
                break;
            }
        }
        CFRelease(list as *const c_void);
        selected
    }
}

/// Bundle identifier of the frontmost application.
///
/// # Must be called on the main thread
///
/// `NSWorkspace` is AppKit, with the same main-thread requirement as
/// [`current_layout`].
pub fn frontmost_app() -> Option<String> {
    use objc2_app_kit::NSWorkspace;
    let workspace = NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    app.bundleIdentifier().map(|id| id.to_string())
}

/// True when a password field has taken over keyboard input.
pub fn secure_input_active() -> bool {
    unsafe { IsSecureEventInputEnabled() }
}

/// Sample everything the engine needs to know about the current moment.
///
/// # Must be called on the main thread
///
/// See [`current_layout`]. The app samples this on the main thread on a timer
/// and publishes the result for the hook thread to read.
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
        // `Private` deliberately, not `HIDSystemState`. A HID-state source
        // reflects the modifier keys physically held right now, so a
        // correction fired while the user still has Option down would inherit
        // that flag — and since every injected event is built on keycode 0,
        // which is the `a` key, an inherited modifier can override the Unicode
        // string and type a stray `a` instead of the correction.
        let source = CGEventSource::new(CGEventSourceStateID::Private)
            .map_err(|_| HookError::Os("cannot create a CGEventSource".into()))?;
        Ok(MacInjector { source })
    }

    fn post(&self, event: CGEvent) {
        // Belt and braces with the private source above: whatever the user is
        // holding, Rekey's own keystrokes carry no modifiers.
        event.set_flags(CGEventFlags::empty());
        // The signature must be stamped *after* the flags. Setting flags
        // clears the event's user-data field, and without the signature Rekey
        // stops recognising its own keystrokes — so it reacts to its own
        // corrections and types over them.
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
        std::thread::sleep(KEYSTROKE_GAP);
    }
}

/// Pause between injected keystrokes.
///
/// Synthetic events posted back to back are not always all delivered: the
/// receiving application can drop or coalesce them, which shows up as a
/// replacement that deleted one character too few and left a stray letter
/// behind. A real keyboard never produces keystrokes this close together, and
/// a few milliseconds each is imperceptible against the 25ms Rekey already
/// waits before replacing.
const KEYSTROKE_GAP: Duration = Duration::from_millis(4);

/// Virtual keycodes macOS uses for the keys the layout tables do not cover.
const VK_DELETE: u16 = 51;
const VK_SPACE: u16 = 49;
const VK_TAB: u16 = 48;
const VK_RETURN: u16 = 36;

/// macOS virtual key codes for each of the 47 printable keys, in the same
/// physical order `rekey_core::layout` uses.
///
/// These are positions, not characters: key code 0 is the key labelled `a` on
/// a US keyboard, `ф` on a Russian one. That is exactly why replaying them
/// works — the active layout decides what they produce.
const KEYCODES: [u16; rekey_core::layout::KEY_COUNT] = [
    // ` 1 2 3 4 5 6 7 8 9 0 - =
    50, 18, 19, 20, 21, 23, 22, 26, 28, 25, 29, 27, 24, // q w e r t y u i o p [ ] \
    12, 13, 14, 15, 17, 16, 32, 34, 31, 35, 33, 30, 42, // a s d f g h j k l ; '
    0, 1, 2, 3, 5, 4, 38, 40, 37, 41, 39, // z x c v b n m , . /
    6, 7, 8, 9, 11, 45, 46, 43, 47, 44,
];

impl MacInjector {
    /// Post one physical key press, with shift if the layout needs it.
    fn tap_key_with_shift(&self, keycode: u16, shift: bool) {
        for down in [true, false] {
            let Ok(event) = CGEvent::new_keyboard_event(self.source.clone(), keycode, down) else {
                return;
            };
            event.set_flags(if shift {
                CGEventFlags::CGEventFlagShift
            } else {
                CGEventFlags::empty()
            });
            // After the flags, never before: see `post`.
            event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, REKEY_SIGNATURE);
            event.post(CGEventTapLocation::HID);
        }
        std::thread::sleep(KEYSTROKE_GAP);
    }
}

impl Injector for MacInjector {
    fn backspace(&self, count: usize) {
        for _ in 0..count {
            self.tap_key(VK_DELETE);
        }
    }

    fn type_keys(&self, presses: &[rekey_core::layout::KeyPress]) -> bool {
        use rekey_core::layout::KeyPress;
        for press in presses {
            let (keycode, shift) = match *press {
                KeyPress::Key { index, shift } => match KEYCODES.get(index) {
                    Some(&code) => (code, shift),
                    None => return false,
                },
                KeyPress::Space => (VK_SPACE, false),
                KeyPress::Tab => (VK_TAB, false),
                KeyPress::Enter => (VK_RETURN, false),
            };
            self.tap_key_with_shift(keycode, shift);
        }
        true
    }

    fn type_text(&self, text: &str) {
        // One event per character, because a CGEvent *is* one keystroke.
        //
        // Attaching a whole string to a single event relies on the receiving
        // app inserting `[NSEvent characters]` verbatim. Some do; TextEdit
        // does not, and takes only the first character — so a correction to
        // "привет" arrived as a lone "п". Worse, an app that re-derives the
        // character from the keycode sees keycode 0, which is the `a` key, and
        // types `a` on a US layout or `ф` on a Russian one.
        //
        // Sending each character on its own event is what a keyboard does, and
        // every app handles it.
        let mut encoded = String::with_capacity(4);
        for ch in text.chars() {
            encoded.clear();
            encoded.push(ch);
            for down in [true, false] {
                let Ok(event) = CGEvent::new_keyboard_event(self.source.clone(), 0, down) else {
                    return;
                };
                event.set_string(&encoded);
                self.post(event);
            }
            std::thread::sleep(KEYSTROKE_GAP);
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
        // Modifier keys: command, shift, caps lock, option, control, fn.
        //
        // These must be named explicitly. They normally arrive as flags
        // changes, but when one does arrive as a key event its reported
        // character is a space — so the whitespace fallback below would read a
        // press of Option or Shift as the end of a word, ending it early and
        // disarming the manual shortcut a moment before it is released.
        54..=63 => Key::ModifiersChanged,
        // Otherwise go by what the key produced — but never infer a word
        // boundary from the character alone.
        //
        // Whitespace is identified by key code above, because that is what a
        // keyboard sends. Modifier and other non-typing events can report a
        // space as their character, and reading those as a boundary ends the
        // word the user is in the middle of typing. Anything that claims to be
        // whitespace without the matching key is treated as "something else
        // happened", which is the safe reading.
        _ => match character {
            Some(c) if c.is_whitespace() => Key::Other,
            Some(c) if !c.is_control() => Key::Character,
            _ => Key::Other,
        },
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

    // The tap's own port, so the callback can switch it back on. It is filled
    // in immediately after creation; until then the callback cannot fire.
    let tap_port: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let callback_port = tap_port.clone();

    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        // Listen-only: the user's keystroke is never delayed or swallowed.
        CGEventTapOptions::ListenOnly,
        vec![CGEventType::KeyDown, CGEventType::FlagsChanged],
        move |_proxy, event_type, event| {
            // macOS switches a tap off if its callback is ever too slow, or if
            // the user's own input trips the watchdog, and it stays off until
            // explicitly re-armed. Without this Rekey goes quietly deaf
            // part-way through a session, and every symptom after that looks
            // like a detection bug instead of a dead hook.
            if matches!(
                event_type,
                CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
            ) {
                let port = callback_port.load(Ordering::Relaxed);
                if port != 0 {
                    log::warn!("keyboard tap was disabled by the system; re-arming");
                    unsafe { CGEventTapEnable(port as *mut c_void, true) };
                }
                return CallbackResult::Keep;
            }

            let synthetic = event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
                == REKEY_SIGNATURE;

            let key_event = match event_type {
                CGEventType::KeyDown => {
                    let keycode =
                        event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
                    let character = event_character(event);
                    Some(KeyEvent {
                        character,
                        key: classify(keycode, character),
                        modifiers: modifiers_of(event),
                        synthetic,
                    })
                }
                // A modifier on its own arrives as a flags change, never as a
                // key press. Tapping one is how the manual "cycle the last
                // word" shortcut is triggered, so the engine has to see these.
                CGEventType::FlagsChanged => Some(KeyEvent {
                    character: None,
                    key: Key::ModifiersChanged,
                    modifiers: modifiers_of(event),
                    synthetic,
                }),
                _ => None,
            };

            if let Some(key_event) = key_event {
                if let Ok(mut handler) = on_key.lock() {
                    handler(key_event);
                }
            }

            // Listen-only: every keystroke continues to its destination
            // untouched. Rekey corrects afterwards, it never intercepts.
            CallbackResult::Keep
        },
    )
    .map_err(|_| HookError::PermissionDenied)?;

    tap_port.store(
        tap.mach_port().as_concrete_TypeRef() as usize,
        Ordering::Relaxed,
    );

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
        assert_eq!(
            layout_from_input_source("com.apple.inputmethod.Kotoeri"),
            None
        );
        assert_eq!(layout_from_input_source("com.apple.keylayout.Dvorak"), None);
    }

    #[test]
    fn keycodes_match_the_layout_table_positions() {
        // The key codes are physical positions. Typing "привет" on Russian must
        // come out as the same keys that spell "ghbdtn" on US: g,h,b,d,t,n.
        use rekey_core::layout::KeyPress;
        let presses = rekey_core::layout::presses_for("привет ", "ru").unwrap();
        let codes: Vec<u16> = presses
            .iter()
            .map(|p| match *p {
                KeyPress::Key { index, .. } => KEYCODES[index],
                KeyPress::Space => VK_SPACE,
                KeyPress::Tab => VK_TAB,
                KeyPress::Enter => VK_RETURN,
            })
            .collect();
        assert_eq!(
            codes,
            vec![5, 4, 11, 2, 17, 45, VK_SPACE],
            "g h b d t n, then the space that ended the word"
        );
    }

    #[test]
    fn every_keycode_is_distinct() {
        // A duplicate would silently type the wrong character for one key.
        let mut seen = KEYCODES.to_vec();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), before, "duplicate key code in the table");
    }

    #[test]
    fn classifies_editing_keys() {
        assert_eq!(classify(51, None), Key::Backspace);
        assert_eq!(classify(49, Some(' ')), Key::Space);
        assert_eq!(classify(123, None), Key::Navigation);
        assert_eq!(classify(0, Some('a')), Key::Character);
    }

    #[test]
    fn modifier_keys_are_never_mistaken_for_whitespace() {
        // macOS reports a space as the character for modifier key events, so
        // without an explicit case these end the word the user is typing.
        for keycode in 54..=63 {
            assert_eq!(
                classify(keycode, Some(' ')),
                Key::ModifiersChanged,
                "key code {keycode} should be a modifier, not a space"
            );
        }
    }

    #[test]
    fn whitespace_is_recognised_by_key_not_by_character() {
        // Keyboards send whitespace on its own key code …
        assert_eq!(classify(49, Some(' ')), Key::Space);
        assert_eq!(classify(48, Some('\t')), Key::Tab);
        assert_eq!(classify(36, Some('\r')), Key::Enter);

        // … and anything else claiming to be a space is not a word boundary.
        // Modifier and other non-typing events report a space as their
        // character, and treating those as boundaries ends the word the user
        // is still typing — and disarms the manual shortcut a moment before
        // it fires.
        assert_eq!(classify(0, Some(' ')), Key::Other);
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
