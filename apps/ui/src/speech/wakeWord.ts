/**
 * Radio-style voice words of hands-free v2 (used by the Voco wake flow,
 * `vocoFlow.ts`).
 *
 * - Wake word: "보코" / "Voco" / 「ボコ」, said at the START of an utterance:
 *   "보코, 테스트 돌려 줘, 오버".
 * - Send phrase: "보내줘 오버" / "Over" / 「送信、オーバー」 / "Cambio", said at
 *   the END of an utterance.
 * - The app's reply is "로저" / "Roger" (see `radioSpeech.ts` for how voices
 *   pronounce it).
 *
 * Speech recognizers mishear short words, so both matchers accept the common
 * mishearings, and both require the word to be isolated so ordinary speech
 * does not trigger them:
 *
 * - The wake word must be the first word, after at most two fillers or
 *   greetings ("음", "hey", "oye"). A Korean wake word may carry the
 *   vocative "야" ("보코야") but not a particle: "보코가 …", "보코의 …" talk
 *   ABOUT Voco and do not wake it. "보고" (a real word: "report", "see") only
 *   counts when it is the whole utterance or followed by a pause mark
 *   ("보고, 테스트 돌려 줘"); "보고서 작성해" and "보고 싶어" never wake.
 *   The same goes for "보컬" / "보컬아" and "Vocal", how recognizers often
 *   write a quick "보코": "보컬." wakes, "보컬 트레이닝 앱 만들어" does not.
 * - Plain send commands ("전송해", "보내줘", "send it", 「送って」, "envíalo")
 *   only end a message after a pause; see `PAUSED_SEND`.
 * - Decision: an utterance that starts with a bare wake word followed by more
 *   words ("보코 기능을 만들어 줘 오버") DOES wake: that is exactly the
 *   wake-then-command syntax and cannot be told apart lexically. Talking
 *   about the app at the start of a dictation needs a particle ("보코의
 *   기능을 …") or another word first; hands-free reads the command back
 *   before sending anyway.
 * - A bare send word ("오버", "over", "cambio", 「オーバー」) must be the last
 *   word and either the whole utterance, after a pause mark (", over"), or
 *   (Korean, English, Spanish) after a word that does not make it part of an
 *   ordinary phrase ("game over", "start over", "un cambio", "너무 오버").
 *   Japanese needs a pause or 「送信」 because 「時間オーバー」 and
 *   「予算オーバー」 are ordinary words. Two-word phrases ("보내줘 오버",
 *   "send it over", 「送信オーバー」) need no pause.
 *
 * Matching: Unicode NFKC and lower case; Latin words also lose their accents
 * and match "voco" within an edit distance of one when they start with "v"
 * ("vocco", "voko", "vico"), plus the listed b-forms ("boko", "boco"); Korean
 * and Japanese use explicit lists because their one-letter neighbours are
 * common words ("보호", 「ボロ」). All languages are accepted at any time.
 *
 * How people actually call it:
 * - stretched ("보코오~", "보오코"): repeated vowels and "~" are collapsed;
 * - repeated ("보코 보코", "보코, 보코", "보코보코"): one wake (the repeats
 *   are not dictated); "보코보코" only on its own ("보코보코 맞았어" is content);
 * - split by the recognizer ("보 코", "보, 코"): the syllables are joined;
 * - other hearings of a short call: a two-syllable word one jamo away from
 *   "보코" (ㅂ/ㅃ/ㅍ + ㅗ/ㅓ/ㅜ, then 코/꼬/고/컬/크) wakes only when it is the
 *   whole utterance and the speech was short (`RadioOptions.speechMs`).
 * Intonation (a sing-song or rising call) is not modelled: it only matters
 * through what the recognizer writes, which these forms cover.
 */
import { collapseStretch, jamoDistance, syllableJamo } from './jamo';

export type RadioLang = 'ko' | 'en' | 'ja' | 'es';

/** A wake word found at the start of an utterance. */
export interface WakeMatch {
  lang: RadioLang;
  /** The wake word as written in the utterance ("보코야", "Vo co"). */
  heard: string;
  /** UTF-16 offsets of the wake word in the utterance. */
  start: number;
  end: number;
  /** The rest of the utterance, without leading separators. */
  rest: string;
}

