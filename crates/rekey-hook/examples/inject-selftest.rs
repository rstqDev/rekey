//! Drives the platform layer end to end by typing into whatever is focused.
//!
//! ```text
//! cargo run -p rekey-hook --example inject-selftest            # as Rekey
//! cargo run -p rekey-hook --example inject-selftest -- --as-user
//! ```
//!
//! Two modes, because they prove different things.
//!
//! The default types through [`MacInjector`], which stamps Rekey's signature
//! into every event. Rekey recognises those as its own and ignores them, so
//! nothing is corrected — but the event tap still fires, and the tap reads the
//! cached OS context on *every* event. That read is what once aborted the
//! process: Text Input Services is main-thread-only and kills the app with
//! SIGILL when touched from the tap thread. This mode proves the tap survives.
//!
//! `--as-user` posts the same keystrokes *without* the signature, so a running
//! Rekey treats them as genuine typing and corrects them. That proves the
//! whole pipeline — observe, detect, replace — actually works.
//!
//! Focus a scratch document first: the characters really are typed.

use core_graphics::event::{CGEvent, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use rekey_hook::platform::MacInjector;
use rekey_hook::Injector;
use std::time::Duration;

/// macOS virtual keycodes used to read the result back.
const VK_A: u16 = 0;
const VK_C: u16 = 8;
const VK_SPACE: u16 = 49;

/// Command-modifier mask on a CGEvent.
const CMD: u64 = 1 << 20;

/// Select all and copy, so the caller can read what landed via `pbpaste`.
///
/// Reading the target document is otherwise surprisingly hard: TextEdit's
/// AppleEvent save handler refuses, and anything AX-based needs its own
/// Accessibility grant.
fn copy_all() {
    use core_graphics::event::CGEventFlags;
    let source =
        CGEventSource::new(CGEventSourceStateID::HIDSystemState).expect("create event source");
    for key in [VK_A, VK_C] {
        for down in [true, false] {
            let event =
                CGEvent::new_keyboard_event(source.clone(), key, down).expect("keyboard event");
            event.set_flags(CGEventFlags::from_bits_truncate(CMD));
            event.post(CGEventTapLocation::HID);
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

/// Type `text` with no Rekey marker, so a running Rekey sees genuine input.
fn type_as_user(text: &str) {
    let source =
        CGEventSource::new(CGEventSourceStateID::HIDSystemState).expect("create event source");
    for ch in text.chars() {
        for down in [true, false] {
            // Space goes out on its real keycode, the way a keyboard sends it.
            let keycode = if ch == ' ' { VK_SPACE } else { 0 };
            let event =
                CGEvent::new_keyboard_event(source.clone(), keycode, down).expect("keyboard event");
            if ch != ' ' {
                event.set_string(&ch.to_string());
            }
            event.post(CGEventTapLocation::HID);
        }
        // Typing speed matters: Rekey evaluates a word when the space lands.
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn main() {
    let as_user = std::env::args().any(|a| a == "--as-user");

    println!(
        "typing in 3 seconds ({}) — focus a scratch document now",
        if as_user { "as a user" } else { "as Rekey" }
    );
    std::thread::sleep(Duration::from_secs(3));

    if as_user {
        type_as_user("ghbdtn ");
        // Give Rekey time to notice and replace.
        std::thread::sleep(Duration::from_millis(900));
        copy_all();
        std::thread::sleep(Duration::from_millis(300));
        println!("typed 'ghbdtn ' and copied the result to the clipboard");
    } else {
        let injector = MacInjector::new().expect("create injector");
        injector.type_text("ghbdtn ");
        std::thread::sleep(Duration::from_millis(400));
        injector.backspace(7);
        println!("typed and removed 7 characters; the tap saw them all");
    }
}
