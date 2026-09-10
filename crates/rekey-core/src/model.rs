//! Compact per-language scoring models.
//!
//! A model answers one question: *how plausible is this string as a word in
//! this language?* It combines two signals.
//!
//! 1. **A word table.** Hashed word -> log10 unigram probability, built from an
//!    OpenSubtitles frequency list. Because the corpus is transcribed speech it
//!    covers slang, contractions and profanity that a formal dictionary omits.
//! 2. **A character trigram table.** For anything the word table misses —
//!    typos, inflections, names, brand-new slang — the model falls back to how
//!    word-like the character sequence is for the language.
//!
//! Both candidate readings of a keystroke sequence are scored with the same
//! formula, so the *difference* is what matters and absolute calibration is
//! only used for the confidence reported to the UI.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

const MAGIC: &[u8; 4] = b"RKM2";
const VERSION: u16 = 2;

/// Word boundary marker used when extracting trigrams.
pub const BOUND: char = '\u{2}';

/// Score assigned to a trigram that never appeared in training.
const UNSEEN_TRIGRAM: f32 = -7.0;

/// Additive penalty applied to a word that is not in the word table, keeping
/// dictionary hits clearly ahead of merely-plausible letter sequences.
const OOV_PENALTY: f32 = -3.0;

/// A trained model for one language.
#[derive(Debug, Clone)]
pub struct Model {
    pub lang: String,
    /// (hash, log10 probability), sorted by hash for binary search.
    words: Vec<(u32, f32)>,
    /// (hash, log10 probability), sorted by hash.
    trigrams: Vec<(u32, f32)>,
}

