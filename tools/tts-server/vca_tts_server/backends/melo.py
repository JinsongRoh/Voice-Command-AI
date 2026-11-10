"""MeloTTS backend (https://github.com/myshell-ai/MeloTTS).

Install MeloTTS yourself (see the README); its weights are downloaded by
MeloTTS on first use, not by this project.

Environment:
  MELO_LANGUAGES  Comma-separated MeloTTS languages to load: EN, ES, FR, ZH,
                  JP, KR (default: EN). Each loads one model.
  MELO_DEVICE     "auto" (default), "cpu", "cuda", "mps".

Voice ids are MeloTTS speaker names, e.g. EN-US, EN-BR, EN-AU, KR, JP.
"""

from __future__ import annotations

import os
from typing import Any

from ..audio import float_to_pcm16
from .base import Backend, BackendError, Synthesis, Voice

LANGUAGE_TAGS = {"EN": "en", "ES": "es", "FR": "fr", "ZH": "zh", "JP": "ja", "KR": "ko"}


def _speakers(spk2id: Any) -> dict[str, int]:
    """MeloTTS keeps speakers in an HParams object or a dict."""
    if hasattr(spk2id, "items"):
        return {str(k): int(v) for k, v in spk2id.items()}
    return {str(k): int(v) for k, v in vars(spk2id).items()}


class MeloBackend(Backend):
    name = "melo"

    def __init__(self, languages: list[str], device: str = "auto") -> None:
        try:
            from melo.api import TTS  # type: ignore[import-not-found]
        except ImportError as e:  # pragma: no cover - needs MeloTTS
            raise BackendError(
                "MeloTTS is not installed; see tools/tts-server/README.md"
            ) from e
        self._models: dict[str, Any] = {}
        # voice id -> (language, speaker index)
        self._voices: dict[str, tuple[str, int]] = {}
        self._order: list[Voice] = []
        for lang in languages:
            lang = lang.strip().upper()
            if not lang:
                continue
            if lang not in LANGUAGE_TAGS:
                raise BackendError(
                    f"unknown MeloTTS language {lang!r}; use {', '.join(LANGUAGE_TAGS)}"
                )
            model = TTS(language=lang, device=device)  # pragma: no cover
            self._models[lang] = model
            for speaker, index in _speakers(model.hps.data.spk2id).items():
                self._voices[speaker] = (lang, index)
                self._order.append(
                    Voice(id=speaker, name=f"MeloTTS {speaker}", language=LANGUAGE_TAGS[lang])
                )
        if not self._order:
            raise BackendError("no MeloTTS language loaded (set MELO_LANGUAGES)")

    @classmethod
    def from_env(cls) -> "MeloBackend":
        languages = os.environ.get("MELO_LANGUAGES", "EN").split(",")
        return cls(languages, os.environ.get("MELO_DEVICE", "auto"))

    def voices(self) -> list[Voice]:
        return list(self._order)

    def synthesize(  # pragma: no cover
        self, text: str, voice: str, speed: float, language: str | None = None
    ) -> Synthesis:
        # Each MeloTTS voice belongs to one language model.
        lang, index = self._voices[voice]
        model = self._models[lang]
        audio = model.tts_to_file(text, index, None, speed=speed)
        return Synthesis(
            pcm=float_to_pcm16(audio),
            sample_rate=int(model.hps.data.sampling_rate),
        )
