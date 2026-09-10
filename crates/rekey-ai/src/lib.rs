//! Optional Claude assist for the words the local models cannot settle.
//!
//! # Why this is a learner, not a filter
//!
//! A network round trip is three orders of magnitude slower than the local
//! decision, so this deliberately does **not** sit in the keystroke path. The
//! local engine always decides in the moment. Ambiguous words are queued, asked
//! about in the background, and the answer is remembered — so the *next* time
//! that word appears, the local path already knows and answers instantly.
//!
//! # Privacy
//!
//! This is off by default and must be switched on explicitly. When it is on,
//! only single ambiguous words are sent — never whole sentences, never
//! anything containing digits or symbols, and never anything from an excluded
//! app or a password field, because those never reach the detector at all. The
//! app says so plainly in its settings, and the verdict cache means a given
//! word is sent at most once.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

/// Default model. Deliberately a capable one: this runs in the background
/// where a few hundred milliseconds cost nothing, and a wrong answer here is
/// cached and reused, so accuracy matters more than latency.
pub const DEFAULT_MODEL: &str = "claude-opus-5";

/// Classification needs very few tokens.
const MAX_TOKENS: u32 = 256;

const SYSTEM_PROMPT: &str = "\
You settle ambiguous keyboard-layout corrections.

A user typed a word while the wrong keyboard layout was active. The local \
model could not decide whether it was a genuine mistake or intentional text.

You are given the word as typed and the reading it would have on the other \
layout. Decide which the user actually meant.

Answer with exactly one word:
- CONVERT if the alternative reading is what the user meant
- KEEP if the text as typed is what the user meant

Consider slang, names, brand names, deliberate transliteration, and technical \
jargon. When genuinely uncertain, answer KEEP: leaving text alone is always \
recoverable, silently rewriting it is not.";

/// What the assist concluded about one word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Convert,
    Keep,
}

/// One question for the model.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Question {
    /// The word exactly as the user typed it.
    pub typed: String,
    /// What it reads as on the candidate layout.
    pub alternative: String,
    pub typed_language: String,
    pub alternative_language: String,
}

impl Question {
    fn prompt(&self) -> String {
        format!(
            "Typed: {}\nLanguage of the active layout: {}\n\
             Alternative reading: {}\nLanguage of the other layout: {}\n\n\
             CONVERT or KEEP?",
            self.typed, self.typed_language, self.alternative, self.alternative_language
        )
    }

    /// Cache key. Case-insensitive, since the verdict does not depend on case.
    fn key(&self) -> String {
        format!(
            "{}|{}",
            self.typed.to_lowercase(),
            self.alternative.to_lowercase()
        )
    }
}

#[derive(Debug)]
pub enum AiError {
    NoApiKey,
    Http(String),
    /// The API answered, but not with something we can act on.
    Unparseable(String),
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiError::NoApiKey => write!(f, "no Anthropic API key configured"),
            AiError::Http(e) => write!(f, "request failed: {e}"),
            AiError::Unparseable(e) => write!(f, "unexpected response: {e}"),
        }
    }
}

impl std::error::Error for AiError {}

/// Verdicts learned so far, shared with the engine so the local path can use
/// them without waiting for anything.
#[derive(Debug, Default, Clone)]
pub struct VerdictCache {
    inner: Arc<Mutex<HashMap<String, Verdict>>>,
}

impl VerdictCache {
    pub fn new() -> VerdictCache {
        VerdictCache::default()
    }

    pub fn get(&self, question: &Question) -> Option<Verdict> {
        self.inner.lock().ok()?.get(&question.key()).copied()
    }

