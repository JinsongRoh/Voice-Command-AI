//! Local Whisper inference through `whisper-rs` (whisper.cpp bindings).
//!
//! Enabled by the `whisper-local` cargo feature. Accepts WAV or PCM16 input,
//! converts it to 16 kHz mono `f32` and runs inference on a blocking thread.
//! GPU acceleration depends on how whisper.cpp is built (see the whisper-rs
//! features `cuda`, `metal`, `vulkan`); this module only uses the CPU API.
//! Unverified on Windows and macOS.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{Segment, SttEngine, TranscribeOptions, Transcript, base_language};
use crate::audio::{AudioInput, to_whisper_input};
use crate::error::{Result, SpeechError};

/// Configuration of [`WhisperLocalEngine`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhisperLocalConfig {
    /// Path to a ggml/gguf Whisper model (for example `ggml-large-v3-turbo.bin`).
    pub model_path: PathBuf,
    /// Inference threads; `0` uses the available parallelism (max 8).
    pub threads: usize,
    /// Use the GPU when whisper.cpp was built with GPU support.
    pub use_gpu: bool,
}

impl WhisperLocalConfig {
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            threads: 0,
            use_gpu: true,
        }
    }
}

/// Speech-to-text with a local Whisper model.
pub struct WhisperLocalEngine {
    ctx: Arc<WhisperContext>,
    threads: usize,
}

impl std::fmt::Debug for WhisperLocalEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperLocalEngine")
            .field("threads", &self.threads)
            .finish_non_exhaustive()
    }
}

impl WhisperLocalEngine {
    /// Loads the model. This is slow (seconds) and memory-heavy; call it once
    /// and share the engine.
    pub fn load(config: &WhisperLocalConfig) -> Result<Self> {
        if !config.model_path.is_file() {
            return Err(SpeechError::Config(format!(
                "Whisper model not found: {}",
                config.model_path.display()
            )));
        }
        let mut params = WhisperContextParameters::default();
        params.use_gpu(config.use_gpu);
        let ctx = WhisperContext::new_with_params(&config.model_path, params)
            .map_err(|e| SpeechError::Engine(e.to_string()))?;
        let threads = if config.threads == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get().min(8))
                .unwrap_or(4)
        } else {
            config.threads
        };
        Ok(Self {
            ctx: Arc::new(ctx),
            threads,
        })
    }
}

#[async_trait]
impl SttEngine for WhisperLocalEngine {
    fn name(&self) -> &str {
        "whisper-local"
    }

    async fn transcribe(
        &self,
        audio: &AudioInput,
        options: &TranscribeOptions,
    ) -> Result<Transcript> {
        let samples = to_whisper_input(audio)?;
        let ctx = Arc::clone(&self.ctx);
        let threads = self.threads;
        // Whisper takes a single language or auto-detection; with secondary
        // languages configured we let it detect so code-switching works.
        let language = options
            .primary_language
            .as_deref()
            .filter(|_| options.secondary_languages.is_empty())
            .map(base_language);
        let prompt = options.prompt_hint.clone().unwrap_or_default();
        tokio::task::spawn_blocking(move || run(&ctx, &samples, threads, language, &prompt))
            .await
            .map_err(|e| SpeechError::Engine(e.to_string()))?
    }
}

fn run(
    ctx: &WhisperContext,
    samples: &[f32],
    threads: usize,
    language: Option<String>,
    prompt: &str,
) -> Result<Transcript> {
    let engine_err = |e: whisper_rs::WhisperError| SpeechError::Engine(e.to_string());
    let mut state = ctx.create_state().map_err(engine_err)?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(threads as std::ffi::c_int);
    params.set_language(Some(language.as_deref().unwrap_or("auto")));
    params.set_translate(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    params.set_print_timestamps(false);
    if !prompt.is_empty() {
        params.set_initial_prompt(prompt);
    }
    state.full(params, samples).map_err(engine_err)?;
    let detected = whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_owned);
    let mut segments = Vec::new();
    for seg in state.as_iter() {
        let text = seg.to_str_lossy().map_err(engine_err)?.trim().to_owned();
        if text.is_empty() {
            continue;
        }
        // Timestamps are in units of 10 ms.
        let start = u64::try_from(seg.start_timestamp()).ok().map(|t| t * 10);
        let end = u64::try_from(seg.end_timestamp()).ok().map(|t| t * 10);
        segments.push(Segment {
            text,
            start_ms: start,
            end_ms: end,
            confidence: Some((1.0 - seg.no_speech_probability()).clamp(0.0, 1.0)),
            language: None,
        });
    }
    let text = super::google::join_segments(segments.iter().map(|s| s.text.as_str()));
    Ok(Transcript {
        text,
        language: language.or(detected),
        segments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_model_is_a_config_error() {
        let err = WhisperLocalEngine::load(&WhisperLocalConfig::new("/nonexistent/ggml.bin"))
            .unwrap_err();
        assert!(matches!(err, SpeechError::Config(_)));
    }

    /// Runs real inference when `VCA_WHISPER_MODEL` points to a ggml model:
    /// `VCA_WHISPER_MODEL=ggml-tiny.bin cargo test -p vca-speech --features whisper-local -- --ignored`
    #[tokio::test]
    #[ignore = "needs a Whisper model file"]
    async fn transcribes_with_a_real_model() {
        let Ok(path) = std::env::var("VCA_WHISPER_MODEL") else {
            return;
        };
        let engine = WhisperLocalEngine::load(&WhisperLocalConfig::new(path)).unwrap();
        let samples = vec![0i16; 16_000 * 2];
        let audio = AudioInput::from_pcm16(&samples, 16_000);
        let t = engine
            .transcribe(&audio, &TranscribeOptions::default())
            .await
            .unwrap();
        assert!(t.text.len() < 200);
    }
}
