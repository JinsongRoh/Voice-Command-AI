/**
 * Messages the UI needs that are NOT yet part of vca-core.
 *
 * They are kept separate from `types.ts` so that file stays an exact mirror of
 * the Rust protocol. Each one is listed as a requested core change; once
 * vca-core adopts them (possibly with different names), move them over.
 * A real server that does not know them answers `error { code: "invalid_request" }`
 * (deserialization failure) and the UI degrades gracefully.
 */
import type { CloudProvider, UnixMillis } from './types';

/** CLD-03: start a cloud session (provider, repository, prompt). */
export interface CloudLaunchRequest {
  provider: CloudProvider;
  repository: string;
  prompt: string;
  display_name: string | null;
}

/** SEC-10: a device paired with the server. */
export interface PairedDevice {
  id: string;
  name: string;
  platform: string;
  paired_at: UnixMillis;
  last_seen: UnixMillis;
  /** True for the device running this UI. */
  current: boolean;
}

export type ProposedClientMsg =
  | { type: 'launch_cloud'; request: CloudLaunchRequest }
  | { type: 'list_devices' }
  | { type: 'revoke_device'; device_id: string }
  /** SEC-09: drop every remote connection and close the tunnel. */
  | { type: 'kill_switch' };

/** Answers to proposed requests. `launch_cloud` answers with the core `launched` message. */
export type ProposedServerMsg = { type: 'devices'; devices: PairedDevice[] };
