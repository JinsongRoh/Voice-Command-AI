import styles from './Gauge.module.css';

/** Semi-circular gauge. `value` is clamped to 0..1. */
export function Gauge({
  label,
  value,
  valueText,
  detail,
}: {
  label: string;
  value: number;
  valueText: string;
  detail?: string;
}) {
  const v = Math.max(0, Math.min(1, Number.isFinite(value) ? value : 0));
  const r = 40;
  const circumference = Math.PI * r;
  const level = v >= 0.85 ? 'high' : v >= 0.6 ? 'mid' : 'low';
  return (
    <figure className={styles.gauge} data-level={level}>
      <svg
        viewBox="0 0 100 58"
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(v * 100)}
        aria-valuetext={valueText}
      >
        <path className={styles.track} d="M10 50 A40 40 0 0 1 90 50" pathLength={circumference} />
        <path
          className={styles.bar}
          d="M10 50 A40 40 0 0 1 90 50"
          pathLength={circumference}
          strokeDasharray={`${v * circumference} ${circumference}`}
        />
        <text x="50" y="47" textAnchor="middle" className={styles.value}>
          {valueText}
        </text>
      </svg>
      <figcaption className={styles.caption}>
        <span className={styles.label}>{label}</span>
        {detail && <span className={styles.detail}>{detail}</span>}
      </figcaption>
    </figure>
  );
}
