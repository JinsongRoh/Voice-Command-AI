/**
 * The Voco wake flow of hands-free mode (v2):
 *
 * 1. Waiting (`waiting`): the microphone is open (`vocoListener.ts`) but only a
 *    short head of each utterance (`WAKE_HEAD_MS`) is transcribed, to look
 *    for the wake word. Without it, nothing is kept, shown or sent.
 * 2. "보코" (or a mishearing such as "보컬", isolated, see `wakeWord.ts`): a
 *    chime, a short "네" (yes) and Listening (`awake`). What follows is dictated into
 *    the composer, one utterance at a time, without the wake word.
 * 3. The message ends with a send phrase ("…, 보내줘 오버", "…, 오버",
 *    "…, 전송해"): the phrase is removed, "로저, Orbit 플랫폼 세션에 전송합니다" is said and
 *    the text is sent after a 3 s undo window (`undo`) in which "취소"
 *    recalls it. Nothing has left the device until the window is over, so
 *    the recall is real. Or it ends in a command form ("…고쳐줘") followed by
 *    2.5 s of silence: a short countdown with soft ticks (`countdown`), then
 *    the same send. Speaking again cancels the countdown (the user goes on);
 *    "취소" discards the dictation; a tap stops it and keeps the text.
 *
 *    Nothing is ever sent on a pause alone. The listener cuts a long
 *    dictation into pieces at short pauses, so each rule looks only at what
 *    the user really ended with: a message that stops on a particle or a
 *    connective ("…해당 내용을", "…불러 본 거고") is never sent by itself; a
 *    plain send command after other words ("…고쳐줘, 전송해") only counts
 *    after `TRAILING_SEND_SILENCE_MS` more silence (if the user goes on it
 *    was part of the sentence and stays in the text), and never after an
 *    object ("…내용을, 전송해" is content); new dictation in the undo window
 *    (not "취소", not a yes) takes the message back and keeps listening.
 * 4. "취소" discards the dictation, "다시" restarts it, "보코, 3번 세션" or
 *    "보코, Orbit 플랫폼" switches the target session (by number or by name;
 *    "어느 세션인가요?" when the name fits two), "보코, 현황" plays the
 *    status briefing.
 *
 * Sessions are named in replies by the name the user gave them, not by
 * their card number (see `sessionNames.ts`).
 * 5. No speech for `IDLE_MS` after the last words: back to Waiting, quietly
 *    (the text stays in the box).
 *
 * With the read-back option the old confirmation ("…보낼까요?") replaces the
 * undo window. Wake words and send phrases never reach the agent.
 *
 * Pure apart from the injected `VocoDeps` (mocked in tests).
 */
import { diag } from '../diag/log';
import { joinText } from './dictation';
import { classifySendAnswer, normalizeAnswer, PHRASES, type VoiceLang } from './handsFree';
import type { VocoConfirm } from '../state/settings';
import { isBriefingPhrase } from './briefingPhrases';
import { normalizeKoreanNumbers } from './koreanNumbers';
import type { Transcript } from './transcribe';
import type { HeardUtterance } from './vocoListener';
import { matchSendPhrase, matchWakeWord, type SendMatch, type WakeMatch } from './wakeWord';

/** Speech transcribed per utterance while waiting for the wake word. */
export const WAKE_HEAD_MS = 2000;
/**
 * While waiting, an utterance whose check would start this long after it
 * ended, with newer speech queued behind it, is skipped unless it is short
 * enough to be a call: on a slow link, nearby talk must not pile up wake
 * checks ahead of the real "보코".
 */
export const STALE_WAKE_MS = 4000;
/** Utterances shorter than this are call-like and always checked. */
export const CALL_LIKE_MS = 1500;
/** Silence after the wake word (or the last words) before going back to Waiting. */
export const IDLE_MS = 8000;
/** Silence after a command-form ending before the scheduled send starts. */
export const SCHEDULE_SILENCE_MS = 2500;
/**
 * Silence after a plain send command at the end of a longer utterance
 * ("…고쳐줘, 전송해") before it counts: speech within it means the words were
 * the start of a longer sentence, cut by a thinking pause.
 */
export const TRAILING_SEND_SILENCE_MS = 2500;
/** Undo window after a spoken send phrase. */
export const UNDO_MS = 3000;
/** Wait after the undo window for an utterance that started in it ("취소"?)… */
export const HOLD_MAX_MS = 4000;
/** …extended while the user is still speaking or it is being transcribed, up to this. */
export const HOLD_SPEAKING_MAX_MS = 45_000;
/** "취소" this soon after a send gets the honest "already sent". */
const TOO_LATE_MS = 10_000;

export type VocoPhase = 'waiting' | 'awake' | 'countdown' | 'undo';
/**
 * `queued`: the send waits for attachments that are still uploading.
 * `missed`: a short call-like utterance that did not read as the wake word
 * ("Didn't catch 'Voco'"), so the person knows to say it again.
 */
export type VocoResult = 'sent' | 'cancelled' | 'kept' | 'queued' | 'missed';

export interface VocoView {
  phase: VocoPhase;
  /** Waiting and speech is coming in now (the chip shows it hears). */
  hearing?: boolean;
  /** Seconds left in `countdown` / `undo`. */
  seconds?: number;
  /** How the last message ended (shown briefly while back in Waiting). */
  result?: VocoResult;
}

export type VocoChime = 'wake' | 'tick' | 'sent' | 'cancel' | 'missed';

export interface VocoSettings {
  confirm: VocoConfirm;
  /** Schedule the send after a command-form ending and silence. */
  autoSend: boolean;
  countdownMs: number;
  /** User wake words and send phrases (added to the built-in ones). */
  wakeWords: readonly string[];
  sendPhrases: readonly string[];
}

