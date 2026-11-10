"""The HTTP API: an OpenAI-compatible subset for text-to-speech.

* ``POST /v1/audio/speech``  ``{model, input, voice, response_format, speed,
  language}`` answers audio bytes (``wav`` and ``pcm`` always; ``mp3``, ``opus``, ``aac``
  and ``flac`` when ffmpeg is installed). ``language`` (BCP-47, e.g. ``ko``)
  is the language of the text; multilingual backends need it, because a
  guess from mixed text ("npm run build 실패") can pick the wrong one.
* ``GET /v1/audio/voices``  the engine's voices.
* ``GET /v1/models``  one model entry, for clients that list models.
* ``GET /health``  no token needed; no engine details beyond its name.

With a token configured, every ``/v1`` request needs
``Authorization: Bearer <token>``. The text and the audio are never logged.
"""

from __future__ import annotations

import hmac
import logging
import threading
import time
from typing import Any

from fastapi import FastAPI, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel, Field

from .audio import CONTENT_TYPES, AudioFormatError, encode
from .backends import Backend, BackendError

log = logging.getLogger("vca_tts_server")

DEFAULT_MAX_CHARS = 1000
MODEL_ID = "tts-1"


class SpeechRequest(BaseModel):
    model: str | None = None
    input: str
    voice: str | None = None
    response_format: str = "mp3"
    speed: float = Field(default=1.0)
    # OpenAI's newer field; accepted and ignored.
    instructions: str | None = None
    # The language of the text (VCA sends the base tag, e.g. "ko").
    language: str | None = Field(default=None, max_length=35)


def error(status: int, message: str, kind: str = "invalid_request_error") -> JSONResponse:
    """An OpenAI-style error body."""
    return JSONResponse(status_code=status, content={"error": {"message": message, "type": kind}})


def create_app(
    backend: Backend,
    token: str | None = None,
    max_chars: int = DEFAULT_MAX_CHARS,
    strict_voices: bool = False,
) -> FastAPI:
    """The app around `backend`. `strict_voices`: refuse unknown voices
    instead of using the default one (VCA sends "alloy" when a profile names
    no voice)."""
    app = FastAPI(title="VCA reference TTS server", docs_url=None, redoc_url=None)
    lock = threading.Lock()
    token_bytes = token.encode() if token else None

    def authorized(request: Request) -> bool:
        if token_bytes is None:
            return True
        header = request.headers.get("authorization", "")
        scheme, _, value = header.partition(" ")
        return scheme.lower() == "bearer" and hmac.compare_digest(
            value.strip().encode(), token_bytes
        )

    @app.middleware("http")
    async def require_token(request: Request, call_next: Any) -> Any:
        if request.url.path.startswith("/v1") and not authorized(request):
            return error(401, "missing or wrong bearer token", "authentication_error")
        return await call_next(request)

    @app.exception_handler(RequestValidationError)
    async def bad_request(_request: Request, exc: RequestValidationError) -> JSONResponse:
        # Only field names: the request text is never echoed.
        fields = sorted({".".join(str(p) for p in e.get("loc", ())[1:]) for e in exc.errors()})
        return error(400, f"invalid request body: {', '.join(fields) or 'body'}")

    @app.get("/health")
    def health() -> dict[str, str]:
        return {"status": "ok", "backend": backend.name}

    @app.get("/v1/models")
    def models() -> dict[str, Any]:
        return {"object": "list", "data": [{"id": MODEL_ID, "object": "model", "owned_by": "local"}]}

    @app.get("/v1/audio/voices")
    def voices() -> dict[str, Any]:
        return {
            "voices": [
                {"id": v.id, "name": v.name, "language": v.language, "gender": v.gender}
                for v in backend.voices()
            ]
        }

    @app.post("/v1/audio/speech")
    def speech(req: SpeechRequest) -> Response:
        text = " ".join(req.input.split())
        if not text:
            return error(400, "input is empty")
        if len(text) > max_chars:
            return error(400, f"input is {len(text)} characters; the limit is {max_chars}")
        fmt = req.response_format.lower()
        if fmt not in CONTENT_TYPES:
            return error(400, f"response_format must be one of {', '.join(CONTENT_TYPES)}")
        if not 0.25 <= req.speed <= 4.0:
            return error(400, "speed must be between 0.25 and 4.0")
        known = {v.id for v in backend.voices()}
        voice = (req.voice or "").strip()
        if voice not in known:
            if strict_voices and voice:
                return error(400, f"unknown voice {voice!r}; see GET /v1/audio/voices")
            voice = backend.default_voice()
        started = time.monotonic()
        try:
            with lock:
                out = backend.synthesize(text, voice, req.speed, language=req.language)
            audio = encode(out.pcm, out.sample_rate, fmt)
        except AudioFormatError as e:
            return error(400, str(e))
        except BackendError as e:
            log.warning("synthesis failed: %s", e)
            return error(500, f"the TTS engine failed: {e}", "server_error")
        except Exception as e:  # noqa: BLE001 - an engine bug must not crash the server
            # The exception type only (no message, no traceback): engine
            # messages may quote the input.
            log.error("synthesis failed (%s)", type(e).__name__)
            return error(500, "the TTS engine failed", "server_error")
        log.info(
            "synthesized %d chars with voice %s as %s in %.0f ms",
            len(text),
            voice,
            fmt,
            (time.monotonic() - started) * 1000,
        )
        return Response(content=audio, media_type=CONTENT_TYPES[fmt])

    return app
