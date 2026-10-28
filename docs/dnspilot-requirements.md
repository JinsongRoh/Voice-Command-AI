# DNS Pilot Integration

> **Status:** Draft v0.5. Based on the DNS Pilot feature guide and the DNS Pilot team's answers to VCA's technical questionnaire (console 1.24.3, Agent 0.5.4 for Windows and 0.5.0 for Linux, 2026-10-29). Items marked *in progress* are being built by the DNS Pilot team and are not yet in production.

DNS Pilot is one of VCA's interchangeable connectivity providers, alongside Tailscale, Cloudflare Tunnel, direct LAN and custom providers. This document covers four things:

1. how DNS Pilot works, as far as it matters for VCA;
2. what VCA's design must account for;
3. the integration plan;
4. the status of every gap.

---

## 1. Facts that shape the integration

### 1.1 Tunnel (DPT/1)

| Topic | Behavior |
|---|---|
| Routing | The relay routes by TLS SNI and forwards bytes; it never terminates TLS. |
| TLS termination | On the DNS Pilot Agent, on the user's machine. |
| Protocol to the local service | **HTTP/1.1 reverse proxy.** There is no HTTP/2, no HTTP/3 and no raw TCP mode. |
| WebSocket | HTTP/1.1 Upgrade is supported. HTTP/2 Extended CONNECT is not. |
| Local scheme | The target can be `http` or `https`. With `https`, the Agent does not verify the local certificate. |
| Timeouts | No idle or maximum lifetime on the relay pipe. However, the front proxy on port 443 closes connections after **1 hour with no traffic** (being raised to 24 hours, *in progress*). Port 9443 has no such limit. |
| Streaming | Server-sent events and responses without a known length are flushed immediately. Always-flush is *in progress*. |
| Body size | No limit other than the monthly transfer quota. |
| Relay restart | Drops all connections. Graceful drain with GoAway is *in progress*. Clients must always handle reconnection. |
| Regions | Seoul (primary) and Tokyo (standby), with automatic failover after repeated failures. There is no latency-based selection and no latency measurements yet. |
| Certificates | One Let's Encrypt certificate per tunnel hostname. **Every hostname appears in public Certificate Transparency logs.** |
| Certificate keys | Generated and stored encrypted by the DNS Pilot console, then delivered to the Agent. The relay cannot decrypt traffic, but the console side theoretically could. |
| Forwarded headers | `X-Forwarded-For` (single visitor IP) is trustworthy. **`X-Real-IP` is passed through unmodified and can be spoofed.** There is no country header. |
| Access modes | Public, password and organization login. Without a session cookie, password and organization-login modes return 401 (or a login page), which a native app cannot pass. |

### 1.2 P2P (DPT/2)

- Direct QUIC with NAT traversal and relay fallback. There is no relay fallback on the Free plan.
- Implemented in Go. A **gomobile binding already exists** (`NewClient`, `StartLogin`, `Start`, `Connect`, `NetworkChanged`, and so on).
- An Android "viewer" client exists. There is no iOS client yet.
- Both peers must currently belong to the same DNS Pilot organization.
- Network changes (Wi-Fi ↔ cellular) are handled by re-detecting NAT, while the connection is carried over the relay in the meantime. This has not yet been validated on real devices.
- Direct-connection success rates on cellular networks have not been measured.

### 1.3 Automation and platform

- **Console API:**
  - Listing, enabling, disabling and deleting tunnels and P2P services works with API tokens.
  - **Creating tunnels or changing their access mode requires an interactive re-authentication.** Third-party apps therefore need OAuth (*in progress*).
- **Agent local control:**
  - An IPC channel exists (named pipe on Windows, Unix socket on Linux).
  - Tunnel control requires administrator rights and only changes local state.
  - A per-user local REST API with a kill switch and an event stream is *in progress*.
- **Agent modules:** compiled-in Go packages. VCA (Rust) cannot be embedded directly, so the module will **install and supervise the standalone VCA build**.
- **Platforms and signing:**
  - The Windows Agent is not code-signed yet, so it ships as a preview.
  - The macOS Agent is built but not released.
- **Device activation:** always requires browser approval with re-authentication. Unattended activation is not possible, by design.
- **Payments:** currently in one local currency only. International payment support is required for a global audience.
- **Status:** a public, machine-readable status API exists, but it has no relay or P2P fields yet.

## 2. Design consequences for VCA

These apply to VCA's own code regardless of DNS Pilot's roadmap.

