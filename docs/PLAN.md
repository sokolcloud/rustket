# Rustket — native Rocket.Chat desktop client

Goal: a full-featured Rocket.Chat desktop client written in Rust, without Electron
or any embedded web view. Anything that needs a browser (OAuth, OpenID Connect,
SAML, CAS, video conferences, external links) is opened in the user's default
browser and handed back to the app through the `rocketchat://` URL scheme.

Target server: Rocket.Chat 8.x (developed against 8.9.1). Older 7.x servers should
work where the API is compatible, but they are not a goal.

## Technology

| Concern | Choice | Notes |
|---|---|---|
| UI | `iced` 0.14 (wgpu + tiny-skia fallback) | Elm architecture, tokio executor, IME, `text_editor`, `rich_text`, images, SVG, QR codes |
| Async | `tokio` | One runtime owned by iced (`tokio` feature) |
| HTTP | `reqwest` 0.13 + rustls (aws-lc-rs) | Shared rustls `ClientConfig` with the WebSocket |
| Realtime | `tokio-tungstenite` + rustls | Meteor DDP over `wss://host/websocket` |
| TLS trust | `rustls-platform-verifier` + per-server pinned certificates | Self-signed servers need explicit user confirmation |
| Secrets | `keyring` 4 (Secret Service over zbus) | Fallback: 0600 file in the config dir, with a warning |
| Notifications | `notify-rust` (zbus) | Freedesktop notifications with actions |
| Tray | `ksni` (StatusNotifierItem) | KDE/GNOME (with extension)/XFCE |
| File dialogs | `rfd` (xdg-desktop-portal) | |
| Clipboard | iced clipboard for text, `arboard` for images | |
| Browser | `webbrowser` crate (`xdg-open`) | |
| Local cache | `rusqlite` (bundled SQLite), later phase | |

Constraint: pure-Rust stack, no system `-dev` packages (no GTK, no OpenSSL, no ALSA
headers). The machine has no passwordless sudo.

## Workspace layout

```
crates/
  rocketchat/     protocol crate, no UI: models, REST, DDP, auth flows, TLS
  rustket-core/   UI-agnostic application state: accounts, secrets, sync engine,
                  per-workspace stores, reducers, permissions, emoji tables
  rustket/        iced binary: screens, widgets, theme, i18n, desktop integration
dev/              docker compose stack (Rocket.Chat + MongoDB + Mailpit + Keycloak),
                  seed.sh, setup-oauth.sh, oauth_probe.py
docs/             this plan, protocol notes
.ref/             (gitignored) reference sources: iced 0.14 with examples,
                  Rocket.Chat 8.9.1 typings/server code, Rocket.Chat.ReactNative
```

### Layering rules

- `rocketchat` knows nothing about the UI or persistence. Every server interaction
  lives here and is covered by unit tests on captured JSON fixtures plus live
  integration tests against the dev server.
- `rustket-core` turns protocol events into application state. All reducers are
  plain functions on plain structs so they are unit-testable without iced.
- `rustket` only maps state to widgets and user input to core commands. The
  bridge between a running `Session` and iced is an `iced::Subscription` that
  yields `SessionEvent`s, plus `Task`s for one-shot requests.

## Authentication design (external browser)

Verified against the Rocket.Chat 8.9.1 server bundle:

1. **Password / LDAP / 2FA** — native forms. `POST /api/v1/login`
   `{user, password}`; on `totp-required` / email 2FA errors re-send with `code`
   (email code requested with `users.2fa.sendEmailCode`). Resume with
   `{resume: token}` over DDP and REST.
2. **OAuth / OpenID (legacy Meteor flow, default: `Accounts_OAuth_Use_Modern_Flow=false`)** —
   the client builds the provider authorize URL (from `settings.oauth` /
   `service.configurations`) with `redirect_uri = {server}/_oauth/{service}` and
   `state = base64url({"loginStyle":"redirect","credentialToken":<random 43>,
   "isCordova":true,"redirectUrl":"rocketchat://auth"})`. The server special-cases
   `redirectUrl` starting with `rocketchat://auth` and redirects the browser to
   `rocketchat://auth?host=..&type=oauth&credentialToken=..&credentialSecret=..`.
   The app then logs in with `{oauth: {credentialToken, credentialSecret}}`.
   Verified end-to-end against the dev Keycloak (`dev/oauth_probe.py`): the server
   answers `/_oauth/{service}` with an HTML page whose script navigates to the deep
   link (no HTTP 302), and `host` carries a trailing slash.
3. **OAuth (modern passport flow, `Accounts_OAuth_Use_Modern_Flow=true`)** — open
   `{server}/oauth/{service}?loginClient=desktop`. The server and its web page end
   with a redirect to `rocketchat://auth?host=..&token=<loginToken>&userId=..`;
   2FA is handled by the server's web pages.
4. **SAML** — open `{server}/_saml/authorize/{provider}/{credentialToken}?loginClient=desktop`.
   Modern flow redirects to `rocketchat://auth?type=saml&host=..&credentialToken=..`;
   for the legacy flow the app polls `login {saml: true, credentialToken}`.
