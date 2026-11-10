import { useCallback, useEffect, useRef, useState, type FormEvent, type ReactNode } from 'react';

import { useI18n } from '../i18n/context';
import type { TranslateFn } from '../i18n/i18n';
import type {
  DnsPilotAppClient,
  DnsPilotCreateTunnel,
  DnsPilotDevice,
  DnsPilotInvite,
  DnsPilotPrompt,
  DnsPilotRegistration,
  DnsPilotRemoteTunnel,
  DnsPilotService,
  DnsPilotStatus,
  DnsPilotTunnel,
  DnsPilotZone,
} from '../native/bridge';
import { toNativeError } from '../native/bridge';
import { describeNativeError } from '../native/errors';
import { isDesktopPlatform } from '../native/host';
import { Modal } from './Modal';
import styles from './DnsPilotConnect.module.css';

/** vcad's loopback port. */
const VCAD_PORT = 47800;

/** vcad's settings for a tunnel, to paste into its config.toml. */
function vcadConfigSnippet(tunnel: DnsPilotTunnel): string {
  const port = tunnel.publicPort === 443 ? '' : `\npublic_port = ${tunnel.publicPort}`;
  return `[remote.provider]\nkind = "dnspilot"\n[remote.provider.dnspilot]\nhostname = "${tunnel.hostname}"${port}`;
}

/** Readable console status of a tunnel. */
function tunnelStatusText(t: TranslateFn, status: string): string {
  if (status === 'active') return t('dnspilot.status.active');
  if (status === 'suspended') return t('dnspilot.status.suspended');
  if (status === 'disabled') return t('dnspilot.status.disabled');
  return status;
}

/** Why the key must be registered in the console. */
function needsMessage(
  reason: string,
): 'dnspilot.needs.notOwned' | 'dnspilot.needs.notConnected' | 'dnspilot.needs.noId' {
  if (reason === 'not_created_by_app') return 'dnspilot.needs.notOwned';
  if (reason === 'not_connected') return 'dnspilot.needs.notConnected';
  return 'dnspilot.needs.noId';
}

/** Where an app client's key came from. */
function sourceText(t: TranslateFn, source: string | null): string | null {
  if (source === 'invite') return t('dnspilot.source.invite');
  if (source === 'oauth') return t('dnspilot.source.oauth');
  if (source === 'console') return t('dnspilot.source.console');
  if (source === 'local') return t('dnspilot.source.local');
  return source;
}

function positiveInt(value: string): number | null {
  const n = Number(value.trim());
  return Number.isInteger(n) && n > 0 ? n : null;
}

/** "Connect DNSPilot" button that opens the setup (loads nothing until opened). */
export function DnsPilotPanel({ service }: { service: DnsPilotService }) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  if (open) return <DnsPilotConnect service={service} />;
  return (
    <div className={styles.row}>
      <button type="button" className="btn" onClick={() => setOpen(true)}>
        {t('dnspilot.show')}
      </button>
    </div>
  );
}

/**
 * "Reconnect DNSPilot": runs the DNSPilot approval again, connected or not
 * (the new grant replaces the old one). Used where a pairing code could not
 * carry an invite because DNSPilot is not connected or refused the grant.
 */
export function DnsPilotRelink({
  service,
  onConnected,
}: {
  service: DnsPilotService;
  onConnected?: () => void;
}) {
  const { t } = useI18n();
  const [prompt, setPrompt] = useState<DnsPilotPrompt | null>(null);
  const [error, setError] = useState<string | null>(null);
  const running = useRef(false);
  const [busy, setBusy] = useState(false);

  const start = async () => {
    if (running.current) return;
    running.current = true;
    setBusy(true);
    setError(null);
    try {
      setPrompt(await service.authorizeStart());
      await service.authorizeWait();
      onConnected?.();
    } catch (err) {
      if (toNativeError(err).code !== 'dnspilot_cancelled') setError(describeNativeError(t, err));
    } finally {
      running.current = false;
      setPrompt(null);
      setBusy(false);
    }
  };

  return (
    <div className={styles.box}>
      {prompt ? (
        <Approval
          prompt={prompt}
          onOpen={() =>
            void service.openActivation().catch((e) => setError(describeNativeError(t, e)))
          }
          onCancel={() => void service.authorizeCancel().catch(() => {})}
        />
      ) : (
        <div className={styles.row}>
          <button
            type="button"
            className="btn btn-primary btn-sm"
            disabled={busy}
            onClick={() => void start()}
          >
            {t('host.invite.relink')}
          </button>
        </div>
      )}
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
    </div>
  );
}

