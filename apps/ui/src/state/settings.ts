import { detectLocale, isLocale, type Locale } from '../i18n/i18n';

export type ThemeSetting = 'system' | 'light' | 'dark';
export type SttEngine = 'whisper_server' | 'whisper_device' | 'openai' | 'google';
export const STT_ENGINES: SttEngine[] = ['whisper_server', 'whisper_device', 'openai', 'google'];

/**
 * How the microphone button records: `toggle` = tap to start dictating, tap
 * again to stop (default; suits long dictation); `hold` = push-to-talk.
 */
export type MicMode = 'hold' | 'toggle';
export const MIC_MODES: MicMode[] = ['toggle', 'hold'];

/** Kinds of alerts the user can turn on or off individually (NTF-06). */
export type AlertCategory = 'decision' | 'input' | 'error' | 'completed' | 'activity';
export const ALERT_CATEGORIES: AlertCategory[] = [
  'decision',
  'input',
  'error',
  'completed',
  'activity',
];

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
  quietHours: QuietHours;
  sttEngine: SttEngine;
  micMode: MicMode;
}

export function defaultSettings(languages: readonly string[] = []): Settings {
  return {
    theme: 'system',
    language: detectLocale(languages),
    notifications: { decision: true, input: true, error: true, completed: true, activity: false },
    spokenAlerts: false,
    quietHours: { enabled: false, start: '22:00', end: '07:00' },
    sttEngine: 'whisper_server',
    micMode: 'toggle',
  };
}

const STORAGE_KEY = 'vca.settings.v1';

/** Merges stored values over defaults, ignoring anything malformed. */
export function parseSettings(raw: string | null, defaults: Settings): Settings {
  if (!raw) return defaults;
  try {
    const v = JSON.parse(raw) as Partial<Settings>;
    return {
      theme:
        v.theme === 'light' || v.theme === 'dark' || v.theme === 'system'
          ? v.theme
          : defaults.theme,
      language:
        typeof v.language === 'string' && isLocale(v.language) ? v.language : defaults.language,
      notifications: { ...defaults.notifications, ...(v.notifications ?? {}) },
      spokenAlerts: typeof v.spokenAlerts === 'boolean' ? v.spokenAlerts : defaults.spokenAlerts,
      quietHours: { ...defaults.quietHours, ...(v.quietHours ?? {}) },
      sttEngine: STT_ENGINES.includes(v.sttEngine as SttEngine)
        ? (v.sttEngine as SttEngine)
        : defaults.sttEngine,
      // Settings saved before `micMode` existed get the default.
      micMode: MIC_MODES.includes(v.micMode as MicMode) ? (v.micMode as MicMode) : defaults.micMode,
    };
  } catch {
    return defaults;
  }
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

/** Whether an alert should also be spoken; quiet hours silence spoken alerts. */
export function shouldSpeak(settings: Settings, category: AlertCategory, date: Date): boolean {
  return (
    settings.spokenAlerts &&
    settings.notifications[category] &&
    !inQuietHours(settings.quietHours, date)
  );
}
