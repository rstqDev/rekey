//! The background service: watches the keyboard, runs the engine, applies
//! corrections.

use rekey_core::engine::{Action, Engine};
use rekey_core::input::{Context, TextWriter};
use rekey_hook::platform;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::AppHandle;

/// How long a sampled OS context stays fresh.
///
/// Asking the OS for the frontmost app and active layout on *every* keystroke
/// is an IPC round trip per character, which is far too expensive at typing
/// speed. A short cache makes it negligible while still noticing an app or
/// layout change well within one word.
const CONTEXT_TTL: Duration = Duration::from_millis(150);

/// How long to wait before replacing text.
///
/// The event tap sees a keystroke while it is still in flight, so the space
/// that ended the word has not necessarily reached the application yet.
/// Replacing immediately can race it and eat the wrong character. A short
/// pause costs nothing perceptible and removes the race.
const REPLACE_DELAY: Duration = Duration::from_millis(25);

/// How often to re-check whether Accessibility has been granted.
///
/// macOS gives no notification when the user flips the switch, so the only way
/// to notice is to ask. A second is far below the time it takes to find the
/// setting, and the check is a cheap in-process call.
const PERMISSION_POLL: Duration = Duration::from_secs(1);

/// How long to wait for a requested layout to become active, and in what steps.
/// Short, because the wait happens on the main thread.
const LAYOUT_SETTLE_STEP: Duration = Duration::from_millis(10);
const LAYOUT_SETTLE_STEPS: usize = 8;
const LAYOUT_SWITCH_TIMEOUT: Duration = Duration::from_millis(400);

/// The most recently sampled OS context, shared with the hook thread.
///
/// The hook thread must never sample this itself. macOS Text Input Services
/// and AppKit are main-thread-only: calling `TISGetInputSourceProperty` from
/// the event tap thread trips `dispatch_assert_queue` and kills the process
/// with SIGILL on the first keystroke. The sampling therefore happens on the
/// main thread and the result is published here for the hook to read.
type SharedContext = Arc<Mutex<Context>>;

/// Poll the OS for the frontmost app, active layout and secure-input state,
/// on the main thread, and publish the result for the hook thread.
///
/// Sampling per keystroke would be an IPC round trip per character, which is
/// far too expensive at typing speed; [`CONTEXT_TTL`] is short enough to notice
/// an app or layout change well within one word.
fn spawn_context_sampler(app: &AppHandle, context: SharedContext) {
    let handle = app.clone();
    std::thread::Builder::new()
        .name("rekey-context".into())
        .spawn(move || loop {
            let slot = context.clone();
            // Hop to the main thread to do the actual OS calls.
            let posted = handle.run_on_main_thread(move || {
                let sampled = platform::current_context();
                if let Ok(mut current) = slot.lock() {
                    *current = sampled;
                }
            });
            if posted.is_err() {
                // The app is shutting down.
                break;
            }
            std::thread::sleep(CONTEXT_TTL);
        })
        .expect("spawn context sampler");
}

/// Shared handle to the running engine, used by the UI and the tray.
pub type SharedEngine = Arc<Mutex<Engine>>;

