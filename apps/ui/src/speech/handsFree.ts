/**
 * Hands-free mode (voice item 4) and spoken decisions (item 5).
 *
 * - After a dictation, the beginning of the text is read back with its target
 *   ("삼번 세션에 테스트 돌려 줘. 보낼까요?") and a short spoken answer sends
 *   it, cancels it or dictates again.
 * - A pending decision is read out and answered by voice, but ONLY when the
 *   server classifies its command `safe` and only with the plain yes / no
 *   options. Anything else is "check the screen" and never answered by voice.
 *
 * Send answers are matched against a closed vocabulary: the utterance must be
 * made only of words of ONE answer, plus fillers ("어", "um") and courtesy
 * endings ("주세요", "please"), so "어 보내" and "네 보내주세요" send. Only an
 * explicit cancel word cancels; silence or an unclear answer asks again and
 * finally leaves the text unsent in the box. Nothing is ever sent, and nothing
 * is ever discarded, on an unclear answer. Decision answers stay exact words.
 *
 * Everything here is pure or takes its speech I/O as a parameter (`VoiceIO`),
 * so the flows are tested without a microphone, STT or TTS.
 */
import { diag } from '../diag/log';
import type { DangerLevel, Decision } from '../protocol/types';
import { isHangulSyllable, jamoDistance } from './jamo';
import { normalizeKoreanNumbers } from './koreanNumbers';
import { voiceQueue } from './voiceQueue';

export type VoiceLang = 'ko' | 'en' | 'ja' | 'es';

/** Spoken prompts follow the UI language; unknown languages use English. */
export function voiceLang(locale: string): VoiceLang {
  const base = locale.toLowerCase().split(/[-_]/)[0];
  return base === 'ko' || base === 'ja' || base === 'es' ? base : 'en';
}

// ------------------------------------------------------------------ vocabulary

const KATAKANA_START = 0x30a1;
const KATAKANA_END = 0x30f6;

/**
 * Normalizes a recognized answer for matching: Unicode NFKC, lower case,
 * katakana folded to hiragana, and every space, punctuation mark and symbol
 * removed ("Yes." -> "yes", "보내 줘!" -> "보내줘", "ハイ。" -> "はい").
 */
export function normalizeAnswer(text: string): string {
  return foldAnswer(text).replace(/[\s\p{P}\p{S}]/gu, '');
}

/** NFKC, lower case and katakana folded to hiragana (spaces and marks kept). */
function foldAnswer(text: string): string {
  return Array.from(text.normalize('NFKC').toLowerCase(), (ch) => {
    const code = ch.codePointAt(0) ?? 0;
    return code >= KATAKANA_START && code <= KATAKANA_END ? String.fromCodePoint(code - 0x60) : ch;
  }).join('');
}

function wordSet(words: readonly string[]): ReadonlySet<string> {
  return new Set(words.map(normalizeAnswer));
}

/** Words of each answer, per language (all languages are accepted at any time). */
export const SEND_WORDS = {
  ko: ['응', '네', '예', '보내', '보내줘', '전송', '그래', '좋아'],
  en: ['yes', 'send'],
  ja: ['はい', '送信'],
  es: ['sí', 'si', 'enviar'],
} as const satisfies Record<VoiceLang, readonly string[]>;

export const CANCEL_WORDS = {
  ko: ['취소', '아니', '아니요', '아니오', '하지 마'],
  en: ['no', 'cancel'],
  ja: ['いいえ', 'キャンセル'],
  es: ['no', 'cancelar'],
} as const satisfies Record<VoiceLang, readonly string[]>;

/**
 * Other spellings of the send words that recognizers produce. "보네" is how
 * STT often writes "보내" (the two sound the same in standard Korean).
 */
const SEND_VARIANTS = [
  '응응',
  '네네',
  '예예',
  '보네',
  '보낸',
  '보내라',
  '보내봐',
  '보내자',
  '보내기',
  '보낼게',
  '보냅시다',
  '그레',
  '맞아',
  '오케이',
  '오키',
  '고고',
  '전송해',
  'yeah',
  'yep',
  'うん',
  'envía',
  'envia',
  'envíalo',
  'envialo',
];

/** Other explicit refusals ("보내지 마" is as explicit as "하지 마"). */
const CANCEL_VARIANTS = [
  '아냐',
  '아뇨',
  '아니야',
  '노',
  '됐어',
  '그만',
  '취소할게',
  '보내지 마',
  '안 보내',
  'nope',
  "don't send",
];

/**
 * Explicit refusals that decide an answer wherever they appear: "네 보내지
 * 마", "보내는 거 취소해", "아니 취소" are cancels, never sends. (Softer
 * refusals such as "아니" next to a send word are contradictory: ask again.)
 */
const EXPLICIT_CANCEL = [
  '취소',
  '보내지마',
  '보내지말',
  '안보내',
  '하지마',
  '그만',
  'cancel',
  "don'tsend",
  'dontsend',
  'きゃんせる',
  'cancelar',
  'noloenvies',
].map((w) => w.normalize('NFKC'));

