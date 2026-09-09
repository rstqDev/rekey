//! The decision engine: given a word and the layout it was typed with, decide
//! whether the user meant it on a different layout.
//!
//! The mechanic is simple — re-type the word on every enabled layout and score
//! each reading — but almost all of the value is in the guards. A switcher that
//! is right 95% of the time is worse than useless, because the 5% lands in the
//! middle of a password field or a git command. Everything here is biased
//! toward doing nothing when the evidence is less than clear.

use crate::config::Config;
use crate::layout::{self, Layout};
use crate::model::Models;

/// A rejected or accepted alternative reading, with the reasoning attached so
/// the UI can explain itself instead of feeling like magic.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub to_layout: &'static str,
    pub converted: String,
    /// log10 score of the text as typed, in the active layout's language.
    pub score_as_typed: f32,
    /// log10 score of the converted reading, in the target layout's language.
    pub score_converted: f32,
    /// How much better the alternative is. Positive means "probably a mistake".
    pub delta: f32,
    pub target_knows_word: bool,
    pub source_knows_word: bool,
}

impl Candidate {
    /// Squash the score advantage into a 0..1 confidence for display.
    pub fn confidence(&self) -> f32 {
        1.0 / (1.0 + (-(self.delta - 1.5)).exp())
    }
}

/// Why the detector declined to act. Surfaced in the debug log, which is the
/// difference between "it randomly does nothing" and a tool people trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    Disabled,
    TooShort,
    AllCaps,
    UserException,
    /// Contains digits, punctuation or symbols: paths, handles, passwords.
    NotAWord,
    /// No other layouts configured.
    NoAlternatives,
    /// No trained model for one of the languages involved.
    ModelMissing,
    /// The alternative reading was not convincing enough.
    NotConfident,
    /// Conversion produced characters the target layout cannot type.
    Unconvertible,
}

#[derive(Debug, Clone)]
pub enum Verdict {
    /// Leave the text alone.
    Keep(Skip),
    /// Replace the typed text with `best.converted`.
    Switch(Box<Candidate>),
    /// Plausible but under threshold. With AI assist on, this is what gets
    /// escalated; otherwise it is treated as Keep.
    Ambiguous(Box<Candidate>),
}

impl Verdict {
    pub fn is_switch(&self) -> bool {
        matches!(self, Verdict::Switch(_))
    }

    pub fn candidate(&self) -> Option<&Candidate> {
        match self {
            Verdict::Switch(c) | Verdict::Ambiguous(c) => Some(c),
            Verdict::Keep(_) => None,
        }
    }
}

pub struct Detector {
    pub models: Models,
    pub config: Config,
}

/// How much of a word must be typeable on the target layout for the conversion
/// to be considered meaningful rather than mangling.
const MIN_COVERAGE: f32 = 0.99;

/// Band below the switch threshold that counts as "worth asking the AI about".
const AMBIGUOUS_BAND: f32 = 1.5;

/// Characters that mark a token as structure rather than prose — URLs, paths,
/// namespaced identifiers, Windows drive letters. None of them is reachable as
/// a letter on any layout Rekey ships, so rejecting them costs nothing.
const STRUCTURAL: &[char] = &['@', ':', '\\', '|', '"', '<', '>', '{', '}', '='];

/// A single `/` is allowed because it is the Arabic letter ظ on a US keyboard,
/// but two or more mean a path or URL.
const MAX_SLASHES: usize = 1;

/// log10 unigram probability above which a word counts as genuinely common.
///
/// The frequency lists have a long tail of subtitle noise — OCR errors, foreign
/// fragments, stray letter pairs — and treating those as "already a real word"
/// would hand them the full anti-clobbering surcharge and block legitimate
/// corrections. Real vocabulary sits comfortably above this line.
const COMMON_WORD_FLOOR: f32 = -5.5;

/// Fraction of the surcharge granted to a word that is in the corpus but below
/// the floor: some protection, since it might be a rare real word, but not
/// enough to veto strong evidence.
const RARE_WORD_PROTECTION: f32 = 0.4;

impl Detector {
    pub fn new(models: Models, config: Config) -> Detector {
        Detector { models, config }
    }

