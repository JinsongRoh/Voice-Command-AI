import { afterEach, describe, expect, it, vi } from 'vitest';

import { copyText, splitFences } from './clipboard';

describe('splitFences', () => {
  it('returns plain text unchanged', () => {
    expect(splitFences('hello\nworld')).toEqual([{ type: 'text', text: 'hello\nworld' }]);
  });

  it('separates fenced code blocks from prose', () => {
    expect(splitFences('Run this:\n```bash\ncargo test\n```\nThen check.')).toEqual([
      { type: 'text', text: 'Run this:' },
      { type: 'code', lang: 'bash', code: 'cargo test' },
      { type: 'text', text: 'Then check.' },
    ]);
  });

  it('keeps an unterminated fence as code (streaming replies)', () => {
    expect(splitFences('```\nline 1\nline 2')).toEqual([
      { type: 'code', lang: '', code: 'line 1\nline 2' },
    ]);
  });

  it('does not close a long fence with a shorter one', () => {
    const parts = splitFences('````md\n```js\nx\n```\n````');
    expect(parts).toEqual([{ type: 'code', lang: 'md', code: '```js\nx\n```' }]);
  });
});

describe('copyText', () => {
  afterEach(() => {
    vi.restoreAllMocks();
    Reflect.deleteProperty(navigator, 'clipboard');
  });

  it('uses the Clipboard API when available', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    await expect(copyText('hi')).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith('hi');
  });

  it('falls back to execCommand when the API is denied', async () => {
    const writeText = vi.fn().mockRejectedValue(new Error('denied'));
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const exec = vi.fn().mockReturnValue(true);
    Object.defineProperty(document, 'execCommand', { value: exec, configurable: true });
    await expect(copyText('hi')).resolves.toBe(true);
    expect(exec).toHaveBeenCalledWith('copy');
    expect(document.querySelector('textarea')).toBeNull();
  });
});
