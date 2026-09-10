# Rekey social automation

Posts Rekey's content rotation to X, with a rendered card attached.

Nothing here posts by accident. `post.py` previews by default and publishes
only when given `--post`; the GitHub Action publishes on its schedule, or on a
manual run where the *publish* box was explicitly ticked.

## Files

| File | What it is |
|---|---|
| `content.json` | The rotation. Each entry has an `id`, `text`, and optional `card`. |
| `post.py` | Previews or publishes the next unposted item. |
| `x_client.py` | Minimal X API client — OAuth 1.0a, standard library only. |
| `make_cards.py` | Renders `cards/card.html` to 1600×900 PNGs with headless Chrome. |
| `check_content.py` | Verifies every queued post fits X's limit. Runs in CI. |
| `state.json` | Which posts have gone out. Created on first publish. |

## Credentials

Create an app at [developer.x.com](https://developer.x.com) inside a project,
and give it **Read and write** permission. A read-only token authenticates
fine and then fails at posting time, which is a confusing way to find out.

Four values are needed:

```bash
export X_API_KEY=…            # consumer key
export X_API_SECRET=…         # consumer secret
export X_ACCESS_TOKEN=…       # access token, user context
export X_ACCESS_SECRET=…      # access token secret
```

For the GitHub Action, add the same four as repository secrets under
**Settings → Secrets and variables → Actions**.

Note: posting through the API requires a paid X API tier. The free tier is
read-only for most endpoints.

## Usage

```bash
python3 automation/make_cards.py        # render all cards to out/
python3 automation/post.py              # preview the next post
python3 automation/post.py --list       # show the rotation and history
python3 automation/post.py --id privacy # preview one specific post
python3 automation/post.py --post       # publish it
```

## Adding a post

Append to `content.json`. Use `{repo}` for the repository URL — X counts every
link as 23 characters regardless of length, and `check_content.py` accounts for
that. Then:

```bash
python3 automation/check_content.py
```

If a post references a `card`, add the corresponding entry to `CARDS` in
`make_cards.py` so the image can be rendered.

## A note on tone

The posts that do well here are the specific ones — a real bug, a real number,
a real limitation. `content.json` leans that way on purpose: it includes the
Hebrew-and-Arabic-read-as-SHOUTING bug and the fact that Spanish support barely
works. Marketing that admits the weak spots is more credible about the strong
ones.