/** A session the user named ("3번 세션"). */
export interface SessionRef {
  number: number;
}

export interface FoundSession {
  /** Spoken label ("Orbit Platform 세션"). */
  label: string;
  /** Makes it the target; `carry` is dictated text to take along. */
  select(carry: string): void;
}

export interface VocoTimers {
  set(fn: () => void, ms: number): () => void;
  now(): number;
}

export interface VocoDeps {
  lang(): VoiceLang;
  settings(): VocoSettings;
  /** `wake`: a short wake check (no context); `dictation`: a whole utterance. */
  transcribe(audio: Blob, purpose: 'wake' | 'dictation'): Promise<Transcript>;
  /** Speaks (the microphone is closed meanwhile); resolves when done. */
  speak(text: string): Promise<void>;
  chime(kind: VocoChime): void;
  /** The composer text now. */
  text(): string;
  setText(text: string): void;
  /** Sends exactly `text`; `true` once delivered. */
  send(text: string): Promise<boolean>;
  /**
   * A send now waits for uploads (the composer's queued send): the flow goes
   * back to Waiting at once and keeps listening while it waits.
   */
  queued?(): boolean;
  /** Drops the send that waits for uploads. */
  cancelQueued?(): void;
  /** The old read-back confirmation, which sends or keeps the text itself. */
  readBack(text: string): Promise<void>;
  /** Spoken label of the current target ("Orbit Platform 세션"). */
  sessionLabel(): string;
  findSession(ref: SessionRef): FoundSession | null;
  /** The session a spoken name means ("오빗 플랫폼"); `ambiguous` when it fits several. */
  findSessionByName?(name: string): FoundSession | 'ambiguous' | null;
  /** The short "네" after the wake word, as fast as possible (default: `speak`). */
  ack?(text: string): Promise<void>;
  briefing(): void;
  onView(view: VocoView): void;
  timers?: VocoTimers;
}

// ------------------------------------------------------------------ phrases

export const VOCO_PHRASES: Record<
  VoiceLang,
  {
    ack: string;
    /** Before the undo window: the message is about to go. */
    roger(label: string): string;
    /** After a scheduled send went out. */
    rogerSent(label: string): string;
    rogerSwitch(label: string): string;
    whichSession: string;
    recalled: string;
    tooLate: string;
    noSession: string;
    session(n: number): string;
    /** The only answer to the wake word while the app is locked. */
    locked: string;
  }
> = {
  ko: {
    ack: '네',
    roger: (l) => `로저, ${l}에 전송합니다`,
    rogerSent: (l) => `로저, ${l}에 전송했습니다`,
    rogerSwitch: (l) => `로저, ${l}`,
    whichSession: '어느 세션인가요?',
    recalled: '취소했어요. 보내지 않았어요.',
    tooLate: '이미 보내서 취소할 수 없어요.',
    noSession: '그 세션을 찾지 못했어요.',
    session: (n) => `${n}번 세션`,
    locked: '잠겨 있습니다. 잠금을 풀어 주세요.',
  },
  en: {
    ack: 'Yes?',
    roger: (l) => `Roger, sending to ${l}.`,
    rogerSent: (l) => `Roger, sent to ${l}.`,
    rogerSwitch: (l) => `Roger, ${l}.`,
    whichSession: 'Which session?',
    recalled: 'Cancelled. Nothing was sent.',
    tooLate: 'It was already sent and cannot be recalled.',
    noSession: "I can't find that session.",
    session: (n) => `session ${n}`,
    locked: 'The app is locked. Please unlock it first.',
  },
  ja: {
    ack: 'はい',
    roger: (l) => `ラジャー、${l}に送信します`,
    rogerSent: (l) => `ラジャー、${l}に送信しました`,
    rogerSwitch: (l) => `ラジャー、${l}`,
    whichSession: 'どのセッションですか？',
    recalled: 'キャンセルしました。送信していません。',
    tooLate: 'すでに送信したため、取り消せません。',
    noSession: 'そのセッションが見つかりません。',
    session: (n) => `セッション${n}`,
    locked: 'ロックされています。ロックを解除してください。',
  },
  es: {
    ack: '¿Sí?',
    roger: (l) => `Recibido, envío a ${l}.`,
    rogerSent: (l) => `Recibido, enviado a ${l}.`,
    rogerSwitch: (l) => `Recibido, ${l}.`,
    whichSession: '¿Qué sesión?',
    recalled: 'Cancelado. No se envió nada.',
    tooLate: 'Ya se envió y no se puede recuperar.',
    noSession: 'No encuentro esa sesión.',
    session: (n) => `sesión ${n}`,
    locked: 'La app está bloqueada. Desbloquéala primero.',
  },
};

// ----------------------------------------------------------- spoken commands

const KO_DIGITS: Record<string, number> = {
  일: 1,
  이: 2,
  삼: 3,
  사: 4,
  오: 5,
  육: 6,
  칠: 7,
  팔: 8,
  구: 9,
};
const WORD_NUMBERS: Record<string, number> = {
  one: 1,
  two: 2,
  three: 3,
  four: 4,
  five: 5,
  six: 6,
  seven: 7,
  eight: 8,
  nine: 9,
  ten: 10,
  uno: 1,
  dos: 2,
  tres: 3,
  cuatro: 4,
  cinco: 5,
  seis: 6,
  siete: 7,
  ocho: 8,
  nueve: 9,
  diez: 10,
};