/** "A or B": an utterance offering both answers decides nothing. */
const CHOICE_WORDS = ['또는', '아니면', '혹은', 'or', 'または', 'か', 'o'];

/** Hangul answer words for the jamo near-match ("보넨" -> 보내, "취쏘" -> 취소). */
const FUZZY_SEND = [
  '보내',
  '보낸',
  '보내요',
  '보내줘',
  '보내라',
  '보낼게',
  '보내자',
  '보내기',
  '전송',
  '전송해',
  '그래',
  '좋아',
  '오케이',
  '오키',
  '고고',
];
const FUZZY_CANCEL = [
  '취소',
  '취소해',
  '취소할게',
  '아니',
  '아니야',
  '아니요',
  '그만',
  '됐어',
  '하지마',
  '보내지마',
  '안보내',
];
/** Stems that make a Korean word an answer on their own ("보내드려" -> send). */
const SEND_STEMS = ['보내', '전송'];
const CANCEL_STEMS = ['취소'];

/**
 * Courtesy words and endings that may follow or surround an answer word
 * ("보내 주세요", "취소해 줘", "yes please", "送信してください", "sí, por favor").
 * They never make an answer on their own.
 */
const COURTESY = wordSet([
  '요',
  '야',
  '부탁해',
  '부탁합니다',
  '줘',
  '줘요',
  '주세요',
  '세요',
  '해',
  '해요',
  '해줘',
  '해줘요',
  '해주세요',
  '하세요',
  '합니다',
  'please',
  'it',
  'now',
  'して',
  'します',
  'ください',
  'してください',
  'お願いします',
  'です',
  'por favor',
  'lo',
  'ya',
]);

/** Hesitations around an answer ("어 보내", "um, yes", "えっと、はい", "eh, sí"). */
const FILLERS = wordSet([
  '어',
  '음',
  '아',
  '으음',
  '음음',
  '엄',
  '흠',
  '그',
  '저',
  '에',
  '자',
  '그럼',
  '그렇게',
  'uh',
  'um',
  'umm',
  'er',
  'erm',
  'hmm',
  'mm',
  'oh',
  'well',
  'so',
  'ok',
  'okay',
  'えー',
  'えっと',
  'ええと',
  'あの',
  'あのー',
  'うーん',
  'eh',
  'em',
  'este',
  'pues',
  'bueno',
  'vale',
]);

export const AGAIN_WORDS = {
  ko: ['다시'],
  en: ['again'],
  ja: ['もう一度'],
  es: ['otra vez', 'de nuevo'],
} as const satisfies Record<VoiceLang, readonly string[]>;

export const ALLOW_WORDS = {
  ko: ['허용'],
  en: ['allow'],
  ja: ['許可'],
  es: ['permitir'],
} as const satisfies Record<VoiceLang, readonly string[]>;

export const DENY_WORDS = {
  ko: ['거절'],
  en: ['deny'],
  ja: ['拒否'],
  es: ['rechazar'],
} as const satisfies Record<VoiceLang, readonly string[]>;

const all = (table: Record<VoiceLang, readonly string[]>, extra: readonly string[] = []) =>
  wordSet([...Object.values(table).flat(), ...extra]);
const SEND = all(SEND_WORDS, SEND_VARIANTS);
const CANCEL = all(CANCEL_WORDS, CANCEL_VARIANTS);
const AGAIN = all(AGAIN_WORDS);
const ALLOW = all(ALLOW_WORDS);
const DENY = all(DENY_WORDS);

/**
 * Whether `s` (normalized, no spaces) is made entirely of `words`, courtesy
 * words and fillers, with at least one of `words`. Recognizers do not always
 * put spaces between words ("네보내주세요", "はい送信"), so this segments the
 * text rather than its space-separated tokens.
 */
function madeOf(s: string, words: ReadonlySet<string>): boolean {
  if (!s) return false;
  const pieces = [...words, ...COURTESY, ...FILLERS];
  // reach[i]: 0 = unreachable, 1 = reachable without an answer word yet, 2 = with one.
  const reach = new Array<number>(s.length + 1).fill(0);
  reach[0] = 1;
  for (let i = 0; i < s.length; i += 1) {
    const from = reach[i] ?? 0;
    if (!from) continue;
    for (const piece of pieces) {
      if (!piece || !s.startsWith(piece, i)) continue;
      const to = words.has(piece) ? 2 : from;
      const end = i + piece.length;
      reach[end] = Math.max(reach[end] ?? 0, to);
    }
  }
  return reach[s.length] === 2;
}

export type SendAnswer = 'send' | 'cancel' | 'again' | 'unclear';

/** Which rule decided an answer (logged; never the words). */
export type AnswerRule = 'exact' | 'fuzzy' | 'negation' | 'none';

/** Hangul syllables of a word. */
const syllables = (w: string) => Array.from(w).filter(isHangulSyllable).length;

/**
 * The closest answer to one Korean word by jamo distance: at most 1 for
 * words of two syllables, 2 for longer ones; one-syllable words ("응",
 * "노") only match exactly. Stems decide too ("보내드려", "취소요").
 */
