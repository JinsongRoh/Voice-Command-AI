import { describe, expect, it } from 'vitest';
import {
  isLatinWake,
  matchSendPhrase,
  matchWakeWord,
  normalizeRadioWord,
  parseRadioUtterance,
} from './wakeWord';

describe('matchWakeWord', () => {
  it('accepts the wake word and its common mishearings at the start', () => {
    const cases: [string, string, string][] = [
      ['보코, 테스트 돌려 줘', '보코', 'ko'],
      ['보꼬 테스트 돌려 줘', '보꼬', 'ko'],
      ['버코 테스트 돌려 줘', '버코', 'ko'],
      ['뽀코 테스트 돌려 줘', '뽀코', 'ko'],
      ['보코야 테스트 돌려 줘', '보코야', 'ko'],
      ['Voco, run the tests', 'Voco', 'en'],
      ['VOCO run the tests', 'VOCO', 'en'],
      ['Boko run the tests', 'Boko', 'en'],
      ['Vocco, run the tests', 'Vocco', 'en'],
      ['Voko run the tests', 'Voko', 'en'],
      ['Vo co, run the tests', 'Vo co', 'en'],
      ['ボコ、テストを実行して', 'ボコ', 'ja'],
      ['ヴォコ、テストを実行して', 'ヴォコ', 'ja'],
      ['ボコ　テストを実行して', 'ボコ', 'ja'],
      ['ボコ、実行して', 'ボコ', 'ja'],
      ['ボコは実行して', 'ボコ', 'ja'],
      ['voco, ejecuta las pruebas', 'voco', 'en'],
      ['Boco, ejecuta las pruebas', 'Boco', 'es'],
      ['Vóco, ejecuta las pruebas', 'Vóco', 'en'],
    ];
    for (const [text, heard, lang] of cases) {
      const m = matchWakeWord(text);
      expect(m, text).not.toBeNull();
      expect(m!.heard, text).toBe(heard);
      expect(m!.lang, text).toBe(lang);
    }
  });

  it('returns the rest of the utterance', () => {
    expect(matchWakeWord('보코, 테스트 돌려 줘')!.rest).toBe('테스트 돌려 줘');
    expect(matchWakeWord('Voco... run it')!.rest).toBe('run it');
    expect(matchWakeWord('ボコ、テストを実行して')!.rest).toBe('テストを実行して');
    expect(matchWakeWord('보코')!.rest).toBe('');
  });

  it('allows at most two fillers or greetings before the wake word', () => {
    expect(matchWakeWord('음 보코 테스트 돌려 줘')?.heard).toBe('보코');
    expect(matchWakeWord('어 저기 보코 테스트 돌려 줘')?.heard).toBe('보코');
    expect(matchWakeWord('hey Voco, run the tests')?.heard).toBe('Voco');
    expect(matchWakeWord('oye, voco, ejecuta')?.heard).toBe('voco');
    expect(matchWakeWord('ねえ、ボコ、実行して')?.heard).toBe('ボコ');
    // Three words before it, or a real word before it: not a wake call.
    expect(matchWakeWord('음 어 저기 보코 테스트')).toBeNull();
    expect(matchWakeWord('이 보코 기능 고쳐 줘')).toBeNull();
    expect(matchWakeWord('fix the Voco build')).toBeNull();
  });

  it('accepts "보고" only alone or before a pause', () => {
    expect(matchWakeWord('보고')?.heard).toBe('보고');
    expect(matchWakeWord('보고.')?.heard).toBe('보고');
    expect(matchWakeWord('보고, 테스트 돌려 줘')?.rest).toBe('테스트 돌려 줘');
    expect(matchWakeWord('보고 싶어')).toBeNull();
    expect(matchWakeWord('보고서 작성해')).toBeNull();
    expect(matchWakeWord('보고 오버')).toBeNull();
  });

  it('rejects ordinary words', () => {
    for (const text of [
      '보고서 작성해',
      '보호 기능 추가해 줘',
      '보코가 응답이 없어',
      '보코의 기능을 만들어 줘 오버',
      '보코를 다시 시작해',
      '다시 보고 오버',
      'vocal track',
      'Voto en contra',
      'poco a poco',
      'loco',
      'ボコボコにする',
      'ボロボロ',
      'Bob, run it',
      '',
      '...',
    ]) {
      expect(matchWakeWord(text), text).toBeNull();
    }
  });

  it('decides that a bare wake word followed by a command wakes', () => {
    // Documented decision: the wake-then-command syntax cannot be told apart
    // from talking about the app, so a bare "보코" at the start wakes.
    const m = matchWakeWord('보코 기능을 만들어 줘 오버');
    expect(m?.heard).toBe('보코');
    expect(m?.rest).toBe('기능을 만들어 줘 오버');
  });
});

