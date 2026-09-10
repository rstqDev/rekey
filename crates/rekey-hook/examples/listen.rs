//! Prints every keyboard event Rekey's hook observes, for a few seconds.
//!
//! `cargo run -p rekey-hook --example listen`
//!
//! The quickest way to answer "is the hook seeing this at all?" — which is a
//! very different question from "is the engine reacting to it".
//!
//! It is also how to check that Rekey still recognises its own typing. Run
//! this, then in another shell:
//!
//! ```text
//! cargo run -p rekey-hook --example inject-selftest -- --text "hi " --on-layout us
//! ```
//!
//! Every resulting event must report `synthetic=true`. If it does not, Rekey
//! will react to its own corrections and type over them — which looks like
//! doubled letters rather than anything to do with detection.

use rekey_hook::platform;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();

    std::thread::spawn(move || {
        let result = platform::run(move |event| {
            let n = counter.fetch_add(1, Ordering::Relaxed);
            println!(
                "{n:>3}  {:<18} char={:?} alt={} shift={} ctrl={} cmd={} synthetic={}",
                format!("{:?}", event.key),
                event.character,
                event.modifiers.alt,
                event.modifiers.shift,
                event.modifiers.control,
                event.modifiers.meta,
                event.synthetic,
            );
        });
        if let Err(e) = result {
            eprintln!("hook failed: {e}");
        }
    });

    println!("listening for {seconds}s…");
    std::thread::sleep(std::time::Duration::from_secs(seconds));
    println!("saw {} events", count.load(Ordering::Relaxed));
}
