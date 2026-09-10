//! Precision/recall harness for the detector.
//!
//! Two questions, measured separately because they trade off against each
//! other and only one of them is career-limiting:
//!
//! * **Recall** — of words genuinely typed on the wrong layout, how many does
//!   Rekey fix? Misses are mildly annoying; the user retypes the word.
//! * **Precision** — of words typed correctly, how many does Rekey wreck? Every
//!   one of these is a small betrayal, so the thresholds are tuned to keep the
//!   false-positive rate near zero even at the cost of recall.
//!
//! Usage: `eval <corpus-dir> <models-dir> [sample-size]`

use rekey_core::config::{Config, Sensitivity};
use rekey_core::detect::Detector;
use rekey_core::model::Models;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Pairs we report on. Latin-to-Latin is included precisely because it is the
/// hard case and its numbers should be visibly worse.
const PAIRS: &[(&str, &str)] = &[
    ("us", "ru"),
    ("us", "uk"),
    ("us", "he"),
    ("us", "ar"),
    ("us", "el"),
    ("us", "de"),
    ("us", "fr"),
    ("us", "es"),
    ("us", "tr"),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: eval <corpus-dir> <models-dir> [sample-size]");
        return ExitCode::FAILURE;
    }
    let corpus = PathBuf::from(&args[1]);
    let models_dir = PathBuf::from(&args[2]);
    let sample: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3000);

    let models = match Models::load_dir(&models_dir) {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => {
            eprintln!("no models in {}", models_dir.display());
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("cannot load models: {e}");
            return ExitCode::FAILURE;
        }
    };

    for sensitivity in [
        Sensitivity::Cautious,
        Sensitivity::Balanced,
        Sensitivity::Eager,
    ] {
        println!("\n=== {sensitivity:?} ===");
        println!(
            "{:<10} {:>8} {:>8} {:>10} {:>10}",
            "pair", "recall", "recall", "false-pos", "false-pos"
        );
        println!(
            "{:<10} {:>8} {:>8} {:>10} {:>10}",
            "", "A->B", "B->A", "in A", "in B"
        );

        let mut tot_recall = (0usize, 0usize);
        let mut tot_fp = (0usize, 0usize);

        for (a, b) in PAIRS {
            let config = Config {
                layouts: vec![a.to_string(), b.to_string()],
                sensitivity,
                ..Config::default()
            };
            let det = Detector::new(models.clone(), config);

            let lang_a = rekey_core::layout(a).unwrap().def.lang;
            let lang_b = rekey_core::layout(b).unwrap().def.lang;

            let words_a = load_words(&corpus, lang_a, sample);
            let words_b = load_words(&corpus, lang_b, sample);

            // Recall: take a real word of language B, render the keystrokes as
            // layout A would, and check we recover it.
            let rec_ba = recall(&det, &words_b, b, a);
            let rec_ab = recall(&det, &words_a, a, b);
            // Precision: real words typed on their own correct layout.
            let fp_a = false_positives(&det, &words_a, a);
            let fp_b = false_positives(&det, &words_b, b);

            println!(
                "{:<10} {:>7.1}% {:>7.1}% {:>9.2}% {:>9.2}%",
                format!("{a}<->{b}"),
                pct(rec_ba),
                pct(rec_ab),
                pct(fp_a),
                pct(fp_b),
            );
            tot_recall.0 += rec_ba.0 + rec_ab.0;
            tot_recall.1 += rec_ba.1 + rec_ab.1;
            tot_fp.0 += fp_a.0 + fp_b.0;
            tot_fp.1 += fp_a.1 + fp_b.1;
        }
        println!(
            "{:<10} {:>7.1}% (overall recall)   {:>7.2}% (overall false positives)",
            "TOTAL",
            pct(tot_recall),
            pct(tot_fp)
        );
    }
    ExitCode::SUCCESS
}

fn pct((hit, total): (usize, usize)) -> f64 {
    if total == 0 {
        0.0
    } else {
        100.0 * hit as f64 / total as f64
    }
}

/// Words genuinely meant for `target_layout` but typed while `typed_on` was
/// active. Returns (fixed, attempted).
fn recall(det: &Detector, words: &[String], target_layout: &str, typed_on: &str) -> (usize, usize) {
    let mut fixed = 0;
    let mut total = 0;
    for w in words {
        let Some(mistyped) = rekey_core::convert_by_id(w, target_layout, typed_on) else {
            continue;
        };
        total += 1;
        if let Some(c) = det.evaluate(&mistyped, typed_on).candidate() {
            if det.evaluate(&mistyped, typed_on).is_switch() && c.converted == *w {
                fixed += 1;
            }
        }
    }
    (fixed, total)
}

/// Correctly typed words that the detector wrongly rewrites. Returns
/// (wrecked, attempted).
fn false_positives(det: &Detector, words: &[String], layout: &str) -> (usize, usize) {
    let mut bad = 0;
    let total = words.len();
    for w in words {
        if det.evaluate(w, layout).is_switch() {
            bad += 1;
        }
    }
    (bad, total)
}

fn load_words(corpus: &Path, lang: &str, n: usize) -> Vec<String> {
    let path = corpus.join(format!("{lang}_50k.txt"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("warning: no corpus for {lang} at {}", path.display());
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(rekey_core::model::normalize)
        .filter(|w| w.chars().count() >= 3 && w.chars().all(|c| c.is_alphabetic()))
        .take(n)
        .collect()
}
