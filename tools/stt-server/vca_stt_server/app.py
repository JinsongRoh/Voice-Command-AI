# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
"""The HTTP API: an OpenAI-compatible subset for speech-to-text.

* ``POST /v1/audio/transcriptions``  multipart: ``file``, ``model``, ``language``,
  ``prompt``, ``response_format`` (``json``, ``verbose_json`` or ``text``),
  ``temperature``.
* ``GET /v1/models``  the served model, as ``whisper-1`` and under its own name.
* ``GET /health``  ``{"ok": true}``, no token needed.
* ``GET /``  a small status page, no token needed (``VCA_STT_LANDING=0`` turns it off).

Security, in order, before any route runs or any body is read (``Guard``):

1. ``/``, ``/health`` and ``/favicon.ico`` (GET/HEAD) pass without a token and
   never count toward the lockout.
2. An IP that is locked out gets 429.
3. Every other path needs ``Authorization: Bearer <token>``; unknown paths
   answer 401 too, so paths cannot be probed. A missing or bad token counts
   toward that IP's lockout.
4. Per-user rate limit (429), then the body cap: ``Content-Length`` over the
   cap gets 413 at once, and a streamed body is cut off when it grows past it.

The handler then allows one transcription at a time with a short bounded
queue (503 when full) and caps the audio duration. The request log holds
user, client IP, sizes, durations and timing; never audio or text.
"""

from __future__ import annotations

import html
import logging
import logging.handlers
import sys
import threading
import time
from collections import defaultdict, deque
from typing import Any

from fastapi import FastAPI, File, Form, HTTPException, Request, UploadFile
from fastapi.responses import JSONResponse, PlainTextResponse, Response

from . import __version__
from .config import Settings
from .tokens import TokenStore
from .transcriber import AudioTooLong, BadAudio, Transcriber

PUBLIC_PATHS = ("/", "/health", "/favicon.ico")
RESPONSE_FORMATS = ("json", "verbose_json", "text")
PAGE_HEADERS = {
    "content-type": "text/html; charset=utf-8",
    "cache-control": "no-store",
    "content-security-policy": "default-src 'none'; style-src 'unsafe-inline'; "
                               "frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
    "x-content-type-options": "nosniff",
    "referrer-policy": "no-referrer",
}


def _request_logger(settings: Settings) -> logging.Logger:
    # A private logger object per app, so tests and reloads never stack handlers.
    logger = logging.Logger("vca-stt-requests", logging.INFO)
    if settings.log_file == "-":
        handler: logging.Handler = logging.StreamHandler(sys.stderr)
    else:
        settings.home.mkdir(parents=True, exist_ok=True)
        handler = logging.handlers.RotatingFileHandler(
            settings.log_file, maxBytes=settings.log_max_bytes,
            backupCount=settings.log_backups, encoding="utf-8")
    handler.setFormatter(logging.Formatter("%(asctime)s %(message)s"))
    logger.addHandler(handler)
    return logger


