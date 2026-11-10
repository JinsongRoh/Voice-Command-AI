import { describe, expect, it } from 'vitest';

import {
  defaultSettings,
  inQuietHours,
  parseSettings,
  phraseList,
  shouldSpeak,
  shouldToast,
} from './settings';

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

  it('speaks decisions in hands-free mode without turning spoken alerts on', () => {
    const s = { ...defaultSettings(), spokenAlerts: false, handsFree: true };
    expect(shouldSpeak(s, 'decision', at(12))).toBe(true);
    // Only decisions: completions stay silent unless spoken alerts are on.
    expect(shouldSpeak(s, 'completed', at(12))).toBe(false);
    // The per-category switch and quiet hours still apply.
    expect(
      shouldSpeak(
        { ...s, notifications: { ...s.notifications, decision: false } },
        'decision',
        at(12),
      ),
    ).toBe(false);
    expect(
      shouldSpeak(
        { ...s, quietHours: { enabled: true, start: '22:00', end: '07:00' } },
        'decision',
        at(23),
      ),
    ).toBe(false);
    expect(s.spokenAlerts).toBe(false);
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
    expect(parsed).not.toHaveProperty('sttEngine');
    expect(parsed.notifications).toEqual({ ...d.notifications, activity: true });
  });

  it('alerts for questions by default, also with settings saved before they existed', () => {
    const d = defaultSettings();
    expect(d.notifications.question).toBe(true);
    const old = parseSettings(
      JSON.stringify({
        notifications: {
          decision: true,
          input: true,
          error: true,
          completed: true,
          activity: false,
        },
      }),
      d,
    );
    expect(shouldToast(old, 'question')).toBe(true);
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

describe('Voco settings', () => {
  it('defaults: wake word on, undo window, scheduled send after 3 s', () => {
    const d = defaultSettings();
    expect(d.vocoWake).toBe(true);
    expect(d.vocoConfirm).toBe('undo');
    expect(d.vocoAutoSend).toBe(true);
    expect(d.vocoCountdownSec).toBe(3);
  });

  it('parses stored values and clamps the countdown to 2..5 s', () => {
    const d = defaultSettings();
    const s = parseSettings(
      JSON.stringify({ vocoWake: false, vocoConfirm: 'readback', vocoCountdownSec: 9 }),
      d,
    );
    expect(s.vocoWake).toBe(false);
    expect(s.vocoConfirm).toBe('readback');
    expect(s.vocoCountdownSec).toBe(5);
    expect(
      parseSettings(JSON.stringify({ vocoConfirm: 'x', vocoCountdownSec: 1 }), d),
    ).toMatchObject({ vocoConfirm: 'undo', vocoCountdownSec: 2 });
  });

  it('splits user phrases', () => {
    expect(phraseList(' 자비스, hey computer ,,')).toEqual(['자비스', 'hey computer']);
  });
});

describe('removed phone session strip setting', () => {
  it('reads older saved settings with the strip fields and drops them', () => {
    const parsed = parseSettings(
      JSON.stringify({
        theme: 'dark',
        phoneStrip: 'tap',
        phoneStripChosen: true,
        phoneCarousel: true,
      }),
      defaultSettings(),
    );
    expect(parsed.theme).toBe('dark');
    expect(parsed).not.toHaveProperty('phoneStrip');
    expect(parsed).not.toHaveProperty('phoneStripChosen');
    expect(parsed).not.toHaveProperty('phoneCarousel');
    expect(defaultSettings()).not.toHaveProperty('phoneStrip');
  });
});