/** A send phrase found at the end of an utterance. */
export interface SendMatch {
  lang: RadioLang;
  /** The phrase as written ("보내 줘 오버", "Over"). */
  heard: string;
  start: number;
  end: number;
  /** The utterance before the phrase, without trailing separators. */
  before: string;
  /**
   * A plain send command ("전송해", "보내줘", "send it": ordinary words, see
   * `PAUSED_SEND`) at the end of a longer utterance. It may be the start of
   * a sentence the user goes on with after a thinking pause ("해당 내용을,
   * 전송해 … 서"), so the flow only sends after a further silence
   * (`vocoFlow.ts`). `false` for radio phrases ("보내줘 오버", "오버") and
   * for a plain command said on its own.
   */
  trailing: boolean;
}

/** Options of the matchers. */
export interface RadioOptions {
  /**
   * Extra wake words or send phrases set by the user (Settings). They only
   * count when isolated: a wake word followed by a pause or the end of the
   * utterance, a send phrase after a pause (see `PAUSED_SEND`).
   */
  extra?: readonly string[];
  /**
   * The audio had a pause (about 0.6 s of silence) right before the last
   * words of the utterance, even if the transcript shows no pause mark.
   */
  pausedBefore?: boolean;
  /**
   * Length of the speech (ms). A near-hearing of the wake word (see
   * `isNearWake`) only counts in a short utterance said on its own.
   */
  speechMs?: number;
}

/** An utterance split into its radio parts. */
export interface RadioUtterance {
  wake: WakeMatch | null;
  send: SendMatch | null;
  /** The text between the wake word and the send phrase (the whole text without them). */
  command: string;
}

// ------------------------------------------------------------------ words

const KO_WAKE = [
  '보코',
  '보꼬',
  '버코',
  '뽀코',
  '뽀꼬',
  '부코',
  '보꾸',
  '보콜',
  '보커',
  '보크',
  '보코요',
];
/**
 * Only when isolated (whole utterance or followed by a pause): these are
 * real words. Recognizers often write a quick "보코" as "보컬" (vocal), but
 * "보컬 트레이닝 앱 만들어" is ordinary speech and must not wake; "보고서",
 * "보고 싶어" and "포코 …" never do either. "복호" is how a "보코" in noise
 * often comes out, and a real word too ("복호 알고리즘", "복호화").
 */
const KO_WAKE_ISOLATED = ['보고', '포코', '보구', '보컬', '보컬아', '복호'];
/** Longest speech for a near-hearing of the wake word (a slow "보…코" included). */
export const NEAR_WAKE_MAX_MS = 2000;
const NEAR_INITIALS = new Set(['ㅂ', 'ㅃ', 'ㅍ']);
const NEAR_VOWELS = new Set(['ㅗ', 'ㅓ', 'ㅜ']);
const NEAR_SECOND = new Set(['코', '꼬', '고', '컬', '크']);

/**
 * A two-syllable Korean word that is probably a misheard "보코": ㅂ/ㅃ/ㅍ with
 * ㅗ/ㅓ/ㅜ, then 코/꼬/고/컬/크, at most one jamo away ("포코", "부코", "보크").
 * Only used for a short utterance said on its own.
 */
export function isNearWake(word: string): boolean {
  const chars = Array.from(word);
  if (chars.length !== 2) return false;
  const first = syllableJamo(chars[0]!);
  if (!first || !NEAR_INITIALS.has(first.initial) || !NEAR_VOWELS.has(first.vowel)) return false;
  if (!NEAR_SECOND.has(chars[1]!)) return false;
  return jamoDistance(word, '보코') <= 1;
}
/**
 * Vocative and affectionate endings accepted after a Korean wake word
 * ("보코야", "보코씨", "보코님", "보코짱"). Particles that talk ABOUT Voco
 * ("보코가", "보코의", "보코를") are not among them.
 */
const KO_VOCATIVES = ['야', '아', '이', '씨', '님', '군', '양', '짱'];
/** The same words written apart ("보코 군", "보코 님"): part of the call. */
const KO_VOCATIVE_WORDS = ['씨', '님', '군', '양', '짱', '야', '아'];