function fuzzyAnswer(word: string): 'send' | 'cancel' | null {
  if (!/^\p{Script=Hangul}+$/u.test(word)) return null;
  if (CANCEL_STEMS.some((st) => word.startsWith(st))) return 'cancel';
  if (SEND_STEMS.some((st) => word.startsWith(st)) && !/^보내지/u.test(word)) return 'send';
  const best = (words: readonly string[]) =>
    Math.min(
      ...words.map((w) => {
        const n = Math.max(syllables(w), syllables(word));
        if (n < 2) return Infinity;
        const d = jamoDistance(word, w);
        return d <= (n === 2 ? 1 : 2) ? d : Infinity;
      }),
    );
  const send = best(FUZZY_SEND);
  const cancel = best(FUZZY_CANCEL);
  if (send === Infinity && cancel === Infinity) return null;
  if (send === cancel) return null;
  return send < cancel ? 'send' : 'cancel';
}

/**
 * The answer to "보낼까요?" and the rule that decided it:
 *
 * 1. `negation`: an explicit refusal anywhere ("보내지 마", "보내는 거
 *    취소해", "네 아니 취소") is a cancel, even next to a send word: when in
 *    doubt nothing is sent.
 * 2. `exact`: the whole utterance is one answer's words with fillers and
 *    courtesy words around them ("어 보내", "네 보내주세요", "취소해 줘").
 * 3. `fuzzy`: every remaining Korean word is near one answer by jamo
 *    distance or stem ("보넨", "부내", "보낸다", "취쏘"); other words or
 *    a mix of answers make it unclear.
 * 4. Anything else, or a mix ("네 아니요", "보낼까 말까"), is `unclear`
 *    (the caller asks again).
 */
export function classifySendAnswerRule(heard: string): { answer: SendAnswer; rule: AnswerRule } {
  // Fillers or courtesy words alone ("음", "please") match no answer.
  const s = normalizeAnswer(heard);
  if (!s) return { answer: 'unclear', rule: 'none' };
  // "응 또는 취소" (the prompt said back) offers both: no answer.
  if (
    CHOICE_WORDS.some((w) =>
      foldAnswer(heard)
        .split(/[\s\p{P}]+/u)
        .includes(w),
    )
  ) {
    return { answer: 'unclear', rule: 'none' };
  }
  if (EXPLICIT_CANCEL.some((w) => s.includes(w))) {
    return { answer: 'cancel', rule: madeOf(s, CANCEL) ? 'exact' : 'negation' };
  }
  const matches: SendAnswer[] = [];
  if (madeOf(s, SEND)) matches.push('send');
  if (madeOf(s, CANCEL)) matches.push('cancel');
  if (madeOf(s, AGAIN)) matches.push('again');
  if (matches.length === 1) return { answer: matches[0]!, rule: 'exact' };
  if (matches.length > 1) return { answer: 'unclear', rule: 'none' };
  // Word by word: fillers and courtesy words drop out, the rest must agree.
  const words = foldAnswer(heard)
    .split(/[\s\p{P}\p{S}]+/u)
    .filter((w) => w && !FILLERS.has(w) && !COURTESY.has(w));
  const found = new Set<string>();
  for (const w of words) {
    const exact = SEND.has(w) ? 'send' : CANCEL.has(w) ? 'cancel' : null;
    const a = exact ?? fuzzyAnswer(w);
    if (!a) return { answer: 'unclear', rule: 'none' };
    found.add(a);
  }
  if (found.size === 1) return { answer: [...found][0] as SendAnswer, rule: 'fuzzy' };
  return { answer: 'unclear', rule: 'none' };
}

/** The answer to "보낼까요?" (see `classifySendAnswerRule`). */
export function classifySendAnswer(heard: string): SendAnswer {
  return classifySendAnswerRule(heard).answer;
}

/** STT prompt of the answer capture: a bias toward the answer words. */
export const ANSWER_PROMPTS: Record<VoiceLang, string> = {
  ko: '보내 취소 응 네 아니',
  en: 'send, cancel, yes, no',
  ja: '送信、キャンセル、はい、いいえ',
  es: 'enviar, cancelar, sí, no',
};

export type DecisionAnswer = 'allow' | 'deny' | 'stop' | 'unclear';

/**
 * The answer to "허용할까요?": only 허용 / 거절 (and their equivalents) answer;
 * a cancel word stops listening without answering. "응" or "yes" is NOT an
 * approval: a decision needs the explicit word.
 */
export function classifyDecisionAnswer(heard: string): DecisionAnswer {
  const word = normalizeAnswer(heard);
  if (!word) return 'unclear';
  if (ALLOW.has(word)) return 'allow';
  if (DENY.has(word)) return 'deny';
  if (CANCEL.has(word)) return 'stop';
  return 'unclear';
}

// ------------------------------------------------------------------- prompts

