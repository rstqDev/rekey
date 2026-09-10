#!/usr/bin/env python3
"""Verify every queued post fits X's limit. Run in CI so a long draft cannot
sit in the rotation waiting to fail at posting time."""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import x_client as x  # noqa: E402

ROOT = Path(__file__).resolve().parent


def main() -> int:
    content = json.loads((ROOT / "content.json").read_text())
    repo = content["repo"]
    cards = {p.stem for p in (ROOT / "out").glob("*.png")}

    failures = 0
    seen = set()
    for post in content["posts"]:
        pid = post["id"]
        if pid in seen:
            print(f"  DUPE {pid}: id used more than once")
            failures += 1
        seen.add(pid)

        text = post["text"].replace("{repo}", repo)
        n = x.weighted_length(text)
        status = "ok  "
        if n > x.TWEET_LIMIT:
            status = "LONG"
            failures += 1
        note = ""
        if post.get("card") and post["card"] not in cards:
            note = f"  (card {post['card']}.png not rendered)"
        print(f"  {status} {pid:<16} {n:>3}/{x.TWEET_LIMIT}{note}")

    print(f"\n{len(content['posts'])} posts, {failures} problem(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