describe('matchSendPhrase', () => {
  it('accepts the send phrases at the end', () => {
    const cases: [string, string, string, string][] = [
      ['테스트 돌려 줘 보내줘 오버', '보내줘 오버', 'ko', '테스트 돌려 줘'],
      ['테스트 돌려 줘, 보내 줘 오버.', '보내 줘 오버', 'ko', '테스트 돌려 줘'],
      ['테스트 돌려 줘 보내줘오버', '보내줘오버', 'ko', '테스트 돌려 줘'],
      ['테스트 돌려 줘, 오버', '오버', 'ko', '테스트 돌려 줘'],
      ['테스트 돌려 줘 오버', '오버', 'ko', '테스트 돌려 줘'],
      ['다시 보고 오버', '오버', 'ko', '다시 보고'],
      ['Run the tests, over.', 'over', 'en', 'Run the tests'],
      ['Run the tests. Over!', 'Over', 'en', 'Run the tests'],
      ['Run the tests and send it over', 'send it over', 'en', 'Run the tests and'],
      ['テストを実行して、送信、オーバー。', '送信、オーバー', 'ja', 'テストを実行して'],
      ['テストを実行して送信オーバー', '送信オーバー', 'ja', 'テストを実行して'],
      ['テストを実行して、オーバー', 'オーバー', 'ja', 'テストを実行して'],
      ['Ejecuta las pruebas, cambio.', 'cambio', 'es', 'Ejecuta las pruebas'],
      ['Ejecuta las pruebas cambio', 'cambio', 'es', 'Ejecuta las pruebas'],
      ['오버', '오버', 'ko', ''],
      ['Over.', 'Over', 'en', ''],
    ];
    for (const [text, heard, lang, before] of cases) {
      const m = matchSendPhrase(text);
      expect(m, text).not.toBeNull();
      expect(m!.heard, text).toBe(heard);
      expect(m!.lang, text).toBe(lang);
      expect(m!.before, text).toBe(before);
    }
  });

  it('rejects ordinary phrases that end with the send word', () => {
    for (const text of [
      'game over',
      'Let us start over',
      'turn it over',
      'it is over',
      'over and over',
      'The meeting is over',
      '게임 오버',
      '그건 너무 오버',
      '時間オーバー',
      'ゲームオーバー',
      '予算オーバー',
      'Hubo un cambio',
      'sin cambio',
      'overflow',
      '오버해서 보내 줘',
      'over the top',
      '',
    ]) {
      expect(matchSendPhrase(text), text).toBeNull();
    }
  });
});

