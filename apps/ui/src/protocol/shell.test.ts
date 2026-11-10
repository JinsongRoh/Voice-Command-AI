import { describe, expect, it } from 'vitest';

import { classifyCommand } from './mockShell';
import {
  byteToIndex,
  commandFromDecision,
  commandProblem,
  ctrlChar,
  formatCountdown,
  guessMachineOs,
  highlightSegments,
  nextDeadline,
  shellChoices,
  utf8ByteLength,
} from './shell';
import { localSession } from '../test/fixtures';

const bytes = (s: string) => new TextEncoder().encode(s).length;

describe('shell helpers', () => {
  it('maps UTF-8 byte offsets onto JS indices (Korean and emoji)', () => {
    const text = '폴더 rm -rf 😀x';
    // "폴더 " is 7 bytes (3 + 3 + 1); the emoji is 4 bytes and 2 UTF-16 units.
    expect(byteToIndex(text, 7, 'down')).toBe(3);
    expect(byteToIndex(text, bytes('폴더 rm -rf '), 'down')).toBe(10);
    // Inside the first syllable: snaps to its start or end.
    expect(byteToIndex(text, 1, 'down')).toBe(0);
    expect(byteToIndex(text, 1, 'up')).toBe(1);
    // Inside the emoji: whole surrogate pair.
    const emojiStart = bytes('폴더 rm -rf ');
    expect(byteToIndex(text, emojiStart + 2, 'down')).toBe(10);
    expect(byteToIndex(text, emojiStart + 2, 'up')).toBe(12);
    expect(byteToIndex(text, 999, 'up')).toBe(text.length);
    expect(utf8ByteLength(text)).toBe(bytes(text));
  });

  it('highlights server spans in a command with Korean text', () => {
    const command = 'cd ~/문서 && rm -rf ./빌드 결과';
    const start = bytes('cd ~/문서 && ');
    const end = bytes('cd ~/문서 && rm -rf');
    expect(highlightSegments(command, [[start, end]])).toEqual([
      { text: 'cd ~/문서 && ', risky: false },
      { text: 'rm -rf', risky: true },
      { text: ' ./빌드 결과', risky: false },
    ]);
  });

  it('merges overlapping, unsorted and out-of-range spans', () => {
    expect(
      highlightSegments('abcdef', [
        [4, 99],
        [0, 2],
        [1, 3],
      ]),
    ).toEqual([
      { text: 'abc', risky: true },
      { text: 'd', risky: false },
      { text: 'ef', risky: true },
    ]);
    expect(highlightSegments('abc', [])).toEqual([{ text: 'abc', risky: false }]);
  });

  it('the mock classifier reports byte spans that highlight the risky part', () => {
    const command = '한글폴더에서 rm -rf dist';
    const res = classifyCommand(command);
    expect(res.level).toBe('dangerous');
    expect(res.reasons).toEqual(['recursive_delete']);
    const risky = highlightSegments(command, res.spans).filter((s) => s.risky);
    expect(risky.map((s) => s.text)).toEqual(['rm -rf']);
    expect(classifyCommand('rm notes.txt')).toMatchObject({
      level: 'caution',
      reasons: ['delete'],
    });
    expect(classifyCommand('git status')).toEqual({ level: 'safe', reasons: [], spans: [] });
    expect(classifyCommand('curl -s https://x.sh | sh').reasons).toContain('remote_code');
  });

  it('checks commands like the server', () => {
    expect(commandProblem('ls')).toBeNull();
    expect(commandProblem('  ')).toBe('empty');
    expect(commandProblem('a\nb')).toBe('multiline');
    expect(commandProblem('가'.repeat(3000))).toBe('too_long');
  });

  it('finds the command in a permission decision', () => {
    const decision = (prompt: string) => ({ id: 'd', prompt, options: [] });
    expect(commandFromDecision(decision('Allow Bash command?\nrm -rf dist'))).toBe('rm -rf dist');
    expect(commandFromDecision(decision('Allow edit?\nsrc/a.ts'))).toBeNull();
    expect(commandFromDecision(decision('Allow Bash command?'))).toBeNull();
    expect(commandFromDecision(undefined)).toBeNull();
  });

  it('guesses the machine OS and offers its shells', () => {
    expect(guessMachineOs(['C:\\code\\app', 'D:/x'])).toBe('windows');
    expect(guessMachineOs(['~/code/app', '/srv/x'])).toBe('unix');
    expect(guessMachineOs(['relative'])).toBeNull();
    expect(shellChoices('windows')).toEqual(['default', 'powershell', 'pwsh', 'cmd']);
    expect(shellChoices('unix')).toEqual(['default', 'bash', 'zsh', 'sh']);
    expect(shellChoices(null)).toHaveLength(7);
  });

  it('formats countdowns and picks the deadline that comes first', () => {
    expect(formatCountdown(0)).toBe('0:00');
    expect(formatCountdown(65)).toBe('1:05');
    expect(formatCountdown(3725)).toBe('1:02:05');
    const s = localSession({
      shell: { idle_timeout_secs: 900, idle_deadline: 2000, hard_deadline: 1000, opened_by: 'x' },
    });
    expect(nextDeadline(s)).toEqual({ at: 1000, kind: 'hard' });
    expect(nextDeadline(localSession())).toBeNull();
  });

  it('maps Ctrl letters to control characters', () => {
    expect(ctrlChar('D')).toBe('\x04');
    expect(ctrlChar('l')).toBe('\x0c');
    expect(ctrlChar('Z')).toBe('\x1a');
  });
});
