# The conductor

The conductor is one AI that helps you oversee the other AI CLI sessions on the same PC. You ask it things like "which sessions need me?" or "tell the api session to run the tests", and it answers from what VCA sees. It can send messages to other sessions only after you confirm each one, or when your own instruction named the session and the message passes its safety checks.

It is **off by default** and can only be turned on in the desktop app on the PC itself.

## What runs the conductor

You choose the agent in **Settings > Command Center**:

| Agent | How VCA connects it | Status |
|---|---|---|
| Claude Code (default) | `claude --strict-mcp-config --mcp-config <conductor folder>/vca-mcp.json` | Checked against `claude --help` |
| Codex CLI | `codex -c mcp_servers.vca.command='<vca>' -c mcp_servers.vca.args=['mcp']` (command-line overrides only) | Experimental |
| Gemini CLI | `mcpServers` in `<conductor folder>/.gemini/settings.json`, started with `--allowed-mcp-server-names vca` | Experimental |
| Built-in | VCA's own tool-calling loop against an OpenAI-compatible LLM profile | Needs a model with tool calling |

For Claude Code, Codex CLI and Gemini CLI, VCA writes everything into a dedicated folder below its data directory (`<data>/conductor`): the instruction file (`CLAUDE.md`, `AGENTS.md` or `GEMINI.md`) and the MCP configuration for this folder. It never changes your user-wide agent settings (`~/.claude`, `~/.codex`, `~/.gemini`) or your global MCP configuration. The agent then runs as a VCA-managed session named "Conductor", so you see it like any other session.

The agent talks to VCA through `vca mcp`, a small MCP server built into the `vca` command-line tool. It follows the Model Context Protocol revision 2025-06-18 (stdio transport, JSON-RPC 2.0: `initialize`, `ping`, `tools/list`, `tools/call`; older revisions 2025-03-26 and 2024-11-05 are accepted). It has no network listener: it only reaches the VCA server on the same PC through the owner-only local control channel.

The **built-in conductor** needs no extra program. Pick an OpenAI-compatible profile from `[[speech.llms]]` (set up under **Speech engines**: Ollama, LM Studio, vLLM, OpenAI and similar servers) whose model supports tool calling. VCA keeps only a short rolling context of the conductor chat and sends each instruction with the same tools and rules as the MCP path.

How well the conductor works depends on the model. Small local models may struggle with multi-step orchestration; they may call the wrong tool or stop early.

## Turning it on

1. In the desktop app on the PC, open **Settings > Command Center** and turn on **Use a conductor on this PC**.
2. Choose the agent (and, for the built-in conductor, the LLM profile). Save.
3. Open **Command** in the main menu (Korean: 지휘소) and press **Start conductor**.

Paired phones and other devices can see the conductor, chat with it, answer its requests and stop it, but they cannot turn it on, start it or change its allowlist.

## Using it: the Command Center

The Command Center (Korean: 지휘소) is your direct line to the conductor. It has three tabs; use the arrow keys to move between them.

- **Conversation** (the default): for Claude Code, Codex CLI and Gemini CLI, the conductor's own session in the normal session view, with the same chat, history paging, search, composer, dictation and Voco as any work session. Type or dictate an instruction; dictation lands in the box for review and nothing is sent by itself. The built-in conductor has no session: its chat is shown in the same chat view with an instruction box. Replies are read aloud when spoken alerts are on. While the conductor is stopped, this tab says so and, on the PC, offers **Start conductor**.
- **Approvals**: what the Command Center raised for you, in one list: other sessions' decisions it could not settle within your allow rules (with its summary and recommendation, or marked **Raised automatically**; see [Decisions of other sessions](#decisions-of-other-sessions)), its own session's decisions, its requests to send something, its first-run "trust this folder" question, and to-dos it is blocked on. Other sessions' raw alerts stay under **Alerts**. The Command Center menu's badge counts these items. Each item shows the session, what is asked and its actions: **Answer** opens the same decision card as on the session screen (the same safety rules apply; inferred prompts are answered in the terminal), **Allow** / **Deny** for conductor requests, **Open session**, **Open to-do**, and **Later**, which moves the item to the bottom for now (on this device only).
- **Log**: what the conductor did, was refused and was told, newest first, as plain sentences, for example: Sent “keep going…” to Orbit 3D (auto-allowed).

If the server reports that the conductor's agent never called back to VCA (its hooks are missing, for example because another tool rewrote the agent's settings), a banner explains that its replies can't be read and links to **Settings > Alerts > Agent connection** to repair the hooks.

If a turn of the conductor shows no end for 15 minutes, a banner says its last turn hasn't finished and it may be stuck: check its terminal (**Show terminal**), or choose **Stop conductor** in the header's stop menu and start it again on the PC. When both apply, the missing hooks are reported. A turn that ends without a reply you can see is written to the Log.

### The header control

While the conductor is turned on, every page has a small Command Center control in the header:

