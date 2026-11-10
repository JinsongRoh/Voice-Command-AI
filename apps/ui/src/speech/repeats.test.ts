import { describe, expect, it } from 'vitest';
import { collapseRepeats, isRepetitionHallucination } from './repeats';

// The same cases as `cleanup::tests` in crates/vca-speech/src/cleanup.rs.
describe('collapseRepeats', () => {
  it('collapses engine repetition loops', () => {
    expect(collapseRepeats('음음음음음음음음 테스트')).toBe('음 테스트');
    expect(collapseRepeats('음 음 음 음 음 테스트 해줘')).toBe('음 테스트 해줘');
    expect(collapseRepeats('thank you thank you thank you thank you.')).toBe('thank you');
    expect(collapseRepeats('감사합니다 감사합니다 감사합니다 감사합니다')).toBe('감사합니다');
    for (const t of [
      'PNPM 테스트 실행해 줘',
      'a a b',
      'ㅋㅋㅋ 좋아',
      'really really good',
      '1000 users',
    ]) {
      expect(collapseRepeats(t)).toBe(t);
    }
  });

  it('drops a transcript that is one loop', () => {
    const loop120 = 'O.'.repeat(120);
    expect(collapseRepeats(loop120)).toBe('');
    expect(collapseRepeats(`${loop120}O`)).toBe('');
    expect(collapseRepeats('O. '.repeat(50))).toBe('');
    expect(collapseRepeats('아. '.repeat(30))).toBe('');
    expect(collapseRepeats('네 '.repeat(10))).toBe('');
    expect(collapseRepeats('ㅋ'.repeat(40))).toBe('');
    expect(collapseRepeats('.'.repeat(40))).toBe('');
    expect(collapseRepeats('you '.repeat(12))).toBe('');
    expect(collapseRepeats(`네 ${'O.'.repeat(60)}`)).toBe('');
    expect(isRepetitionHallucination(loop120)).toBe(true);
    expect(isRepetitionHallucination('테스트 실행해 줘')).toBe(false);
    expect(isRepetitionHallucination('')).toBe(false);
  });

  it('collapses short-unit loops inside text', () => {
    expect(collapseRepeats('O.O.O.O.O 테스트')).toBe('O 테스트');
    expect(collapseRepeats('O.O.O.O.O. 테스트')).toBe('O. 테스트');
    expect(collapseRepeats('아. 아. 아. 아. 로그인 고쳐줘')).toBe('아. 로그인 고쳐줘');
    expect(collapseRepeats('네, 네, 네, 네, 보내줘')).toBe('네, 보내줘');
    expect(collapseRepeats('ㅋㅋㅋㅋㅋㅋ 좋아')).toBe('ㅋ 좋아');
    expect(collapseRepeats('hahahaha that works')).toBe('ha that works');
    expect(collapseRepeats('음..음..음..음.. 테스트')).toBe('음.. 테스트');
    expect(collapseRepeats('Wait.......... what')).toBe('Wait... what');
    expect(collapseRepeats('aaaaa ok')).toBe('a ok');
    expect(collapseRepeats('로그인 화면의 버그를 고쳐줘 O.O.O.O.O.O')).toBe(
      '로그인 화면의 버그를 고쳐줘 O',
    );
    expect(collapseRepeats('O.o.O.o 끝')).toBe('O 끝');
  });

  it('keeps legitimate text', () => {
    for (const t of [
      'ㅋㅋ',
      'ㅋㅋㅋ',
      '1.2.3',
      '1.1.1.1',
      'DNS 서버를 1.1.1.1로 바꿔 줘',
      'version 1.1.1.1.1 released',
      '1.0.0.0 and 10000 and 2.2.2.2',
      'Wait......',
      '음...... 그래',
      'a.b.c.d',
      'foo.bar.baz.qux()',
      'AAAA 레코드 추가해 줘',
      'add an AAAA record',
      'https://aaaa.example.com/a/a/a/a',
      'www.ooooo.com',
      'send it to bbbbb@example.com',
      'src/a/a/a/a/mod.rs',
      'Mississippi',
      '1111 and 0000',
      'x = a - b - c',
      '네 보내줘',
      '네 네',
      '',
    ]) {
      expect(collapseRepeats(t)).toBe(t);
    }
  });
});
