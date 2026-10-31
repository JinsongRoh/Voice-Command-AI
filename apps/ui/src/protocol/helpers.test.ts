import { describe, expect, it } from 'vitest';

import {
  agentLabel,
  executableName,
  folderName,
  hasChatTranscript,
  needsAttention,
  originMonogram,
  originTitle,
  stripAnsi,
} from './helpers';
import type { SessionOrigin } from './types';

const generic = (name: string): SessionOrigin => ({
  type: 'local',
  agent: { type: 'generic', name },
  project_dir: '/',
  pid: null,
});

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

  it('names generic agents launched by path after the executable', () => {
    expect(executableName('/Users/x/dev/voice-command-ai/target/debug/vca-fake-agent')).toBe(
      'vca-fake-agent',
    );
    expect(executableName('E:\\dev\\target\\debug\\vca-fake-agent.exe')).toBe('vca-fake-agent');
    expect(executableName('E:/dev/tool.CMD')).toBe('tool');
    expect(executableName('mycli')).toBe('mycli');
    const path = '/Users/x/dev/voice-command-ai/target/debug/vca-fake-agent';
    expect(agentLabel({ type: 'generic', name: path })).toBe('vca-fake-agent');
    expect(originTitle(generic(path))).toBe(path);
  });

  it('builds monograms for generic agents from the executable basename', () => {
    expect(originMonogram(generic('/Users/x/target/debug/vca-fake-agent'))).toBe('VF');
    expect(originMonogram(generic('E:/x/target/debug/vca-fake-agent.exe'))).toBe('VF');
    expect(originMonogram(generic('C:\\tools\\my_tool.exe'))).toBe('MT');
    expect(originMonogram(generic('aider2'))).toBe('AI');
    expect(originMonogram(generic('/'))).not.toContain('/');
  });

  it('knows which origins have a chat transcript', () => {
    expect(hasChatTranscript(generic('vca-fake-agent'))).toBe(false);
    expect(
      hasChatTranscript({ type: 'local', agent: { type: 'codex' }, project_dir: '/', pid: 1 }),
    ).toBe(true);
    expect(hasChatTranscript({ type: 'cloud', provider: 'jules', remote_id: 'x', url: null })).toBe(
      true,
    );
  });
});