5. **CAS** — open the CAS login URL with `service={server}/_cas/{credentialToken}`
   and poll `login {cas: {credentialToken}}`.
6. **"Sign in with browser"** (universal fallback, any auth method the web UI
   supports) — open `{server}/home?loginClient=desktop`; once the web session is
   logged in, the web app redirects to `rocketchat://auth?host=..&token=..&userId=..`.
7. **Personal access token** — user id + token entered manually.

`rocketchat://` handling: the binary is single-instance. A second invocation with a
URL argument forwards it over a Unix socket in `$XDG_RUNTIME_DIR` and exits. The app
offers to register itself as `x-scheme-handler/rocketchat` (writes a `.desktop` file
into `~/.local/share/applications` and runs `xdg-mime`), warning that this takes the
scheme over from the official desktop client. An auth deep link is accepted
silently only if it matches a pending login started by the app (same host, same
`credentialToken`, not expired); anything else requires explicit confirmation.
Fallback when the scheme is not registered: a field to paste the `rocketchat://auth…`
link from the browser.

## Phases

Every phase ends with: `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings`,
`cargo test --workspace`, live integration tests against the dev server, an xhigh
adversarial review, fixes, and a commit.

### Phase 0 — Bootstrap (done by the orchestrator)
Toolchain, workspace, dev docker stack, reference sources, `CLAUDE.md`, seed script.

### Phase 1 — Protocol crate `rocketchat`
- Models with tolerant serde (`#[serde(default)]`, `Option`, unknown fields kept
  where useful): `User`, `Room`, `Subscription`, `Message` (incl. `md` AST,
  attachments, reactions, mentions, channels, urls/link previews, files, threads
  `tmid/tcount/tlm/replies`, discussions `drid/dcount`, pins, stars, edits,
  system type `t`, `blocks`, e2e fields), `ServerInfo`, `Setting`,
  `LoginServiceConfig`, `Permission`, `Role`, `CustomEmoji`, `CustomUserStatus`,
  `SlashCommand`, `Team`, `VideoConference`.
- `RcDate`: accepts ISO strings (REST) and `{"$date": ms}` (DDP).
- TLS module: one rustls `ClientConfig` (platform verifier + pinned extra certs).
- REST client: auth headers, typed endpoint methods, error mapping (`success:false`,
  `status:error`, `errorType`, 2FA details), multipart upload with progress
  (`rooms.media` + `rooms.mediaConfirm`), authenticated file download.
- DDP client: connect handshake, ping/pong both ways, method calls with results,
  subscriptions with routed `changed` events, reconnect with backoff + jitter,
  re-login with resume token and re-subscribe after reconnect, connection state
  events.
- `Session`: REST + DDP for one account; typed `SessionEvent` stream (room
  messages, deletes, subscription/room changes, typing/user-activity, presence,
  notifications, settings/permissions/roles changes, emoji changes,
  `uiInteraction`, `video-conference`, `force_logout`).
- Auth flows from the section above (URL building, state encoding, deep-link
  parsing, polling), unit-tested; password + 2FA + resume + PAT live-tested.

### Phase 2 — Core state `rustket-core`
- Config (servers list, UI prefs) as TOML/JSON in XDG config dir; secrets via keyring.
- Multi-workspace model; per-workspace store: rooms+subscriptions merged view,
  room list sorting (activity / alphabetical) and grouping (unread, favorites,
  teams, discussions, channels, private groups, DMs, omnichannel), unread and
  mention counters, presence cache, typing state with expiry, drafts.
- Timeline per room: ordering by `ts`, dedup by `_id`, edits, deletes, pagination
  cursors, thread reply aggregation, temporary (pending/failed) local messages.
- Sync engine: initial load (`subscriptions.get`, `rooms.get`, `permissions.listAll`,
  `settings.public`, `emoji-custom.list`, `custom-user-status.list`), delta sync via
  `updatedSince` after reconnects, realtime event application.
- Permission checks (`has_permission(perm, rid)` with global + room roles).
- Emoji tables: Unicode shortcodes (`emojis` crate) + custom emoji registry.

### Phase 3 — App shell (iced)
- Window, theme (light/dark, RC-like palette), i18n (en, ru).
- Add-server screen (URL normalisation, `/api/info` check, certificate prompt).
- Login screen: form login, 2FA dialog, login-service buttons, "sign in with
  browser", PAT; register-URL-handler prompt; deep-link handling.
- Workspace rail (server icons, unread badges), sidebar (room list with sections,
  filter, avatars, presence dots, unread/mention badges), room header, timeline,
  composer; send message; realtime updates; mark as read; typing indicators;
  connection banner (connecting / offline / reconnecting).

### Phase 4 — Rich message rendering
`md` AST rendering (bold/italic/strike/inline code/code blocks/links/mentions/
channels/emoji/big emoji/quotes/lists/tasks/headings/spoilers/KaTeX as code),
fallback parser for messages without `md`, system messages, attachments (image
thumbnails with lazy load + disk cache, files with download/open, audio/video via
external player), message attachments/quotes, link previews, reactions, custom emoji,
avatars/aliases/bot emoji avatars, date separators, unread divider, grouping,
edited/pinned/starred markers, thread and discussion summaries, read receipts.

