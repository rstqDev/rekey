//! Physical-key layout tables and cross-layout transliteration.
//!
//! Every layout is described as 47 space-separated tokens, one per printable
//! key of a US ANSI keyboard, always in the same physical order:
//!
//! ```text
//!   row 1 (13):  ` 1 2 3 4 5 6 7 8 9 0 - =
//!   row 2 (13):  q w e r t y u i o p [ ] \
//!   row 3 (11):  a s d f g h j k l ; '
//!   row 4 (10):  z x c v b n m , . /
//! ```
//!
//! Because the index *is* the physical key, converting a string between two
//! layouts is a lookup in one table and an emit from the other: "ghbdtn" typed
//! on US maps to keys g,h,b,d,t,n which on the Russian table are п,р,и,в,е,т.
//!
//! `NONE` marks a key that produces nothing on that layout.

use std::collections::HashMap;
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

/// Number of printable keys we model.
pub const KEY_COUNT: usize = 47;

/// Sentinel token for "this key produces nothing on this layout".
const NONE: &str = "~NONE~";

/// Writing system, used to short-circuit obviously-impossible conversions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Latin,
    Cyrillic,
    Hebrew,
    Arabic,
    Greek,
}

/// A static keyboard layout definition.
#[derive(Debug, Clone, Copy)]
pub struct LayoutDef {
    /// Stable identifier used in config files and IPC.
    pub id: &'static str,
    /// Human-readable name shown in the UI.
    pub name: &'static str,
    /// Language model this layout types in.
    pub lang: &'static str,
    pub script: Script,
    /// True when the layout shares the Latin alphabet with US QWERTY, so a
    /// mistyped word is still mostly readable and detection must be stricter.
    pub latin_overlap: bool,
    lower: &'static str,
    upper: &'static str,
    /// Dead keys, as `(combining mark, the character the dead key shows)`.
    ///
    /// Accented letters on these layouts are two keystrokes — `;` then `a` for
    /// Greek `ά` — so they have no single key position. Without this, accented
    /// text simply passes through unconverted and whole languages look broken.
    dead: &'static [(char, char)],
}

// ---------------------------------------------------------------------------
// Layout tables
// ---------------------------------------------------------------------------

