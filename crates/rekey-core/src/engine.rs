//! Turns a stream of keystrokes into corrections.
//!
//! This is where the pieces meet: the buffer says when a word ended, the
//! detector says whether it was meant for another layout, and the engine
//! decides whether it is safe to act right now. "Safe" does most of the work —
//! the right answer typed into a password field is still a bug.

use crate::buffer::{Buffer, Input, LastCorrection};
use crate::config::Config;
use crate::detect::{Detector, Verdict};
use crate::input::{Context, Key, KeyEvent, TextWriter};
use crate::model::Models;

/// What the engine wants done to the text the user is typing.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Leave everything alone.
    None,
    /// Delete `delete` characters and type `text` in their place.
    Replace { delete: usize, text: String },
}

impl Action {
    pub fn apply(&self, writer: &dyn TextWriter) {
        if let Action::Replace { delete, text } = self {
            writer.replace(*delete, text);
        }
    }
}

/// Running totals, shown in the menu bar popover.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub corrections: u64,
    pub undos: u64,
}

pub struct Engine {
    pub detector: Detector,
    buffer: Buffer,
    last_correction: Option<LastCorrection>,
    last_context: Option<(Option<String>, Option<String>)>,
    stats: Stats,
}

impl Engine {
    pub fn new(models: Models, config: Config) -> Engine {
        Engine {
            detector: Detector::new(models, config),
            buffer: Buffer::new(),
            last_correction: None,
            last_context: None,
            stats: Stats::default(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.detector.config
    }

    pub fn set_config(&mut self, config: Config) {
        self.detector.config = config;
        // Settings changed under the user's fingers; the word in progress was
        // judged under the old rules, so start clean.
        self.buffer.push(Input::Reset);
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Feed one keystroke. Returns what should be done to the document.
    pub fn on_key(&mut self, event: &KeyEvent, ctx: &Context) -> Action {
        // Never react to our own typing, or the app would correct itself in a
        // loop until the buffer ran out.
        if event.synthetic {
            return Action::None;
        }

        // The caret may have moved between apps or layouts. Either way the
        // buffer's idea of what is behind the caret is no longer trustworthy.
        let context_key = (ctx.app.clone(), ctx.layout.clone());
        if self
            .last_context
            .as_ref()
            .is_some_and(|prev| *prev != context_key)
        {
            self.buffer.push(Input::Reset);
            self.last_correction = None;
        }
        self.last_context = Some(context_key);

        if !self.detector.config.enabled {
            return Action::None;
        }
        // A password field has taken over the keyboard; do not even track.
        if ctx.secure_input {
            self.buffer.push(Input::Reset);
            return Action::None;
        }
        if let Some(app) = &ctx.app {
            if self.detector.config.is_excluded_app(app) {
                self.buffer.push(Input::Reset);
                return Action::None;
            }
        }
        // Ctrl/Cmd chords are commands, not typing, and they usually move the
        // caret as a side effect.
        if event.modifiers.is_shortcut() {
            self.buffer.push(Input::Reset);
            return Action::None;
        }

        let input = match event.key {
            Key::Backspace => Input::Backspace,
            Key::Navigation | Key::Escape | Key::Other => Input::Reset,
            // Only whitespace ends a word. Punctuation cannot: `,` and `.` are
            // the Cyrillic letters б and ю, so treating them as boundaries
            // would chop the very words this app exists to fix.
            Key::Space => Input::Boundary(' '),
            Key::Tab => Input::Boundary('\t'),
            Key::Enter => Input::Boundary('\n'),
            Key::Character => match event.character {
                Some(c) if !c.is_control() => Input::Char(c),
                _ => Input::Reset,
            },
        };

        let Some(word) = self.buffer.push(input) else {
            return Action::None;
        };
        let Some(active) = ctx.layout.as_deref() else {
            // Without knowing the active layout there is nothing to compare
            // against; better to do nothing than to guess.
            return Action::None;
        };

        self.consider(&word.text, word.terminator, active)
    }

    /// Evaluate one finished word and produce the corresponding action.
    fn consider(&mut self, text: &str, terminator: Option<char>, active: &str) -> Action {
        let (prefix, core, suffix) = self.detector.trim_affixes(text, active);
        if core.is_empty() {
            return Action::None;
        }

        let Verdict::Switch(candidate) = self.detector.evaluate(core, active) else {
            return Action::None;
        };

        let corrected = format!("{prefix}{}{suffix}", candidate.converted);
        let mut replacement = corrected.clone();
        if let Some(t) = terminator {
            replacement.push(t);
        }

        let delete = text.chars().count() + usize::from(terminator.is_some());
        self.last_correction = Some(LastCorrection {
            original: text.to_string(),
            corrected,
            terminator,
        });
        self.stats.corrections += 1;

        Action::Replace {
            delete,
            text: replacement,
        }
    }

    /// Undo the most recent automatic correction. Backs the undo hotkey.
    pub fn undo(&mut self) -> Action {
        let Some(last) = self.last_correction.take() else {
            return Action::None;
        };
        self.stats.undos += 1;
        // The user disagreed with us about this word; remember that, so the
        // same correction is not offered again.
        let word = last.original.to_lowercase();
        if !self.detector.config.exceptions.contains(&word) {
            self.detector.config.exceptions.push(word);
        }
        let action = Action::Replace {
            delete: last.chars_to_delete(),
            text: last.restore_text(),
        };
        self.buffer.push(Input::Reset);
        action
    }

    /// Force a conversion of the word being typed, ignoring every threshold.
    /// Backs the manual hotkey for when the detector chose not to act.
    pub fn force_convert(&mut self, active: &str) -> Action {
        let Some(word) = self.buffer.take_current() else {
            return Action::None;
        };
        let Some(target) = self.detector.best_target(&word.text, active) else {
            return Action::None;
        };
        let Some(converted) = self.detector.force_convert(&word.text, active, target) else {
            return Action::None;
        };
        self.last_correction = Some(LastCorrection {
            original: word.text.clone(),
            corrected: converted.clone(),
            terminator: None,
        });
        self.stats.corrections += 1;
        Action::Replace {
            delete: word.text.chars().count(),
            text: converted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Key, Modifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent {
            character: Some(c),
            key: Key::Character,
            modifiers: Modifiers::default(),
            synthetic: false,
        }
    }

    fn special(k: Key) -> KeyEvent {
        KeyEvent {
            character: None,
            key: k,
            modifiers: Modifiers::default(),
            synthetic: false,
        }
    }

    fn ctx(layout: &str) -> Context {
        Context {
            app: Some("com.apple.TextEdit".into()),
            layout: Some(layout.into()),
            secure_input: false,
        }
    }

    fn engine() -> Engine {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/models");
        let models = Models::load_dir(&dir).expect("models");
        assert!(!models.is_empty(), "run modelgen first");
        Engine::new(
            models,
            Config {
                layouts: vec!["us".into(), "ru".into()],
                ..Config::default()
            },
        )
    }

    /// Type a string and return the last non-None action.
    fn type_str(e: &mut Engine, s: &str, layout: &str) -> Action {
        let c = ctx(layout);
        let mut last = Action::None;
        for ch in s.chars() {
            let ev = if ch == ' ' {
                special(Key::Space)
            } else {
                key(ch)
            };
            let action = e.on_key(&ev, &c);
            if action != Action::None {
                last = action;
            }
        }
        last
    }

    #[test]
    fn corrects_a_word_at_the_space() {
        let mut e = engine();
        let action = type_str(&mut e, "ghbdtn ", "us");
        assert_eq!(
            action,
            Action::Replace {
                delete: 7,
                text: "привет ".into()
            }
        );
        assert_eq!(e.stats().corrections, 1);
    }

    #[test]
    fn leaves_correct_words_untouched() {
        let mut e = engine();
        assert_eq!(type_str(&mut e, "hello world ", "us"), Action::None);
        assert_eq!(e.stats().corrections, 0);
    }

    #[test]
    fn keeps_trailing_punctuation_and_puts_it_back() {
        let mut e = engine();
        // `!` is punctuation on both layouts, so it is set aside and restored.
        let action = type_str(&mut e, "ghbdtn! ", "us");
        assert_eq!(
            action,
            Action::Replace {
                delete: 8,
                text: "привет! ".into()
            }
        );
    }

    #[test]
    fn does_not_strip_punctuation_that_is_a_cyrillic_letter() {
        let mut e = engine();
        // The trailing `,` is the letter б, part of the word "рыб".
        let action = type_str(&mut e, "hs, ", "us");
        match action {
            Action::Replace { text, .. } => {
                assert!(text.starts_with("рыб"), "expected рыб, got {text:?}");
            }
            Action::None => panic!("expected a correction for 'hs,'"),
        }
    }

    #[test]
    fn ignores_its_own_typing() {
        let mut e = engine();
        // Rekey replaying a correction must not be read back as user input, or
        // the app would keep correcting its own output.
        for ch in "ghbdtn".chars() {
            let mut ev = key(ch);
            ev.synthetic = true;
            assert_eq!(e.on_key(&ev, &ctx("us")), Action::None);
        }
        let mut space = special(Key::Space);
        space.synthetic = true;
        assert_eq!(e.on_key(&space, &ctx("us")), Action::None);
        assert_eq!(e.stats().corrections, 0);
    }

    #[test]
    fn does_not_convert_gibberish_into_other_gibberish() {
        let mut e = engine();
        // "hbdtn" reads as "ривет" in Russian, which is not a word either.
        // Rewriting one nonsense string as another is pure churn.
        assert_eq!(type_str(&mut e, "hbdtn ", "us"), Action::None);
    }

    #[test]
    fn stands_down_in_password_fields() {
        let mut e = engine();
        let secure = Context {
            secure_input: true,
            ..ctx("us")
        };
        for ch in "ghbdtn".chars() {
            e.on_key(&key(ch), &secure);
        }
        assert_eq!(e.on_key(&special(Key::Space), &secure), Action::None);
        assert_eq!(e.stats().corrections, 0);
    }

    #[test]
    fn stands_down_in_excluded_apps() {
        let mut e = engine();
        let terminal = Context {
            app: Some("com.googlecode.iterm2".into()),
            ..ctx("us")
        };
        for ch in "ghbdtn".chars() {
            e.on_key(&key(ch), &terminal);
        }
        assert_eq!(e.on_key(&special(Key::Space), &terminal), Action::None);
    }

    #[test]
    fn a_shortcut_discards_the_word_in_progress() {
        let mut e = engine();
        for ch in "ghbdt".chars() {
            e.on_key(&key(ch), &ctx("us"));
        }
        let mut save = key('s');
        save.modifiers.meta = true;
        e.on_key(&save, &ctx("us"));
        // "n " alone is not a word, and the earlier letters are gone.
        assert_eq!(type_str(&mut e, "n ", "us"), Action::None);
    }

    #[test]
    fn switching_apps_discards_the_word_in_progress() {
        let mut e = engine();
        for ch in "ghbdt".chars() {
            e.on_key(&key(ch), &ctx("us"));
        }
        let other = Context {
            app: Some("com.apple.Safari".into()),
            ..ctx("us")
        };
        e.on_key(&key('n'), &other);
        assert_eq!(e.on_key(&special(Key::Space), &other), Action::None);
    }

    #[test]
    fn arrow_keys_discard_the_word_in_progress() {
        let mut e = engine();
        for ch in "ghbdt".chars() {
            e.on_key(&key(ch), &ctx("us"));
        }
        e.on_key(&special(Key::Navigation), &ctx("us"));
        assert_eq!(type_str(&mut e, "n ", "us"), Action::None);
    }

    #[test]
    fn undo_restores_the_original_and_learns_the_exception() {
        let mut e = engine();
        type_str(&mut e, "ghbdtn ", "us");
        let undo = e.undo();
        assert_eq!(
            undo,
            Action::Replace {
                delete: 7,
                text: "ghbdtn ".into()
            }
        );
        assert_eq!(e.stats().undos, 1);
        // Having been overruled once, it does not try again.
        assert_eq!(type_str(&mut e, "ghbdtn ", "us"), Action::None);
    }

    #[test]
    fn undo_without_a_correction_does_nothing() {
        let mut e = engine();
        assert_eq!(e.undo(), Action::None);
    }

    #[test]
    fn manual_hotkey_converts_below_threshold() {
        let mut e = engine();
        // Two letters that the detector would never touch on its own.
        for ch in "qx".chars() {
            e.on_key(&key(ch), &ctx("us"));
        }
        match e.force_convert("us") {
            Action::Replace { delete, text } => {
                assert_eq!(delete, 2);
                assert_eq!(text, "йч");
            }
            Action::None => panic!("manual conversion should always act"),
        }
    }

    #[test]
    fn disabled_engine_does_nothing() {
        let mut e = engine();
        let mut cfg = e.config().clone();
        cfg.enabled = false;
        e.set_config(cfg);
        assert_eq!(type_str(&mut e, "ghbdtn ", "us"), Action::None);
    }

    #[test]
    fn unknown_layout_is_never_guessed() {
        let mut e = engine();
        let unknown = Context {
            layout: None,
            ..ctx("us")
        };
        for ch in "ghbdtn".chars() {
            e.on_key(&key(ch), &unknown);
        }
        assert_eq!(e.on_key(&special(Key::Space), &unknown), Action::None);
    }
}