### Phase 5 — Composer and message actions
Multi-line editor (Enter/Shift+Enter), autocomplete popups for `@`, `#`, `:`, `/`,
emoji picker (categories, search, recent, custom), uploads (portal picker, drag and
drop, clipboard image paste, progress, description), edit (incl. Up for last),
delete, quote, reply in thread, copy text/link, pin, star, react, forward, mark
unread, report, start discussion, follow/unfollow thread, slash commands with
previews, drafts.

### Phase 6 — Threads, discussions, contextual panels
Thread panel and threads list, discussions list and creation, room info (view/edit),
members (search, roles, DM, kick, mute, owner/moderator/leader, ignore), pinned/
starred/mentions/files lists, search in room, notification preferences, room
settings, invite users, invite links, prune messages, export.

### Phase 7 — Rooms, directory, user
Create channel/group/DM/group DM/discussion/team, directory (users/channels/teams),
quick switcher (Ctrl+K, `spotlight`), join/leave/hide/favorite/mark read, status
menu (online/away/busy/offline + custom message + custom statuses), profile edit,
avatar upload, preferences, personal access tokens, 2FA setup (TOTP QR + backup
codes, email 2FA), active sessions.

### Phase 8 — Desktop integration
Notifications honouring server prefs (desktop alert rules, DND/busy, muted rooms),
notification actions (open room, reply where supported), sounds (notification
sound hint, custom server sounds via `pw-play`/`paplay` fallback), tray with unread
badge and menu, window title badge, single instance + deep links
(`rocketchat://room?...`, `https://go.rocket.chat/...`), autostart, minimize to
tray, idle detection → away, proxy settings, downloads, zoom, keyboard shortcuts.

### Phase 9 — Teams, video conferences, omnichannel
Teams (team channels, add/remove rooms, autojoin, members, convert), video
conferences (start/join/ring via external browser, `videoconf` message blocks,
incoming call popup), omnichannel agent (queue/inquiries, take, close with
comment/tags, transfer to agent/department, visitor info, canned responses,
availability toggle, on-hold, contact history).

### Phase 10 — Apps and UIKit
Render message `blocks` (section, actions, context, divider, image, input, preview,
callout, video_conf), block actions via `/api/apps/ui.interaction/:appId`, modals and
contextual bars from the `uiInteraction` stream, app action buttons (room, message,
composer menus).

### Phase 11 — End-to-end encryption
Compatible with Rocket.Chat 8.9 E2EE: key pair generation, private key
backup encrypted with the E2E password, room key distribution
(`e2e.*` endpoints, suggested keys), message encrypt/decrypt (all algorithm versions
the server and official clients use), encrypted uploads, key reset. Implemented
strictly from the reference sources in `.ref/` with cross-compatibility tests.

### Phase 12 — Administration
Workspace info/statistics, users, rooms, invites, custom emoji, custom sounds,
custom user statuses, integrations (incoming/outgoing webhooks), OAuth apps, mailer,
permissions and roles, generic settings editor (driven by setting metadata and
types), apps (installed list, enable/disable, settings), email inboxes, LDAP
test/sync, moderation console, license/subscription info.

### Phase 13 — Offline cache and polish
SQLite cache for instant start and offline reading, outgoing message queue with
retry, selectable message text (custom widget on top of `iced::advanced::text`),
accessibility, performance (lazy timeline, image cache limits), packaging
(`.deb` via cargo-deb, AppImage, desktop file, icons).

## Known limitations / decisions

- iced 0.14 `text`/`rich_text` is not mouse-selectable; until the custom selectable
  widget lands (phase 13) copying works via message actions.
- Voice messages need audio capture without ALSA headers: planned through
  `pw-record`/`parecord` subprocesses (phase 8+), playback via external player.
- Spell checking is out of scope for now.
- Federation (Matrix) features are server-side; the client only renders them.
- Accessibility (phase 13): iced 0.14 has no accessibility tree (no AccessKit
  in iced, iced_winit or the widgets), so screen readers see an empty window;
  revisit when iced ships AccessKit. What exists: keyboard focus on buttons,
  rows and menu items (`rustket_ui::widget::focus_ring`, visible ring in both
  themes, Enter/Space activate), Tab / Shift+Tab through the window (dialogs
  keep Tab inside), F6 / Shift+F6 between sidebar, timeline, composer and the
  right panel, Escape closes the innermost popover / dialog / input focus /
  right panel, Alt+Up/Down switch rooms and scroll the sidebar to them, arrow
  keys in menus and the status menu (see `crates/rustket/src/focus.rs`). Text
  scaling is the zoom setting (iced scale factor: every size scales). Theme
  tokens are checked against WCAG AA in `rustket-ui` tests (text 4.5:1, focus
  ring and icons 3:1).
- Image caches: decoded images in memory are an LRU bounded by 256 MB; the
  disk cache is capped (Settings → Storage, 500 MB by default) and trimmed
  oldest first.
