//! Behavioural tests against the actual trained models in `data/models`.
//!
//! Unit tests cover the mechanics; these cover the thing users care about —
//! does it fix real mistakes, and does it keep its hands off everything else.

use rekey_core::config::{Config, Sensitivity};
use rekey_core::detect::{Detector, Skip, Verdict};
use rekey_core::model::Models;
use std::path::PathBuf;

fn models_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/models")
}

fn detector(layouts: &[&str]) -> Detector {
    let models = Models::load_dir(&models_dir()).expect("models load");
    assert!(
        !models.is_empty(),
        "no trained models found in {} — run `cargo run -p modelgen -- tools/.cache crates/rekey-core/data/models`",
        models_dir().display()
    );
    let config = Config {
        layouts: layouts.iter().map(|s| s.to_string()).collect(),
        ..Config::default()
    };
    Detector::new(models, config)
}

/// Type `intended` (a real word of `intended_layout`) while `typed_on` is the
/// active layout, and assert the detector recovers it.
///
/// Generating the mistyped form from the layout tables rather than writing it
/// by hand keeps the tests honest: a hand-typed "ghbdtn" is easy to get subtly
/// wrong, and the resulting failure looks like a detector bug.
fn assert_recovers(d: &Detector, intended: &str, intended_layout: &str, typed_on: &str) {
    let mistyped = rekey_core::convert_by_id(intended, intended_layout, typed_on)
        .unwrap_or_else(|| panic!("cannot render {intended:?} on {typed_on}"));
    match d.evaluate(&mistyped, typed_on) {
        Verdict::Switch(c) => assert_eq!(
            c.converted, intended,
            "{intended:?} typed on {typed_on} came out as {mistyped:?}, \
             which the detector turned into {:?} rather than {intended:?}",
            c.converted
        ),
        other => panic!(
            "{intended:?} typed on {typed_on} looks like {mistyped:?}; \
             expected recovery, got {other:?}"
        ),
    }
}

fn assert_switches(d: &Detector, word: &str, active: &str, expect: &str) {
    match d.evaluate(word, active) {
        Verdict::Switch(c) => assert_eq!(
            c.converted, expect,
            "{word:?} on {active} converted to {:?}, expected {expect:?}",
            c.converted
        ),
        other => panic!("{word:?} on {active}: expected a switch to {expect:?}, got {other:?}"),
    }
}

fn assert_keeps(d: &Detector, word: &str, active: &str) {
    match d.evaluate(word, active) {
        Verdict::Switch(c) => panic!(
            "{word:?} on {active} was wrongly converted to {:?} (delta {:.2})",
            c.converted, c.delta
        ),
        _ => {}
    }
}

// -- the headline case ------------------------------------------------------

#[test]
fn fixes_russian_typed_on_english_layout() {
    let d = detector(&["us", "ru"]);
    for w in ["привет", "спасибо", "хорошо", "завтра", "работа"] {
        assert_recovers(&d, w, "ru", "us");
    }
}

#[test]
fn fixes_words_whose_keys_are_punctuation_on_the_wrong_layout() {
    // Russian б and ю live on the `,` and `.` keys, so "спасибо" mistyped on a
    // US layout reads "cgfcb,j" — commas and all. A naive "letters only" filter
    // throws away exactly the input this app exists to fix.
    let d = detector(&["us", "ru"]);
    assert_eq!(
        rekey_core::convert_by_id("спасибо", "ru", "us").unwrap(),
        "cgfcb,j"
    );
    assert_recovers(&d, "спасибо", "ru", "us");
    assert_recovers(&d, "любовь", "ru", "us");
}

#[test]
fn fixes_english_typed_on_russian_layout() {
    let d = detector(&["us", "ru"]);
    for w in ["hello", "meeting", "tomorrow", "keyboard"] {
        assert_recovers(&d, w, "us", "ru");
    }
}

#[test]
fn recognises_slang_not_found_in_dictionaries() {
    let d = detector(&["us", "ru"]);
    // The words a formal dictionary would miss and a subtitle corpus has in
    // abundance — the whole reason for the corpus choice.
    for w in ["норм", "чувак", "офигеть", "кайф"] {
        assert_recovers(&d, w, "ru", "us");
    }
    for w in ["gonna", "wanna", "dude", "yeah"] {
        assert_recovers(&d, w, "us", "ru");
    }
}

// -- the expensive mistakes -------------------------------------------------

#[test]
fn leaves_real_english_alone() {
    let d = detector(&["us", "ru"]);
    for w in [
        "hello", "world", "keyboard", "the", "meeting", "tomorrow", "because",
        "language", "switch", "happy", "friend", "computer",
    ] {
        assert_keeps(&d, w, "us");
    }
}

#[test]
fn leaves_real_russian_alone() {
    let d = detector(&["us", "ru"]);
    for w in ["привет", "спасибо", "хорошо", "человек", "работа", "сегодня"] {
        assert_keeps(&d, w, "ru");
    }
}