pub static LAYOUTS: &[LayoutDef] = &[
    LayoutDef {
        id: "us",
        name: "English (US QWERTY)",
        lang: "en",
        script: Script::Latin,
        latin_overlap: true,
        lower: "` 1 2 3 4 5 6 7 8 9 0 - = q w e r t y u i o p [ ] \\ a s d f g h j k l ; ' z x c v b n m , . /",
        upper: "~ ! @ # $ % ^ & * ( ) _ + Q W E R T Y U I O P { } | A S D F G H J K L : \" Z X C V B N M < > ?",
        dead: &[],
    },
    LayoutDef {
        id: "ru",
        name: "Russian (ЙЦУКЕН)",
        lang: "ru",
        script: Script::Cyrillic,
        latin_overlap: false,
        lower: "ё 1 2 3 4 5 6 7 8 9 0 - = й ц у к е н г ш щ з х ъ \\ ф ы в а п р о л д ж э я ч с м и т ь б ю .",
        upper: "Ё ! \" № ; % : ? * ( ) _ + Й Ц У К Е Н Г Ш Щ З Х Ъ / Ф Ы В А П Р О Л Д Ж Э Я Ч С М И Т Ь Б Ю ,",
           dead: &[],
    },
    LayoutDef {
        id: "uk",
        name: "Ukrainian (ЙЦУКЕН)",
        lang: "uk",
        script: Script::Cyrillic,
        latin_overlap: false,
        lower: "' 1 2 3 4 5 6 7 8 9 0 - = й ц у к е н г ш щ з х ї ґ ф і в а п р о л д ж є я ч с м и т ь б ю .",
        upper: "₴ ! \" № ; % : ? * ( ) _ + Й Ц У К Е Н Г Ш Щ З Х Ї Ґ Ф І В А П Р О Л Д Ж Є Я Ч С М И Т Ь Б Ю ,",
           dead: &[],
    },
    LayoutDef {
        id: "he",
        name: "Hebrew",
        lang: "he",
        script: Script::Hebrew,
        latin_overlap: false,
        lower: "; 1 2 3 4 5 6 7 8 9 0 - = / ' ק ר א ט ו ן ם פ ] [ \\ ש ד ג כ ע י ח ל ך ף , ז ס ב ה נ מ צ ת ץ .",
        upper: "~ ! @ # $ % ^ & * ( ) _ + Q W ק ר א ט ו ן ם פ } { | ש ד ג כ ע י ח ל ך ף \" ז ס ב ה נ מ צ ת ץ ?",
           dead: &[],
    },
    LayoutDef {
        id: "ar",
        name: "Arabic (101)",
        lang: "ar",
        script: Script::Arabic,
        latin_overlap: false,
        lower: "ذ 1 2 3 4 5 6 7 8 9 0 - = ض ص ث ق ف غ ع ه خ ح ج د \\ ش س ي ب ل ا ت ن م ك ط ئ ء ؤ ر لا ى ة و ز ظ",
        upper: "ّ ! @ # $ % ^ & * ( ) _ + َ ً ُ ٌ لإ إ ‘ ÷ × ؛ < > | ِ ٍ ] [ لأ أ ـ ، / : \" ~ ْ } { لآ آ ’ , . ؟",
           dead: &[],
    },
    LayoutDef {
        id: "el",
        name: "Greek",
        lang: "el",
        script: Script::Greek,
        latin_overlap: false,
        lower: "` 1 2 3 4 5 6 7 8 9 0 - = ; ς ε ρ τ υ θ ι ο π [ ] \\ α σ δ φ γ η ξ κ λ ΄ ' ζ χ ψ ω β ν μ , . /",
        upper: "~ ! @ # $ % ^ & * ( ) _ + : Σ Ε Ρ Τ Υ Θ Ι Ο Π { } | Α Σ Δ Φ Γ Η Ξ Κ Λ ¨ \" Ζ Χ Ψ Ω Β Ν Μ < > ?",
           dead: &[('\u{301}', '\u{384}'), ('\u{308}', '\u{a8}')],
    },
    LayoutDef {
        id: "de",
        name: "German (QWERTZ)",
        lang: "de",
        script: Script::Latin,
        latin_overlap: true,
        lower: "^ 1 2 3 4 5 6 7 8 9 0 ß ´ q w e r t z u i o p ü + # a s d f g h j k l ö ä y x c v b n m , . -",
        upper: "° ! \" § $ % & / ( ) = ? ` Q W E R T Z U I O P Ü * ' A S D F G H J K L Ö Ä Y X C V B N M ; : _",
           dead: &[('\u{301}', '\u{b4}'), ('\u{300}', '\u{60}'), ('\u{302}', '\u{5e}')],
    },
    LayoutDef {
        id: "fr",
        name: "French (AZERTY)",
        lang: "fr",
        script: Script::Latin,
        latin_overlap: true,
        lower: "² & é \" ' ( - è _ ç à ) = a z e r t y u i o p ^ $ * q s d f g h j k l m ù w x c v b n , ; : !",
        upper: "~NONE~ 1 2 3 4 5 6 7 8 9 0 ° + A Z E R T Y U I O P ¨ £ µ Q S D F G H J K L M % W X C V B N ? . / §",
           dead: &[('\u{302}', '\u{5e}'), ('\u{308}', '\u{a8}')],
    },
    LayoutDef {
        id: "es",
        name: "Spanish (QWERTY)",
        lang: "es",
        script: Script::Latin,
        latin_overlap: true,
        lower: "º 1 2 3 4 5 6 7 8 9 0 ' ¡ q w e r t y u i o p ` + ç a s d f g h j k l ñ ´ z x c v b n m , . -",
        upper: "ª ! \" · $ % & / ( ) = ? ¿ Q W E R T Y U I O P ^ * Ç A S D F G H J K L Ñ ¨ Z X C V B N M ; : _",
           dead: &[('\u{301}', '\u{b4}'), ('\u{308}', '\u{a8}'), ('\u{300}', '\u{60}'), ('\u{302}', '\u{5e}')],
    },
    LayoutDef {
        id: "tr",
        name: "Turkish (Q)",
        lang: "tr",
        script: Script::Latin,
        latin_overlap: true,
        lower: "\" 1 2 3 4 5 6 7 8 9 0 * - q w e r t y u ı o p ğ ü , a s d f g h j k l ş i z x c v b n m ö ç .",
        upper: "é ! ' ^ + % & / ( ) = ? _ Q W E R T Y U I O P Ğ Ü ; A S D F G H J K L Ş İ Z X C V B N M Ö Ç :",
           dead: &[],
    },
];

// ---------------------------------------------------------------------------
// Runtime tables
// ---------------------------------------------------------------------------