/** "3", "삼", "십이", "three" -> the number (1..99), else null. */
export function parseSpokenNumber(word: string): number | null {
  const w = word.trim().toLowerCase();
  if (/^\d{1,3}$/.test(w)) return Number(w) || null;
  if (w in WORD_NUMBERS) return WORD_NUMBERS[w]!;
  const m = /^([일이삼사오육칠팔구])?(십)?([일이삼사오육칠팔구])?$/u.exec(w);
  if (!m || !w) return null;
  const [, a, ten, b] = m;
  if (ten) return (a ? KO_DIGITS[a]! : 1) * 10 + (b ? KO_DIGITS[b]! : 0);
  if (a && b) return null;
  return a ? KO_DIGITS[a]! : null;
}

const NUM = '(\\d{1,3}|[일이삼사오육칠팔구십]{1,3}|[a-z]+)';
const SESSION_PATTERNS = [
  // "3번 세션", "3번 세션으로", "삼번 세션으로 이동", "3번으로 가" (go to session 3)
  new RegExp(
    `^${NUM}\\s*번\\s*(?:세션)?\\s*(?:으로|로)?\\s*(?:가|가줘|가자|이동|이동해|이동해줘|전환|전환해|전환해줘|바꿔|바꿔줘)?$`,
    'u',
  ),
  new RegExp(`^세션\\s*${NUM}\\s*(?:번)?\\s*(?:으로|로)?\\s*(?:가|이동|이동해|전환|바꿔)?$`, 'u'),
  new RegExp(`^(?:go to |switch to )?session (?:number )?${NUM}$`, 'u'),
  new RegExp(`^セッション\\s*${NUM}\\s*(?:番)?\\s*(?:に|へ)?$`, 'u'),
  new RegExp(`^(?:ir a |cambiar a )?(?:la )?sesion (?:numero )?${NUM}$`, 'u'),
];

/** "3번 세션", "session 3", 「セッション3」, "sesión 3" -> the session number. */
export function parseSessionRef(text: string): SessionRef | null {
  const flat = text
    .normalize('NFKC')
    .toLowerCase()
    .normalize('NFD')
    .replace(/[̀-ͯ]/g, '')
    .normalize('NFC')
    .replace(/[.,!?。、！？…]/gu, ' ')
    .replace(/\s+/g, ' ')
    .trim();
  for (const p of SESSION_PATTERNS) {
    const m = p.exec(flat);
    const n = m ? parseSpokenNumber(m[1]!) : null;
    if (n) return { number: n };
  }
  return null;
}

/** Briefing requests after the wake word ("보코, 현황"). */
const BRIEFING_WORDS = new Set(
  ['현황', '상태', '브리핑', 'status', 'briefing', '状況', 'estado'].map(normalizeAnswer),
);

const CANCEL_CORE = /취소|cancel|きゃんせる|cancelar/u;

export type VoiceCommand =
  | { kind: 'empty' }
  | { kind: 'cancel' }
  | { kind: 'again' }
  | { kind: 'briefing' }
  | { kind: 'session'; ref: SessionRef };

/**
 * A whole utterance that is a flow command rather than dictation: "취소"
 * (an explicit cancel word only; "아니" alone is content here), "다시",
 * "현황" and "3번 세션". `null` for anything else.
 */
export function voiceCommand(text: string): VoiceCommand | null {
  const n = normalizeAnswer(text);
  if (!n) return { kind: 'empty' };
  const answer = classifySendAnswer(text);
  if (answer === 'cancel' && CANCEL_CORE.test(n)) return { kind: 'cancel' };
  if (answer === 'again') return { kind: 'again' };
  if (BRIEFING_WORDS.has(n) || isBriefingPhrase(text)) return { kind: 'briefing' };
  const ref = parseSessionRef(text);
  if (ref) return { kind: 'session', ref };
  return null;
}

// --------------------------------------------------------- command endings

/**
 * Korean endings of a request or command ("…고쳐줘", "…실행해", "…해 주세요").
 * A small fixed list, no language model: a dictation that ends like this and
 * is followed by silence is scheduled for sending.
 */
const KO_COMMAND_ENDINGS = [
  '해줘',
  '해주세요',
  '해라',
  '하라',
  '하십시오',
  '해요',
  '하세요',
  '해',
  '줘',
  '줘요',
  '주세요',
  '고쳐',
  '만들어',
  '돌려',
  '실행해',
  '확인해',
  '추가해',
  '진행해',
  '보여줘',
  '알려줘',
];
/**
 * Last words that end like a command ("…해") but are not one: connectives
 * ("…에 대해", "…을 위해", "…을 통해") and nouns ("이해", "피해", "올해").
 * A message that stops on them is unfinished and is never sent by itself.
 */
const KO_NOT_COMMANDS = new Set([
  '대해',
  '위해',
  '통해',
  '인해',
  '의해',
  '비해',
  '관해',
  '향해',
  '이해',
  '오해',
  '피해',
  '손해',
  '방해',
  '재해',
  '올해',
  '매해',
  '내해',
]);
/**
 * Korean endings that leave a sentence open: particles ("을", "를", "이",
 * "가", "은", "는", "에", "에게", "한테", "으로", "로", "와", "과", "도", "만")
 * and connectives ("고", "서", "는데", "지만", "면", "니까", "려고", "도록",
 * "거나", …). A message whose last word ends like this is never sent by
 * itself, whatever else matched.
 */
const KO_OPEN_ENDINGS = [
  '을',
  '를',
  '이',
  '가',
  '은',
  '는',
  '에',
  '에서',
  '에게',
  '한테',
  '께',
  '으로',
  '로',
  '와',
  '과',
  '랑',
  '하고',
  '도',
  '만',
  '의',
  '고',
  '서',
  '는데',
  '은데',
  '인데',
  '지만',
  '면',
  '면은',
  '니까',
  '려고',
  '러',
  '도록',
  '거나',
  '든지',
  '면서',
  '자마자',
];

