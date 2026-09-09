//! Turns `word count` frequency lists into Rekey's binary `.rkm` models.
//!
//! Usage: `modelgen <corpus-dir> <output-dir>`
//!
//! Every `<lang>_50k.txt` in the corpus directory becomes `<lang>.rkm`.

use rekey_core::model::Model;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Words shorter than this carry almost no evidence and mostly add noise to the
/// trigram table, but they are still kept in the word table so the detector can
/// recognise "a", "я", "и" as real words and refuse to convert them.
const MIN_TRIGRAM_LEN: usize = 2;

/// Entries below this count are overwhelmingly OCR noise and foreign-script
/// fragments in the OpenSubtitles lists.
const MIN_COUNT: u64 = 2;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: modelgen <corpus-dir> <output-dir>");
        return ExitCode::FAILURE;
    }
    let corpus = PathBuf::from(&args[1]);
    let out = PathBuf::from(&args[2]);

    let mut files: Vec<PathBuf> = match std::fs::read_dir(&corpus) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with("_50k.txt"))
            })
            .collect(),
        Err(e) => {
            eprintln!("cannot read corpus dir {}: {e}", corpus.display());
            return ExitCode::FAILURE;
        }
    };
    files.sort();

    if files.is_empty() {
        eprintln!("no *_50k.txt files in {}", corpus.display());
        return ExitCode::FAILURE;
    }

    let mut total_bytes = 0u64;
    for path in files {
        let lang = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.split('_').next())
            .unwrap_or("??")
            .to_string();

        match build(&path, &lang) {
            Ok(model) => {
                let dest = out.join(format!("{lang}.rkm"));
                if let Err(e) = model.save(&dest) {
                    eprintln!("{lang}: cannot write {}: {e}", dest.display());
                    return ExitCode::FAILURE;
                }
                let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                total_bytes += size;
                println!(
                    "{lang}: {:>6} words, {:>6} trigrams, {:>5} KB",
                    model.word_count(),
                    model.trigram_count(),
                    size / 1024
                );
            }
            Err(e) => {
                eprintln!("{lang}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    println!("total: {} KB", total_bytes / 1024);
    ExitCode::SUCCESS
}

fn build(path: &Path, lang: &str) -> std::io::Result<Model> {
    let text = std::fs::read_to_string(path)?;
    let mut counts: HashMap<String, u64> = HashMap::new();

    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(word), Some(count)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(count) = count.parse::<u64>() else {
            continue;
        };
        if count < MIN_COUNT {
            continue;
        }
        let w = rekey_core::model::normalize(word);
        // Drop entries containing digits or punctuation: they are artefacts of
        // subtitle formatting, not vocabulary.
        if w.is_empty() || w.chars().any(|c| !c.is_alphabetic()) {
            continue;
        }
        if w.chars().count() < MIN_TRIGRAM_LEN && count < 1000 {
            continue;
        }
        *counts.entry(w).or_insert(0) += count;
    }

    if counts.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "no usable entries in corpus",
        ));
    }
    Ok(Model::train(lang, &counts))
}
