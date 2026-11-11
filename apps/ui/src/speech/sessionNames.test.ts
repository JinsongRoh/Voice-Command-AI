import { afterEach, describe, expect, it } from 'vitest';

import { completionLine, errorLine } from '../state/spokenAlerts';
import { translate } from '../i18n/i18n';
import { setPronunciations } from './pronunciation';
import {
  matchSessionName,
  sessionLabel,
  shortSessionName,
  spokenSessionName,
} from './sessionNames';
import { VOCO_PHRASES } from './vocoFlow';

const s = (id: string, display_name: string) => ({ id, display_name });

afterEach(() => setPronunciations([]));

describe('sessions in spoken phrases', () => {
  it('uses the name, shortened when long, and the number only for twins', () => {
    const sessions = {
      a: s('a', 'Orbit Platform'),
      b: s('b', 'Voice Command AI'),
      c: s('c', 'Voice Command AI'),
      d: s('d', 'payments-api · claude'),
      e: s('e', 'A very long session name that keeps going and going'),
    };
    const order = ['a', 'b', 'c', 'd', 'e'];
    expect(spokenSessionName(sessions.a, sessions, order, 'ko')).toBe('Orbit Platform');
    expect(spokenSessionName(sessions.b, sessions, order, 'ko')).toBe('Voice Command AI 2번');
    expect(spokenSessionName(sessions.c, sessions, order, 'en')).toBe('Voice Command AI number 3');
    expect(spokenSessionName(sessions.d, sessions, order, 'ko')).toBe('payments-api');
    expect(shortSessionName(sessions.e.display_name)).toBe('A very long session name');
    expect(
      Array.from(shortSessionName('가나다라마바사아자차카타파하가나다라마바사아자차카타')).length,
    ).toBe(24);
  });

  it('says the send confirmation with the name', () => {
    const ko = sessionLabel('Orbit 플랫폼', 'ko');
    expect(VOCO_PHRASES.ko.rogerSent(ko)).toBe('로저, Orbit 플랫폼 세션에 전송했습니다');
    expect(VOCO_PHRASES.en.rogerSent(sessionLabel('Orbit Platform', 'en'))).toBe(
      'Roger, sent to Orbit Platform.',
    );
    expect(VOCO_PHRASES.ko.rogerSwitch(sessionLabel('오빗 플랫폼', 'ko'))).toBe(
      '로저, 오빗 플랫폼 세션',
    );
    expect(sessionLabel('빌드 세션', 'ko')).toBe('빌드 세션');
  });

  it('names sessions in alerts', () => {
    const t = (k: Parameters<typeof translate>[1], v?: Parameters<typeof translate>[2]) =>
      translate('ko', k, v);
    expect(completionLine(t, 4, '로그인 버그 수정', 'Orbit Platform')).toBe(
      'Orbit Platform 세션, 로그인 버그 수정 작업이 끝났습니다.',
    );
    expect(completionLine(t, 4, 'Orbit Platform', 'Orbit Platform')).toBe(
      'Orbit Platform 세션 작업이 끝났습니다.',
    );
    expect(errorLine(t, 4, { type: 'error', message: 'permission denied' }, 'Orbit Platform')).toBe(
      'Orbit Platform 세션에서 오류가 발생했습니다: 권한 문제.',
    );
    // Without a name the number still works.
    expect(completionLine(t, 4, null)).toBe('4번 세션 작업이 끝났습니다.');
  });
});

describe('matchSessionName', () => {
  const list = [
    s('a', 'Orbit Platform'),
    s('b', 'Voice Command AI'),
    s('c', 'NoVa AI'),
    s('d', '결제 서버'),
    s('e', '결제 서버 2'),
  ];

  it('finds a session by its name, its reading or a close hearing', () => {
    setPronunciations([
      { term: 'Orbit', spoken: '오빗', language: 'ko' },
      { term: 'Platform', spoken: '플랫폼', language: 'ko' },
      { term: 'NoVa', spoken: '노바', language: 'ko' },
    ] as never);
    expect(matchSessionName('Orbit Platform', list)).toMatchObject({ kind: 'one', id: 'a' });
    expect(matchSessionName('오빗 플랫폼', list)).toMatchObject({ kind: 'one', id: 'a' });
    expect(matchSessionName('오비트 플랫폼 세션으로', list)).toMatchObject({
      kind: 'one',
      id: 'a',
    });
    expect(matchSessionName('노바 AI 세션', list)).toMatchObject({ kind: 'one', id: 'c' });
    expect(matchSessionName('voice command ai', list)).toMatchObject({ kind: 'one', id: 'b' });
  });

  it('is ambiguous when a name fits two sessions, and null for other words', () => {
    expect(matchSessionName('결제 서버', list)).toMatchObject({ kind: 'one', id: 'd' });
    expect(matchSessionName('결제', list)).toEqual({ kind: 'ambiguous', ids: ['d', 'e'] });
    expect(matchSessionName('로그인 버그 고쳐줘', list)).toBeNull();
    expect(matchSessionName('테스트 돌려 줘', list)).toBeNull();
  });
});