/// A layout with lookup tables built, ready for conversion.
pub struct Layout {
    pub def: &'static LayoutDef,
    /// key index -> (unshifted token, shifted token)
    keys: Vec<(String, String)>,
    /// character -> (key index, needs shift)
    chars: HashMap<char, (usize, bool)>,
    /// Multi-codepoint tokens (e.g. Arabic لا) -> (key index, needs shift).
    /// Matched greedily before single characters so they round-trip as one key.
    multi: HashMap<String, (usize, bool)>,
    /// Longest multi token in chars, 0 when there are none.
    max_multi: usize,
    /// combining mark -> the key that acts as its dead key on this layout.
    dead_by_mark: HashMap<char, (usize, bool)>,
    /// key index (+shift) -> the combining mark that key applies.
    mark_by_key: HashMap<(usize, bool), char>,
}

impl Layout {
    fn build(def: &'static LayoutDef) -> Layout {
        let lower: Vec<&str> = def.lower.split(' ').collect();
        let upper: Vec<&str> = def.upper.split(' ').collect();
        assert_eq!(
            lower.len(),
            KEY_COUNT,
            "layout {} has {} lower tokens, expected {}",
            def.id,
            lower.len(),
            KEY_COUNT
        );
        assert_eq!(
            upper.len(),
            KEY_COUNT,
            "layout {} has {} upper tokens, expected {}",
            def.id,
            upper.len(),
            KEY_COUNT
        );

        let mut keys = Vec::with_capacity(KEY_COUNT);
        let mut chars: HashMap<char, (usize, bool)> = HashMap::new();

        for i in 0..KEY_COUNT {
            let lo = if lower[i] == NONE { "" } else { lower[i] };
            let up = if upper[i] == NONE { "" } else { upper[i] };

            // Only single-character tokens are addressable as input. Multi-char
            // tokens (e.g. Arabic لا) can still be produced as output; their
            // first char is registered as a fallback if nothing else claims it.
            for (tok, shifted) in [(lo, false), (up, true)] {
                let mut it = tok.chars();
                if let (Some(c), rest) = (it.next(), it.as_str()) {
                    if rest.is_empty() {
                        chars.entry(c).or_insert((i, shifted));
                    }
                }
            }
            keys.push((lo.to_string(), up.to_string()));
        }
        // Second pass: collect multi-codepoint tokens so they can be matched as
        // a unit, and register their leading char only if it is otherwise
        // unreachable on this layout.
        let mut multi: HashMap<String, (usize, bool)> = HashMap::new();
        let mut max_multi = 0usize;
        for i in 0..KEY_COUNT {
            for (tok, shifted) in [(keys[i].0.clone(), false), (keys[i].1.clone(), true)] {
                let len = tok.chars().count();
                if len > 1 {
                    max_multi = max_multi.max(len);
                    multi.entry(tok.clone()).or_insert((i, shifted));
                    if let Some(c) = tok.chars().next() {
                        chars.entry(c).or_insert((i, shifted));
                    }
                }
            }
        }

        // Dead keys: locate the key that produces each dead character so a
        // composed letter can be expressed as (dead key, base key).
        let mut dead_by_mark = HashMap::new();
        let mut mark_by_key = HashMap::new();
        for (mark, dead_char) in def.dead {
            if let Some(&(idx, shifted)) = chars.get(dead_char) {
                dead_by_mark.insert(*mark, (idx, shifted));
                mark_by_key.insert((idx, shifted), *mark);
            }
        }

        Layout {
            def,
            keys,
            chars,
            multi,
            max_multi,
            dead_by_mark,
            mark_by_key,
        }
    }

