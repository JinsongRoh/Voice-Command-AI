import type { MessageKey } from '../../i18n/i18n';

/**
 * Tabs of the Command Center. Missions/Org and Reports come with their server features
 * (Phase B/C): until a server announces them they are not shown at all.
 * The remote shell (PowerShell on Windows) is not a tab here: it sits in
 * the conversation, next to its Chat and Terminal views.
 */
export const COMMAND_TABS = ['conversation', 'approvals', 'log'] as const;
export type CommandTab = (typeof COMMAND_TABS)[number];

export const COMMAND_TAB_LABELS: Record<CommandTab, MessageKey> = {
  conversation: 'cc.tab.conversation',
  approvals: 'cc.tab.approvals',
  log: 'cc.tab.log',
};
