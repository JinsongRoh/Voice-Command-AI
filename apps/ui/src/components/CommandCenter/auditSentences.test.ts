import { describe, expect, it } from 'vitest';

import { translate, type MessageKey, type Vars } from '../../i18n/i18n';
import type { AuditEntry } from '../../protocol/types';
import { auditSentence, isCommandEntry, parseDetail } from './auditSentences';

const en = (key: MessageKey, vars?: Vars) => translate('en', key, vars);
const ko = (key: MessageKey, vars?: Vars) => translate('ko', key, vars);
const names: Record<string, string> = { s1: 'Orbit 3D' };
const nameOf = (id: string) => names[id] ?? null;

function entry(event: string, detail: string, session_id: string | null = null): AuditEntry {
  return { ts: 1, device_id: 'd', device_name: 'PC', event, session_id, detail };
}

describe('parseDetail', () => {
  it('reads key=value fields, the text to the end', () => {
    expect(parseDetail('outcome=sent confirmed_by=allowlist text=keep going a=b now')).toEqual({
      outcome: 'sent',
      confirmed_by: 'allowlist',
      text: 'keep going a=b now',
    });
    expect(parseDetail('tool=send_to_session reason=the session waits on a decision')).toEqual({
      tool: 'send_to_session',
      reason: 'the session waits on a decision',
    });
    expect(parseDetail('a session event happened')).toEqual({});
    expect(parseDetail('')).toEqual({});
  });
});

