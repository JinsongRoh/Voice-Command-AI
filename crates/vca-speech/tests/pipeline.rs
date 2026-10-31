//! End-to-end pipeline: VAD → STT (mock) → clean-up → vocabulary → intent.

use std::path::Path;

use vca_speech::cleanup::FillerRemover;
use vca_speech::intent::{IntentContext, SessionAlias};
use vca_speech::stt::MockSttEngine;
use vca_speech::vad::{VadConfig, VadEvent, VoiceActivityDetector};
use vca_speech::{
    AudioInput, ControlAction, Intent, IntentRouter, SttEngine, Target, TranscribeOptions,
    Transcript, VocabularyEngine,
};

fn tone(ms: u32) -> Vec<i16> {
    (0..16 * ms)
        .map(|i| ((i as f32 * 0.09).sin() * 9000.0) as i16)
        .collect()
}

#[tokio::test]
async fn utterances_flow_through_the_pipeline() {
    let vocab = VocabularyEngine::from_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lexicon"),
        None,
    )
    .unwrap();
    let stt = MockSttEngine::default();
    stt.push(Transcript::plain(
        "um, ask cloud code to fix the use effect hook",
    ))
    .push(Transcript::plain("세션 2 일시정지 해줘"));
    let router = IntentRouter::new();
    let filler = FillerRemover::new();
    let ctx = IntentContext {
        sessions: vec![SessionAlias {
            id: "s-2".into(),
            number: Some(2),
            names: vec!["api".into()],
        }],
        ..IntentContext::default()
    };
    let options = TranscribeOptions {
        primary_language: Some("en".into()),
        secondary_languages: vec!["ko".into()],
        prompt_hint: Some(vocab.hint_prompt(400)),
        phrase_hints: vec![],
    };

    let mut vad = VoiceActivityDetector::new(VadConfig::default());
    let mut audio = vec![0i16; 16 * 300];
    audio.extend(tone(800));
    audio.extend(vec![0i16; 16 * 1200]);
    audio.extend(tone(600));
    audio.extend(vec![0i16; 16 * 1200]);

    let mut intents = Vec::new();
    for event in vad.push(&audio) {
        let VadEvent::SpeechEnd { samples, .. } = event else {
            continue;
        };
        let input = AudioInput::from_pcm16(&samples, 16_000);
        let transcript = stt.transcribe(&input, &options).await.unwrap();
        let cleaned = filler.remove(&transcript.text);
        let corrected = vocab.correct(&cleaned);
        intents.push((corrected.text.clone(), router.route(&corrected.text, &ctx)));
    }

    assert_eq!(intents.len(), 2);
    assert_eq!(intents[0].0, "ask Claude Code to fix the useEffect hook");
    assert!(matches!(intents[0].1, Intent::Prompt { .. }));
    assert_eq!(
        intents[1].1,
        Intent::Control {
            action: ControlAction::PauseSession,
            target: Some(Target::Session("s-2".into())),
            confidence: 0.95,
        }
    );
    let calls = stt.calls();
    assert_eq!(calls.len(), 2);
    assert!(
        calls[0]
            .1
            .prompt_hint
            .as_deref()
            .unwrap()
            .contains("Claude Code")
    );
}
