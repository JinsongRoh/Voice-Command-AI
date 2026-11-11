# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
"""The hallucination guard: segment filtering and the decoder options."""

from __future__ import annotations

import sys
import types
from dataclasses import dataclass

import pytest

from vca_stt_server import transcriber as tr
from vca_stt_server.transcriber import (
    COMPRESSION_RATIO_LIMIT,
    Segment,
    compression_ratio,
    is_hallucination,
    keep_segments,
)

LOOP = "O." * 120


def test_compression_ratio_flags_repetition_loops():
    assert compression_ratio("") == 0.0
    assert compression_ratio("로그인 버그를 고쳐 줘") < COMPRESSION_RATIO_LIMIT
    assert compression_ratio("Run the tests and fix the warning.") < COMPRESSION_RATIO_LIMIT
    assert compression_ratio(LOOP) > COMPRESSION_RATIO_LIMIT
    assert compression_ratio("thank you " * 20) > COMPRESSION_RATIO_LIMIT


@pytest.mark.parametrize(
    ("segment", "dropped"),
    [
        (Segment(0, 1, " 로그인 버그를 고쳐 줘", -0.2, 0.01), False),
        # A repetition loop, measured from the text when the engine gives no ratio.
        (Segment(0, 30, " " + LOOP, -0.3, 0.1), True),
        # The engine's own ratio wins.
        (Segment(0, 1, " fine", -0.3, 0.1, 2.5), True),
        (Segment(0, 1, " fine", -0.3, 0.1, 2.4), False),
        # No speech and an unsure decode: silence turned into words.
        (Segment(0, 1, " 감사합니다.", -1.2, 0.8), True),
        # A known stock phrase needs only one weak score.
        (Segment(0, 1, " 감사합니다.", -0.5, 0.8), True),
        (Segment(0, 1, " 감사합니다.", -0.9, 0.3), True),
        (Segment(0, 1, " Thank you.", -0.3, 0.7), True),
        (Segment(0, 3, " 시청해 주셔서 감사합니다", -0.3, 0.55), True),
        # ...and stays when the model is confident the user spoke.
        (Segment(0, 1, " 감사합니다.", -0.5, 0.3), False),
        (Segment(0, 1, " Thank you.", -0.2, 0.05), False),
        (Segment(0, 1, " 감사합니다.", None, None), False),
        # Other text needs both conditions.
        (Segment(0, 1, " 로그인 고쳐 줘", -1.2, 0.3), False),
        (Segment(0, 1, " 로그인 고쳐 줘", -0.5, 0.8), False),
        # A phrase inside a longer segment is never a stock phrase.
        (Segment(0, 3, " 도와주셔서 감사합니다 다음 작업 진행해", -0.9, 0.7), False),
        # Missing scores never drop a segment on their own.
        (Segment(0, 1, " hello", None, None), False),
    ],
)
def test_is_hallucination(segment, dropped):
    assert is_hallucination(segment) is dropped


def test_keep_segments_keeps_order():
    good1 = Segment(0, 1, " 테스트", -0.2, 0.01)
    bad = Segment(1, 2, " " + LOOP, -0.2, 0.01)
    good2 = Segment(2, 3, " 실행해 줘", -0.2, 0.01)
    assert keep_segments([good1, bad, good2]) == [good1, good2]
    assert keep_segments([]) == []


@dataclass
class _FwSegment:
    start: float
    end: float
    text: str
    avg_logprob: float
    no_speech_prob: float
    compression_ratio: float


class _FwModel:
    """Stands in for faster_whisper.WhisperModel and records its options."""

    last_kwargs: dict = {}

    def __init__(self, *args, **kwargs) -> None:
        self.supported_languages = ["en", "ko"]

    def transcribe(self, samples, **kwargs):
        type(self).last_kwargs = kwargs
        segs = [
            _FwSegment(0.0, 1.0, " 테스트 실행해 줘", -0.2, 0.02, 1.1),
            _FwSegment(1.0, 9.0, " " + LOOP, -0.4, 0.3, 9.5),
            _FwSegment(9.0, 10.0, " 감사합니다.", -1.5, 0.9, 1.0),
        ]
        info = types.SimpleNamespace(language="ko", duration=10.0)
        return iter(segs), info


@pytest.fixture
def fake_faster_whisper(monkeypatch):
    module = types.ModuleType("faster_whisper")
    module.WhisperModel = _FwModel
    module.decode_audio = lambda _file, sampling_rate: [0.0] * sampling_rate * 10
    monkeypatch.setitem(sys.modules, "faster_whisper", module)
    monkeypatch.setattr(tr, "_has_cuda", lambda: False)
    return module


def test_faster_whisper_runs_with_the_guard(fake_faster_whisper):
    t = tr.FasterWhisperTranscriber(model="small", device="cpu")
    out = t.transcribe(b"audio", language="ko", prompt="보코, 오버", temperature=0.0,
                       max_seconds=600)
    kwargs = _FwModel.last_kwargs
    assert kwargs["condition_on_previous_text"] is False
    assert kwargs["compression_ratio_threshold"] == 2.4
    assert kwargs["no_speech_threshold"] == 0.6
    assert kwargs["log_prob_threshold"] == -1.0
    assert kwargs["initial_prompt"] == "보코, 오버"
    # The loop and the silent "thank you" are gone; the speech is kept.
    assert out.text == "테스트 실행해 줘"
    assert [s.text for s in out.segments] == [" 테스트 실행해 줘"]
    assert out.segments[0].compression_ratio == 1.1


def test_phrase_list_matches_the_shared_json():
    import json
    from pathlib import Path

    from vca_stt_server.hallucination_phrases import PHRASES, is_hallucination_phrase

    shared = Path(__file__).resolve().parents[3] / "apps/ui/src/speech/hallucinationPhrases.json"
    if not shared.exists():  # the server was copied out of the VCA repository
        pytest.skip("shared phrase list not available")
    data = json.loads(shared.read_text(encoding="utf-8"))
    assert PHRASES == data["phrases"]
    for case in data["cases"]:
        assert is_hallucination_phrase(case["text"]) is case["phrase"], case["text"]


def test_keep_segments_drops_trailing_phrase_after_silence():
    speech = Segment(0, 3, " 로그인 버그 고쳐 줘", -0.2, 0.02)
    ghost = Segment(9, 10, " 시청해 주셔서 감사합니다.", -0.6, 0.8)
    assert keep_segments([speech, ghost]) == [speech]
