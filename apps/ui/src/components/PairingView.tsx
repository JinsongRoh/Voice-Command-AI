import { useEffect, useRef, useState, type FormEvent, type ReactNode } from 'react';

import { DemoEntry } from '../demo/DemoBanner';
import { useI18n } from '../i18n/context';
import { NativeError } from '../native/bridge';
import { useNative, type NativeServices } from '../native/context';
import { describePairingError, isTotpPrompt } from '../native/errors';
import { BrandLogo } from './BrandLogo';
import { ConfirmCode } from './ConfirmCode';
import { DnsPilotPanel } from './DnsPilotConnect';
import { ConnectPhoneCard } from './ConnectPhoneCard';
import { Icon } from './Icon';
import { openModalLayer } from './Modal';
import { ENROLL_RETRY_MS, retryWhileEnrolling, secondsUntil } from '../native/enrollWait';
import { classifyLink, linkRequiresTotp } from '../native/pairLinks';
import { canScanQr, type QrScanResult } from '../native/qrScanner';
import styles from './PairingView.module.css';

/** Set on `<html>` while the camera scans (see `PairingView.module.css`). */
const SCANNING_CLASS = 'vca-scanning';

const PLATFORM_NAMES: Record<string, string> = {
  windows: 'Windows',
  macos: 'Mac',
  linux: 'Linux',
  android: 'Android',
  ios: 'iPhone',
};

/** Pairing links look like `vca://pair?...`. */

/**
 * Device pairing (SEC-01/02). In the native shell the pairing link is pasted
 * and handed to Rust (`vca_pair`), which runs the Noise handshake; in the
 * browser/mock build this is an explanatory placeholder.
 */
export function PairingView({
  onPaired,
  intro,
  onConnectPhone,
  enrollRetryMs,
  lead,
  onTryDemo,
}: {
  onPaired?: (machineId: string) => void;
  /** Optional lead text (e.g. when no machine is paired yet). */
  intro?: string;
  /** Desktop: opens "Connect a phone" (pair a phone with this PC). */
  onConnectPhone?: () => void;
  /** Delay between connection attempts after an invite (tests). */
  enrollRetryMs?: number;
  /** Shown first, under the logo (desktop setup: this PC's own server). */
  lead?: ReactNode;
  /** First run: "Try the demo" right under the logo (no computer needed). */
  onTryDemo?: () => void;
}) {
  const native = useNative();
  const { t } = useI18n();
  return (
    <div className={styles.screen}>
      <PairingHero />
      {/* Before everything else, so it shows without scrolling on a phone. */}
      {onTryDemo && <DemoEntry onTry={onTryDemo} />}
      {lead}
      {onConnectPhone && <ConnectPhoneCard onOpen={onConnectPhone} />}
      {native ? (
        <NativePairing
          native={native}
          onPaired={onPaired}
          intro={intro}
          // Desktop: this form adds a remote computer; phones use the card above.
          title={onConnectPhone ? t('host.addComputer') : undefined}
          help={onConnectPhone ? t('host.addComputerHelp') : undefined}
          // Phones: the order of steps across PC and phone.
          guide={!onConnectPhone}
          enrollRetryMs={enrollRetryMs}
        />
      ) : (
        <PairingPlaceholder intro={intro} />
      )}
    </div>
  );
}

/** Brand hero: the vertical lockup with the slogan. */
function PairingHero() {
  const { t } = useI18n();
  return (
    <div className={styles.hero}>
      <BrandLogo
        variant="vertical"
        tone="gradient"
        title={t('app.title')}
        className={styles.heroLogo}
      />
      <p className={styles.slogan}>{t('brand.slogan')}</p>
    </div>
  );
}

function PairingPlaceholder({ intro }: { intro?: string }) {
  const { t } = useI18n();
  return (
    <section className={`panel ${styles.pairing}`} aria-labelledby="pair-title">
      <h2 id="pair-title">{t('pairing.title')}</h2>
      {intro && <p>{intro}</p>}
      <div className={styles.qr} aria-hidden="true">
        <Icon name="qr" size={96} />
      </div>
      <ol className={styles.steps}>
        <li>{t('pairing.step1')}</li>
        <li>{t('pairing.step2')}</li>
        <li>{t('pairing.step3')}</li>
      </ol>
      <button
        type="button"
        className="btn btn-primary"
        disabled
        aria-describedby="pair-unavailable"
      >
        <Icon name="qr" size={18} />
        {t('pairing.scan')}
      </button>
      <p id="pair-unavailable" className="muted">
        {t('pairing.unavailable')}
      </p>
      <p className={styles.note}>{t('pairing.note')}</p>
    </section>
  );
}