function isKoWakeForm(word: string): boolean {
  return (
    KO_WAKE.includes(word) ||
    KO_VOCATIVES.some((v) => KO_WAKE.includes(word.slice(0, -v.length)) && word.endsWith(v))
  );
}
const JA_WAKE = ['ボコ', 'ヴォコ'];
const LATIN_WAKE = ['voco', 'boko', 'vocco', 'voko', 'boco', 'bocco'];
/** Latin hearings that are real words: only when isolated ("Vocal." alone). */
const LATIN_WAKE_ISOLATED = ['vocal', 'poco'];
/** Real words one edit away from "voco". */
const LATIN_WAKE_BLOCK = ['voto', 'voce', 'vaca', 'vino', 'vivo', 'loco', 'poco', 'coco', 'foco'];

/** Fillers and greetings allowed before the wake word (at most two). */
const WAKE_LEAD = [
  '우리',
  '헤이',
  '오케이',
  '음',
  '어',
  '아',
  '저기',
  '자',
  '야',
  'hey',
  'hi',
  'ok',
  'okay',
  'um',
  'uh',
  'oh',
  'so',
  'yo',
  'ねえ',
  'ねぇ',
  'あの',
  'えっと',
  'おい',
  'oye',
  'hola',
  'eh',
  'ey',
  'vale',
];
const MAX_WAKE_LEAD = 2;

/** Multi-word send phrases (tokens), checked before the bare words. */
const SEND_PHRASES: readonly { lang: RadioLang; tokens: readonly string[] }[] = [
  { lang: 'ko', tokens: ['보내줘', '오버'] },
  { lang: 'ko', tokens: ['보내', '줘', '오버'] },
  { lang: 'ko', tokens: ['보내줘오버'] },
  { lang: 'en', tokens: ['send', 'it', 'over'] },
  { lang: 'ja', tokens: ['送信', 'オーバー'] },
  { lang: 'ja', tokens: ['送信オーバー'] },
];

/**
 * Plain send commands ("…, 전송해", "…, 보내줘", "…, send it"). They are
 * ordinary words ("파일 전송해" is content, "보내는 기능 만들어" too), so they
 * only end a message as the LAST words AFTER A PAUSE: a pause mark in the
 * transcript, a pause in the audio (`RadioOptions.pausedBefore`) or the
 * whole utterance (it was said on its own, after a silence). The end of the
 * text alone is not enough.
 *
 * Bare stems (`alone`: "전송", "보내", "send") only count as the whole
 * utterance: after other words they are usually the first part of a longer
 * phrase cut by a thinking pause ("해당 내용을 … 전송 … 하고", "보내 … 서").
 */
const PAUSED_SEND: readonly { lang: RadioLang; tokens: readonly string[]; alone?: boolean }[] = [
  { lang: 'ko', tokens: ['전송해', '주세요'] },
  { lang: 'ko', tokens: ['전송', '해', '줘'] },
  { lang: 'ko', tokens: ['전송해', '줘'] },
  { lang: 'ko', tokens: ['전송', '해줘'] },
  { lang: 'ko', tokens: ['전송', '해'] },
  { lang: 'ko', tokens: ['보내', '주세요'] },
  { lang: 'ko', tokens: ['보내', '줘'] },
  { lang: 'ko', tokens: ['전송해주세요'] },
  { lang: 'ko', tokens: ['전송해줘'] },
  { lang: 'ko', tokens: ['전송해'] },
  { lang: 'ko', tokens: ['전송'], alone: true },
  { lang: 'ko', tokens: ['보내주세요'] },
  { lang: 'ko', tokens: ['보내줘'] },
  { lang: 'ko', tokens: ['보내'], alone: true },
  { lang: 'en', tokens: ['send', 'it'] },
  { lang: 'en', tokens: ['send'], alone: true },
  { lang: 'ja', tokens: ['送信して'] },
  { lang: 'ja', tokens: ['送って'] },
  { lang: 'es', tokens: ['envialo'] },
  { lang: 'es', tokens: ['envia'] },
];

