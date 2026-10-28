//! A scripted engine for tests and demos.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use super::{SttEngine, TranscribeOptions, Transcript};
use crate::audio::AudioInput;
use crate::error::{Result, SpeechError};

/// Returns queued transcripts in order (then repeats a fallback, if any) and
/// records the options of every call.
#[derive(Debug, Default)]
pub struct MockSttEngine {
    queue: Mutex<VecDeque<Result<Transcript, String>>>,
    fallback: Option<Transcript>,
    calls: Mutex<Vec<(usize, TranscribeOptions)>>,
}

impl MockSttEngine {
    /// An engine that always returns `text`.
    pub fn always(text: impl Into<String>) -> Self {
        Self {
            fallback: Some(Transcript::plain(text)),
            ..Self::default()
        }
    }

    /// Queues a transcript for the next call.
    pub fn push(&self, transcript: Transcript) -> &Self {
        self.lock_queue().push_back(Ok(transcript));
        self
    }

    /// Queues an engine failure for the next call.
    pub fn push_error(&self, message: impl Into<String>) -> &Self {
        self.lock_queue().push_back(Err(message.into()));
        self
    }

    /// Audio byte lengths and options of all calls so far.
    pub fn calls(&self) -> Vec<(usize, TranscribeOptions)> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn lock_queue(&self) -> std::sync::MutexGuard<'_, VecDeque<Result<Transcript, String>>> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl SttEngine for MockSttEngine {
    fn name(&self) -> &str {
        "mock"
    }

    async fn transcribe(
        &self,
        audio: &AudioInput,
        options: &TranscribeOptions,
    ) -> Result<Transcript> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((audio.bytes.len(), options.clone()));
        match self.lock_queue().pop_front() {
            Some(Ok(t)) => Ok(t),
            Some(Err(e)) => Err(SpeechError::Engine(e)),
            None => self.fallback.clone().ok_or_else(|| {
                SpeechError::Engine("mock engine has no scripted transcript".into())
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_queue_then_fallback() {
        let m = MockSttEngine::always("default");
        m.push(Transcript::plain("first")).push_error("boom");
        let audio = AudioInput::from_pcm16(&[0; 10], 16_000);
        let o = TranscribeOptions::default();
        assert_eq!(m.transcribe(&audio, &o).await.unwrap().text, "first");
        assert!(m.transcribe(&audio, &o).await.is_err());
        assert_eq!(m.transcribe(&audio, &o).await.unwrap().text, "default");
        assert_eq!(m.calls().len(), 3);
        assert_eq!(m.calls()[0].0, 20);
    }
}