    pub fn id(&self) -> &'static str {
        self.def.id
    }

    /// Which physical key produces `c` on this layout, and whether shift is held.
    pub fn key_for(&self, c: char) -> Option<(usize, bool)> {
        self.chars.get(&c).copied()
    }

    /// The token of this layout that `s` starts with, as
    /// `(byte length consumed, key index, needs shift)`.
    ///
    /// Single characters are matched first and multi-codepoint tokens only as a
    /// fallback. Some layouts can produce the same text two ways — Arabic `لا`
    /// is both the dedicated `b` ligature key and `ل` (g) followed by `ا` (h) —
    /// and preferring the single-character reading makes the *foreign* text a
    /// fixed point of round-tripping, which is the property the detector needs.
    /// The Latin intermediate is only ever a scratch representation.
    fn match_prefix(&self, s: &str) -> Option<(usize, usize, bool)> {
        if let Some(c) = s.chars().next() {
            if let Some((idx, shifted)) = self.key_for(c) {
                return Some((c.len_utf8(), idx, shifted));
            }
        }
        if self.max_multi > 1 {
            let mut end = 0usize;
            for (n, (off, c)) in s.char_indices().enumerate() {
                if n >= self.max_multi {
                    break;
                }
                end = off + c.len_utf8();
            }
            // Try progressively shorter prefixes, longest first.
            let mut cand: Vec<usize> = s[..end]
                .char_indices()
                .map(|(i, c)| i + c.len_utf8())
                .collect();
            cand.reverse();
            for cut in cand {
                if cut == 0 {
                    continue;
                }
                if let Some(&(idx, shifted)) = self.multi.get(&s[..cut]) {
                    return Some((cut, idx, shifted));
                }
            }
        }
        None
    }

    /// What this layout produces for a physical key.
    pub fn token_at(&self, idx: usize, shifted: bool) -> &str {
        let (lo, up) = &self.keys[idx];
        if shifted {
            up
        } else {
            lo
        }
    }

    /// Split a composed character into the (dead key, base key) presses this
    /// layout would need for it, e.g. Greek `ά` -> the `;` and `a` keys.
    fn dead_key_presses(&self, c: char) -> Option<[(usize, bool); 2]> {
        let decomposed: Vec<char> = c.nfd().collect();
        if decomposed.len() != 2 {
            return None;
        }
        let (base, mark) = (decomposed[0], decomposed[1]);
        let dead = *self.dead_by_mark.get(&mark)?;
        let base_key = self.key_for(base)?;
        Some([dead, base_key])
    }

    /// The combining mark this key applies, if it is a dead key.
    fn mark_for_key(&self, idx: usize, shifted: bool) -> Option<char> {
        self.mark_by_key.get(&(idx, shifted)).copied()
    }

    /// True when this key is a dead key on this layout.
    pub fn is_dead_key(&self, idx: usize, shifted: bool) -> bool {
        self.mark_by_key.contains_key(&(idx, shifted))
    }

    /// Fraction of `s` that this layout can actually type. Used to reject
    /// conversions that would mangle text rather than fix it.
    pub fn coverage(&self, s: &str) -> f32 {
        let mut total = 0usize;
        let mut hit = 0usize;
        for c in s.chars() {
            if c.is_whitespace() {
                continue;
            }
            total += 1;
            if self.chars.contains_key(&c) || self.dead_key_presses(c).is_some() {
                hit += 1;
            }
        }
        if total == 0 {
            return 0.0;
        }
        hit as f32 / total as f32
    }
}

static REGISTRY: OnceLock<HashMap<&'static str, Layout>> = OnceLock::new();

fn registry() -> &'static HashMap<&'static str, Layout> {
    REGISTRY.get_or_init(|| {
        LAYOUTS
            .iter()
            .map(|def| (def.id, Layout::build(def)))
            .collect()
    })
}

/// Look up a built layout by id (`"us"`, `"ru"`, …).
pub fn layout(id: &str) -> Option<&'static Layout> {
    registry().get(id)
}

/// Every layout id we know about.
pub fn layout_ids() -> Vec<&'static str> {
    LAYOUTS.iter().map(|d| d.id).collect()
}

/// Re-type `text` as if the same physical keys had been pressed with `to`
/// active instead of `from`.
///
/// Characters `from` cannot produce are passed through untouched, which keeps
/// digits, spaces and emoji intact.
/// One thing the user did: either a physical key, or a character that no key on
/// the source layout can produce and which is therefore carried through as-is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Press {
    Key { idx: usize, shifted: bool },
    Literal(char),
}

/// Reconstruct the keystrokes that produced `text` on `from`.
fn to_presses(text: &str, from: &Layout) -> Vec<Press> {
    let mut presses = Vec::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some((consumed, idx, shifted)) = from.match_prefix(rest) {
            presses.push(Press::Key { idx, shifted });
            rest = &rest[consumed..];
            continue;
        }
        let c = rest.chars().next().expect("rest is non-empty");
        // An accented letter is two keystrokes on layouts with dead keys.
        if let Some([dead, base]) = from.dead_key_presses(c) {
            presses.push(Press::Key {
                idx: dead.0,
                shifted: dead.1,
            });
            presses.push(Press::Key {
                idx: base.0,
                shifted: base.1,
            });
        } else {
            presses.push(Press::Literal(c));
        }
        rest = &rest[c.len_utf8()..];
    }
    presses
}