#[test]
fn never_touches_passwords_or_identifiers() {
    let d = detector(&["us", "ru"]);
    // Anything with a digit or symbol is refused outright.
    for s in [
        "hunter2", "P@ssw0rd", "git@github.com", "/usr/local/bin", "--force",
        "v1.2.3", "user_name", "#hashtag", "a1b2c3",
    ] {
        match d.evaluate(s, "us") {
            Verdict::Keep(Skip::NotAWord) => {}
            other => panic!("{s:?} should be refused as not-a-word, got {other:?}"),
        }
    }
}

#[test]
fn leaves_acronyms_alone() {
    let d = detector(&["us", "ru"]);
    for s in ["HTTP", "NASA", "SQL", "API"] {
        match d.evaluate(s, "us") {
            Verdict::Keep(Skip::AllCaps) => {}
            other => panic!("{s:?} should be skipped as all-caps, got {other:?}"),
        }
    }
}

#[test]
fn respects_user_exceptions() {
    let models = Models::load_dir(&models_dir()).unwrap();
    let config = Config {
        layouts: vec!["us".into(), "ru".into()],
        exceptions: vec!["ghbdtn".into()],
        ..Config::default()
    };
    let d = Detector::new(models, config);
    match d.evaluate("ghbdtn", "us") {
        Verdict::Keep(Skip::UserException) => {}
        other => panic!("exception word should be kept, got {other:?}"),
    }
}

#[test]
fn very_short_words_need_a_dictionary_hit() {
    let d = detector(&["us", "ru"]);
    // Random two-letter noise is never converted on letter-shape alone.
    assert_keeps(&d, "qx", "us");
    assert_keeps(&d, "zq", "us");
}

#[test]
fn three_layouts_do_not_degrade_the_two_layout_result() {
    // Adding languages must not make existing pairs worse.
    let two = detector(&["us", "ru"]);
    let many = detector(&["us", "ru", "uk", "he", "el"]);
    for w in ["привет", "спасибо", "хорошо"] {
        assert_recovers(&two, w, "ru", "us");
        assert_recovers(&many, w, "ru", "us");
    }
}

// -- other language pairs ---------------------------------------------------

#[test]
fn fixes_ukrainian() {
    let d = detector(&["us", "uk"]);
    for w in ["привіт", "дуже", "будь", "ласка"] {
        assert_recovers(&d, w, "uk", "us");
    }
}

#[test]
fn fixes_hebrew() {
    let d = detector(&["us", "he"]);
    for w in ["שלום", "תודה", "בסדר"] {
        assert_recovers(&d, w, "he", "us");
    }
    // Hebrew is unicameral: every letter reports as neither upper nor lower
    // case, so the acronym guard must not treat whole words as shouting.
    for w in ["hello", "please", "tomorrow"] {
        assert_recovers(&d, w, "us", "he");
    }
}

#[test]
fn fixes_arabic() {
    let d = detector(&["us", "ar"]);
    for w in ["شكرا", "مرحبا", "كثير"] {
        assert_recovers(&d, w, "ar", "us");
    }
}

#[test]
fn fixes_greek_including_accented_words() {
    let d = detector(&["us", "el"]);
    // Greek accents are dead keys, so these words contain a `;` when mistyped.
    for w in ["γεια", "ευχαριστώ", "καλημέρα", "μία"] {
        assert_recovers(&d, w, "el", "us");
    }
}

#[test]
fn latin_pairs_are_held_to_a_higher_bar() {
    // German on a US layout only differs by y/z and the accented keys, so the
    // detector must be conservative or it will churn ordinary English.
    let d = detector(&["us", "de"]);
    for w in ["yellow", "lazy", "zone", "system", "analyze"] {
        assert_keeps(&d, w, "us");
    }
}

#[test]
fn three_layouts_pick_the_right_target() {
    let d = detector(&["us", "ru", "uk"]);
    // Distinctly Ukrainian: і and є have no Russian equivalent on those keys.
    assert_recovers(&d, "привіт", "uk", "us");
    // Distinctly Russian.
    assert_recovers(&d, "привет", "ru", "us");
}

#[test]
fn sensitivity_changes_how_eager_it_is() {
    let models = Models::load_dir(&models_dir()).unwrap();
    let mk = |s| {
        Detector::new(
            models.clone(),
            Config {
                layouts: vec!["us".into(), "ru".into()],
                sensitivity: s,
                ..Config::default()
            },
        )
    };
    let cautious = mk(Sensitivity::Cautious);
    let eager = mk(Sensitivity::Eager);
    // Whatever the corpus says, eager must never be stricter than cautious.
    let mut eager_switches = 0;
    let mut cautious_switches = 0;
    for w in ["ghbdtn", "yjhv", "rfr", "ds", "ntcn", "vjq", "yt"] {
        if eager.evaluate(w, "us").is_switch() {
            eager_switches += 1;
        }
        if cautious.evaluate(w, "us").is_switch() {
            cautious_switches += 1;
        }
    }
    assert!(
        eager_switches >= cautious_switches,
        "eager ({eager_switches}) should switch at least as often as cautious ({cautious_switches})"
    );
}
