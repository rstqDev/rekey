# Rekey

**Fixes text typed on the wrong keyboard layout, as you type.**

You meant to type `привет`. Your keyboard was still on English, so you got
`ghbdtn`. Rekey notices and fixes it before you do.

```
ghbdtn        →  привет
cgfcb,j       →  спасибо
руддщ         →  hello
akuo          →  שלום
```

Free, open source, and it runs entirely on your machine. macOS and Windows.

<!-- BADGES -->

---

## Why another layout switcher

Punto Switcher has done this on Windows for twenty years, and nothing
comparable exists on macOS that is both maintained and trustworthy. Rekey is
built around one belief: **a switcher that is occasionally wrong is worse than
no switcher at all.** Being right 95% of the time sounds good until the other
5% lands inside a password field or a git command.

So Rekey is tuned to do nothing unless it is sure. On the language pairs it
supports well, it measures **0.00% false positives** — it never rewrote a
single correctly-typed word across 54,000 test words — while still catching
88–99% of genuine mistakes.

## What it knows

Rekey scores every word against language models trained on the
[OpenSubtitles frequency lists][freq]. Those are transcribed speech, so they
cover the vocabulary formal dictionaries miss — `норм`, `чувак`, `gonna`,
`lol`, `dude` are all in there with real frequencies. When a word is not in the
list at all, a character-trigram model judges how word-like it is, so brand-new
slang still beats gibberish.

| Layout pair | Mistakes caught | Correct words wrongly changed |
|---|---|---|
| English ↔ Russian | 99.0% / 99.5% | 0.00% |
| English ↔ Ukrainian | 93.3% / 99.4% | 0.00% |
| English ↔ Hebrew | 95.5% / 96.5% | 0.00% |
| English ↔ Arabic | 98.9% / 89.7% | 0.00% |
| English ↔ Greek | 95.8% / 88.0% | 0.00% |
| English ↔ German, Spanish, French, Turkish | 1–43% | 0.00% |

Reproduce these numbers yourself:

```bash
./tools/fetch_corpora.sh
cargo run --release -p modelgen -- tools/.cache crates/rekey-core/data/models
cargo run --release -p eval -- tools/.cache crates/rekey-core/data/models
```

### About those last four

Spanish and US QWERTY produce **identical letters** for a–z. Only `ñ` and the
accent keys differ. German swaps `y` and `z` and adds three umlauts; French
moves a handful of keys. So for these, "typed on the wrong layout" barely
exists as a failure mode — the letters come out the same either way, and there
is almost nothing for a detector to notice.

Rekey ships them anyway, marked **limited** in the settings window, and holds
them to a higher bar so they cannot churn ordinary English. What Spanish and
German writers actually want is *diacritic restoration* — turning `cancion`
into `canción` — which is a different feature and is not built yet.

## When it stays out of the way

Every one of these is a deliberate refusal, and each has a test:

- **Password fields.** macOS secure input is respected; Rekey stops tracking
  entirely while it is on.
- **Password managers and terminals.** Excluded by default.
- **Anything that is not prose.** A word containing a digit or a symbol —
  `hunter2`, `P@ssw0rd`, `v1.2.3`, `--force`, `git@github.com`, `/usr/local/bin` —
  is refused outright.
- **Acronyms.** `HTTP`, `NASA`, `SQL` are left alone.
- **Words that are already real.** Rewriting a valid word is the most damaging
  mistake available, so it takes overwhelming evidence.
- **Gibberish.** If neither reading is a real word, Rekey leaves what you typed.
- **After you disagree.** Undo a correction once and that word is never
  corrected again.
- **Caret movement.** Arrow keys, clicks, shortcuts, and app switches all throw
  away the word in progress rather than risk replacing the wrong text.

Rekey **never swallows a keystroke**. The character you typed always lands
first; corrections are applied afterwards as ordinary backspaces and text.
That is why it works in every app without integrating with any of them.

## Privacy

Everything runs locally. No telemetry, no analytics, no network calls.

