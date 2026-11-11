import { describe, expect, it, vi } from 'vitest';

import { translate } from '../i18n/i18n';
import type { ClientMsg, ServerMsg } from '../protocol/types';
import { defaultSettings } from './settings';
import { shouldSummarize, speakResultSummary, turnEnded } from './resultSummary';
import { completionLine } from './spokenAlerts';

const running = { type: 'running' } as const;
const idle = { type: 'idle' } as const;
const input = { type: 'awaiting_input' } as const;
const decision = { type: 'awaiting_decision' } as const;

describe('result summaries', () => {
  it('are off by default and follow only finished turns', () => {
    const now = new Date(2026, 0, 1, 12, 0);
    expect(defaultSettings().resultSummary).toBe(false);
    expect(defaultSettings().resultSummaryDetails).toBe(false);
    expect(shouldSummarize(defaultSettings(), running, idle, now)).toBe(false);
    const on = { ...defaultSettings(), spokenAlerts: true, resultSummary: true };
    expect(shouldSummarize(on, running, idle, now)).toBe(true);
    expect(shouldSummarize(on, running, input, now)).toBe(true);
    // Spoken alerts off silences them too.
    expect(shouldSummarize({ ...on, spokenAlerts: false }, running, idle, now)).toBe(false);
    expect(turnEnded(running, decision)).toBe(false);
    expect(turnEnded(idle, idle)).toBe(false);
    expect(turnEnded(input, idle)).toBe(false);
    // Quiet hours silence them like other spoken alerts.
    const quiet = { ...on, quietHours: { enabled: true, start: '11:00', end: '13:00' } };
    expect(shouldSummarize(quiet, running, idle, now)).toBe(false);
  });

  it('speak the summary of the PC with Korean numbers read as Sino-Korean', async () => {
    const speak = vi.fn();
    const request = vi.fn(async (body: ClientMsg): Promise<ServerMsg> => ({
      type: 'summary',
      session_id: body.type === 'summarize' ? body.session_id : '',
      text: '3번 세션: 빌드를 고쳤습니다.',
    }));
    const spoken = await speakResultSummary({ request, speak, language: 'ko' }, 's1', 3, 'unused');
    expect(request).toHaveBeenCalledWith({
      type: 'summarize',
      session_id: 's1',
      language: 'ko',
      number: 3,
    });
    expect(spoken).toBe('삼번 세션: 빌드를 고쳤습니다.');
    expect(speak).toHaveBeenCalledWith('삼번 세션: 빌드를 고쳤습니다.');
  });

  it('say what finished when the PC gives no LLM summary', async () => {
    const speak = vi.fn();
    const ko = (k: Parameters<typeof translate>[1], v?: Record<string, string | number>) =>
      translate('ko', k, v);
    const failing = vi.fn(async () => {
      throw new Error('old server');
    });
    await speakResultSummary(
      { request: failing, speak, language: 'ko' },
      's1',
      3,
      completionLine(ko, 3, '로그인 버그 수정'),
    );
    expect(speak).toHaveBeenLastCalledWith('삼번 세션, 로그인 버그 수정 작업이 끝났습니다.');
    // The PC's own fixed line is replaced by the richer local one.
    const fixed = vi.fn(async (): Promise<ServerMsg> => ({
      type: 'summary',
      session_id: 's1',
      text: 'Session 5 finished its work.',
      fallback: true,
    }));
    await speakResultSummary(
      { request: fixed, speak, language: 'ko' },
      's1',
      5,
      completionLine(ko, 5, null),
    );
    expect(speak).toHaveBeenLastCalledWith('오번 세션 작업이 끝났습니다.');
  });
});
