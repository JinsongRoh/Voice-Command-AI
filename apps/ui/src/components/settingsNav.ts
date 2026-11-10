import type { MessageKey } from '../i18n/i18n';

/**
 * Settings tabs, the section registry (which tab each section lives in) and
 * deep links into Settings ("open Settings at the conductor section").
 * Kept out of the component file so other views can link without pulling
 * Settings in.
 */

export const SETTINGS_TABS = [
  'general',
  'voice',
  'alerts',
  'connection',
  'security',
  'conductor',
  'about',
] as const;
export type SettingsTab = (typeof SETTINGS_TABS)[number];

export const SETTINGS_TAB_LABELS: Record<SettingsTab, MessageKey> = {
  general: 'settings.tab.general',
  voice: 'settings.tab.voice',
  alerts: 'settings.tab.alerts',
  connection: 'settings.tab.connection',
  security: 'settings.tab.security',
  conductor: 'settings.tab.conductor',
  about: 'settings.tab.about',
};

/**
 * Every top-level Settings section (its heading id) and its tab, in display
 * order. A section missing here has no tab and is never shown: the
 * Settings tests list them all.
 */
export const SETTINGS_SECTIONS = {
  'set-appearance': 'general',
  'set-startup': 'general',
  'set-speech': 'voice',
  'set-misheard': 'voice',
  'set-spoken': 'voice',
  'set-engines': 'voice',
  'set-notify': 'alerts',
  'set-push': 'alerts',
  'set-agent-hooks': 'alerts',
  'set-connect-phone': 'connection',
  'set-conn': 'connection',
  'set-dnspilot': 'connection',
  'set-machines': 'connection',
  'set-devices': 'connection',
  'set-audit': 'connection',
  'set-app-lock': 'security',
  'set-sec-status': 'security',
  'set-sec-mail': 'security',
  'set-sec-recovery': 'security',
  'set-security': 'security',
  'set-conductor': 'conductor',
  'set-about': 'about',
  'set-oss': 'about',
  'set-support': 'about',
  'set-dev': 'about',
} as const satisfies Record<string, SettingsTab>;

export type SettingsSectionId = keyof typeof SETTINGS_SECTIONS;

/** Anchors inside a section that a deep link can land on. */
const SETTINGS_ANCHORS = {
  'set-updates': 'set-about',
} as const satisfies Record<string, SettingsSectionId>;

export type SettingsTarget = SettingsSectionId | keyof typeof SETTINGS_ANCHORS;

export function isSettingsTab(v: unknown): v is SettingsTab {
  return typeof v === 'string' && (SETTINGS_TABS as readonly string[]).includes(v);
}

/** The tab that shows a section (or an anchor inside one). */
export function tabOf(target: SettingsTarget): SettingsTab {
  const section: SettingsSectionId =
    target in SETTINGS_ANCHORS
      ? SETTINGS_ANCHORS[target as keyof typeof SETTINGS_ANCHORS]
      : (target as SettingsSectionId);
  return SETTINGS_SECTIONS[section];
}

const TAB_KEY = 'vca.settings.tab';

/** The last tab the person opened (storage may be unavailable). */
export function loadSettingsTab(): SettingsTab {
  try {
    const v = localStorage.getItem(TAB_KEY);
    return isSettingsTab(v) ? v : 'general';
  } catch {
    return 'general';
  }
}

export function saveSettingsTab(tab: SettingsTab) {
  try {
    localStorage.setItem(TAB_KEY, tab);
  } catch {
    // Private mode or storage full: the tab just isn't remembered.
  }
}

/** A deep link waits this long for Settings to open. */
const PENDING_MS = 3000;

let pending: { target: SettingsTarget; at: number } | null = null;
const listeners = new Set<(target: SettingsTarget) => void>();

/**
 * Asks Settings to show `target` (switching tab and scrolling to it). Call
 * it right before opening Settings; an open Settings view follows at once.
 */
export function openSettingsAt(target: SettingsTarget) {
  if (listeners.size > 0) {
    pending = null;
    for (const l of listeners) l(target);
    return;
  }
  pending = { target, at: Date.now() };
}

/** The deep link waiting for Settings to open (does not consume it). */
export function peekSettingsTarget(): SettingsTarget | null {
  if (pending && Date.now() - pending.at > PENDING_MS) pending = null;
  return pending?.target ?? null;
}

const viewListeners = new Set<() => void>();

/**
 * Opens Settings at `target` from anywhere (an error message's "Open
 * Settings" button): the app switches to the Settings view, which then
 * shows the section.
 */
export function requestSettingsView(target: SettingsTarget) {
  openSettingsAt(target);
  for (const l of viewListeners) l();
}

/** The app shell listens here and switches to Settings. */
export function onSettingsViewRequest(listener: () => void): () => void {
  viewListeners.add(listener);
  return () => {
    viewListeners.delete(listener);
  };
}

/** Settings opened: drop the waiting deep link and follow later ones. */
export function subscribeSettingsTarget(listener: (target: SettingsTarget) => void) {
  pending = null;
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