/** Longest command read out in a decision; longer commands are cut with "…". */
export const COMMAND_READ_CHARS = 120;
/** A read-back says about this many words of the text… */
export const READ_BACK_WORDS = 8;
/** …and at most this many characters, then "…". */
export const READ_BACK_CHARS = 40;

const TRAILING_STOP = /[.。!?！？]+$/u;

/** `text` on one line, cut to `max` characters (whole code points) with "…". */
export function excerpt(text: string, max = COMMAND_READ_CHARS): string {
  const flat = text.replace(/\s+/g, ' ').trim();
  const chars = Array.from(flat);
  if (chars.length <= max) return flat.replace(TRAILING_STOP, '');
  return `${chars.slice(0, max).join('').trimEnd()}…`;
}

/**
 * The beginning of `text` for a read-back: the first `READ_BACK_WORDS` words,
 * at most `READ_BACK_CHARS` characters (cut at a space when there is one in
 * the second half), and "…" when anything was left out.
 */
export function readBackExcerpt(text: string): string {
  const flat = text.replace(/\s+/g, ' ').trim();
  let head = flat.split(' ').slice(0, READ_BACK_WORDS).join(' ');
  const chars = Array.from(head);
  if (chars.length > READ_BACK_CHARS) {
    const cut = chars.slice(0, READ_BACK_CHARS).join('');
    const space = cut.lastIndexOf(' ');
    head = space >= cut.length / 2 ? cut.slice(0, space) : cut;
  }
  if (head.length >= flat.length) return flat.replace(TRAILING_STOP, '');
  return `${head.trimEnd().replace(/[\s,.。、!?！？;:]+$/u, '')}…`;
}

const READ_BACK: Record<VoiceLang, (text: string) => string> = {
  ko: (s) => `보낼 내용: ${s}. 보낼까요?`,
  en: (s) => `Message: ${s}. Send it?`,
  ja: (s) => `送信する内容: ${s}。送信しますか？`,
  es: (s) => `Mensaje: ${s}. ¿Lo envío?`,
};

const READ_BACK_TO: Record<VoiceLang, (name: string, text: string) => string> = {
  ko: (n, s) => `${normalizeKoreanNumbers(n, 'ko')}에 ${s}. 보낼까요?`,
  en: (n, s) => `To ${n}: ${s}. Send it?`,
  ja: (n, s) => `${n}に、${s}。送信しますか？`,
  es: (n, s) => `A ${n}: ${s}. ¿Lo envío?`,
};

export interface ReadBackOptions {
  /** The target session ("3번 세션" is read "삼번 세션에 …" in Korean). */
  sessionName?: string | null;
  /** Read the whole text instead of its beginning (setting, default off). */
  full?: boolean;
}

/**
 * "삼번 세션에 <the first words>… 보낼까요?": the target and the beginning
 * of the text (`readBackExcerpt`), or the whole text with `full`.
 */
export function readBackText(text: string, lang: VoiceLang, opts: ReadBackOptions = {}): string {
  const s = opts.full
    ? text.replace(/\s+/g, ' ').trim().replace(TRAILING_STOP, '')
    : readBackExcerpt(text);
  const name = opts.sessionName?.trim();
  const spoken = name ? READ_BACK_TO[lang](name, s) : READ_BACK[lang](s);
  // After "…" a period would be read as a second pause.
  return s.endsWith('…') ? spoken.replace('….', '…').replace('…。', '…') : spoken;
}

export const PHRASES: Record<
  VoiceLang,
  {
    sendRetry: string;
    /** Speech was heard but not understood. */
    sendChoices: string;
    sent: string;
    cancelled: string;
    kept: string;
    keptAttachments: string;
    keptChanged: string;
    keptFailed: string;
    decisionRetry: string;
    approved: string;
    denied: string;
    checkScreen: string;
    changed: string;
  }
