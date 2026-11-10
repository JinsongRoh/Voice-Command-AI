# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
"""Known Whisper hallucination phrases.

Stock phrases Whisper invents on silence or noise (YouTube-trained data). The
list mirrors ``apps/ui/src/speech/hallucinationPhrases.json`` in the VCA
repository, which the app's Rust and TypeScript code share; a test keeps the
copies equal. Matching ignores case, punctuation and spacing. A ``*`` stands
for a short free part (a name). A phrase counts only as a whole segment.
"""

from __future__ import annotations

PHRASES: dict[str, list[str]] = {
    "ko": [
        "감사합니다",
        "시청해 주셔서 감사합니다",
        "구독과 좋아요 부탁드립니다",
        "구독과 좋아요 알림설정 부탁드립니다",
        "다음 영상에서 만나요",
        "MBC 뉴스 * 입니다",
        "자막 제공 *",
    ],
    "en": [
        "Thank you",
        "Thanks for watching",
        "Thank you for watching",
        "Please subscribe",
        "Please like and subscribe",
        "See you in the next video",
        "Subtitles by the Amara.org community",
    ],
    "ja": [
        "ご視聴ありがとうございました",
        "ご視聴ありがとうございます",
        "最後までご視聴いただきありがとうございました",
        "チャンネル登録お願いします",
    ],
    "es": [
        "¡Gracias por ver!",
        "Gracias por ver el video",
        "Subtítulos realizados por *",
        "Subtítulos por la comunidad de Amara.org",
    ],
}

MAX_WILDCARD_CHARS = 30


def _normalize(text: str) -> str:
    return "".join(c for c in text.lower() if c.isalnum())


_COMPILED: list[tuple[str, str | None]] = []
for _entries in PHRASES.values():
    for _entry in _entries:
        _head, _star, _tail = _entry.partition("*")
        _COMPILED.append((_normalize(_head), _normalize(_tail) if _star else None))


def is_hallucination_phrase(text: str) -> bool:
    """Whether the whole text is a known hallucination phrase."""
    norm = _normalize(text)
    if not norm:
        return False
    for prefix, suffix in _COMPILED:
        if suffix is None:
            if norm == prefix:
                return True
        elif norm.startswith(prefix):
            rest = norm[len(prefix):]
            if rest.endswith(suffix) and len(rest) - len(suffix) <= MAX_WILDCARD_CHARS:
                return True
    return False