/**
 * "Connect DNSPilot": authorizes VCA in DNSPilot with the device flow,
 * creates (or records) an app-mode tunnel to vcad on the PC and registers
 * this app's client key on it. Keys and tokens never reach the webview.
 */
export function DnsPilotConnect({ service }: { service: DnsPilotService }) {
  const { t } = useI18n();
  const [status, setStatus] = useState<DnsPilotStatus | null>(null);
  const [prompt, setPrompt] = useState<DnsPilotPrompt | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [registrations, setRegistrations] = useState<Record<string, DnsPilotRegistration>>({});
  const [version, setVersion] = useState(0);
  /** `null` until known. Phones only use tunnels; they never create them. */
  const [desktop, setDesktop] = useState<boolean | null>(null);
  const [confirmDisconnect, setConfirmDisconnect] = useState(false);
  /** Also stops a second click that arrives before `busy` renders. */
  const running = useRef(false);

  const reload = useCallback(async () => {
    try {
      setStatus(await service.status());
    } catch (err) {
      setError(describeNativeError(t, err));
    }
  }, [service, t]);

  useEffect(() => {
    let active = true;
    service
      .platform()
      .then((p) => {
        if (active) setDesktop(isDesktopPlatform(p));
      })
      .catch(() => {
        if (active) setDesktop(false);
      });
    return () => {
      active = false;
    };
  }, [service]);

  useEffect(() => {
    let active = true;
    service
      .status()
      .then((s) => {
        if (active) setStatus(s);
      })
      .catch((err: unknown) => {
        if (active) setError(describeNativeError(t, err));
      });
    return () => {
      active = false;
    };
  }, [service, t]);

  const run = async (action: () => Promise<unknown>) => {
    if (running.current) return;
    running.current = true;
    setBusy(true);
    setError(null);
    try {
      await action();
      await reload();
    } catch (err) {
      setError(describeNativeError(t, err));
      // What did happen stays visible (a tunnel created before a failure).
      await reload();
    } finally {
      running.current = false;
      setBusy(false);
      setVersion((v) => v + 1);
    }
  };

  const connect = async () => {
    setError(null);
    let started: DnsPilotPrompt;
    try {
      started = await service.authorizeStart();
    } catch (err) {
      setError(describeNativeError(t, err));
      return;
    }
    setPrompt(started);
    try {
      setStatus(await service.authorizeWait());
    } catch (err) {
      if (toNativeError(err).code !== 'dnspilot_cancelled') setError(describeNativeError(t, err));
    } finally {
      setPrompt(null);
    }
  };

  const cancel = () => {
    void service.authorizeCancel().catch(() => {});
  };

  const register = (hostname: string) =>
    run(async () => {
      const result = await service.register(hostname);
      setRegistrations((r) => ({ ...r, [hostname]: result }));
    });

  if (!status) {
    return (
      <div className={styles.box}>
        <p className="muted">{t('dnspilot.loading')}</p>
        {error && (
          <p role="alert" className={styles.error}>
            {error}
          </p>
        )}
      </div>
    );
  }

  return (
    <div className={styles.box}>
      <p className={styles.intro}>{t('dnspilot.intro')}</p>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}

      {prompt ? (
        <Approval
          prompt={prompt}
          onOpen={() =>
            void service.openActivation().catch((e) => setError(describeNativeError(t, e)))
          }
          onCancel={cancel}
        />
      ) : status.connected ? (
        <div className={styles.row}>
          <span className={styles.ok}>{t('dnspilot.connected')}</span>
          <button
            type="button"
            className="btn btn-sm"
            disabled={busy}
            onClick={() => setConfirmDisconnect(true)}
          >
            {t('dnspilot.disconnect')}
          </button>
        </div>
      ) : (
        <div className={styles.row}>
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy}
            onClick={() => void connect()}
          >
            {t('dnspilot.connect')}
          </button>
        </div>
      )}

      {confirmDisconnect && (
        <Modal
          title={t('dnspilot.disconnectTitle')}
          onClose={() => setConfirmDisconnect(false)}
          footer={
            <>
              <button type="button" className="btn" onClick={() => setConfirmDisconnect(false)}>
                {t('dnspilot.cancel')}
              </button>
              <button
                type="button"
                className="btn btn-danger"
                disabled={busy}
                onClick={() => {
                  setConfirmDisconnect(false);
                  void run(() => service.disconnect());
                }}
              >
                {t('dnspilot.disconnectConfirm')}
              </button>
            </>
          }
        >
          <p>{t('dnspilot.disconnectWarning')}</p>
          <p>{t('dnspilot.disconnectWarningGrant')}</p>
        </Modal>
      )}

      {status.connected && !prompt && desktop !== null && (
        <ConnectedSetup
          service={service}
          busy={busy}
          version={version}
          desktop={desktop}
          onCreate={(req) =>
            run(async () => {
              const tunnel = await service.createTunnel(req);
              if (!tunnel.clientRegistered) {
                const result = await service.register(tunnel.hostname);
                setRegistrations((r) => ({ ...r, [tunnel.hostname]: result }));
              }
            })
          }
          onUse={(tunnelId) =>
            run(async () => {
              const result = await service.useTunnel(tunnelId);
              const current = await service.status();
              const tunnel = current.tunnels.find((x) => x.tunnelId === tunnelId);
              if (tunnel) setRegistrations((r) => ({ ...r, [tunnel.hostname]: result }));
            })
          }
        />
      )}

      {!prompt && (
        <ExistingTunnelForm
          busy={busy}
          onAdd={(hostname, tunnelId) =>
            run(async () => {
              const tunnel = await service.addTunnel(hostname, tunnelId);
              const result = await service.register(tunnel.hostname);
              setRegistrations((r) => ({ ...r, [tunnel.hostname]: result }));
            })
          }
        />
      )}

      {status.tunnels.length > 0 && (
        <ul className={styles.tunnels} aria-label={t('dnspilot.tunnels')}>
          {status.tunnels.map((tunnel) => (
            <TunnelItem
              key={tunnel.hostname}
              tunnel={tunnel}
              busy={busy}
              registration={registrations[tunnel.hostname]}
              onSetId={(id) => run(() => service.setTunnelId(tunnel.hostname, id))}
              onRegister={() => register(tunnel.hostname)}
              onMarkRegistered={() => run(() => service.markRegistered(tunnel.hostname))}
              onRemove={() => run(() => service.removeTunnel(tunnel.hostname))}
              onOpenConsole={() =>
                void service.openConsole().catch((e) => setError(describeNativeError(t, e)))
              }
              onIssueToken={() => run(() => service.issueAppToken(tunnel.hostname))}
              clients={
                desktop && status.connected && tunnel.tunnelId !== null ? (
                  <TunnelClients
                    service={service}
                    tunnelId={tunnel.tunnelId}
                    onChanged={() => void reload()}
                  />
                ) : null
              }
            />
          ))}
        </ul>
      )}

      {status.identity && (
        <p className={styles.fingerprint}>
          {t('dnspilot.fingerprint')} <code>{status.identity.spkiSha256}</code>
        </p>
      )}
    </div>
  );
}