describe('auditSentence', () => {
  it('says what a send did, by the session name', () => {
    expect(
      auditSentence(
        entry(
          'conductor_send',
          'outcome=sent confirmed_by=allowlist text=계속 이어서 진행해 주세요',
          's1',
        ),
        en,
        nameOf,
      ),
    ).toBe('Sent “계속 이어서 진행해 주세요” to Orbit 3D (auto-allowed).');
    expect(
      auditSentence(
        entry(
          'conductor_send',
          'outcome=sent confirmed_by=person request=cr-1 text=npm test',
          's1',
        ),
        en,
        nameOf,
      ),
    ).toBe('Sent “npm test” to Orbit 3D (you allowed it).');
    const byInstruction = entry(
      'conductor_send',
      'outcome=sent confirmed_by=instruction grant=ig-1-1 text=빌드 상태 확인해 줘',
      's1',
    );
    expect(auditSentence(byInstruction, ko, nameOf)).toBe(
      '지시에 따라 자동 승인: “Orbit 3D”에 “빌드 상태 확인해 줘” 전달',
    );
    expect(auditSentence(byInstruction, en, nameOf)).toBe(
      'Auto-approved by your instruction: sent “빌드 상태 확인해 줘” to Orbit 3D.',
    );
    expect(
      auditSentence(
        entry('conductor_grant', 'grant=ig-1-1 all=false minutes=10 sessions=s1,gone'),
        en,
        nameOf,
      ),
    ).toBe(
      'Your instruction: checked sends to Orbit 3D, a session are auto-approved for this turn (up to 10 min).',
    );
    expect(
      auditSentence(
        entry('conductor_grant', 'grant=ig-1-2 all=true minutes=10 sessions=s1'),
        ko,
        nameOf,
      ),
    ).toBe('지시에 따라 이번 턴 동안 모든 세션에 보내는 내용은 확인 후 자동 승인(최대 10분)');
    expect(
      auditSentence(
        entry('conductor_send', 'outcome=timed_out request=cr-2 text=x', 'gone'),
        en,
        nameOf,
      ),
    ).toBe('Did not send “x” to a session: nobody answered in time.');
    expect(
      auditSentence(
        entry('conductor_send', 'outcome=denied request=cr-3 text=x', 's1'),
        ko,
        nameOf,
      ),
    ).toBe('Orbit 3D 세션에 “x”을(를) 보내지 않았습니다. 거부되었습니다.');
  });

  it('reads entries that keep the length of the text only', () => {
    const sent = (detail: string, t = en) =>
      auditSentence(entry('conductor_send', detail, 's1'), t, nameOf);
    expect(sent('outcome=sent confirmed_by=allowlist chars=14')).toBe(
      'Sent a message to Orbit 3D (auto-allowed).',
    );
    expect(sent('outcome=sent confirmed_by=person request=cr-1 needs=x chars=8')).toBe(
      'Sent a message to Orbit 3D (you allowed it).',
    );
    expect(sent('outcome=sent confirmed_by=instruction grant=ig-1-1 chars=12')).toBe(
      'Auto-approved by your instruction: sent a message to Orbit 3D.',
    );
    expect(sent('outcome=sent chars=3')).toBe('Sent a message to Orbit 3D.');
    expect(sent('outcome=timed_out request=cr-2 needs=x chars=1')).toBe(
      'Did not send a message to Orbit 3D: nobody answered in time.',
    );
    expect(sent('outcome=denied request=cr-3 needs=x chars=1', ko)).toBe(
      'Orbit 3D 세션에 메시지를 보내지 않았습니다. 거부되었습니다.',
    );
    expect(auditSentence(entry('conductor_wake', 'events=2 chars=310'), en, nameOf)).toBe(
      'Passed new events to the conductor.',
    );
    expect(
      auditSentence(
        entry('conductor_escalate', 'source=conductor decision=d1 summary_chars=24', 's1'),
        en,
        nameOf,
      ),
    ).toBe('Raised a decision of Orbit 3D to your approvals.');
    // No sentence quotes a number as if it were the text.
    expect(sent('outcome=sent confirmed_by=allowlist chars=14')).not.toContain('“');
  });

  it('cuts long text', () => {
    const long = 'a'.repeat(200);
    const s = auditSentence(entry('conductor_send', `outcome=sent text=${long}`, 's1'), en, nameOf);
    expect(s).toContain('…');
    expect(s.length).toBeLessThan(100);
  });

  it('covers the other conductor events', () => {
    expect(
      auditSentence(entry('conductor_start', 'agent=claude_code hooks=conductor'), en, nameOf),
    ).toBe('The conductor started (Claude Code).');
    expect(auditSentence(entry('conductor_stop', ''), en, nameOf)).toBe('The conductor stopped.');
    expect(auditSentence(entry('conductor_kill', ''), en, nameOf)).toBe(
      'The kill switch stopped the conductor.',
    );
    expect(auditSentence(entry('conductor_decide', 'request=cr-1 allow=true'), en, nameOf)).toBe(
      'A conductor request was allowed.',
    );
    expect(auditSentence(entry('conductor_allow', 'minutes=15', 's1'), en, nameOf)).toBe(
      'Sends to Orbit 3D need no confirmation for 15 min.',
    );
    expect(auditSentence(entry('conductor_allow', 'minutes=0', 's1'), en, nameOf)).toBe(
      'Sends to Orbit 3D need confirmation again.',
    );
    expect(
      auditSentence(
        entry('conductor_refused', 'tool=send_to_session reason=hourly limit'),
        en,
        nameOf,
      ),
    ).toBe('Was refused “send_to_session”: hourly limit.');
    expect(auditSentence(entry('conductor_restore', 'profile=work windows=1'), en, nameOf)).toBe(
      'Restored the window layout “work” (1 window).',
    );
    expect(
      auditSentence(
        entry(
          'conductor_settings',
          'enabled=true agent=codex wake_events=false max_actions_per_hour=20',
        ),
        en,
        nameOf,
      ),
    ).toBe('Conductor settings saved (Codex CLI, at most 20 actions an hour).');
    expect(auditSentence(entry('conductor_wake', 'turn ended in Orbit 3D'), en, nameOf)).toBe(
      'Was told: turn ended in Orbit 3D',
    );
    // Pausing and resuming the Command Center.
    expect(auditSentence(entry('cc_pause', 'paused=true'), en, nameOf)).toBe(
      'The Command Center was paused.',
    );
    expect(auditSentence(entry('cc_pause', 'paused=false'), en, nameOf)).toBe(
      'The Command Center was resumed.',
    );
    expect(auditSentence(entry('cc_pause', 'paused=false'), ko, nameOf)).toBe(
      '지휘소의 일시 정지가 해제되었습니다.',
    );
    // A kill switch from a phone without a valid code (no tool field).
    expect(
      auditSentence(entry('conductor_refused', 'kill switch without a valid code'), en, nameOf),
    ).toBe('A kill switch request was refused: the authenticator code was missing or wrong.');
    // A turn that ended without a reply, with and without hook events.
    expect(
      auditSentence(
        entry('conductor_no_reply', 'the turn ended without a new reply in the transcript', 's1'),
        en,
        nameOf,
      ),
    ).toBe('A conductor turn ended without a reply you could see.');
    expect(
      auditSentence(
        entry(
          'conductor_no_reply',
          'the turn ended without a reply; the session never reported a hook event (VCA hooks missing?)',
        ),
        en,
        nameOf,
      ),
    ).toBe('A conductor turn ended without a reply you could see; the VCA hooks may be missing.');
    // Unknown events fall back to their label and the raw detail.
    expect(auditSentence(entry('cc_future', 'x=1'), en, nameOf)).toBe('cc_future · x=1');
  });

  it('says which rule let the conductor answer and what it escalated', () => {
    const answered = entry(
      'conductor_answer',
      'decision=d1 option=1 answer=approve rule=tests command=cargo test -p core',
      's1',
    );
    expect(parseDetail(answered.detail).command).toBe('cargo test -p core');
    expect(auditSentence(answered, en, nameOf)).toBe(
      'Approved a decision of Orbit 3D under your rule “tests”.',
    );
    expect(
      auditSentence(
        entry('conductor_escalate', 'source=conductor decision=d1 summary=배포 확인 필요', 's1'),
        ko,
        nameOf,
      ),
    ).toBe('Orbit 3D의 결정을 결재함에 올렸습니다: “배포 확인 필요”');
    expect(
      auditSentence(
        entry('conductor_escalate', 'source=fallback decision=d1 reason=timeout', 's1'),
        en,
        nameOf,
      ),
    ).toBe('A decision of Orbit 3D went to your approvals by itself.');
  });

  it('keeps the conductor and Command Center entries only', () => {
    expect(isCommandEntry(entry('conductor_send', ''))).toBe(true);
    expect(isCommandEntry(entry('cc_pause', ''))).toBe(true);
    expect(isCommandEntry(entry('shell_open', ''))).toBe(false);
  });
});
