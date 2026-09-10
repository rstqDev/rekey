//! Where Rekey's settings live on disk, and how they are loaded.

use rekey_core::config::Config;
use std::path::PathBuf;

/// Settings plus the things that are not part of the detection config.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub config: Config,
    /// Anthropic API key for the optional assist. Stored in the app's own
    /// config directory; empty unless the user pastes one in.
    pub api_key: String,
    pub launch_at_login: bool,
}

/// `~/Library/Application Support/app.rekey.desktop` (macOS) or
/// `%APPDATA%\app.rekey.desktop` (Windows).
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("app.rekey.desktop")
}

pub fn config_path() -> PathBuf {
    config_dir().join("settings.json")
}

impl Settings {
    /// Load settings, falling back to defaults for anything unreadable.
    ///
    /// A corrupt settings file must never stop the app from starting: the user
    /// would have no way to fix it from inside the UI.
    pub fn load() -> Settings {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(settings) => settings,
                Err(e) => {
                    log::warn!(
                        "settings at {} are unreadable ({e}); using defaults",
                        path.display()
                    );
                    Settings::default()
                }
            },
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let dir = config_dir();
        std::fs::create_dir_all(&dir)?;
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(config_path(), text)
    }
}

/// Find the trained language models.
///
/// Tried in order: the bundled resource directory, then the source tree, so a
/// development build works without installing anything.
pub fn models_dir(app: &tauri::AppHandle) -> PathBuf {
    use tauri::Manager;
    if let Ok(resources) = app.path().resource_dir() {
        let bundled = resources.join("models");
        if bundled.is_dir() {
            return bundled;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/rekey-core/data/models")
}
