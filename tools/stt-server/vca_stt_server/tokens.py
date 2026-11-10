# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
"""Per-user bearer tokens: the token store and the ``tokens`` command.

    python -m vca_stt_server.tokens add <user> [--days N]   print a new token once
    python -m vca_stt_server.tokens list
    python -m vca_stt_server.tokens remove <user>

``add`` on an existing user replaces (rotates) that user's token. Only the
SHA-256 of each token is stored in ``tokens.json`` (owner-only file mode);
the server reloads the file when it changes, so no restart is needed.

The file location comes from ``VCA_STT_TOKENS``, else ``$VCA_STT_HOME/tokens.json``,
else ``./tokens.json``.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import os
import re
import secrets
import sys
import threading
import time
from pathlib import Path

TOKEN_PREFIX = "stt_"
USER_RE = re.compile(r"^[A-Za-z0-9._-]{1,64}$")


def hash_token(token: str) -> str:
    return hashlib.sha256(token.encode("utf-8")).hexdigest()


def new_token() -> str:
    """32 random bytes from the OS CSPRNG, URL-safe (43 characters) plus a prefix."""
    return TOKEN_PREFIX + secrets.token_urlsafe(32)


def load_file(path: Path) -> dict:
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
    except FileNotFoundError:
        return {}
    if not isinstance(data, dict):
        raise ValueError(f"{path} must hold a JSON object")
    return data


def save_file(path: Path, users: dict) -> None:
    """Writes atomically with mode 600 (owner read/write only)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(users, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp, path)


class TokenStore:
    """Maps a presented token to a user name. Reloads the file on change."""

    def __init__(self, path: Path) -> None:
        self.path = Path(path)
        self._lock = threading.Lock()
        self._stamp: tuple | None = None
        self._table: list[tuple[str, str, float | None]] = []  # (sha256, user, expires)

    def _refresh(self) -> list[tuple[str, str, float | None]]:
        try:
            st = os.stat(self.path)
        except OSError:
            with self._lock:
                self._stamp, self._table = None, []
            return []
        # The writer replaces the file atomically, so the inode changes too.
        stamp = (st.st_mtime_ns, st.st_size, st.st_ino)
        with self._lock:
            if stamp != self._stamp:
                table = []
                try:
                    users = load_file(self.path)
                except (OSError, ValueError):
                    users = {}  # unreadable file: nobody gets in until it is fixed
                for user, entry in users.items():
                    if isinstance(entry, str):
                        table.append((entry, user, None))
                    elif isinstance(entry, dict) and isinstance(entry.get("sha256"), str):
                        table.append((entry["sha256"], user, entry.get("expires")))
                self._table, self._stamp = table, stamp
            return self._table

    def user_for(self, token: str) -> str | None:
        if not token:
            return None
        digest = hash_token(token)
        now = time.time()
        found = None
        # Compare against every entry in constant time; do not stop early.
        for sha, user, expires in self._refresh():
            if hmac.compare_digest(sha, digest) and (expires is None or now < expires):
                found = user
        return found


def default_path() -> Path:
    if os.environ.get("VCA_STT_TOKENS"):
        return Path(os.environ["VCA_STT_TOKENS"])
    return Path(os.environ.get("VCA_STT_HOME") or ".") / "tokens.json"


def main(argv: list[str] | None = None, path: Path | None = None) -> int:
    p = argparse.ArgumentParser(prog="python -m vca_stt_server.tokens",
                                description="Manage the users of the speech-to-text server.")
    sub = p.add_subparsers(dest="cmd", required=True)
    add = sub.add_parser("add", help="create or rotate a user's token and print it once")
    add.add_argument("user")
    add.add_argument("--days", type=float, help="expire the token after N days")
    sub.add_parser("list", help="list users and expiry dates (never tokens)")
    rm = sub.add_parser("remove", help="remove a user (revokes the token at once)")
    rm.add_argument("user")
    args = p.parse_args(argv)
    path = path or default_path()
    users = load_file(path)

    if args.cmd == "add":
        if not USER_RE.match(args.user):
            print("user names use letters, digits, '.', '_' and '-' (max 64)", file=sys.stderr)
            return 2
        token = new_token()
        entry: dict = {"sha256": hash_token(token), "created": int(time.time())}
        if args.days is not None:
            if args.days <= 0:
                print("--days must be positive", file=sys.stderr)
                return 2
            entry["expires"] = int(time.time() + args.days * 86400)
        users[args.user] = entry
        save_file(path, users)
        print(token)
        print("Shown only once. Give it to the user over a private channel.", file=sys.stderr)
    elif args.cmd == "list":
        if not users:
            print("(no users)")
        for name in sorted(users):
            entry = users[name]
            exp = entry.get("expires") if isinstance(entry, dict) else None
            if exp is None:
                when = "no expiry"
            else:
                state = "expired" if exp <= time.time() else "expires"
                when = time.strftime(f"{state} %Y-%m-%d %H:%M", time.localtime(exp))
            print(f"{name}\t{when}")
    elif args.cmd == "remove":
        if users.pop(args.user, None) is None:
            print(f"no user {args.user!r}", file=sys.stderr)
            return 1
        save_file(path, users)
    return 0


if __name__ == "__main__":
    sys.exit(main())