/**
 * Whether a Korean `text` stops on a particle or a connective ("…해당
 * 내용을", "…불러 본 거고", "…있는데"): the sentence goes on.
 */
export function endsOpenKorean(text: string): boolean {
  const body = text
    .normalize('NFKC')
    .trim()
    .replace(/[\s.,!?。、…~]+$/u, '');
  if (!/\p{Script=Hangul}$/u.test(body)) return false;
  const last = body.split(/\s+/).pop()!;
  if (KO_NOT_COMMANDS.has(last)) return true;
  // A one-syllable word is not a particle on its own ("이", "가" as words).
  if (Array.from(last).length < 2) return false;
  return KO_OPEN_ENDINGS.some((e) => last.endsWith(e) && last !== e);
}

const JA_COMMAND_ENDINGS = ['ください', '下さい', 'して', 'してくれ', 'お願いします', 'お願い'];
const EN_IMPERATIVE_VERBS = new Set(
  (
    'add build change check commit create delete deploy explain find fix give implement install ' +
    'list make merge move open push refactor remove rename restart review run show start stop ' +
    'summarize tell test update write'
  ).split(' '),
);
const ES_IMPERATIVE_VERBS = new Set(
  (
    'agrega anade arregla cambia corrige crea ejecuta elimina explica haz instala muestra ' +
    'revisa abre actualiza borra escribe'
  ).split(' '),
);

function stripAccents(s: string): string {
  return s.normalize('NFD').replace(/[̀-ͯ]/g, '').normalize('NFC');
}

/**
 * Whether `text` ends in a command form: Korean imperative or request
 * endings, Japanese 「…してください」, an English sentence that starts with a
 * verb such as "fix" or "run" and ends with a period, or one with "please";
 * Spanish likewise ("arregla …", "por favor"). Questions never count.
 */
export function endsWithCommand(text: string): boolean {
  const t = text.normalize('NFKC').trim();
  if (!t || /[?？]\s*$/u.test(t)) return false;
  const body = t.replace(/[\s.!。！…]+$/u, '');
  if (!body) return false;
  if (/\p{Script=Hangul}$/u.test(body)) {
    const words = body.split(/\s+/);
    const last = words[words.length - 1]!;
    // "…에 대해", "…을 위해": connectives, the sentence goes on.
    if (KO_NOT_COMMANDS.has(last) || endsOpenKorean(body)) return false;
    const lastTwo = words.slice(-2).join('');
    return KO_COMMAND_ENDINGS.some((e) => last.endsWith(e) || lastTwo.endsWith(e));
  }
  if (/[\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Han}]$/u.test(body)) {
    return JA_COMMAND_ENDINGS.some((e) => body.endsWith(e));
  }
  const sentence = stripAccents(body.toLowerCase())
    .split(/[.!?]\s+/)
    .pop()!
    .replace(/^[\s,;:]+/, '');
  if (/(^|\s|,)please$/.test(sentence) || /^please\s/.test(sentence)) return true;
  if (/(^|\s|,)por favor$/.test(sentence)) return true;
  const first = sentence.split(/[\s,]+/)[0] ?? '';
  const endedWithPeriod = /[.!]$/u.test(t);
  return endedWithPeriod && (EN_IMPERATIVE_VERBS.has(first) || ES_IMPERATIVE_VERBS.has(first));
}

// -------------------------------------------------------------- hand-off

let handoff: { at: number; carry: string } | null = null;

/** A spoken session switch: the next composer starts awake with `carry`. */
export function handOffAwake(carry: string, at: number): void {
  handoff = { at, carry };
}

/** The pending hand-off (once), if it is recent. */
export function takeHandOff(now: number): { carry: string } | null {
  const h = handoff;
  handoff = null;
  return h && now - h.at < 10_000 ? { carry: h.carry } : null;
}

// ------------------------------------------------------------------ flow

const defaultTimers: VocoTimers = {
  set(fn, ms) {
    const id = setTimeout(fn, ms);
    return () => clearTimeout(id);
  },
  now: () => Date.now(),
};

interface Countdown {
  reason: 'scheduled' | 'undo';
  message: string;
  endsAt: number;
  /** Someone spoke during the undo window: wait for what was said. */
  held: boolean;
  expired: boolean;
  cancel: () => void;
}

/** See the module comment. */
export class VocoFlow {
  private phase: VocoPhase = 'waiting';
  /** Composer text from before the wake word. */
  private base = '';
  /** Text dictated since the wake word (without wake words and send phrases). */
  private dictated = '';
  private speaking = false;
  private lastEnd = 0;
  private pending = 0;
  private chain: Promise<void> = Promise.resolve();
  private idleCancel: (() => void) | null = null;
  private scheduleCancel: (() => void) | null = null;
  private countdown: Countdown | null = null;
  private sentAt = -Infinity;
  private disposed = false;
  /** "어느 세션인가요?" was asked: the next words name the session. */
  private askingTarget = false;
  /**
   * A plain send command ended the last utterance ("…, 전송해"): it is held
   * back (not in the text) and sends after `TRAILING_SEND_SILENCE_MS` of
   * silence. If the user goes on instead, it was part of the sentence and
   * is put back before the next words.
   */
  private pendingSend: string | null = null;
  /** A message is on its way to the session. */
  private sending = false;
  /** A message waits for its uploads and can still be stopped. */
  private queuedSend = false;
  private queuedCanceled = false;
  /** The held-back send command whose send is in the undo window now. */
  private undoSendWords: string | null = null;
  private readonly timers: VocoTimers;

  constructor(private readonly d: VocoDeps) {
    this.timers = d.timers ?? defaultTimers;
  }

  get state(): VocoPhase {
    return this.phase;
  }

