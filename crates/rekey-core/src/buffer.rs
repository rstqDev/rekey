//! Tracks the word currently being typed.
//!
//! The hook feeds this every keystroke; it decides when a word has ended and
//! how much text would have to be deleted to replace it. It is deliberately
//! conservative about what counts as "still the same word": anything that could
//! mean the caret moved — a click, an arrow key, a shortcut, a focus change —
//! throws the buffer away, because replacing text based on a stale position is
//! how a tool like this corrupts a document.

/// What the user did, as far as the buffer cares.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// A printable character was typed.
    Char(char),
    Backspace,
    /// Space, tab, enter — ends a word and keeps typing flowing.
    Boundary(char),
    /// Caret may have moved: arrows, Home/End, page keys, a mouse click, a
    /// shortcut, or the frontmost app changing.
    Reset,
}

/// A word that just ended and is worth checking.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    /// The word itself, without the character that ended it.
    pub text: String,
    /// The boundary character that ended it, if any. `None` when the word was
    /// force-flushed, e.g. by the manual hotkey.
    pub terminator: Option<char>,
}

impl Word {
    /// Characters that must be deleted to remove the word from the document.
    /// The terminator is included because it was typed after the word and has
    /// to come back afterwards.
    pub fn chars_to_delete(&self) -> usize {
        self.text.chars().count() + usize::from(self.terminator.is_some())
    }
}

/// The longest word the buffer will accumulate. Beyond this something is wrong
/// — a paste, a stream of generated text — and it is safer to stop tracking
/// than to attempt a huge replacement.
const MAX_WORD: usize = 64;

#[derive(Debug, Default)]
pub struct Buffer {
    current: String,
    /// Set when the buffer stopped trusting its position. Cleared on the next
    /// boundary, so tracking resumes at a known-good point.
    poisoned: bool,
}

impl Buffer {
    pub fn new() -> Buffer {
        Buffer::default()
    }

    pub fn current(&self) -> &str {
        &self.current
    }

    pub fn is_empty(&self) -> bool {
        self.current.is_empty()
    }

    /// Feed one input. Returns a word when one just ended and is safe to act on.
    pub fn push(&mut self, input: Input) -> Option<Word> {
        match input {
            Input::Char(c) => {
                if self.current.chars().count() >= MAX_WORD {
                    self.poisoned = true;
                }
                self.current.push(c);
                None
            }
            Input::Backspace => {
                // A backspace that empties the buffer may be eating text we
                // never saw, so stop trusting the position.
                if self.current.pop().is_none() {
                    self.poisoned = true;
                }
                None
            }
            Input::Boundary(c) => {
                let word = std::mem::take(&mut self.current);
                let was_poisoned = std::mem::replace(&mut self.poisoned, false);
                if was_poisoned || word.is_empty() {
                    return None;
                }
                Some(Word {
                    text: word,
                    terminator: Some(c),
                })
            }
            Input::Reset => {
                self.current.clear();
                self.poisoned = false;
                None
            }
        }
    }

    /// Take the word in progress without waiting for a boundary. Backs the
    /// manual "fix what I just typed" hotkey.
    pub fn take_current(&mut self) -> Option<Word> {
        if self.current.is_empty() || self.poisoned {
            return None;
        }
        Some(Word {
            text: std::mem::take(&mut self.current),
            terminator: None,
        })
    }
}

/// The last automatic correction, kept so the user can undo it.
///
/// Undo is not a nicety. A switcher that occasionally guesses wrong is only
/// tolerable if taking the guess back is one keystroke, and remembering the
/// exact original is what makes that possible.
#[derive(Debug, Clone, PartialEq)]
pub struct LastCorrection {
    pub original: String,
    pub corrected: String,
    pub terminator: Option<char>,
}

impl LastCorrection {
    /// Characters to delete to undo: the correction plus its terminator.
    pub fn chars_to_delete(&self) -> usize {
        self.corrected.chars().count() + usize::from(self.terminator.is_some())
    }

