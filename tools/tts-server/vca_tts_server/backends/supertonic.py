"""Supertonic backend (https://github.com/supertone-inc/supertonic).

Clone the Supertonic repository and download its ONNX models and voice
styles yourself, following its README; this project ships none of them.
The backend uses the Python helper of that repository (``py/helper.py``:
``load_text_to_speech`` and ``load_voice_style``).

Environment:
  SUPERTONIC_DIR    The cloned repository (required). Expected inside it:
                    py/helper.py, assets/onnx/ and assets/voice_styles/*.json.
  SUPERTONIC_ONNX_DIR, SUPERTONIC_VOICE_DIR
                    Override those two folders.
  SUPERTONIC_STEPS  Denoising steps (default 5; more is slower and cleaner).
  SUPERTONIC_GPU    "1" to use the GPU when the ONNX runtime supports it.
  SUPERTONIC_LANG   Language when a request names none and the text gives
                    no clue (default "en"). Multilingual Supertonic releases
                    take a language per call; the request's ``language`` is
                    used, else Hangul text is "ko" and kana text "ja".

Voice ids are the voice style file names without ".json" (e.g. F1, M1).
"""

from __future__ import annotations

import inspect
import os
import re
import sys
from pathlib import Path
from typing import Any

from ..audio import float_to_pcm16
from .base import Backend, BackendError, Synthesis, Voice


def _gender(voice_id: str) -> str:
    first = voice_id[:1].upper()
    return {"F": "female", "M": "male"}.get(first, "other")


def find_voice_styles(voice_dir: Path) -> list[Voice]:
    """Voices from the style files of a folder, sorted by name."""
    if not voice_dir.is_dir():
        return []
    return [
        Voice(id=p.stem, name=f"Supertonic {p.stem}", gender=_gender(p.stem))
        for p in sorted(voice_dir.glob("*.json"))
    ]


_HANGUL = re.compile(r"[\uac00-\ud7a3\u1100-\u11ff\u3130-\u318f]")
_KANA = re.compile(r"[\u3040-\u30ff]")


def pick_language(text: str, language: str | None, default: str = "en") -> str:
    """The base language to synthesize `text` in: the request's language
    ("ko-KR" -> "ko"), else Korean for any Hangul, Japanese for any kana,
    else `default`. Mixed text such as "npm run build 실패" is Korean: read
    in another language, its Hangul comes out as foreign-sounding noise."""
    base = (language or "").strip().replace("_", "-").split("-")[0].lower()
    if base.isalpha() and 2 <= len(base) <= 3:
        return base
    if _HANGUL.search(text):
        return "ko"
    if _KANA.search(text):
        return "ja"
    return default


def takes_language(tts: object) -> bool:
    """True for Supertonic helpers whose call takes a language (2 and later)."""
    try:
        return "lang" in inspect.signature(tts).parameters
    except (TypeError, ValueError):
        return False


class SupertonicBackend(Backend):
    name = "supertonic"

    def __init__(
        self,
        repo_dir: Path,
        onnx_dir: Path | None = None,
        voice_dir: Path | None = None,
        steps: int = 5,
        use_gpu: bool = False,
        default_language: str = "en",
    ) -> None:
        py_dir = repo_dir / "py"
        self._onnx_dir = onnx_dir or repo_dir / "assets" / "onnx"
        self._voice_dir = voice_dir or repo_dir / "assets" / "voice_styles"
        self._steps = max(1, steps)
        self._default_language = default_language
        if not (py_dir / "helper.py").is_file():
            raise BackendError(
                f"{py_dir / 'helper.py'} not found: set SUPERTONIC_DIR to the cloned "
                "Supertonic repository"
            )
        self._voices = find_voice_styles(self._voice_dir)
        if not self._voices:
            raise BackendError(
                f"no voice styles (*.json) in {self._voice_dir}; download the Supertonic assets"
            )
        if str(py_dir) not in sys.path:
            sys.path.insert(0, str(py_dir))
        try:  # pragma: no cover - needs Supertonic
            import helper  # type: ignore[import-not-found]
        except ImportError as e:  # pragma: no cover
            raise BackendError(
                f"cannot load Supertonic's helper ({e}); install its Python requirements"
            ) from e
        self._helper: Any = helper  # pragma: no cover
        self._tts: Any = helper.load_text_to_speech(str(self._onnx_dir), use_gpu)  # pragma: no cover
        self._multilingual = takes_language(self._tts)  # pragma: no cover
        self._styles: dict[str, Any] = {}

    @classmethod
    def from_env(cls) -> "SupertonicBackend":
        repo = os.environ.get("SUPERTONIC_DIR")
        if not repo:
            raise BackendError("set SUPERTONIC_DIR to the cloned Supertonic repository")
        onnx = os.environ.get("SUPERTONIC_ONNX_DIR")
        voices = os.environ.get("SUPERTONIC_VOICE_DIR")
        return cls(
            Path(repo),
            Path(onnx) if onnx else None,
            Path(voices) if voices else None,
            int(os.environ.get("SUPERTONIC_STEPS", "5")),
            os.environ.get("SUPERTONIC_GPU") == "1",
            os.environ.get("SUPERTONIC_LANG", "en"),
        )

    def voices(self) -> list[Voice]:
        return list(self._voices)

    def _style(self, voice: str) -> Any:  # pragma: no cover - needs Supertonic
        if voice not in self._styles:
            path = self._voice_dir / f"{voice}.json"
            self._styles[voice] = self._helper.load_voice_style([str(path)])
        return self._styles[voice]

    def synthesize(  # pragma: no cover
        self, text: str, voice: str, speed: float, language: str | None = None
    ) -> Synthesis:
        style = self._style(voice)
        if self._multilingual:
            lang = pick_language(text, language, self._default_language)
            wav, duration = self._tts(text, lang, style, self._steps, speed)
        else:
            wav, duration = self._tts(text, style, self._steps, speed)
        rate = int(self._tts.sample_rate)
        # The helper returns a batch: the first item, cut to its duration.
        samples = wav[0]
        try:
            samples = samples[: int(rate * float(duration[0]))]
        except (TypeError, IndexError):
            pass
        return Synthesis(pcm=float_to_pcm16(samples), sample_rate=rate)
