import { DIAG_DEFAULT_ON } from '../diag/log';
import { detectLocale, isLocale, type Locale } from '../i18n/i18n';
import { DEFAULT_APP_LOCK_IDLE_MIN, isIdleChoice, type AppLockIdleMinutes } from './appLock';
import { DEFAULT_MIC_ANIMATION, isMicAnimation, type MicAnimation } from './micAnimation';

export type ThemeSetting = 'system' | 'light' | 'dark' | 'black';

/**
 * How the microphone button records: `toggle` = tap to start dictating, tap
 * again to stop (default; suits long dictation); `hold` = push-to-talk.
 */
export type MicMode = 'hold' | 'toggle';

/**
 * Language the speech recognizer listens for. `auto` (default) sends no
 * language, so the computer's speech settings decide (its primary and
 * secondary languages); the UI language is never used for this, because a
 * forced language makes Whisper translate speech in another language.
 */
export type SpeechLanguage = 'auto' | 'ko' | 'en' | 'ja' | 'es' | 'zh' | 'de' | 'fr';
export const SPEECH_LANGUAGES: SpeechLanguage[] = [
  'auto',
  'ko',
  'en',
  'ja',
  'es',
  'zh',
  'de',
  'fr',
];

/** The `language` to send with a transcription request (`null`: the server decides). */
export function speechLanguageParam(choice: SpeechLanguage): string | null {
  return choice === 'auto' ? null : choice;
}
export const MIC_MODES: MicMode[] = ['toggle', 'hold'];

/** Kinds of alerts the user can turn on or off individually (NTF-06). */
export type AlertCategory = 'decision' | 'question' | 'input' | 'error' | 'completed' | 'activity';
export const ALERT_CATEGORIES: AlertCategory[] = [
  'decision',
  'question',
  'input',
  'error',
  'completed',
  'activity',
];

/** What happens when a session needs an answer: `ask` shows Go / Later. */
export type ApprovalMode = 'ask' | 'auto' | 'off';

/** How a hands-free (Voco) message is confirmed: a 3 s undo window or a read-back. */
export type VocoConfirm = 'undo' | 'readback';
export const APPROVAL_MODES: ApprovalMode[] = ['ask', 'auto', 'off'];

export interface QuietHours {
  enabled: boolean;
  /** "HH:MM", local time. */
  start: string;
  end: string;
}

export interface Settings {
  theme: ThemeSetting;
  language: Locale;
  notifications: Record<AlertCategory, boolean>;
  spokenAlerts: boolean;
  /** `voiceURI` of the spoken-alert voice; empty = Auto (best available). */
  ttsVoice: string;
  /** Spoken-alert speed, 0.8 to 1.3. */
  ttsRate: number;
  /**
   * Voice source of spoken alerts: `null` = the device voice (default), else
   * the name of a TTS engine on the PC (`[[tts.engines]]`).
   */
  ttsServerEngine: string | null;
  /**
   * Speaker id (`F1`, `M2`, ...) of the PC engine; empty = the engine's
   * default (the first female speaker, or its configured voice).
   */
  ttsServerVoice: string;
  quietHours: QuietHours;
  micMode: MicMode;
  /** How the symbol on the dictation button moves (see `micAnimation.ts`). */
  micAnimation: MicAnimation;
  /**
   * Reduce background noise in the microphone signal before recognition
   * (default on): RNNoise in the native apps, the WebView's own filter in a
   * browser. Also applies to wake word listening.
   */
  noiseSuppression: boolean;
  speechLanguage: SpeechLanguage;
  approvalMode: ApprovalMode;
  /**
   * Hands-free mode (default off): dictations are read back and sent by a
   * spoken "yes"; pending `safe` decisions are read out and answered by voice.
   */
  handsFree: boolean;
  /** The one-time note "command text is read aloud" was acknowledged. */
  handsFreeNoteSeen: boolean;
  /**
   * Hands-free read-back says the whole text before "보낼까요?" (default off:
   * only the target session and the first few words).
   */
  handsFreeFullReadBack: boolean;
  /**
   * Hands-free listens for the wake word "보코" / "Voco" (default on; only
   * while hands-free mode is on). See `speech/vocoFlow.ts`.
   */
  vocoWake: boolean;
  /** Extra wake words, comma separated (added to the built-in ones). */
  vocoWakeWords: string;
  /** Extra send phrases, comma separated (said after a pause). */
  vocoSendPhrases: string;
  /**
   * After a send phrase: `undo` sends after a 3 s window in which "취소"
   * recalls it; `readback` asks "…보낼까요?" first (the older behaviour).
   */
  vocoConfirm: VocoConfirm;
  /** Schedule the send when a message ends in a command form and silence follows. */
  vocoAutoSend: boolean;
  /** Countdown of the scheduled send, 2 to 5 seconds. */
  vocoCountdownSec: number;
  /**
   * Read a short summary of a session's answer aloud when its turn ends
   * (default off: it reads conversation content). Uses the PC's LLM.
   */
  resultSummary: boolean;
  /** Summaries may name commands, paths and code (default off). */
  resultSummaryDetails: boolean;
  /** The one-line "what will be read aloud" note of the quick toggle was shown. */
  spokenAlertsNoteSeen: boolean;
  /**
   * App lock (default on): the app opens locked and locks again after
   * `appLockIdleMin` minutes without use. Turning it off needs a
   * verification; the Command Center asks for one either way.
   */
  appLock: boolean;
  appLockIdleMin: AppLockIdleMinutes;
  /**
   * Keep the diagnostic log of Settings -> About (default off: opt-in). Off
   * clears it (`diag/log.ts`).
   */
  diagnostics: boolean;
  /**
   * The person set `diagnostics` themselves. Builds before the opt-in default
   * saved `diagnostics: true` with every settings change, so a stored value
   * without this mark is the old default, not a choice, and is ignored.
   */
  diagnosticsChosen: boolean;
}

