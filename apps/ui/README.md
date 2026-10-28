# VCA UI (`apps/ui`)

Shared front-end for Voice Command AI. It will be embedded in the Tauri 2 desktop
and mobile apps; for now it runs as a standalone web app against a simulated
server (`MockTransport`).

Stack: Vite, React 19, TypeScript (strict), pnpm, CSS modules with CSS-variable
themes, xterm.js for the raw terminal, `@msgpack/msgpack` for the wire codec,
Vitest + Testing Library for tests.

## Scripts

Run from `apps/ui` (Node 20.19+ and pnpm 10).

| Command                             | What it does                                                         |
| ----------------------------------- | -------------------------------------------------------------------- |
| `pnpm install`                      | Install dependencies                                                 |
| `pnpm dev`                          | Dev server at http://localhost:5173 (mock servers)                   |
| `pnpm build`                        | Type-check and build to `dist/` (relative base, embeddable by Tauri) |
| `pnpm preview`                      | Serve the production build                                           |
| `pnpm typecheck`                    | `tsc -b --noEmit`                                                    |
| `pnpm lint`                         | ESLint (typescript-eslint, react-hooks, prettier-compatible)         |
| `pnpm format` / `pnpm format:check` | Prettier                                                             |
| `pnpm test` / `pnpm test:watch`     | Vitest (jsdom)                                                       |
| `pnpm check`                        | All of the above, as CI would run them                               |

## Layout

```
src/
  protocol/   TypeScript mirror of vca-core (types.ts), proposed additions
              (proposed.ts), MessagePack/JSON codec, Transport interface,
              MockTransport + scenario data
  state/      reducer (pure), alert derivation, settings, store/provider
  i18n/       en.json (source), ja/ko/es stubs, tiny translate()/formatters
  speech/     transcribe() contract + mock, MediaRecorder push-to-talk, TTS
  components/ deck, cards, live view (chat + xterm), composer, decision sheet,
              resources, new-session dialog, settings, pairing, toasts
```

## Protocol mirror

`src/protocol/types.ts` mirrors `crates/vca-core/src/{protocol,session,event,resource,error}.rs`:
serde internal `type` tags in snake_case, `Option<T>` as `T | null`,
`AgentKind` as `{ type, name? }` (`tag = "type", content = "name"`), and
`Output.data` as `Uint8Array` (MessagePack `bin`; a number array in JSON).
`codec.test.ts` checks the decoder against bytes produced by `rmp_serde::to_vec_named`
and the encoder against `serde_json` output of vca-core. Regenerate the fixtures
with a small throwaway binary that depends on `vca-core` when the protocol changes.

Messages the UI needs that vca-core does not define yet (cloud launch, paired
devices, kill switch) live in `src/protocol/proposed.ts` and are only
implemented by the mock.

## Transport

```ts
interface Transport {
  readonly state: ConnectionState; // disconnected | connecting | connected | reconnecting
  connect(): Promise<void>;
  disconnect(): void;
  send(envelope: Envelope<WireClientMsg>): Promise<WireServerMsg>; // response with the same id
  onMessage(listener): Unsubscribe; // server pushes (id = 0)
  onStateChange(listener): Unsubscribe;
}
```

The store sends `hello`, `list_sessions`, then `subscribe` for every session.
After a `reconnecting → connected` transition it resubscribes with
`from_seq = last seen seq`; a `stream_gap` clears the view and shows a notice.
A Tauri implementation only has to move envelopes (MessagePack via `codec.ts`)
over the Noise channel.

`main.tsx` wires two simulated machines (`workstation`, `build-server`). The
Settings screen has developer buttons to simulate a connection drop, with or
without loss of buffered history.

## Speech

`TranscribeFn(blob, { engine, locale, sessionId })` is injected through
`SpeechProvider`. The mock returns canned transcripts with vocabulary
corrections, which the composer highlights. The mic button is push-to-talk
(pointer or Space/Enter held); the microphone is released on stop and when the
page is hidden. In the mock build, a missing microphone falls back to a demo
transcript.

## Accessibility and i18n

Mobile-first (tested at 360 px and 390 px) with a desktop split view from
900 px. Light and dark themes via CSS variables (`data-theme` override or system
preference), visible focus rings, labelled controls, `role="log"` / `status` /
`alert` regions, and state chips that pair color with text. All strings come
from `src/i18n/en.json`; other locales fall back to English per key.
