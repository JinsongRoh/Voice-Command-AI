# Security setup on the server computer

Voice Command AI has no user accounts of its own: the owner of the computer,
verified by the operating system, is the identity. This page covers local
verification, mail notifications and recovery. The design of pairing and
encryption is in [architecture.md](architecture.md).

## Local verification

Security-sensitive actions on the server computer need a fresh proof that the
owner is sitting at it:

| System | How |
|---|---|
| Windows | Windows Hello; the authenticator code if Hello is not set up |
| macOS | Touch ID or the login password |
| Linux, headless servers | The authenticator code. `vca` asks for it on the terminal |

A verification approves one request. vcad decides what that is: a gated request
without a token gets a challenge, the prompt describes that request, and the token
that comes out works once, for that request, and only for the process that asked.
What the prompt says never comes from the caller.

Four kinds of work are several requests long, so one verification covers that kind
of work (and nothing else) for a short grace window, 2 minutes by default
(`vca security grace <seconds>`, 30 to 300). Using it never extends it:

- setting up or replacing the authenticator (start, confirm, a mistyped code);
- recovery (start, then finish once the mail arrives);
- mail settings (the form and its secrets, saved together);
- allowing what the conductor asks, which comes in a row while you work with it.

Everything else asks every time, even inside a grace window: creating a pairing
code (connecting a phone), widening a device's permission, and the other actions
listed under "What needs it" below. The app's setting describes it the same way:
the grace window covers the steps of one task, such as setting up the authenticator.

Canceled prompts and wrong codes slow further attempts down, up to one try a minute.
They never lock you out: any program on this computer can cause failures, and you
must still get in. A prompt nobody answers ends after 3 minutes.

Paired devices can never trigger, satisfy or extend a verification.

### The first authenticator on a computer with no system prompt

On a computer with neither an OS verifier nor an authenticator there is nothing to
verify against. So the first enrollment needs a person to say so: the first-run
confirmation in the desktop app, or `vca totp enroll` on an interactive terminal,
where you type `yes`. vcad refuses this from anything running inside a session VCA
started. On Linux it also refuses anything without a terminal, except the desktop
app installed next to vcad; a desktop app that is not installed there (a development
build) uses `vca totp enroll` instead. Everything else stays
refused until an authenticator exists. That includes pairing, so on a headless
server run `vca totp enroll` first.

This is the weakest step in the system, and there is no strong answer to it. A
one-time code printed by the server, or written to a file, does not help: a program
running as the owner can read those as well as you can. So this step keeps out
agents VCA runs and anything without a terminal, and it does not keep out a hostile
program you started yourself in your own terminal. Set the authenticator up during
installation, before you run agents on that computer.

### What needs it

Creating a pairing code, setting up, replacing or turning off the authenticator,
new backup codes, widening a device's permission, turning remote shells on,
security and mail settings, recovery, allowing remote access again after the kill
switch, changing or starting the conductor, allowing it a session or a request,
the task settings, and sending a saved engine key to a new address.

New backup codes and a new place for mail need more than the approval: a code from
the current authenticator too, and so does replacing or deleting a sending secret.
Otherwise one approval could create a recovery proof, or read the recovery mail. On
a computer with no OS prompt the code you verified with counts: one code is enough.

Revoking or renaming a device, narrowing its permission and tripping the kill
switch stay available to an admin device, so a phone can cut off a stolen one.
Only the computer itself can widen a permission.

### What it stops, and what it does not

The gates are in the server, because anything running as the owner can reach the
local channel, including an AI agent or a paired device using a remote shell.

**Step-up does not contain a hostile program running as the owner.** It is there
to stop mistakes and casual misuse, to make a person look at what is about to
happen, and to leave a record. It does not defend against code that is determined
and already runs under your account. Run agents you trust, or run them under
another account.

On Windows and macOS this stops an agent from pairing a device or changing these
settings through VCA unless a person at the computer approves the system prompt. An
authenticator code does not replace that prompt.

