# Rustket

Native Rocket.Chat desktop client in Rust + iced 0.14. No Electron, no embedded web
view: browser-based flows (OAuth/OpenID/SAML/CAS, video calls) use the default
external browser and return via the `rocketchat://` scheme. Full plan and phase
breakdown: `docs/PLAN.md`. Protocol notes: `docs/protocol.md`.

## Toolchain and commands

Rust is installed per-user and is not on the default PATH:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Live integration tests (need the dev server running):

```sh
docker compose -f dev/docker-compose.yml up -d     # Rocket.Chat 8.9.1 on http://localhost:3000
dev/seed.sh                                         # idempotent: test users and rooms
RUSTKET_TEST_SERVER=http://localhost:3000 cargo test -p rocketchat --test live -- --test-threads=1
```

Dev accounts: `admin` / `adminpass` (admin), `alice` / `alicepass`, `bob` / `bobpass`,
`carol` / `carolpass`. Mailpit (emails, e.g. 2FA codes): http://localhost:8025.
MongoDB is 7.0 because 8.0 refuses to start on Linux kernel ≥ 6.19.
Live tests must create their own uniquely named rooms/messages and never depend on
state left by other tests.

## Workspace

- `crates/rocketchat` — protocol only (models, REST, DDP, auth, TLS). No UI, no persistence.
- `crates/rustket-core` — UI-agnostic state, sync, reducers, accounts, secrets.
- `crates/rustket` — iced binary (screens, widgets, theme, i18n, desktop integration).

## Reference sources (read these instead of guessing APIs)

`.ref/` is gitignored and contains:
- `.ref/iced` — iced 0.14.0 sources with `examples/` (the API changed a lot between
  versions; always check the 0.14 sources/examples before using an iced API).
- `.ref/rocketchat` — Rocket.Chat 8.9.1, sparse: `packages/rest-typings` (every REST
  endpoint's params/response), `packages/core-typings` (IMessage, IRoom,
  ISubscription, IUser, …), `packages/ddp-client`, `packages/message-parser`
  (the `md` AST), `packages/ui-kit`, `apps/meteor/app/api/server` (REST handlers),
  e2ee client code, oauth/saml server code.
- `.ref/rn` — Rocket.Chat.ReactNative (`app/lib/encryption`, `app/lib/methods`,
  `app/lib/services`, login services, sagas) — a working native client to compare with.
- The running dev server is the source of truth: when in doubt, call the endpoint
  with curl and look at the real JSON.

## Conventions

- Code, comments, identifiers, commit messages: English. UI strings go through the
  i18n layer (English + Russian).
- Pure-Rust dependencies only: no crates that need system `-dev` packages
  (no openssl, gtk, alsa, dbus C libs). TLS is rustls everywhere; reuse the shared
  `ClientConfig` from `rocketchat::tls`.
- Libraries use `thiserror` error enums; no `unwrap()`/`expect()` outside tests except
  for documented invariants. No `unsafe`.
- `tracing` for logs. Never log passwords, auth tokens, credential secrets, E2E keys
  or message bodies at info level or above.
- Secrets (auth tokens) only through the secret store abstraction in `rustket-core`.
- Serde models must be tolerant: unknown fields ignored, almost everything optional
  or `#[serde(default)]`; Rocket.Chat payloads vary between versions and between REST
  (ISO date strings) and DDP (`{"$date": ms}`) — use `rocketchat::RcDate`.
- Tests: unit tests next to code; JSON fixtures captured from the real dev server go
  to `crates/<crate>/tests/fixtures/`; live tests in `crates/rocketchat/tests/live.rs`
  skip themselves when `RUSTKET_TEST_SERVER` is unset.
- Keep comments sparse and factual; match the surrounding style.
- Subagents do not commit; the orchestrator commits after review.