/** Bare send words and the previous words that make them ordinary speech. */
const SEND_WORDS: readonly { lang: RadioLang; word: string; block: readonly string[] }[] = [
  {
    lang: 'ko',
    word: '오버',
    block: ['게임', '좀', '너무', '완전', '진짜', '약간', '살짝', '많이', '되게', '엄청', '조금'],
  },
  {
    lang: 'en',
    word: 'over',
    block: [
      'all',
      'and',
      'are',
      'be',
      'been',
      'bend',
      'carry',
      'come',
      'game',
      'get',
      'go',
      'hand',
      'hover',
      'is',
      'it',
      "it's",
      'its',
      'left',
      'look',
      'move',
      'pass',
      'pull',
      'push',
      'roll',
      'start',
      'switch',
      'take',
      "that's",
      'think',
      'turn',
      'was',
      'were',
    ],
  },
  { lang: 'ja', word: 'オーバー', block: [] },
  {
    lang: 'es',
    word: 'cambio',
    block: [
      'al',
      'algun',
      'cada',
      'con',
      'de',
      'del',
      'el',
      'ese',
      'este',
      'gran',
      'hay',
      'mi',
      'ningun',
      'otro',
      'primer',
      'sin',
      'su',
      'tu',
      'un',
      'en',
    ],
  },
];

// ------------------------------------------------------------------ tokens

interface Token {
  /** Normalized text. */
  norm: string;
  start: number;
  end: number;
}

