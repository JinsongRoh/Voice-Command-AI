//! Speech pipeline for Voice Command AI.
//!
//! The pipeline (architecture §9) is:
//!
//! ```text
//! mic ─► VAD ─► audio ─► STT engine ─► clean-up ─► vocabulary correction ─► intent router ─► editable transcript
//! ```
//!
//! - [`vad`]: energy-based voice activity detection that ends utterances.
//! - [`auth`]: where engine requests carry the API key, and an optional
//!   gateway token for engines behind an authenticating reverse proxy.
//! - [`stt`]: the [`SttEngine`] trait and its implementations (OpenAI-compatible
//!   HTTP API, Google Cloud Speech-to-Text v2, local Whisper behind the
//!   `whisper-local` feature, and a mock for tests).
//! - [`lexicon`]: the three-layer developer vocabulary engine (§3.3.1) that
//!   produces STT hint prompts and corrects transcripts after recognition.
//! - [`intent`]: separates control commands ("pause session three") from
//!   prompts meant for the agent.
//! - [`cleanup`]: deterministic filler-word removal plus a hook for an
//!   optional LLM clean-up pass.
//! - [`llm`]: optional contextual correction of misheard words and topic
//!   detection through an OpenAI-compatible chat completions endpoint.
//! - [`radio`]: the radio-style voice words (wake word "Voco", send word
//!   "Over", reply "Roger"): prompt words and their TTS pronunciation.
//! - [`pronounce`]: the pronunciation dictionary of spoken alerts (how a
//!   voice says names written in Latin letters) and reading suggestions.
//! - [`retry`]: one retry for transcription requests whose connection
//!   dropped before the server answered.
//! - [`tts`]: the [`TtsEngine`] trait for spoken alerts synthesized by the
//!   owner's own TTS server (OpenAI-compatible speech endpoint).

pub mod audio;
pub mod auth;
pub mod cleanup;
pub mod error;
pub mod intent;
pub mod ko_text;
pub mod lexicon;
pub mod llm;
pub mod pronounce;
pub mod radio;
pub mod retry;
pub mod stt;
pub mod text;
pub mod tts;
pub mod vad;

pub use audio::{AudioFormat, AudioInput};
pub use auth::EngineAuth;
pub use error::{Result, SpeechError};
pub use intent::{ControlAction, Intent, IntentContext, IntentRouter, Target};
pub use lexicon::{CorrectionResult, Layer, VocabularyEngine};
pub use stt::{Segment, SttEngine, TranscribeOptions, Transcript};
pub use tts::{SynthesisRequest, Synthesized, TtsEngine, TtsFormat};
