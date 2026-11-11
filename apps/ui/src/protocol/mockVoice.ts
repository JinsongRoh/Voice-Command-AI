/**
 * Briefing and summary answers of the simulated computer. The real server
 * builds these (`crates/vca-server/src/voice_brief.rs`); the demo only
 * needs something short and plausible in the listener's language.
 */
import type { ServerMsg, SessionId, SessionInfo } from './types';

type Lang = 'en' | 'ko' | 'ja' | 'es';

function lang(tag: string | null): Lang {
  const base = (tag ?? '').toLowerCase().split(/[-_]/)[0];
  return base === 'ko' || base === 'ja' || base === 'es' ? base : 'en';
}

function waiting(s: SessionInfo): boolean {
  return s.state.type === 'awaiting_decision' || s.state.type === 'awaiting_input';
}

/** "N sessions, M waiting for you." in the listener's language. */
export function mockBriefing(
  sessions: SessionInfo[],
  _order: SessionId[],
  language: string | null,
): ServerMsg {
  const total = sessions.length;
  const wait = sessions.filter(waiting).length;
  const texts: Record<Lang, string> = {
    en: `${total} sessions on the demo computer, ${wait} waiting for you.`,
    ko: `데모 컴퓨터의 세션 ${total}개, 기다리는 세션 ${wait}개.`,
    ja: `デモのコンピューターのセッション${total}件、対応待ち${wait}件。`,
    es: `${total} sesiones en el equipo de demostración, ${wait} esperan tu respuesta.`,
  };
  return { type: 'briefing', text: texts[lang(language)] };
}

/** The fixed "finished" line: the demo has no LLM. */
export function mockSummary(
  sessionId: SessionId,
  number: number | null | undefined,
  language: string | null,
): ServerMsg {
  const n = number ?? 1;
  const texts: Record<Lang, string> = {
    en: `Session ${n} finished its work.`,
    ko: `${n}번 세션 작업이 끝났습니다.`,
    ja: `${n}番のセッションの作業が終わりました。`,
    es: `La sesión ${n} terminó su trabajo.`,
  };
  return { type: 'summary', session_id: sessionId, text: texts[lang(language)], fallback: true };
}
