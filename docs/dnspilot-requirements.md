# DNS Pilot Integration Requirements

> **Status:** Draft v0.2. This document will be split into "already supported" and "to be built" once the DNS Pilot feature specification is available.

This document defines what Voice Command AI (VCA) needs from DNS Pilot, its default remote-connectivity provider. It is written so that it can be handed directly to the DNS Pilot team (or to an AI coding agent working on DNS Pilot) as a requirements brief.

---

## Brief

You are working on DNS Pilot, a DDNS and private-tunnel service. The open-source project **Voice Command AI** (VCA) will use DNS Pilot as its default way to reach a user's machine from the internet. VCA works like this:

- A long-running Rust server (VCA Server) runs on the user's development machine.
- A mobile app (VCA Mobile) connects to that server from anywhere.
- Through that connection, the user controls AI coding agent sessions such as Claude Code, Codex and Gemini CLI.

Review the requirements below. Classify each item as **supported**, **needs development** or **not feasible**, and propose an API or SDK design for each.

### 1. Private tunnel

- Expose a local service (for example `127.0.0.1:47800`) to the internet **without port forwarding**. It must work behind CGNAT, corporate firewalls and double NAT.
- Protocols:
  - Required: HTTPS and WebSocket, with long-lived bidirectional streaming.
  - Optional: raw TCP and QUIC.
- Latency target: at most 50 ms of added round-trip time within the same region. Voice-command responsiveness and terminal streaming depend on it.
- Resilience:
  - Automatic reconnection, with session continuity across reconnects.
  - Heartbeats and connection-state events.
- A stable hostname per machine (for example `<machine>.<user>.<dnspilot-domain>`). TLS certificates are issued and renewed automatically.

### 2. Local control API and SDK (VCA Server → DNS Pilot Agent)

- A local API that lets VCA drive the DNS Pilot Agent programmatically. It listens on localhost only and is authenticated with a local token. It covers:
  - Create, list, pause and delete tunnels.
  - Query the public URL and tunnel status.
  - Subscribe to events: connected, disconnected, inbound connection attempt.
- Provide a Rust crate or an OpenAPI specification.
- Provide a CLI, for example `dnspilot tunnel open --port 47800 --name vca`.
- Provide a way to detect whether the Agent is installed and which version it is. Also provide a sign-in and linking flow (such as device-code login) for VCA's setup wizard.

### 3. Tunnel-level security

Access control at the tunnel edge, as a layer independent from VCA's own authentication:

- Allow-list of paired devices (device certificates or mTLS, or device tokens).
- Optional IP and country allow/deny rules.
- Rate limiting and brute-force protection.
- **Kill-switch API:** immediately close the tunnel and drop all active connections. It must also be triggerable from the web console and from mobile.
- **Access audit log:** timestamp, source IP, device identifier and outcome, with a query API.
- **New-device and new-location alerts**, delivered via webhook or push.
- State clearly whether end-to-end encryption that the tunnel operator cannot read is supported, and how (for example TLS passthrough).

### 4. Push notification relay

A user's machine cannot send push notifications through APNs or FCM directly, so DNS Pilot relays them.

- An API for the mobile app to register its push token against a DNS Pilot account and machine.
- An API for VCA Server to request a push:
  - Inputs: target device, title, body and data.
  - Options: priority and sound.
- Payloads are **end-to-end encrypted by VCA**. The relay never sees plaintext, and the app decrypts on receipt.
- Per-account delivery quotas and a retry policy for failed deliveries.
- Support for both iOS and Android, including the procedure for registering the VCA app's bundle IDs and credentials.

### 5. Multiple machines

- One account can register several machines. Provide an API that returns each machine's tunnel status and online state.
- The mobile app must be able to fetch the machines registered to the account and connect to any of them.

### 6. Commercial and open-source policy

- Define a free tier for VCA users and where paid plans begin (traffic, number of machines, push volume, and so on).
- Provide a public client ID and API terms that may be embedded in open-source code.
- Define behavior during a service outage. For example, provide a status API so that VCA can fail over to an alternative provider (Tailscale, Cloudflare Tunnel).

### Requested deliverables

1. Status of each item (supported / needs development / not feasible) with effort estimates.
2. Draft API specifications (OpenAPI) for the local API, the SDK and the push relay.
3. Security model: encryption boundaries, key management and threat model.
4. Sequence diagram of the integration flow used by the VCA setup wizard.
