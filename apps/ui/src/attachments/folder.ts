/**
 * Folder uploads: picking the files of a folder (`webkitdirectory` input or
 * drag and drop), skipping hidden and system entries, and checking the caps
 * before anything is sent. The server applies the same rules again.
 */
import type { FolderFile } from './upload';

/** Defaults of the server's `[attachments]` section. */
export const MAX_FILE_BYTES = 25 * 1024 * 1024;
export const MAX_FOLDER_BYTES = 100 * 1024 * 1024;
export const MAX_FOLDER_FILES = 500;

const SYSTEM_NAMES = new Set([
  'node_modules',
  'thumbs.db',
  'desktop.ini',
  '$recycle.bin',
  '__macosx',
]);

/** Mirrors the server's `is_hidden_or_system`. */
export function isHiddenOrSystem(component: string): boolean {
  return component.startsWith('.') || SYSTEM_NAMES.has(component.toLowerCase());
}

export interface FolderSelection {
  /** Folder name (the first path component). */
  name: string;
  /** Files to send, with paths relative to the folder. */
  files: FolderFile[];
  /** Hidden or system files left out. */
  skipped: number;
  totalBytes: number;
}

export type FolderProblem =
  | { kind: 'empty' }
  | { kind: 'tooManyFiles'; count: number; max: number }
  | { kind: 'tooLarge'; bytes: number; max: number }
  | { kind: 'fileTooLarge'; name: string; bytes: number; max: number };

export interface FolderLimits {
  maxFiles: number;
  maxBytes: number;
  maxFileBytes: number;
}

export const DEFAULT_FOLDER_LIMITS: FolderLimits = {
  maxFiles: MAX_FOLDER_FILES,
  maxBytes: MAX_FOLDER_BYTES,
  maxFileBytes: MAX_FILE_BYTES,
};

/**
 * Builds a folder selection from files with full relative paths
 * (`Folder/sub/file.txt`, as in `webkitRelativePath`).
 */
export function selectFolder(
  entries: { file: Blob; relativePath: string }[],
  limits: FolderLimits = DEFAULT_FOLDER_LIMITS,
): { selection: FolderSelection; problem: FolderProblem | null } {
  let name = '';
  let skipped = 0;
  let totalBytes = 0;
  const files: FolderFile[] = [];
  for (const { file, relativePath } of entries) {
    const parts = relativePath.split(/[/\\]/).filter(Boolean);
    if (parts.length < 2) {
      // A loose file without a folder: keep it at the top level.
      parts.unshift(name || 'files');
    }
    name ||= parts[0] ?? 'folder';
    const inner = parts.slice(1);
    if (inner.some(isHiddenOrSystem)) {
      skipped += 1;
      continue;
    }
    files.push({ file, path: inner.join('/') });
    totalBytes += file.size;
  }
  const selection = { name: name || 'folder', files, skipped, totalBytes };
  return { selection, problem: checkFolder(selection, limits) };
}

export function checkFolder(
  selection: FolderSelection,
  limits: FolderLimits = DEFAULT_FOLDER_LIMITS,
): FolderProblem | null {
  if (selection.files.length === 0) return { kind: 'empty' };
  if (selection.files.length > limits.maxFiles) {
    return { kind: 'tooManyFiles', count: selection.files.length, max: limits.maxFiles };
  }
  if (selection.totalBytes > limits.maxBytes) {
    return { kind: 'tooLarge', bytes: selection.totalBytes, max: limits.maxBytes };
  }
  const big = selection.files.find((f) => f.file.size > limits.maxFileBytes);
  if (big) {
    return { kind: 'fileTooLarge', name: big.path, bytes: big.file.size, max: limits.maxFileBytes };
  }
  return null;
}

/** Files of a `webkitdirectory` input, with their relative paths. */
export function fromDirectoryInput(files: ArrayLike<File>): { file: File; relativePath: string }[] {
  return Array.from(files, (file) => ({
    file,
    relativePath: file.webkitRelativePath || file.name,
  }));
}

// ------------------------------------------------------------ drag and drop

interface FsEntry {
  isFile: boolean;
  isDirectory: boolean;
  name: string;
  file?(ok: (f: File) => void, err: (e: unknown) => void): void;
  createReader?(): { readEntries(ok: (e: FsEntry[]) => void, err: (e: unknown) => void): void };
}

async function walk(
  entry: FsEntry,
  prefix: string,
  out: { file: File; relativePath: string }[],
  limit: number,
): Promise<void> {
  if (out.length > limit) return;
  const path = prefix ? `${prefix}/${entry.name}` : entry.name;
  if (entry.isFile && entry.file) {
    const file = await new Promise<File>((ok, err) => entry.file!(ok, err));
    out.push({ file, relativePath: path });
    return;
  }
  // Hidden and system folders are skipped without reading them.
  if (!entry.isDirectory || !entry.createReader || (prefix && isHiddenOrSystem(entry.name))) {
    return;
  }
  const reader = entry.createReader();
  for (;;) {
    const batch = await new Promise<FsEntry[]>((ok, err) => reader.readEntries(ok, err));
    if (batch.length === 0) break;
    for (const child of batch) await walk(child, path, out, limit);
  }
}

/**
 * Whether a dropped `File` can be read. Without `webkitGetAsEntry` a dropped
 * folder arrives as a `File` without a type whose contents can't be read.
 */
async function readable(file: Blob): Promise<boolean> {
  try {
    const head = file.slice(0, 1);
    if (typeof head.arrayBuffer === 'function') await head.arrayBuffer();
    else await new Response(head).arrayBuffer();
    return true;
  } catch {
    return false;
  }
}

/**
 * Files and folders dropped on a session (Chromium/WebView2 and WebKit expose
 * `webkitGetAsEntry`). Returns one entry list per dropped folder; plain
 * files are returned separately, and the names of folders that could not be
 * read (no entry API) in `unreadable`.
 *
 * Call it from inside the drop event: the items expire once the event ends.
 */
export async function readDrop(
  items: DataTransferItemList | null,
  files: FileList | ArrayLike<File> | null,
): Promise<{
  folders: { file: File; relativePath: string }[][];
  files: File[];
  unreadable: string[];
}> {
  const folders: { file: File; relativePath: string }[][] = [];
  const loose: File[] = [];
  /** Files that may be folders (no entry to tell): checked by reading them. */
  const unsure: File[] = [];
  const entries: FsEntry[] = [];
  if (items) {
    for (const item of Array.from(items)) {
      if (item.kind !== 'file') continue;
      const getEntry = (item as { webkitGetAsEntry?: () => FsEntry | null }).webkitGetAsEntry;
      const entry = getEntry ? getEntry.call(item) : null;
      if (entry?.isDirectory) entries.push(entry);
      else {
        const file = item.getAsFile();
        if (!file) continue;
        if (!entry && !file.type) unsure.push(file);
        else loose.push(file);
      }
    }
  } else if (files) {
    for (const file of Array.from(files)) (file.type ? loose : unsure).push(file);
  }
  for (const entry of entries) {
    const out: { file: File; relativePath: string }[] = [];
    // Read a little past the cap so the limit check can report it.
    await walk(entry, '', out, MAX_FOLDER_FILES * 2);
    folders.push(out);
  }
  const unreadable: string[] = [];
  for (const file of unsure) {
    if (await readable(file)) loose.push(file);
    else unreadable.push(file.name || 'folder');
  }
  return { folders, files: loose, unreadable };
}
