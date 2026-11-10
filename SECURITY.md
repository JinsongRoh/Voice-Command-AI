# Security policy

Voice Command AI (VCA) lets a paired phone type into AI agent sessions and open shells on your PC. We take reports about it seriously and thank everyone who reports a problem responsibly.

## Supported versions

VCA is pre-release. Only the **latest release** (and the `main` branch it is built from) receives security fixes. Desktop apps update themselves from GitHub Releases; please update before you report, and say which version you tested.

| Version | Supported |
|---|---|
| Latest release | Yes |
| Older releases | No: update to the latest release |
| Development branches other than `main` | No |

## Reporting a vulnerability

**Do not open a public issue, discussion or pull request for a security problem.**

Report it privately through GitHub's private vulnerability reporting:

1. Open <https://github.com/JinsongRoh/Voice-Command-AI/security/advisories/new> (or **Security > Report a vulnerability** on the repository page).
2. Describe the problem, the affected version and platform, and the steps to reproduce it. A proof of concept helps.
3. Remove secrets from what you send: authenticator or backup codes, pairing links, tokens, API keys, private keys and personal paths.

There is no security email address. Private reports go only through GitHub.

## What to expect

The project is maintained by a small team, so these are targets, not guarantees:

- **Acknowledgement** within 7 days.
- **First assessment** (whether we can reproduce it and how severe it is) within 14 days.
- **Updates** on the advisory as the fix progresses.
- **Fix and disclosure:** we aim to ship a fix in a new release within 90 days, sooner for severe issues, and then publish the advisory. We credit you in the advisory unless you ask us not to.

Please give us a reasonable time to fix the problem before you disclose it publicly. Test only against machines and accounts that you own.

## Scope

In scope are the code in this repository and the builds published in its GitHub Releases, in particular:

| Area | What we want to hear about |
|---|---|
| **Pairing** | Pairing without the one-time pairing code, reuse of a code after it was used or expired (5 minutes), or pairing a device with more permissions than were granted. |
| **Noise encryption** | Anything that weakens the end-to-end Noise (`XX`) channel between a paired device and `vcad`: downgrade, key confusion, replay, or reading or changing traffic from a connectivity provider (LAN, Tailscale, Cloudflare Tunnel, DNSPilot), which VCA treats as an untrusted transport. |
| **TOTP** | Bypassing the authenticator code at sign-in or for step-up actions, reusing a code, or brute forcing despite rate limiting and lockout. |
| **Device permissions and tokens** | A view or command device doing what needs admin permission, revoked devices or the kill switch not cutting access, token theft or refresh-token misuse. |
| **Workspace allow-list** | Creating sessions, opening shells, restoring windows or writing attachments outside `workspace_roots`, for example through path traversal or links. |
| **Agent hooks** | Hook installation changing more than VCA's own entries in agent settings, hooks that run a different program than the `vca` installed with the app, or hook events that let a process on the PC impersonate an agent's approval. |
| **Remote shell** | Opening, watching or typing into a remote shell without admin permission and a fresh authenticator code, getting administrator rights through it, dangerous commands that skip the step-up check, or secrets written to the audit log. |
| **The conductor** | The conductor sending to a session without your confirmation, answering a permission decision, reaching a remote shell, or following instructions from untrusted session output. |
| **Updater signatures** | The desktop app installing an update whose minisign signature does not match the public key in the app, or fetching updates from anywhere other than the configured GitHub Releases endpoint. |
| **Secrets** | Device keys, tokens or engine API keys leaving the PC or the OS credential store, or appearing in logs, the UI or the audit log. |

Out of scope:

- Problems in third-party services and tools that VCA connects to (agent CLIs, speech or LLM engines, Tailscale, Cloudflare, DNSPilot). Report those to their owners. For DNSPilot, see <https://dnspilot.kr>.
- Attacks that need an attacker who already runs code as your user on the PC. VCA runs as your user and cannot protect against that user.
- Settings that the documentation marks as unsafe, such as `[sessions] allow_generic_launch = true`, publishing `vcad` through a public tunnel, or approving agent commands that bypass VCA's tools.
- Missing hardening without a concrete attack, and reports from automated scanners without a working proof.
