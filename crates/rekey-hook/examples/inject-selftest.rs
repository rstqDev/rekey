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
//!
//! macOS only: it drives Core Graphics directly. On other platforms it builds
//! to a stub so that `cargo clippy --all-targets` stays clean everywhere.

#[cfg(target_os = "macos")]
mod imp {
    use core_graphics::event::{CGEvent, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use rekey_hook::platform::MacInjector;
    use rekey_hook::Injector;
    use std::time::Duration;

    /// macOS virtual keycodes used to read the result back.
    const VK_A: u16 = 0;
    const VK_C: u16 = 8;
    const VK_SPACE: u16 = 49;

    /// US-layout virtual keycodes, so the harness can type the way hardware does:
    /// a keycode with no attached string, which macOS resolves through whatever
    /// layout is active. Injecting a Unicode string instead bypasses that
    /// resolution entirely and is therefore a weaker test.
    fn us_keycode(ch: char) -> Option<u16> {
        Some(match ch.to_ascii_lowercase() {
            'a' => 0,
            's' => 1,
            'd' => 2,
            'f' => 3,
            'h' => 4,
            'g' => 5,
            'z' => 6,
            'x' => 7,
            'c' => 8,
            'v' => 9,
            'b' => 11,
            'q' => 12,
            'w' => 13,
            'e' => 14,
            'r' => 15,
            'y' => 16,
            't' => 17,
            'o' => 31,
            'u' => 32,
            'i' => 34,
            'p' => 35,
            'l' => 37,
            'j' => 38,
            'k' => 40,
            'n' => 45,
            'm' => 46,
            '[' => 33,
            ']' => 30,
            ';' => 41,
            '\'' => 39,
            ',' => 43,
            '.' => 47,
            '/' => 44,
            '\\' => 42,
            '`' => 50,
            '-' => 27,
            '=' => 24,
            ' ' => VK_SPACE,
            _ => return None,
        })
    }

    /// Command-modifier mask on a CGEvent.
    const CMD: u64 = 1 << 20;

    /// Tap the left Option key: press and release with nothing in between.
    ///
    /// This is the manual "cycle the last word" shortcut, so the harness needs to
    /// be able to produce it.
    fn tap_option() {
        const VK_LEFT_OPTION: u16 = 58;
        use core_graphics::event::CGEventFlags;
        let source =
            CGEventSource::new(CGEventSourceStateID::HIDSystemState).expect("create event source");
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(source.clone(), VK_LEFT_OPTION, down)
                .expect("keyboard event");
            // macOS reports a modifier as a flags change carrying the new state.
            event.set_type(core_graphics::event::CGEventType::FlagsChanged);
            event.set_flags(if down {
                CGEventFlags::CGEventFlagAlternate
            } else {
                CGEventFlags::empty()
            });
            event.post(CGEventTapLocation::HID);
            std::thread::sleep(Duration::from_millis(60));
        }
    }

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
    fn type_as_user(text: &str, delay_ms: u64) {
        let source =
            CGEventSource::new(CGEventSourceStateID::HIDSystemState).expect("create event source");
        for ch in text.chars() {
            for down in [true, false] {
                // Prefer a real keycode with no attached string: that is what a
                // keyboard sends, and it exercises macOS's layout resolution the
                // same way. Fall back to a Unicode string for anything unmapped.
                let event = match us_keycode(ch) {
                    Some(code) => CGEvent::new_keyboard_event(source.clone(), code, down)
                        .expect("keyboard event"),
                    None => {
                        let e = CGEvent::new_keyboard_event(source.clone(), 0, down)
                            .expect("keyboard event");
                        e.set_string(&ch.to_string());
                        e
                    }
                };
                event.post(CGEventTapLocation::HID);
            }
            // Typing speed matters: Rekey evaluates a word when the space lands,
            // and a fast typist starts the next word while that is happening.
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().collect();
        let as_user = args.iter().any(|a| a == "--as-user");
        let phrase = args
            .iter()
            .position(|a| a == "--text")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| "ghbdtn ".to_string());
        let delay_ms: u64 = args
            .iter()
            .position(|a| a == "--delay")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(40);

        println!(
            "typing in 3 seconds ({}) — focus a scratch document now",
            if as_user { "as a user" } else { "as Rekey" }
        );
        std::thread::sleep(Duration::from_secs(3));

        // Tests need a known starting layout, or a leftover switch from an earlier
        // run silently changes what the injected keycodes mean.
        if let Some(target) = args
            .iter()
            .position(|a| a == "--set-layout")
            .and_then(|i| args.get(i + 1))
        {
            let requested = rekey_hook::platform::select_layout(target);
            // TISSelectInputSource is asynchronous. A short-lived process can exit
            // before the change lands, leaving the next test typing on the wrong
            // layout and silently measuring nothing, so wait for confirmation.
            let mut settled = false;
            for _ in 0..40 {
                if rekey_hook::platform::current_layout().as_deref() == Some(target.as_str()) {
                    settled = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            println!("set layout {target} -> requested={requested} settled={settled}");
            if !settled {
                std::process::exit(1);
            }
            return;
        }

        // Varies the two remaining differences between the working matrix and the
        // real injector: the user-data stamp, and whether the text is ASCII or in
        // the script of the active layout.
        if args.iter().any(|a| a == "--matrix2") {
            use core_graphics::event::EventField;
            const REKEY_SIGNATURE: i64 = 0x52_454B_4559;
            std::thread::sleep(Duration::from_secs(3));
            for (label, stamp, text) in [
                ("A", false, "[ascii-nostamp]"),
                ("B", true, "[ascii-stamp]"),
                ("C", false, "[кириллица-nostamp]"),
                ("D", true, "[кириллица-stamp]"),
            ] {
                let source = CGEventSource::new(CGEventSourceStateID::Private).expect("source");
                for down in [true, false] {
                    let e = CGEvent::new_keyboard_event(source.clone(), 0, down)
                        .expect("keyboard event");
                    e.set_string(text);
                    if stamp {
                        e.set_integer_value_field(
                            EventField::EVENT_SOURCE_USER_DATA,
                            REKEY_SIGNATURE,
                        );
                    }
                    e.post(CGEventTapLocation::HID);
                }
                let _ = label;
                std::thread::sleep(Duration::from_millis(250));
            }
            std::thread::sleep(Duration::from_millis(400));
            copy_all();
            std::thread::sleep(Duration::from_millis(300));
            println!("matrix2 done; result on the clipboard");
            return;
        }

        // Isolates which part of the injector setup breaks set_string. Every
        // injected event is built on keycode 0, so when the Unicode string is
        // ignored exactly one character arrives: whatever keycode 0 means on the
        // active layout.
        if args.iter().any(|a| a == "--matrix") {
            use core_graphics::event::CGEventFlags;
            std::thread::sleep(Duration::from_secs(3));
            for (label, state, clear_flags) in [
                ("hid+keepflags", CGEventSourceStateID::HIDSystemState, false),
                ("hid+clearflags", CGEventSourceStateID::HIDSystemState, true),
                ("private+keepflags", CGEventSourceStateID::Private, false),
                ("private+clearflags", CGEventSourceStateID::Private, true),
            ] {
                let source = CGEventSource::new(state).expect("event source");
                for down in [true, false] {
                    let e = CGEvent::new_keyboard_event(source.clone(), 0, down)
                        .expect("keyboard event");
                    e.set_string(&format!("<{label}>"));
                    if clear_flags {
                        e.set_flags(CGEventFlags::empty());
                    }
                    e.post(CGEventTapLocation::HID);
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            std::thread::sleep(Duration::from_millis(400));
            copy_all();
            std::thread::sleep(Duration::from_millis(300));
            println!("matrix done; result on the clipboard");
            return;
        }

        // Demonstrates what an inherited modifier flag does to injected text.
        // Every injected event is built on keycode 0 — the `a` key — so a stray
        // flag can override the Unicode string entirely.
        if args.iter().any(|a| a == "--flags-demo") {
            use core_graphics::event::CGEventFlags;
            let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
                .expect("create event source");
            std::thread::sleep(Duration::from_secs(3));
            for (label, flags) in [
                ("clean", CGEventFlags::empty()),
                ("option", CGEventFlags::CGEventFlagAlternate),
            ] {
                for ch in format!("[{label}]").chars() {
                    for down in [true, false] {
                        let e = CGEvent::new_keyboard_event(source.clone(), 0, down)
                            .expect("keyboard event");
                        e.set_string(&ch.to_string());
                        e.set_flags(flags);
                        e.post(CGEventTapLocation::HID);
                    }
                    std::thread::sleep(Duration::from_millis(30));
                }
            }
            std::thread::sleep(Duration::from_millis(400));
            copy_all();
            std::thread::sleep(Duration::from_millis(300));
            println!("flags demo done; result on the clipboard");
            return;
        }

        // Replays the key presses for `--text` on the layout given by
        // `--on-layout`, which is how corrections are actually typed.
        if let Some(target) = args
            .iter()
            .position(|a| a == "--on-layout")
            .and_then(|i| args.get(i + 1))
        {
            let presses = rekey_hook::rekey_core_presses(&phrase, target)
                .expect("text should be typeable on that layout");
            let injector = MacInjector::new().expect("injector");
            println!(
                "replaying {} presses for {phrase:?} on {target}",
                presses.len()
            );
            std::thread::sleep(Duration::from_millis(300));
            rekey_hook::Injector::type_keys(&injector, &presses);
            std::thread::sleep(Duration::from_millis(300));
            return;
        }

        // Types a phrase and then taps Option twice, all from one process so that
        // nothing can steal focus between the steps. Separate invocations kept
        // handing focus back to whichever app was frontmost, which made every
        // result meaningless.
        if args.iter().any(|a| a == "--scenario") {
            eprintln!(
                "  target: app={:?} layout={:?}",
                rekey_hook::platform::frontmost_app(),
                rekey_hook::platform::current_layout()
            );
            type_as_user(&phrase, delay_ms);
            std::thread::sleep(Duration::from_millis(900));
            println!("STEP typed");

            for step in 1..=2 {
                tap_option();
                std::thread::sleep(Duration::from_millis(900));
                println!("STEP option{step}");
            }
            return;
        }

        if args.iter().any(|a| a == "--tap-option") {
            tap_option();
            // Deliberately no select-all/copy afterwards: those are chords, and a
            // chord clears the very record the next tap needs. Read the target
            // document directly instead.
            std::thread::sleep(Duration::from_millis(700));
            println!("tapped Option");
            return;
        }

        if as_user {
            eprintln!(
                "  target: app={:?} layout={:?}",
                rekey_hook::platform::frontmost_app(),
                rekey_hook::platform::current_layout()
            );
            type_as_user(&phrase, delay_ms);
            // Give Rekey time to notice and replace.
            std::thread::sleep(Duration::from_millis(900));
            copy_all();
            std::thread::sleep(Duration::from_millis(300));
            println!("typed {phrase:?} at {delay_ms}ms/key; result on the clipboard");
        } else {
            // Report the target, so an empty document is never mistaken for a
            // broken injector when the real cause was focus sitting elsewhere.
            eprintln!(
                "  target: app={:?} layout={:?}",
                rekey_hook::platform::frontmost_app(),
                rekey_hook::platform::current_layout()
            );
            let injector = MacInjector::new().expect("create injector");
            injector.type_text(&phrase);
            std::thread::sleep(Duration::from_millis(400));
            if args.iter().any(|a| a == "--keep") {
                println!("typed {phrase:?} and left it in place");
            } else {
                injector.backspace(phrase.chars().count());
                println!("typed {phrase:?} and removed it again");
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    imp::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("inject-selftest exercises the macOS injector; nothing to do here");
}
