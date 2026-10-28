# DNS Pilot Integration Requirements

> **Status:** Draft v0.4. Based on the DNS Pilot feature guide (console 1.24.2, 2026-10-28). Pricing and plan limits on the DNS Pilot side are not final. The DNS Pilot team has agreed to close the gaps below; implementation is tracked on the DNS Pilot side.

DNS Pilot is VCA's default connectivity provider. This document covers four things:

1. what DNS Pilot already offers;
2. how VCA will use it;
3. the gaps that must be closed;
4. a requirements brief for the DNS Pilot team.

---

## 1. Relevant DNS Pilot capabilities

| Capability | Summary | Relevance to VCA |
|---|---|---|
| **DNS Pilot Agent** | Resident app for Windows 10/11 (x64, arm64; preview) and Linux (amd64, arm64, armv7; production). macOS is built but not yet released. Android exists as a reference viewer only. Signed auto-update. | Runs next to VCA Server on the developer machine. |
| **Device activation** | Browser-based device-code flow (8-character code) with the account's 2FA or passkey. No password is stored in the app. Devices can be blocked instantly, including from an e-mail link. | Matches VCA's setup-wizard needs. |
| **Tunnel (DPT/1)** | Outbound connection from the Agent to a relay. Exposes `https://name.domain` with no port forwarding. TLS is terminated **on the Agent** (the relay only sees SNI and byte counts). Certificates are issued automatically. Port 443 or 9443. Paid plans only. | Primary remote path for VCA Mobile. |
| **Tunnel access modes** | Password, organization login (2FA, with an `X-DNSPilot-User` header passed to the local service) and fully public (reviewed per organization). | Edge access control, but designed for browsers (see gap G1). |
| **P2P (DPT/2)** | Agent-to-Agent direct QUIC connection with NAT traversal. Falls back automatically to an encrypted relay (about 80–90 Mbps measured). Destinations are restricted to services registered in the console. | Ideal path for VCA Mobile, provided the phone can act as a P2P peer (see gap G2). |
| **DDNS** | Agent-driven updates (60 s TTL), token API and DNSEver-compatible API. Free `<name>.yoitne.kr` hostnames. | Fallback for users who prefer port forwarding. |
| **Console API** | Scoped API tokens (scopes, IP restriction, expiry), webhooks and an audit log. Tunnels and P2P services can already be created entirely through the API. | Programmatic tunnel and P2P setup from VCA. |
| **Agent modules** | Console-managed extensions with settings and secrets. | Possible optional distribution channel for VCA. |
| **Account security** | TOTP, passkeys, IP allow-lists, login rate limiting, re-authentication for sensitive actions. | Strong baseline for the account that owns the tunnel. |

## 2. Integration design

### 2.1 Track A: Tunnel (needs R1)

```
VCA Mobile ──HTTPS/WSS (SNI=vca-<machine>.<domain>)──► relay ══► DNS Pilot Agent ──► 127.0.0.1:47800 (VCA Server)
     └───────────────────── Noise E2E channel (VCA pairing keys) ─────────────────────────────┘
```

- TLS terminates at the Agent on the user's own machine, so neither the relay nor any third party can read traffic. VCA adds its own Noise end-to-end layer on top, so security does not depend on the tunnel configuration.
- The VCA setup wizard performs these steps:
  1. Detect the Agent.
  2. Start device activation if needed.
  3. Create the tunnel or P2P service through the console API (already supported).
  4. Embed the resulting hostname in the pairing QR code.

### 2.2 Track B: Embedded P2P (needs R2)

Tracks A and B are developed **in parallel**. VCA supports both, and the setup wizard picks P2P when the direct path works, falling back to the tunnel otherwise.

```
VCA Mobile (embedded DPT/2 client) ◄──QUIC direct / relay fallback──► DNS Pilot Agent ──► 127.0.0.1:47800
```

- Lower latency and no relay bandwidth cost when a direct path exists.
- The service never gets a public hostname, which shrinks the attack surface.
- This path could be offered on lower-priced plans, which lowers the barrier for open-source users.

### 2.3 Distribution as a DNS Pilot Agent module

Besides its standalone installers, VCA is also distributed as a **DNS Pilot Agent module**. DNS Pilot users can enable VCA from the console with one click. The module and the standalone build are the same software; the module simply comes pre-wired to the DNS Pilot provider.

### 2.4 Other providers (bring your own)

VCA treats connectivity providers as interchangeable choices. Users are free to pick whichever fits them, through VCA's provider abstraction:

- built-in providers: Tailscale, Cloudflare Tunnel and direct LAN;
- a **custom provider**: any tunnel or VPN that can forward to `127.0.0.1:47800`.

With any provider other than DNS Pilot, the user is responsible for configuring that provider securely. VCA's own pairing, authentication and Noise end-to-end encryption always stay in force, whatever the provider.

## 3. Gap analysis

