//! User-tunable behaviour for the detection engine.

use serde::{Deserialize, Serialize};

/// How eager the detector should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    /// Only act on overwhelming evidence. Almost never wrong, misses more.
    Cautious,
    /// The default: acts when the other reading is clearly a real word.
    Balanced,
    /// Acts on weaker evidence. Catches more slang, occasionally overreaches.
    Eager,
}

impl Sensitivity {
    /// Minimum log10 score advantage the alternative reading must have.
    pub fn threshold(self) -> f32 {
        match self {
            Sensitivity::Cautious => 3.5,
            Sensitivity::Balanced => 2.0,
            Sensitivity::Eager => 1.0,
        }
    }

    /// Extra advantage demanded when the text as typed is *already* a real word
    /// in the active language. Converting those is how a switcher earns its
    /// reputation for being infuriating, so the bar is deliberately steep.
    pub fn known_word_surcharge(self) -> f32 {
        match self {
            Sensitivity::Cautious => 6.0,
            Sensitivity::Balanced => 4.5,
            Sensitivity::Eager => 3.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Master switch.
    pub enabled: bool,
    /// Layout ids the user actually types in, e.g. `["us", "ru"]`.
    pub layouts: Vec<String>,
    pub sensitivity: Sensitivity,
    /// Words shorter than this are never converted automatically.
    pub min_word_len: usize,
    /// Below this length, only an exact dictionary hit will do; letter-shape
    /// evidence alone is too weak on very short words.
    pub require_dict_below_len: usize,
    /// Never touch words the user has corrected back by hand.
    pub exceptions: Vec<String>,
    /// Applications that Rekey ignores entirely (bundle id or executable name).
    pub excluded_apps: Vec<String>,
    /// Leave ALL-CAPS tokens alone; they are usually acronyms or constants.
    pub skip_all_caps: bool,
    /// Ask the optional AI assist about words the local model finds ambiguous.
    pub ai_assist: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            enabled: true,
            layouts: vec!["us".into(), "ru".into()],
            sensitivity: Sensitivity::Balanced,
            min_word_len: 2,
            require_dict_below_len: 4,
            exceptions: Vec::new(),
            excluded_apps: default_excluded_apps(),
            skip_all_caps: true,
            ai_assist: false,
        }
    }
}

/// Password managers and terminals, where silently rewriting keystrokes ranges
/// from annoying to genuinely destructive.
fn default_excluded_apps() -> Vec<String> {
    [
        "com.apple.keychainaccess",
        "com.1password.1password",
        "com.agilebits.onepassword7",
        "com.bitwarden.desktop",
        "com.apple.Terminal",
        "com.googlecode.iterm2",
        "1Password.exe",
        "KeePass.exe",
        "Bitwarden.exe",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Config {
    /// Layout ids other than `active` that are worth testing as alternatives.
    pub fn alternatives(&self, active: &str) -> Vec<&str> {
        self.layouts
            .iter()
            .map(|s| s.as_str())
            .filter(|id| *id != active)
            .collect()
    }

    pub fn is_exception(&self, word: &str) -> bool {
        let w = word.to_lowercase();
        self.exceptions.iter().any(|e| e.to_lowercase() == w)
    }

    pub fn is_excluded_app(&self, app: &str) -> bool {
        self.excluded_apps
            .iter()
            .any(|a| a.eq_ignore_ascii_case(app))
    }
}
