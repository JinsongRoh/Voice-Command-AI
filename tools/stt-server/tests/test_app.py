# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
"""Tests with a fake transcriber: no model, GPU or network is needed."""

from __future__ import annotations

import json
import threading
import time
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from vca_stt_server import tokens as tokens_cli
from vca_stt_server.app import create_app
from vca_stt_server.config import Settings
from vca_stt_server.transcriber import AudioTooLong, BadAudio, Segment, Transcription

SECRET_TEXT = "the quick brown fox"


class FakeTranscriber:
    name = "fake-whisper"

    def __init__(self) -> None:
        self.calls: list[dict] = []
        self.gate: threading.Event | None = None
        self.entered = threading.Event()

    def transcribe(self, audio, *, language, prompt, temperature, max_seconds):
        self.calls.append({"bytes": len(audio), "language": language, "prompt": prompt})
        self.entered.set()
        if self.gate is not None:
            self.gate.wait(5)
        if audio.startswith(b"BAD"):
            raise BadAudio("cannot decode the audio")
        duration = 2.0 if not audio.startswith(b"LONG") else 9999.0
        if duration > max_seconds:
            raise AudioTooLong("too long")
        seg = Segment(0.0, duration, " " + SECRET_TEXT, -0.1, 0.01)
        return Transcription(SECRET_TEXT, language or "en", duration, [seg])


def make(tmp_path: Path, **overrides):
    settings = Settings(home=tmp_path, **overrides)
    fake = FakeTranscriber()
    client = TestClient(create_app(settings, fake))
    return settings, fake, client


def add_user(settings: Settings, user: str, days: float | None = None, capsys=None) -> str:
    argv = ["add", user] + (["--days", str(days)] if days is not None else [])
    import contextlib
    import io

    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        assert tokens_cli.main(argv, path=settings.tokens_file) == 0
    return out.getvalue().strip()


def upload(client, token: str | None, data: bytes = b"RIFFfakeaudio", **form):
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    return client.post("/v1/audio/transcriptions", headers=headers,
                       files={"file": ("a.wav", data, "audio/wav")},
                       data={"model": "whisper-1", **form})


def test_public_paths_need_no_token(tmp_path):
    _, _, c = make(tmp_path)
    assert c.get("/health").json() == {"ok": True}
    r = c.get("/")
    assert r.status_code == 200 and "Speech-to-text server" in r.text
    assert "content-security-policy" in r.headers
    assert c.get("/favicon.ico").status_code == 204


def test_landing_can_be_turned_off(tmp_path):
    _, _, c = make(tmp_path, landing_page=False)
    assert c.get("/").status_code == 404


def test_every_other_path_needs_a_token(tmp_path):
    s, _, c = make(tmp_path)
    for path in ("/v1/models", "/docs", "/openapi.json", "/does-not-exist"):
        r = c.get(path)
        assert r.status_code == 401, path
        assert r.headers["www-authenticate"] == "Bearer"
    tok = add_user(s, "alice")
    assert c.get("/does-not-exist", headers={"Authorization": f"Bearer {tok}"}).status_code == 404
    assert upload(c, "stt_wrong").status_code == 401


def test_transcription_formats(tmp_path):
    s, fake, c = make(tmp_path)
    tok = add_user(s, "alice")
    assert tok.startswith("stt_") and len(tok) > 40

    r = upload(c, tok, language="ko-KR", prompt="x" * 5000)
    assert r.status_code == 200
    assert r.json()["text"] == SECRET_TEXT
    assert fake.calls[-1]["language"] == "ko"
    assert len(fake.calls[-1]["prompt"]) == 1000

    r = upload(c, tok, response_format="verbose_json")
    body = r.json()
    assert body["segments"][0]["avg_logprob"] == -0.1
    assert body["language"] == "en" and body["duration"] == 2.0

    r = upload(c, tok, response_format="text")
    assert r.text == SECRET_TEXT

    assert upload(c, tok, response_format="srt").status_code == 400
    assert upload(c, tok, language="not a language").status_code == 400


def test_models_lists_alias_and_name(tmp_path):
    s, _, c = make(tmp_path)
    tok = add_user(s, "alice")
    ids = [m["id"] for m in c.get("/v1/models", headers={"Authorization": f"Bearer {tok}"}).json()["data"]]
    assert ids == ["whisper-1", "fake-whisper"]


def test_bad_and_long_audio(tmp_path):
    s, _, c = make(tmp_path)
    tok = add_user(s, "alice")
    assert upload(c, tok, data=b"BADDATA").status_code == 400
    assert upload(c, tok, data=b"LONGDATA").status_code == 413
    assert upload(c, tok, data=b"").status_code == 400


