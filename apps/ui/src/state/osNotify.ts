/**
 * Operating system notifications for approval alerts. In the native app they
 * go through `tauri-plugin-notification` (WebView2 and WKWebView do not
 * reliably offer the web `Notification` API); in a browser or the mock build
 * they use the web API, and where neither exists nothing is shown. Only shown
 * while the app is not in front; the in-app toast covers the rest.
 *
 * Click-through: the plugin reports a tap on a notification on phones
 * (`onAction`, Android and iOS) but not on desktop (Windows, macOS, Linux), so
 * there the in-app alert is what offers the jump. The web API path brings the
 * window forward and jumps on click.
 *
 * Callers pass only a session name and a generic line: never command text,
 * paths or conversation.
 */

import { isTauri } from '@tauri-apps/api/core';
import {
  isPermissionGranted,
  onAction,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification';

export type OsNotificationState = 'unavailable' | 'default' | 'granted' | 'denied';

const ASKED_KEY = 'vca.osNotifyAsked';

function native(): boolean {
  try {
    return isTauri();
  } catch {
    return false;
  }
}

function webState(): OsNotificationState {
  if (typeof Notification === 'undefined') return 'unavailable';
  return Notification.permission;
}

/** Last answer of the plugin (the web view's value is only a first guess). */
let pluginState: OsNotificationState | null = null;

/** Current state without asking anything (a first guess until `loadOsNotificationState`). */
export function osNotificationState(): OsNotificationState {
  if (native() && pluginState) return pluginState;
  return webState();
}

/** Reads the real permission from the plugin; never prompts. */
export async function loadOsNotificationState(): Promise<OsNotificationState> {
  if (!native()) return webState();
  try {
    pluginState = (await isPermissionGranted()) ? 'granted' : webDeniedOrDefault();
  } catch {
    pluginState = webState();
  }
  return pluginState;
}

function webDeniedOrDefault(): OsNotificationState {
  return webState() === 'denied' ? 'denied' : 'default';
}

function wasAsked(): boolean {
  try {
    return localStorage.getItem(ASKED_KEY) === '1';
  } catch {
    return false;
  }
}

function markAsked(): void {
  try {
    localStorage.setItem(ASKED_KEY, '1');
  } catch {
    // Private mode: the system keeps its own answer anyway.
  }
}

/**
 * Asks for permission, once: after the first request the system's answer
 * stands and later calls only report it. Call it from a click.
 */
export async function requestOsNotifications(): Promise<OsNotificationState> {
  if (!native()) {
    if (typeof Notification === 'undefined') return 'unavailable';
    if (Notification.permission !== 'default' || wasAsked()) return Notification.permission;
    markAsked();
    return Notification.requestPermission();
  }
  const current = await loadOsNotificationState();
  if (current !== 'default' || wasAsked()) return current;
  markAsked();
  try {
    const answer = await requestPermission();
    pluginState = answer === 'granted' ? 'granted' : answer === 'denied' ? 'denied' : 'default';
  } catch {
    pluginState = 'unavailable';
  }
  return pluginState;
}

// --- click-through (phones) -------------------------------------------------

const clickHandlers = new Map<string, () => void>();
let listening = false;

/** The tag we put into `extra`, wherever the plugin nests the notification. */
function tagOf(payload: unknown): string | null {
  const p = payload as {
    extra?: { tag?: unknown };
    notification?: { extra?: { tag?: unknown } };
  } | null;
  const tag = p?.notification?.extra?.tag ?? p?.extra?.tag;
  return typeof tag === 'string' ? tag : null;
}

function listenForTaps(): void {
  if (listening) return;
  listening = true;
  // Desktop builds of the plugin have no action events: this rejects there.
  void onAction((payload) => {
    const tag = tagOf(payload);
    const handler = tag ? clickHandlers.get(tag) : undefined;
    if (!handler) return;
    clickHandlers.delete(tag!);
    handler();
  }).catch(() => {});
}

function inFront(): boolean {
  return typeof document !== 'undefined' && document.hasFocus();
}

/**
 * `title` is the session name line, `body` a generic sentence; `tag` is the
 * alert key. Safe to call without checking support.
 */
export function showOsNotification(
  title: string,
  body: string,
  tag: string,
  onClick: () => void,
): void {
  if (inFront()) return;
  if (native()) {
    void showNative(title, body, tag, onClick);
    return;
  }
  showWeb(title, body, tag, onClick);
}

async function showNative(
  title: string,
  body: string,
  tag: string,
  onClick: () => void,
): Promise<void> {
  try {
    if ((await loadOsNotificationState()) !== 'granted') return;
    clickHandlers.set(tag, onClick);
    listenForTaps();
    sendNotification({ title, body, extra: { tag } });
  } catch {
    // The in-app alert still shows.
  }
}

function showWeb(title: string, body: string, tag: string, onClick: () => void): void {
  if (webState() !== 'granted') return;
  try {
    const notification = new Notification(title, { body, tag });
    notification.onclick = () => {
      window.focus();
      onClick();
      notification.close();
    };
  } catch {
    // Some web views refuse the constructor; the in-app alert still shows.
  }
}
