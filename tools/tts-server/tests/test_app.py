"""Tests with a fake backend: no TTS engine, model or network is needed."""

from __future__ import annotations

import io
import logging
import wave
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from vca_tts_server import audio
from vca_tts_server.__main__ import DEFAULT_HOST, DEFAULT_PORT, parse_args
from vca_tts_server.app import create_app
from vca_tts_server.backends import Backend, BackendError, Synthesis, Voice, load_backend
from vca_tts_server.backends.supertonic import (
    SupertonicBackend,
    find_voice_styles,
    pick_language,
    takes_language,
)

SECRET_TEXT = "the quarterly numbers are confidential"


class FakeBackend(Backend):
    name = "fake"

    def __init__(self, fail: Exception | None = None) -> None:
        self.calls: list[tuple[str, str, float]] = []
        self.languages: list[str | None] = []
        self.fail = fail

    def voices(self) -> list[Voice]:
        return [
            Voice(id="F1", name="Fake female", language="en", gender="female"),
            Voice(id="M1", name="Fake male", language="ko", gender="male"),
        ]

    def synthesize(
        self, text: str, voice: str, speed: float, language: str | None = None
    ) -> Synthesis:
        if self.fail:
            raise self.fail
        self.calls.append((text, voice, speed))
        self.languages.append(language)
        # 10 ms of silence at 16 kHz.
        return Synthesis(pcm=b"\x00\x00" * 160, sample_rate=16000)


def client(backend: Backend | None = None, **kwargs) -> tuple[TestClient, Backend]:
    backend = backend or FakeBackend()
    return TestClient(create_app(backend, **kwargs)), backend


def speak(c: TestClient, **body):
    payload = {"model": "tts-1", "input": "Build finished", "voice": "F1", **body}
    return c.post("/v1/audio/speech", json=payload)


def test_lists_voices_and_models():
    c, _ = client()
    voices = c.get("/v1/audio/voices").json()["voices"]
    assert voices == [
        {"id": "F1", "name": "Fake female", "language": "en", "gender": "female"},
        {"id": "M1", "name": "Fake male", "language": "ko", "gender": "male"},
    ]
    assert c.get("/v1/models").json()["data"][0]["id"] == "tts-1"
    assert c.get("/health").json() == {"status": "ok", "backend": "fake"}


def test_synthesizes_wav_and_pcm():
    c, backend = client()
    r = speak(c, response_format="wav", speed=1.25)
    assert r.status_code == 200
    assert r.headers["content-type"] == "audio/wav"
    with wave.open(io.BytesIO(r.content)) as w:
        assert (w.getframerate(), w.getnchannels(), w.getsampwidth()) == (16000, 1, 2)
        assert w.getnframes() == 160
    assert backend.calls == [("Build finished", "F1", 1.25)]
    r = speak(c, response_format="pcm", voice="M1")
    assert r.status_code == 200
    assert r.content == b"\x00\x00" * 160
    assert backend.calls[-1][1] == "M1"


def test_the_language_reaches_the_backend():
    c, backend = client()
    assert speak(c, response_format="pcm", language="ko").status_code == 200
    assert speak(c, response_format="pcm").status_code == 200
    assert backend.languages == ["ko", None]
    assert speak(c, response_format="pcm", language="x" * 40).status_code == 400


def test_supertonic_language_choice():
    assert pick_language("npm run build 실패", None) == "ko"
    assert pick_language("anything", "ko-KR") == "ko"
    assert pick_language("세션", "en_US") == "en"
    assert pick_language("ビルド完了", None) == "ja"
    assert pick_language("Build finished", None) == "en"
    assert pick_language("Build finished", " ", "es") == "es"

    def v1(text, style, total_step, speed=1.05): ...

    def v2(text, lang, style, total_step, speed=1.05): ...

    assert not takes_language(v1)
    assert takes_language(v2)
    assert not takes_language(object())


def test_unknown_voices_use_the_default_unless_strict():
    c, backend = client()
    # VCA sends "alloy" when its profile names no voice.
    assert speak(c, voice="alloy", response_format="wav").status_code == 200
    assert speak(c, voice=None, response_format="wav").status_code == 200
    assert [call[1] for call in backend.calls] == ["F1", "F1"]
    strict, _ = client(strict_voices=True)
    r = speak(strict, voice="alloy", response_format="wav")
    assert r.status_code == 400
    assert "unknown voice" in r.json()["error"]["message"]


