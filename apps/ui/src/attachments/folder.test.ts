import { describe, expect, it } from 'vitest';

import { detectPlatform } from './platform';
import { checkFolder, fromDirectoryInput, isHiddenOrSystem, selectFolder } from './folder';

const blob = (n: number) => new Blob([new Uint8Array(n)]);

describe('selectFolder', () => {
  it('keeps relative paths and skips hidden and system entries', () => {
    const { selection, problem } = selectFolder([
      { file: blob(5), relativePath: 'project/README.md' },
      { file: blob(7), relativePath: 'project/src/main.rs' },
      { file: blob(1), relativePath: 'project/.git/config' },
      { file: blob(1), relativePath: 'project/node_modules/x/index.js' },
      { file: blob(1), relativePath: 'project/docs/.DS_Store' },
      { file: blob(1), relativePath: 'project/Thumbs.db' },
      { file: blob(1), relativePath: 'project/sub\\desktop.ini' },
    ]);
    expect(problem).toBeNull();
    expect(selection.name).toBe('project');
    expect(selection.files.map((f) => f.path)).toEqual(['README.md', 'src/main.rs']);
    expect(selection.skipped).toBe(5);
    expect(selection.totalBytes).toBe(12);
  });

  it('reports the caps', () => {
    const limits = { maxFiles: 2, maxBytes: 10, maxFileBytes: 6 };
    const three = [1, 2, 3].map((i) => ({ file: blob(1), relativePath: `f/${i}.txt` }));
    expect(selectFolder(three, limits).problem).toEqual({ kind: 'tooManyFiles', count: 3, max: 2 });
    const big = [
      { file: blob(6), relativePath: 'f/a' },
      { file: blob(6), relativePath: 'f/b' },
    ];
    expect(selectFolder(big, limits).problem).toEqual({ kind: 'tooLarge', bytes: 12, max: 10 });
    const oneBig = [{ file: blob(7), relativePath: 'f/huge.bin' }];
    expect(selectFolder(oneBig, limits).problem).toMatchObject({
      kind: 'fileTooLarge',
      name: 'huge.bin',
    });
    const hiddenOnly = [{ file: blob(1), relativePath: 'f/.env' }];
    expect(selectFolder(hiddenOnly, limits).problem).toEqual({ kind: 'empty' });
    expect(checkFolder({ name: 'x', files: [], skipped: 0, totalBytes: 0 })).toEqual({
      kind: 'empty',
    });
  });

  it('reads webkitRelativePath from directory inputs', () => {
    const file = new File(['x'], 'a.txt');
    Object.defineProperty(file, 'webkitRelativePath', { value: 'dir/sub/a.txt' });
    expect(fromDirectoryInput([file])).toEqual([{ file, relativePath: 'dir/sub/a.txt' }]);
    expect(isHiddenOrSystem('NODE_MODULES')).toBe(true);
    expect(isHiddenOrSystem('src')).toBe(false);
  });
});

describe('detectPlatform', () => {
  const doc = document;
  it('offers folders on desktop only', () => {
    const desktop = detectPlatform(
      { userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) Edg/140', maxTouchPoints: 0 },
      doc,
    );
    expect(desktop.mobile).toBe(false);
    expect(desktop.folders).toBe('webkitdirectory' in doc.createElement('input'));
    for (const ua of [
      'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15',
      'Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 Mobile Safari/537.36',
    ]) {
      expect(detectPlatform({ userAgent: ua, maxTouchPoints: 5 }, doc)).toEqual({
        mobile: true,
        folders: false,
      });
    }
    // iPadOS reports a Mac user agent with touch.
    const ipad = detectPlatform(
      { userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)', maxTouchPoints: 5 },
      doc,
    );
    expect(ipad.mobile).toBe(true);
  });
});