> = {
  ko: {
    sendRetry: '보낼까요? 응 또는 취소로 답해 주세요.',
    sendChoices: '보내기 또는 취소로 말씀해 주세요.',
    sent: '보냈습니다.',
    cancelled: '취소했습니다.',
    kept: '보내지 않았어요. 내용은 입력창에 남아 있어요.',
    keptAttachments: '첨부 파일을 올리는 중이라 보내지 않았어요. 내용은 입력창에 남아 있어요.',
    keptChanged: '입력창의 내용이 바뀌어서 보내지 않았어요. 내용은 입력창에 남아 있어요.',
    keptFailed: '전송에 실패해서 보내지 않았어요. 내용은 입력창에 남아 있어요.',
    decisionRetry: '허용 또는 거절로 답해 주세요. 허용할까요?',
    approved: '허용했습니다.',
    denied: '거절했습니다.',
    checkScreen: '화면에서 확인해 주세요.',
    changed: '요청이 바뀌었습니다. 화면에서 확인해 주세요.',
  },
  en: {
    sendRetry: 'Send it? Say yes or cancel.',
    sendChoices: 'Please say send or cancel.',
    sent: 'Sent.',
    cancelled: 'Cancelled.',
    kept: 'Not sent. The text is still in the box.',
    keptAttachments: 'Not sent: attachments are still uploading. The text is still in the box.',
    keptChanged: 'Not sent: the text in the box changed. The text is still in the box.',
    keptFailed: 'Not sent: sending failed. The text is still in the box.',
    decisionRetry: 'Say allow or deny. Allow it?',
    approved: 'Allowed.',
    denied: 'Denied.',
    checkScreen: 'Please check the screen.',
    changed: 'The request changed. Please check the screen.',
  },
  ja: {
    sendRetry: '送信しますか？はい、または、キャンセルで答えてください。',
    sendChoices: '送信、または、キャンセルと言ってください。',
    sent: '送信しました。',
    cancelled: 'キャンセルしました。',
    kept: '送信していません。内容は入力欄に残っています。',
    keptAttachments:
      '添付ファイルをアップロード中のため送信していません。内容は入力欄に残っています。',
    keptChanged: '入力欄の内容が変わったため送信していません。内容は入力欄に残っています。',
    keptFailed: '送信に失敗したため送信していません。内容は入力欄に残っています。',
    decisionRetry: '許可、または、拒否で答えてください。許可しますか？',
    approved: '許可しました。',
    denied: '拒否しました。',
    checkScreen: '画面で確認してください。',
    changed: 'リクエストが変わりました。画面で確認してください。',
  },
  es: {
    sendRetry: '¿Lo envío? Di sí o cancelar.',
    sendChoices: 'Di enviar o cancelar, por favor.',
    sent: 'Enviado.',
    cancelled: 'Cancelado.',
    kept: 'No lo envié. El texto sigue en el cuadro.',
    keptAttachments:
      'No lo envié: los adjuntos aún se están subiendo. El texto sigue en el cuadro.',
    keptChanged: 'No lo envié: el texto del cuadro cambió. El texto sigue en el cuadro.',
    keptFailed: 'No lo envié: falló el envío. El texto sigue en el cuadro.',
    decisionRetry: 'Di permitir o rechazar. ¿Lo permito?',
    approved: 'Permitido.',
    denied: 'Rechazado.',
    checkScreen: 'Revísalo en la pantalla.',
    changed: 'La solicitud cambió. Revísala en la pantalla.',
  },
};

/** Final consonant of the Sino-Korean reading of each digit (영, 일, 삼, 육, 칠, 팔 have one). */
const DIGIT_BATCHIM = [true, true, false, true, false, false, true, true, true, false];

/** Korean subject particle after `word`: 이 after a final consonant, else 가. */
export function subjectParticle(word: string): '이' | '가' {
  const last = Array.from(word.trim()).pop() ?? '';
  const code = last.codePointAt(0) ?? 0;
  if (code >= 0xac00 && code <= 0xd7a3) return (code - 0xac00) % 28 === 0 ? '가' : '이';
  if (/\d/.test(last)) return DIGIT_BATCHIM[Number(last)] ? '이' : '가';
  if (/[aeiouy]/i.test(last)) return '가';
  return '이';
}

/**
 * "오번 세션이 pnpm build 실행을 허용해 달라고 합니다. 허용할까요?" (Korean
 * number reading for "5번 세션"). The command is cut like a read-back.
 */
export function decisionAskText(name: string, command: string, lang: VoiceLang): string {
  const cmd = excerpt(command);
  switch (lang) {
    case 'ko': {
      const spoken = normalizeKoreanNumbers(name, 'ko');
      return `${spoken}${subjectParticle(spoken)} ${cmd} 실행을 허용해 달라고 합니다. 허용할까요?`;
    }
    case 'ja':
      return `${name}が ${cmd} の実行の許可を求めています。許可しますか？`;
    case 'es':
      return `${name} pide permiso para ejecutar ${cmd}. ¿Lo permito?`;
    default:
      return `${name} asks to run ${cmd}. Allow it?`;
  }
}

/** "<name>에 결정이 필요합니다. 화면에서 확인해 주세요." (never the command). */
export function decisionScreenText(name: string, lang: VoiceLang): string {
  switch (lang) {
    case 'ko':
      return `${normalizeKoreanNumbers(name, 'ko')}에 결정이 필요합니다. 화면에서 확인해 주세요.`;
    case 'ja':
      return `${name}で判断が必要です。画面で確認してください。`;
    case 'es':
      return `${name} necesita una decisión. Revísala en la pantalla.`;
    default:
      return `${name} needs a decision. Please check the screen.`;
  }
}

/**
 * A question form: "<name>에서 질문이 왔습니다. <question>. 화면에서 답해
 * 주세요." Only read out; the answers are picked on the screen.
 */
export function questionScreenText(name: string, question: string, lang: VoiceLang): string {
  // `excerpt` drops the question's own "?" (it is read as a statement).
  const q = excerpt(question);
  switch (lang) {
    case 'ko':
      return `${normalizeKoreanNumbers(name, 'ko')}에서 질문이 왔습니다. ${q}. 화면에서 답해 주세요.`;
    case 'ja':
      return `${name}から質問があります。${q}。画面で回答してください。`;
    case 'es':
      return `${name} hace una pregunta: ${q}. Respóndela en la pantalla.`;
    default:
      return `${name} asks a question: ${q}. Please answer on the screen.`;
  }
}

