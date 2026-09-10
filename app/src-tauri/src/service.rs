//! The background service: watches the keyboard, runs the engine, applies
//! corrections.

use rekey_core::engine::{Action, Engine};
use rekey_core::input::{Context, TextWriter};
use rekey_hook::platform;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a sampled OS context stays fresh.
///
/// Asking the OS for the frontmost app and active layout on *every* keystroke
/// is an IPC round trip per character, which is far too expensive at typing
/// speed. A short cache makes it negligible while still noticing an app or
/// layout change well within one word.
const CONTEXT_TTL: Duration = Duration::from_millis(150);

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

/// Start the keyboard hook on its own thread.
///
/// The hook API blocks forever, so it gets a dedicated thread; the engine is
/// shared with the UI behind a mutex that is only ever held for the duration of
/// a single keystroke evaluation.
pub fn spawn(engine: SharedEngine) -> Result<(), rekey_hook::HookError> {
    if !platform::has_permission() {
        return Err(rekey_hook::HookError::PermissionDenied);
    }

    let injector: Arc<dyn TextWriter> = Arc::new(new_injector()?);

    std::thread::Builder::new()
        .name("rekey-hook".into())
        .spawn(move || {
            let mut cache = ContextCache::new();
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
                    action.apply(injector.as_ref());
                }
            });
            if let Err(e) = result {
                log::error!("keyboard hook stopped: {e}");
            }
        })
        .map_err(|e| rekey_hook::HookError::Os(e.to_string()))?;

    Ok(())
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
