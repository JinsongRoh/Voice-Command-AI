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
import { classifyLink, linkRequiresTotp, type LinkKind } from '../native/pairLinks';
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

/** A complete 2-step verification code. */
const SIX_DIGITS = /^\d{6}$/;

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

/** The steps of the pairing card. */
type StepId = 'scan' | 'remote' | 'code' | 'connect';
type StepState = 'pending' | 'active' | 'done' | 'error';

/** A failure, shown on the step it belongs to. */
interface Failure {
  step: StepId;
  message: string;
  /** Raw error code, shown small for support. */
  code: string | null;
}

/** What the last successful pairing went through (for the finished card). */
interface PairedSummary {
  name: string;
  remote: boolean;
  code: boolean;
}

/** The form as last rendered, for handlers that run after an `await`. */
interface Live {
  link: string;
  fromScan: boolean;
  confirmCode: string | null;
  matches: boolean;
  code: string;
  codeRequired: boolean;
}

function isPairKind(kind: LinkKind | null): kind is 'pair' | 'pair_invite' {
  return kind === 'pair' || kind === 'pair_invite';
}

/**
 * True when a link from the app's own scanner may pair without a tap on
 * Pair: the person checked the confirm code and, when the PC asks for one,
 * entered all six digits. A link that arrived any other way (pasted, opened
 * from elsewhere) never pairs by itself.
 */
function autoReady(s: Live): boolean {
  return (
    s.fromScan &&
    isPairKind(classifyLink(s.link)) &&
    s.confirmCode !== null &&
    s.matches &&
    (!s.codeRequired || SIX_DIGITS.test(s.code))
  );
}

/** Pairing errors that only a new QR code from the PC fixes. */
const NEW_QR_ERRORS = ['invalid_pairing', 'pairing_attempts_exhausted', 'expired', 'invalid_uri'];

/** The step a failed pairing attempt belongs to. */
function pairFailureStep(code: string | null, codeStep: boolean): StepId {
  if (isTotpPrompt(code)) return 'code';
  if (code && NEW_QR_ERRORS.includes(code)) return 'scan';
  if (code === 'rate_limited' && codeStep) return 'code';
  return 'connect';
}