On a computer with no OS prompt, a code decides, so no code is accepted while any
remote shell is open. Close the shells first.

Requests from a remote shell are refused where vcad can tell: it looks up the
process on the other end of the channel and its parents, and shells also mark their
processes in the environment. A process can leave that tree (a background service,
a scheduled task), and an environment variable can be unset, so this catches
mistakes and ordinary commands, not an intruder who works around it.

It does not stop:

- a person at the computer approving a prompt they did not start. The prompt names
  the action, and vcad chooses that text, so read it;
- code that runs as the owner and works on the owner's files directly. Such code
  can read or change anything below the data directory, such as `totp.json` or
  `devices.json`, or read another of the owner's processes. Verification proves a
  person was present; it is not a sandbox;
- on Linux and headless servers, anyone holding a valid authenticator code, or
  able to read `totp.json`. There is no OS prompt, so a remote shell user with the
  authenticator can pass where vcad cannot tell it is a remote shell. If that
  matters, keep remote shells off: `vca security remote-shell off`;
- a program of yours that is not an agent of VCA, on a computer with no verifier and
  no authenticator yet (see above);
- a program running as the owner that reads a session's hook token from its process
  environment, or an observed session's id from a transcript, and then forges that
  session's hook events. Managed sessions need their token and observed sessions
  need their id, which keeps other sessions out; neither is a secret from the owner.

### App lock