| # | Gap | Impact | Priority |
|---|---|---|---|
| **G1** | **No access mode for native apps.** Password and organization-login modes are interactive browser pages, which a native app or WebSocket client cannot pass. Fully public mode requires per-organization review and removes edge protection. | Blocks Phase A without weakening security. | **Critical** |
| **G2** | **No embeddable P2P client.** DPT/2 requires a full Agent on both sides. There is no iOS client, and Android is a reference viewer only. | Blocks Phase B. | High |
| G3 | WebSocket and long-lived connection support through tunnels is not documented (upgrade handling, idle timeouts, maximum connection duration). | Live streaming and notifications rely on it. | High |
| G4 | No **push notification relay** (APNs/FCM). Webhooks go to chat tools only. | Background notifications on mobile. | High |
| G5 | Relay regions cover Seoul and Tokyo only. | High latency for users in the Americas and Europe. | High (global launch) |
| G6 | The macOS Agent is not released, and the Windows Agent is in preview. | macOS is a primary VCA platform. | High |
| G7 | There is no documented **local Agent API** (localhost) for detecting the Agent and toggling tunnels, and no programmatic kill switch. | Setup wizard automation and VCA kill switch. | Medium |
| G8 | No per-request **tunnel access log** or new-location alerts for tunnel visitors (only demo-link visits are logged). | VCA security alerts (SEC-08). | Medium |
| G9 | Tunnels require a paid plan, and free hostnames use a country-specific domain. | Adoption barrier for a global open-source audience. | Medium |
| G10 | The English landing page is built but not deployed. | VCA documentation links to DNS Pilot. | Low (but needed before launch) |

## 4. Requirements brief for the DNS Pilot team

> Hand this section to the DNS Pilot team (or to a coding agent working on DNS Pilot) as-is.

You are working on DNS Pilot. The open-source project **Voice Command AI** (VCA) will use DNS Pilot as its default remote-connectivity provider. VCA works like this:

- A Rust server listens on `127.0.0.1:47800` on the developer's machine.
- A native mobile app connects to that server over HTTPS and WebSocket.
- Through that connection, the user controls AI coding agents.

VCA already has its own device pairing and end-to-end encryption. From DNS Pilot it needs reachability, edge protection and push relay. Implement or specify the following, and for each item return the API design and an effort estimate.

### R1. App access mode for tunnels (closes G1, critical)

Add a fourth access mode, **"App token"**, for non-browser clients:

- The tunnel accepts requests carrying a valid credential and rejects everything else at the relay or Agent, before the request reaches the local service. The credential is either:
  - `Authorization: Bearer <tunnel-client-token>`, or
  - an mTLS client certificate.
- Client tokens are issued per device, scoped to one tunnel, revocable, and optionally expiring. They are created through the console API so that VCA can mint one during pairing.
- The mode must work for WebSocket upgrades.
- Optional: allow this mode to be combined with organization login, so that browsers use login and apps use tokens on the same tunnel.

### R2. Embeddable P2P client SDK (closes G2)

- Provide a DPT/2 client library (ideally a Rust crate with iOS and Android bindings) that a third-party mobile app can embed to connect to a registered P2P service on an Agent, without installing the Agent on the phone.
- Authentication: a scoped client credential issued through the console API, as in R1.
- Behavior: direct QUIC first, then relay fallback, then automatic reconnection on network change (Wi-Fi ↔ cellular).
- Must respect iOS and Android background and network restrictions.

### R3. WebSocket and long-lived connections (closes G3)

Document and guarantee the following:

- WebSocket upgrade through tunnels.
- Idle timeout of at least 10 minutes, or no idle timeout when keep-alive pings are present.
- No forced maximum connection duration, or one of at least 24 hours.
- Streaming responses without buffering.

### R4. Push notification relay (closes G4)

- An API to register a mobile push token (APNs/FCM) against an account and machine.
- An API for the local service to send a push:
  - Inputs: target device(s), opaque **end-to-end encrypted** payload, priority, collapse key.
  - The relay never sees plaintext; the app decrypts on receipt.
- Delivery quotas per plan, retries and delivery status.
- A registration process for third-party app credentials (VCA's iOS bundle ID and Android package).

### R5. Additional relay regions (closes G5)

In addition to Seoul and Tokyo: North America (west and east), Europe and Southeast Asia. The Agent selects a region automatically by latency and fails over on outage.

### R6. Platform coverage (closes G6)

General availability of the macOS Agent (arm64 and amd64) and of the Windows Agent.

### R7. Local Agent API (closes G7)

A localhost-only API (or CLI) authenticated with a local token. It provides:

- Agent presence, version and sign-in state.
- Start device activation.
- List, enable and disable tunnels and P2P services owned by this device.
- **Kill switch:** close all tunnels and P2P sessions of this device immediately.
- An event stream: tunnel up and down, inbound connection, client token rejected.

### R8. Tunnel access log and alerts (closes G8)

- A per-request (or per-connection) log with timestamp, source IP, country, client token ID and result.
- A query API for these logs.
- Alerts for a new client token or new country, and for bursts of rejected requests.

### R9. Plans and naming for open-source users (closes G9)

- Consider a free or low-cost tier that covers one machine with P2P (R2) or one app-token tunnel with modest transfer limits.
- Consider a globally neutral free-hostname domain alongside the existing country-specific one.

### R10. Public developer documentation (closes G10)

- Deploy the English landing page.
- Expose API documentation pages without login, so that VCA's documentation can link to them.

### R11. VCA as an Agent module

- Package VCA Server as a DNS Pilot Agent module that can be enabled from the console.
- The module installs or updates the same signed VCA build that the standalone installers ship, and wires it to the DNS Pilot provider automatically (tunnel or P2P service, client credentials, push relay).
- Define the module interface, signing and release process for a third-party, open-source module.

### Requested deliverables

1. Status and effort estimate for R1–R11.
2. OpenAPI specifications for R1, R4, R7 and R8, and the SDK interface for R2.
3. Security model notes covering credential lifecycles and what the relay can observe.
4. Sequence diagram of the VCA setup-wizard integration (activation → tunnel or P2P service → client credential → pairing).

## 5. Open questions for DNS Pilot

A detailed technical questionnaire has been sent to the DNS Pilot team. Answers will be folded into this document. Settled so far:

- Tunnels and P2P services can be created entirely through the console API.
- VCA will also be distributed as an Agent module (R11).