def test_body_cap_by_length_and_by_stream(tmp_path):
    s, fake, c = make(tmp_path, max_bytes=1000)
    tok = add_user(s, "alice")
    assert upload(c, tok, data=b"x" * 5000).status_code == 413

    def chunks():
        for _ in range(10):
            yield b"y" * 500

    r = c.post("/v1/audio/transcriptions", content=chunks(),
               headers={"Authorization": f"Bearer {tok}",
                        "content-type": "multipart/form-data; boundary=zzz"})
    assert r.status_code == 413
    assert fake.calls == []


def test_lockout_ignores_public_paths(tmp_path):
    s, _, c = make(tmp_path, fail_limit=3)
    tok = add_user(s, "alice")
    for _ in range(10):
        c.get("/")
        c.get("/health")
        c.get("/favicon.ico")
    assert upload(c, tok).status_code == 200  # public hits never lock anyone out
    for _ in range(3):
        assert c.get("/v1/models").status_code == 401
    r = upload(c, tok)  # even a good token is refused while locked
    assert r.status_code == 429 and "retry-after" in r.headers
    assert c.get("/health").status_code == 200


def test_trusted_proxy_header_only_from_loopback(tmp_path):
    s, _, c = make(tmp_path, fail_limit=2, trusted_proxies=frozenset({"testclient"}))
    for _ in range(2):
        c.get("/v1/models", headers={"X-Real-IP": "203.0.113.7"})
    # 203.0.113.7 is locked, another client behind the same proxy is not.
    assert c.get("/v1/models", headers={"X-Real-IP": "203.0.113.7"}).status_code == 429
    assert c.get("/v1/models", headers={"X-Real-IP": "203.0.113.9"}).status_code == 401
    assert c.get("/v1/models", headers={"X-Forwarded-For": "1.2.3.4, 203.0.113.10"}).status_code == 401

    s2, _, c2 = make(tmp_path / "b", fail_limit=2)  # "testclient" is not trusted here
    for _ in range(2):
        c2.get("/v1/models", headers={"X-Real-IP": f"203.0.113.{_}"})
    assert c2.get("/v1/models", headers={"X-Real-IP": "203.0.113.9"}).status_code == 429


def test_rate_limit_per_user(tmp_path):
    s, _, c = make(tmp_path, rate_per_min=2)
    a, b = add_user(s, "alice"), add_user(s, "bob")
    assert upload(c, a).status_code == 200
    assert upload(c, a).status_code == 200
    assert upload(c, a).status_code == 429
    assert upload(c, b).status_code == 200


def test_queue_is_bounded(tmp_path):
    s, fake, c = make(tmp_path, max_queue=0)
    tok = add_user(s, "alice")
    fake.gate = threading.Event()
    results = []
    t = threading.Thread(target=lambda: results.append(upload(c, tok).status_code))
    t.start()
    assert fake.entered.wait(5)
    assert upload(c, tok).status_code == 503  # one running, no waiting slot
    fake.gate.set()
    t.join(5)
    assert results == [200]


def test_tokens_expire_rotate_and_revoke(tmp_path):
    s, _, c = make(tmp_path)
    old = add_user(s, "alice")
    new = add_user(s, "alice")  # rotate
    time.sleep(0.01)
    assert upload(c, old).status_code == 401
    assert upload(c, new).status_code == 200

    exp = add_user(s, "carol", days=1)
    users = json.loads(Path(s.tokens_file).read_text())
    users["carol"]["expires"] = int(time.time()) - 1
    Path(s.tokens_file).write_text(json.dumps(users))
    assert upload(c, exp).status_code == 401

    assert tokens_cli.main(["remove", "alice"], path=s.tokens_file) == 0
    assert upload(c, new).status_code == 401
    assert tokens_cli.main(["remove", "nobody"], path=s.tokens_file) == 1
    assert tokens_cli.main(["add", "bad name"], path=s.tokens_file) == 2


def test_token_file_holds_hashes_only(tmp_path):
    s, _, _ = make(tmp_path)
    tok = add_user(s, "alice")
    raw = Path(s.tokens_file).read_text()
    assert tok not in raw and tokens_cli.hash_token(tok) in raw


def test_log_has_no_content(tmp_path):
    s, _, c = make(tmp_path)
    tok = add_user(s, "alice")
    assert upload(c, tok, prompt="secret prompt words").status_code == 200
    log = Path(s.log_file).read_text()
    assert "user=alice" in log and "audio=2.0s" in log
    assert SECRET_TEXT not in log and "secret prompt" not in log and tok not in log


def test_settings_from_env_defaults_to_loopback(tmp_path):
    s = Settings.from_env({"VCA_STT_HOME": str(tmp_path)})
    assert s.host == "127.0.0.1"
    assert s.tokens_file == tmp_path / "tokens.json"
    s = Settings.from_env({"VCA_STT_TRUSTED_PROXIES": "10.0.0.1, ::1", "VCA_STT_VAD": "0"})
    assert s.trusted_proxies == {"10.0.0.1", "::1"} and s.vad_filter is False
    with pytest.raises(SystemExit):
        Settings.from_env({"VCA_STT_PORT": "abc"})
