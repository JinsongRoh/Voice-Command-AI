import { describe, expect, it } from 'vitest';

import { defaultSettings, inQuietHours, parseSettings, shouldSpeak, shouldToast } from './settings';

const at = (h: number, m = 0) => new Date(2026, 0, 1, h, m);

describe('settings', () => {
  it('detects the default language', () => {
    expect(defaultSettings(['ja-JP', 'en']).language).toBe('ja');
    expect(defaultSettings(['de-DE']).language).toBe('en');
  });

  it('handles quiet hours across midnight', () => {
    const q = { enabled: true, start: '22:00', end: '07:00' };
    expect(inQuietHours(q, at(23))).toBe(true);
    expect(inQuietHours(q, at(6, 59))).toBe(true);
    expect(inQuietHours(q, at(7))).toBe(false);
    expect(inQuietHours(q, at(12))).toBe(false);
    expect(inQuietHours({ ...q, enabled: false }, at(23))).toBe(false);
    expect(inQuietHours({ enabled: true, start: '09:00', end: '17:00' }, at(10))).toBe(true);
  });

  it('gates toasts by category and speech by quiet hours', () => {
    const s = {
      ...defaultSettings(),
      spokenAlerts: true,
      quietHours: { enabled: true, start: '22:00', end: '07:00' },
    };
    expect(shouldToast(s, 'decision')).toBe(true);
    expect(shouldToast(s, 'activity')).toBe(false);
    expect(shouldSpeak(s, 'decision', at(12))).toBe(true);
    expect(shouldSpeak(s, 'decision', at(23))).toBe(false);
    expect(shouldSpeak({ ...s, spokenAlerts: false }, 'decision', at(12))).toBe(false);
  });

  it('parses stored settings defensively', () => {
    const d = defaultSettings();
    expect(parseSettings(null, d)).toBe(d);
    expect(parseSettings('{bad', d)).toBe(d);
    const parsed = parseSettings(
      JSON.stringify({
        theme: 'dark',
        language: 'xx',
        sttEngine: 'openai',
        notifications: { activity: true },
      }),
      d,
    );
    expect(parsed.theme).toBe('dark');
    expect(parsed.language).toBe(d.language);
    expect(parsed.sttEngine).toBe('openai');
    expect(parsed.notifications).toEqual({ ...d.notifications, activity: true });
  });

  it('defaults to tap-to-toggle dictation and migrates old settings', () => {
    const d = defaultSettings();
    expect(d.micMode).toBe('toggle');
    // Saved before micMode existed.
    const old = JSON.stringify({ theme: 'dark', sttEngine: 'openai' });
    expect(parseSettings(old, d).micMode).toBe('toggle');
    expect(parseSettings(old, d).theme).toBe('dark');
    expect(parseSettings(JSON.stringify({ micMode: 'bogus' }), d).micMode).toBe('toggle');
    expect(parseSettings(JSON.stringify({ micMode: 'hold' }), d).micMode).toBe('hold');
  });
});
