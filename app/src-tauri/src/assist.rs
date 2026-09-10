//! Connects the engine's ambiguity seam to the Claude-backed assist.
//!
//! The engine knows nothing about networks; this adapter is where the two
//! meet. It is attached only while the setting is on *and* an API key exists,
//! so the default build never has anything to send.

use rekey_ai::{AiConfig, Question as AiQuestion, Verdict};
use rekey_core::assist::{Assist, Question};

pub struct ClaudeAssist {
    inner: rekey_ai::Assist,
}

impl ClaudeAssist {
    /// Start the background worker. Returns `None` without an API key, so the
    /// caller cannot accidentally attach an assist that can only fail.
    pub fn start(api_key: &str) -> Option<ClaudeAssist> {
        if api_key.trim().is_empty() {
            return None;
        }
        Some(ClaudeAssist {
            inner: rekey_ai::Assist::start(AiConfig::new(api_key)),
        })
    }
}

fn to_ai(question: Question) -> AiQuestion {
    AiQuestion {
        typed: question.typed,
        alternative: question.alternative,
        typed_language: question.typed_language,
        alternative_language: question.alternative_language,
    }
}

impl Assist for ClaudeAssist {
    fn cached(&self, typed: &str, alternative: &str) -> Option<bool> {
        let question = AiQuestion {
            typed: typed.to_string(),
            alternative: alternative.to_string(),
            // Only the two spellings identify a cached verdict; the language
            // tags are context for the question, not part of its identity.
            typed_language: String::new(),
            alternative_language: String::new(),
        };
        self.inner
            .cached(&question)
            .map(|verdict| verdict == Verdict::Convert)
    }

    fn enqueue(&self, question: Question) {
        self.inner.enqueue(to_ai(question));
    }

    fn learned(&self) -> usize {
        self.inner.learned()
    }
}
