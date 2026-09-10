//! Reports what Rekey can see of the system right now.
//!
//! `cargo run -p rekey-hook --example doctor`
//!
//! Useful both for diagnosing "why isn't it correcting anything" and for
//! checking the platform FFI works without building the whole app.

use rekey_hook::platform;

fn main() {
    println!("Rekey doctor\n");

    let permission = platform::has_permission();
    println!(
        "  accessibility     {}",
        if permission {
            "granted"
        } else {
            "NOT granted — Rekey cannot watch the keyboard"
        }
    );

    match platform::current_layout() {
        Some(layout) => {
            let name = rekey_core::layout(&layout)
                .map(|l| l.def.name)
                .unwrap_or("unknown");
            println!("  active layout     {layout}  ({name})");
        }
        None => println!(
            "  active layout     unrecognised — Rekey does nothing rather than guess"
        ),
    }

    match platform::frontmost_app() {
        Some(app) => println!("  frontmost app     {app}"),
        None => println!("  frontmost app     unknown"),
    }

    println!(
        "  secure input      {}",
        if platform::secure_input_active() {
            "ON — a password field has the keyboard; Rekey stands down"
        } else {
            "off"
        }
    );

    println!("\nLayouts Rekey knows:");
    for def in rekey_core::layout::LAYOUTS {
        let limited = if def.latin_overlap && def.id != "us" {
            "  (limited: shares the Latin alphabet with US QWERTY)"
        } else {
            ""
        };
        println!("  {:<4} {}{}", def.id, def.name, limited);
    }

    if !permission {
        println!(
            "\nGrant access in System Settings → Privacy & Security → Accessibility,\n\
             then run this again. The app must be relaunched after granting."
        );
    }
}
