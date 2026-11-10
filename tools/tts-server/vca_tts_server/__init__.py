"""A small OpenAI-compatible text-to-speech server for Voice Command AI.

It exposes ``POST /v1/audio/speech`` and ``GET /v1/audio/voices`` in front of
a TTS engine that the user installs (MeloTTS or Supertonic). This package
ships no model weights.
"""

__version__ = "0.1.0"
