/**
 * Saves text (backup codes, exports) as a file in every place the UI runs.
 *
 * Tauri webviews ignore `<a download>` blob links, so:
 * - desktop shell: `vca_save_text_file` opens the OS "Save as" dialog from
 *   Rust and writes only the path the user picked;
 * - phones (mobile shell): the Web Share API with a file, or the text when
 *   the webview cannot share files;
 * - a plain browser: the usual blob download.
 */
import type { TranslateFn } from '../i18n/i18n';
import { bridge, call, isNative, NativeError, toNativeError } from './bridge';

export type SaveTextResult =
  /** Written (desktop) or handed to the browser's downloads. */
  | { status: 'saved'; fileName: string }
  /** Handed to the phone's share sheet. */
  | { status: 'shared'; fileName: string }
  | { status: 'cancelled' }
  | { status: 'error'; error: NativeError };

export type SaveTarget = 'desktop' | 'phone' | 'browser';

let target: Promise<SaveTarget> | null = null;

/** Where this UI runs, for choosing how to save (cached). */
export function saveTarget(): Promise<SaveTarget> {
  if (!isNative()) return Promise.resolve('browser');
  target ??= bridge.platform().then(
    (p): SaveTarget => (p === 'android' || p === 'ios' ? 'phone' : 'desktop'),
    (): SaveTarget => 'desktop',
  );
  return target;
}

/** Forgets the cached target (tests). */
export function resetSaveTarget(): void {
  target = null;
}

function mimeFor(name: string): string {
  if (name.endsWith('.csv')) return 'text/csv';
  if (name.endsWith('.md')) return 'text/markdown';
  if (name.endsWith('.json')) return 'application/json';
  return 'text/plain';
}

async function saveWithDialog(suggestedName: string, text: string): Promise<SaveTextResult> {
  try {
    const res = await call<{ status: 'saved'; fileName: string } | { status: 'cancelled' }>(
      'vca_save_text_file',
      { suggestedName, text },
    );
    return res.status === 'saved'
      ? { status: 'saved', fileName: res.fileName || suggestedName }
      : { status: 'cancelled' };
  } catch (err) {
    return { status: 'error', error: toNativeError(err) };
  }
}

async function saveWithShare(name: string, text: string): Promise<SaveTextResult> {
  if (typeof navigator === 'undefined' || typeof navigator.share !== 'function') {
    return {
      status: 'error',
      error: new NativeError('save_unsupported', 'sharing is not available here'),
    };
  }
  try {
    let data: ShareData = { title: name, text };
    if (typeof File === 'function') {
      const file = new File([text], name, { type: mimeFor(name) });
      if (navigator.canShare?.({ files: [file] })) data = { title: name, files: [file] };
    }
    await navigator.share(data);
    return { status: 'shared', fileName: name };
  } catch (err) {
    // A dismissed share sheet (`DOMException`, not always an `Error`).
    if ((err as { name?: unknown } | null)?.name === 'AbortError') return { status: 'cancelled' };
    return { status: 'error', error: toNativeError(err) };
  }
}

function saveWithDownload(name: string, text: string): SaveTextResult {
  try {
    const url = URL.createObjectURL(new Blob([text], { type: mimeFor(name) }));
    const a = document.createElement('a');
    a.href = url;
    a.download = name;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    return { status: 'saved', fileName: name };
  } catch (err) {
    return { status: 'error', error: toNativeError(err) };
  }
}

/** Saves `text` as a file named (or suggested as) `name`. Never throws. */
export async function saveTextFile(name: string, text: string): Promise<SaveTextResult> {
  switch (await saveTarget()) {
    case 'desktop':
      return saveWithDialog(name, text);
    case 'phone':
      return saveWithShare(name, text);
    default:
      return saveWithDownload(name, text);
  }
}

/** The status line for a save: "Saved to …", "Cancelled." or the error. */
export function describeSaveResult(t: TranslateFn, result: SaveTextResult): string {
  switch (result.status) {
    case 'saved':
      return t('file.save.saved', { name: result.fileName });
    case 'shared':
      return t('file.save.shared', { name: result.fileName });
    case 'cancelled':
      return t('file.save.cancelled');
    default:
      return result.error.code === 'save_unsupported'
        ? t('file.save.unsupported')
        : t('file.save.failed', { error: result.error.message });
  }
}