// ------------------------------------------------------------ decision options

const YES_LABELS = wordSet([
  'yes',
  'allow',
  '예',
  '네',
  '허용',
  'はい',
  '許可',
  'sí',
  'si',
  'permitir',
  // A one-time approval is the same plain "yes" (never "always" or "for the
  // session"); a spoken answer is still taken only for commands the PC
  // classifies as safe to approve by voice.
  'allow once',
  'yes, once',
  'yes, allow once',
  'allow this time',
  '이번만 허용',
  '한 번만 허용',
  '한 번 허용',
  '今回のみ許可',
  '一度だけ許可',
  'permitir una vez',
  'sí, una vez',
  'permitir esta vez',
]);
const NO_LABELS = wordSet(['no', 'deny', '아니요', '아니오', '거절', 'いいえ', '拒否', 'rechazar']);
/** "No, tell the agent what to do instead": a plain refusal with an explanation. */
const NO_WITH_REASON = /^\s*no\s*[,.—–-]/i;

export interface VoiceChoices {
  allow: string;
  deny: string;
}

/**
 * The plain yes and plain no options of a decision, the only ones a voice
 * answer may pick. `null` (screen only) unless there is exactly one of each;
 * options such as "Yes, and don't ask again" are never picked by voice.
 */
export function voiceChoices(decision: Decision): VoiceChoices | null {
  const yes = decision.options.filter((o) => YES_LABELS.has(normalizeAnswer(o.label)));
  const no = decision.options.filter(
    (o) => NO_LABELS.has(normalizeAnswer(o.label)) || NO_WITH_REASON.test(o.label),
  );
  if (yes.length !== 1 || no.length !== 1) return null;
  return { allow: yes[0]!.id, deny: no[0]!.id };
}

/** Same id, question and options: a voice answer still answers what was read out. */
export function sameDecision(a: Decision | undefined, b: Decision | undefined): boolean {
  if (!a || !b) return false;
  return (
    a.id === b.id &&
    a.prompt === b.prompt &&
    a.options.length === b.options.length &&
    a.options.every((o, i) => o.id === b.options[i]?.id && o.label === b.options[i]?.label)
  );
}

// --------------------------------------------------------------------- flows

/** Speech I/O of the flows (injected; mocked in tests). */
export interface VoiceIO {
  /** Speaks `text`; resolves once it has finished or was stopped by `signal`. Never rejects. */
  speak(text: string, signal: AbortSignal): Promise<void>;
  /**
   * A short answer capture (microphone + STT, with a silence timeout).
   * Resolves with what was heard, or `null` on silence, timeout or failure.
   */
  listen(signal: AbortSignal): Promise<string | null>;
}

/**
 * Visual states of a read-back (확인 대기 / 듣는 중 / 보냄 / 취소됨 / 보내지
 * 않음: the text stays in the box).
 */
export type SendStatus = 'confirming' | 'listening' | 'sent' | 'cancelled' | 'kept';

/**
 * - `send` / `again`: the answer was a send / again word.
 * - `cancel`: an explicit cancel word (`answer`) or a tap (`interrupted`).
 * - `kept`: no clear answer after every ask (silence or unclear words); the
 *   text is left in the box unsent. Never a cancel, never discarded.
 */
export type ConfirmResult =
  | { outcome: 'send' }
  | { outcome: 'again' }
  | { outcome: 'cancel'; reason: 'answer' | 'interrupted' }
  | { outcome: 'kept'; reason: 'timeout' | 'unclear' };

/** The question plus this many re-asks ("보낼까요? 응 또는 취소로 답해 주세요."). */
export const SEND_RETRIES = 2;

export interface ConfirmSendOptions extends ReadBackOptions {
  text: string;
  lang: VoiceLang;
  io: VoiceIO;
  signal: AbortSignal;
  onStatus(status: SendStatus): void;
}

/**
 * Reads the beginning of `text` back and listens for the answer. Never
 * resolves with `send` unless the answer was made of send words, and never
 * with `cancel` unless it was made of cancel words (or a tap stopped it).
 * Silence and unclear answers ask again up to `SEND_RETRIES` times, then
 * resolve `kept`. The caller sends, shows `cancelled` or shows `kept`.
 */