  /** Every queued utterance has been handled (tests). */
  settled(): Promise<void> {
    return this.chain;
  }

  /** Starts listening for the message right away (after a spoken session switch). */
  wakeNow(carry = ''): void {
    this.base = this.d.text();
    this.dictated = '';
    this.setView({ phase: 'awake' });
    this.d.chime('wake');
    if (carry.trim()) this.append(carry.trim());
    this.lastEnd = this.timers.now();
    this.arm();
  }

  speechStart(): void {
    if (this.disposed) return;
    this.speaking = true;
    this.clearWaitTimers();
    // Instant feedback: the chip shows that the call is being heard.
    if (this.phase === 'waiting' && !this.view.hearing)
      this.setView({ phase: 'waiting', hearing: true });
    const c = this.countdown;
    if (c?.reason === 'scheduled') {
      // The user goes on talking: no send, keep listening.
      diag('voco.countdown.cancel', { by: 'speech' });
      this.stopCountdown();
      this.setView({ phase: 'awake' });
    } else if (c?.reason === 'undo') {
      c.held = true;
    }
  }

  speechEnd(u: HeardUtterance | null): void {
    if (this.disposed) return;
    this.speaking = false;
    if (!u) {
      this.unhear();
      this.release();
      this.arm();
      return;
    }
    this.lastEnd = u.endedAt;
    this.pending += 1;
    this.chain = this.chain.then(async () => {
      try {
        await this.process(u);
      } catch (err) {
        diag('voco.utterance.error', { message: (err as Error)?.message ?? String(err) });
      } finally {
        this.pending -= 1;
      }
      if (this.pending === 0) this.unhear();
      this.release();
      this.arm();
    });
  }

  /** A tap anywhere: a countdown stops and the text stays in the box. */
  tap(): void {
    // Once the send is on its way a tap changes nothing.
    if (this.disposed || this.sending) return;
    // Also before a countdown exists: a held-back send command waiting for
    // silence, and the undo window while its announcement is still spoken.
    const going = !!this.countdown || !!this.pendingSend || this.phase === 'undo';
    if (!going) return;
    diag('voco.countdown.cancel', { by: 'tap', reason: this.countdown?.reason ?? 'pending' });
    this.toWaiting('kept');
  }

  dispose(): void {
    this.disposed = true;
    this.clearWaitTimers();
    this.stopCountdown();
  }

  // ---------------------------------------------------------------- inner

  private view: VocoView = { phase: 'waiting' };

  private setView(view: VocoView) {
    this.phase = view.phase;
    this.view = view;
    if (!this.disposed) this.d.onView(view);
  }

  /** The heard speech was not a call: the chip rests again. */
  private unhear() {
    if (this.phase === 'waiting' && this.view.hearing) this.setView({ phase: 'waiting' });
  }

  private toWaiting(result?: VocoResult) {
    this.clearWaitTimers();
    this.stopCountdown();
    this.dictated = '';
    this.pendingSend = null;
    this.undoSendWords = null;
    this.setView(result ? { phase: 'waiting', result } : { phase: 'waiting' });
  }

  private clearWaitTimers() {
    this.idleCancel?.();
    this.idleCancel = null;
    this.scheduleCancel?.();
    this.scheduleCancel = null;
  }

  private stopCountdown() {
    this.countdown?.cancel();
    this.countdown = null;
  }

  private findWake(t: Transcript, speechMs?: number): WakeMatch | null {
    const opts = { extra: this.d.settings().wakeWords, speechMs };
    return matchWakeWord(t.text, opts) ?? (t.rawText ? matchWakeWord(t.rawText, opts) : null);
  }

  /**
   * The wake word at the start of an utterance in the middle of a message.
   * The listener cuts a long dictation at every short pause, so a piece may
   * start with "보코" that is content ("일단은 지금 … 보코 너를 불러 본
   * 거고"). It is a call only when it stands apart (a pause mark after it,
   * or nothing else) or is followed by a flow command or a session name
   * ("보코 3번 세션", "보코 취소"); otherwise the words stay in the text.
   */
  private findCall(t: Transcript, speechMs: number): WakeMatch | null {
    const opts = { extra: this.d.settings().wakeWords, speechMs };
    for (const source of [t.text, t.rawText]) {
      if (!source) continue;
      const wake = matchWakeWord(source, opts);
      if (!wake) continue;
      if (!this.dictated) return wake;
      const rest = wake.rest;
      const apart = /^\s*[,，、.。!！?？…~〜]/u.test(source.slice(wake.end));
      if (
        !rest ||
        apart ||
        voiceCommand(rest) !== null ||
        (this.d.findSessionByName?.(rest) ?? null) !== null
      ) {
        return wake;
      }
      diag('voco.wake.content', { restChars: rest.length });
      return null;
    }
    return null;
  }

  private append(text: string) {
    this.dictated = joinText(this.dictated, text);
    this.d.setText(joinText(this.d.text(), text));
  }

  /** (Re)starts the Waiting and scheduled-send timers after the last words. */
  private arm() {
    if (this.disposed || this.phase !== 'awake' || this.speaking || this.pending > 0) return;
    this.clearWaitTimers();
    const now = this.timers.now();
    this.idleCancel = this.timers.set(
      () => {
        if (this.phase !== 'awake' || this.speaking || this.pending > 0) return;
        diag('voco.timeout', { dictatedChars: this.dictated.length });
        this.toWaiting();
      },
      Math.max(0, this.lastEnd + IDLE_MS - now),
    );
    const s = this.d.settings();
    if (this.pendingSend) {
      // "…, 전송해" and then silence: the send command stands, it is sent.
      this.scheduleCancel = this.timers.set(
        () => {
          if (this.phase !== 'awake' || this.speaking || this.pending > 0) return;
          const words = this.pendingSend;
          this.pendingSend = null;
          diag('voco.send.confirmed', { textChars: this.dictated.length });
          void this.commit('phrase', words);
        },
        Math.max(0, this.lastEnd + TRAILING_SEND_SILENCE_MS - now),
      );
    } else if (s.autoSend && this.dictated && endsWithCommand(this.dictated)) {
      this.scheduleCancel = this.timers.set(
        () => {
          if (this.phase !== 'awake' || this.speaking || this.pending > 0) return;
          void this.commit('scheduled');
        },
        Math.max(0, this.lastEnd + SCHEDULE_SILENCE_MS - now),
      );
    }
  }

