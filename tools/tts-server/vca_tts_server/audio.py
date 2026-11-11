"""Audio helpers: WAV encoding and optional conversion with ffmpeg."""

from __future__ import annotations

import io
import shutil
import subprocess
import wave
from typing import Sequence

# OpenAI `response_format` values and their content types.
CONTENT_TYPES = {
    "mp3": "audio/mpeg",
    "opus": "audio/ogg",
    "aac": "audio/aac",
    "flac": "audio/flac",
    "wav": "audio/wav",
    "pcm": "audio/pcm",
}

# Formats produced without extra tools.
NATIVE_FORMATS = ("wav", "pcm")

_FFMPEG_ARGS = {
    "mp3": ["-f", "mp3", "-codec:a", "libmp3lame", "-q:a", "4"],
    "opus": ["-f", "ogg", "-codec:a", "libopus", "-b:a", "48k"],
    "aac": ["-f", "adts", "-codec:a", "aac", "-b:a", "96k"],
    "flac": ["-f", "flac", "-codec:a", "flac"],
}


class AudioFormatError(Exception):
    """The requested format cannot be produced on this machine."""


def float_to_pcm16(samples: Sequence[float]) -> bytes:
    """Converts samples in [-1, 1] to little-endian 16-bit PCM."""
    try:
        import numpy as np  # engines bring numpy; the fallback below does not need it

        arr = np.clip(np.asarray(samples, dtype=np.float32).reshape(-1), -1.0, 1.0)
        return (np.round(arr * 32767.0)).astype("<i2").tobytes()
    except ImportError:
        pass
    out = bytearray()
    for s in samples:
        v = max(-1.0, min(1.0, float(s)))
        out += int(round(v * 32767)).to_bytes(2, "little", signed=True)
    return bytes(out)


def pcm16_to_wav(pcm: bytes, sample_rate: int, channels: int = 1) -> bytes:
    """Wraps 16-bit PCM in a WAV container."""
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        w.writeframes(pcm)
    return buf.getvalue()


def ffmpeg_path() -> str | None:
    """The ffmpeg program, if installed."""
    return shutil.which("ffmpeg")


def encode(pcm: bytes, sample_rate: int, fmt: str, timeout: float = 30.0) -> bytes:
    """Encodes mono 16-bit PCM as `fmt` (one of CONTENT_TYPES)."""
    if fmt == "pcm":
        return pcm
    wav = pcm16_to_wav(pcm, sample_rate)
    if fmt == "wav":
        return wav
    if fmt not in _FFMPEG_ARGS:
        raise AudioFormatError(f"unknown audio format {fmt!r}")
    ffmpeg = ffmpeg_path()
    if ffmpeg is None:
        raise AudioFormatError(
            f"{fmt} needs ffmpeg on this PC; install ffmpeg, or use the wav format "
            '(format = "wav" in VCA)'
        )
    proc = subprocess.run(
        [ffmpeg, "-hide_banner", "-loglevel", "error", "-f", "wav", "-i", "pipe:0",
         *_FFMPEG_ARGS[fmt], "pipe:1"],
        input=wav,
        capture_output=True,
        timeout=timeout,
        check=False,
    )
    if proc.returncode != 0 or not proc.stdout:
        raise AudioFormatError(f"ffmpeg could not encode {fmt}")
    return proc.stdout
