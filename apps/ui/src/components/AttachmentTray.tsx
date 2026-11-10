import { useState } from 'react';

import { formatBytes, type AttachmentItem } from '../attachments/useAttachments';
import { useI18n } from '../i18n/context';
import { middleEllipsis } from '../protocol/helpers';
import { Icon } from './Icon';
import styles from './Attachments.module.css';

export interface AttachmentTrayProps {
  items: AttachmentItem[];
  /**
   * One line instead of the chips ("사진 5장 첨부됨 · 올리는 중 3/5"): typing
   * with the on-screen keyboard open leaves no room for thumbnails.
   */
  summary?: boolean;
  /** The composer can take the mention (otherwise "mention it" is shown). */
  canPrefill: boolean;
  onCancel(id: string): void;
  onRetry(id: string): void;
  onDismiss(id: string): void;
}

/** Long names keep their start and their extension: `very-long-scre…shot.png`. */
const NAME_CHARS = 28;

function percent(item: AttachmentItem): number {
  if (item.size <= 0) return item.status === 'done' ? 100 : 0;
  return Math.min(100, Math.round((item.sent / item.size) * 100));
}

/** Counts for the one-line summary (cancelled items are left out). */
function trayCounts(items: readonly AttachmentItem[]) {
  const kept = items.filter((it) => it.status !== 'cancelled');
  return {
    photos: kept.filter((it) => it.image).length,
    files: kept.filter((it) => !it.image).length,
    done: kept.filter((it) => it.status === 'done').length,
    failed: kept.filter((it) => it.status === 'error').length,
    total: kept.length,
  };
}

/** Upload chips: thumbnail or icon, name and size, progress, cancel and retry. */
export function AttachmentTray({
  items,
  summary = false,
  canPrefill,
  onCancel,
  onRetry,
  onDismiss,
}: AttachmentTrayProps) {
  const { t } = useI18n();
  const [details, setDetails] = useState<Set<string>>(() => new Set());
  if (items.length === 0) return null;
  if (summary) {
    const c = trayCounts(items);
    const parts = [
      c.photos > 0 ? t('attach.summaryPhotos', { count: c.photos }) : '',
      c.files > 0 ? t('attach.summaryFiles', { count: c.files }) : '',
      c.done + c.failed < c.total
        ? t('attach.summaryUploading', { done: c.done, total: c.total })
        : '',
      c.failed > 0 ? t('attach.summaryFailed', { count: c.failed }) : '',
    ].filter(Boolean);
    if (parts.length === 0) return null;
    return (
      <p className={styles.traySummary} role="status" data-testid="attach-summary">
        <Icon name="paperclip" size={14} />
        <span className={styles.traySummaryText}>{parts.join(' · ')}</span>
      </p>
    );
  }
  // "Photo 12/50: preparing…" while a batch of photos goes through.
  const photos = items.filter((it) => it.image && it.status !== 'cancelled');
  const photosActive = photos.filter(
    (it) => it.status === 'queued' || it.status === 'preparing' || it.status === 'uploading',
  );
  const batch =
    photos.length >= 2 && photosActive.length > 0
      ? t(
          photosActive[0]?.status === 'uploading'
            ? 'attach.batchUploading'
            : 'attach.batchPreparing',
          { index: photos.length - photosActive.length + 1, total: photos.length },
        )
      : null;
  const toggleDetails = (id: string) =>
    setDetails((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  return (
    <ul className={styles.tray} aria-label={t('attach.tray')}>
      {batch && (
        <li className={styles.chipMeta} role="status" data-testid="attach-batch">
          {batch}
        </li>
      )}
      {items.map((item) => {
        const active =
          item.status === 'queued' || item.status === 'preparing' || item.status === 'uploading';
        const pct = percent(item);
        const shortName = middleEllipsis(item.name, NAME_CHARS);
        const showDetail = item.status === 'error' && item.errorDetail && details.has(item.id);
        const sizeText =
          item.kind === 'folder'
            ? t('attach.folderSize', { count: item.files, size: formatBytes(item.size) })
            : formatBytes(item.size);
        let status: string;
        switch (item.status) {
          case 'queued':
            status = t('attach.queued');
            break;
          case 'preparing':
            status = t('attach.preparing');
            break;
          case 'uploading':
            status = t('attach.uploading', { percent: pct });
            break;
          case 'done':
            status = item.result?.typed
              ? t('attach.typed')
              : canPrefill
                ? t('attach.prefilled')
                : t('attach.savedMention', { path: item.result?.path ?? '' });
            break;
          case 'cancelled':
            status = t('attach.cancelled');
            break;
          case 'error':
            status = item.error ?? t('attach.failed');
            break;
        }
        return (
          <li key={item.id} className={styles.chip} data-status={item.status}>
            {item.thumbUrl ? (
              <img className={styles.thumb} src={item.thumbUrl} alt="" />
            ) : (
              <span className={styles.thumbIcon} aria-hidden="true">
                {item.kind === 'folder' ? '▤' : '▢'}
              </span>
            )}
            <span className={styles.chipBody}>
              <span className={styles.chipName} title={item.name}>
                {item.status === 'done' ? t('attach.attached', { name: shortName }) : shortName}
              </span>
              <span className={styles.chipMeta}>
                {sizeText}
                {item.skipped > 0 && (
                  <>
                    <span aria-hidden="true"> · </span>
                    {t('attach.skipped', { count: item.skipped })}
                  </>
                )}
              </span>
              <span
                className={styles.chipStatus}
                role={item.status === 'error' ? 'alert' : 'status'}
                title={item.status === 'error' ? (item.errorDetail ?? undefined) : undefined}
              >
                {status}
              </span>
              {item.status === 'error' && item.errorDetail && (
                <button
                  type="button"
                  className={styles.detailToggle}
                  aria-expanded={Boolean(showDetail)}
                  onClick={() => toggleDetails(item.id)}
                >
                  {t(showDetail ? 'attach.hideDetails' : 'attach.details')}
                </button>
              )}
              {showDetail && <span className={styles.chipDetail}>{item.errorDetail}</span>}
              {active && (
                <span
                  className={styles.progress}
                  role="progressbar"
                  aria-label={t('attach.progress', { name: item.name })}
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={pct}
                >
                  <span className={styles.progressBar} style={{ width: `${pct}%` }} />
                </span>
              )}
            </span>
            {(item.status === 'error' || item.status === 'cancelled') && (
              <button
                type="button"
                className={styles.chipAction}
                onClick={() => onRetry(item.id)}
                aria-label={t('attach.retry', { name: item.name })}
              >
                <Icon name="restart" size={16} />
              </button>
            )}
            <button
              type="button"
              className={styles.chipAction}
              onClick={() => (active ? onCancel(item.id) : onDismiss(item.id))}
              aria-label={t(active ? 'attach.cancel' : 'attach.dismiss', { name: item.name })}
            >
              <Icon name="close" size={16} />
            </button>
          </li>
        );
      })}
    </ul>
  );
}
