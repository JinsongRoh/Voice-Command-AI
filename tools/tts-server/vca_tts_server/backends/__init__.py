"""TTS engine backends.

A backend turns text into mono 16-bit PCM. Engines are imported only when
their backend is chosen, so the server (and its tests) run without any
engine installed.
"""

from __future__ import annotations

from .base import Backend, BackendError, Synthesis, Voice

BACKENDS = ("melo", "supertonic")


def load_backend(name: str) -> Backend:
    """Creates the backend `name`, configured from environment variables."""
    if name == "melo":
        from .melo import MeloBackend

        return MeloBackend.from_env()
    if name == "supertonic":
        from .supertonic import SupertonicBackend

        return SupertonicBackend.from_env()
    raise BackendError(f"unknown backend {name!r}; choose one of: {', '.join(BACKENDS)}")


__all__ = ["BACKENDS", "Backend", "BackendError", "Synthesis", "Voice", "load_backend"]
