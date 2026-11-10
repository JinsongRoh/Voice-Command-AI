import { describe, expect, it } from 'vitest';

import { plainSpeech } from './plainSpeech';

describe('plainSpeech', () => {
  it('reads list items and emphasis without the marks', () => {
    const md = [
      '## 진행 상황',
      '',
      '- **Nimbus** 세션: 빌드 완료',
      '* *Orbit* 세션 - 테스트 중',
      '1. 다음 단계',
    ].join('\n');
    expect(plainSpeech(md)).toBe(
      '진행 상황. Nimbus 세션: 빌드 완료. Orbit 세션, 테스트 중. 1. 다음 단계.',
    );
  });

  it('drops code blocks, link targets, URLs and table bars', () => {
    const md = [
      '결과는 [보고서](https://example.com/r) 참고',
      '```bash',
      'rm -rf build',
      '```',
      '| 기기 | 상태 |',
      '|---|---|',
      '| 아이폰 | 설치됨 |',
      '---',
      '`vca hooks install` 실행 → 완료',
    ].join('\n');
    expect(plainSpeech(md)).toBe(
      '결과는 보고서 참고. 기기, 상태. 아이폰, 설치됨. vca hooks install 실행, 완료.',
    );
  });

  it('keeps hyphenated words and plain sentences', () => {
    expect(plainSpeech('web-dashboard 작업이 끝났습니다.')).toBe(
      'web-dashboard 작업이 끝났습니다.',
    );
  });
});
