//! Debug helper: show exactly why the detector reached its verdict.
//!
//! `cargo run --example explain -- <active-layout> <word> [layouts...]`

use rekey_core::config::Config;
use rekey_core::detect::Detector;
use rekey_core::model::Models;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: explain <active-layout> <word> [layout ...]");
        std::process::exit(1);
    }
    let active = args[0].clone();
    let word = args[1].clone();
    let mut layouts: Vec<String> = args[2..].to_vec();
    if layouts.is_empty() {
        layouts = vec!["us".into(), "ru".into()];
    }
    if !layouts.contains(&active) {
        layouts.push(active.clone());
    }

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/models");
    let models = Models::load_dir(&dir).expect("models");
    let det = Detector::new(
        models,
        Config {
            layouts: layouts.clone(),
            ..Config::default()
        },
    );

    println!("word     {word:?}   active={active}   layouts={layouts:?}");

    let from = rekey_core::layout(&active).expect("active layout");
    for alt in det.config.alternatives(&active) {
        let to = rekey_core::layout(alt).unwrap();
        let conv = rekey_core::convert(&word, from, to);
        let src_score = det.models.get(from.def.lang).map(|m| m.score(&word));
        let dst_score = det.models.get(to.def.lang).map(|m| m.score(&conv));
        println!(
            "  -> {alt:<3} {conv:<24} src[{}]={:>7}  dst[{}]={:>7}  all-alpha={}  coverage={:.2}",
            from.def.lang,
            src_score
                .map(|s| format!("{s:.2}"))
                .unwrap_or("none".into()),
            to.def.lang,
            dst_score
                .map(|s| format!("{s:.2}"))
                .unwrap_or("none".into()),
            conv.chars().all(|c| c.is_alphabetic()),
            to.coverage(&conv),
        );
    }
    println!("verdict  {:?}", det.evaluate(&word, &active));
}
