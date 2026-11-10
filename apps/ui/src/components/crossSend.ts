/**
 * Sending text to another session than the one on screen ("Send to
 * session…" → "Send now", and `@name` in the composer). It goes through the
 * same `send_input` path as the composer and keeps the target's guards:
 *
 * - sessions that take no input (ended, watched only) refuse it;
 * - text never lands in a prompt the target has open (a decision the person
 *   cannot see from here): it stays put, and the person answers there first;
 * - remote shells only take it through the "Run in shell" sheet (danger
 *   classification, explicit confirmation, code when needed), one line only.
 */
import { useCallback, useEffect, useRef } from 'react';

import { useI18n } from '../i18n/context';
import type { TranslateFn } from '../i18n/i18n';
import { isOpenShell, isShellSession } from '../protocol/helpers';
import { commandProblem } from '../protocol/shell';
import type { SessionId, SessionInfo } from '../protocol/types';
import { awaitsDecision, inputBlockReason } from '../state/reducer';
import { useActions, useAppState } from '../state/store';
import { useShell } from './ShellProvider';

export type CrossSendBlock = 'gone' | 'empty' | 'exited' | 'observe' | 'decision' | 'shell';

export type CrossSendResult =
  | { status: 'sent'; session: SessionInfo }
  /** Handed to the "Run in shell" sheet, which confirms and runs it. */
  | { status: 'shell'; session: SessionInfo }
  | { status: 'blocked'; reason: CrossSendBlock }
  | { status: 'failed'; error: Error };

/** Why `text` cannot go to `target` now (`null`: it can). */
export function crossSendBlock(
  target: SessionInfo | undefined,
  text: string,
  shellAllowed: boolean,
): CrossSendBlock | null {
  if (!target) return 'gone';
  if (!text.trim()) return 'empty';
  const reason = inputBlockReason(target);
  if (reason) return reason;
  if (isShellSession(target)) {
    if (!shellAllowed || !isOpenShell(target) || commandProblem(text.trim())) return 'shell';
    return null;
  }
  if (awaitsDecision(target)) return 'decision';
  return null;
}

export function describeCrossSendBlock(t: TranslateFn, reason: CrossSendBlock, name: string) {
  switch (reason) {
    case 'gone':
      return t('sendTo.blocked.gone');
    case 'empty':
      return t('sendTo.blocked.empty');
    case 'exited':
      return t('sendTo.blocked.exited', { name });
    case 'observe':
      return t('sendTo.blocked.observe', { name });
    case 'decision':
      return t('sendTo.blocked.decision', { name });
    case 'shell':
      return t('sendTo.blocked.shell', { name });
  }
}

/** Sends text to a session that is not (necessarily) the one on screen. */
export function useCrossSend(): (targetId: SessionId, text: string) => Promise<CrossSendResult> {
  const { t } = useI18n();
  const actions = useActions();
  const shell = useShell();
  const sessions = useAppState((s) => s.sessions);
  // Read when the send happens, not when the button was rendered.
  const latest = useRef(sessions);
  useEffect(() => {
    latest.current = sessions;
  });

  return useCallback(
    async (targetId, text) => {
      const target = latest.current[targetId];
      const block = crossSendBlock(target, text, !!shell?.allowed);
      if (block || !target) return { status: 'blocked', reason: block ?? 'gone' };
      const value = text.trim();
      if (isShellSession(target)) {
        // The sheet shows the classification and asks before anything runs.
        shell!.runInShell({ command: value, sessionId: target.id });
        return { status: 'shell', session: target };
      }
      try {
        // Not typed into the target's own box: never an answer to its prompt.
        await actions.sendText(target.id, value, undefined, false);
      } catch (err) {
        return { status: 'failed', error: err as Error };
      }
      actions.showToast({
        id: `sendto:${target.id}:${Date.now()}`,
        category: 'activity',
        sessionId: target.id,
        title: t('sendTo.sentToast', { name: target.display_name }),
        body: '',
        // Always shown: the person must see where the text went.
        security: true,
      });
      return { status: 'sent', session: target };
    },
    [actions, shell, t],
  );
}