class _State:
    """Lockout and rate-limit counters, kept in memory."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.hits: dict[str, deque] = defaultdict(deque)   # user -> request times
        self.fails: dict[str, deque] = defaultdict(deque)  # ip -> failed-auth times
        self.locked: dict[str, float] = {}                 # ip -> locked until


async def _reply(send, status: int, body: dict, extra: tuple = ()) -> None:
    import json

    data = json.dumps(body).encode()
    await send({"type": "http.response.start", "status": status,
                "headers": [(b"content-type", b"application/json"),
                            (b"content-length", str(len(data)).encode()), *extra]})
    await send({"type": "http.response.body", "body": data})


class Guard:
    """Pure ASGI middleware: runs before routing and before any body is read."""

    def __init__(self, inner, settings: Settings, tokens: TokenStore, state: _State,
                 log: logging.Logger) -> None:
        self.inner, self.s, self.tokens, self.state, self.log = inner, settings, tokens, state, log

    def client_ip(self, scope) -> str:
        peer = (scope.get("client") or ("?", 0))[0]
        if peer not in self.s.trusted_proxies:
            return peer
        headers = dict(scope.get("headers") or [])
        real = headers.get(b"x-real-ip", b"").decode("latin-1").strip()
        if real:
            return real
        # The right-most entry is the one the trusted proxy added itself.
        fwd = headers.get(b"x-forwarded-for", b"").decode("latin-1").split(",")[-1].strip()
        return fwd or peer

    async def __call__(self, scope, receive, send):
        if scope["type"] != "http":
            return await self.inner(scope, receive, send)
        if scope.get("method") in ("GET", "HEAD") and scope.get("path") in PUBLIC_PATHS:
            return await self.inner(scope, receive, send)

        st, s = self.state, self.s
        ip = self.client_ip(scope)
        now = time.monotonic()
        with st.lock:
            until = st.locked.get(ip, 0.0)
            if until and until <= now:
                st.locked.pop(ip, None)
                until = 0.0
        if until:
            return await _reply(send, 429, {"detail": "too many failed attempts"},
                                ((b"retry-after", str(int(until - now) + 1).encode()),))

        headers = dict(scope.get("headers") or [])
        auth = headers.get(b"authorization", b"").decode("latin-1")
        token = auth[7:].strip() if auth[:7].lower() == "bearer " else ""
        user = self.tokens.user_for(token)
        if user is None:
            with st.lock:
                fails = st.fails[ip]
                while fails and now - fails[0] > s.fail_window:
                    fails.popleft()
                fails.append(now)
                if len(fails) >= s.fail_limit:
                    st.locked[ip] = now + s.lockout_seconds
                    fails.clear()
                    self.log.info("lockout ip=%s", ip)
            return await _reply(send, 401, {"detail": "unauthorized"},
                                ((b"www-authenticate", b"Bearer"),))

        with st.lock:
            hits = st.hits[user]
            while hits and now - hits[0] > 60:
                hits.popleft()
            limited = len(hits) >= s.rate_per_min
            if not limited:
                hits.append(now)
        if limited:
            return await _reply(send, 429, {"detail": "rate limit exceeded"},
                                ((b"retry-after", b"60"),))

        length = headers.get(b"content-length")
        if length is not None and (not length.isdigit() or int(length) > s.max_bytes):
            return await _reply(send, 413, {"detail": "request too large"})

        received = 0

        async def capped_receive():
            # Also enforced for chunked uploads that send no Content-Length.
            nonlocal received
            message = await receive()
            if message["type"] == "http.request":
                received += len(message.get("body", b""))
                if received > s.max_bytes:
                    raise HTTPException(status_code=413, detail="request too large")
            return message

        scope.setdefault("state", {})
        scope["state"]["user"] = user
        scope["state"]["ip"] = ip
        return await self.inner(scope, capped_receive, send)


def _landing(ok: bool) -> str:
    status = "running" if ok else "starting"
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>Speech-to-text server</title>
<style>
body{{font:16px/1.5 system-ui,sans-serif;max-width:40rem;margin:3rem auto;padding:0 1rem;color:#222}}
code{{background:#f2f2f2;padding:0 .3em;border-radius:3px}}
.ok{{color:#1a7f37}}
</style></head><body>
<h1>Speech-to-text server</h1>
<p>Status: <strong class="ok">{html.escape(status)}</strong></p>
<p>This is a private, OpenAI-compatible speech-to-text endpoint for
Voice Command AI. Every API request needs a personal access token from the
administrator of this server.</p>
<p>In VCA, add a speech recognition engine with the base URL
<code>https://&lt;this host&gt;/v1</code> and the model <code>whisper-1</code>,
and paste your token as its API key.</p>
</body></html>
"""


