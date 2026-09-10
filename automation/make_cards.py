#!/usr/bin/env python3
"""Render Rekey's promotional cards to PNG using headless Chrome.

One HTML template (cards/card.html) is driven by query parameters, so every
card shares the same typography and brand treatment and none of them is a
hand-made image that will drift out of date.

    python3 automation/make_cards.py            # render all cards
    python3 automation/make_cards.py swap-ru    # render one

Output lands in automation/out/<slug>.png at 1600x900, the 16:9 that X shows
without cropping.
"""

import json
import shutil
import subprocess
import sys
import tempfile
import urllib.parse
from pathlib import Path

ROOT = Path(__file__).resolve().parent
TEMPLATE = ROOT / "cards/card.html"
OUT = ROOT / "out"

WIDTH, HEIGHT = 1600, 900

# Shown in every card's footer. Point this at a custom domain once there is
# one; until then the repository is the real home.
SITE_URL = "github.com/rstqDev/rekey"

CHROME_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
]

# Every card the rotation can draw on. Keep these factual: the accuracy figures
# are produced by `cargo run -p eval` and should be re-checked when the models
# or thresholds change.
CARDS = {
    "swap-ru": {
        "wrong": "ghbdtn",
        "right": "привет",
        "sub": "Rekey spots words typed on the wrong keyboard layout and fixes them as you type.",
    },
    "swap-ru-thanks": {
        "wrong": "cgfcb,j",
        "right": "спасибо",
        "sub": "Even when the wrong-layout spelling is full of punctuation.",
    },
    "swap-en": {
        "wrong": "руддщ",
        "right": "hello",
        "sub": "It works in both directions, in every app, with no setup.",
    },
    "swap-he": {
        "wrong": "akuo",
        "right": "שלום",
        "sub": "Hebrew, Arabic, Greek, Russian and Ukrainian — five scripts, one shortcut you never press.",
    },
    "swap-slang": {
        "wrong": "yjhv xedfr",
        "right": "норм чувак",
        "sub": "Trained on subtitles, not dictionaries — so it knows slang too.",
    },
    "accuracy": {
        "headline": "The number that actually matters",
        "n1": "0.00%",
        "l1": "correctly-typed words wrecked",
        "n2": "99%",
        "l2": "genuine mistakes caught",
        "sub": "Measured across 54,000 words. A switcher that is sometimes wrong is worse than none.",
    },
    "privacy": {
        "headline": "It never phones home",
        "sub": "No telemetry, no account, no network calls. The language models ship inside the app — 4.6 MB for ten languages.",
    },
    "open-source": {
        "headline": "You can read every line",
        "sub": "Rekey watches your keyboard, so it is MIT licensed and fully open. Trust should be checkable.",
    },
}


def find_chrome() -> str:
    for path in CHROME_CANDIDATES:
        if Path(path).exists():
            return path
    found = shutil.which("google-chrome") or shutil.which("chromium")
    if found:
        return found
    sys.exit(
        "No Chrome or Chromium found. Install one, or add its path to "
        "CHROME_CANDIDATES in this script."
    )


def render(chrome: str, profile: str, slug: str, params: dict) -> Path:
    """Render one card. The profile directory is shared across cards: creating
    a fresh one per render makes Chrome redo first-run setup every time, which
    turns a one-second job into a minute-long one."""
    OUT.mkdir(parents=True, exist_ok=True)
    dest = OUT / f"{slug}.png"
    url = TEMPLATE.as_uri() + "?" + urllib.parse.urlencode(params)

    result = subprocess.run(
        [
            chrome,
            # Legacy headless: the "new" mode does not exit after --screenshot.
            "--headless",
            "--disable-gpu",
            "--hide-scrollbars",
            "--force-device-scale-factor=1",
            # Without these, each launch stalls on first-run setup and
            # background network probes before it draws anything.
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-sync",
            "--disable-extensions",
            "--disable-component-update",
            f"--user-data-dir={profile}",
            f"--window-size={WIDTH},{HEIGHT}",
            f"--screenshot={dest}",
            "--virtual-time-budget=800",
            url,
        ],
        capture_output=True,
        # Chrome can be slow to start under a restricted sandbox; on a normal
        # machine or in CI a card renders in about a second.
        timeout=240,
    )
    if not dest.exists():
        raise RuntimeError(
            f"Chrome produced no image for {slug}:\n"
            + result.stderr.decode(errors="replace")[-800:]
        )
    return dest


def main() -> int:
    wanted = sys.argv[1:] or list(CARDS)
    unknown = [slug for slug in wanted if slug not in CARDS]
    if unknown:
        print(f"unknown card(s): {', '.join(unknown)}", file=sys.stderr)
        print(f"available: {', '.join(CARDS)}", file=sys.stderr)
        return 1

    chrome = find_chrome()
    manifest = {}
    with tempfile.TemporaryDirectory() as profile:
        for slug in wanted:
            try:
                dest = render(chrome, profile, slug, {**CARDS[slug], "url": SITE_URL})
            except (subprocess.TimeoutExpired, RuntimeError) as e:
                print(f"  {slug:<18} FAILED: {e}", file=sys.stderr)
                return 1
            size = dest.stat().st_size
            manifest[slug] = str(dest.relative_to(ROOT.parent))
            print(f"  {slug:<18} {size // 1024:>4} KB  {dest.name}")

    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