export async function confirmSend(o: ConfirmSendOptions): Promise<ConfirmResult> {
  const interrupted = { outcome: 'cancel', reason: 'interrupted' } as const;
  let prompt = readBackText(o.text, o.lang, { sessionName: o.sessionName, full: o.full });
  for (let attempt = 0; ; attempt += 1) {
    if (o.signal.aborted) return interrupted;
    o.onStatus('confirming');
    await o.io.speak(prompt, o.signal);
    if (o.signal.aborted) return interrupted;
    o.onStatus('listening');
    const heard = await o.io.listen(o.signal);
    if (o.signal.aborted) return interrupted;
    const classified = heard ? classifySendAnswerRule(heard) : null;
    const answer = heard === null ? null : (classified?.answer ?? 'unclear');
    // Counts and labels only: never what was said.
    diag('handsfree.confirm.answer', {
      attempt,
      answer: answer ?? 'none',
      rule: classified?.rule ?? 'none',
      heardChars: heard?.length ?? 0,
      speechNoText: heard === '',
    });
    if (answer === 'send') return { outcome: 'send' };
    if (answer === 'again') return { outcome: 'again' };
    if (answer === 'cancel') return { outcome: 'cancel', reason: 'answer' };
    if (attempt >= SEND_RETRIES) {
      return { outcome: 'kept', reason: answer === null ? 'timeout' : 'unclear' };
    }
    // Speech came but no words: the two choices, shortly.
    prompt = heard === '' ? PHRASES[o.lang].sendChoices : PHRASES[o.lang].sendRetry;
  }
}

/** Visual states of a spoken decision. */
export type DecisionVoiceStatus =
  | 'checking'
  | 'reading'
  | 'listening'
  | 'approved'
  | 'denied'
  | 'screen'
  | 'changed'
  | 'stopped'
  | 'failed';

export type ScreenReason =
  'no_command' | 'options' | 'caution' | 'dangerous' | 'unclassified' | 'question';

export type SpokenDecisionResult =
  | { result: 'approved' | 'denied'; optionId: string }
  | { result: 'screen'; reason: ScreenReason }
  | { result: 'changed' }
  | { result: 'stopped'; reason: 'unclear' | 'timeout' | 'answer' | 'interrupted' }
  | { result: 'failed'; error: unknown };

export interface SpokenDecisionOptions {
  decision: Decision;
  sessionName: string;
  lang: VoiceLang;
  io: VoiceIO;
  signal: AbortSignal;
  /** The server's classification (`classify_command`). */
  /** `safe`: on the server's list of what a spoken approval may run. */
  classify(command: string): Promise<DangerLevel | 'unclassified'>;
  /** The decision pending NOW (the latest state). */
  current(): Decision | undefined;
  /** Sends exactly what a tap on the option sends. */
  choose(optionId: string): Promise<void>;
  onStatus(status: DecisionVoiceStatus): void;
}

/**
 * Reads a pending decision and takes a spoken 허용 / 거절 for `safe`
 * commands with plain yes / no options. `caution`, `dangerous`, unknown
 * classification, no command or other options: "check the screen", and no
 * voice answer is taken at all. The answer is only sent while the pending
 * decision is still exactly the one that was read out.
 */
export async function spokenDecision(o: SpokenDecisionOptions): Promise<SpokenDecisionResult> {
  const { decision, lang, io, signal } = o;
  const interrupted = { result: 'stopped', reason: 'interrupted' } as const;
  const phrases = PHRASES[lang];
  o.onStatus('checking');
  const toScreen = async (reason: ScreenReason): Promise<SpokenDecisionResult> => {
    o.onStatus('screen');
    await io.speak(decisionScreenText(o.sessionName, lang), signal);
    return { result: 'screen', reason };
  };

  if (decision.questions?.length) {
    // A question form is read out and answered on the screen.
    o.onStatus('screen');
    const first = decision.questions[0]!;
    await io.speak(questionScreenText(o.sessionName, first.question, lang), signal);
    return { result: 'screen', reason: 'question' };
  }
  // What the agent's hook reported, never the text on the screen.
  const command = decision.command ?? null;
  const choices = voiceChoices(decision);
  if (!command) return toScreen('no_command');
  if (!choices) return toScreen('options');
  let level: DangerLevel | 'unclassified';
  try {
    level = await o.classify(command);
  } catch {
    level = 'unclassified';
  }
  if (signal.aborted) return interrupted;
  if (!sameDecision(o.current(), decision)) {
    o.onStatus('changed');
    return { result: 'changed' };
  }
  if (level !== 'safe') return toScreen(level);

  let prompt = decisionAskText(o.sessionName, command, lang);
  for (let attempt = 0; ; attempt += 1) {
    o.onStatus('reading');
    await io.speak(prompt, signal);
    if (signal.aborted) return interrupted;
    o.onStatus('listening');
    const heard = await io.listen(signal);
    if (signal.aborted) return interrupted;
    const answer = heard === null ? null : classifyDecisionAnswer(heard);
    if (answer === null || answer === 'stop' || (answer === 'unclear' && attempt >= 1)) {
      o.onStatus('stopped');
      await io.speak(phrases.checkScreen, signal);
      return {
        result: 'stopped',
        reason: answer === null ? 'timeout' : answer === 'stop' ? 'answer' : 'unclear',
      };
    }
    if (answer === 'unclear') {
      prompt = phrases.decisionRetry;
      continue;
    }
    // The decision may have been answered elsewhere or replaced meanwhile.
    if (!sameDecision(o.current(), decision)) {
      o.onStatus('changed');
      await io.speak(phrases.changed, signal);
      return { result: 'changed' };
    }
    const optionId = answer === 'allow' ? choices.allow : choices.deny;
    try {
      await o.choose(optionId);
    } catch (error) {
      o.onStatus('failed');
      return { result: 'failed', error };
    }
    const result = answer === 'allow' ? 'approved' : 'denied';
    o.onStatus(result);
    void io.speak(result === 'approved' ? phrases.approved : phrases.denied, signal);
    return { result, optionId };
  }
}

