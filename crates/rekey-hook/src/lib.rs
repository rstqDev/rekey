//! Watching the keyboard and putting corrected text back, per platform.
//!
//! Rekey observes keystrokes but never swallows them: the character the user
//! typed always lands. A correction is applied afterwards by sending backspaces
//! followed by the replacement, which is what makes the app work in every text
//! field on the system without knowing anything about the app it is typing in.
//!
//! Everything here is a thin, honest wrapper over OS APIs. Anything resembling
//! a decision belongs in `rekey-core`.

use std::fmt;

pub use rekey_core::input::{Context, Key, KeyEvent, Modifiers, TextWriter as Injector};

pub mod noop;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos as platform;

#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
pub use windows as platform;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use noop as platform;

#[derive(Debug)]
pub enum HookError {
    /// The OS refused to install the hook. On macOS this is almost always a
    /// missing Accessibility grant.
    PermissionDenied,
    Unsupported(&'static str),
    Os(String),
}

impl fmt::Display for HookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HookError::PermissionDenied => write!(
                f,
                "Rekey needs Accessibility permission to watch the keyboard"
            ),
            HookError::Unsupported(s) => write!(f, "not supported on this platform: {s}"),
            HookError::Os(s) => write!(f, "operating system error: {s}"),
        }
    }
}

impl std::error::Error for HookError {}
