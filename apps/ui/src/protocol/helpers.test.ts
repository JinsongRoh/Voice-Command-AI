import { describe, expect, it } from 'vitest';

import { agentLabel, folderName, needsAttention, originMonogram, stripAnsi } from './helpers';

describe('helpers', () => {
  it('needsAttention mirrors vca-core', () => {
    expect(needsAttention({ type: 'awaiting_input' })).toBe(true);
    expect(needsAttention({ type: 'awaiting_decision' })).toBe(true);
    expect(needsAttention({ type: 'error', message: 'x' })).toBe(true);
    expect(needsAttention({ type: 'running' })).toBe(false);
    expect(needsAttention({ type: 'exited', code: 1 })).toBe(false);
  });

  it('labels agents including generic ones', () => {
    expect(agentLabel({ type: 'gemini_cli' })).toBe('Gemini CLI');
    expect(agentLabel({ type: 'generic', name: 'mycli' })).toBe('mycli');
  });

  it('builds monograms', () => {
    expect(
      originMonogram({
        type: 'local',
        agent: { type: 'claude_code' },
        project_dir: '/',
        pid: null,
      }),
    ).toBe('CC');
    expect(originMonogram({ type: 'cloud', provider: 'jules', remote_id: 'x', url: null })).toBe(
      'JU',
    );
  });

  it('extracts folder names on any OS', () => {
    expect(folderName('/home/a/proj')).toBe('proj');
    expect(folderName('C:\\Users\\a\\proj\\')).toBe('proj');
  });

  it('strips ANSI sequences', () => {
    expect(stripAnsi('\x1b[32m✓\x1b[0m ok\x07')).toBe('✓ ok');
  });
});