    /// Decide what to do about `word`, typed while `active` was the layout.
    pub fn evaluate(&self, word: &str, active: &str) -> Verdict {
        if !self.config.enabled {
            return Verdict::Keep(Skip::Disabled);
        }
        let Some(from) = layout::layout(active) else {
            return Verdict::Keep(Skip::ModelMissing);
        };
        if let Some(skip) = self.prefilter(word, from) {
            return Verdict::Keep(skip);
        }

        let Some(src_model) = self.models.get(from.def.lang) else {
            return Verdict::Keep(Skip::ModelMissing);
        };

        let alternatives = self.config.alternatives(active);
        if alternatives.is_empty() {
            return Verdict::Keep(Skip::NoAlternatives);
        }

        let score_as_typed = src_model.score(word);
        let source_word_logp = src_model.word_logp(&crate::model::normalize(word));
        let source_knows_word = source_word_logp.is_some();

        let mut best: Option<Candidate> = None;
        let mut saw_convertible = false;

        for alt in alternatives {
            let Some(to) = layout::layout(alt) else {
                continue;
            };
            let Some(dst_model) = self.models.get(to.def.lang) else {
                continue;
            };
            let converted = layout::convert(word, from, to);
            if converted == word {
                continue;
            }
            // Refuse conversions that leave characters the target cannot type;
            // those are mangling, not fixing.
            if to.coverage(&converted) < MIN_COVERAGE {
                continue;
            }
            // The decisive test for punctuation-bearing input: a genuine
            // mistyped word becomes *entirely* letters once corrected. This is
            // what lets "cgfcb,j" through as спасибо while still rejecting the
            // stray commas and semicolons of ordinary English typing.
            if !converted.chars().all(|c| c.is_alphabetic()) {
                continue;
            }
            saw_convertible = true;

            let score_converted = dst_model.score(&converted);
            let target_knows_word = dst_model.knows(&crate::model::normalize(&converted));

            let cand = Candidate {
                to_layout: to.def.id,
                converted,
                score_as_typed,
                score_converted,
                delta: score_converted - score_as_typed,
                target_knows_word,
                source_knows_word,
            };
            if best.as_ref().is_none_or(|b| cand.delta > b.delta) {
                best = Some(cand);
            }
        }

        if !saw_convertible {
            return Verdict::Keep(Skip::Unconvertible);
        }
        let Some(best) = best else {
            return Verdict::Keep(Skip::NotConfident);
        };

        // -- thresholds -----------------------------------------------------
        let mut required = self.config.sensitivity.threshold();

        // Rewriting something that is already a valid word is the most
        // damaging mistake this app can make, so known words get a steep
        // surcharge — graded by how common the word actually is, so that
        // corpus-tail noise cannot veto an otherwise obvious correction.
        let surcharge = self.config.sensitivity.known_word_surcharge();
        required += match source_word_logp {
            Some(lp) if lp >= COMMON_WORD_FLOOR => surcharge,
            Some(_) => surcharge * RARE_WORD_PROTECTION,
            None => 0.0,
        };

        // Neither reading is a known word: this is a guess from letter shape
        // alone, so demand much more before touching the user's text.
        if !best.target_knows_word {
            required += self.config.sensitivity.unknown_target_surcharge();
        }

        // Latin-to-Latin pairs share an alphabet, so a "wrong layout" word
        // still looks like text and the evidence is inherently weaker.
        let from_latin = from.def.latin_overlap;
        let to_latin = layout::layout(best.to_layout).is_some_and(|l| l.def.latin_overlap);
        if from_latin && to_latin {
            required += 1.5;
        }

        // Short words carry little evidence: insist on a real dictionary hit.
        let len = word.chars().count();
        if len < self.config.require_dict_below_len && !best.target_knows_word {
            return Verdict::Keep(Skip::TooShort);
        }

        if best.delta >= required {
            Verdict::Switch(Box::new(best))
        } else if best.delta >= required - AMBIGUOUS_BAND {
            Verdict::Ambiguous(Box::new(best))
        } else {
            Verdict::Keep(Skip::NotConfident)
        }
    }

