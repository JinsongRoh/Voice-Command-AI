import { useId, useState } from 'react';

import { useMediaQuery } from '../hooks/useMediaQuery';
import { useI18n } from '../i18n/context';
import {
  MIC_ANIMATION_CODES,
  MIC_ANIMATION_GROUPS,
  type MicAnimation,
} from '../state/micAnimation';
import { useSettings } from '../state/store';
import { MicSymbol } from './MicSymbol';
import { REDUCED_MOTION_QUERY } from './VoiceWave';
import styles from './MicAnimationSettings.module.css';

/**
 * Settings › Voice: the dictation button's animation. Every style has a live
 * mini button; only the selected one and the one under the pointer or focus
 * move (never all 18 at once), and choosing a style plays it. Collapsed, it is
 * one row with the current style; all 18 stay one click away.
 */
export function MicAnimationSettings({ defaultExpanded = false }: { defaultExpanded?: boolean }) {
  const { t } = useI18n();
  const { settings, updateSettings } = useSettings();
  const reduced = useMediaQuery(REDUCED_MOTION_QUERY);
  const uid = useId();
  const [hovered, setHovered] = useState<MicAnimation | null>(null);
  const [focused, setFocused] = useState<MicAnimation | null>(null);
  /** The last style chosen (or tapped again) and how often: replays its assembly. */
  const [replay, setReplay] = useState<{ id: MicAnimation | null; n: number }>({
    id: null,
    n: 0,
  });
  const [expanded, setExpanded] = useState(defaultExpanded);
  const selected = settings.micAnimation;
  const labelId = `${uid}-label`;
  const helpId = `${uid}-help`;
  const pickerId = `${uid}-picker`;

  return (
    <div className="field">
      <span className="label" id={labelId}>
        {t('settings.micAnimation')}
      </span>
      <p id={helpId} className={`muted ${styles.help}`}>
        {t(reduced ? 'settings.micAnimationReduced' : 'settings.micAnimationHelp')}
      </p>
      <div className={styles.summary} data-testid="mic-animation-current">
        <span className={styles.preview} aria-hidden="true">
          <MicSymbol
            animation={selected}
            phase={reduced ? 'idle' : 'recording'}
            replay={0}
            logoClassName={styles.previewLogo}
          />
        </span>
        <span className={styles.text}>
          <span className={styles.name}>
            <span className={styles.code} aria-hidden="true">
              {MIC_ANIMATION_CODES[selected]}
            </span>
            {t(`settings.micAnimation.${selected}`)}
          </span>
          <span className={styles.desc}>{t(`settings.micAnimation.${selected}.desc`)}</span>
        </span>
        <button
          type="button"
          className={`btn btn-sm ${styles.toggle}`}
          aria-expanded={expanded}
          aria-controls={pickerId}
          onClick={() => setExpanded((v) => !v)}
        >
          {t(expanded ? 'settings.micAnimation.hide' : 'settings.micAnimation.change')}
        </button>
      </div>
      <div
        id={pickerId}
        className={styles.groups}
        role="group"
        aria-labelledby={labelId}
        aria-describedby={helpId}
        data-testid="mic-animation-picker"
        hidden={!expanded}
      >
        {MIC_ANIMATION_GROUPS.map((group) => (
          <fieldset key={group.kind} className={styles.group}>
            <legend className={styles.legend}>
              {t(`settings.micAnimation.group.${group.kind}`)}
            </legend>
            <div className={styles.grid}>
              {group.items.map((id) => {
                const inputId = `${uid}-${id}`;
                const nameId = `${inputId}-name`;
                const descId = `${inputId}-desc`;
                const isSelected = selected === id;
                const active = !reduced && (isSelected || hovered === id || focused === id);
                return (
                  <label
                    key={id}
                    htmlFor={inputId}
                    className={styles.option}
                    data-selected={isSelected || undefined}
                    data-active={active || undefined}
                    onMouseEnter={() => setHovered(id)}
                    onMouseLeave={() => setHovered((h) => (h === id ? null : h))}
                  >
                    <input
                      id={inputId}
                      type="radio"
                      className={styles.radio}
                      name={`${uid}-mic-animation`}
                      value={id}
                      checked={isSelected}
                      aria-labelledby={nameId}
                      aria-describedby={descId}
                      onChange={() => updateSettings({ micAnimation: id })}
                      // A click (also on the chosen one) or an arrow key plays it.
                      onClick={() => setReplay((r) => ({ id, n: r.n + 1 }))}
                      onFocus={() => setFocused(id)}
                      onBlur={() => setFocused((f) => (f === id ? null : f))}
                    />
                    <span className={styles.preview} aria-hidden="true">
                      <MicSymbol
                        animation={id}
                        phase={active ? 'recording' : 'idle'}
                        replay={replay.id === id ? replay.n : 0}
                        logoClassName={styles.previewLogo}
                      />
                    </span>
                    <span className={styles.text}>
                      <span className={styles.name} id={nameId}>
                        <span className={styles.code} aria-hidden="true">
                          {MIC_ANIMATION_CODES[id]}
                        </span>
                        {t(`settings.micAnimation.${id}`)}
                      </span>
                      <span className={styles.desc} id={descId}>
                        {t(`settings.micAnimation.${id}.desc`)}
                      </span>
                    </span>
                  </label>
                );
              })}
            </div>
          </fieldset>
        ))}
      </div>
    </div>
  );
}