def create_app(settings: Settings | None = None, transcriber: Transcriber | None = None) -> FastAPI:
    """Builds the app. Without a transcriber, loads faster-whisper (slow)."""
    settings = settings or Settings.from_env()
    log = _request_logger(settings)
    if transcriber is None:
        from .transcriber import FasterWhisperTranscriber

        transcriber = FasterWhisperTranscriber(
            model=settings.model, device=settings.device, compute_type=settings.compute_type,
            beam_size=settings.beam_size, vad_filter=settings.vad_filter,
            download_root=settings.download_root)
    tokens = TokenStore(settings.tokens_file)
    state = _State()
    job = threading.Lock()                                         # one job at a time
    slots = threading.BoundedSemaphore(max(settings.max_queue, 0) + 1)  # running + waiting

    app = FastAPI(title="vca-stt-server", version=__version__,
                  docs_url=None, redoc_url=None, openapi_url=None)
    app.add_middleware(Guard, settings=settings, tokens=tokens, state=state, log=log)
    app.state.settings = settings
    app.state.transcriber = transcriber

    @app.exception_handler(404)
    async def not_found(_: Request, __: Any):
        return JSONResponse({"detail": "not found"}, status_code=404)

    @app.exception_handler(Exception)
    async def internal(_: Request, exc: Exception):
        # No stack traces or exception text reach clients (or the log).
        log.info("error %s", type(exc).__name__)
        return JSONResponse({"detail": "internal error"}, status_code=500)

    @app.api_route("/health", methods=["GET", "HEAD"])
    def health():
        return {"ok": True}

    @app.api_route("/", methods=["GET", "HEAD"], include_in_schema=False)
    def landing():
        if not settings.landing_page:
            raise HTTPException(status_code=404)
        return Response(_landing(True), headers=PAGE_HEADERS)

    @app.api_route("/favicon.ico", methods=["GET", "HEAD"], include_in_schema=False)
    def favicon():
        return Response(status_code=204, headers={"cache-control": "no-store"})

    @app.get("/v1/models")
    def models():
        ids = ["whisper-1"] + ([transcriber.name] if transcriber.name != "whisper-1" else [])
        return {"object": "list",
                "data": [{"id": i, "object": "model", "owned_by": "self-hosted"} for i in ids]}

    @app.post("/v1/audio/transcriptions")
    def transcribe(
        request: Request,
        file: UploadFile = File(...),
        model: str = Form("whisper-1"),
        language: str | None = Form(None),
        prompt: str | None = Form(None),
        response_format: str = Form("json"),
        temperature: float = Form(0.0),
    ):
        user, ip = request.scope["state"]["user"], request.scope["state"]["ip"]
        if response_format not in RESPONSE_FORMATS:
            raise HTTPException(status_code=400,
                                detail=f"response_format must be one of {', '.join(RESPONSE_FORMATS)}")
        lang = (language or "").strip().lower().split("-")[0] or None
        if lang is not None and not (2 <= len(lang) <= 3 and lang.isalpha()):
            raise HTTPException(status_code=400, detail="invalid language")
        data = file.file.read(settings.max_bytes + 1)
        file.file.close()  # drop a spooled temporary file right away
        size = len(data)
        if size > settings.max_bytes:
            raise HTTPException(status_code=413, detail="audio too large")
        if size == 0:
            raise HTTPException(status_code=400, detail="empty file")
        if not slots.acquire(blocking=False):
            raise HTTPException(status_code=503, detail="busy, try again shortly",
                                headers={"retry-after": "2"})
        started = time.monotonic()
        try:
            with job:
                result = transcriber.transcribe(
                    data,
                    language=lang,
                    prompt=(prompt[-1000:] if prompt else None),
                    temperature=min(max(temperature, 0.0), 1.0),
                    max_seconds=settings.max_audio_seconds,
                )
        except AudioTooLong:
            raise HTTPException(status_code=413, detail="audio too long") from None
        except BadAudio as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from None
        finally:
            slots.release()
            del data
        log.info("ok user=%s ip=%s bytes=%d audio=%.1fs took=%.2fs lang=%s",
                 user, ip, size, result.duration, time.monotonic() - started,
                 result.language or "-")
        if response_format == "text":
            return PlainTextResponse(result.text)
        # Extra fields are ignored by strict OpenAI clients and help VCA.
        out: dict[str, Any] = {"text": result.text, "language": result.language,
                               "duration": result.duration}
        if response_format == "verbose_json":
            out.update(
                task="transcribe",
                segments=[
                    {"id": i, "start": s.start, "end": s.end, "text": s.text,
                     "avg_logprob": s.avg_logprob, "no_speech_prob": s.no_speech_prob}
                    for i, s in enumerate(result.segments)
                ],
            )
        return out

    return app
