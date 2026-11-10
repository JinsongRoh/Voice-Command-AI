/**
 * The status briefing ("현황 듣기"): the PC describes its sessions in a few
 * sentences (fixed templates, no LLM), the text is shown in a toast and
 * read aloud with the spoken-alert voice.
 */
import { useCallback, useState } from 'react';

import { useI18n } from '../i18n/context';
import type { SessionId } from '../protocol/types';
import { useActions } from '../state/store';
import { isBriefingPhrase } from './briefingPhrases';
import { normalizeKoreanNumbers } from './koreanNumbers';

let briefingCounter = 0;

export interface Briefing {
  /** A briefing is being fetched. */
  busy: boolean;
  /** Fetches, shows and speaks the briefing. Never rejects. */
  run(): Promise<void>;
  /**
   * For a finished dictation: when the whole text is a briefing phrase, runs
   * the briefing instead of sending it and offers "Send anyway" for
   * `sessionId`. Returns whether the text was taken.
   */
  takeDictation(text: string, sessionId: SessionId): boolean;
}

export function useBriefing(): Briefing {
  const { t, locale } = useI18n();
  const actions = useActions();
  const [busy, setBusy] = useState(false);

  const run = useCallback(async () => {
    setBusy(true);
    let text: string;
    try {
      text = await actions.briefing(locale);
    } catch {
      text = t('briefing.unavailable');
    } finally {
      setBusy(false);
    }
    briefingCounter += 1;
    actions.showToast({
      id: `briefing:${briefingCounter}`,
      category: 'activity',
      sessionId: null,
      title: t('briefing.title'),
      body: text,
    });
    actions.speak(normalizeKoreanNumbers(text, locale));
  }, [actions, locale, t]);

  const takeDictation = useCallback(
    (text: string, sessionId: SessionId) => {
      if (!isBriefingPhrase(text)) return false;
      const phrase = text.trim();
      briefingCounter += 1;
      actions.showToast({
        id: `briefing-taken:${briefingCounter}`,
        category: 'activity',
        sessionId: null,
        title: t('briefing.taken'),
        body: t('briefing.takenBody', { text: phrase }),
        action: {
          label: t('briefing.sendAnyway'),
          run: () => void actions.sendText(sessionId, phrase).catch(() => {}),
        },
      });
      void run();
      return true;
    },
    [actions, run, t],
  );

  return { busy, run, takeDictation };
}