describe('parseRadioUtterance', () => {
  it('splits wake word, command and send phrase', () => {
    const p = parseRadioUtterance('보코, 테스트 돌려 줘, 보내줘 오버');
    expect(p.wake?.heard).toBe('보코');
    expect(p.send?.heard).toBe('보내줘 오버');
    expect(p.command).toBe('테스트 돌려 줘');
    const en = parseRadioUtterance('Hey Voco, run the tests. Over.');
    expect([en.wake?.heard, en.command, en.send?.heard]).toEqual(['Voco', 'run the tests', 'Over']);
    const ja = parseRadioUtterance('ボコ、テストを実行して、送信、オーバー');
    expect([ja.wake?.heard, ja.command, ja.send?.heard]).toEqual([
      'ボコ',
      'テストを実行して',
      '送信、オーバー',
    ]);
  });

  it('keeps the parts that are missing null', () => {
    const noWake = parseRadioUtterance('다시 보고 오버');
    expect(noWake.wake).toBeNull();
    expect(noWake.send?.heard).toBe('오버');
    expect(noWake.command).toBe('다시 보고');
    const noSend = parseRadioUtterance('보코 테스트 돌려 줘');
    expect(noSend.send).toBeNull();
    expect(noSend.command).toBe('테스트 돌려 줘');
    const plain = parseRadioUtterance('보고서 작성해');
    expect(plain).toEqual({ wake: null, send: null, command: '보고서 작성해' });
    const both = parseRadioUtterance('보코 오버');
    expect([both.wake?.heard, both.send?.heard, both.command]).toEqual(['보코', '오버', '']);
  });
});

describe('helpers', () => {
  it('normalizes words', () => {
    expect(normalizeRadioWord('VÓCO')).toBe('voco');
    expect(normalizeRadioWord('ｖｏｃｏ')).toBe('voco');
    expect(normalizeRadioWord('ヴォコ')).toBe('ヴォコ');
    expect(normalizeRadioWord('It’s')).toBe("it's");
  });

  it('matches Latin hearings of "voco" by edit distance', () => {
    for (const w of ['voco', 'vocco', 'voko', 'vico', 'voc', 'vocos', 'boko', 'boco']) {
      expect(isLatinWake(w), w).toBe(true);
    }
    for (const w of ['voto', 'voce', 'vocal', 'bob', 'poco', 'loco', 'vo', 'video']) {
      expect(isLatinWake(w), w).toBe(false);
    }
  });
});

describe('Voco v2 wake aliases', () => {
  it('accepts the common mishearings of "보코" when isolated', () => {
    for (const text of [
      '보컬',
      '보컬.',
      '보컬, 로그인 버그 고쳐줘',
      '보컬아',
      '보코야',
      '보꼬',
      '뽀코',
      '보고.',
    ]) {
      expect(matchWakeWord(text), text).not.toBeNull();
    }
    for (const text of ['Vocal.', 'Vocal', 'Voco', 'Boko, fix it']) {
      expect(matchWakeWord(text), text).not.toBeNull();
    }
  });

  it('accepts 복호, a hearing of "보코" in noise, only when it stands alone', () => {
    for (const text of ['복호', '복호.', '복호, 테스트 돌려 줘', '음, 복호.']) {
      expect(matchWakeWord(text), text).not.toBeNull();
    }
    expect(matchWakeWord('복호, 테스트 돌려 줘')!.rest).toBe('테스트 돌려 줘');
    for (const text of [
      '복호 알고리즘 설명해 줘',
      '복호화 해 줘',
      '복호가 안 돼',
      '암호화와 복호',
      '이 파일 복호해 줘',
    ]) {
      expect(matchWakeWord(text), text).toBeNull();
    }
  });

  it('does not wake on the real words 보컬 / vocal inside speech', () => {
    for (const text of [
      '보컬 트레이닝 앱 만들어',
      '앱에 보컬 기능 추가해',
      '보컬이 너무 작아',
      'vocal training app please',
      'the vocal track is too loud',
    ]) {
      expect(matchWakeWord(text), text).toBeNull();
    }
  });

  it('accepts user wake words only when isolated', () => {
    const extra = ['자비스', 'hey computer'];
    expect(matchWakeWord('자비스', { extra })?.heard).toBe('자비스');
    expect(matchWakeWord('자비스, 테스트 돌려', { extra })?.rest).toBe('테스트 돌려');
    expect(matchWakeWord('Hey computer, run it', { extra })?.heard).toBe('Hey computer');
    expect(matchWakeWord('자비스 같은 앱 만들어', { extra })).toBeNull();
  });
});

