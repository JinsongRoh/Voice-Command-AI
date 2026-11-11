# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
"""A small, hardened, OpenAI-compatible speech-to-text server for Voice Command AI.

It serves Whisper models through faster-whisper behind per-user bearer
tokens, rate limits, size caps and a one-job-at-a-time GPU queue. It ships
no model weights: the model is downloaded from Hugging Face on first start.
"""

__version__ = "0.1.0"