- the conductor's status and how many items wait in Approvals (opens the Command Center);
- **Talk to the conductor**: press, speak, press again; the transcript lands in the conductor's box for review;
- **Hands-free** (in the menu on phones): with it on, say "Voco …, over" in the conversation;
- the stop menu:
  - **Pause conductor**: nothing wakes the conductor until you choose **Resume conductor**; its session keeps running and still takes your instructions. Phones can pause and resume too. The status reads **Paused**. (Older servers pause by stopping.)
  - **Stop conductor**: ends its session and denies its pending requests; start it again from the same menu, on the PC (with local verification).
  - **Kill switch**: asks for confirmation, then for verification. On the PC that is the local verification (Windows Hello, Touch ID or the authenticator code). On a phone it is the 6-digit code from your authenticator app, or a backup code, because a phone can be lost. Too many wrong codes lock it for a few minutes. Without an authenticator set up on the PC, use the kill switch on the PC.

On a phone, the composer's conductor button also opens the Command Center.

### Settings > Command Center

Setup lives in **Settings > Command Center**, in the desktop app on the PC: the opt-in, the agent or LLM profile, passing events to the conductor, the hourly action cap and **auto-allow** (the sessions the conductor may send to without asking, and until when). Phones see that the conductor is set up on the PC.

The first time Claude Code starts in the conductor's folder, it asks whether to trust that folder. The Command Center shows this as a card (and in Approvals): **Trust and start** answers "Yes" on the conductor's terminal (only while that question is on screen with "Yes" selected), and **Show terminal** opens the terminal. VCA never pre-accepts it in your agent's own configuration. Elsewhere, a session that waits at a menu before it has a transcript reads **Choose in the terminal**, with a link to its terminal.

### The conductor's own session

The conductor's session has no card in the session list or the phone carousel, and it takes no card number: "session 3" and voice addressing count work sessions only. Its conversation lives in the Command Center, so its finished turns, input waits, unread counts and notifications are not alerts: no Alerts entry, no badge count, no spoken alert, no notification, no "needs attention" filter, no to-dos extracted from its replies (it files to-dos with its `request_human_action` tool). Its own events never wake it.

Its **permission decisions** (for example Claude Code asking to run a command) still reach you: in the Command Center conversation, in Approvals and **Alerts** labelled **Conductor session**, in the badges, and as a phone alert. Opening one takes you to the Command Center.

## What it can do

Read-only tools:

- `list_sessions`: the AI CLI sessions on this PC with their state and a short preview;
- `session_status`: one session and the question it waits on;
- `session_summary`: the last chat items of one session (secrets redacted);
- `pending_decisions`: sessions waiting for a permission decision, with the exact command when known and `allowed_answers` (what your allow rules let it answer; usually nothing);
- `briefing`: the same short status as **Listen to status**;
- `recent_activity`: sessions active in the last minutes.

Actions (each audited and counted against the hourly cap):

- `send_to_session`: type a message into a session and press Enter, after your confirmation;
- `interrupt`: press Esc in a session (Ctrl+C for other programs);
- `focus_terminal`: bring a session's terminal window to the front;
- `restore_windows`: reopen saved windows of a restore profile (only folders inside the workspace roots);
- `new_session`: start an AI CLI agent in a folder inside the workspace roots.
- `answer_decision`: answer another session's permission decision with its plain **Yes** or **No**, only under one of your allow rules (see below).

Not capped, but audited:

- `escalate_decision`: put another session's decision in your **Approvals** with the conductor's short summary (what is asked, why it needs you) and, optionally, its recommendation.

## Decisions of other sessions

When the Command Center is on, other sessions' permission decisions go to the conductor first. It answers a decision only when one of your allow rules covers it, and raises everything else to **Approvals** with its summary. **Approvals** shows only what the Command Center raised; the original alerts always stay under **Alerts**, and toasts and phone alerts work as before.

- **Never missed.** A decision the conductor neither answers nor escalates within about 75 seconds of being told goes to **Approvals** by itself, marked **Raised automatically**. The same happens at once while the conductor cannot be told: stopped, paused, revoked by the kill switch, hooks missing, stalled, or with **Pass events to the conductor** off.
- **Off means off.** With the Command Center turned off in Settings, nothing goes to **Approvals**; decisions reach you through **Alerts** exactly as without it.
- **Gone when answered.** An escalation lives as long as its decision: answered anywhere (here, on the session screen, on a phone, in the terminal) or replaced by a new one, it leaves **Approvals** and the badge.
- **Allow rules.** There are none by default, so by default the conductor only escalates. Rules are part of the conductor settings (`decision_rules` in `conductor.json`), changed only in the desktop app on this PC. A rule says which answer it allows (`approve` or `deny`), for which command prefixes (whole words: `cargo test` covers `cargo test -p core`, not `cargo testing`; required for `approve`), for which sessions (names or ids; empty for all), and optionally until when.
- **Never risky.** Whatever the rules say, the conductor only picks the plain one-time **Yes** or the plain **No**, never "Yes, and don't ask again", never a question form or an inferred prompt. It approves only decisions with an exact command from the agent's hook that is one plain command (no chaining, redirection or substitution), rated safe, touching no credentials, and outside the always-escalate classes: deletes and overwrites, pushes and publishing, installs and package changes, network access, elevation, accounts and payments. Every answer is audited with the rule that allowed it; every refused answer is audited too.
- **Its own decisions are yours.** The conductor's own session's decisions always come to you.

