# Text-to-speech engine benchmark

Voice Command AI can read alerts aloud with your own TTS engine (see [speech-engines.md](speech-engines.md)). To help you pick one, we compared seven open-source engines on the same phrases in Korean, English, Japanese and Spanish. VCA ships no engine and no model weights: you install the engine you choose and connect it in the app.

Measured on 2026-10-31.

## Results

| Engine | Device | Load (s) | RTF (median) | Memory (GB) | Sample rate | CER ko | CER en | CER ja | CER es | Code licence | Weights licence |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **Supertonic-3** (F1, GPU) | GPU | 0.9 | 0.043 | 2.4 | 44.1 kHz | 0.000 | 0.000 | 0.031 | 0.000 | MIT | BigScience OpenRAIL-M |
| **Supertonic-3** (F1, CPU, 4 threads) | CPU | 0.6 | 0.196 | 0.4 | 44.1 kHz | 0.000 | 0.000 | 0.000 | 0.000 | MIT | BigScience OpenRAIL-M |
| **MeloTTS** | GPU | 5.8 | 0.024 | 3.7 | 44.1 kHz | 0.000 | 0.098 | 0.020 | 0.000 | MIT | MIT |
| Chatterbox Multilingual (v3) | GPU | 11.7 | 0.570 | 6.4 | 24 kHz | 0.055 | 0.000 | 0.013 | 0.002 | MIT | MIT (outputs carry a Perth watermark) |
| CosyVoice 2 (0.5B) | GPU | 9.5 | 0.548 | 6.9 | 24 kHz | 0.067 | 0.000 | 0.200 | 0.316 † | Apache-2.0 | Apache-2.0 |
| XTTS-v2 | GPU | 16.1 | 0.219 | 6.6 | 24 kHz | 0.649 | 0.000 | 0.029 | 0.000 | MPL-2.0 | CPML (non-commercial) |
| F5-TTS (v1 Base) | GPU | 6.0 | 0.563 | 3.8 | 24 kHz | 0.983 ‡ | 0.000 | 0.968 ‡ | 0.131 | MIT | CC-BY-NC-4.0 (non-commercial) |
| Fish Speech (S2-Pro, 4B) | GPU | 136.3 | 2.563 | 22.9 | 44.1 kHz | 0.000 | 0.000 | 0.000 | 0.000 | Fish Audio Community License | Fish Audio Research License (non-commercial) |

† CosyVoice 2 does not support Spanish. ‡ The F5-TTS base model covers English and Chinese only.

- **RTF** (real-time factor): seconds of compute per second of audio, median of 20 phrases after warm-up. Below 1.0 is faster than real time.
- **Memory**: the drop in free system memory while the engine runs (the test machine shares one memory pool between CPU and GPU).
- **CER** (character error rate): each clip was transcribed with Whisper (`faster-whisper` large-v3-turbo, int8, language forced) and compared with the input text. 0 is perfect; per-clip values are capped at 1.0. Spelling variants of numbers ("3" vs "three") count as correct.

## Observations

- **Supertonic-3** was accurate in all four languages, fast on GPU and real-time even on four CPU threads, with the smallest memory footprint. It supports 31 languages and ships five female (F1–F5) and five male (M1–M5) voices.
- **MeloTTS** was the fastest engine and is MIT throughout, a good fallback. English intelligibility was a little lower.
- **Chatterbox** sounded natural but was about 20× slower than Supertonic on the same GPU.
- **CosyVoice 2** has no built-in voice (we used the reference clip shipped in its repository), no Spanish, and was unstable in Japanese.
- **XTTS-v2** turned some short Korean phrases into unrelated speech.
- **Fish Speech S2-Pro** was perfectly intelligible but slower than real time and needed about 23 GB.
- **Numbers in Korean**: several engines read "3번" as "세 번" (three times) instead of "삼번" (number three). VCA rewrites numbered items to the Sino-Korean reading before it sends text to any engine.

## Licences

Check both the code licence and the **model weights** licence for your use; they often differ.

- **Supertonic-3** weights use the BigScience OpenRAIL-M licence. Commercial use is allowed, but its use restrictions carry over to anyone who redistributes or uses the model, including clause (e): generated content must not be presented as human. VCA shows a "machine-generated voice" note in Settings when an engine declares it.
- **XTTS-v2** (CPML), **F5-TTS** (CC-BY-NC-4.0) and **Fish Speech** (Fish Audio Research License) weights are non-commercial. Using them inside a company can count as commercial use; read the licence before you deploy them.
- **MeloTTS**, **Chatterbox** and **CosyVoice 2** weights are MIT or Apache-2.0.

## Method

- **Machine**: an ARM64 (aarch64) Linux workstation with an NVIDIA GPU. One engine ran at a time in its own Python virtual environment.
- **Voices**: each engine's default or built-in voice. Engines without one used the reference clip shipped in their own repository. No voice cloning of real people.
- **Phrases**: five per language, for example "3번 세션, 결정이 필요합니다", "Session 3 needs your decision", "セッション3、判断が必要です", "Sesión 3, necesita tu decisión", plus a longer sentence.
- **Install notes**:
  - PyTorch 2.9 and later need the FFmpeg shared libraries for `torchcodec`; without them use torch 2.8.
  - `onnxruntime-gpu` on aarch64 needs `onnxruntime.preload_dlls()` before creating a session.
  - MeloTTS needs `python-mecab-ko` for Korean.