There is one optional exception, off by default and clearly labelled: **Ask
Claude about ambiguous words.** When you switch it on and supply your own
Anthropic API key, single ambiguous words — never whole sentences, never
anything from an excluded app or a password field — are sent to Anthropic's API
in the background, and the answer is cached locally so each word is sent at
most once. Leave it off and Rekey never opens a socket.

### The log file

Rekey writes a small log beside its settings
(`~/Library/Application Support/app.rekey.desktop/rekey.log` on macOS),
truncated on every launch, so that a misbehaviour can be diagnosed without
guesswork.

**It never records what you type.** Entries note decisions and lengths — "word
of 6 chars on us -> corrected" — never the word itself. A diagnostic that
quietly became a keystroke record would be worse than having no diagnostics at
all, so the distinction is deliberate and worth checking if you change the
logging.

## Install

Download the latest build from [Releases](../../releases).

**macOS** — open the `.dmg`, drag Rekey to Applications, launch it, and grant
**Accessibility** access when asked (System Settings → Privacy & Security →
Accessibility). Quit and reopen once you have. Rekey cannot see your keyboard
without this, and macOS gives no way around it.

The build is not notarized yet, so the first launch needs a right-click → Open.

**Windows** — run the installer. No special permissions needed.

## Build from source

```bash
git clone https://github.com/rstqDev/rekey
cd rekey
./tools/fetch_corpora.sh                                              # word lists
cargo run --release -p modelgen -- tools/.cache crates/rekey-core/data/models
cargo tauri build                                                     # or: cargo tauri dev
```

Requires Rust 1.77+. On macOS you also need Xcode command line tools.

### Signing on macOS

An unsigned or ad-hoc-signed build works, with one sharp edge: macOS ties the
Accessibility grant to the app's *code signature*, not its path. Ad-hoc
signatures change on every build, so each rebuild silently invalidates the
grant — System Settings keeps showing Rekey as enabled while the app is told it
has no permission. Signing with a real certificate fixes it, because signed
apps are identified by team and bundle id instead.

```bash
security find-identity -v -p codesigning        # find yours
export APPLE_SIGNING_IDENTITY="Apple Development: you@example.com (XXXXXXXXXX)"
cargo tauri build
```

If the grant does get into a bad state, clear it and grant once more:

```bash
tccutil reset Accessibility app.rekey.desktop
```

## How it works

```
keystroke ─→ hook ─→ buffer ─→ detector ─→ engine ─→ injector
             (OS)    (word)    (scoring)   (safety)   (backspace + type)
```

1. **`rekey-core/layout`** describes ten keyboard layouts by *physical key
   position*, so converting between them is a table lookup. It handles dead
   keys (Greek accents are `;` then a vowel) and the genuinely ambiguous cases
   — Arabic reaches `لا` two different ways, Greek uppercases both `ς` and `σ`
   to `Σ`.
2. **`rekey-core/model`** scores a string as a word in a language: a hashed
   word table with a character-trigram fallback. 4.6 MB for ten languages.
3. **`rekey-core/detect`** re-types the word on every enabled layout, scores
   each reading, and applies the guards above.
4. **`rekey-core/engine`** decides whether acting is *safe* right now.
5. **`rekey-hook`** is the only platform-specific code: a listen-only
   `CGEventTap` on macOS, `WH_KEYBOARD_LL` on Windows.

The split matters: everything interesting is platform-independent and tested
without a keyboard, an OS, or a network.

## Tests

```bash
cargo test --workspace
```

## Contributing

Adding a language means adding one row to `LAYOUTS` in
[`crates/rekey-core/src/layout.rs`](crates/rekey-core/src/layout.rs) — 47
space-separated tokens, one per physical key — and a frequency list to
`tools/fetch_corpora.sh`. The test suite will tell you if the table is wrong.

## Support

Rekey is free and always will be. If it saves you from retyping one more word,
[buy me a coffee](https://ko-fi.com/rekeyapp). ☕

## Licence

MIT. See [LICENSE](LICENSE).

Word frequency data from [hermitdave/FrequencyWords][freq] (MIT), built from
the OpenSubtitles corpus.

[freq]: https://github.com/hermitdave/FrequencyWords