  private async process(u: HeardUtterance): Promise<void> {
    if (this.phase === 'waiting') {
      // `pending` counts this utterance too: above one, newer ones wait.
      const ageMs = this.timers.now() - u.endedAt;
      if (this.pending > 1 && ageMs > STALE_WAKE_MS && u.speechMs >= CALL_LIKE_MS) {
        diag('voco.wake.skipped', { ageMs, speechMs: u.speechMs, queued: this.pending - 1 });
        return;
      }
      const truncated = u.speechMs > WAKE_HEAD_MS;
      const audio = truncated ? u.head(WAKE_HEAD_MS) : u.audio;
      const requestAt = this.timers.now();
      const t = await this.d.transcribe(audio, 'wake');
      const responseAt = this.timers.now();
      if (this.disposed || this.phase !== 'waiting') return;
      let wake = this.findWake(t, u.speechMs);
      // Counts only: never what was said.
      diag('voco.wake.check', {
        speechMs: u.speechMs,
        truncated,
        wake: !!wake,
        bytes: audio.size,
        sttMs: responseAt - requestAt,
      });
      if (!wake) {
        // A short call-like utterance that did not read as the wake word:
        // logged as a near miss (length only), never answered.
        const chars = Array.from(t.text.replace(/[\s\p{P}\p{S}]+/gu, '')).length;
        const near = u.speechMs >= 250 && u.speechMs < 1200 && chars <= 6;
        if (near) diag('voco.wake.near', { chars, speechMs: u.speechMs });
        const canceled = voiceCommand(t.text)?.kind === 'cancel';
        // A send still waiting for its uploads has not left: it can be stopped.
        if (canceled && this.queuedSend) {
          await this.cancelQueued();
          return;
        }
        // "취소" right after a send: say honestly that it cannot be recalled.
        if (canceled && (this.sending || this.timers.now() - this.sentAt < TOO_LATE_MS)) {
          diag('voco.undo.tooLate');
          await this.d.speak(VOCO_PHRASES[this.d.lang()].tooLate);
          return;
        }
        // A near miss: a soft low tone and a short notice, so an eyes-free
        // user does not keep talking to a microphone that is not listening.
        if (near && !canceled) {
          this.setView({ phase: 'waiting', result: 'missed' });
          this.d.chime('missed');
        }
        return;
      }
      diag('voco.wake.detected', {
        lang: wake.lang,
        heardChars: wake.heard.length,
        restChars: wake.rest.length,
      });
      this.latency = { endedAt: u.endedAt, requestAt, responseAt };
      let rest = wake.rest;
      if (truncated) {
        // The wake word started a longer utterance: now it may be transcribed whole.
        const full = await this.d.transcribe(u.audio, 'dictation');
        if (this.disposed) return;
        wake = this.findWake(full);
        rest = wake ? wake.rest : full.text.trim();
      }
      await this.onWake(rest, u.pausedBeforeLast);
      return;
    }
    const t = await this.d.transcribe(u.audio, 'dictation');
    if (this.disposed) return;
    const wake = this.findCall(t, u.speechMs);
    const text = wake ? wake.rest : t.text.trim();
    if (this.askingTarget && (await this.answerTarget(text))) return;
    await this.onAwakeText(text, !!wake, u.pausedBeforeLast);
  }

  /** Timing of the last wake (for `voco.wake.latency`). */
  private latency: { endedAt: number; requestAt: number; responseAt: number } | null = null;

  /** Logs how long the wake took, stage by stage, once the chime played. */
  private logLatency(stage: 'chime' | 'ack') {
    const l = this.latency;
    if (!l) return;
    const now = this.timers.now();
    diag('voco.wake.latency', {
      stage,
      // Speech end -> STT request (end-of-utterance silence and slicing).
      toRequestMs: l.requestAt - l.endedAt,
      sttMs: l.responseAt - l.requestAt,
      // Speech end -> chime / "네" finished.
      totalMs: now - l.endedAt,
    });
    if (stage === 'ack') this.latency = null;
  }

  /** "오빗 플랫폼" after "어느 세션인가요?": `true` when it was a session. */
  private async answerTarget(text: string): Promise<boolean> {
    this.askingTarget = false;
    const cmd = voiceCommand(text);
    if (cmd?.kind === 'session') {
      await this.switchTo(cmd.ref, this.dictated);
      return true;
    }
    const named = this.d.findSessionByName?.(text) ?? null;
    if (named && named !== 'ambiguous') {
      await this.switchToFound(named, this.dictated);
      return true;
    }
    return false;
  }

  /** A spoken name; `true` when it was one (switched, or asked which). */
  private async byName(text: string, carry: string): Promise<boolean> {
    const named = this.d.findSessionByName?.(text) ?? null;
    if (!named) return false;
    if (named === 'ambiguous') {
      diag('voco.command', { kind: 'session', found: false, ambiguous: true });
      this.askingTarget = true;
      if (this.phase === 'waiting') {
        this.base = this.d.text();
        this.dictated = '';
        this.setView({ phase: 'awake' });
      }
      await this.d.speak(VOCO_PHRASES[this.d.lang()].whichSession);
      this.lastEnd = this.timers.now();
      return true;
    }
    await this.switchToFound(named, carry);
    return true;
  }

