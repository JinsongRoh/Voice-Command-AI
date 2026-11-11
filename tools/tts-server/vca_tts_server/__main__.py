"""Command line: ``python -m vca_tts_server --backend melo``."""

from __future__ import annotations

import argparse
import logging
import os
import sys

from .backends import BACKENDS, BackendError, load_backend

DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 8880
TOKEN_ENV = "VCA_TTS_TOKEN"


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="vca_tts_server",
        description="OpenAI-compatible TTS server for Voice Command AI (MeloTTS or Supertonic).",
    )
    p.add_argument("--backend", choices=BACKENDS, default=os.environ.get("VCA_TTS_BACKEND", "melo"))
    p.add_argument(
        "--host",
        default=os.environ.get("VCA_TTS_HOST", DEFAULT_HOST),
        help="listen address (default 127.0.0.1: this PC only)",
    )
    p.add_argument("--port", type=int, default=int(os.environ.get("VCA_TTS_PORT", DEFAULT_PORT)))
    p.add_argument(
        "--max-chars",
        type=int,
        default=int(os.environ.get("VCA_TTS_MAX_CHARS", "1000")),
        help="longest accepted input",
    )
    p.add_argument(
        "--strict-voices",
        action="store_true",
        help="refuse unknown voices instead of using the default voice",
    )
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    log = logging.getLogger("vca_tts_server")
    # The token comes from the environment only, so it never shows in a
    # process list.
    token = os.environ.get(TOKEN_ENV) or None
    if args.host not in ("127.0.0.1", "localhost", "::1") and token is None:
        log.warning(
            "listening on %s without a token: anyone who can reach this port can use it; "
            "set %s",
            args.host,
            TOKEN_ENV,
        )
    try:
        backend = load_backend(args.backend)
    except BackendError as e:
        log.error("%s", e)
        return 2
    import uvicorn

    from .app import create_app

    app = create_app(backend, token, args.max_chars, args.strict_voices)
    log.info(
        "%s backend ready with %d voices on http://%s:%d/v1 (token %s)",
        backend.name,
        len(backend.voices()),
        args.host,
        args.port,
        "required" if token else "off",
    )
    uvicorn.run(app, host=args.host, port=args.port, log_level="warning")
    return 0


if __name__ == "__main__":
    sys.exit(main())
