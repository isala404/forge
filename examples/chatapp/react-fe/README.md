# react-fe

The shared React SPA for the chatapp example. One client, three interchangeable
GraphQL backends (rust-be, node-be, python-be). It talks to whichever backend the
env points it at, because all three serve the one canonical `../schema.graphql`.

## Stack

- Vite + React 19 + TypeScript
- [urql](https://commerce.nearform.com/open-source/urql/) with the normalized
  `@urql/exchange-graphcache`, `@urql/exchange-auth`, and a `graphql-ws`
  subscription socket
- `@graphql-codegen/cli` with the client preset generating typed `graphql()`
  documents from `../schema.graphql`
- Phosphor icons, self-hosted Geist variable fonts

## How the GraphQL layer is wired

Every operation is a typed generated document built from named fragments. There
are no hand-concatenated query strings anywhere.

- All fragments and operations live in `src/graphql/operations.ts`, written with
  the typed `graphql()` tag from `src/gql` (the codegen client preset output).
- `bun run codegen` reads `codegen.ts`, collects the `graphql()` documents under
  `src/`, validates them against `../schema.graphql`, and regenerates `src/gql/`.
- Components select fields through fragments and unmask them with the generated
  fragment helper (re-exported as `readFragment` in `src/lib/fragments.ts`, since
  the helper is a pure cast, not a React hook). Derivations live in
  `src/lib/derive.ts`.

### Normalized cache

`User`, `Chat`, and `Message` are entities keyed by `id` (`src/lib/cache.ts`), so
a message that first arrives over a subscription and the same message inside a
query share one cache record. `Receipt` has no id (it is identified by message +
user) and resolves inline on its parent message; value types like
`SessionPayload`, `OpsStats`, and `UploadTicket` are not normalized.

### Live subscriptions

`graphql-transport-ws` powers `messageAdded`, `typing`, `receiptChanged`, and
`presenceChanged`. The conversation hook (`src/hooks/useConversation.ts`) folds
new messages and receipt changes into the live window; presence updates
(`src/hooks/usePresence.ts`) flow straight through the normalized cache and light
up every `online` dot at once.

### Bearer auth

The session token is held in memory plus `localStorage` (`src/lib/token.ts`).
`authExchange` adds `Authorization: Bearer <token>` to every HTTP request; the
`graphql-ws` client sends the same token in `connectionParams` as
`{ authorization: "Bearer <token>" }`. A token change disposes the socket so the
next subscription reconnects with the new principal. An `UNAUTHENTICATED` error
clears the session and drops back to the login screen.

## Features

Login / signup, chat-list sidebar with unread badges and last-message previews,
the live conversation view (messages, typing indicator, presence dots, read
receipts), attachment upload (requestUpload -> direct PUT to the presigned URL ->
sendMessage with the media key), group creation and add-member, the disappearing
-message toggle, and a settings view (mint API key, reactions rollout flag, ops
gauges for online count and DLQ depth, trigger a failing job, sign out
everywhere). Loading, empty, and error states are handled throughout. Light and
dark themes follow `prefers-color-scheme`.

## Configuration

Backend endpoints come from env, defaulting to node-be:

| Var                 | Default                         |
| ------------------- | ------------------------------- |
| `VITE_GRAPHQL_HTTP` | `http://localhost:8082/graphql` |
| `VITE_GRAPHQL_WS`   | `ws://localhost:8082/graphql`   |

For local dev, copy `.env.example` to `.env` and point it at the backend you are
running (rust-be 8081, node-be 8082, python-be 8083). In the Docker image the
URLs are injected at container start into `env.js`, so one prebuilt image targets
any backend without rebuilding.

## Commands

```bash
bun install
bun run codegen     # regenerate src/gql from ../schema.graphql
bun run dev         # vite dev server on :5173
bun run build       # tsc -b + vite build
bun run lint        # eslint
bun run test:e2e    # against docker compose stacks from ..
```

(`npm` works too; `bun` is the project default.)

On OrbStack, bring the stacks up from `examples/chatapp` with
`docker compose --env-file .env.orb up --build`, then run
`bun run test:e2e --config=playwright.orb.config.ts` from this directory.

## Docker

`docker build -t react-fe .` produces an nginx image serving the static bundle on
port 80. Each backend's `docker-compose.yml` builds this directory and sets
`VITE_GRAPHQL_HTTP` / `VITE_GRAPHQL_WS` to that backend's published URL.