/// Start the keyboard hook, waiting for Accessibility if it is not granted yet.
///
/// Returns a flag the UI can read to tell whether corrections are actually
/// running. The hook API blocks forever, so it gets a dedicated thread; the
/// engine is shared with the UI behind a mutex that is only ever held for the
/// duration of a single keystroke evaluation.
///
/// Waiting rather than failing matters: the alternative is telling the user to
/// quit and reopen the app after granting permission, which is the single most
/// confusing moment in installing a tool like this.
pub fn spawn(
    app: &AppHandle,
    engine: SharedEngine,
) -> Result<Arc<AtomicBool>, rekey_hook::HookError> {
    let running = Arc::new(AtomicBool::new(false));
    let flag = running.clone();

    let context: SharedContext = Arc::new(Mutex::new(Context::default()));
    spawn_context_sampler(app, context.clone());

    let injector: Arc<dyn TextWriter> = Arc::new(new_injector()?);
    let replacements = spawn_injector(app, injector)?;

    std::thread::Builder::new()
        .name("rekey-hook".into())
        .spawn(move || {
            if !platform::has_permission() {
                log::info!("waiting for Accessibility permission…");
                while !platform::has_permission() {
                    std::thread::sleep(PERMISSION_POLL);
                }
                log::info!("Accessibility granted; starting the keyboard hook");
            }

            flag.store(true, Ordering::Relaxed);
            let result = platform::run(move |event| {
                // Read the published sample; never call into the OS here.
                let ctx = match context.lock() {
                    Ok(current) => current.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let action = match engine.lock() {
                    Ok(mut engine) => engine.on_key(&event, &ctx),
                    Err(poisoned) => {
                        // A panic in the engine must not silently stop
                        // corrections for the rest of the session.
                        log::error!("engine mutex was poisoned; recovering");
                        poisoned.into_inner().on_key(&event, &ctx)
                    }
                };
                if let Action::Replace { .. } = &action {
                    // Hand the work to the injector thread and return at once.
                    // macOS disables an event tap whose callback is slow, and
                    // a replacement is a dozen synthetic events.
                    let _ = replacements.send(action);
                }
            });
            flag.store(false, Ordering::Relaxed);
            if let Err(e) = result {
                log::error!("keyboard hook stopped: {e}");
            }
        })
        .map_err(|e| rekey_hook::HookError::Os(e.to_string()))?;

    Ok(running)
}

/// Carry out one correction.
///
/// The keyboard is switched *before* the replacement is typed, not after.
/// Rekey types by replaying physical key presses, and a key press only means
/// the right character once the matching layout is active — so the order is
/// part of the mechanism, not a nicety. It also leaves the user on the right
/// layout for the next word, which is the point of switching at all.
fn apply(handle: &AppHandle, injector: &dyn TextWriter, action: &Action) {
    let Action::Replace {
        delete,
        text,
        switch_to,
    } = action
    else {
        return;
    };

    let selected = match switch_to {
        Some(layout) => select_layout_and_wait(handle, layout),
        None => None,
    };

    if *delete > 0 {
        injector.backspace(*delete);
    }

    // Replay the presses when the right layout is active; otherwise fall back
    // to attaching the text to the events and hope the app honours it.
    let replayed = selected
        .as_deref()
        .and_then(|layout| rekey_core::layout::presses_for(text, layout))
        .is_some_and(|presses| injector.type_keys(&presses));

    if !replayed {
        log::debug!("replaying key presses was not possible; typing {text:?} directly");
        injector.type_text(text);
    }
}

/// Select `layout` on the main thread and confirm it took effect.
///
/// Returns the layout once it is actually active. Typing replayed key presses
/// against the wrong layout would produce the wrong characters, so this is
/// deliberately a confirmation rather than a request.
fn select_layout_and_wait(handle: &AppHandle, layout: &str) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let wanted = layout.to_string();
    let reply = wanted.clone();

    let posted = handle.run_on_main_thread(move || {
        let mut active = platform::select_layout(&reply)
            && platform::current_layout().as_deref() == Some(reply.as_str());
        // Selection is asynchronous; give it a moment to land. The window is
        // short because it runs on the main thread.
        for _ in 0..LAYOUT_SETTLE_STEPS {
            if active {
                break;
            }
            std::thread::sleep(LAYOUT_SETTLE_STEP);
            active = platform::current_layout().as_deref() == Some(reply.as_str());
        }
        let _ = tx.send(active);
    });

    if posted.is_err() {
        return None;
    }
    match rx.recv_timeout(LAYOUT_SWITCH_TIMEOUT) {
        Ok(true) => Some(wanted),
        Ok(false) => {
            log::debug!("layout {wanted} did not become active; not replaying key presses");
            None
        }
        Err(_) => None,
    }
}

/// Start the thread that applies corrections, and return its inbox.
///
/// Injection is deliberately off the tap callback: it keeps the callback fast
/// enough that macOS will not disable the tap, and it lets the keystroke that
/// triggered the correction land before the correction is typed.
fn spawn_injector(
    app: &AppHandle,
    injector: Arc<dyn TextWriter>,
) -> Result<Sender<Action>, rekey_hook::HookError> {
    let (tx, rx) = mpsc::channel::<Action>();
    let handle = app.clone();
    std::thread::Builder::new()
        .name("rekey-inject".into())
        .spawn(move || {
            for action in rx {
                std::thread::sleep(REPLACE_DELAY);
                apply(&handle, injector.as_ref(), &action);
            }
        })
        .map_err(|e| rekey_hook::HookError::Os(e.to_string()))?;
    Ok(tx)
}

#[cfg(target_os = "macos")]
fn new_injector() -> Result<rekey_hook::macos::MacInjector, rekey_hook::HookError> {
    rekey_hook::macos::MacInjector::new()
}

#[cfg(target_os = "windows")]
fn new_injector() -> Result<rekey_hook::windows::WindowsInjector, rekey_hook::HookError> {
    rekey_hook::windows::WindowsInjector::new()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn new_injector() -> Result<rekey_hook::noop::RecordingInjector, rekey_hook::HookError> {
    Ok(rekey_hook::noop::RecordingInjector::new())
}