    /// Cheap rejections that need no models.
    ///
    /// The subtle part is punctuation. A word typed on the wrong layout is full
    /// of it — Russian `б` and `ю` live on the `,` and `.` keys, Ukrainian `ж`
    /// on `;` — so "letters only" would reject precisely the input this app
    /// exists to fix. Instead a non-letter is tolerated when some enabled
    /// layout turns that physical key into a letter, and the conversion step
    /// later insists the corrected reading is entirely alphabetic.
    fn prefilter(&self, word: &str, from: &Layout) -> Option<Skip> {
        let len = word.chars().count();
        if len < self.config.min_word_len {
            return Some(Skip::TooShort);
        }
        if word.chars().any(|c| c.is_numeric() || c.is_whitespace() || c.is_control()) {
            return Some(Skip::NotAWord);
        }
        if word.chars().any(|c| STRUCTURAL.contains(&c)) {
            return Some(Skip::NotAWord);
        }
        if word.chars().filter(|c| *c == '/').count() > MAX_SLASHES {
            return Some(Skip::NotAWord);
        }

        let alternatives = self.config.alternatives(from.def.id);
        for c in word.chars() {
            if c.is_alphabetic() {
                continue;
            }
            if !self.is_letter_capable(c, from, &alternatives) {
                return Some(Skip::NotAWord);
            }
        }

        // Only bicameral scripts can shout. Hebrew and Arabic letters have no
        // case at all, so `all(!is_lowercase)` is trivially true for every word
        // in them — requiring a genuine uppercase character keeps the acronym
        // guard from silently disabling those languages entirely.
        if self.config.skip_all_caps
            && len > 1
            && word.chars().any(|c| c.is_uppercase())
            && word.chars().all(|c| !c.is_lowercase())
        {
            return Some(Skip::AllCaps);
        }
        if self.config.is_exception(word) {
            return Some(Skip::UserException);
        }
        None
    }

    /// True when the physical key behind `c` contributes to a letter on at
    /// least one of the layouts the user actually types in.
    ///
    /// Dead keys count. Greek marks stress on nearly every multisyllabic word,
    /// so `;` — which is the tonos key — appears inside the great majority of
    /// mistyped Greek words even though `΄` is a modifier symbol rather than a
    /// letter in its own right.
    fn is_letter_capable(&self, c: char, from: &Layout, alternatives: &[&str]) -> bool {
        let Some((idx, shifted)) = from.key_for(c) else {
            return false;
        };
        alternatives.iter().any(|alt| {
            layout::layout(alt).is_some_and(|to| {
                if to.is_dead_key(idx, shifted) {
                    return true;
                }
                let tok = to.token_at(idx, shifted);
                !tok.is_empty() && tok.chars().all(|ch| ch.is_alphabetic())
            })
        })
    }

    /// Split a typed token into `(prefix, core, suffix)`, where the affixes are
    /// punctuation that no enabled layout could turn into a letter.
    ///
    /// This is what lets `ghbdtn!` be corrected: the `!` is punctuation on every
    /// layout the user has on, so it can be set aside and put back afterwards.
    /// A trailing `,` is left attached when a Cyrillic layout is enabled,
    /// because there it is the letter `б` and stripping it would corrupt the
    /// word.
    pub fn trim_affixes<'a>(&self, token: &'a str, active: &str) -> (&'a str, &'a str, &'a str) {
        let Some(from) = layout::layout(active) else {
            return ("", token, "");
        };
        let alternatives = self.config.alternatives(active);
        let is_affix =
            |c: char| !c.is_alphabetic() && !self.is_letter_capable(c, from, &alternatives);

        let start = token
            .char_indices()
            .find(|(_, c)| !is_affix(*c))
            .map(|(i, _)| i)
            .unwrap_or(token.len());
        let end = token
            .char_indices()
            .rev()
            .find(|(_, c)| !is_affix(*c))
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(start);
        (&token[..start], &token[start..end], &token[end..])
    }

    /// Convert `word` between two layouts unconditionally. Backs the manual
    /// "fix my selection" hotkey, which bypasses every guard by design.
    pub fn force_convert(&self, word: &str, from: &str, to: &str) -> Option<String> {
        layout::convert_by_id(word, from, to)
    }

    /// Best guess at which of the configured layouts `text` was *meant* for.
    /// Used by the manual hotkey when the user has more than two layouts on.
    pub fn best_target(&self, text: &str, active: &str) -> Option<&'static str> {
        let from = layout::layout(active)?;
        let mut best: Option<(&'static str, f32)> = None;
        for alt in self.config.alternatives(active) {
            let to: &Layout = layout::layout(alt)?;
            let model = self.models.get(to.def.lang)?;
            let converted = layout::convert(text, from, to);
            let score = model.score(&converted);
            if best.is_none_or(|(_, b)| score > b) {
                best = Some((to.def.id, score));
            }
        }
        best.map(|(id, _)| id)
    }
}