  private async onWake(rest: string, pausedBefore: boolean) {
    const cmd = voiceCommand(rest);
    if (!cmd && rest && (await this.byName(rest, ''))) return;
    switch (cmd?.kind) {
      case 'empty':
      case 'again':
        await this.goAwake();
        return;
      case 'cancel':
        diag('voco.command', { kind: 'cancel', phase: 'waiting' });
        if (this.queuedSend) {
          await this.cancelQueued();
          return;
        }
        this.d.chime('cancel');
        return;
      case 'briefing':
        diag('voco.command', { kind: 'briefing' });
        this.d.briefing();
        return;
      case 'session':
        await this.switchTo(cmd.ref, '');
        return;
      default:
        // "보코, 로그인 버그 고쳐줘 오버" in one breath.
        this.base = this.d.text();
        this.dictated = '';
        this.setView({ phase: 'awake' });
        this.d.chime('wake');
        await this.onAwakeText(rest, false, pausedBefore);
    }
  }

  private async goAwake() {
    this.base = this.d.text();
    this.dictated = '';
    this.pendingSend = null;
    this.setView({ phase: 'awake' });
    // The chime at once; the short "네" right after it.
    this.d.chime('wake');
    this.logLatency('chime');
    const ack = VOCO_PHRASES[this.d.lang()].ack;
    await (this.d.ack ? this.d.ack(ack) : this.d.speak(ack));
    this.logLatency('ack');
    this.lastEnd = this.timers.now();
  }

  private async switchTo(ref: SessionRef, carry: string) {
    const found = this.d.findSession(ref);
    const lang = this.d.lang();
    diag('voco.command', { kind: 'session', found: !!found, carriedChars: carry.length });
    if (!found) {
      await this.d.speak(VOCO_PHRASES[lang].noSession);
      return;
    }
    await this.switchToFound(found, carry);
  }

  private async switchToFound(found: FoundSession, carry: string) {
    const lang = this.d.lang();
    await this.d.speak(normalizeKoreanNumbers(VOCO_PHRASES[lang].rogerSwitch(found.label), lang));
    if (carry) this.d.setText(this.base);
    this.toWaiting();
    found.select(carry);
  }

  private async onAwakeText(text: string, hadWake: boolean, pausedBefore: boolean) {
    const lang = this.d.lang();
    const cmd = voiceCommand(text);
    if (this.phase === 'undo') {
      if (cmd?.kind === 'cancel') {
        await this.recall();
        return;
      }
      if (cmd || classifySendAnswer(text) === 'send') {
        // Nothing, a yes ("응 그래") or another command word: the send stands.
        diag('voco.undo.ignored', { chars: text.length });
        return;
      }
      // The user goes on with the message: it was not finished, so it is not
      // sent. The held-back send words go back into the text.
      diag('voco.undo.resumed', { chars: text.length });
      this.stopCountdown();
      this.setView({ phase: 'awake' });
      if (this.undoSendWords) this.append(this.undoSendWords);
      this.undoSendWords = null;
    }
    if (this.phase === 'waiting') {
      // The message was already sent (or dropped) while this was transcribed.
      if (cmd?.kind === 'cancel' && this.timers.now() - this.sentAt < TOO_LATE_MS) {
        diag('voco.undo.tooLate');
        await this.d.speak(VOCO_PHRASES[lang].tooLate);
      }
      return;
    }
    if (this.phase === 'countdown' && cmd?.kind !== 'cancel') this.stopCountdown();
    switch (cmd?.kind) {
      case 'cancel':
        this.stopCountdown();
        diag('voco.command', { kind: 'cancel', dictatedChars: this.dictated.length });
        this.d.setText(this.base);
        this.d.chime('cancel');
        this.toWaiting('cancelled');
        await this.d.speak(PHRASES[lang].cancelled);
        return;
      case 'again':
        diag('voco.command', { kind: 'again' });
        this.d.setText(this.base);
        this.dictated = '';
        this.pendingSend = null;
        this.setView({ phase: 'awake' });
        this.d.chime('wake');
        return;
      case 'empty':
        if (hadWake) await this.goAwake();
        return;
      case 'briefing':
        if (hadWake) {
          diag('voco.command', { kind: 'briefing' });
          this.d.briefing();
          return;
        }
        break;
      case 'session':
        if (hadWake) {
          await this.switchTo(cmd.ref, this.dictated);
          return;
        }
        break;
      default:
        // "보코, 오빗 플랫폼" in the middle of a message.
        if (hadWake && text && (await this.byName(text, this.dictated))) return;
        break;
    }
    if (this.phase === 'countdown') this.setView({ phase: 'awake' });
    const s = this.d.settings();
    let send: SendMatch | null = matchSendPhrase(text, { extra: s.sendPhrases, pausedBefore });
    if (send?.trailing && endsOpenKorean(send.before)) {
      // "…해당 내용을, 전송해": the verb of the sentence ("send the
      // content"), i.e. content for the agent, not a send command.
      diag('voco.send.content', { lang: send.lang });
      send = null;
    }
    // The user went on after a held-back "전송해": it was part of the sentence.
    if (this.pendingSend) {
      diag('voco.send.resumed', { chars: text.length });
      this.append(this.pendingSend);
      this.pendingSend = null;
    }
    const body = send ? send.before : text;
    if (body) this.append(body);
    if (!send) return;
    diag('voco.send.detected', {
      lang: send.lang,
      trailing: send.trailing,
      strippedChars: text.length - body.length,
      textChars: this.dictated.length,
    });
    if (send.trailing) {
      // Sent only after a further silence (`arm`).
      this.pendingSend = send.heard;
      return;
    }
    await this.commit('phrase');
  }