/** Word characters: letters, digits, combining marks and apostrophes. */
const WORD = /[\p{L}\p{N}\p{M}'’]+/gu;
/** Marks that stand for a pause between words. */
const PAUSE = /[,，、.。!！?？…;；:：—–-]/u;
const KATAKANA = /[\p{Script=Katakana}ー]/u;
const LATIN_WORD = /^[\p{Script=Latin}'’]+$/u;

/** NFKC, lower case, curly apostrophes straightened, Latin accents removed. */
export function normalizeRadioWord(word: string): string {
  const w = word.normalize('NFKC').toLowerCase().replace(/’/g, "'");
  return LATIN_WORD.test(w) ? w.normalize('NFD').replace(/\p{M}/gu, '').normalize('NFC') : w;
}

function tokenize(text: string): Token[] {
  return Array.from(text.matchAll(WORD), (m) => ({
    norm: normalizeRadioWord(m[0]),
    start: m.index ?? 0,
    end: (m.index ?? 0) + m[0].length,
  }));
}

function pauseBetween(text: string, from: number, to: number): boolean {
  return PAUSE.test(text.slice(from, to));
}

/** Levenshtein distance (small strings only). */
function editDistance(a: string, b: string): number {
  const x = Array.from(a);
  const y = Array.from(b);
  let prev = y.map((_, j) => j + 1);
  prev.unshift(0);
  for (let i = 1; i <= x.length; i += 1) {
    const row = [i];
    for (let j = 1; j <= y.length; j += 1) {
      row[j] = Math.min(
        prev[j]! + 1,
        row[j - 1]! + 1,
        prev[j - 1]! + (x[i - 1] === y[j - 1] ? 0 : 1),
      );
    }
    prev = row;
  }
  return prev[y.length]!;
}

/** Whether a normalized Latin word is a hearing of "voco". */
export function isLatinWake(word: string): boolean {
  if (LATIN_WAKE.includes(word)) return true;
  if (!word.startsWith('v') || LATIN_WAKE_BLOCK.includes(word)) return false;
  return word.length >= 3 && editDistance(word, 'voco') <= 1;
}

// ------------------------------------------------------------------ wake

/** Wake form at the start of `token` (in Japanese the word may run on). */
function wakeIn(token: Token, isolated: boolean): { lang: RadioLang; length: number } | null {
  const w = token.norm;
  // "보코보코" (one breath, two calls): one wake, but only on its own.
  if (isolated && /^\p{Script=Hangul}+$/u.test(w)) {
    const half = w.slice(0, w.length / 2);
    if (w.length % 2 === 0 && half + half === w && KO_WAKE.includes(half)) {
      return { lang: 'ko', length: token.end - token.start };
    }
  }
  for (const k of KO_WAKE) {
    if (w === k || KO_VOCATIVES.some((v) => w === k + v)) {
      return { lang: 'ko', length: token.end - token.start };
    }
  }
  if (isolated && KO_WAKE_ISOLATED.includes(w)) {
    return { lang: 'ko', length: token.end - token.start };
  }
  if (isolated && LATIN_WAKE_ISOLATED.includes(w)) {
    return { lang: 'en', length: token.end - token.start };
  }
  for (const j of JA_WAKE) {
    // 「ボコ」 may be followed by kana or kanji in the same token, but not by
    // more katakana (「ボコボコ」 is a different word).
    if (w.startsWith(j) && !KATAKANA.test(w.charAt(j.length))) {
      return { lang: 'ja', length: j.length };
    }
  }
  if (LATIN_WORD.test(w) && isLatinWake(w)) {
    // "boco" is how Spanish recognizers write it; the rest is reported as English.
    const lang = w === 'boco' || w === 'bocco' ? 'es' : 'en';
    return { lang, length: token.end - token.start };
  }
  return null;
}

/**
 * The wake word at the start of `text`, after at most two fillers, or null.
 * See the module comment for the rules.
 */
export function matchWakeWord(text: string, opts: RadioOptions = {}): WakeMatch | null {
  const tokens = tokenize(text);
  for (const t of tokens) {
    // "보코오~" / "보오코" are read as "보코".
    if (/\p{Script=Hangul}/u.test(t.norm)) t.norm = collapseStretch(t.norm);
  }
  const found = findWake(text, tokens, opts);
  if (!found) return null;
  // "보코 보코", "보코, 보코", "보코 군": repeats and a separate vocative
  // belong to the call, not to the message.
  let end = found.end;
  for (const t of tokens) {
    if (t.start < end) continue;
    const vocative = found.lang === 'ko' && KO_VOCATIVE_WORDS.includes(t.norm);
    if (pauseOrSpaceOnly(text, end, t.start) && (isRepeatOfWake(t.norm) || vocative)) end = t.end;
    else break;
  }
  return end === found.end ? found : wakeMatch(text, found.lang, found.start, end);
}

function pauseOrSpaceOnly(text: string, from: number, to: number): boolean {
  return /^[\s,，、.。!！?？…~〜]*$/u.test(text.slice(from, to));
}

function isRepeatOfWake(word: string): boolean {
  return isKoWakeForm(word) || (LATIN_WORD.test(word) && isLatinWake(word));
}

function findWake(text: string, tokens: Token[], opts: RadioOptions): WakeMatch | null {
  const extra = (opts.extra ?? []).map(tokenize).filter((w) => w.length > 0);
  for (let i = 0; i < tokens.length && i <= MAX_WAKE_LEAD; i += 1) {
    const t = tokens[i]!;
    const next = tokens[i + 1];
    const isolated = !next || pauseBetween(text, t.end, next.start);
    for (const word of extra) {
      const n = word.length;
      const run = tokens.slice(i, i + n);
      const after = tokens[i + n];
      if (
        run.length === n &&
        run.every((r, k) => r.norm === word[k]!.norm) &&
        (!after || pauseBetween(text, run[n - 1]!.end, after.start))
      ) {
        return wakeMatch(text, langOf(t.norm), t.start, run[n - 1]!.end);
      }
    }
    const found = wakeIn(t, isolated);
    if (found) return wakeMatch(text, found.lang, t.start, t.start + found.length);
    // "보 코", "보, 코": the recognizer split the call into syllables.
    if (next && Array.from(t.norm).length === 1 && Array.from(next.norm).length === 1) {
      const joined = t.norm + next.norm;
      const after = tokens[i + 2];
      const joinedIsolated = !after || pauseBetween(text, next.end, after.start);
      if (
        /^\p{Script=Hangul}+$/u.test(joined) &&
        (KO_WAKE.includes(joined) || (joinedIsolated && KO_WAKE_ISOLATED.includes(joined)))
      ) {
        return wakeMatch(text, 'ko', t.start, next.end);
      }
    }
    // A short call heard as a near word ("포코", "부코"): only on its own.
    if (
      tokens.length === i + 1 &&
      opts.speechMs !== undefined &&
      opts.speechMs <= NEAR_WAKE_MAX_MS &&
      isNearWake(t.norm)
    ) {
      return wakeMatch(text, 'ko', t.start, t.end);
    }
    // "vo co": two short Latin pieces.
    if (next && LATIN_WORD.test(t.norm) && t.norm.length <= 3 && next.norm.length <= 3) {
      const joined = t.norm + next.norm;
      if (
        LATIN_WORD.test(joined) &&
        isLatinWake(joined) &&
        !pauseBetween(text, t.end, next.start)
      ) {
        return wakeMatch(text, 'en', t.start, next.end);
      }
    }
    if (!WAKE_LEAD.includes(t.norm)) return null;
  }
  return null;
}

const HANGUL = /\p{Script=Hangul}/u;
const CJK_KANA = /[\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Han}]/u;

/** Language reported for a user-defined word (by its script). */
function langOf(word: string): RadioLang {
  if (HANGUL.test(word)) return 'ko';
  if (CJK_KANA.test(word)) return 'ja';
  return 'en';
}

function wakeMatch(text: string, lang: RadioLang, start: number, end: number): WakeMatch {
  return {
    lang,
    heard: text.slice(start, end),
    start,
    end,
    rest: text.slice(end).replace(/^[\s\p{P}~〜～]+/u, ''),
  };
}

// ------------------------------------------------------------------ send

/**
 * The send phrase at the end of `text`, or null. See the module comment for
 * the rules.
 */
export function matchSendPhrase(text: string, opts: RadioOptions = {}): SendMatch | null {
  const tokens = tokenize(text);
  if (tokens.length === 0) return null;
  for (const p of SEND_PHRASES) {
    const n = p.tokens.length;
    if (tokens.length < n) continue;
    const tail = tokens.slice(-n);
    if (tail.every((t, k) => t.norm === p.tokens[k])) {
      return sendMatch(text, p.lang, tail[0]!.start, tail[n - 1]!.end);
    }
  }
  const paused: { lang: RadioLang; tokens: readonly string[]; alone?: boolean }[] = [
    ...(opts.extra ?? []).map((w) => {
      const words = tokenize(w).map((t) => t.norm);
      return { lang: langOf(words.join('')), tokens: words };
    }),
    ...PAUSED_SEND,
  ];
  for (const p of paused) {
    const n = p.tokens.length;
    if (n === 0 || tokens.length < n) continue;
    const tail = tokens.slice(-n);
    if (!tail.every((t, k) => t.norm === p.tokens[k])) continue;
    const prev = tokens[tokens.length - n - 1];
    // Said on its own…
    if (!prev) return sendMatch(text, p.lang, tail[0]!.start, tail[n - 1]!.end);
    // …or (full commands only) after a pause mark or a pause in the audio.
    if (!p.alone && (pauseBetween(text, prev.end, tail[0]!.start) || opts.pausedBefore)) {
      return sendMatch(text, p.lang, tail[0]!.start, tail[n - 1]!.end, true);
    }
  }
  const last = tokens[tokens.length - 1]!;
  const prev = tokens[tokens.length - 2];
  for (const s of SEND_WORDS) {
    if (s.lang === 'ja') {
      // 「送信オーバー」 inside a longer token, else a separate 「オーバー」.
      const idx = last.norm.lastIndexOf('送信' + s.word);
      if (idx >= 0 && idx + 2 + s.word.length === last.norm.length) {
        return sendMatch(text, 'ja', last.start + idx, last.end);
      }
    }
    if (last.norm !== s.word) continue;
    const ok =
      !prev ||
      pauseBetween(text, prev.end, last.start) ||
      (s.lang !== 'ja' && !s.block.includes(prev.norm));
    if (ok) return sendMatch(text, s.lang, last.start, last.end);
  }
  return null;
}

function sendMatch(
  text: string,
  lang: RadioLang,
  start: number,
  end: number,
  trailing = false,
): SendMatch {
  return {
    lang,
    heard: text.slice(start, end),
    start,
    end,
    before: text.slice(0, start).replace(/[\s\p{P}]+$/u, ''),
    trailing,
  };
}

// ------------------------------------------------------------------ both

/**
 * Splits an utterance into wake word, command and send phrase ("보코, 테스트
 * 돌려 줘, 오버" -> wake "보코", command "테스트 돌려 줘", send "오버").
 */
export function parseRadioUtterance(
  text: string,
  opts: { wake?: RadioOptions; send?: RadioOptions } = {},
): RadioUtterance {
  const wake = matchWakeWord(text, opts.wake);
  let send = matchSendPhrase(text, opts.send);
  if (wake && send && send.start < wake.end) send = null;
  const from = wake ? wake.end : 0;
  const to = send ? send.start : text.length;
  const command = text
    .slice(from, to)
    .replace(/^[\s\p{P}]+/u, '')
    .replace(/[\s\p{P}]+$/u, '');
  return { wake, send, command };
}