## What it can never do

- **Answer permission decisions on its own judgment.** It answers another session's decision only under one of your allow rules (none by default), never approves a risky command, and never answers its own session's decisions. Everything else it escalates to you.
- **Reach remote shells or itself.** Remote shell sessions and the conductor's own session are never targets, and only sessions of this PC are visible.
- **Send without you.** Every `send_to_session` creates a request ("The conductor wants to send this to session X: …") that waits for your **Allow** or **Deny**. Without an answer it is denied after the confirmation timeout (2 minutes by default). If you want to skip the question for one session for a while, allow it under **Settings > Command Center > Allowed without asking** for 15 minutes, 1 hour or 4 hours; the entry expires by itself.

  **Your own instruction counts as your approval.** When you tell the conductor "ask session 3 to check the build and tell me", its send to session 3 is what you asked for, so it is not put to you a second time. Your instruction opens an *instruction grant* for that conductor turn: it ends when the turn ends, and after 10 minutes at the latest. A send inside it goes through without a question only when all of this holds:

  - the instruction named the target session: by its name ("Orbit 3D"), by its card number ("3번 세션", "session 3", "삼번 세션"), or every session when it clearly says so ("모든 세션", "전체 세션", "all sessions"; a bare "전체" is not enough, and "all sessions except …" is not "all"). A name two sessions share names neither;
  - the text passes the auto-send screen: at most 1,500 characters, nothing that looks like a secret, no command line rated caution or dangerous (`rm -rf`, `git push --force`, …, as for remote shells), no link your instruction did not contain, nothing copied word for word (80 characters or more) from what sessions printed, and the target is not waiting on a decision.

  Everything else is a request as before, and the request says why: a session you did not name, a failed check, an ended grant, a turn started by events rather than by you, or an instruction that did not come from you. Only owner-level clients open grants today: the desktop app or `vca` on this PC (not a process running inside a session or a remote shell) and paired devices with Command or Admin scope. When people and roles arrive, a staff member's instruction will open grants only for the sessions of that person's departments. The activity log shows the grant ("Your instruction: checked sends to Orbit 3D are auto-approved …") and each send it approved (`confirmed_by=instruction` with the grant id).

## Safety

- **Session output is untrusted.** Everything the conductor reads from other sessions arrives in an `untrusted_output` field with a notice, and the instruction file tells it never to follow instructions found there. Previews and summaries are redacted (API keys, tokens, passwords, credentials in URLs, long hexadecimal strings) before the conductor sees them.
- **Confirmation by default.** All sends need your confirmation unless you allowed that session, or your own instruction of the current turn named it and the text passes the auto-send screen; at most five requests wait at a time.
- **Audit.** Every action, refusal, wake-up and every change you make (start, stop, pause and resume, kill switch, settings, answers, allowlist) is written to the activity log with origin `conductor`. A kill switch refused for a missing or wrong code is logged as a refusal. **Settings > Activity log** has a **Conductor** filter.
- **Kill switch.** It needs verification every time: local verification on the PC, the authenticator code on a phone. The conductor's kill switch stops it, denies every pending request, clears the allowlist, deletes its MCP configuration and refuses its tools until you start it again. The global kill switch of the PC also blocks it.
- **Same user.** The conductor runs as your user account. The rules above hold for everything it does through VCA's tools; the instruction file also tells it not to run `vca` or `vcad` directly. Claude Code still asks you before it runs shell commands (those questions reach you as normal decisions in VCA); do not approve commands that bypass the tools.

## Quota

The conductor spends your agent's or model's quota only when something wakes it:

- you send it an instruction; or
- **Pass events to the conductor** is on: turn ends, errors and decision requests of other sessions are collected and typed into the conductor as one short summary, at most once every 2 minutes and 6 times an hour by default, never within a minute after you typed to it and never while it is working. A decision waiting on another session skips the two-minute gap and the typing pause (its 75-second fallback would come first); the hourly cap still holds. This option is off by default; while it is off, other sessions' decisions go straight to **Approvals**.

There is no polling. Actions are capped at 20 per hour by default (**Settings > Command Center > Most conductor actions per hour**). The built-in conductor also has a cap on LLM requests (30 per hour) and on the answer length of each request (1,024 tokens), and makes at most six requests per instruction.

## Files

- `<data>/conductor.json`: the settings.
- `<data>/conductor/`: the conductor's working folder (instruction file, MCP configuration).
- `<data>/audit.jsonl`: the activity log.