export function defaultSettings(languages: readonly string[] = []): Settings {
  return {
    theme: 'system',
    language: detectLocale(languages),
    notifications: {
      decision: true,
      question: true,
      input: true,
      error: true,
      completed: true,
      activity: false,
    },
    spokenAlerts: false,
    ttsVoice: '',
    ttsRate: 1,
    ttsServerEngine: null,
    ttsServerVoice: '',
    quietHours: { enabled: false, start: '22:00', end: '07:00' },
    micMode: 'toggle',
    micAnimation: DEFAULT_MIC_ANIMATION,
    noiseSuppression: true,
    speechLanguage: 'auto',
    approvalMode: 'ask',
    handsFree: false,
    handsFreeNoteSeen: false,
    handsFreeFullReadBack: false,
    vocoWake: true,
    vocoWakeWords: '',
    vocoSendPhrases: '',
    vocoConfirm: 'undo',
    vocoAutoSend: true,
    vocoCountdownSec: 3,
    resultSummary: false,
    resultSummaryDetails: false,
    spokenAlertsNoteSeen: false,
    appLock: true,
    appLockIdleMin: DEFAULT_APP_LOCK_IDLE_MIN,
    diagnostics: DIAG_DEFAULT_ON,
    diagnosticsChosen: false,
  };
}

const STORAGE_KEY = 'vca.settings.v1';