function NativePairing({
  native,
  onPaired,
  intro,
  title,
  help,
  guide = false,
  enrollRetryMs = ENROLL_RETRY_MS,
}: {
  native: NativeServices;
  onPaired?: (machineId: string) => void;
  intro?: string;
  title?: string;
  help?: string;
  guide?: boolean;
  enrollRetryMs?: number;
}) {
  const { t } = useI18n();
  const [uri, setUriValue] = useState('');
  /** The field holds a link from this app's own QR scanner. */
  const fromScanner = useRef(false);
  const setUri = (value: string, scanned = false) => {
    fromScanner.current = scanned;
    setCodeMatches(false);
    setUriValue(value);
  };
  /** The code of a link, to compare with the PC. */
  const [linkCode, setLinkCode] = useState<{ link: string; code: string } | null>(null);
  const confirmCode = linkCode?.link === uri.trim() ? linkCode.code : null;
  /** The person says the PC shows the same code. */
  const [codeMatches, setCodeMatches] = useState(false);
  useEffect(() => {
    let active = true;
    const link = uri.trim();
    if (classifyLink(link) === null || classifyLink(link) === 'enroll') return;
    native
      .pairingCode(link)
      .then((code) => {
        if (active) setLinkCode({ link, code });
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [native, uri]);
  // A `vca://pair` link that opened the app only fills the field.
  useEffect(() => {
    const links = native.pairLinks;
    if (!links) return;
    const fill = () => {
      const link = links.take();
      if (link) setUri(link);
    };
    fill();
    return links.subscribe(fill);
  }, [native]);
  const [deviceName, setDeviceName] = useState('');
  const [nameTouched, setNameTouched] = useState(false);
  const [totp, setTotp] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Raw code of the last failure, shown small for support. */
  const [errorCode, setErrorCode] = useState<string | null>(null);
  /** The server asked for a 2-step verification code for this link. */
  const [codePrompt, setCodePrompt] = useState(false);
  /** Bumped to move the focus to the code field. */
  const [focusCode, setFocusCode] = useState(0);
  const totpRef = useRef<HTMLInputElement>(null);
  /** "My PC shows the same code": checked before the code is typed. */
  const confirmBoxRef = useRef<HTMLInputElement>(null);
  const handledFocus = useRef(0);
  const [paired, setPaired] = useState<string | null>(null);
  const [platform, setPlatform] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [scanNotice, setScanNotice] = useState<string | null>(null);
  const [cameraDenied, setCameraDenied] = useState(false);
  /** Seconds announced after an invite, while connections may be refused. */
  const [waitSeconds, setWaitSeconds] = useState<number | null>(null);
  /** Until when (ms) a refused connection means "the Agent is not ready yet". */
  const readyAt = useRef(0);
  /** The link already enrolled (its code works once). */
  const enrolledLink = useRef<string | null>(null);
  const scanner = native.qrScanner;
  const canScan = !!scanner && canScanQr(platform);
  // The link says so (`tf=1`), or the server asked for it.
  const codeRequired = codePrompt || linkRequiresTotp(uri);

  // The code step: first the "same code" check (when the link carries a
  // confirm code), then the code field. The confirm code may arrive a moment
  // after the link: the focus moves to it then, unless the person moved on.
  useEffect(() => {
    if (focusCode === 0) return;
    const fresh = handledFocus.current !== focusCode;
    handledFocus.current = focusCode;
    const active = document.activeElement;
    if (!fresh && active && active !== document.body && active !== totpRef.current) return;
    const box = confirmBoxRef.current;
    if (box && !box.checked && !totpRef.current?.value) box.focus();
    else if (fresh) totpRef.current?.focus();
  }, [focusCode, confirmCode]);

  /** Shows the 2-step verification prompt and focuses the code field. */
  const askForCode = (message: string) => {
    setCodePrompt(true);
    setError(message);
    setFocusCode((n) => n + 1);
  };

  // The platform: default device name ("My Android") and whether the camera
  // can scan (phones only).
  useEffect(() => {
    let active = true;
    native
      .platform()
      .then((p) => {
        if (active) setPlatform(p);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [native]);
  // Until the user types a name, it follows the platform and the language.
  const name = nameTouched
    ? deviceName
    : platform === null
      ? ''
      : t('pairing.defaultDevice', { platform: PLATFORM_NAMES[platform] ?? platform });

  // While the camera runs it is drawn behind the webview: the page turns
  // transparent and only the scan overlay stays visible. Leaving the screen
  // stops the scan.
  useEffect(() => {
    if (!scanning) return;
    const root = document.documentElement;
    root.classList.add(SCANNING_CLASS);
    return () => {
      root.classList.remove(SCANNING_CLASS);
    };
  }, [scanning]);
  const scanInFlight = useRef(false);
  useEffect(
    () => () => {
      if (scanInFlight.current) void scanner?.cancel();
    },
    [scanner],
  );

  // The wait message goes away with the window.
  useEffect(() => {
    if (waitSeconds === null) return;
    const id = setTimeout(() => setWaitSeconds(null), waitSeconds * 1000);
    return () => clearTimeout(id);
  }, [waitSeconds]);

  const showWait = () => {
    const seconds = secondsUntil(readyAt.current);
    setWaitSeconds(seconds > 0 ? seconds : null);
  };

  /** Registers this app's key with the link's invite, once per link. */
  const enroll = async (link: string) => {
    if (enrolledLink.current === link) return;
    if (!native.dnspilot) throw new NativeError('invalid_uri', 'DNSPilot is not available');
    const enrollment = await native.dnspilot.enroll(link);
    enrolledLink.current = link;
    if (enrollment) readyAt.current = Date.now() + enrollment.readyWithinSec * 1000;
  };

  /**
   * Handles an accepted link. A link from the camera only pairs once a code
   * is in the field (or the user taps Pair). The server spends the one-time
   * pairing secret only after the code checks out, so a missing or wrong
   * code can be retried with the same link until it expires.
   */
  const process = async (link: string, scanned: boolean, typed?: string) => {
    const kind = classifyLink(link);
    setErrorCode(null);
    if (kind === null) {
      setError(t('native.error.invalid_uri'));
      return;
    }
    const code = (typed ?? totp).trim();
    if (kind !== 'enroll' && !code && !scanned && (codePrompt || linkRequiresTotp(link))) {
      // Known to be needed: ask before contacting the PC.
      askForCode(t('pairing.totpPrompt'));
      return;
    }
    setError(null);
    setPaired(null);
    setScanNotice(null);
    setCameraDenied(false);
    setBusy(true);
    try {
      if (kind !== 'pair') await enroll(link);
      if (kind === 'enroll') {
        setUri('');
        setScanNotice(t('pairing.enrollNext'));
        return;
      }
      showWait();
      if (scanned && !code) {
        if (linkRequiresTotp(link)) askForCode(t('pairing.totpPrompt'));
        else setScanNotice(t('pairing.enrollEnterCode'));
        return;
      }
      // A forged link would send the code to someone else's server. No code
      // to compare (not loaded, or the link is broken) means no.
      if (!confirmCode || !codeMatches) {
        setError(t('pairing.confirmFirst'));
        return;
      }
      const info = await retryWhileEnrolling(
        () => native.pair(link, name.trim() || 'VCA', code || null),
        { until: readyAt.current, retryMs: enrollRetryMs },
      );
      readyAt.current = 0;
      enrolledLink.current = null;
      setWaitSeconds(null);
      setPaired(t('pairing.success', { name: info.name }));
      setUri('');
      setTotp('');
      setCodePrompt(false);
      onPaired?.(info.id);
    } catch (err) {
      setWaitSeconds(null);
      const failure = describePairingError(t, err);
      setErrorCode(failure.code);
      if (isTotpPrompt(failure.code)) {
        // Same link, same form: only the code is missing or wrong.
        setTotp('');
        askForCode(failure.message);
      } else {
        setError(failure.message);
      }
    } finally {
      setBusy(false);
    }
  };

  const startScan = async () => {
    if (!scanner || scanInFlight.current || busy) return;
    setError(null);
    setPaired(null);
    setScanNotice(null);
    setCameraDenied(false);
    setScanning(true);
    scanInFlight.current = true;
    let result: QrScanResult;
    try {
      result = await scanner.scan();
    } catch (err) {
      result = { kind: 'error', message: String(err) };
    } finally {
      scanInFlight.current = false;
    }
    setScanning(false);
    switch (result.kind) {
      case 'link':
        setUri(result.uri, true);
        if (classifyLink(result.uri) === 'pair') {
          // Only fills the field: pairing still needs the tap on Pair (or,
          // when the PC asks for a code, its sixth digit).
          if (linkRequiresTotp(result.uri)) askForCode(t('pairing.totpPrompt'));
          else setScanNotice(t('pairing.scanFilled'));
        } else {
          await process(result.uri, true);
        }
        break;
      case 'invalid':
        setError(t('pairing.scanInvalid'));
        break;
      case 'cancelled':
        setScanNotice(t('pairing.scanCancelled'));
        break;
      case 'denied':
        setCameraDenied(true);
        setError(t('pairing.scanDenied'));
        break;
      case 'error':
        setError(t('pairing.scanFailed', { message: result.message }));
        break;
    }
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    await process(uri.trim(), false);
  };

  const onCodeChange = (value: string) => {
    const digits = value.replace(/\D/g, '').slice(0, 6);
    setTotp(digits);
    // The sixth digit submits when the code is known to be needed, for a
    // link from the app's own scanner whose code the person checked. A link
    // that arrived any other way never goes without a tap on Pair.
    const link = uri.trim();
    const kind = classifyLink(link);
    if (
      digits.length === 6 &&
      codeRequired &&
      fromScanner.current &&
      confirmCode !== null &&
      codeMatches &&
      !busy &&
      !scanning &&
      kind !== null &&
      kind !== 'enroll'
    ) {
      void process(link, false, digits);
    }
  };

  return (
    <section className={`panel ${styles.pairing}`} aria-labelledby="pair-title">
      <h2 id="pair-title">{title ?? t('pairing.title')}</h2>
      {intro && <p>{intro}</p>}
      {help && <p className={styles.help}>{help}</p>}
      {guide && (
        <ol className={styles.steps} aria-label={t('pairing.guideTitle')}>
          <li>{t('pairing.guide1')}</li>
          <li>{t('pairing.guide2')}</li>
          <li>{t('pairing.guide3')}</li>
          <li>{t('pairing.guide4')}</li>
        </ol>
      )}
      <form className={styles.form} onSubmit={(e) => void submit(e)} noValidate>
        <div className="field">
          <label htmlFor="pair-uri">{t('pairing.uri')}</label>
          <input
            id="pair-uri"
            type="text"
            value={uri}
            onChange={(e) => setUri(e.target.value)}
            placeholder="vca://pair?..."
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            aria-describedby="pair-uri-help"
            required
          />
          <span id="pair-uri-help" className={styles.help}>
            {t('pairing.uriHelp')}
          </span>
        </div>
        {confirmCode && (
          <div className={styles.notice} data-testid="pair-confirm">
            <p>
              {t('pairing.confirmTitle')} <ConfirmCode code={confirmCode} />
            </p>
            <p className={styles.help}>{t('pairing.confirmHelp')}</p>
            <label>
              <input
                ref={confirmBoxRef}
                type="checkbox"
                checked={codeMatches}
                onChange={(e) => {
                  setCodeMatches(e.target.checked);
                  // Checked: the authenticator code is next.
                  if (e.target.checked && codeRequired) totpRef.current?.focus();
                }}
              />{' '}
              {t('pairing.confirmCheck')}
            </label>
          </div>
        )}
        <div className="field">
          <label htmlFor="pair-name">{t('pairing.deviceName')}</label>
          <input
            id="pair-name"
            type="text"
            value={name}
            maxLength={64}
            onChange={(e) => {
              setNameTouched(true);
              setDeviceName(e.target.value);
            }}
          />
        </div>
        <div className="field">
          <label htmlFor="pair-totp">{t('pairing.totp')}</label>
          <input
            id="pair-totp"
            ref={totpRef}
            type="text"
            inputMode="numeric"
            pattern="[0-9]*"
            autoComplete="one-time-code"
            maxLength={10}
            value={totp}
            required={codeRequired}
            aria-required={codeRequired}
            onChange={(e) => onCodeChange(e.target.value)}
            aria-describedby="pair-totp-help"
          />
          <span id="pair-totp-help" className={styles.help}>
            {t('pairing.totpHelp')}{' '}
            <strong>
              {codeRequired ? t('pairing.totpRequiredHint') : t('pairing.totpOptional')}
            </strong>
          </span>
        </div>
        {waitSeconds !== null && (
          <p role="status" className={styles.notice}>
            {t('pairing.enrollWait', { seconds: waitSeconds })}
          </p>
        )}
        {error && (
          <p role="alert" className={styles.error}>
            {error}
            {errorCode && (
              <small className={styles.errorCode}>
                {t('pairing.errorCode', { code: errorCode })}
              </small>
            )}
          </p>
        )}
        {cameraDenied && scanner && (
          <button
            type="button"
            className={`btn ${styles.cameraSettings}`}
            onClick={() => void scanner.openSettings()}
          >
            {t('pairing.scanOpenSettings')}
          </button>
        )}
        {paired && (
          <p role="status" className={styles.success}>
            {paired}
          </p>
        )}
        {scanNotice && (
          <p role="status" className={styles.notice}>
            {scanNotice}
          </p>
        )}
        <div className={styles.actions}>
          <button type="submit" className="btn btn-primary" disabled={busy || scanning}>
            {busy ? t('pairing.pairing') : t('pairing.submit')}
          </button>
          {canScan ? (
            <button
              type="button"
              className="btn"
              disabled={busy || scanning}
              onClick={() => void startScan()}
            >
              <Icon name="qr" size={18} />
              {t('pairing.scan')}
            </button>
          ) : (
            <button type="button" className="btn" disabled aria-describedby="pair-scan-later">
              <Icon name="qr" size={18} />
              {t('pairing.scan')}
            </button>
          )}
        </div>
        {!canScan && (
          <p id="pair-scan-later" className="muted">
            {t('pairing.scanLater')}
          </p>
        )}
      </form>
      {native.dnspilot && (
        <div className={styles.dnspilot}>
          <h3>{t('dnspilot.pairingLead')}</h3>
          <DnsPilotPanel service={native.dnspilot} />
        </div>
      )}
      <p className={styles.note}>{t('pairing.note')}</p>
      {scanning && scanner && <ScanOverlay onCancel={() => void scanner.cancel()} />}
    </section>
  );
}

/**
 * The camera view's overlay. The camera is drawn behind a transparent page,
 * so this is not a `Modal` (its backdrop would cover the picture), but it
 * behaves like one: the focus moves to Cancel and stays inside, Escape
 * cancels, and the focus returns afterwards. The page behind it is hidden
 * (`visibility`), so assistive technology does not reach it either.
 */
function ScanOverlay({ onCancel }: { onCancel: () => void }) {
  const { t } = useI18n();
  const cancelRef = useRef<HTMLButtonElement>(null);
  const onCancelRef = useRef(onCancel);
  useEffect(() => {
    onCancelRef.current = onCancel;
  });
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    cancelRef.current?.focus();
    const layer = openModalLayer();
    const onKey = (e: KeyboardEvent) => {
      if (!layer.isTop()) return;
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        onCancelRef.current();
      } else if (e.key === 'Tab') {
        // Cancel is the only control: Tab stays on it.
        e.preventDefault();
        cancelRef.current?.focus();
      }
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('keydown', onKey);
      layer.close();
      previous?.focus?.();
    };
  }, []);
  return (
    <div
      className={styles.scanOverlay}
      role="dialog"
      aria-modal="true"
      aria-labelledby="pair-scan-hint"
      data-testid="scan-overlay"
    >
      <div className={styles.scanFrame} aria-hidden="true" />
      <p id="pair-scan-hint" className={styles.scanHint}>
        {t('pairing.scanHint')}
      </p>
      <button ref={cancelRef} type="button" className="btn" onClick={onCancel}>
        {t('pairing.scanCancel')}
      </button>
    </div>
  );
}
