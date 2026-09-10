#!/usr/bin/env python3
"""A very small X (Twitter) API client: OAuth 1.0a signing, media upload, post.

Standard library only — no dependencies to install, audit or keep patched, and
the signing is short enough to read in one sitting.

Credentials come from the environment and are never written to disk:

    X_API_KEY             consumer key
    X_API_SECRET          consumer secret
    X_ACCESS_TOKEN        access token       (user context)
    X_ACCESS_SECRET       access token secret

Create these at developer.x.com under a project with **Read and write**
permission; a read-only token will authenticate happily and then fail to post.
"""

import base64
import hashlib
import hmac
import json
import mimetypes
import os
import secrets
import time
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

TWEET_URL = "https://api.x.com/2/tweets"
MEDIA_URL = "https://upload.x.com/1.1/media/upload.json"

# X counts every URL as this many characters regardless of real length.
URL_WEIGHT = 23
TWEET_LIMIT = 280


class XError(RuntimeError):
    pass


class Credentials:
    __slots__ = ("api_key", "api_secret", "access_token", "access_secret")

    def __init__(self, api_key, api_secret, access_token, access_secret):
        self.api_key = api_key
        self.api_secret = api_secret
        self.access_token = access_token
        self.access_secret = access_secret

    @classmethod
    def from_env(cls):
        names = ("X_API_KEY", "X_API_SECRET", "X_ACCESS_TOKEN", "X_ACCESS_SECRET")
        values = [os.environ.get(n, "").strip() for n in names]
        missing = [n for n, v in zip(names, values) if not v]
        if missing:
            raise XError(
                "missing credentials: " + ", ".join(missing) +
                "\nSet them in the environment; see automation/README.md."
            )
        return cls(*values)


def _quote(value: str) -> str:
    """Percent-encoding as OAuth 1.0a defines it (RFC 3986, nothing exempt)."""
    return urllib.parse.quote(str(value), safe="~")


def _sign(creds: Credentials, method: str, url: str, params: dict) -> str:
    """Build the OAuth Authorization header for one request.

    `params` must contain the request's query parameters and, for
    form-encoded bodies, the body parameters. Multipart bodies contribute
    nothing to the signature, which is why media upload passes an empty dict.
    """
    oauth = {
        "oauth_consumer_key": creds.api_key,
        "oauth_nonce": secrets.token_hex(16),
        "oauth_signature_method": "HMAC-SHA1",
        "oauth_timestamp": str(int(time.time())),
        "oauth_token": creds.access_token,
        "oauth_version": "1.0",
    }

    signing_params = {**params, **oauth}
    encoded = "&".join(
        f"{_quote(k)}={_quote(v)}" for k, v in sorted(signing_params.items())
    )
    base = "&".join([method.upper(), _quote(url), _quote(encoded)])
    key = f"{_quote(creds.api_secret)}&{_quote(creds.access_secret)}".encode()
    digest = hmac.new(key, base.encode(), hashlib.sha1).digest()
    oauth["oauth_signature"] = base64.b64encode(digest).decode()

    return "OAuth " + ", ".join(
        f'{_quote(k)}="{_quote(v)}"' for k, v in sorted(oauth.items())
    )


def _request(req: urllib.request.Request) -> dict:
    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            body = response.read().decode()
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")
        raise XError(f"HTTP {e.code} from {req.full_url}: {detail}") from None
    except urllib.error.URLError as e:
        raise XError(f"cannot reach {req.full_url}: {e.reason}") from None
    return json.loads(body) if body else {}


def upload_media(creds: Credentials, path: Path) -> str:
    """Upload one image and return its media id."""
    data = path.read_bytes()
    mime = mimetypes.guess_type(path.name)[0] or "image/png"
    boundary = uuid.uuid4().hex

    body = b"".join([
        f"--{boundary}\r\n".encode(),
        f'Content-Disposition: form-data; name="media"; filename="{path.name}"\r\n'.encode(),
        f"Content-Type: {mime}\r\n\r\n".encode(),
        data,
        f"\r\n--{boundary}--\r\n".encode(),
    ])

    # Multipart bodies are excluded from the OAuth signature base string.
    header = _sign(creds, "POST", MEDIA_URL, {})
    req = urllib.request.Request(
        MEDIA_URL,
        data=body,
        method="POST",
        headers={
            "Authorization": header,
            "Content-Type": f"multipart/form-data; boundary={boundary}",
        },
    )
    result = _request(req)
    media_id = result.get("media_id_string")
    if not media_id:
        raise XError(f"upload succeeded but returned no media id: {result}")
    return media_id


def post_tweet(creds: Credentials, text: str, media_ids=None, reply_to=None) -> dict:
    payload = {"text": text}
    if media_ids:
        payload["media"] = {"media_ids": list(media_ids)}
    if reply_to:
        payload["reply"] = {"in_reply_to_tweet_id": reply_to}

    # A JSON body contributes nothing to the OAuth signature.
    header = _sign(creds, "POST", TWEET_URL, {})
    req = urllib.request.Request(
        TWEET_URL,
        data=json.dumps(payload).encode(),
        method="POST",
        headers={"Authorization": header, "Content-Type": "application/json"},
    )
    return _request(req)


def weighted_length(text: str) -> int:
    """Length as X counts it: every URL costs a flat 23 characters."""
    total = 0
    for token in text.split(" "):
        if token.startswith(("http://", "https://")):
            total += URL_WEIGHT
        else:
            total += len(token)
    # Spaces between tokens.
    return total + max(0, len(text.split(" ")) - 1)


def check_length(text: str) -> None:
    n = weighted_length(text)
    if n > TWEET_LIMIT:
        raise XError(f"post is {n} characters, limit is {TWEET_LIMIT}:\n{text}")