    pub fn insert(&self, question: &Question, verdict: Verdict) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(question.key(), verdict);
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Configuration for the assist.
#[derive(Debug, Clone)]
pub struct AiConfig {
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
}

impl AiConfig {
    pub fn new(api_key: impl Into<String>) -> AiConfig {
        AiConfig {
            api_key: api_key.into(),
            model: DEFAULT_MODEL.to_string(),
            timeout: Duration::from_secs(30),
        }
    }
}

/// Ask Claude about one word. Blocking; callers run it off the typing path.
pub fn ask(config: &AiConfig, question: &Question) -> Result<Verdict, AiError> {
    if config.api_key.trim().is_empty() {
        return Err(AiError::NoApiKey);
    }

    let body = serde_json::json!({
        "model": config.model,
        "max_tokens": MAX_TOKENS,
        // A one-word classification does not need deep reasoning, and low
        // effort keeps both latency and cost down. Thinking is left at its
        // default rather than disabled, which is the documented guidance.
        "output_config": { "effort": "low" },
        "system": SYSTEM_PROMPT,
        "messages": [{ "role": "user", "content": question.prompt() }],
    });

    let response = ureq::post(API_URL)
        .header("x-api-key", &config.api_key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .config()
        .timeout_global(Some(config.timeout))
        .build()
        .send_json(&body)
        .map_err(|e| AiError::Http(e.to_string()))?
        .body_mut()
        .read_json::<serde_json::Value>()
        .map_err(|e| AiError::Unparseable(e.to_string()))?;

    parse_verdict(&response)
}

/// Pull the verdict out of a Messages API response.
fn parse_verdict(response: &serde_json::Value) -> Result<Verdict, AiError> {
    // A safety decline is a valid outcome, not an error: leave the text alone.
    if response.get("stop_reason").and_then(|s| s.as_str()) == Some("refusal") {
        return Ok(Verdict::Keep);
    }

    let text = response
        .get("content")
        .and_then(|c| c.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .ok_or_else(|| AiError::Unparseable("no content blocks in response".into()))?;

    let upper = text.to_uppercase();
    // Checked in this order so a reply that mentions both still resolves to the
    // conservative answer unless CONVERT clearly leads.
    match (upper.find("CONVERT"), upper.find("KEEP")) {
        (Some(c), Some(k)) => Ok(if c < k {
            Verdict::Convert
        } else {
            Verdict::Keep
        }),
        (Some(_), None) => Ok(Verdict::Convert),
        (None, Some(_)) => Ok(Verdict::Keep),
        (None, None) => Err(AiError::Unparseable(format!(
            "expected CONVERT or KEEP, got {text:?}"
        ))),
    }
}

/// Background worker that answers questions without blocking typing.
pub struct Assist {
    tx: Sender<Question>,
    cache: VerdictCache,
}

impl Assist {
    /// Start the worker thread. It exits when the sender is dropped.
    pub fn start(config: AiConfig) -> Assist {
        let (tx, rx): (Sender<Question>, Receiver<Question>) = mpsc::channel();
        let cache = VerdictCache::new();
        let worker_cache = cache.clone();

        std::thread::Builder::new()
            .name("rekey-ai".into())
            .spawn(move || {
                for question in rx {
                    if worker_cache.get(&question).is_some() {
                        continue;
                    }
                    match ask(&config, &question) {
                        Ok(verdict) => {
                            log::debug!("ai: {:?} -> {verdict:?}", question.typed);
                            worker_cache.insert(&question, verdict);
                        }
                        Err(e) => log::warn!("ai assist failed for {:?}: {e}", question.typed),
                    }
                }
            })
            .expect("spawn ai worker");

        Assist { tx, cache }
    }

    /// The verdict for this word if it has already been learned.
    pub fn cached(&self, question: &Question) -> Option<Verdict> {
        self.cache.get(question)
    }

    /// Queue a word to be asked about. Returns immediately; never blocks the
    /// keystroke path even if the worker is busy or the network is down.
    pub fn enqueue(&self, question: Question) {
        if self.cache.get(&question).is_some() {
            return;
        }
        let _ = self.tx.send(question);
    }

    pub fn learned(&self) -> usize {
        self.cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q() -> Question {
        Question {
            typed: "ghbdtn".into(),
            alternative: "привет".into(),
            typed_language: "en".into(),
            alternative_language: "ru".into(),
        }
    }

    #[test]
    fn parses_a_plain_convert() {
        let r = serde_json::json!({
            "content": [{"type": "text", "text": "CONVERT"}],
            "stop_reason": "end_turn"
        });
        assert_eq!(parse_verdict(&r).unwrap(), Verdict::Convert);
    }

    #[test]
    fn parses_a_plain_keep() {
        let r = serde_json::json!({
            "content": [{"type": "text", "text": "KEEP"}],
            "stop_reason": "end_turn"
        });
        assert_eq!(parse_verdict(&r).unwrap(), Verdict::Keep);
    }

    #[test]
    fn tolerates_surrounding_prose() {
        let r = serde_json::json!({
            "content": [{"type": "text", "text": "This is Russian typed on QWERTY. CONVERT"}],
            "stop_reason": "end_turn"
        });
        assert_eq!(parse_verdict(&r).unwrap(), Verdict::Convert);
    }

    #[test]
    fn ignores_thinking_blocks() {
        let r = serde_json::json!({
            "content": [
                {"type": "thinking", "thinking": "KEEP might apply here but no"},
                {"type": "text", "text": "CONVERT"}
            ],
            "stop_reason": "end_turn"
        });
        assert_eq!(parse_verdict(&r).unwrap(), Verdict::Convert);
    }

    #[test]
    fn a_refusal_leaves_the_text_alone() {
        let r = serde_json::json!({
            "content": [],
            "stop_reason": "refusal",
            "stop_details": {"type": "refusal", "category": "other"}
        });
        assert_eq!(parse_verdict(&r).unwrap(), Verdict::Keep);
    }

    #[test]
    fn an_unusable_answer_is_an_error_not_a_guess() {
        let r = serde_json::json!({
            "content": [{"type": "text", "text": "I am not sure about this one"}],
            "stop_reason": "end_turn"
        });
        assert!(parse_verdict(&r).is_err());
    }

    #[test]
    fn missing_api_key_is_refused_before_any_request() {
        let config = AiConfig::new("   ");
        assert!(matches!(ask(&config, &q()), Err(AiError::NoApiKey)));
    }

    #[test]
    fn cache_is_case_insensitive() {
        let cache = VerdictCache::new();
        cache.insert(&q(), Verdict::Convert);
        let upper = Question {
            typed: "GHBDTN".into(),
            alternative: "ПРИВЕТ".into(),
            ..q()
        };
        assert_eq!(cache.get(&upper), Some(Verdict::Convert));
    }

    #[test]
    fn prompt_includes_both_readings_and_languages() {
        let p = q().prompt();
        assert!(p.contains("ghbdtn"));
        assert!(p.contains("привет"));
        assert!(p.contains("en"));
        assert!(p.contains("ru"));
    }
}
