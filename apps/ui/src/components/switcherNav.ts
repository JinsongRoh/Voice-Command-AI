/**
 * Opens the quick session switcher from anywhere (the session title on
 * phones, Ctrl+K on desktops). The app shell listens and shows the sheet.
 */
const listeners = new Set<() => void>();

export function requestSessionSwitcher(): void {
  for (const l of listeners) l();
}

export function onSessionSwitcherRequest(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Ctrl+K (Cmd+K on macOS) opens the switcher. */
export function isSwitcherShortcut(e: KeyboardEvent): boolean {
  return (
    (e.ctrlKey || e.metaKey) &&
    !e.altKey &&
    !e.shiftKey &&
    (e.key === 'k' || e.key === 'K') &&
    !e.isComposing
  );
}