function Approval({
  prompt,
  onOpen,
  onCancel,
}: {
  prompt: DnsPilotPrompt;
  onOpen: () => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  return (
    <div className={styles.approval} role="status" aria-live="polite">
      <p>{t('dnspilot.approve', { url: prompt.verificationUri })}</p>
      <p className={styles.code} aria-label={t('dnspilot.code')}>
        {prompt.userCode}
      </p>
      <ol className={styles.steps}>
        <li>{t('dnspilot.approveStep1')}</li>
        <li>{t('dnspilot.approveStep2')}</li>
        <li>{t('dnspilot.approveStep3')}</li>
      </ol>
      <p className="muted">{t('dnspilot.approveAdmin')}</p>
      <div className={styles.row}>
        <button type="button" className="btn btn-primary" onClick={onOpen}>
          {t('dnspilot.open')}
        </button>
        <button type="button" className="btn" onClick={onCancel}>
          {t('dnspilot.cancel')}
        </button>
      </div>
      <p className="muted">{t('dnspilot.waiting')}</p>
    </div>
  );
}

/** Picker data loaded once DNSPilot is connected. */
interface SetupData {
  devices: DnsPilotDevice[];
  /** `null` while DNSPilot does not offer the zones call (type the id). */
  zones: DnsPilotZone[] | null;
  remote: DnsPilotRemoteTunnel[];
  localDevice: number | null;
}

function ConnectedSetup({
  service,
  busy,
  version,
  desktop,
  onCreate,
  onUse,
}: {
  service: DnsPilotService;
  busy: boolean;
  /** Changes after each action, to reload the lists. */
  version: number;
  /** Phones get the reuse list only. */
  desktop: boolean;
  onCreate: (req: DnsPilotCreateTunnel) => Promise<void>;
  onUse: (tunnelId: number) => Promise<void>;
}) {
  const { t } = useI18n();
  const [data, setData] = useState<SetupData | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    Promise.all([
      desktop ? service.devices() : [],
      desktop ? service.zones() : null,
      service.remoteTunnels(),
      desktop ? service.localDevice().catch(() => null) : null,
    ])
      .then(([devices, zones, remote, localDevice]) => {
        if (active) setData({ devices, zones, remote, localDevice });
      })
      .catch((err: unknown) => {
        if (active) setLoadError(describeNativeError(t, err));
      });
    return () => {
      active = false;
    };
  }, [service, t, version, desktop]);

  if (loadError) {
    return (
      <p role="alert" className={styles.error}>
        {loadError}
      </p>
    );
  }
  if (!data) return <p className="muted">{t('dnspilot.loading')}</p>;
  const reusable = data.remote.filter((r) => !r.known);
  return (
    <>
      {reusable.length > 0 && (
        <div className={styles.form}>
          <h4>{t('dnspilot.reuse')}</h4>
          <ul className={styles.tunnels} aria-label={t('dnspilot.reuse')}>
            {reusable.map((r) => (
              <li key={r.id} className={styles.row}>
                <strong className={styles.host}>{r.hostname}</strong>
                <span className="muted">
                  {[r.deviceName, r.status].filter((x) => x).join(' · ')}
                </span>
                <button
                  type="button"
                  className="btn btn-sm"
                  disabled={busy}
                  aria-label={t('dnspilot.useTunnel', { host: r.hostname })}
                  onClick={() => void onUse(r.id)}
                >
                  {t('dnspilot.use')}
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
      {desktop ? (
        <CreateTunnelForm
          key={data.devices.map((d) => d.id).join(',')}
          busy={busy}
          devices={data.devices}
          zones={data.zones}
          localDevice={data.localDevice}
          remote={data.remote}
          onCreate={onCreate}
        />
      ) : (
        <p className="muted">{t('dnspilot.phoneHelp')}</p>
      )}
    </>
  );
}

function deviceLabel(t: TranslateFn, d: DnsPilotDevice): string {
  const parts = [d.name];
  if (d.os) parts.push(d.os);
  parts.push(d.online ? t('dnspilot.online') : t('dnspilot.offline'));
  return parts.join(' · ');
}

/** What the confirmation shows before anything is created. */
interface PendingCreate {
  deviceId: number;
  deviceName: string;
  zoneId: number;
  zoneName: string;
}

function CreateTunnelForm({
  busy,
  devices,
  zones,
  localDevice,
  remote,
  onCreate,
}: {
  busy: boolean;
  devices: DnsPilotDevice[];
  zones: DnsPilotZone[] | null;
  localDevice: number | null;
  /** Existing tunnels: a PC that already has one gets no second one. */
  remote: DnsPilotRemoteTunnel[];
  onCreate: (req: DnsPilotCreateTunnel) => Promise<void>;
}) {
  const { t } = useI18n();
  const local = devices.find((d) => d.id === localDevice);
  // Only the PC this app runs on may be preselected: a tunnel on another
  // device exposes that device.
  const [deviceId, setDeviceId] = useState(local ? String(local.id) : '');
  const [zoneId, setZoneId] = useState(zones?.[0] ? String(zones[0].id) : '');
  const [invalid, setInvalid] = useState(false);
  const [pending, setPending] = useState<PendingCreate | null>(null);
  const chosen = devices.find((d) => String(d.id) === deviceId);
  const existing = chosen
    ? remote.find((r) => r.deviceId === chosen.id && r.localPort === VCAD_PORT)
    : undefined;
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const zone = positiveInt(zoneId);
    if (!chosen || zone === null) {
      setInvalid(true);
      return;
    }
    setInvalid(false);
    setPending({
      deviceId: chosen.id,
      deviceName: chosen.name,
      zoneId: zone,
      zoneName: zones?.find((z) => z.id === zone)?.name ?? `#${zone}`,
    });
  };
  const confirm = () => {
    if (!pending) return;
    setPending(null);
    void onCreate({ deviceId: pending.deviceId, zoneId: pending.zoneId });
  };
  return (
    <form className={styles.form} onSubmit={submit} noValidate aria-labelledby="dp-create">
      <h4 id="dp-create">{t('dnspilot.create')}</h4>
      <p className="muted">{t('dnspilot.createHelp')}</p>
      {devices.length === 0 ? (
        <p className="muted">{t('dnspilot.noDevices')}</p>
      ) : (
        <div className="field">
          <label htmlFor="dp-device">{t('dnspilot.device')}</label>
          <select
            id="dp-device"
            value={deviceId}
            onChange={(e) => setDeviceId(e.target.value)}
            aria-describedby="dp-device-help"
          >
            {!local && <option value="">{t('dnspilot.choosePc')}</option>}
            {devices.map((d) => (
              <option key={d.id} value={String(d.id)}>
                {d.id === localDevice
                  ? t('dnspilot.thisPc', { device: deviceLabel(t, d) })
                  : deviceLabel(t, d)}
              </option>
            ))}
          </select>
          <span id="dp-device-help" className={styles.help}>
            {t('dnspilot.deviceHelp')}
          </span>
        </div>
      )}
      {zones ? (
        <div className="field">
          <label htmlFor="dp-zone">{t('dnspilot.zone')}</label>
          <select id="dp-zone" value={zoneId} onChange={(e) => setZoneId(e.target.value)}>
            {zones.map((z) => (
              <option key={z.id} value={String(z.id)}>
                {z.name}
              </option>
            ))}
          </select>
        </div>
      ) : (
        <div className="field">
          <label htmlFor="dp-zone">{t('dnspilot.zoneId')}</label>
          <input
            id="dp-zone"
            inputMode="numeric"
            value={zoneId}
            onChange={(e) => setZoneId(e.target.value)}
            aria-describedby="dp-zone-help"
          />
          <span id="dp-zone-help" className={styles.help}>
            {t('dnspilot.zoneIdHelp')}
          </span>
        </div>
      )}
      {invalid && (
        <p role="alert" className={styles.error}>
          {t('dnspilot.createInvalid')}
        </p>
      )}
      <p className="muted">{t('dnspilot.createNotice')}</p>
      {existing ? (
        <p role="status">
          {t('dnspilot.createExists', { device: chosen?.name ?? '', host: existing.hostname })}
        </p>
      ) : (
        <button type="submit" className="btn btn-sm btn-primary" disabled={busy || !chosen}>
          {chosen ? t('dnspilot.createOn', { device: chosen.name }) : t('dnspilot.createSubmit')}
        </button>
      )}
      {pending && (
        <Modal
          title={t('dnspilot.createConfirmTitle')}
          onClose={() => setPending(null)}
          footer={
            <>
              <button type="button" className="btn" onClick={() => setPending(null)}>
                {t('dnspilot.cancel')}
              </button>
              <button type="button" className="btn btn-primary" disabled={busy} onClick={confirm}>
                {t('dnspilot.createConfirmSubmit')}
              </button>
            </>
          }
        >
          <p>
            {t('dnspilot.createConfirm', { device: pending.deviceName, zone: pending.zoneName })}
          </p>
        </Modal>
      )}
    </form>
  );
}

function ExistingTunnelForm({
  busy,
  onAdd,
}: {
  busy: boolean;
  onAdd: (hostname: string, tunnelId: number | null) => Promise<void>;
}) {
  const { t } = useI18n();
  const [hostname, setHostname] = useState('');
  const [tunnelId, setTunnelId] = useState('');
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (!hostname.trim()) return;
    void onAdd(hostname.trim(), positiveInt(tunnelId));
  };
  return (
    <details className={styles.details}>
      <summary>{t('dnspilot.existing')}</summary>
      <form className={styles.form} onSubmit={submit} noValidate>
        <p className="muted">{t('dnspilot.existingHelp')}</p>
        <div className="field">
          <label htmlFor="dp-host">{t('dnspilot.hostname')}</label>
          <input
            id="dp-host"
            value={hostname}
            placeholder="vca-k3x9q2ab4m.example.com"
            autoCapitalize="off"
            spellCheck={false}
            onChange={(e) => setHostname(e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor="dp-existing-id">{t('dnspilot.tunnelIdOptional')}</label>
          <input
            id="dp-existing-id"
            inputMode="numeric"
            value={tunnelId}
            onChange={(e) => setTunnelId(e.target.value)}
          />
        </div>
        <button type="submit" className="btn btn-sm" disabled={busy}>
          {t('dnspilot.existingSubmit')}
        </button>
      </form>
    </details>
  );
}

function TunnelItem({
  tunnel,
  busy,
  registration,
  onSetId,
  onRegister,
  onMarkRegistered,
  onRemove,
  onOpenConsole,
  onIssueToken,
  clients,
}: {
  tunnel: DnsPilotTunnel;
  busy: boolean;
  registration?: DnsPilotRegistration;
  onSetId: (id: number) => Promise<void>;
  onRegister: () => Promise<void>;
  onMarkRegistered: () => Promise<void>;
  onRemove: () => Promise<void>;
  onOpenConsole: () => void;
  onIssueToken: () => Promise<void>;
  /** Registered devices of the tunnel (desktop). */
  clients: ReactNode;
}) {
  const { t } = useI18n();
  const [idInput, setIdInput] = useState('');
  const needs = registration?.result === 'needs_console' ? registration : null;
  return (
    <li className={styles.tunnel}>
      <div className={styles.row}>
        <strong className={styles.host}>{tunnel.hostname}</strong>
        <span className={tunnel.clientRegistered ? styles.ok : styles.pending}>
          {tunnel.clientRegistered ? t('dnspilot.ready') : t('dnspilot.notRegistered')}
        </span>
        {tunnel.status && (
          <span className={tunnel.status === 'active' ? styles.ok : styles.pending}>
            {tunnelStatusText(t, tunnel.status)}
          </span>
        )}
      </div>

      {tunnel.tunnelId === null && (
        <form
          className={styles.inline}
          onSubmit={(e) => {
            e.preventDefault();
            const id = positiveInt(idInput);
            if (id !== null) void onSetId(id);
          }}
        >
          <label htmlFor={`dp-id-${tunnel.hostname}`}>{t('dnspilot.tunnelId')}</label>
          <input
            id={`dp-id-${tunnel.hostname}`}
            inputMode="numeric"
            value={idInput}
            onChange={(e) => setIdInput(e.target.value)}
            aria-describedby={`dp-id-help-${tunnel.hostname}`}
          />
          <button type="submit" className="btn btn-sm" disabled={busy}>
            {t('dnspilot.save')}
          </button>
          <span id={`dp-id-help-${tunnel.hostname}`} className={styles.help}>
            {t('dnspilot.tunnelIdHelp')}
          </span>
        </form>
      )}

      {!tunnel.clientRegistered && (
        <div className={styles.row}>
          <button
            type="button"
            className="btn btn-sm btn-primary"
            disabled={busy}
            onClick={() => void onRegister()}
          >
            {t('dnspilot.register')}
          </button>
        </div>
      )}

      {needs && !tunnel.clientRegistered && (
        <div className={styles.manual} role="note">
          <p>{t(needsMessage(needs.reason))}</p>
          <ol className={styles.steps}>
            <li>{t('dnspilot.manualStep1')}</li>
            <li>
              {t('dnspilot.manualStep2')} <code className={styles.copy}>{needs.spki_sha256}</code>
            </li>
            <li>{t('dnspilot.manualStep3')}</li>
          </ol>
          <p className="muted">
            {t('dnspilot.manualCli')}{' '}
            <code className={styles.copy}>
              dnspilot client add {needs.tunnel_id ?? '<tunnel-id>'} --spki {needs.spki_sha256}
            </code>
          </p>
          <div className={styles.row}>
            <button type="button" className="btn btn-sm" onClick={onOpenConsole}>
              {t('dnspilot.openConsole')}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              disabled={busy}
              onClick={() => void onMarkRegistered()}
            >
              {t('dnspilot.markRegistered')}
            </button>
          </div>
        </div>
      )}

      {tunnel.clientRegistered && (
        <div className={styles.next}>
          <p>{t('dnspilot.nextPair')}</p>
          <pre className={styles.snippet}>{vcadConfigSnippet(tunnel)}</pre>
        </div>
      )}

      {clients}

      {tunnel.createdByApp && tunnel.tunnelId !== null && (
        <details className={styles.details}>
          <summary>{t('dnspilot.advanced')}</summary>
          <div className={styles.manual}>
            <p>
              <strong>{t('dnspilot.appTokenWarning')}</strong>
            </p>
            <p className="muted">{t('dnspilot.appTokenHelp')}</p>
            {tunnel.hasAppToken ? (
              <p>{t('dnspilot.appTokenStored')}</p>
            ) : (
              <div className={styles.row}>
                <button
                  type="button"
                  className="btn btn-sm"
                  disabled={busy}
                  onClick={() => void onIssueToken()}
                >
                  {t('dnspilot.appToken')}
                </button>
              </div>
            )}
          </div>
        </details>
      )}

      <div className={styles.row}>
        <button
          type="button"
          className="btn btn-sm btn-danger"
          disabled={busy}
          onClick={() => void onRemove()}
        >
          {t('dnspilot.remove')}
        </button>
      </div>
    </li>
  );
}

/** A two-step revoke: a registered phone stops connecting right away. */
function RevokeButton({
  label,
  disabled,
  onRevoke,
}: {
  label: string;
  disabled: boolean;
  onRevoke: () => void;
}) {
  const { t } = useI18n();
  const [asked, setAsked] = useState(false);
  if (!asked) {
    return (
      <button
        type="button"
        className="btn btn-sm"
        disabled={disabled}
        aria-label={label}
        onClick={() => setAsked(true)}
      >
        {t('dnspilot.revoke')}
      </button>
    );
  }
  return (
    <>
      <button
        type="button"
        className="btn btn-sm btn-danger"
        disabled={disabled}
        onClick={() => {
          setAsked(false);
          onRevoke();
        }}
      >
        {t('dnspilot.revokeConfirm')}
      </button>
      <button type="button" className="btn btn-sm" onClick={() => setAsked(false)}>
        {t('dnspilot.cancel')}
      </button>
    </>
  );
}

/** The keys registered on a tunnel and its unused invites, with revoke. */
function TunnelClients({
  service,
  tunnelId,
  onChanged,
}: {
  service: DnsPilotService;
  tunnelId: number;
  onChanged: () => void;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const [clients, setClients] = useState<DnsPilotAppClient[] | null>(null);
  const [invites, setInvites] = useState<DnsPilotInvite[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  const load = useCallback(async () => {
    try {
      const [c, i] = await Promise.all([service.appClients(tunnelId), service.invites(tunnelId)]);
      setClients(c);
      setInvites(i.filter((x) => x.status === 'active'));
    } catch (err) {
      setError(describeNativeError(t, err));
    }
  }, [service, tunnelId, t]);

  useEffect(() => {
    if (!open) return;
    let active = true;
    Promise.all([service.appClients(tunnelId), service.invites(tunnelId)])
      .then(([c, i]) => {
        if (!active) return;
        setClients(c);
        setInvites(i.filter((x) => x.status === 'active'));
      })
      .catch((err: unknown) => {
        if (active) setError(describeNativeError(t, err));
      });
    return () => {
      active = false;
    };
  }, [open, service, tunnelId, t]);

  const revoke = async (action: () => Promise<void>) => {
    setWorking(true);
    setError(null);
    try {
      await action();
      await load();
      onChanged();
    } catch (err) {
      setError(describeNativeError(t, err));
    } finally {
      setWorking(false);
    }
  };

  if (!open) {
    return (
      <div className={styles.row}>
        <button type="button" className="btn btn-sm" onClick={() => setOpen(true)}>
          {t('dnspilot.clients')}
        </button>
      </div>
    );
  }
  return (
    <div className={styles.manual}>
      <h5>{t('dnspilot.clients')}</h5>
      {error && (
        <p role="alert" className={styles.error}>
          {error}
        </p>
      )}
      {clients === null ? (
        !error && <p className="muted">{t('dnspilot.loading')}</p>
      ) : clients.length === 0 ? (
        <p className="muted">{t('dnspilot.clientsEmpty')}</p>
      ) : (
        <ul className={styles.tunnels} aria-label={t('dnspilot.clients')}>
          {clients.map((c) => {
            const name = c.label ?? t('dnspilot.clientUnnamed');
            return (
              <li key={c.id} className={styles.row}>
                <strong>{name}</strong>
                <span className="muted">
                  {[sourceText(t, c.source), c.thisApp ? t('dnspilot.clientThisApp') : null]
                    .filter((x) => x)
                    .join(' · ')}
                </span>
                <RevokeButton
                  label={t('dnspilot.revokeClient', { name })}
                  disabled={working}
                  onRevoke={() => void revoke(() => service.revokeAppClient(tunnelId, c.id))}
                />
              </li>
            );
          })}
        </ul>
      )}
      {invites.length > 0 && (
        <>
          <h5>{t('dnspilot.invites')}</h5>
          <ul className={styles.tunnels} aria-label={t('dnspilot.invites')}>
            {invites.map((i) => {
              const name = i.label ?? t('dnspilot.clientUnnamed');
              return (
                <li key={i.id} className={styles.row}>
                  <strong>{name}</strong>
                  {i.expiresAt && (
                    <span className="muted">
                      {t('dnspilot.inviteExpires', {
                        time: new Date(i.expiresAt).toLocaleString(),
                      })}
                    </span>
                  )}
                  <RevokeButton
                    label={t('dnspilot.revokeInvite', { name })}
                    disabled={working}
                    onRevoke={() => void revoke(() => service.revokeInvite(tunnelId, i.id))}
                  />
                </li>
              );
            })}
          </ul>
        </>
      )}
    </div>
  );
}
