import { describe, expect, it } from 'vitest';

import { translate, type Locale } from '../i18n/i18n';
import { normalizeKoreanNumbers } from '../speech/koreanNumbers';
import { completionLine, errorLine, priorityOf, spokenTopic } from './spokenAlerts';

const tr =
  (lang: Locale) => (k: Parameters<typeof translate>[1], v?: Record<string, string | number>) =>
    translate(lang, k, v);

describe('spoken alert texts', () => {
  it('say what finished, in four languages', () => {
    const topic = spokenTopic({ auto_name: '로그인 버그 수정', display_name: 'x' });
    expect(normalizeKoreanNumbers(completionLine(tr('ko'), 3, topic), 'ko')).toBe(
      '삼번 세션, 로그인 버그 수정 작업이 끝났습니다.',
    );
    expect(completionLine(tr('en'), 3, 'Fix login bug')).toBe(
      'Session 3, Fix login bug, finished.',
    );
    expect(completionLine(tr('ja'), 3, 'ログイン修正')).toBe(
      '3番のセッション、ログイン修正の作業が終わりました。',
    );
    expect(completionLine(tr('es'), 3, 'Fix login bug')).toBe(
      'La sesión 3, Fix login bug, terminó.',
    );
    // Without a safe topic: the number alone.
    expect(normalizeKoreanNumbers(completionLine(tr('ko'), 3, null), 'ko')).toBe(
      '삼번 세션 작업이 끝났습니다.',
    );
    expect(completionLine(tr('en'), null, null)).toBe('A session finished its work.');
  });

  it('never read commands, paths or code as a topic', () => {
    for (const name of [
      'payments-api · Claude Code',
      'run pnpm build',
      'fix src/auth/login.ts',
      'C:\\work\\app',
      'git push --force',
      'echo $TOKEN',
      'see https://example.com',
      'update config.toml',
      'a'.repeat(41),
    ]) {
      expect(spokenTopic({ auto_name: name, display_name: name }), name).toBeNull();
    }
    expect(spokenTopic({ auto_name: null, display_name: 'Checkout fix' })).toBe('Checkout fix');
  });

  it('errors get a short generic reason, never the message', () => {
    const state = (message: string) => ({ type: 'error' as const, message });
    expect(
      normalizeKoreanNumbers(
        errorLine(tr('ko'), 5, state('EACCES: permission denied, open /etc/x')),
        'ko',
      ),
    ).toBe('오번 세션에서 오류가 발생했습니다: 권한 문제.');
    expect(errorLine(tr('en'), 5, state('request timed out after 30s'))).toBe(
      'Session 5 hit an error: it timed out.',
    );
    expect(errorLine(tr('ja'), 2, { type: 'exited', code: 1 })).toBe(
      '2番のセッションでエラーが発生しました：終了コード1。',
    );
    expect(errorLine(tr('es'), 1, state('weird /secret/path thing'))).toBe(
      'La sesión 1 tuvo un error.',
    );
    expect(errorLine(tr('en'), null, state('ECONNREFUSED'))).toBe(
      'A session hit an error: a connection problem.',
    );
  });

  it('rank decisions first, then errors, then completions', () => {
    expect(priorityOf('decision')).toBe('decision');
    expect(priorityOf('input')).toBe('decision');
    expect(priorityOf('error')).toBe('error');
    expect(priorityOf('completed')).toBe('completion');
  });
});
