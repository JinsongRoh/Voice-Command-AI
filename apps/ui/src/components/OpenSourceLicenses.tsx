import { useEffect, useMemo, useRef, useState } from 'react';

import { useI18n } from '../i18n/context';
import {
  NOTICE_GROUPS,
  licenseIds,
  loadNotices,
  matchesQuery,
  type NoticeGroup,
  type NoticePackage,
  type Notices,
} from '../licenses/notices';
import { Icon } from './Icon';
import { Modal } from './Modal';
import styles from './OpenSourceLicenses.module.css';

/**
 * Settings > Open-source licenses: every bundled third-party component with
 * its license text and repository, offline. Loaded lazily (own chunk).
 */
export default function OpenSourceLicenses({ onClose }: { onClose: () => void }) {
  const { t } = useI18n();
  const [notices, setNotices] = useState<Notices | null>(null);
  const [failed, setFailed] = useState(false);
  const [query, setQuery] = useState('');
  const [open, setOpen] = useState<ReadonlySet<NoticeGroup>>(new Set());
  const [selected, setSelected] = useState<NoticePackage | null>(null);

  useEffect(() => {
    let live = true;
    loadNotices().then(
      (data) => live && setNotices(data),
      () => live && setFailed(true),
    );
    return () => {
      live = false;
    };
  }, []);

  return (
    <Modal title={t('oss.title')} onClose={onClose} fullScreen wide>
      {failed ? (
        <p role="alert">{t('oss.loadError')}</p>
      ) : !notices ? (
        <p className="muted">{t('oss.loading')}</p>
      ) : selected ? (
        <PackageDetail pkg={selected} notices={notices} onBack={() => setSelected(null)} />
      ) : (
        <Overview
          notices={notices}
          query={query}
          onQuery={setQuery}
          open={open}
          onToggle={(group, isOpen) =>
            setOpen((prev) => {
              const next = new Set(prev);
              if (isOpen) next.add(group);
              else next.delete(group);
              return next;
            })
          }
          onSelect={setSelected}
        />
      )}
    </Modal>
  );
}

