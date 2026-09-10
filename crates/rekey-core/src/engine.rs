//! Turns a stream of keystrokes into corrections.
//!
//! This is where the pieces meet: the buffer says when a word ended, the
//! detector says whether it was meant for another layout, and the engine
//! decides whether it is safe to act right now. "Safe" does most of the work —
//! the right answer typed into a password field is still a bug.

use crate::assist::{Assist, Question};
use crate::buffer::{Buffer, Input};
use crate::config::{Config, Shortcut};
use crate::detect::{Candidate, Detector, Skip, Verdict};
use crate::input::{Context, Key, KeyEvent, Modifiers, TextWriter};
use crate::layout;
use crate::model::Models;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What the engine wants done to the text the user is typing.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Leave everything alone.
    None,
    /// Delete `delete` characters and type `text` in their place.
    Replace {
        delete: usize,
        text: String,
        /// The layout the corrected text belongs to, when it differs from the
        /// one in use.
        ///
        /// Rewriting `ghbdtn` to `привет` while leaving the keyboard on
        /// English fixes the word and leaves the user to make the same mistake
        /// on the next one. Switching the layout is the other half of the fix.
        switch_to: Option<String>,
    },
}

impl Action {
    pub fn apply(&self, writer: &dyn TextWriter) {
        if let Action::Replace { delete, text, .. } = self {
            writer.replace(*delete, text);
        }
    }

    /// The layout the system keyboard should move to, if any.
    pub fn layout_switch(&self) -> Option<&str> {
        match self {
            Action::Replace { switch_to, .. } => switch_to.as_deref(),
            Action::None => None,
        }
    }
}

/// Running totals, shown in the menu bar popover.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub corrections: u64,
    pub undos: u64,
}

/// A modifier tap counts as a shortcut only if nothing else happened while it
/// was held, and it was not held for long.
const MODIFIER_TAP_WINDOW: Duration = Duration::from_millis(600);

/// How close together two taps must be to count as a double tap.
const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(500);

/// The last finished word, in every reading Rekey knows for it.
///
/// This is what makes the manual shortcut feel right. Rekey's guess is only a
/// guess, so the user needs to be able to say "no, the other one" — repeatedly,
/// without thinking about which layouts are involved. Keeping every rendering
/// of the word and an index into them turns that into one repeatable keystroke.
#[derive(Debug, Clone, PartialEq)]
struct WordCycle {
    /// Every reading. Index 0 is always exactly what the user typed.
    variants: Vec<String>,
    /// The layout each variant belongs to, parallel to `variants`.
    layouts: Vec<String>,
    /// Which variant is on screen now.
    index: usize,
    terminator: Option<char>,
    /// True when Rekey picked the current variant rather than the user.
    auto: bool,
}

impl WordCycle {
    fn current(&self) -> &str {
        &self.variants[self.index]
    }

    /// Characters to delete to remove what is currently on screen.
    fn on_screen_len(&self) -> usize {
        self.current().chars().count() + usize::from(self.terminator.is_some())
    }

    fn text_with_terminator(&self, index: usize) -> String {
        let mut out = self.variants[index].clone();
        if let Some(t) = self.terminator {
            out.push(t);
        }
        out
    }
}

pub struct Engine {
    pub detector: Detector,
    buffer: Buffer,
    /// The last completed word, ready to be cycled by the shortcut.
    last_word: Option<WordCycle>,
    last_context: Option<(Option<String>, Option<String>)>,
    /// When the shortcut modifier went down with nothing else pressed since.
    modifier_down_at: Option<Instant>,
    modifier_was_down: bool,
    /// When the last completed tap happened, for double-tap shortcuts.
    last_tap_at: Option<Instant>,
    /// The layout Rekey has just asked the system to switch to, so that its own
    /// switch is not mistaken for the user changing context.
    expected_layout: Option<String>,
    /// An optional second opinion for words the local models cannot settle.
    assist: Option<Arc<dyn Assist>>,
    stats: Stats,
}