/** Merges stored values over defaults, ignoring anything malformed. */
export function parseSettings(raw: string | null, defaults: Settings): Settings {
  if (!raw) return defaults;
  try {
    const v = JSON.parse(raw) as Partial<Settings> & { phoneCarousel?: unknown };
    return {
      theme:
        v.theme === 'light' || v.theme === 'dark' || v.theme === 'black' || v.theme === 'system'
          ? v.theme
          : defaults.theme,
      language:
        typeof v.language === 'string' && isLocale(v.language) ? v.language : defaults.language,
      notifications: { ...defaults.notifications, ...(v.notifications ?? {}) },
      spokenAlerts: typeof v.spokenAlerts === 'boolean' ? v.spokenAlerts : defaults.spokenAlerts,
      ttsVoice: typeof v.ttsVoice === 'string' ? v.ttsVoice : defaults.ttsVoice,
      ttsRate:
        typeof v.ttsRate === 'number' && Number.isFinite(v.ttsRate)
          ? Math.min(1.3, Math.max(0.8, v.ttsRate))
          : defaults.ttsRate,
      ttsServerEngine:
        typeof v.ttsServerEngine === 'string' && v.ttsServerEngine
          ? v.ttsServerEngine
          : defaults.ttsServerEngine,
      ttsServerVoice:
        typeof v.ttsServerVoice === 'string' ? v.ttsServerVoice : defaults.ttsServerVoice,
      quietHours: { ...defaults.quietHours, ...(v.quietHours ?? {}) },
      // Settings saved before `micMode` existed get the default.
      micMode: MIC_MODES.includes(v.micMode as MicMode) ? (v.micMode as MicMode) : defaults.micMode,
      // Missing (saved before the setting existed) or unknown: the default style.
      micAnimation: isMicAnimation(v.micAnimation) ? v.micAnimation : defaults.micAnimation,
      noiseSuppression:
        typeof v.noiseSuppression === 'boolean' ? v.noiseSuppression : defaults.noiseSuppression,
      speechLanguage: SPEECH_LANGUAGES.includes(v.speechLanguage as SpeechLanguage)
        ? (v.speechLanguage as SpeechLanguage)
        : defaults.speechLanguage,
      approvalMode: APPROVAL_MODES.includes(v.approvalMode as ApprovalMode)
        ? (v.approvalMode as ApprovalMode)
        : defaults.approvalMode,
      handsFree: typeof v.handsFree === 'boolean' ? v.handsFree : defaults.handsFree,
      handsFreeNoteSeen:
        typeof v.handsFreeNoteSeen === 'boolean' ? v.handsFreeNoteSeen : defaults.handsFreeNoteSeen,
      handsFreeFullReadBack:
        typeof v.handsFreeFullReadBack === 'boolean'
          ? v.handsFreeFullReadBack
          : defaults.handsFreeFullReadBack,
      vocoWake: typeof v.vocoWake === 'boolean' ? v.vocoWake : defaults.vocoWake,
      vocoWakeWords: typeof v.vocoWakeWords === 'string' ? v.vocoWakeWords : defaults.vocoWakeWords,
      vocoSendPhrases:
        typeof v.vocoSendPhrases === 'string' ? v.vocoSendPhrases : defaults.vocoSendPhrases,
      vocoConfirm:
        v.vocoConfirm === 'undo' || v.vocoConfirm === 'readback'
          ? v.vocoConfirm
          : defaults.vocoConfirm,
      vocoAutoSend: typeof v.vocoAutoSend === 'boolean' ? v.vocoAutoSend : defaults.vocoAutoSend,
      vocoCountdownSec:
        typeof v.vocoCountdownSec === 'number' && Number.isFinite(v.vocoCountdownSec)
          ? Math.min(5, Math.max(2, Math.round(v.vocoCountdownSec)))
          : defaults.vocoCountdownSec,
      resultSummary:
        typeof v.resultSummary === 'boolean' ? v.resultSummary : defaults.resultSummary,
      resultSummaryDetails:
        typeof v.resultSummaryDetails === 'boolean'
          ? v.resultSummaryDetails
          : defaults.resultSummaryDetails,
      spokenAlertsNoteSeen:
        typeof v.spokenAlertsNoteSeen === 'boolean'
          ? v.spokenAlertsNoteSeen
          : defaults.spokenAlertsNoteSeen,
      appLock: typeof v.appLock === 'boolean' ? v.appLock : defaults.appLock,
      appLockIdleMin: isIdleChoice(v.appLockIdleMin) ? v.appLockIdleMin : defaults.appLockIdleMin,
      // Only a value the person chose counts: older builds stored the old
      // default (on) whether or not anyone touched the switch.
      ...(v.diagnosticsChosen === true && typeof v.diagnostics === 'boolean'
        ? { diagnostics: v.diagnostics, diagnosticsChosen: true }
        : { diagnostics: defaults.diagnostics, diagnosticsChosen: defaults.diagnosticsChosen }),
      // Removed settings (`phoneStrip`, `phoneStripChosen`, `phoneCarousel`)
      // in older saved settings are ignored: only the fields above are kept.
    };
  } catch {
    return defaults;
  }
}

/** "a, b ,c" -> ["a", "b", "c"] (user wake words and send phrases). */
export function phraseList(value: string): string[] {
  return value
    .split(/[,，、\n]/u)
    .map((w) => w.trim())
    .filter(Boolean);
}

export function loadSettings(): Settings {
  const defaults = defaultSettings(typeof navigator === 'undefined' ? [] : navigator.languages);
  try {
    return parseSettings(localStorage.getItem(STORAGE_KEY), defaults);
  } catch {
    return defaults;
  }
}

export function saveSettings(settings: Settings): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
  } catch {
    // Storage may be unavailable (private mode); settings then last for the session.
  }
}

function minutes(hhmm: string): number {
  const [h, m] = hhmm.split(':').map(Number);
  return (h ?? 0) * 60 + (m ?? 0);
}

/** True when `date` falls inside the quiet hours window (which may cross midnight). */
export function inQuietHours(quiet: QuietHours, date: Date): boolean {
  if (!quiet.enabled) return false;
  const now = date.getHours() * 60 + date.getMinutes();
  const start = minutes(quiet.start);
  const end = minutes(quiet.end);
  if (start === end) return false;
  return start < end ? now >= start && now < end : now >= start || now < end;
}

/** Whether an in-app alert (toast) of this category should be shown. */
export function shouldToast(settings: Settings, category: AlertCategory): boolean {
  return settings.notifications[category];
}

/**
 * Whether an alert should also be spoken; quiet hours silence spoken alerts.
 * Hands-free mode speaks decisions even with spoken alerts off: eyes off the
 * screen, a decision waiting in another session would otherwise go unnoticed.
 * The `spokenAlerts` setting itself is left as the user chose it.
 */
export function shouldSpeak(settings: Settings, category: AlertCategory, date: Date): boolean {
  const voiced = settings.spokenAlerts || (settings.handsFree && category === 'decision');
  return voiced && settings.notifications[category] && !inQuietHours(settings.quietHours, date);
}
