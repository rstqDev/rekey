#!/usr/bin/env python3
"""Post the next item in Rekey's content rotation to X.

Safe by default: it prints what it *would* post and exits. Publishing requires
an explicit --post, because an account posting on its own is a thing you should
have to ask for twice.

    python3 automation/post.py                  # preview the next post
    python3 automation/post.py --id privacy     # preview a specific one
    python3 automation/post.py --post           # actually publish it
    python3 automation/post.py --list           # show the rotation and history

State lives in automation/state.json so the rotation survives restarts and the
same post is not published twice.
"""

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import x_client as x  # noqa: E402

ROOT = Path(__file__).resolve().parent
CONTENT = ROOT / "content.json"
STATE = ROOT / "state.json"
CARDS = ROOT / "out"


def load_content() -> dict:
    return json.loads(CONTENT.read_text())


def load_state() -> dict:
    if STATE.exists():
        return json.loads(STATE.read_text())
    return {"posted": []}


def save_state(state: dict) -> None:
    STATE.write_text(json.dumps(state, indent=2) + "\n")


def render(post: dict, repo: str) -> str:
    return post["text"].replace("{repo}", repo)


def next_post(content: dict, state: dict):
    """The first post that has not gone out yet."""
    done = {entry["id"] for entry in state["posted"]}
    for post in content["posts"]:
        if post["id"] not in done:
            return post
    return None


def show(post: dict, text: str, card: Path | None) -> None:
    print("─" * 60)
    print(f"id:     {post['id']}")
    print(f"length: {x.weighted_length(text)}/{x.TWEET_LIMIT}")
    print(f"image:  {card.name if card else '(none)'}")
    print("─" * 60)
    print(text)
    print("─" * 60)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--post", action="store_true",
                        help="actually publish (otherwise this is a preview)")
    parser.add_argument("--id", help="post this specific item instead of the next one")
    parser.add_argument("--list", action="store_true", help="show the rotation and exit")
    args = parser.parse_args()

    content = load_content()
    state = load_state()
    repo = content["repo"]

    if args.list:
        done = {e["id"]: e["at"] for e in state["posted"]}
        for post in content["posts"]:
            when = done.get(post["id"])
            mark = f"posted {when}" if when else "queued"
            print(f"  {post['id']:<16} {mark}")
        return 0

    if args.id:
        post = next((p for p in content["posts"] if p["id"] == args.id), None)
        if post is None:
            print(f"no post with id {args.id!r}", file=sys.stderr)
            return 1
    else:
        post = next_post(content, state)
        if post is None:
            print("Every post in the rotation has gone out. Add more to content.json.")
            return 0

    text = render(post, repo)
    try:
        x.check_length(text)
    except x.XError as e:
        print(e, file=sys.stderr)
        return 1

    card = None
    if post.get("card"):
        candidate = CARDS / f"{post['card']}.png"
        if candidate.exists():
            card = candidate
        else:
            print(
                f"warning: card {candidate.name} is missing — "
                f"run `python3 automation/make_cards.py {post['card']}`",
                file=sys.stderr,
            )

    show(post, text, card)

    if not args.post:
        print("\nPreview only. Re-run with --post to publish.")
        return 0

    try:
        creds = x.Credentials.from_env()
        media_ids = [x.upload_media(creds, card)] if card else None
        result = x.post_tweet(creds, text, media_ids)
    except x.XError as e:
        print(f"\nnot posted: {e}", file=sys.stderr)
        return 1

    tweet_id = result.get("data", {}).get("id", "?")
    print(f"\nposted: https://x.com/i/status/{tweet_id}")

    state["posted"].append({
        "id": post["id"],
        "at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "tweet_id": tweet_id,
    })
    save_state(state)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
