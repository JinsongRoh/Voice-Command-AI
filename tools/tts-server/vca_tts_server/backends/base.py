"""The interface every backend implements."""

from __future__ import annotations

import abc
from dataclasses import dataclass, field


class BackendError(Exception):
    """The engine is missing, misconfigured or failed."""


@dataclass(frozen=True)
class Voice:
    """A voice the engine offers. `id` is what clients send as `voice`."""

    id: str
    name: str
    language: str | None = None
    # "female", "male" or "other" (VCA's speaker genders).
    gender: str = "other"


@dataclass(frozen=True)
class Synthesis:
    """Mono 16-bit little-endian PCM."""

    pcm: bytes
    sample_rate: int
    extra: dict = field(default_factory=dict)


class Backend(abc.ABC):
    """A TTS engine. Calls are serialized by the server, so implementations
    need not be thread-safe."""

    #: Short engine name, shown by /health.
    name: str = "backend"

    @abc.abstractmethod
    def voices(self) -> list[Voice]:
        """The voices, the default first."""

    @abc.abstractmethod
    def synthesize(
        self, text: str, voice: str, speed: float, language: str | None = None
    ) -> Synthesis:
        """Speaks `text` with the voice id `voice` at `speed` (1.0 normal).
        `language` is the BCP-47 language of the text when the client knows
        it; backends with one language per voice may ignore it."""

    def default_voice(self) -> str:
        voices = self.voices()
        if not voices:
            raise BackendError("the engine offers no voices")
        return voices[0].id