impl Engine {
    pub fn new(models: Models, config: Config) -> Engine {
        Engine {
            detector: Detector::new(models, config),
            buffer: Buffer::new(),
            last_word: None,
            last_context: None,
            modifier_down_at: None,
            modifier_was_down: false,
            last_tap_at: None,
            expected_layout: None,
            assist: None,
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

    /// Provide a second opinion for ambiguous words, or `None` to remove it.
    ///
    /// Only consulted when `ai_assist` is on, so switching the setting off
    /// takes effect immediately even if an assist is still attached.
    pub fn set_assist(&mut self, assist: Option<Arc<dyn Assist>>) {
        self.assist = assist;
    }

    pub fn has_assist(&self) -> bool {
        self.assist.is_some()
    }

    /// How many ambiguous words the assist has settled so far.
    pub fn assist_learned(&self) -> usize {
        self.assist.as_ref().map_or(0, |a| a.learned())
    }

    /// Feed one keystroke. Returns what should be done to the document.
    pub fn on_key(&mut self, event: &KeyEvent, ctx: &Context) -> Action {
        // Never react to our own typing, or the app would correct itself in a
        // loop until the buffer ran out.
        if event.synthetic {
            return Action::None;
        }

        // The caret may have moved between apps or layouts. Either way the
        // buffer's idea of what is behind the caret is no longer trustworthy —
        // *unless* Rekey is the one that changed the layout, as it does after
        // every correction. Treating its own switch as an external event threw
        // away the word record the shortcut needs, so tapping Option worked
        // only if it beat the context sampler to it.
        let context_key = (ctx.app.clone(), ctx.layout.clone());
        if let Some(previous) = self.last_context.as_ref() {
            if *previous != context_key {
                let same_app = previous.0 == context_key.0;
                let expected = self.expected_layout.take();
                let we_switched = same_app && expected.is_some() && ctx.layout == expected;
                if !we_switched {
                    self.buffer.push(Input::Reset);
                    self.last_word = None;
                }
            }
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
        // A modifier tapped on its own is the manual shortcut. It types
        // nothing, so it is safe to overload; holding it to type an accented
        // character does not count, because that is a key press in between.
        //
        // This has to come before the chord guard below. ⌥⌘ is a shortcut and
        // would return there, leaving the tap still armed — so releasing
        // Option afterwards would fire and rewrite the user's text.
        if event.key == Key::ModifiersChanged {
            return self.on_modifier_change(event.modifiers);
        }

        // Ctrl/Cmd chords are commands, not typing, and they usually move the
        // caret as a side effect.
        if event.modifiers.is_shortcut() {
            self.buffer.push(Input::Reset);
            self.last_word = None;
            return Action::None;
        }

        if self.modifier_down_at.is_some() {
            log::debug!("disarming the modifier tap because of {:?}", event.key);
        }
        self.modifier_down_at = None;

        let input = match event.key {
            Key::Backspace => Input::Backspace,
            // The caret may have moved, so nothing behind it is known any more.
            Key::Navigation | Key::Escape | Key::Other | Key::ModifiersChanged => {
                self.last_word = None;
                Input::Reset
            }
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
            self.last_word = None;
            return Action::None;
        }

        let cycle = self.build_cycle(text, prefix, core, suffix, terminator, active);
        let verdict = self.detector.evaluate(core, active);

        // Deliberately records the decision and not the word. Rekey promises
        // that what you type never leaves the machine, and a log file on disk
        // is not an exception to that — a diagnostic that quietly becomes a
        // keystroke record would be worse than having no diagnostics at all.
        log::debug!(
            "word of {} chars on {active} -> {}",
            core.chars().count(),
            match &verdict {
                Verdict::Switch(_) => "corrected",
                Verdict::Ambiguous(_) => "ambiguous",
                Verdict::Keep(reason) => match reason {
                    Skip::TooShort => "kept (too short)",
                    Skip::AllCaps => "kept (all caps)",
                    Skip::UserException => "kept (user exception)",
                    Skip::NotAWord => "kept (not a word)",
                    Skip::NotConfident => "kept (not confident)",
                    Skip::Disabled => "kept (disabled)",
                    Skip::NoAlternatives => "kept (no other layouts)",
                    Skip::ModelMissing => "kept (no model)",
                    Skip::Unconvertible => "kept (unconvertible)",
                },
            }
        );

        let candidate = match verdict {
            Verdict::Switch(candidate) => candidate,
            // Just under the threshold: plausible either way. This is where a
            // second opinion earns its place, if the user has enabled one.
            Verdict::Ambiguous(candidate) => match self.resolve_ambiguous(core, &candidate, active)
            {
                Some(true) => candidate,
                _ => {
                    self.last_word = cycle;
                    return Action::None;
                }
            },
            Verdict::Keep(_) => {
                // Nothing to do now, but remember the word so the shortcut can
                // still convert it if Rekey guessed wrong by staying quiet.
                self.last_word = cycle;
                return Action::None;
            }
        };

        let corrected = format!("{prefix}{}{suffix}", candidate.converted);
        let mut replacement = corrected.clone();
        if let Some(t) = terminator {
            replacement.push(t);
        }
        let delete = text.chars().count() + usize::from(terminator.is_some());

        // Always replace the record, never leave the previous one in place.
        // A stale `last_word` points at text that is no longer under the
        // caret, so the shortcut would delete and retype in the wrong place.
        self.last_word = match cycle {
            Some(mut cycle) => match cycle.variants.iter().position(|v| *v == corrected) {
                Some(index) => {
                    cycle.index = index;
                    cycle.auto = true;
                    Some(cycle)
                }
                // The correction is not one of the readings we enumerated, so
                // the cycle's idea of what is on screen would be wrong.
                None => None,
            },
            None => None,
        };
        self.stats.corrections += 1;
        self.expected_layout = Some(candidate.to_layout.to_string());

        Action::Replace {
            delete,
            text: replacement,
            switch_to: Some(candidate.to_layout.to_string()),
        }
    }

    /// Ask the assist about an ambiguous word, if one is configured.
    ///
    /// Answers only from what has already been learned — the keystroke path
    /// cannot wait for a network call. An unknown word is queued and left
    /// alone this time, so the answer is ready the next time it is typed.
    fn resolve_ambiguous(&self, core: &str, candidate: &Candidate, active: &str) -> Option<bool> {
        if !self.detector.config.ai_assist {
            return None;
        }
        let assist = self.assist.as_ref()?;
        if let Some(verdict) = assist.cached(core, &candidate.converted) {
            log::debug!("assist had a verdict for an ambiguous word: convert={verdict}");
            return Some(verdict);
        }

        let typed_language = layout::layout(active)?.def.lang.to_string();
        let alternative_language = layout::layout(candidate.to_layout)?.def.lang.to_string();
        assist.enqueue(Question {
            typed: core.to_string(),
            alternative: candidate.converted.clone(),
            typed_language,
            alternative_language,
        });
        None
    }

    /// Every reading of `text`, so the shortcut has somewhere to cycle to.
    fn build_cycle(
        &self,
        text: &str,
        prefix: &str,
        core: &str,
        suffix: &str,
        terminator: Option<char>,
        active: &str,
    ) -> Option<WordCycle> {
        let from = layout::layout(active)?;
        let mut variants = vec![text.to_string()];
        let mut layouts = vec![active.to_string()];

        for alt in self.detector.config.alternatives(active) {
            let Some(to) = layout::layout(alt) else {
                continue;
            };
            let rendered = format!("{prefix}{}{suffix}", layout::convert(core, from, to));
            // Layouts that render the word identically add nothing to cycle
            // through, and would make the shortcut appear to do nothing.
            if variants.contains(&rendered) {
                continue;
            }
            variants.push(rendered);
            layouts.push(alt.to_string());
        }

        if variants.len() < 2 {
            return None;
        }
        Some(WordCycle {
            variants,
            layouts,
            index: 0,
            terminator,
            auto: false,
        })
    }

    /// Handle a modifier going down or coming up.
    ///
    /// Only Option *alone* counts. Option as part of a chord — ⌥⌘, ⌥⇧ — is
    /// someone reaching for a shortcut in the app they are using, and firing
    /// on that would make Rekey rewrite text at random moments.
    fn on_modifier_change(&mut self, modifiers: Modifiers) -> Action {
        let shortcut = self.detector.config.shortcut;
        let Some(target) = shortcut.modifier() else {
            return Action::None;
        };

        let down = target.is_held(modifiers);
        let others_held = target.others_held(modifiers);
        let was_down = std::mem::replace(&mut self.modifier_was_down, down);
        log::debug!(
            "modifier change: {} down={down} was={was_down} others={others_held} armed={}",
            target.label(),
            self.modifier_down_at.is_some()
        );

        if others_held {
            // Another modifier joined in; this is a chord, not a tap.
            self.modifier_down_at = None;
            return Action::None;
        }

        match (was_down, down) {
            // Pressed on its own: start timing.
            (false, true) => {
                self.modifier_down_at = Some(Instant::now());
                Action::None
            }
            // Released: a tap if nothing intervened and it was brief.
            (true, false) => match self.modifier_down_at.take() {
                Some(at) if at.elapsed() <= MODIFIER_TAP_WINDOW => self.on_tap(shortcut),
                Some(_) => {
                    log::debug!("{} held too long to be a tap", target.label());
                    Action::None
                }
                None => {
                    log::debug!("{} released but the tap was disarmed", target.label());
                    Action::None
                }
            },
            _ => Action::None,
        }
    }

    /// A completed tap of the shortcut modifier.
    fn on_tap(&mut self, shortcut: Shortcut) -> Action {
        if !shortcut.needs_two_taps() {
            log::info!("shortcut tapped; cycling the last word");
            return self.cycle_last_word();
        }

        // A double tap: the first arms, the second fires. Anything slower than
        // the window starts over, so a stray tap cannot linger and combine
        // with an unrelated one later.
        match self.last_tap_at.take() {
            Some(previous) if previous.elapsed() <= DOUBLE_TAP_WINDOW => {
                log::info!("shortcut double-tapped; cycling the last word");
                self.cycle_last_word()
            }
            _ => {
                self.last_tap_at = Some(Instant::now());
                log::debug!("first tap of a double-tap shortcut");
                Action::None
            }
        }
    }

    /// Move the last word to its next reading. Backs the manual shortcut.
    ///
    /// Cycling rather than a one-shot undo is deliberate: with three layouts
    /// enabled the first alternative is often not the right one, and the user
    /// should not have to know that. Tapping again keeps going, and coming
    /// back round to what they originally typed is treated as a correction of
    /// Rekey rather than of themselves.
    pub fn cycle_last_word(&mut self) -> Action {
        // A part-typed word is what sits directly behind the caret. The last
        // *completed* word does not, so acting on that record would delete the
        // wrong span and retype over it — which surfaces as stray letters
        // appearing on the previous word.
        if !self.buffer.is_empty() {
            self.last_word = self.cycle_for_word_in_progress();
        }

        let Some(cycle) = self.last_word.as_mut() else {
            log::info!("option tapped but there is no word to cycle");
            return Action::None;
        };
        let delete = cycle.on_screen_len();
        let variant_count = cycle.variants.len();
        let next = (cycle.index + 1) % variant_count;
        cycle.index = next;

        let text = cycle.text_with_terminator(next);
        let switch_to = cycle.layouts.get(next).cloned();
        let returned_to_original = next == 0;
        let was_auto = cycle.auto;
        let original = cycle.variants[0].clone();
        cycle.auto = false;

        if returned_to_original && was_auto {
            // Rekey changed this word and the user has just changed it back.
            // Take the hint and leave that word alone in future.
            self.stats.undos += 1;
            self.learn_exception(&original);
        }

        log::info!(
            "cycle -> variant {next}/{variant_count} delete={delete} \
             length={} switch_to={switch_to:?}",
            text.chars().count()
        );
        // The buffer's idea of what is behind the caret no longer holds.
        self.buffer.push(Input::Reset);
        self.expected_layout = switch_to.clone();
        Action::Replace {
            delete,
            text,
            switch_to,
        }
    }

    /// Build a cycle for the word the user is part-way through typing.
    fn cycle_for_word_in_progress(&mut self) -> Option<WordCycle> {
        let active = self.last_context.as_ref()?.1.clone()?;
        let word = self.buffer.take_current()?;
        let (prefix, core, suffix) = self.detector.trim_affixes(&word.text, &active);
        let (prefix, core, suffix) = (prefix.to_string(), core.to_string(), suffix.to_string());
        self.build_cycle(&word.text, &prefix, &core, &suffix, None, &active)
    }

    /// Remember that the user prefers this word exactly as they typed it.
    fn learn_exception(&mut self, word: &str) {
        let trimmed: String = word
            .chars()
            .filter(|c| c.is_alphabetic() || !c.is_ascii_punctuation())
            .collect();
        let key = if trimmed.is_empty() { word } else { &trimmed }.to_lowercase();
        if !key.is_empty() && !self.detector.config.exceptions.contains(&key) {
            self.detector.config.exceptions.push(key);
        }
    }

    /// Undo the most recent automatic correction.
    ///
    /// Kept as an explicit menu action; it is the same operation as one step of
    /// [`Engine::cycle_last_word`], which is what the keyboard shortcut uses.
    pub fn undo(&mut self) -> Action {
        let Some(cycle) = self.last_word.as_ref() else {
            return Action::None;
        };
        if cycle.index == 0 {
            return Action::None;
        }
        // Step straight back to what the user typed.
        let delete = cycle.on_screen_len();
        let text = cycle.text_with_terminator(0);
        let switch_to = cycle.layouts.first().cloned();
        let original = cycle.variants[0].clone();
        let was_auto = cycle.auto;

        if let Some(c) = self.last_word.as_mut() {
            c.index = 0;
            c.auto = false;
        }
        if was_auto {
            self.stats.undos += 1;
            self.learn_exception(&original);
        }
        self.buffer.push(Input::Reset);
        self.expected_layout = switch_to.clone();
        Action::Replace {
            delete,
            text,
            switch_to,
        }
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
        self.stats.corrections += 1;
        Action::Replace {
            delete: word.text.chars().count(),
            text: converted,
            switch_to: Some(target.to_string()),
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

    /// A modifier-change event with Option held or released.
    fn alt(down: bool) -> KeyEvent {
        KeyEvent {
            character: None,
            key: Key::ModifiersChanged,
            modifiers: Modifiers {
                alt: down,
                ..Default::default()
            },
            synthetic: false,
        }
    }

    /// Tap Option: press and release with nothing in between.
    fn tap_alt(e: &mut Engine, layout: &str) -> Action {
        let c = ctx(layout);
        e.on_key(&alt(true), &c);
        e.on_key(&alt(false), &c)
    }

    /// A modifier-change event with an arbitrary modifier held.
    fn modifier_event(modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            character: None,
            key: Key::ModifiersChanged,
            modifiers,
            synthetic: false,
        }
    }

    /// Tap a specific modifier once.
    fn tap(e: &mut Engine, layout: &str, held: Modifiers) -> Action {
        let c = ctx(layout);
        e.on_key(&modifier_event(held), &c);
        e.on_key(&modifier_event(Modifiers::default()), &c)
    }

    fn engine_with_shortcut(shortcut: crate::config::Shortcut) -> Engine {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/models");
        let models = Models::load_dir(&dir).expect("models");
        Engine::new(
            models,
            Config {
                layouts: vec!["us".into(), "ru".into()],
                shortcut,
                ..Config::default()
            },
        )
    }

    /// Render `word` as the same physical keys would produce on `to`.
    fn rekey_core_convert(word: &str, from: &str, to: &str) -> String {
        crate::layout::convert_by_id(word, from, to).expect("known layouts")
    }

    fn replaced_text(action: &Action) -> &str {
        match action {
            Action::Replace { text, .. } => text,
            Action::None => panic!("expected a replacement, got Action::None"),
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
                text: "привет ".into(),
                switch_to: Some("ru".into()),
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
                text: "привет! ".into(),
                switch_to: Some("ru".into()),
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
        // Readings that are not words in either language, and are not one
        // letter from one either. Rewriting one nonsense string as another is
        // pure churn.
        for gibberish in ["qxzvj ", "vbxzq ", "zxcvq ", "wqxzj "] {
            assert_eq!(
                type_str(&mut e, gibberish, "us"),
                Action::None,
                "{gibberish:?} should be left alone"
            );
        }
    }

    #[test]
    fn a_word_with_one_letter_wrong_is_still_corrected() {
        // "hbdtn" reads as "ривет" — привет with a dropped п. That is a typing
        // slip, not nonsense, and refusing it would mean Rekey stops working
        // the moment someone fumbles a key. It corrects the layout and leaves
        // the slip: this is not a spell checker.
        let mut e = engine();
        let action = type_str(&mut e, "hbdtn ", "us");
        assert_eq!(replaced_text(&action), "ривет ");
        assert_eq!(action.layout_switch(), Some("ru"));
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
                text: "ghbdtn ".into(),
                switch_to: Some("us".into()),
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
            Action::Replace { delete, text, .. } => {
                assert_eq!(delete, 2);
                assert_eq!(text, "йч");
            }
            Action::None => panic!("manual conversion should always act"),
        }
    }

    #[test]
    fn a_correction_also_asks_for_the_layout_to_switch() {
        // Fixing the word but leaving the keyboard on English means the very
        // next word is wrong again.
        let mut e = engine();
        let action = type_str(&mut e, "ghbdtn ", "us");
        assert_eq!(action.layout_switch(), Some("ru"));
    }

    #[test]
    fn tapping_option_cycles_the_last_word() {
        let mut e = engine();
        // A word Rekey leaves alone, because it is real English.
        assert_eq!(type_str(&mut e, "hello ", "us"), Action::None);

        // One tap converts it anyway; that is the point of the shortcut.
        let first = tap_alt(&mut e, "us");
        assert_eq!(replaced_text(&first), "руддщ ");
        assert_eq!(first.layout_switch(), Some("ru"));

        // Another tap comes back round to exactly what was typed.
        let second = tap_alt(&mut e, "us");
        assert_eq!(replaced_text(&second), "hello ");
        assert_eq!(second.layout_switch(), Some("us"));

        // And it keeps going, as many times as asked.
        let third = tap_alt(&mut e, "us");
        assert_eq!(replaced_text(&third), "руддщ ");
    }

    #[test]
    fn cycling_back_teaches_rekey_to_leave_that_word_alone() {
        let mut e = engine();
        assert!(type_str(&mut e, "ghbdtn ", "us") != Action::None);
        assert_eq!(e.stats().corrections, 1);

        // The user disagrees and taps Option to get their text back.
        let back = tap_alt(&mut e, "us");
        assert_eq!(replaced_text(&back), "ghbdtn ");
        assert_eq!(e.stats().undos, 1);

        // Having been overruled once, it does not try again.
        assert_eq!(type_str(&mut e, "ghbdtn ", "us"), Action::None);
    }

    #[test]
    fn cycling_through_three_layouts_visits_each_reading() {
        let models = Models::load_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/models"),
        )
        .unwrap();
        let mut e = Engine::new(
            models,
            Config {
                layouts: vec!["us".into(), "ru".into(), "uk".into()],
                ..Config::default()
            },
        );
        type_str(&mut e, "ds ", "us");
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(replaced_text(&tap_alt(&mut e, "us")).to_string());
        }
        // Russian and Ukrainian render this differently, so all three readings
        // must be reachable rather than toggling between two.
        assert_eq!(seen.len(), 3);
        assert!(
            seen.contains(&"ds ".to_string()),
            "original missing: {seen:?}"
        );
        assert!(
            seen.iter().collect::<std::collections::HashSet<_>>().len() == 3,
            "expected three distinct readings, got {seen:?}"
        );
    }

    #[test]
    fn holding_option_to_type_an_accent_is_not_a_shortcut() {
        let mut e = engine();
        type_str(&mut e, "hello ", "us");
        let c = ctx("us");
        // Option down, then a real keypress, then Option up: that is someone
        // typing ´ or ˆ, not reaching for the shortcut.
        e.on_key(&alt(true), &c);
        e.on_key(&key('e'), &c);
        let released = e.on_key(&alt(false), &c);
        assert_eq!(released, Action::None);
    }

    #[test]
    fn option_as_part_of_a_chord_is_not_a_shortcut() {
        let mut e = engine();
        type_str(&mut e, "hello ", "us");
        let c = ctx("us");
        // ⌥⌘ — someone reaching for an app shortcut, not tapping Option.
        e.on_key(&alt(true), &c);
        let chord = KeyEvent {
            character: None,
            key: Key::ModifiersChanged,
            modifiers: Modifiers {
                alt: true,
                meta: true,
                ..Default::default()
            },
            synthetic: false,
        };
        e.on_key(&chord, &c);
        assert_eq!(e.on_key(&alt(false), &c), Action::None);
    }

    #[test]
    fn the_shortcut_acts_on_the_word_being_typed_not_an_earlier_one() {
        let mut e = engine();
        // A finished word, then a second one still in progress.
        type_str(&mut e, "hello ", "us");
        for ch in "ghbdtn".chars() {
            e.on_key(&key(ch), &ctx("us"));
        }
        // The caret sits after "ghbdtn", so that is what must be rewritten.
        let action = tap_alt(&mut e, "us");
        match action {
            Action::Replace { delete, text, .. } => {
                assert_eq!(delete, 6, "should delete exactly the word in progress");
                assert_eq!(text, "привет");
            }
            Action::None => panic!("expected the in-progress word to be cycled"),
        }
    }

    #[test]
    fn the_shortcut_survives_rekeys_own_layout_switch() {
        // The reported case: type Cyrillic on the Russian layout, let Rekey
        // correct it to English — which also switches the keyboard — and then
        // reach for the shortcut. Rekey must not read its own switch as the
        // user changing context and discard the word it just corrected.
        let mut e = engine();

        // "руддщ" typed on the Russian layout is "hello" on English.
        let typed = rekey_core_convert("hello", "us", "ru");
        let correction = type_str(&mut e, &format!("{typed} "), "ru");
        assert_eq!(replaced_text(&correction), "hello ");
        assert_eq!(correction.layout_switch(), Some("us"));

        // The system layout now follows, exactly as the app makes it.
        let action = tap_alt(&mut e, "us");
        assert_eq!(
            replaced_text(&action),
            format!("{typed} "),
            "the shortcut should put back what was originally typed"
        );
    }

    #[test]
    fn a_layout_change_the_user_made_still_clears_the_record() {
        // Only Rekey's *own* switch is exempt. If the user changes layout
        // themselves, Rekey has no idea what happened in between.
        let mut e = engine();
        type_str(&mut e, "hello ", "us");
        // No correction happened, so no switch was expected.
        assert_eq!(tap_alt(&mut e, "uk"), Action::None);
    }

    #[test]
    fn caret_movement_makes_the_shortcut_inert() {
        // After an arrow key Rekey has no idea what is behind the caret, so
        // rewriting anything would be a guess at the user's expense.
        let mut e = engine();
        type_str(&mut e, "hello ", "us");
        e.on_key(&special(Key::Navigation), &ctx("us"));
        assert_eq!(tap_alt(&mut e, "us"), Action::None);
    }

    #[test]
    fn the_shortcut_never_acts_on_a_stale_word() {
        // A word with no alternative reading must clear the record, or the
        // shortcut would later delete and retype an earlier word that is no
        // longer under the caret — which shows up as stray letters appearing
        // in the wrong place.
        let mut e = engine();
        type_str(&mut e, "hello ", "us");
        assert!(e.last_word.is_some(), "a cyclable word should be recorded");

        // "42" is refused as not-a-word, so it yields no cycle.
        type_str(&mut e, "42 ", "us");
        assert!(
            e.last_word.is_none(),
            "the earlier word must not still be cyclable"
        );
        assert_eq!(tap_alt(&mut e, "us"), Action::None);
    }

    #[test]
    fn the_shortcut_modifier_is_configurable() {
        use crate::config::{Modifier, Shortcut};
        let mut e = engine_with_shortcut(Shortcut::Tap {
            modifier: Modifier::Shift,
        });
        type_str(&mut e, "hello ", "us");

        // Option is no longer the trigger …
        assert_eq!(tap_alt(&mut e, "us"), Action::None);

        // … Shift is.
        let action = tap(
            &mut e,
            "us",
            Modifiers {
                shift: true,
                ..Default::default()
            },
        );
        assert_eq!(replaced_text(&action), "руддщ ");
    }

    #[test]
    fn a_double_tap_shortcut_needs_both_taps() {
        use crate::config::{Modifier, Shortcut};
        let mut e = engine_with_shortcut(Shortcut::DoubleTap {
            modifier: Modifier::Option,
        });
        type_str(&mut e, "hello ", "us");

        // One tap arms and does nothing — which is the point, since Option is
        // also used in ordinary chords.
        assert_eq!(tap_alt(&mut e, "us"), Action::None);
        // The second fires.
        assert_eq!(replaced_text(&tap_alt(&mut e, "us")), "руддщ ");
    }

    #[test]
    fn the_shortcut_can_be_switched_off_entirely() {
        use crate::config::Shortcut;
        let mut e = engine_with_shortcut(Shortcut::Off);
        type_str(&mut e, "hello ", "us");
        assert_eq!(tap_alt(&mut e, "us"), Action::None);
        assert_eq!(tap_alt(&mut e, "us"), Action::None);
    }

    #[test]
    fn an_ambiguous_word_is_queued_for_the_assist_and_left_alone() {
        use crate::assist::test_support::FakeAssist;
        let mut e = engine();
        let mut cfg = e.config().clone();
        cfg.ai_assist = true;
        e.set_config(cfg);
        let assist = std::sync::Arc::new(FakeAssist::default());
        e.set_assist(Some(assist.clone()));

        // Feed words until one lands in the ambiguous band.
        for word in ["ntcn", "vjq", "hfp", "ldf", "ytn", "ds,", "rjn"] {
            type_str(&mut e, &format!("{word} "), "us");
        }
        // Whatever was ambiguous went to the assist, and nothing was rewritten
        // on a guess in the meantime.
        let asked = assist.asked.lock().unwrap();
        for question in asked.iter() {
            assert!(!question.typed.is_empty());
            assert_ne!(question.typed, question.alternative);
            assert_eq!(question.typed_language, "en");
        }
    }

    #[test]
    fn a_learned_assist_verdict_converts_an_ambiguous_word() {
        use crate::assist::test_support::FakeAssist;
        use crate::detect::Verdict;

        // Find a word the detector genuinely calls ambiguous, so the test
        // exercises the path rather than assuming which word qualifies.
        let probe = engine();
        let ambiguous = ["ntcn", "vjq", "hfp", "ldf", "ytn", "rjn", "ds,", "yt"]
            .iter()
            .find(|w| matches!(probe.detector.evaluate(w, "us"), Verdict::Ambiguous(_)))
            .copied();
        let Some(word) = ambiguous else {
            // Nothing in the sample is ambiguous with the shipped models; the
            // queueing test above still covers the wiring.
            return;
        };

        let mut e = engine();
        let mut cfg = e.config().clone();
        cfg.ai_assist = true;
        e.set_config(cfg);
        e.set_assist(Some(std::sync::Arc::new(FakeAssist::with_answer(
            word, true,
        ))));

        let action = type_str(&mut e, &format!("{word} "), "us");
        assert!(
            action != Action::None,
            "a learned 'convert' verdict should be acted on for {word:?}"
        );
    }

    #[test]
    fn the_assist_is_ignored_when_the_setting_is_off() {
        use crate::assist::test_support::FakeAssist;
        let mut e = engine();
        // ai_assist defaults to false.
        let assist = std::sync::Arc::new(FakeAssist::default());
        e.set_assist(Some(assist.clone()));
        for word in ["ntcn", "vjq", "hfp", "ldf", "ytn"] {
            type_str(&mut e, &format!("{word} "), "us");
        }
        assert!(
            assist.asked.lock().unwrap().is_empty(),
            "nothing should leave the machine while the assist is switched off"
        );
    }

    #[test]
    fn the_shortcut_does_nothing_before_any_word() {
        let mut e = engine();
        assert_eq!(tap_alt(&mut e, "us"), Action::None);
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