function Overview({
  notices,
  query,
  onQuery,
  open,
  onToggle,
  onSelect,
}: {
  notices: Notices;
  query: string;
  onQuery: (q: string) => void;
  open: ReadonlySet<NoticeGroup>;
  onToggle: (group: NoticeGroup, open: boolean) => void;
  onSelect: (pkg: NoticePackage) => void;
}) {
  const { t } = useI18n();
  const searching = query.trim() !== '';
  const groups = useMemo(
    () =>
      NOTICE_GROUPS.map((group) => ({
        group,
        items: notices.packages.filter((p) => p.g.includes(group) && matchesQuery(p, query)),
      })).filter((g) => g.items.length > 0),
    [notices, query],
  );
  const mpl = notices.packages.filter((p) => licenseIds(p.l).includes('MPL-2.0'));

  return (
    <div className={styles.overview}>
      <p>{t('oss.intro')}</p>

      <section aria-labelledby="oss-counts">
        <h3 id="oss-counts" className={styles.heading}>
          {t('oss.counts')} · {t('oss.total', { count: notices.packages.length })}
        </h3>
        <ul className={styles.counts} aria-labelledby="oss-counts">
          {notices.counts.map(([license, count]) => (
            <li key={license} className={styles.count}>
              <span className={styles.countLicense}>{license}</span>
              <span className={styles.countValue}>{count}</span>
            </li>
          ))}
        </ul>
      </section>

      <div className="field">
        <label htmlFor="oss-search">{t('oss.search')}</label>
        <input
          id="oss-search"
          type="text"
          enterKeyHint="search"
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          autoComplete="off"
          spellCheck={false}
        />
      </div>

      {groups.length === 0 && <p className="muted">{t('oss.noResults')}</p>}
      {groups.map(({ group, items }) => {
        const expanded = searching || open.has(group);
        return (
          <details
            key={group}
            className={styles.group}
            open={expanded}
            onToggle={(e) => {
              if (!searching) onToggle(group, e.currentTarget.open);
            }}
          >
            <summary className={styles.summary}>
              <span>{t(`oss.group.${group}`)}</span>
              <span className={styles.groupCount}>{items.length}</span>
            </summary>
            {expanded && (
              <ul className={styles.list}>
                {items.map((pkg) => (
                  <li key={`${pkg.n}@${pkg.v}`}>
                    <button type="button" className={styles.item} onClick={() => onSelect(pkg)}>
                      <span className={styles.itemName}>{pkg.n}</span>
                      <span className={styles.itemVersion}>{pkg.v}</span>
                      <span className={styles.itemLicense}>{pkg.l}</span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </details>
        );
      })}

      {mpl.length > 0 && (
        <section className={styles.note} aria-labelledby="oss-mpl">
          <h3 id="oss-mpl" className={styles.heading}>
            {t('oss.mplTitle')}
          </h3>
          <p className="muted">{t('oss.mplBody')}</p>
          <ul className={styles.links}>
            {mpl.map((pkg) => (
              <li key={`${pkg.n}@${pkg.v}`}>
                <a href={pkg.s ?? pkg.r} target="_blank" rel="noreferrer">
                  {pkg.n} {pkg.v}
                </a>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className={styles.note} aria-labelledby="oss-not-bundled">
        <h3 id="oss-not-bundled" className={styles.heading}>
          {t('oss.notBundledTitle')}
        </h3>
        <p className="muted">{t('oss.notBundledBody')}</p>
      </section>

      <a className={styles.full} href={notices.notices} target="_blank" rel="noreferrer">
        <Icon name="external" size={14} />
        {t('oss.fullNotices')}
      </a>
    </div>
  );
}

function PackageDetail({
  pkg,
  notices,
  onBack,
}: {
  pkg: NoticePackage;
  notices: Notices;
  onBack: () => void;
}) {
  const { t } = useI18n();
  const top = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    top.current?.focus();
    top.current?.scrollIntoView?.({ block: 'start' });
  }, [pkg]);
  const ids = licenseIds(pkg.l);

  return (
    <article className={styles.detail} aria-labelledby="oss-pkg">
      <button ref={top} type="button" className="btn btn-sm" onClick={onBack}>
        <Icon name="back" size={16} />
        {t('oss.back')}
      </button>
      <h3 id="oss-pkg" className={styles.detailName}>
        {pkg.n} <span className={styles.itemVersion}>{pkg.v}</span>
      </h3>
      <dl className={styles.dl}>
        <dt>{t('oss.license')}</dt>
        <dd>
          {pkg.l}
          {pkg.d !== pkg.l && (
            <span className={styles.declared}> {t('oss.declared', { expr: pkg.d })}</span>
          )}
        </dd>
        <dt>{t('oss.repository')}</dt>
        <dd>
          <a href={pkg.r} target="_blank" rel="noreferrer" className={styles.url}>
            {pkg.r}
          </a>
        </dd>
        {pkg.s && (
          <>
            <dt>{t('oss.source')}</dt>
            <dd>
              <a href={pkg.s} target="_blank" rel="noreferrer" className={styles.url}>
                {pkg.s}
              </a>
            </dd>
          </>
        )}
      </dl>
      {pkg.x && <p className="muted">{pkg.x}</p>}
      <h4 className={styles.subheading}>{t('oss.copyright')}</h4>
      <ul className={styles.copyright}>
        {pkg.c.map((line) => (
          <li key={line}>{line}</li>
        ))}
      </ul>
      {ids.map((id) => {
        const terms = notices.proprietary[id];
        const text = notices.texts[id];
        return (
          <section key={id} aria-label={`${t('oss.licenseText')}: ${id}`}>
            <h4 className={styles.subheading}>
              {t('oss.licenseText')}: {id}
            </h4>
            {terms ? (
              <p>
                <a href={terms.url} target="_blank" rel="noreferrer">
                  {terms.label}
                </a>
              </p>
            ) : text ? (
              <pre className={styles.text}>{text}</pre>
            ) : (
              <p className="muted">{t('oss.noText')}</p>
            )}
          </section>
        );
      })}
    </article>
  );
}
