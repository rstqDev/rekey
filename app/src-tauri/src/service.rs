//! The background service: watches the keyboard, runs the engine, applies
//! corrections.

use rekey_core::engine::{Action, Engine};
use rekey_core::input::{Context, TextWriter};
use rekey_hook::platform;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

struct ContextCache {
    value: Context,
    sampled_at: Instant,
}

impl ContextCache {
    fn new() -> ContextCache {
        ContextCache {
            value: platform::current_context(),
            sampled_at: Instant::now(),
        }
    }

    fn get(&mut self) -> &Context {
        if self.sampled_at.elapsed() >= CONTEXT_TTL {
            self.value = platform::current_context();
            self.sampled_at = Instant::now();
        }
        &self.value
    }
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
pub fn spawn(engine: SharedEngine) -> Result<Arc<AtomicBool>, rekey_hook::HookError> {
    let running = Arc::new(AtomicBool::new(false));
    let flag = running.clone();

    let injector: Arc<dyn TextWriter> = Arc::new(new_injector()?);
    let replacements = spawn_injector(injector)?;

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

            let mut cache = ContextCache::new();
            flag.store(true, Ordering::Relaxed);
            let result = platform::run(move |event| {
                let ctx = cache.get().clone();
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

/// Start the thread that applies corrections, and return its inbox.
///
/// Injection is deliberately off the tap callback: it keeps the callback fast
/// enough that macOS will not disable the tap, and it lets the keystroke that
/// triggered the correction land before the correction is typed.
fn spawn_injector(injector: Arc<dyn TextWriter>) -> Result<Sender<Action>, rekey_hook::HookError> {
    let (tx, rx) = mpsc::channel::<Action>();
    std::thread::Builder::new()
        .name("rekey-inject".into())
        .spawn(move || {
            for action in rx {
                std::thread::sleep(REPLACE_DELAY);
                action.apply(injector.as_ref());
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