/// Replay `presses` on `to`, composing accents where `to` has dead keys.
fn render(presses: &[Press], to: &Layout) -> String {
    let mut out = String::new();
    // The combining mark still waiting for a base character, paired with the
    // visible accent the layout shows when nothing follows it.
    let mut pending: Option<(char, String)> = None;

    for press in presses {
        match *press {
            Press::Key { idx, shifted } => {
                // A dead key produces nothing until the next character lands.
                if let Some(mark) = to.mark_for_key(idx, shifted) {
                    let display = to.token_at(idx, shifted).to_string();
                    // Two dead keys in a row: the first one stands alone.
                    if let Some((_, prev_display)) = pending.replace((mark, display)) {
                        out.push_str(&prev_display);
                    }
                    continue;
                }
                let tok = to.token_at(idx, shifted);
                if tok.is_empty() {
                    continue;
                }
                match pending.take() {
                    Some((mark, _)) => {
                        let mut it = tok.chars();
                        let base = it.next().expect("token is non-empty");
                        push_composed(&mut out, mark, base);
                        out.push_str(it.as_str());
                    }
                    None => out.push_str(tok),
                }
            }
            Press::Literal(c) => match pending.take() {
                Some((mark, _)) => push_composed(&mut out, mark, c),
                None => out.push(c),
            },
        }
    }
    // A dead key with nothing after it shows as the layout's bare accent.
    if let Some((_, display)) = pending {
        out.push_str(&display);
    }
    out
}

/// Append `base` with `mark` applied, falling back to the two separate
/// characters when the pair has no precomposed form (e.g. `΄` over a
/// consonant, which the user can still see and correct).
fn push_composed(out: &mut String, mark: char, base: char) {
    let composed: String = [base, mark].iter().collect::<String>().nfc().collect();
    out.push_str(&composed);
}

/// Re-type `text` as if the same physical keys had been pressed with `to`
/// active instead of `from`.
///
/// Characters `from` cannot produce are passed through untouched, which keeps
/// digits, spaces and emoji intact.
pub fn convert(text: &str, from: &Layout, to: &Layout) -> String {
    render(&to_presses(text, from), to)
}

