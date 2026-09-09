#!/usr/bin/env bash
# Download word-frequency lists used to train Rekey's language models.
#
# Source: hermitdave/FrequencyWords (MIT), built from the OpenSubtitles corpus.
# Subtitles are transcribed speech, so these lists are unusually rich in slang,
# contractions and profanity — exactly the vocabulary a layout switcher has to
# recognise and that dictionary-based word lists miss.
set -euo pipefail

BASE="https://raw.githubusercontent.com/hermitdave/FrequencyWords/master/content/2018"
OUT="$(dirname "$0")/.cache"
mkdir -p "$OUT"

LANGS=(en ru uk he ar el de fr es tr)

for lang in "${LANGS[@]}"; do
  dest="$OUT/${lang}_50k.txt"
  if [[ -s "$dest" ]]; then
    echo "cached   $lang ($(wc -l < "$dest" | tr -d ' ') lines)"
    continue
  fi
  if curl -fsS "$BASE/$lang/${lang}_50k.txt" -o "$dest"; then
    echo "fetched  $lang ($(wc -l < "$dest" | tr -d ' ') lines)"
  else
    echo "MISSING  $lang" >&2
    rm -f "$dest"
  fi
done
