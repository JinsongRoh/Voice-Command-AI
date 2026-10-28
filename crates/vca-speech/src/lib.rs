//! Speech pipeline for Voice Command AI.
//!
//! The pipeline (architecture §9) is:
//!
//! ```text
//! mic ─► VAD ─► audio ─► STT engine ─► clean-up ─► vocabulary correction ─► intent router ─► editable transcript
//! ```
//!
//! - [`vad`]: energy-based voice activity detection that ends utterances.
//! - [`stt`]: the [`SttEngine`] trait and its implementations (OpenAI-compatible
//!   HTTP API, Google Cloud Speech-to-Text v2, local Whisper behind the
//!   `whisper-local` feature, and a mock for tests).
//! - [`lexicon`]: the three-layer developer vocabulary engine (§3.3.1) that
//!   produces STT hint prompts and corrects transcripts after recognition.
//! - [`intent`]: separates control commands ("pause session three") from
//!   prompts meant for the agent.
//! - [`cleanup`]: deterministic filler-word removal plus a hook for an
//!   optional LLM clean-up pass.

pub mod audio;
pub mod cleanup;
pub mod error;
pub mod intent;
pub mod lexicon;
pub mod stt;
pub mod text;
pub mod vad;

pub use audio::{AudioFormat, AudioInput};
pub use error::{Result, SpeechError};
pub use intent::{ControlAction, Intent, IntentContext, IntentRouter, Target};
pub use lexicon::{CorrectionResult, Layer, VocabularyEngine};
pub use stt::{Segment, SttEngine, TranscribeOptions, Transcript};