/// Convenience wrapper over [`convert`] taking layout ids.
pub fn convert_by_id(text: &str, from: &str, to: &str) -> Option<String> {
    Some(convert(text, layout(from)?, layout(to)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_layouts_have_47_keys() {
        // Building the registry asserts the token counts.
        assert_eq!(registry().len(), LAYOUTS.len());
    }

    #[test]
    fn en_to_ru_roundtrip() {
        assert_eq!(convert_by_id("ghbdtn", "us", "ru").unwrap(), "привет");
        assert_eq!(convert_by_id("привет", "ru", "us").unwrap(), "ghbdtn");
    }

    #[test]
    fn ru_to_en_roundtrip() {
        assert_eq!(convert_by_id("руддщ", "ru", "us").unwrap(), "hello");
        assert_eq!(convert_by_id("hello", "us", "ru").unwrap(), "руддщ");
    }

    #[test]
    fn ukrainian_differs_from_russian() {
        // s -> і on UA but ы on RU; ' -> є vs э
        assert_eq!(convert_by_id("s'", "us", "uk").unwrap(), "іє");
        assert_eq!(convert_by_id("s'", "us", "ru").unwrap(), "ыэ");
    }

    #[test]
    fn hebrew_roundtrip() {
        let shalom = convert_by_id("akuo", "us", "he").unwrap();
        assert_eq!(convert_by_id(&shalom, "he", "us").unwrap(), "akuo");
    }

    #[test]
    fn greek_roundtrip() {
        let g = convert_by_id("kalhmera", "us", "el").unwrap();
        assert_eq!(convert_by_id(&g, "el", "us").unwrap(), "kalhmera");
    }

    #[test]
    fn qwertz_swaps_y_and_z() {
        assert_eq!(convert_by_id("yz", "us", "de").unwrap(), "zy");
    }

    #[test]
    fn azerty_swaps_a_and_q() {
        assert_eq!(convert_by_id("aq", "us", "fr").unwrap(), "qa");
    }

    #[test]
    fn unmappable_chars_pass_through() {
        assert_eq!(convert_by_id("hi 🎉 42", "us", "ru").unwrap(), "рш 🎉 42");
    }

    /// Layouts where two different keys render identical lowercase text, so an
    /// exact Latin round-trip is impossible by construction.
    /// - `ar`: `لا` is both the `b` ligature key and `g`+`h`.
    const AMBIGUOUS_LOWER: &[&str] = &["ar"];

    /// Same, for uppercase.
    /// - `el`: `ς` (w) and `σ` (s) both uppercase to `Σ`.
    const AMBIGUOUS_UPPER: &[&str] = &["ar", "el"];

    #[test]
    fn unambiguous_layouts_roundtrip_all_us_letters_exactly() {
        let us = layout("us").unwrap();
        let src = "abcdefghijklmnopqrstuvwxyz";
        for def in LAYOUTS {
            if AMBIGUOUS_LOWER.contains(&def.id) {
                continue;
            }
            let other = layout(def.id).unwrap();
            let there = convert(src, us, other);
            let back = convert(&there, other, us);
            assert_eq!(back, src, "roundtrip failed for layout {}", def.id);
        }
    }

    #[test]
    fn foreign_rendering_is_a_fixed_point_for_every_layout() {
        // The property the detector actually relies on: whatever the Latin
        // intermediate ends up being, converting back and forth again must not
        // keep changing the foreign text. Holds even for ambiguous layouts.
        let us = layout("us").unwrap();
        let src = "abcdefghijklmnopqrstuvwxyz";
        for def in LAYOUTS {
            let other = layout(def.id).unwrap();
            let once = convert(src, us, other);
            let twice = convert(&convert(&once, other, us), us, other);
            assert_eq!(once, twice, "not a fixed point for layout {}", def.id);
        }
    }

    #[test]
    fn uppercase_roundtrips_for_bicameral_layouts() {
        let us = layout("us").unwrap();
        let src = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        for id in ["ru", "uk", "el", "de", "es"] {
            if AMBIGUOUS_UPPER.contains(&id) {
                continue;
            }
            let other = layout(id).unwrap();
            let back = convert(&convert(src, us, other), other, us);
            assert_eq!(back, src, "uppercase roundtrip failed for {id}");
        }
    }

    #[test]
    fn arabic_lam_alef_ligature_normalises() {
        // `b` on the Arabic layout produces the two-codepoint ligature لا, which
        // is also reachable as `g` then `h`. Reversing picks the two-key form,
        // but both spellings must render to identical Arabic.
        let from_lig = convert_by_id("b", "us", "ar").unwrap();
        let from_pair = convert_by_id("gh", "us", "ar").unwrap();
        assert_eq!(from_lig, from_pair);
        assert_eq!(convert_by_id(&from_lig, "ar", "us").unwrap(), "gh");
    }

    #[test]
    fn greek_final_sigma_collapses_in_uppercase() {
        // Lowercase keeps ς and σ apart, so lowercase round-trips exactly …
        assert_eq!(convert_by_id("ws", "us", "el").unwrap(), "ςσ");
        assert_eq!(convert_by_id("ςσ", "el", "us").unwrap(), "ws");
        // … but both uppercase to Σ, which is genuinely ambiguous.
        assert_eq!(convert_by_id("WS", "us", "el").unwrap(), "ΣΣ");
    }

    #[test]
    fn greek_accents_roundtrip_through_dead_keys() {
        // ά is `;` then `a` on the Greek layout, so on a US keyboard the same
        // keystrokes read ";a" — and must convert back to the accented letter.
        assert_eq!(convert_by_id("ά", "el", "us").unwrap(), ";a");
        assert_eq!(convert_by_id(";a", "us", "el").unwrap(), "ά");
        let word = "μία";
        let latin = convert_by_id(word, "el", "us").unwrap();
        assert_eq!(convert_by_id(&latin, "us", "el").unwrap(), word);
    }

    #[test]
    fn french_circumflex_roundtrips() {
        let word = "être";
        let latin = convert_by_id(word, "fr", "us").unwrap();
        assert_eq!(convert_by_id(&latin, "us", "fr").unwrap(), word);
    }

    #[test]
    fn trailing_dead_key_shows_as_bare_accent() {
        // Pressing `;` on Greek and stopping leaves the accent visible.
        assert_eq!(convert_by_id(";", "us", "el").unwrap(), "΄");
    }

    #[test]
    fn coverage_detects_foreign_text() {
        let us = layout("us").unwrap();
        let ru = layout("ru").unwrap();
        assert!(us.coverage("hello") > 0.99);
        assert!(us.coverage("привет") < 0.01);
        assert!(ru.coverage("привет") > 0.99);
    }
}