def test_rejects_bad_requests_without_echoing_the_text():
    c, backend = client(max_chars=20)
    for body, needle in [
        ({"input": "   "}, "empty"),
        ({"input": "x" * 21}, "limit is 20"),
        ({"response_format": "ogg"}, "response_format"),
        ({"speed": 9}, "speed"),
    ]:
        r = speak(c, **body)
        assert r.status_code == 400, body
        assert needle in r.json()["error"]["message"]
    r = c.post("/v1/audio/speech", json={"voice": "F1", "extra": SECRET_TEXT})
    assert r.status_code == 400
    assert "input" in r.json()["error"]["message"]
    assert SECRET_TEXT not in r.text
    assert backend.calls == []


def test_compressed_formats_need_ffmpeg(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(audio, "ffmpeg_path", lambda: None)
    c, _ = client()
    r = speak(c, response_format="mp3")
    assert r.status_code == 400
    assert "ffmpeg" in r.json()["error"]["message"]


def test_bearer_token(monkeypatch: pytest.MonkeyPatch):
    c, backend = client(token="s3cret-token")
    assert c.get("/v1/audio/voices").status_code == 401
    assert c.get("/v1/audio/voices", headers={"Authorization": "Bearer nope"}).status_code == 401
    assert speak(c, response_format="wav").status_code == 401
    ok = {"Authorization": "Bearer s3cret-token"}
    assert c.get("/v1/audio/voices", headers=ok).status_code == 200
    r = c.post(
        "/v1/audio/speech",
        json={"input": "hi", "voice": "F1", "response_format": "wav"},
        headers=ok,
    )
    assert r.status_code == 200
    # Health needs no token and tells nothing about it.
    assert c.get("/health").status_code == 200
    assert len(backend.calls) == 1


def test_engine_failures_are_reported_without_the_text(caplog: pytest.LogCaptureFixture):
    caplog.set_level(logging.INFO, logger="vca_tts_server")
    c, _ = client(FakeBackend(fail=RuntimeError(f"cannot say {SECRET_TEXT}")))
    r = speak(c, input=SECRET_TEXT, response_format="wav")
    assert r.status_code == 500
    assert SECRET_TEXT not in r.text
    assert SECRET_TEXT not in caplog.text
    c, _ = client(FakeBackend(fail=BackendError("model missing")))
    r = speak(c, response_format="wav")
    assert r.status_code == 500
    assert "model missing" in r.json()["error"]["message"]


def test_success_logs_never_contain_the_text(caplog: pytest.LogCaptureFixture):
    caplog.set_level(logging.INFO, logger="vca_tts_server")
    c, _ = client()
    assert speak(c, input=SECRET_TEXT, response_format="wav").status_code == 200
    assert "synthesized" in caplog.text
    assert SECRET_TEXT not in caplog.text


def test_cli_binds_this_pc_by_default(monkeypatch: pytest.MonkeyPatch):
    for name in ("VCA_TTS_HOST", "VCA_TTS_PORT", "VCA_TTS_BACKEND"):
        monkeypatch.delenv(name, raising=False)
    args = parse_args([])
    assert (args.host, args.port, args.backend) == (DEFAULT_HOST, DEFAULT_PORT, "melo")
    assert DEFAULT_HOST == "127.0.0.1"
    assert parse_args(["--host", "0.0.0.0", "--backend", "supertonic"]).host == "0.0.0.0"


def test_backends_report_missing_engines(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    with pytest.raises(BackendError, match="unknown backend"):
        load_backend("espeak")
    monkeypatch.delenv("SUPERTONIC_DIR", raising=False)
    with pytest.raises(BackendError, match="SUPERTONIC_DIR"):
        load_backend("supertonic")
    with pytest.raises(BackendError, match="helper.py"):
        SupertonicBackend(tmp_path)
    styles = tmp_path / "styles"
    styles.mkdir()
    for name in ("M1", "F2"):
        (styles / f"{name}.json").write_text("{}")
    voices = find_voice_styles(styles)
    assert [(v.id, v.gender) for v in voices] == [("F2", "female"), ("M1", "male")]


def test_audio_helpers():
    pcm = audio.float_to_pcm16([0.0, 1.0, -1.0, 2.0])
    assert pcm == b"\x00\x00\xff\x7f\x01\x80\xff\x7f"
    wav = audio.pcm16_to_wav(pcm, 22050)
    assert wav[:4] == b"RIFF"
    assert audio.encode(pcm, 22050, "pcm") == pcm
    with pytest.raises(audio.AudioFormatError):
        audio.encode(pcm, 22050, "ogg")