    /// Text to type to restore what the user originally wrote.
    pub fn restore_text(&self) -> String {
        let mut s = self.original.clone();
        if let Some(t) = self.terminator {
            s.push(t);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_str(b: &mut Buffer, s: &str) -> Vec<Word> {
        let mut out = Vec::new();
        for c in s.chars() {
            let input = if c.is_whitespace() {
                Input::Boundary(c)
            } else {
                Input::Char(c)
            };
            if let Some(w) = b.push(input) {
                out.push(w);
            }
        }
        out
    }

    #[test]
    fn emits_words_at_boundaries() {
        let mut b = Buffer::new();
        let words = type_str(&mut b, "hello world ");
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "hello");
        assert_eq!(words[0].terminator, Some(' '));
        assert_eq!(words[1].text, "world");
    }

    #[test]
    fn word_in_progress_is_not_emitted() {
        let mut b = Buffer::new();
        let words = type_str(&mut b, "hello wor");
        assert_eq!(words.len(), 1);
        assert_eq!(b.current(), "wor");
    }

    #[test]
    fn backspace_edits_the_current_word() {
        let mut b = Buffer::new();
        type_str(&mut b, "helxo");
        b.push(Input::Backspace);
        b.push(Input::Backspace);
        b.push(Input::Char('l'));
        b.push(Input::Char('o'));
        let w = b.push(Input::Boundary(' ')).expect("word");
        assert_eq!(w.text, "hello");
    }

    #[test]
    fn backspacing_past_the_start_stops_trusting_the_position() {
        let mut b = Buffer::new();
        // The user backspaced into text the hook never observed.
        b.push(Input::Backspace);
        b.push(Input::Char('x'));
        assert_eq!(b.push(Input::Boundary(' ')), None);
        // …and tracking resumes cleanly afterwards.
        let w = type_str(&mut b, "hello ");
        assert_eq!(w[0].text, "hello");
    }

    #[test]
    fn caret_movement_discards_the_word() {
        let mut b = Buffer::new();
        type_str(&mut b, "hel");
        b.push(Input::Reset);
        assert!(b.is_empty());
        assert_eq!(b.push(Input::Boundary(' ')), None);
    }

    #[test]
    fn repeated_boundaries_emit_nothing() {
        let mut b = Buffer::new();
        let words = type_str(&mut b, "hi   ");
        assert_eq!(words.len(), 1);
    }

    #[test]
    fn very_long_runs_are_abandoned() {
        let mut b = Buffer::new();
        let long: String = std::iter::repeat_n('a', MAX_WORD + 5).collect();
        type_str(&mut b, &long);
        assert_eq!(b.push(Input::Boundary(' ')), None);
    }

    #[test]
    fn deletion_count_includes_the_terminator() {
        let w = Word {
            text: "ghbdtn".into(),
            terminator: Some(' '),
        };
        assert_eq!(w.chars_to_delete(), 7);
        let w = Word {
            text: "ghbdtn".into(),
            terminator: None,
        };
        assert_eq!(w.chars_to_delete(), 6);
    }

    #[test]
    fn deletion_count_is_in_characters_not_bytes() {
        // Cyrillic is two bytes per character; deleting by byte count would
        // eat twice as much text as it should.
        let c = LastCorrection {
            original: "ghbdtn".into(),
            corrected: "привет".into(),
            terminator: Some(' '),
        };
        assert_eq!(c.chars_to_delete(), 7);
        assert_eq!(c.restore_text(), "ghbdtn ");
    }

    #[test]
    fn manual_flush_takes_the_word_in_progress() {
        let mut b = Buffer::new();
        type_str(&mut b, "ghbdt");
        let w = b.take_current().expect("word");
        assert_eq!(w.text, "ghbdt");
        assert_eq!(w.terminator, None);
        assert!(b.is_empty());
    }
}
