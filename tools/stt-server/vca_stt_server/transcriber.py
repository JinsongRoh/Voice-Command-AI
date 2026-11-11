# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
"""The speech recognizer behind the HTTP API.

``FasterWhisperTranscriber`` wraps faster-whisper. Anything with the same
``name`` and ``transcribe`` method works too, which is how the tests run
without downloading a model.
"""

from __future__ import annotations

import io
import logging
import zlib
from dataclasses import dataclass, field
from typing import Protocol

from .hallucination_phrases import is_hallucination_phrase

log = logging.getLogger("vca-stt")


class BadAudio(ValueError):
    """The upload is not decodable audio, or a parameter is invalid (HTTP 400)."""


class AudioTooLong(ValueError):
    """The audio is longer than the configured cap (HTTP 413)."""


# Hallucination guard. Whisper invents text on silence and noise: repetition
# loops ("O.O.O.O..." or "thank you thank you ...") and stock phrases. A
# segment is dropped when its text compresses too well (a loop), or when the
# model itself thinks there was no speech and is unsure of the words. These
# are the thresholds Whisper uses for its own temperature fallback.
COMPRESSION_RATIO_LIMIT = 2.4
NO_SPEECH_LIMIT = 0.6
LOGPROB_LIMIT = -1.0
# A known stock phrase ("감사합니다.", "Thank you.") as a whole segment is dropped
# on weaker evidence than other text: either score alone is enough.
PHRASE_NO_SPEECH_LIMIT = 0.5
PHRASE_LOGPROB_LIMIT = -0.8


@dataclass
class Segment:
    start: float
    end: float
    text: str
    avg_logprob: float | None = None
    no_speech_prob: float | None = None
    compression_ratio: float | None = None


@dataclass
class Transcription:
    text: str
    language: str | None
    duration: float
    segments: list[Segment] = field(default_factory=list)


def compression_ratio(text: str) -> float:
    """Length of the UTF-8 text divided by its zlib-compressed length."""
    data = text.encode("utf-8")
    if not data:
        return 0.0
    return len(data) / len(zlib.compress(data))


def is_hallucination(segment: Segment) -> bool:
    """Whether a segment is a likely hallucination (see the limits above)."""
    ratio = segment.compression_ratio
    if ratio is None:
        ratio = compression_ratio(segment.text.strip())
    if ratio > COMPRESSION_RATIO_LIMIT:
        return True
    if is_hallucination_phrase(segment.text) and (
            (segment.no_speech_prob is not None
             and segment.no_speech_prob > PHRASE_NO_SPEECH_LIMIT)
            or (segment.avg_logprob is not None
                and segment.avg_logprob < PHRASE_LOGPROB_LIMIT)):
        return True
    return (segment.no_speech_prob is not None and segment.avg_logprob is not None
            and segment.no_speech_prob > NO_SPEECH_LIMIT
            and segment.avg_logprob < LOGPROB_LIMIT)


def keep_segments(segments: list[Segment]) -> list[Segment]:
    """The segments that are not hallucinations, in order."""
    return [s for s in segments if not is_hallucination(s)]


class Transcriber(Protocol):
    name: str

    def transcribe(self, audio: bytes, *, language: str | None, prompt: str | None,
                   temperature: float, max_seconds: float) -> Transcription: ...


def _has_cuda() -> bool:
    try:
        import ctranslate2

        return ctranslate2.get_cuda_device_count() > 0
    except Exception:  # noqa: BLE001 - any failure means "no usable GPU"
        return False


class FasterWhisperTranscriber:
    """faster-whisper (CTranslate2). Loads the model once, at start-up."""

    SAMPLE_RATE = 16000

    def __init__(self, model: str = "", device: str = "auto", compute_type: str = "",
                 beam_size: int = 5, vad_filter: bool = True,
                 download_root: str | None = None) -> None:
        from faster_whisper import WhisperModel

        if device == "auto":
            device = "cuda" if _has_cuda() else "cpu"
        model = model or ("large-v3-turbo" if device == "cuda" else "small")
        compute_type = compute_type or ("float16" if device == "cuda" else "int8")
        log.info("loading model=%s device=%s compute=%s", model, device, compute_type)
        self._model = WhisperModel(model, device=device, compute_type=compute_type,
                                   download_root=download_root)
        self.name = model
        self.device = device
        self.beam_size = beam_size
        self.vad_filter = vad_filter
        self._languages = set(getattr(self._model, "supported_languages", []) or [])

    def transcribe(self, audio: bytes, *, language: str | None, prompt: str | None,
                   temperature: float, max_seconds: float) -> Transcription:
        from faster_whisper import decode_audio

        if language and self._languages and language not in self._languages:
            raise BadAudio(f"unsupported language {language!r}")
        try:
            samples = decode_audio(io.BytesIO(audio), sampling_rate=self.SAMPLE_RATE)
        except Exception as exc:  # noqa: BLE001 - PyAV raises many types
            raise BadAudio("cannot decode the audio") from exc
        duration = len(samples) / self.SAMPLE_RATE
        if duration > max_seconds:
            raise AudioTooLong(f"audio longer than {max_seconds:.0f} s")
        segments, info = self._model.transcribe(
            samples,
            language=language,
            initial_prompt=prompt,
            temperature=temperature,
            beam_size=self.beam_size,
            vad_filter=self.vad_filter,
            # Each window is decoded on its own: a hallucination in one
            # window cannot seed a repetition loop in the next.
            condition_on_previous_text=False,
            compression_ratio_threshold=COMPRESSION_RATIO_LIMIT,
            log_prob_threshold=LOGPROB_LIMIT,
            no_speech_threshold=NO_SPEECH_LIMIT,
        )
        segs = keep_segments([
            Segment(s.start, s.end, s.text, s.avg_logprob, s.no_speech_prob,
                    getattr(s, "compression_ratio", None))
            for s in segments
        ])
        return Transcription(
            text="".join(s.text for s in segs).strip(),
            language=info.language,
            duration=info.duration,
            segments=segs,
        )
