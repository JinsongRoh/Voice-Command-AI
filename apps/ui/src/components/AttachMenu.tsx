import { useEffect, useId, useRef, useState, type ChangeEvent } from 'react';

import { detectPlatform, type PickerPlatform } from '../attachments/platform';
import { useI18n } from '../i18n/context';
import styles from './Attachments.module.css';

export interface AttachMenuProps {
  disabled?: boolean;
  onFiles(files: File[]): void;
  /** Files of a picked folder (`webkitRelativePath` set); desktop only. */
  onFolder?(files: File[]): void;
  /** Overrides platform detection (tests). */
  platform?: PickerPlatform;
  autoSend: boolean;
  onAutoSendChange(value: boolean): void;
  /** Opens the drawing sheet (hand-drawn sketch or a marked-up photo). */
  onSketch?(): void;
  /** An unsent drawing is waiting (read when the menu opens). */
  hasSketchDraft?(): boolean;
}

function Paperclip() {
  return (
    <svg
      width={24}
      height={24}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d="M20 11.5l-8.2 8.2a5 5 0 0 1-7.1-7.1l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7l-8.5 8.5a1.7 1.7 0 0 1-2.4-2.4L14.8 7" />
    </svg>
  );
}

/**
 * Paperclip button with a small menu: photo library, camera (phones and
 * tablets), sketch (draw, or mark up a photo), files (device storage and
 * cloud providers) and folder (desktop).
 * Each entry is a standard `<input type="file">`, which WKWebView, WebView2
 * and Android WebView hand to the system picker.
 */
export function AttachMenu({
  disabled,
  onFiles,
  onFolder,
  platform: platformOverride,
  autoSend,
  onAutoSendChange,
  onSketch,
  hasSketchDraft,
}: AttachMenuProps) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const [platform] = useState(() => platformOverride ?? detectPlatform());
  const menuId = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const photosRef = useRef<HTMLInputElement>(null);
  const cameraRef = useRef<HTMLInputElement>(null);
  const filesRef = useRef<HTMLInputElement>(null);
  const folderRef = useRef<HTMLInputElement>(null);
  const showFolder = platform.folders && onFolder !== undefined;

  // `webkitdirectory` is not a React prop; set it on the element.
  useEffect(() => {
    const input = folderRef.current;
    if (!input) return;
    input.setAttribute('webkitdirectory', '');
    input.setAttribute('directory', '');
  }, [showFolder]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setOpen(false);
        buttonRef.current?.focus();
      }
    };
    document.addEventListener('pointerdown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('pointerdown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  const pick = (input: HTMLInputElement | null) => {
    setOpen(false);
    input?.click();
  };

  const onChange = (e: ChangeEvent<HTMLInputElement>, folder = false) => {
    const files = Array.from(e.target.files ?? []);
    // Allow picking the same file again.
    e.target.value = '';
    if (files.length === 0) return;
    if (folder) onFolder?.(files);
    else onFiles(files);
  };

  return (
    <div className={styles.attach} ref={rootRef}>
      <button
        ref={buttonRef}
        type="button"
        className={styles.attachButton}
        aria-label={t('attach.button')}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        disabled={disabled}
        onClick={() => setOpen((v) => !v)}
      >
        <Paperclip />
      </button>
      {open && (
        <div id={menuId} role="menu" className={styles.menu} aria-label={t('attach.button')}>
          <button
            type="button"
            role="menuitem"
            className={styles.menuItem}
            onClick={() => pick(photosRef.current)}
          >
            <span>{t('attach.photos')}</span>
          </button>
          {platform.mobile && (
            <button
              type="button"
              role="menuitem"
              className={styles.menuItem}
              onClick={() => pick(cameraRef.current)}
            >
              <span>{t('attach.camera')}</span>
            </button>
          )}
          {onSketch && (
            <button
              type="button"
              role="menuitem"
              className={styles.menuItem}
              onClick={() => {
                setOpen(false);
                // Focus returns here when the sheet closes.
                buttonRef.current?.focus();
                onSketch();
              }}
            >
              <span>{t('attach.sketch')}</span>
              <span className={styles.menuHint}>
                {t(hasSketchDraft?.() ? 'attach.sketchDraft' : 'attach.sketchHint')}
              </span>
            </button>
          )}
          <button
            type="button"
            role="menuitem"
            className={styles.menuItem}
            onClick={() => pick(filesRef.current)}
          >
            <span>{t('attach.files')}</span>
            <span className={styles.menuHint}>{t('attach.filesHint')}</span>
          </button>
          {showFolder && (
            <button
              type="button"
              role="menuitem"
              className={styles.menuItem}
              onClick={() => pick(folderRef.current)}
            >
              <span>{t('attach.folder')}</span>
              <span className={styles.menuHint}>{t('attach.folderHint')}</span>
            </button>
          )}
          <label className={styles.menuToggle}>
            <input
              type="checkbox"
              checked={autoSend}
              onChange={(e) => onAutoSendChange(e.target.checked)}
            />
            <span>{t('attach.autoSend')}</span>
          </label>
        </div>
      )}
      <input
        ref={photosRef}
        type="file"
        accept="image/*"
        multiple
        hidden
        data-testid="attach-photos"
        onChange={(e) => onChange(e)}
      />
      <input
        ref={cameraRef}
        type="file"
        accept="image/*"
        capture="environment"
        hidden
        data-testid="attach-camera"
        onChange={(e) => onChange(e)}
      />
      {/* No `accept` and no `capture`: iOS opens the Files browser (On My
          iPhone, iCloud Drive, Google Drive, Dropbox, ...), Android the
          document picker (Downloads, storage, Drive), desktop the file dialog. */}
      <input
        ref={filesRef}
        type="file"
        multiple
        hidden
        data-testid="attach-files"
        onChange={(e) => onChange(e)}
      />
      {showFolder && (
        <input
          ref={folderRef}
          type="file"
          multiple
          hidden
          data-testid="attach-folder"
          onChange={(e) => onChange(e, true)}
        />
      )}
    </div>
  );
}
