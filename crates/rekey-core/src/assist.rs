//! The seam between the local engine and an optional second opinion.
//!
//! The engine settles almost everything on its own. What it cannot settle are
//! the words that sit just under the threshold — plausible in both readings,
//! usually new slang or a name. Those are the cases where asking something
//! with more context genuinely helps.
//!
//! This is a trait rather than a direct dependency so that `rekey-core` stays
//! what it claims to be: pure computation, no network, no OS. The application
//! supplies an implementation; with the assist switched off there is none, and
//! the engine behaves exactly as if this module did not exist.

/// A second opinion on words the local models find ambiguous.
///
/// Implementations must never block: the engine calls these from the keystroke
/// path, where a network round trip is three orders of magnitude too slow.
/// [`Assist::cached`] answers from memory or not at all, and
/// [`Assist::enqueue`] hands the question to a background worker so the answer
/// is ready the *next* time that word appears.
pub trait Assist: Send + Sync {
    /// A verdict already learned for this pair: true to convert, false to keep.
    fn cached(&self, typed: &str, alternative: &str) -> Option<bool>;

    /// Ask about this pair in the background. Must return immediately.
    fn enqueue(&self, question: Question);

    /// How many verdicts have been learned so far.
    ///
    /// Surfaced in the settings window so the assist can be seen doing
    /// something, rather than being a switch with no observable effect.
    fn learned(&self) -> usize {
        0
    }
}

/// One ambiguous word, in both readings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub typed: String,
    pub alternative: String,
    pub typed_language: String,
    pub alternative_language: String,
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// An assist with pre-seeded answers, for testing the engine's use of it.
    #[derive(Default)]
    pub struct FakeAssist {
        pub answers: Mutex<HashMap<String, bool>>,
        pub asked: Mutex<Vec<Question>>,
    }

    impl FakeAssist {
        pub fn with_answer(typed: &str, convert: bool) -> FakeAssist {
            let assist = FakeAssist::default();
            assist
                .answers
                .lock()
                .unwrap()
                .insert(typed.to_lowercase(), convert);
            assist
        }
    }

    impl Assist for FakeAssist {
        fn cached(&self, typed: &str, _alternative: &str) -> Option<bool> {
            self.answers
                .lock()
                .unwrap()
                .get(&typed.to_lowercase())
                .copied()
        }

        fn enqueue(&self, question: Question) {
            self.asked.lock().unwrap().push(question);
        }

        fn learned(&self) -> usize {
            self.answers.lock().unwrap().len()
        }
    }
}