/// FNV-1a, 32-bit. Small, fast, no dependency, and good enough for a table
/// this size — at 50k entries the expected number of collisions is well under
/// one, and a collision can only ever nudge a score, never bypass the guards.
pub fn hash_str(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Lowercase a word for lookup. Diacritics are deliberately preserved: for
/// Latin-script pairs they are often the only signal that a layout is wrong.
pub fn normalize(word: &str) -> String {
    word.to_lowercase()
}

/// Yield the padded character trigrams of a word, e.g. `abc` ->
/// `\x02\x02a`, `\x02ab`, `abc`, `bc\x02`, `c\x02\x02`.
pub fn trigrams(word: &str) -> Vec<String> {
    let mut chars: Vec<char> = Vec::with_capacity(word.chars().count() + 4);
    chars.push(BOUND);
    chars.push(BOUND);
    chars.extend(word.chars());
    chars.push(BOUND);
    chars.push(BOUND);
    chars
        .windows(3)
        .map(|w| w.iter().collect::<String>())
        .collect()
}

impl Model {
    pub fn new(lang: impl Into<String>) -> Model {
        Model {
            lang: lang.into(),
            words: Vec::new(),
            trigrams: Vec::new(),
        }
    }

    /// Build a model from `word -> raw count` pairs.
    pub fn train(lang: impl Into<String>, counts: &HashMap<String, u64>) -> Model {
        let total: f64 = counts.values().map(|c| *c as f64).sum();
        let mut words: Vec<(u32, f32)> = Vec::with_capacity(counts.len());
        let mut tri_counts: HashMap<String, f64> = HashMap::new();

        for (word, count) in counts {
            let w = normalize(word);
            if w.is_empty() {
                continue;
            }
            let p = (*count as f64) / total;
            words.push((hash_str(&w), p.log10() as f32));

            // Weight trigrams by word frequency so common spellings dominate,
            // but dampen with a log so a handful of ultra-frequent function
            // words do not define the whole language's letter statistics.
            let weight = (*count as f64).ln().max(1.0);
            for t in trigrams(&w) {
                *tri_counts.entry(t).or_insert(0.0) += weight;
            }
        }

        let tri_total: f64 = tri_counts.values().sum();
        let mut trigrams: Vec<(u32, f32)> = tri_counts
            .into_iter()
            .map(|(t, c)| (hash_str(&t), (c / tri_total).log10() as f32))
            .collect();

        words.sort_unstable_by_key(|e| e.0);
        words.dedup_by_key(|e| e.0);
        trigrams.sort_unstable_by_key(|e| e.0);
        trigrams.dedup_by_key(|e| e.0);

        Model {
            lang: lang.into(),
            words,
            trigrams,
        }
    }

    pub fn word_count(&self) -> usize {
        self.words.len()
    }

    pub fn trigram_count(&self) -> usize {
        self.trigrams.len()
    }

    /// log10 probability of `word` if the word table knows it.
    pub fn word_logp(&self, word: &str) -> Option<f32> {
        let h = hash_str(word);
        self.words
            .binary_search_by_key(&h, |e| e.0)
            .ok()
            .map(|i| self.words[i].1)
    }

    /// True when the word table contains this exact (normalized) word.
    pub fn knows(&self, word: &str) -> bool {
        self.word_logp(word).is_some()
    }

    fn trigram_logp(&self, t: &str) -> f32 {
        let h = hash_str(t);
        self.trigrams
            .binary_search_by_key(&h, |e| e.0)
            .ok()
            .map(|i| self.trigrams[i].1)
            .unwrap_or(UNSEEN_TRIGRAM)
    }

    /// Mean log10 trigram probability per trigram. Length-normalized so it can
    /// be compared across readings of differing length.
    pub fn shape_score(&self, word: &str) -> f32 {
        let tris = trigrams(word);
        if tris.is_empty() {
            return UNSEEN_TRIGRAM;
        }
        let sum: f32 = tris.iter().map(|t| self.trigram_logp(t)).sum();
        sum / tris.len() as f32
    }

    /// The most likely real word within one edit of `word`.
    ///
    /// Returns that word's log10 probability, so the caller can tell a slip in
    /// a common word from a slip that happens to land near an obscure one.
    ///
    /// One edit means a single inserted, deleted, substituted or transposed
    /// letter — the shape of an ordinary typing slip. Wider edit distances
    /// start matching unrelated words, which is the opposite of what this is
    /// for: the point is to recognise a real word that was mistyped, not to
    /// find something vaguely similar.
    ///
    /// Every candidate is generated and looked up, rather than the table being
    /// searched. A six-letter word over a 33-letter alphabet is about 450
    /// lookups, and each is a binary search — cheap enough to sit in the
    /// typing path, and it needs no extra index.
    pub fn best_within_one_edit(&self, word: &str, alphabet: &[char]) -> Option<f32> {
        let chars: Vec<char> = word.chars().collect();
        let mut best: Option<f32> = None;
        let mut consider = |candidate: &str| {
            if let Some(logp) = self.word_logp(candidate) {
                if best.is_none_or(|b| logp > b) {
                    best = Some(logp);
                }
            }
        };

        let mut buffer = String::with_capacity(word.len() + 4);

        // Deletion: one letter too many was typed.
        for skip in 0..chars.len() {
            buffer.clear();
            buffer.extend(
                chars
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != skip)
                    .map(|(_, c)| c),
            );
            consider(&buffer);
        }

        // Transposition: two neighbouring letters arrived in the wrong order.
        for i in 0..chars.len().saturating_sub(1) {
            let mut swapped = chars.clone();
            swapped.swap(i, i + 1);
            buffer.clear();
            buffer.extend(swapped.iter());
            consider(&buffer);
        }

        for &letter in alphabet {
            // Substitution: the wrong letter was typed.
            for i in 0..chars.len() {
                if chars[i] == letter {
                    continue;
                }
                buffer.clear();
                buffer.extend(
                    chars
                        .iter()
                        .enumerate()
                        .map(|(j, c)| if j == i { &letter } else { c }),
                );
                consider(&buffer);
            }
            // Insertion: a letter was missed out.
            for i in 0..=chars.len() {
                buffer.clear();
                buffer.extend(chars[..i].iter());
                buffer.push(letter);
                buffer.extend(chars[i..].iter());
                consider(&buffer);
            }
        }

        best
    }

    /// How plausible `word` is in this language, in log10 units.
    ///
    /// A known word scores its unigram probability. An unknown word scores its
    /// letter-shape plausibility minus a penalty, which keeps real words ahead
    /// of merely-pronounceable ones while still letting unseen slang beat
    /// outright gibberish.
    pub fn score(&self, word: &str) -> f32 {
        let w = normalize(word);
        match self.word_logp(&w) {
            Some(logp) => logp,
            None => self.shape_score(&w) + OOV_PENALTY,
        }
    }

    // -- serialization ------------------------------------------------------

    pub fn write<W: Write>(&self, out: &mut W) -> io::Result<()> {
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        let lang = self.lang.as_bytes();
        out.write_all(&[lang.len() as u8])?;
        out.write_all(lang)?;
        out.write_all(&(self.words.len() as u32).to_le_bytes())?;
        for (h, p) in &self.words {
            out.write_all(&h.to_le_bytes())?;
            out.write_all(&p.to_le_bytes())?;
        }
        out.write_all(&(self.trigrams.len() as u32).to_le_bytes())?;
        for (h, p) in &self.trigrams {
            out.write_all(&h.to_le_bytes())?;
            out.write_all(&p.to_le_bytes())?;
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::new();
        self.write(&mut buf)?;
        fs::write(path, buf)
    }

    pub fn read<R: Read>(input: &mut R) -> io::Result<Model> {
        let mut buf = Vec::new();
        input.read_to_end(&mut buf)?;
        Model::from_bytes(&buf)
    }

    pub fn from_bytes(buf: &[u8]) -> io::Result<Model> {
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
        let mut c = Cursor { buf, pos: 0 };

        if c.take(4)? != MAGIC {
            return Err(bad("not a Rekey model file"));
        }
        let version = u16::from_le_bytes(c.take(2)?.try_into().unwrap());
        if version != VERSION {
            return Err(bad(&format!(
                "model version {version} is not supported (expected {VERSION})"
            )));
        }
        let lang_len = c.take(1)?[0] as usize;
        let lang = String::from_utf8(c.take(lang_len)?.to_vec())
            .map_err(|_| bad("language tag is not valid UTF-8"))?;

        let n_words = u32::from_le_bytes(c.take(4)?.try_into().unwrap()) as usize;
        let mut words = Vec::with_capacity(n_words);
        for _ in 0..n_words {
            let h = u32::from_le_bytes(c.take(4)?.try_into().unwrap());
            let p = f32::from_le_bytes(c.take(4)?.try_into().unwrap());
            words.push((h, p));
        }
        let n_tri = u32::from_le_bytes(c.take(4)?.try_into().unwrap()) as usize;
        let mut trigrams = Vec::with_capacity(n_tri);
        for _ in 0..n_tri {
            let h = u32::from_le_bytes(c.take(4)?.try_into().unwrap());
            let p = f32::from_le_bytes(c.take(4)?.try_into().unwrap());
            trigrams.push((h, p));
        }

        Ok(Model {
            lang,
            words,
            trigrams,
        })
    }

    pub fn load(path: &Path) -> io::Result<Model> {
        Model::from_bytes(&fs::read(path)?)
    }
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "model file is truncated",
            ));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
}

