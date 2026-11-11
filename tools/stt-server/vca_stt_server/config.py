# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
"""Settings, read from ``VCA_STT_*`` environment variables.

Every setting has a safe default; the server binds to loopback unless told
otherwise. See the README for the full table.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Mapping

MiB = 1024 * 1024


def _int(env: Mapping[str, str], name: str, default: int) -> int:
    raw = env.get(name, "").strip()
    if not raw:
        return default
    try:
        return int(raw)
    except ValueError:
        raise SystemExit(f"{name} must be an integer, got {raw!r}") from None


def _float(env: Mapping[str, str], name: str, default: float) -> float:
    raw = env.get(name, "").strip()
    if not raw:
        return default
    try:
        return float(raw)
    except ValueError:
        raise SystemExit(f"{name} must be a number, got {raw!r}") from None


def _bool(env: Mapping[str, str], name: str, default: bool) -> bool:
    raw = env.get(name, "").strip().lower()
    if not raw:
        return default
    return raw in ("1", "true", "yes", "on")


@dataclass
class Settings:
    """Server settings. ``from_env`` builds them from the environment."""

    # Network
    host: str = "127.0.0.1"
    port: int = 8770
    # Data folder: tokens.json and requests.log live here.
    home: Path = field(default_factory=lambda: Path.cwd())
    tokens_file: Path | None = None
    # "-" logs to standard error (for journald) instead of a file.
    log_file: str | None = None
    log_max_bytes: int = 5 * MiB
    log_backups: int = 3

    # Model
    model: str = ""            # empty: large-v3-turbo on a GPU, small on a CPU
    device: str = "auto"       # auto, cuda or cpu
    compute_type: str = ""     # empty: float16 on a GPU, int8 on a CPU
    beam_size: int = 5
    vad_filter: bool = True
    download_root: str | None = None

    # Limits
    max_bytes: int = 25 * MiB
    max_audio_seconds: float = 600.0
    rate_per_min: int = 120
    max_queue: int = 4          # jobs waiting behind the running one
    fail_limit: int = 20        # failed auths per IP ...
    fail_window: float = 600.0  # ... within this many seconds ...
    lockout_seconds: float = 600.0  # ... lock that IP out for this long

    # Proxies whose X-Real-IP / X-Forwarded-For headers are trusted.
    trusted_proxies: frozenset[str] = frozenset({"127.0.0.1", "::1"})
    landing_page: bool = True

    def __post_init__(self) -> None:
        self.home = Path(self.home)
        if self.tokens_file is None:
            self.tokens_file = self.home / "tokens.json"
        if self.log_file is None:
            self.log_file = str(self.home / "requests.log")

    @classmethod
    def from_env(cls, env: Mapping[str, str] | None = None) -> "Settings":
        env = os.environ if env is None else env
        home = Path(env.get("VCA_STT_HOME") or Path.cwd())
        proxies = env.get("VCA_STT_TRUSTED_PROXIES", "127.0.0.1,::1")
        return cls(
            host=env.get("VCA_STT_HOST", "127.0.0.1"),
            port=_int(env, "VCA_STT_PORT", 8770),
            home=home,
            tokens_file=Path(env["VCA_STT_TOKENS"]) if env.get("VCA_STT_TOKENS") else None,
            log_file=env.get("VCA_STT_LOG_FILE") or None,
            model=env.get("VCA_STT_MODEL", ""),
            device=env.get("VCA_STT_DEVICE", "auto"),
            compute_type=env.get("VCA_STT_COMPUTE", ""),
            beam_size=_int(env, "VCA_STT_BEAM_SIZE", 5),
            vad_filter=_bool(env, "VCA_STT_VAD", True),
            download_root=env.get("VCA_STT_MODEL_DIR") or None,
            max_bytes=_int(env, "VCA_STT_MAX_BYTES", 25 * MiB),
            max_audio_seconds=_float(env, "VCA_STT_MAX_AUDIO_SECONDS", 600.0),
            rate_per_min=_int(env, "VCA_STT_RATE_PER_MIN", 120),
            max_queue=_int(env, "VCA_STT_MAX_QUEUE", 4),
            fail_limit=_int(env, "VCA_STT_FAIL_LIMIT", 20),
            fail_window=_float(env, "VCA_STT_FAIL_WINDOW", 600.0),
            lockout_seconds=_float(env, "VCA_STT_LOCKOUT", 600.0),
            trusted_proxies=frozenset(p.strip() for p in proxies.split(",") if p.strip()),
            landing_page=_bool(env, "VCA_STT_LANDING", True),
        )
