/**
 * The demo PC's pronunciation dictionary (same rules as
 * crates/vca-server/src/pronunciations.rs: shipped entries, the user's
 * entries win, a deleted shipped entry stays off). Changes are pushed.
 */
import { termKey, validTerm } from '../speech/pronunciation';
import type { Pronunciation, PronunciationClientMsg } from './pronunciations';
import type { ServerMsg } from './types';

const SHIPPED: [string, string][] = [
  ['Claude Code', '클로드 코드'],
  ['Codex', '코덱스'],
  ['VCA', '브이씨에이'],
  ['AI', '에이아이'],
  ['API', '에이피아이'],
  ['PDF', '피디에프'],
];

function base(language: string | null): string {
  return (language ?? 'ko').toLowerCase().split(/[-_]/)[0] || 'ko';
}

export class MockPronunciations {
  private entries: Pronunciation[] = [
    { term: 'SkyHarbor', spoken: '스카이하버', language: 'ko', source: 'auto', updated_at: 1 },
  ];
  private readonly removed = new Set<string>();

  constructor(
    private readonly now: () => number,
    private readonly push: (body: ServerMsg) => void,
  ) {}

  list(language: string | null): Pronunciation[] {
    const out = [...this.entries];
    const seen = new Set(out.map((e) => `${e.language}:${termKey(e.term)}`));
    for (const [term, spoken] of SHIPPED) {
      const id = `ko:${termKey(term)}`;
      if (seen.has(id) || this.removed.has(id)) continue;
      out.push({ term, spoken, language: 'ko', source: 'default', updated_at: 0 });
    }
    return out
      .filter((e) => language === null || e.language === base(language))
      .sort((a, b) => a.term.toLowerCase().localeCompare(b.term.toLowerCase()));
  }

  handle(msg: PronunciationClientMsg): ServerMsg {
    if (msg.type === 'list_pronunciations')
      return { type: 'pronunciations', entries: this.list(msg.language) };
    const language = base(msg.language);
    const key = termKey(msg.term);
    const id = `${language}:${key}`;
    if (msg.type === 'set_pronunciation') {
      const spoken = msg.spoken.trim().replace(/\s+/g, ' ');
      if (!validTerm(msg.term) || !spoken)
        return { type: 'error', code: 'invalid_request', message: 'invalid pronunciation' };
      this.entries = this.entries.filter(
        (e) => !(e.language === language && termKey(e.term) === key),
      );
      this.entries.push({
        term: msg.term.trim().replace(/\s+/g, ' '),
        spoken,
        language,
        source: 'user',
        updated_at: this.now(),
      });
      this.removed.delete(id);
    } else {
      const before = this.entries.length;
      this.entries = this.entries.filter(
        (e) => !(e.language === language && termKey(e.term) === key),
      );
      const shipped = SHIPPED.some(([t]) => termKey(t) === key) && language === 'ko';
      const existed = before !== this.entries.length || (shipped && !this.removed.has(id));
      if (shipped) this.removed.add(id);
      if (!existed)
        return { type: 'error', code: 'not_found', message: 'no pronunciation for this term' };
    }
    const reply: ServerMsg = { type: 'pronunciations', entries: this.list(null) };
    this.push(reply);
    return reply;
  }
}