/// All loaded language models, keyed by language tag.
#[derive(Debug, Default, Clone)]
pub struct Models {
    map: HashMap<String, Model>,
}

impl Models {
    pub fn new() -> Models {
        Models::default()
    }

    pub fn insert(&mut self, model: Model) {
        self.map.insert(model.lang.clone(), model);
    }

    pub fn get(&self, lang: &str) -> Option<&Model> {
        self.map.get(lang)
    }

    pub fn has(&self, lang: &str) -> bool {
        self.map.contains_key(lang)
    }

    pub fn langs(&self) -> Vec<&str> {
        self.map.keys().map(|s| s.as_str()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Load every `*.rkm` file in `dir`. Missing directory is not an error —
    /// the app degrades to layout conversion without scoring and says so.
    pub fn load_dir(dir: &Path) -> io::Result<Models> {
        let mut models = Models::new();
        if !dir.is_dir() {
            return Ok(models);
        }
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("rkm") {
                models.insert(Model::load(&path)?);
            }
        }
        Ok(models)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toy() -> Model {
        let mut counts = HashMap::new();
        counts.insert("hello".to_string(), 1000u64);
        counts.insert("world".to_string(), 500);
        counts.insert("the".to_string(), 9000);
        Model::train("en", &counts)
    }

    #[test]
    fn trigrams_are_padded() {
        let t = trigrams("ab");
        assert_eq!(t.len(), 4);
        assert_eq!(t[0], format!("{BOUND}{BOUND}a"));
        assert_eq!(t[3], format!("b{BOUND}{BOUND}"));
    }

    #[test]
    fn known_words_beat_gibberish() {
        let m = toy();
        assert!(m.knows("hello"));
        assert!(m.score("hello") > m.score("xqzptr"));
    }

    #[test]
    fn frequency_is_reflected() {
        let m = toy();
        assert!(m.score("the") > m.score("world"));
    }

    #[test]
    fn unknown_but_wordlike_beats_unknown_gibberish() {
        let m = toy();
        // "helld" shares trigrams with the training words; "qqqqq" shares none.
        assert!(m.score("helld") > m.score("qqqqq"));
    }

    #[test]
    fn scoring_is_case_insensitive() {
        let m = toy();
        assert_eq!(m.score("HELLO"), m.score("hello"));
    }

    #[test]
    fn roundtrips_through_bytes() {
        let m = toy();
        let mut buf = Vec::new();
        m.write(&mut buf).unwrap();
        let back = Model::from_bytes(&buf).unwrap();
        assert_eq!(back.lang, "en");
        assert_eq!(back.word_count(), m.word_count());
        assert_eq!(back.trigram_count(), m.trigram_count());
        assert_eq!(back.score("hello"), m.score("hello"));
    }

    #[test]
    fn rejects_foreign_files() {
        assert!(Model::from_bytes(b"not a model at all").is_err());
    }

    #[test]
    fn rejects_truncated_files() {
        let m = toy();
        let mut buf = Vec::new();
        m.write(&mut buf).unwrap();
        buf.truncate(buf.len() / 2);
        assert!(Model::from_bytes(&buf).is_err());
    }
}