| # | Consequence | Where it lands in VCA |
|---|---|---|
| C1 | **Treat every tunnel as an untrusted transport.** The tunnel's TLS is not end-to-end from VCA's point of view, because the provider's console holds the certificate keys. VCA's Noise channel between the paired device and the server is mandatory for every provider, and authentication never relies on tunnel-level controls. | `vca-net`, security model |
| C2 | **Serve plain HTTP on loopback** (`127.0.0.1:47800`, local scheme `http`). Local HTTPS adds nothing because the Agent does not verify it, and all confidentiality comes from Noise. | `vca-server` |
| C3 | **Use HTTP/1.1 WebSocket only.** Do not depend on HTTP/2 or HTTP/3 features. | `vca-net`, mobile client |
| C4 | **Application heartbeat at most every 30 s** in both directions. This keeps idle-timeout proxies (1 h here, often shorter elsewhere) from closing connections. | protocol |
| C5 | **Resumable streams and fast reconnection are required.** Relay restarts and failovers drop connections. Sequence-numbered streams resume from the last offset (already in the architecture). | protocol |
| C6 | **Use non-identifying tunnel hostnames.** Certificate Transparency logs publish every hostname. VCA generates random labels (for example `vca-k3x9q2.<domain>`) and never includes user names, machine names or project names. | setup wizard |
| C7 | **Trust only `X-Forwarded-For`** for the client IP shown in the audit log. Ignore `X-Real-IP`. Treat the forwarded IP as informational, since VCA authenticates by device key. | `vca-server` audit |
| C8 | **The DNS Pilot P2P client is an optional, separately licensed component.** The default open-source build of VCA Mobile must compile and work without it. Release builds may include it through a feature flag, subject to a redistribution license from DNS Pilot. | `apps/mobile`, build system |

## 3. Integration plan

### 3.1 Track A: Tunnel with `app` access mode

```
VCA Mobile ──HTTPS/WSS (HTTP/1.1, mTLS client key)──► relay ══► DNS Pilot Agent ──http──► 127.0.0.1:47800
     └────────────────────── Noise channel (VCA pairing keys) ───────────────────────────────┘
```

1. The setup wizard signs in to DNS Pilot through OAuth device flow (scope `tunnels:write`).
2. It creates an `app`-mode tunnel with a random hostname (C6), local scheme `http`, and port 443.
3. During pairing, the phone's client public key is registered with the tunnel through the Agent's local API.
4. The hostname goes into the pairing QR code.

*Depends on:* `app` mode, OAuth, the local REST API and the 24-hour front-proxy timeout. All are in progress at DNS Pilot.

### 3.2 Track B: P2P from the phone

```
VCA Mobile ── Tauri plugin ── DNS Pilot P2P client (gomobile) ◄─QUIC / relay─► DNS Pilot Agent ──► 127.0.0.1:47800
```

- **Step 1:** a Tauri 2 plugin (Kotlin / Swift) wraps the existing gomobile binding. This avoids waiting for a Rust port. The binding exposes a local forwarded port that the VCA client connects to.
- **Step 2:** delegated access grants signed by the target Agent, so the phone needs no DNS Pilot account. This is planned on the DNS Pilot side.
- Until delegated grants exist, the phone must sign in to the same DNS Pilot organization.

Tracks A and B are developed in parallel. When both are available, the setup wizard prefers P2P and falls back to the tunnel.

### 3.3 Distribution as an Agent module

A DNS Pilot Agent module installs, updates and supervises the same signed VCA build that the standalone installers ship, and wires it to the DNS Pilot provider. This is planned as a second-phase item.

### 3.4 Other providers

Tailscale, Cloudflare Tunnel, direct LAN and custom providers are supported on equal footing. With any provider, the user is responsible for configuring that provider securely. VCA's pairing, authentication and Noise encryption always apply (C1).

## 4. Gap status

| # | Gap | Status at DNS Pilot | Needed by VCA |
|---|---|---|---|
| G1 | `app` access mode (mTLS public-key pinning, optional bearer token) | In progress | M2 |
| G2a | Phone-side P2P client (gomobile binding for Android and iOS, redistributable) | Binding exists. iOS build and a redistribution license are needed. | M2 |
| G2b | Delegated P2P grants (phone without a DNS Pilot account) | Planned (second phase) | M3 |
| G3 | 24-hour front-proxy timeout, always-flush streaming, graceful relay drain | In progress | M2 |
| G4 | Push notification relay (APNs/FCM, end-to-end encrypted payloads) | In progress | M2 |
| G5 | Relay regions in North America and Europe, latency-based selection, latency measurements | Not started | M4 (global launch) |
| G6 | Code-signed Windows Agent, released and notarized macOS Agent | Business decision pending | M4 |
| G7 | Per-user local REST API, kill switch and event stream | In progress | M2 |
| G8 | Per-connection access log and anomaly events | In progress | M2 |
| G9 | Plans for open-source users and international payments | Business decision pending | M4 |
| G10 | Public English landing page and public API documentation (currently 404 and login-only, respectively) | Pending | M4 |
| G11 | OAuth device flow with `tunnels:*` and `p2p:*` scopes, so that `app`-mode tunnels can be created without interactive re-authentication | In progress | M2 |
| G12 | API usage policy and brand guidelines for third-party open-source clients | Legal task pending | Before DNS Pilot provider ships |
| G13 | Strip client-supplied `X-Real-IP` | Not started (reported) | — (VCA ignores it, see C7) |
| G14 | Agent-generated certificate keys (CSR on the Agent), so the console never holds tunnel private keys | Not started | Nice to have (VCA relies on Noise, see C1) |
| G15 | Relay and P2P fields in the public status API | Not started | M2 (provider failover) |