// ------------------------------------------------------------ sharing the mic

/**
 * The background listener of the Voco wake flow holds the microphone while
 * nobody else needs it. Everyone else borrows it first (`borrowMic`): the
 * holder releases it (and the borrow waits for that, so a late release can
 * never cancel the borrower's recording) and resumes once every borrower has
 * given it back.
 */
export interface MicHolder {
  release(): Promise<void>;
  resume(): void;
}

let micHolder: MicHolder | null = null;
const micBorrowers = new Set<string>();
let borrowSeq = 0;

/** Registers the background listener; returns the unregistration. */
export function setMicHolder(holder: MicHolder): () => void {
  micHolder = holder;
  return () => {
    if (micHolder === holder) micHolder = null;
  };
}

/** Someone else is using the microphone or the voice now. */
export function micBorrowed(): boolean {
  return micBorrowers.size > 0;
}

/** Who holds the microphone now (diagnostics: why the wake word is not heard). */
export function micBorrowerKeys(): string[] {
  return [...micBorrowers];
}

/**
 * The composer is idle: a dictation borrow left behind (its end never
 * reached the effect that gives it back) is returned. Returns whether one was.
 */
export function releaseStaleDictation(): boolean {
  if (dictationActive || !micBorrowers.has(DICTATION_MIC)) return false;
  giveBack(DICTATION_MIC);
  return true;
}

function giveBack(key: string): void {
  if (micBorrowers.delete(key) && micBorrowers.size === 0) micHolder?.resume();
}

/**
 * Takes the microphone from the background listener (no-op without one).
 * Resolves once it is released; call the returned function to give it back.
 */
export async function borrowMic(key = `borrow:${(borrowSeq += 1)}`): Promise<() => void> {
  micBorrowers.add(key);
  try {
    await micHolder?.release();
  } catch {
    // Released as far as possible; the borrower's own start reports errors.
  }
  return () => giveBack(key);
}

let voiceTail: Promise<unknown> = Promise.resolve();

/**
 * Runs voice flows one at a time (one microphone, one speaker): a decision
 * that arrives during a read-back waits for it to end. Queued alerts and
 * summaries (`voiceQueue`) pause meanwhile and resume afterwards, and the
 * Voco listener releases the microphone (it is never open while the app
 * speaks or another flow listens).
 */
export function withVoice<T>(fn: () => Promise<T>): Promise<T> {
  const exclusive = () =>
    voiceQueue.exclusive(() => {
      const key = `voice:${(borrowSeq += 1)}`;
      micBorrowers.add(key);
      const run = async () => {
        try {
          return await fn();
        } finally {
          giveBack(key);
        }
      };
      // Without a listener holding the microphone the flow starts at once.
      if (!micHolder) return run();
      return micHolder.release().then(run, run);
    });
  const run = voiceTail.then(exclusive, exclusive);
  voiceTail = run.catch(() => {});
  return run;
}

/** Borrow key of the composer's own dictation. */
export const DICTATION_MIC = 'dictation';

let dictationActive = false;
let vocoActive = false;
/** Ends the voice queue's pause for the running dictation. */
let endVoiceHold: (() => void) | null = null;

/**
 * The composer is recording or transcribing (decisions are not read out
 * then). It also borrows the microphone from the Voco listener; the
 * composer awaits `borrowMic(DICTATION_MIC)` before it starts recording.
 * Queued voice waits until the dictation ends: on iOS playback takes the
 * input away from a running capture. Pauses nest, so a voice flow that runs
 * meanwhile (`withVoice`) neither waits for this one nor ends it.
 */
export function setDictationActive(active: boolean): void {
  dictationActive = active;
  if (active) {
    micBorrowers.add(DICTATION_MIC);
    if (!endVoiceHold) {
      void voiceQueue.exclusive(
        () =>
          new Promise<void>((resolve) => {
            endVoiceHold = resolve;
          }),
      );
    }
  } else {
    endVoiceHold?.();
    endVoiceHold = null;
    giveBack(DICTATION_MIC);
  }
}

/** The Voco flow is taking a message (decisions wait like for a dictation). */
export function setVocoActive(active: boolean): void {
  vocoActive = active;
}

export function isDictationActive(): boolean {
  return dictationActive || vocoActive;
}

/**
 * Stops a voice flow as soon as the person taps or presses a key anywhere:
 * the screen always wins over the voice. Returns the cleanup.
 */
export function interruptOnTap(controller: AbortController, target: EventTarget = document) {
  const stop = () => controller.abort();
  target.addEventListener('pointerdown', stop, true);
  target.addEventListener('keydown', stop, true);
  return () => {
    target.removeEventListener('pointerdown', stop, true);
    target.removeEventListener('keydown', stop, true);
  };
}
