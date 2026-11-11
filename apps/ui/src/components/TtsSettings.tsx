import { useEffect, useMemo, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { TranslateFn } from '../i18n/i18n';
import type { TtsEngineInfo, TtsGender, TtsSpeaker } from '../protocol/types';
import {
  TTS_RATE_MAX,
  TTS_RATE_MIN,
  detectTtsPlatform,
  onlyLegacyVoices,
  ttsAvailable,
  voicesForLang,
  watchVoices,
} from '../speech/tts';
import { previewVoice } from '../speech/handsFree';
import { offeredGenders, resolveSpeaker, speakersByGender, speakersOf } from '../speech/speakers';
import { useActions, useAppState, useSettings } from '../state/store';

/** `<select>` value of the device voice source (engine names never contain `:`). */
const DEVICE = 'device:';

/** `F1` and `M2` are shown as "Female 1" / "Male 2" in the user's language. */
function speakerName(t: TranslateFn, speaker: TtsSpeaker, index: number): string {
  if (speaker.gender === 'other') return speaker.label;
  const n = /(\d+)$/.exec(speaker.id)?.[1] ?? String(index + 1);
  return t(
    speaker.gender === 'female' ? 'settings.tts.speakerFemale' : 'settings.tts.speakerMale',
    {
      n: Number(n),
    },
  );
}

/**
 * Spoken-alert voice: the source (this device, or a TTS engine on the PC
 * when the PC has any), plus voice, speed and preview for the device voice.
 * Preview uses the chosen source.
 */
export function TtsSettings() {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  const actions = useActions();
  const connected = useAppState((s) => s.connection === 'connected');
  const [voices, setVoices] = useState<SpeechSynthesisVoice[]>([]);
  const [engines, setEngines] = useState<TtsEngineInfo[]>([]);

  useEffect(() => watchVoices(setVoices), []);

  useEffect(() => {
    if (!connected) return;
    let active = true;
    actions.listTtsEngines().then(
      (list) => {
        if (active) setEngines(list);
      },
      () => {
        // Older servers do not know the request: only the device voice.
      },
    );
    return () => {
      active = false;
    };
  }, [actions, connected]);

  const server = settings.ttsServerEngine;
  const names = engines.map((e) => e.name);
  // A saved engine stays selectable while the list is loading or gone.
  if (server !== null && !names.includes(server)) names.push(server);
  const showSource = names.length > 0;
  const engine = engines.find((e) => e.name === server);
  const speakers = useMemo(() => speakersOf(engine), [engine]);
  // A saved speaker the engine no longer offers falls back to the default.
  const speaker = resolveSpeaker(engine, settings.ttsServerVoice);
  const genders = offeredGenders(speakers);
  const device = ttsAvailable();
  if (!device && !showSource) return null;

  const list = voicesForLang(voices, settings.language);
  // A saved voice that is gone (or belongs to another language) shows as Auto.
  const selected = list.some((v) => v.voiceURI === settings.ttsVoice) ? settings.ttsVoice : '';
  const hint =
    server === null && onlyLegacyVoices(voices, settings.language)
      ? t(`settings.tts.hint.${detectTtsPlatform()}`)
      : null;

  // Both sources through the app's one speaker, holding the voice.
  const preview = () => {
    const sample = t('settings.tts.sample');
    void previewVoice((signal) =>
      actions.speakAlert(
        sample,
        settings.language,
        {
          serverEngine: server,
          serverVoice: speaker?.id ?? null,
          voice: selected,
          rate: settings.ttsRate,
        },
        signal,
      ),
    );
  };

  return (
    <div>
      {showSource && (
        <div className="field">
          <label htmlFor="set-tts-source">{t('settings.tts.source')}</label>
          <select
            id="set-tts-source"
            value={server ?? DEVICE}
            onChange={(e) =>
              updateSettings({
                ttsServerEngine: e.target.value === DEVICE ? null : e.target.value,
              })
            }
          >
            <option value={DEVICE}>{t('settings.tts.sourceDevice')}</option>
            {names.map((name) => (
              <option key={name} value={name}>
                {t('settings.tts.sourceServer', { name })}
              </option>
            ))}
          </select>
          {server !== null && (
            <p className="muted" style={{ margin: '6px 0 0', fontSize: 13 }}>
              {t('settings.tts.serverHelp')}
            </p>
          )}
        </div>
      )}
      {server !== null && speaker && (
        <div className="field">
          <span className="label" id="set-tts-speaker-label">
            {t('settings.tts.speaker')}
          </span>
          <div className="segmented" role="group" aria-label={t('settings.tts.gender')}>
            {genders.map((g: TtsGender) => (
              <button
                key={g}
                type="button"
                aria-pressed={speaker.gender === g}
                onClick={() => {
                  if (speaker.gender === g) return;
                  const first = speakersByGender(speakers, g)[0];
                  if (first) updateSettings({ ttsServerVoice: first.id });
                }}
              >
                {t(`settings.tts.gender.${g}`)}
              </button>
            ))}
          </div>
          <select
            aria-label={t('settings.tts.speakerVoice')}
            style={{ marginTop: 8 }}
            value={speaker.id}
            onChange={(e) => updateSettings({ ttsServerVoice: e.target.value })}
          >
            {speakersByGender(speakers, speaker.gender).map((sp, i) => (
              <option key={sp.id} value={sp.id}>
                {speakerName(t, sp, i)}
              </option>
            ))}
          </select>
        </div>
      )}
      {server !== null && engine?.disclosure && (
        <p className="muted" style={{ margin: '0 0 8px', fontSize: 13 }}>
          {engine.disclosure}
        </p>
      )}
      {server === null && device && (
        <>
          <div className="field">
            <label htmlFor="set-tts-voice">{t('settings.tts.voice')}</label>
            <select
              id="set-tts-voice"
              value={selected}
              onChange={(e) => updateSettings({ ttsVoice: e.target.value })}
            >
              <option value="">{t('settings.tts.voiceAuto')}</option>
              {list.map((v) => (
                <option key={v.voiceURI} value={v.voiceURI}>
                  {v.name}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label htmlFor="set-tts-rate">
              {t('settings.tts.rate')} ({settings.ttsRate.toFixed(1)}x)
            </label>
            <input
              id="set-tts-rate"
              type="range"
              min={TTS_RATE_MIN}
              max={TTS_RATE_MAX}
              step={0.1}
              value={settings.ttsRate}
              onChange={(e) => updateSettings({ ttsRate: Number(e.target.value) })}
            />
          </div>
        </>
      )}
      {(server !== null || device) && (
        <button type="button" onClick={preview}>
          {t('settings.tts.preview')}
        </button>
      )}
      {hint && (
        <p className="muted" style={{ margin: '6px 0 0', fontSize: 13 }}>
          {hint}
        </p>
      )}
    </div>
  );
}