The desktop app also uses this verification to unlock itself: it opens locked, locks
again after a while without use, and asks again before the Command Center when the
last unlock is more than 5 minutes old. It uses the same test prompt as **Settings →
Test verification**, which approves nothing. With no OS prompt it takes the
authenticator code. With neither it opens with a warning. The lock is in the app:
it keeps a person holding an unlocked computer or phone out of the app, but it does
not protect the server, which still gates its own actions as described here. See
the [user guide](user-guide.md#app-lock).

## Mail

Mail carries security notifications and recovery codes. It is optional. Set it
up in the app under Settings > Security > Mail, or with the CLI. Every change
needs local verification. Changing the addresses or what delivers to them also
needs a code from the authenticator, when one is set up: mail carries the recovery
code. The old address is told over the old settings, so a new server cannot hide
the change, and this mail goes out even with notifications turned off. A secret
that is replaced or deleted is reported before it changes, while mail still works.

There are two modes.

**SMTP**: direct delivery with STARTTLS or implicit TLS. An unencrypted
connection is only accepted to a server on this computer.

```sh
vca mail set --mode smtp --host smtp.example.com --tls starttls \
  --username owner@example.com --sender owner@example.com --to owner@example.com
vca mail password   # hidden prompt
vca mail test
```

**Relay**: `POST <relay_url>/v1/notify/mail` with the JSON body
`{to[], subject, text, html?, kind}` and nothing else. The relay chooses the
sender, and this computer holds no mail password. Authentication works like the
speech engines: the relay key goes in the `api_key_header` header (or
`Authorization: Bearer` when unset), and an optional gateway token goes in
`Authorization: Bearer`.

```sh
vca mail set --mode relay --relay-url https://relay.example.com \
  --api-key-header X-VCA-Key --to owner@example.com
vca mail relay-key       # hidden prompt
vca mail gateway-token   # hidden prompt, optional
vca mail test
```

Secrets are never accepted as arguments. They are stored in the server
credential store (owner-only files below the data directory), not in a settings
file.

### Install-time preset

An installer can preload settings with `mail-preset.toml` next to `config.toml`
(or at the path in `VCA_MAIL_PRESET`). Every key is optional, and unknown keys are
an error. Preset values are defaults: the owner can override them after local
verification, except `allowed_recipient_domains` and `gateway_token_env`, which
only the preset sets. Secrets do not belong in this file; set them afterwards
with the commands above.

```toml
mode = "relay"                      # or "smtp"
sender_name = "Voice Command AI"
language = "en"
recipients = []
allowed_recipient_domains = ["example.com"]  # owner addresses must match

# relay
relay_url = "https://relay.example.com"
api_key_header = "X-VCA-Key"
gateway_token_env = "VCA_RELAY_GATEWAY_TOKEN"

# smtp
smtp_host = "smtp.example.com"
smtp_port = 587
smtp_tls = "starttls"               # or "tls"; "none" for localhost only
smtp_username = "sender@example.com"
sender_address = "sender@example.com"  # SMTP only
```

### Notifications

When mail is set up, these are on by default and can be turned off one by one:
a new device paired, repeated failed attempts (at most one mail per 15 minutes),
authenticator changes, recovery started or completed, and mail settings changed.
Templates exist in Korean and English; other languages use English. Mails contain
no links.

## Lost authenticator

Recovery always needs local verification on this computer, plus a second, separate
proof:

- **Mail set up:** a 6-digit code mailed to the owner address, valid 10 minutes,
  single use, 5 attempts.
- **Any time:** one of the backup codes. Without mail this is the only path, so
  keep the backup codes somewhere safe.

On a computer with no OS verifier, the local verification itself can be a backup
code, for recovery only. The second proof is then the mailed code or another backup
code, so keep at least two.

The proof allows one thing, within 10 minutes: by default a new authenticator. It
ends when that is done, and after 5 wrong codes for the new authenticator. Where no
OS prompt exists, the proof also stands in for that step's verification, since the
lost authenticator was the only one; where one exists, it still asks.

```sh
vca security recover                 # mailed code
vca security recover --backup-code   # backup code
```

Windows Hello alone cannot replace an authenticator: it also needs a code from
the current authenticator, or recovery.

## Pairing a phone

The phone shows a confirm code for the link it got, and the computer shows one next
to the QR code. It has 16 characters in four groups (`XXXX-XXXX-XXXX-XXXX`, 80 bits);
compare all of them. They come from the server's key, so a forged link shows a
different code. Pair only if both are the same, and if they differ do not enter any
code. A link that did not come from the app's own QR scanner never pairs by itself
when the last digit is typed: it needs a tap on Pair.

## Engine keys

A stored key goes only to the address it was saved with, under the same names.
Sending it to a new address, or naming another environment variable for it, needs
local verification, and a test of an unsaved draft never uses a stored key. Engine
and relay requests do not follow a redirect to another host.

A key travels over https. Plain http is allowed only:

- to this computer;
- to the Tailscale range, `100.64.0.0/10`. That traffic is encrypted by the tailnet.
  The same range is also used by some internet providers for carrier-grade NAT, so
  use it only for addresses you know are yours;
- to a private network address (`10/8`, `172.16/12`, `192.168/16`, link-local, IPv6
  ULA), when the engine has **Allow plain http on my private network** turned on.
  Turning it on needs local verification. Anyone on that network can read the key.

Plain http to a public address or to a host name is refused, because a name says
nothing about where it leads. The rule also holds when a key is sent, so an engine
saved earlier fails with a clear error until you change it. In `config.toml` the
setting is `allow_plain_http = true`; a program running as the owner can edit that
file, as with everything else here.

## Agents and permission prompts

While the agent waits for a decision, text that goes out on its own (voice, the
conductor, a task result, `vca send`, also without Enter) is refused, so it cannot
answer the prompt, and it is checked again just before Enter is pressed. If a prompt
opened in between, the text stays in the agent's input line and Enter is not pressed.
Text and keys a person types in the app can reach the prompt, as in a terminal; `vca
send --at-prompt` says the same. A decision the agent has not put on screen yet
(a call in a mode that settles calls on its own) refuses nothing.

This guard stops accidents, such as a dictation landing on a prompt. It does not stop
a hostile local program, which can claim the same thing or type into the terminal.

A spoken approval only works for short commands on the server's list of read-only
programs, taken from the agent's hook; anything else goes to the screen. `git status`
and `git diff` are not on it: they can run what a repository's own config names.