  /** `sendWords`: a held-back send command, put back if the user goes on. */
  private async commit(reason: 'phrase' | 'scheduled', sendWords: string | null = null) {
    const message = this.d.text().trim();
    const s = this.d.settings();
    diag('voco.commit', { reason, chars: message.length, confirm: s.confirm });
    if (!message) return;
    this.clearWaitTimers();
    this.undoSendWords = sendWords;
    if (s.confirm === 'readback') {
      this.toWaiting();
      await this.d.readBack(message);
      return;
    }
    const lang = this.d.lang();
    if (reason === 'phrase') {
      this.setView({ phase: 'undo', seconds: Math.ceil(UNDO_MS / 1000) });
      await this.d.speak(
        normalizeKoreanNumbers(VOCO_PHRASES[lang].roger(this.d.sessionLabel()), lang),
      );
      if (this.disposed || this.phase !== 'undo') return;
      this.startCountdown('undo', UNDO_MS, message);
    } else {
      this.startCountdown('scheduled', s.countdownMs, message);
    }
  }

  private startCountdown(reason: Countdown['reason'], ms: number, message: string) {
    this.stopCountdown();
    let cancelTimer: () => void = () => {};
    const c: Countdown = {
      reason,
      message,
      endsAt: this.timers.now() + ms,
      // Words may be on their way already; they could be "취소".
      held: reason === 'undo' && (this.speaking || this.pending > 0),
      expired: false,
      cancel: () => cancelTimer(),
    };
    this.countdown = c;
    diag('voco.countdown.start', { reason, ms });
    const step = () => {
      if (this.countdown !== c) return;
      const left = c.endsAt - this.timers.now();
      if (left <= 0) {
        this.expire(c);
        return;
      }
      this.setView({
        phase: reason === 'undo' ? 'undo' : 'countdown',
        seconds: Math.ceil(left / 1000),
      });
      if (reason === 'scheduled') this.d.chime('tick');
      cancelTimer = this.timers.set(step, Math.min(1000, left));
    };
    step();
  }

  private expire(c: Countdown) {
    if (c.held) {
      // "취소" may be on its way: wait for it, but not forever.
      c.expired = true;
      // Still speaking (the message goes on?) or still transcribing: keep
      // waiting, so a send never goes out while the user is talking.
      const check = () => {
        if (this.countdown !== c) return;
        const busy = this.speaking || this.pending > 0;
        if (busy && this.timers.now() - c.endsAt < HOLD_SPEAKING_MAX_MS) {
          c.cancel = this.timers.set(check, 500);
          return;
        }
        void this.fire(c);
      };
      c.cancel = this.timers.set(check, HOLD_MAX_MS);
      return;
    }
    void this.fire(c);
  }

  /** The utterance that held the undo window was not "취소". */
  private release() {
    const c = this.countdown;
    if (!c?.held) return;
    c.held = false;
    if (c.expired) {
      c.cancel();
      void this.fire(c);
    }
  }

  private async fire(c: Countdown) {
    if (this.countdown !== c) return;
    this.countdown = null;
    const lang = this.d.lang();
    const queued = !!this.d.queued?.();
    if (queued) {
      // Goes as one prompt once the uploads are done; Voco listens meanwhile.
      diag('voco.send.queued', { reason: c.reason, chars: c.message.length });
      this.toWaiting('queued');
    }
    this.sending = !queued;
    this.queuedSend = queued;
    const ok = await this.d.send(c.message);
    this.sending = false;
    this.queuedSend = false;
    this.sentAt = this.timers.now();
    diag('voco.send.result', { ok, reason: c.reason, chars: c.message.length, queued });
    if (this.disposed) return;
    if (this.queuedCanceled) {
      // `cancelQueued` has said so; the view is its own again.
      this.queuedCanceled = false;
      this.sentAt = 0;
      return;
    }
    // A new message started while the queued one waited: its view stays.
    if (queued && this.phase !== 'waiting') {
      if (ok) this.d.chime('sent');
      return;
    }
    if (ok) {
      this.d.chime('sent');
      this.toWaiting('sent');
      if (c.reason === 'scheduled' || queued) {
        await this.d.speak(
          normalizeKoreanNumbers(VOCO_PHRASES[lang].rogerSent(this.d.sessionLabel()), lang),
        );
      }
    } else {
      this.toWaiting('kept');
      await this.d.speak(PHRASES[lang].keptFailed);
    }
  }

  /** The send waits for uploads: it has not left, so it can still be stopped. */
  private async cancelQueued() {
    this.queuedSend = false;
    this.queuedCanceled = true;
    this.d.cancelQueued?.();
    diag('voco.queued.cancelled');
    this.d.chime('cancel');
    this.toWaiting('cancelled');
    await this.d.speak(VOCO_PHRASES[this.d.lang()].recalled);
  }

  private async recall() {
    // The message is on its way: never say it was not sent.
    if (this.sending) {
      diag('voco.undo.tooLate');
      await this.d.speak(VOCO_PHRASES[this.d.lang()].tooLate);
      return;
    }
    // Without a countdown the announcement is still being spoken: `commit`
    // finds the phase changed and starts none.
    const c = this.countdown;
    this.stopCountdown();
    diag('voco.undo.recalled', { chars: c?.message.length ?? 0 });
    this.d.chime('cancel');
    this.toWaiting('cancelled');
    await this.d.speak(VOCO_PHRASES[this.d.lang()].recalled);
  }
}