function NativePairing({
  native,
  onPaired,
  intro,
  title,
  help,
  enrollRetryMs = ENROLL_RETRY_MS,
}: {
  native: NativeServices;
  onPaired?: (machineId: string) => void;
  intro?: string;
  title?: string;
  help?: string;
  enrollRetryMs?: number;
}) {
  const { t } = useI18n();
  const [uri, setUriValue] = useState('');
  /** The field holds a link from this app's own QR scanner. */
  const [fromScan, setFromScan] = useState(false);
  /** Where the link field is shown (always on devices without a scanner). */
  const [pasteOpen, setPasteOpen] = useState(false);
  const [paired, setPaired] = useState<PairedSummary | null>(null);
  const setUri = (value: string, scanned = false) => {
    setFromScan(scanned);
    setCodeMatches(false);
    setUriValue(value);
    if (value) setPaired(null);
  };
  /** The code of a link, to compare with the PC. */
  const [linkCode, setLinkCode] = useState<{ link: string; code: string } | null>(null);
  const confirmCode = linkCode?.link === uri.trim() ? linkCode.code : null;
  /** The person says the PC shows the same code. */
  const [codeMatches, setCodeMatches] = useState(false);
  useEffect(() => {
    let active = true;
    const link = uri.trim();
    if (!isPairKind(classifyLink(link))) return;
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
      if (link) {
        setUri(link);
        setPasteOpen(true);
      }
    };
    fill();
    return links.subscribe(fill);
  }, [native]);
  const [deviceName, setDeviceName] = useState('');
  const [nameTouched, setNameTouched] = useState(false);
  const [totp, setTotp] = useState('');
  /** What runs: registering the key (DNSPilot), or pairing. */
  const [phase, setPhase] = useState<'enroll' | 'pair' | null>(null);
  const busy = phase !== null;
  /** Set at once (state only on the next render): one attempt at a time. */
  const busyRef = useRef(false);
  const [failure, setFailure] = useState<Failure | null>(null);
  /** Information on the scan step (cancelled scan, nothing to pair yet). */
  const [scanNote, setScanNote] = useState<string | null>(null);
  /** The server asked for a 2-step verification code for this link. */
  const [codePrompt, setCodePrompt] = useState(false);
  /** Bumped to move the focus to the code field. */
  const [focusCode, setFocusCode] = useState(0);
  const totpRef = useRef<HTMLInputElement>(null);
  const [platform, setPlatform] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [cameraDenied, setCameraDenied] = useState(false);
  /** Seconds announced after an invite, while connections may be refused. */
  const [waitSeconds, setWaitSeconds] = useState<number | null>(null);
  /** Until when (ms) a refused connection means "the Agent is not ready yet". */
  const readyAt = useRef(0);
  /** The link already enrolled (its code works once). */
  const enrolledLink = useRef<string | null>(null);
  const [enrolled, setEnrolled] = useState<string | null>(null);
  /** The DNSPilot console's phone QR registered this phone. */
  const [consoleEnrolled, setConsoleEnrolled] = useState(false);
  /** The person confirmed while a registration ran: pair once it is done. */
  const pendingAuto = useRef(false);
  const scanner = native.qrScanner;
  const canScan = !!scanner && canScanQr(platform);
  // The link says so (`tf=1`), or the server asked for it.
  const codeRequired = codePrompt || linkRequiresTotp(uri);

  const live = useRef<Live>({
    link: '',
    fromScan: false,
    confirmCode: null,
    matches: false,
    code: '',
    codeRequired: false,
  });
  useEffect(() => {
    live.current = {
      link: uri.trim(),
      fromScan,
      confirmCode,
      matches: codeMatches,
      code: totp,
      codeRequired,
    };
  });

  useEffect(() => {
    if (focusCode > 0) totpRef.current?.focus();
  }, [focusCode]);

  const fail = (step: StepId, message: string, code: string | null = null) =>
    setFailure({ step, message, code });

  /** Shows the 2-step verification prompt on its step and focuses the field. */
  const askForCode = (message: string, code: string | null = null) => {
    setCodePrompt(true);
    fail('code', message, code);
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
    setEnrolled(link);
    if (enrollment) readyAt.current = Date.now() + enrollment.readyWithinSec * 1000;
  };

  /**
   * Handles an accepted link. A scanned link only registers its invite (if
   * any): pairing waits for the confirm code check. Nothing goes to the PC
   * before the person confirmed the code. The server spends the one-time
   * pairing secret only after the 2-step code checks out, so a missing or
   * wrong code can be retried with the same link until it expires.
   */
  const process = async (
    link: string,
    scanned: boolean,
    opts: { typed?: string; confirmed?: boolean } = {},
  ) => {
    if (busyRef.current) return;
    const kind = classifyLink(link);
    setScanNote(null);
    if (!link.trim()) {
      // Nothing to check yet (for example right after a console invite
      // enrolled and cleared the field): say what to do, not "invalid".
      setFailure(null);
      setScanNote(t('pairing.needLink'));
      return;
    }
    if (kind === null) {
      fail('scan', t('native.error.invalid_uri'));
      return;
    }
    // Read from the last render: this may run after an `await`.
    const now = live.current;
    const code = (opts.typed ?? now.code).trim();
    const needsCode = now.codeRequired || linkRequiresTotp(link);
    if (isPairKind(kind) && !scanned) {
      // A forged link would send the code to someone else's server. No code
      // to compare (not loaded, or the link is broken) means no.
      const known = now.link === link.trim() ? now.confirmCode : null;
      if (!known || !(opts.confirmed ?? now.matches)) {
        fail('scan', t('pairing.confirmFirst'));
        return;
      }
      if (!code && needsCode) {
        // Known to be needed: ask before contacting the PC.
        askForCode(t('pairing.totpPrompt'));
        return;
      }
    }
    setFailure(null);
    setPaired(null);
    setCameraDenied(false);
    busyRef.current = true;
    let step: 'enroll' | 'pair' = 'enroll';
    let enrolledOnly = false;
    try {
      if (kind !== 'pair' && enrolledLink.current !== link) {
        setPhase('enroll');
        await enroll(link);
      }
      if (kind === 'enroll') {
        setUri('');
        setConsoleEnrolled(true);
        return;
      }
      if (scanned) {
        enrolledOnly = true;
        return;
      }
      step = 'pair';
      setPhase('pair');
      showWait();
      const info = await retryWhileEnrolling(
        () => native.pair(link, name.trim() || 'VCA', code || null),
        { until: readyAt.current, retryMs: enrollRetryMs },
      );
      readyAt.current = 0;
      enrolledLink.current = null;
      setPaired({
        name: info.name,
        remote: kind === 'pair_invite' || consoleEnrolled,
        code: needsCode || code !== '',
      });
      setUri('');
      setTotp('');
      setCodePrompt(false);
      setEnrolled(null);
      setConsoleEnrolled(false);
      onPaired?.(info.id);
    } catch (err) {
      const failed = describePairingError(t, err);
      if (step === 'enroll') {
        fail('remote', failed.message, failed.code);
      } else if (isTotpPrompt(failed.code)) {
        // Same link, same form: only the code is missing or wrong.
        setTotp('');
        askForCode(failed.message, failed.code);
      } else {
        fail(pairFailureStep(failed.code, needsCode), failed.message, failed.code);
      }
    } finally {
      busyRef.current = false;
      setPhase(null);
      setWaitSeconds(null);
      // The person confirmed while the invite was being registered.
      const again = pendingAuto.current && enrolledOnly;
      pendingAuto.current = false;
      if (again && autoReady(live.current)) {
        const s = live.current;
        void process(s.link, false, { typed: s.code, confirmed: true });
      }
    }
  };

  /** Pairs a scanned link once everything is checked (see `autoReady`). */
  const tryAutoPair = (s: Live): boolean => {
    if (!autoReady(s) || scanning) return false;
    if (busyRef.current) pendingAuto.current = true;
    else void process(s.link, false, { typed: s.code, confirmed: true });
    return true;
  };

  const startScan = async () => {
    if (!scanner || scanInFlight.current || busyRef.current) return;
    setFailure(null);
    setPaired(null);
    setScanNote(null);
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
      case 'link': {
        const link = result.uri.trim();
        setUri(link, true);
        setTotp('');
        setCodePrompt(false);
        const kind = classifyLink(link);
        // The 2-step code is typed while the confirm code is checked.
        if (isPairKind(kind) && linkRequiresTotp(link)) setFocusCode((n) => n + 1);
        // An invite registers at once (the PC needs a moment to learn the
        // key); pairing still waits for the confirm code check.
        if (kind !== 'pair') await process(link, true);
        break;
      }
      case 'invalid':
        fail('scan', t('pairing.scanInvalid'));
        break;
      case 'cancelled':
        setScanNote(t('pairing.scanCancelled'));
        break;
      case 'denied':
        setCameraDenied(true);
        fail('scan', t('pairing.scanDenied'));
        break;
      case 'error':
        fail('scan', t('pairing.scanFailed', { message: result.message }));
        break;
    }
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busyRef.current) return;
    await process(uri.trim(), false);
  };

  const onConfirmChange = (checked: boolean) => {
    setCodeMatches(checked);
    if (!checked) return;
    if (failure?.step === 'scan') setFailure(null);
    const s = { ...live.current, matches: true };
    // Checked: pair when nothing else is needed, else the code is next.
    if (!tryAutoPair(s) && s.codeRequired) totpRef.current?.focus();
  };

  const onCodeChange = (value: string) => {
    const digits = value.replace(/\D/g, '').slice(0, 6);
    setTotp(digits);
    if (digits.length !== 6) return;
    const s = { ...live.current, code: digits };
    if (tryAutoPair(s)) return;
    // The sixth digit never goes before the confirm code is checked.
    if (s.fromScan && isPairKind(classifyLink(s.link)) && !s.matches) {
      fail('scan', t('pairing.confirmFirst'));
    }
  };

  // ---- Step states ----------------------------------------------------
  const link = uri.trim();
  const kind = classifyLink(link);
  const pairLink = isPairKind(kind);
  const failed = (step: StepId) => (failure?.step === step ? failure : null);
  const scanChecked = pairLink && confirmCode !== null && codeMatches;
  const ready = scanChecked && (!codeRequired || SIX_DIGITS.test(totp));
  const remoteShown = paired
    ? paired.remote
    : kind === 'pair_invite' || kind === 'enroll' || consoleEnrolled;
  const remoteDone =
    kind === 'pair_invite' || kind === 'enroll' ? enrolled === link : consoleEnrolled;
  const codeShown = paired ? paired.code : pairLink && codeRequired;

  const scanState: StepState = paired
    ? 'done'
    : failed('scan')
      ? 'error'
      : scanChecked
        ? 'done'
        : kind === 'enroll'
          ? 'pending'
          : 'active';
  const remoteState: StepState = paired
    ? 'done'
    : failed('remote')
      ? 'error'
      : remoteDone
        ? 'done'
        : phase === 'enroll' || kind === 'enroll'
          ? 'active'
          : 'pending';
  const codeState: StepState = paired
    ? 'done'
    : failed('code')
      ? 'error'
      : scanChecked
        ? 'active'
        : 'pending';
  const connectState: StepState = paired
    ? 'done'
    : failed('connect')
      ? 'error'
      : phase === 'pair' || (ready && !busy)
        ? 'active'
        : 'pending';

  const scanButton = canScan ? (
    <button
      type="button"
      // With a link in, the confirm code check is the step's action.
      className={pairLink ? 'btn btn-sm' : 'btn btn-primary'}
      disabled={busy || scanning}
      onClick={() => void startScan()}
    >
      <Icon name="qr" size={18} />
      {pairLink ? t('pairing.step.rescan') : t('pairing.scan')}
    </button>
  ) : null;

  const linkField = (
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
  );

  // Step numbers: only the steps shown count.
  const remoteNumber = 2;
  const codeNumber = remoteShown ? 3 : 2;
  const connectNumber = 2 + (remoteShown ? 1 : 0) + (codeShown ? 1 : 0);
  return (
    <section className={`panel ${styles.pairing}`} aria-labelledby="pair-title">
      <h2 id="pair-title">{title ?? t('pairing.title')}</h2>
      {intro && <p>{intro}</p>}
      {help && <p className={styles.help}>{help}</p>}
      <form className={styles.form} onSubmit={(e) => void submit(e)} noValidate>
        <ol className={styles.stepper} aria-label={t('pairing.guideTitle')}>
          <StepItem
            index={1}
            state={scanState}
            title={
              scanState === 'done'
                ? t('pairing.step.scanDone')
                : consoleEnrolled && !pairLink
                  ? t('pairing.step.scanAfterEnroll')
                  : t('pairing.step.scan')
            }
            testId="step-scan"
          >
            {!paired && (
              <>
                {scanState === 'active' && !pairLink && (
                  <p className={styles.help}>{t('pairing.step.scanWhere')}</p>
                )}
                {pairLink && confirmCode === null && (
                  <p className={styles.help}>{t('pairing.step.checking')}</p>
                )}
                {confirmCode && (
                  <div className={styles.confirm} data-testid="pair-confirm">
                    <p className={styles.confirmLine}>
                      {t('pairing.confirmTitle')} <ConfirmCode code={confirmCode} />
                    </p>
                    <label className={styles.confirmCheck}>
                      <input
                        type="checkbox"
                        checked={codeMatches}
                        disabled={phase === 'pair'}
                        onChange={(e) => onConfirmChange(e.target.checked)}
                      />{' '}
                      {t('pairing.confirmCheck')}
                    </label>
                    {!codeMatches && <p className={styles.help}>{t('pairing.confirmHelp')}</p>}
                  </div>
                )}
                <StepFailure failure={failed('scan')} />
                {cameraDenied && scanner && (
                  <button
                    type="button"
                    className={`btn btn-primary ${styles.stepAction}`}
                    onClick={() => void scanner.openSettings()}
                  >
                    {t('pairing.scanOpenSettings')}
                  </button>
                )}
                {scanNote && (
                  <p role="status" className={styles.notice}>
                    {scanNote}
                  </p>
                )}
                {scanButton && <div className={styles.stepAction}>{scanButton}</div>}
                {canScan ? (
                  <details className={styles.paste} open={pasteOpen}>
                    <summary
                      onClick={(e) => {
                        // React owns `open` (a link that opened the app unfolds it).
                        e.preventDefault();
                        setPasteOpen((open) => !open);
                      }}
                    >
                      {t('pairing.step.paste')}
                    </summary>
                    {linkField}
                  </details>
                ) : (
                  <>
                    {linkField}
                    <p id="pair-scan-later" className="muted">
                      {t('pairing.scanLater')}
                    </p>
                  </>
                )}
              </>
            )}
          </StepItem>

          {remoteShown && (
            <StepItem
              index={remoteNumber}
              state={remoteState}
              title={
                remoteState === 'done'
                  ? t('pairing.step.remoteDone')
                  : phase === 'enroll'
                    ? t('pairing.step.remoteBusy')
                    : t('pairing.step.remote')
              }
              testId="step-remote"
            >
              {remoteState === 'pending' && (
                <p className={styles.help}>{t('pairing.step.remotePending')}</p>
              )}
              <StepFailure failure={failed('remote')} />
            </StepItem>
          )}

          {codeShown && (
            <StepItem
              index={codeNumber}
              state={codeState}
              title={t('pairing.step.code')}
              testId="step-code"
            >
              {!paired && (
                <>
                  <StepFailure failure={failed('code')} />
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
                      required
                      aria-required
                      disabled={phase === 'pair'}
                      onChange={(e) => onCodeChange(e.target.value)}
                      aria-describedby="pair-totp-help"
                    />
                    <span id="pair-totp-help" className={styles.help}>
                      {t('pairing.totpHelp')} <strong>{t('pairing.totpRequiredHint')}</strong>
                    </span>
                  </div>
                  {codeState === 'pending' && (
                    <p className={styles.help}>{t('pairing.step.codePending')}</p>
                  )}
                </>
              )}
            </StepItem>
          )}

          <StepItem
            index={connectNumber}
            state={connectState}
            title={
              connectState === 'done'
                ? t('pairing.step.connectDone')
                : phase === 'pair'
                  ? t('pairing.step.connectBusy')
                  : t('pairing.step.connect')
            }
            testId="step-connect"
          >
            {paired && (
              <>
                <p role="status" className={styles.success}>
                  {t('pairing.success', { name: paired.name })}
                </p>
                <button type="button" className="btn btn-sm" onClick={() => setPaired(null)}>
                  {t('pairing.again')}
                </button>
              </>
            )}
            {phase === 'pair' && (
              <p role="status" className={styles.notice}>
                {waitSeconds !== null
                  ? t('pairing.step.connectWait', { seconds: waitSeconds })
                  : t('pairing.step.connectBusy')}
              </p>
            )}
            {connectState === 'active' && phase === null && !fromScan && (
              <p className={styles.help}>{t('pairing.step.connectReady')}</p>
            )}
            <StepFailure failure={failed('connect')} />
            {!paired && (
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
            )}
          </StepItem>
        </ol>
        {!paired && (
          <div className={styles.actions}>
            <button
              type="submit"
              className={ready || !canScan ? 'btn btn-primary' : 'btn'}
              disabled={busy || scanning}
            >
              {phase === 'pair' ? t('pairing.pairing') : t('pairing.submit')}
            </button>
          </div>
        )}
      </form>
      {native.dnspilot && (
        <details className={styles.advanced}>
          <summary>{t('pairing.advanced')}</summary>
          <p className={styles.help}>{t('pairing.advancedHelp')}</p>
          <h3>{t('dnspilot.pairingLead')}</h3>
          <DnsPilotPanel service={native.dnspilot} />
        </details>
      )}
      <p className={styles.note}>{t('pairing.note')}</p>
      {scanning && scanner && <ScanOverlay onCancel={() => void scanner.cancel()} />}
    </section>
  );
}

/** One step of the pairing card: number (or ✓ / !), title and its content. */
function StepItem({
  index,
  state,
  title,
  testId,
  children,
}: {
  index: number;
  state: StepState;
  title: string;
  testId: string;
  children?: ReactNode;
}) {
  const { t } = useI18n();
  return (
    <li
      className={styles.step}
      data-state={state}
      data-testid={testId}
      aria-current={state === 'active' || state === 'error' ? 'step' : undefined}
    >
      <span className={styles.stepMark} aria-hidden="true">
        {state === 'done' ? <Icon name="check" size={16} /> : state === 'error' ? '!' : index}
      </span>
      <div className={styles.stepBody}>
        <p className={styles.stepTitle}>
          <span>{title}</span>
          <span className="visually-hidden"> ({t(`pairing.state.${state}`)})</span>
        </p>
        {children}
      </div>
    </li>
  );
}

/** A failure on its step, with the raw code small for support. */
function StepFailure({ failure }: { failure: Failure | null }) {
  const { t } = useI18n();
  if (!failure) return null;
  return (
    <p role="alert" className={styles.error}>
      {failure.message}
      {failure.code && (
        <small className={styles.errorCode}>{t('pairing.errorCode', { code: failure.code })}</small>
      )}
    </p>
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