describe('plain send commands after a pause', () => {
  it('end the message after a pause mark, alone, or after a pause in the audio', () => {
    const cases: [string, string][] = [
      ['로그인 버그 고쳐줘, 전송해', '로그인 버그 고쳐줘'],
      ['로그인 버그 고쳐줘. 보내줘', '로그인 버그 고쳐줘'],
      ['로그인 버그 고쳐줘, 전송해 줘', '로그인 버그 고쳐줘'],
      ['fix the login bug. Send it', 'fix the login bug'],
      ['ログインを直して、送って', 'ログインを直して'],
      ['ログインを直して、送信して', 'ログインを直して'],
      ['arregla el login, envíalo', 'arregla el login'],
      ['arregla el login, envía', 'arregla el login'],
      ['전송해', ''],
      ['보내줘', ''],
    ];
    for (const [text, before] of cases) {
      const m = matchSendPhrase(text);
      expect(m, text).not.toBeNull();
      expect(m!.before, text).toBe(before);
    }
    expect(matchSendPhrase('파일 전송해', { pausedBefore: true })?.before).toBe('파일');
  });

  it('mark a command after other words as trailing (it needs more silence), not one said alone', () => {
    expect(matchSendPhrase('로그인 버그 고쳐줘, 전송해')?.trailing).toBe(true);
    expect(matchSendPhrase('파일 전송해', { pausedBefore: true })?.trailing).toBe(true);
    expect(matchSendPhrase('전송해')?.trailing).toBe(false);
    expect(matchSendPhrase('보내')?.trailing).toBe(false);
    expect(matchSendPhrase('로그인 버그 고쳐줘 보내줘 오버')?.trailing).toBe(false);
    expect(matchSendPhrase('로그인 버그 고쳐줘 오버')?.trailing).toBe(false);
  });

  it('take bare stems ("보내", "전송", "send") only on their own: after words they start a phrase', () => {
    for (const text of [
      '로그인 버그 고쳐줘, 보내',
      '테스트 돌려, 전송',
      'fix the login bug, send',
      // A thinking pause cut "해당 내용을 전송하고 …" / "보내서 …" here.
      '보이스 커맨더 AI한테 해당 내용을 전송',
      '보이스 커맨더 AI한테 해당 내용을 보내',
    ]) {
      expect(matchSendPhrase(text, { pausedBefore: true }), text).toBeNull();
    }
    expect(matchSendPhrase('보내')?.before).toBe('');
    expect(matchSendPhrase('전송')?.before).toBe('');
    expect(matchSendPhrase('send')?.before).toBe('');
  });

  it('are content mid-sentence or without a pause', () => {
    for (const text of [
      '보내는 기능 만들어',
      '전송 속도 개선해',
      '파일 전송해',
      '메일 보내줘',
      '결과를 보내 줘',
      'send the report to the team',
      'please send it',
      'ファイルを送って',
    ]) {
      expect(matchSendPhrase(text), text).toBeNull();
    }
  });

  it('keep the older phrases that need no pause', () => {
    expect(matchSendPhrase('로그인 버그 고쳐줘 보내줘 오버')?.before).toBe('로그인 버그 고쳐줘');
    expect(matchSendPhrase('로그인 버그 고쳐줘 오버')?.before).toBe('로그인 버그 고쳐줘');
  });

  it('accept user send phrases after a pause only', () => {
    const extra = ['날려'];
    expect(matchSendPhrase('테스트 돌려, 날려', { extra })?.before).toBe('테스트 돌려');
    expect(matchSendPhrase('테스트 돌려 날려', { extra })).toBeNull();
  });
});
