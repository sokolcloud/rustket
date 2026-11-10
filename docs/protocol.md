# Rocket.Chat protocol notes

Verified against the dev server (Rocket.Chat 8.9.1) with curl and the live tests in
`crates/rocketchat/tests/live.rs`. Captured responses: `crates/rocketchat/tests/fixtures/`.

## Envelopes and errors

- Success: the payload plus `"success": true` at the top level (`{"message": {...}, "success": true}`).
  The login response is different: `{"status": "success", "data": {...}}`.
- Errors come in these shapes:
  - `{"success": false, "error": "...", "errorType": "error-room-not-found"}`, HTTP 400.
    `error` is a sentence that ends with `[errorType]`.
  - `{"success": false, "errorType": "invalid-params", "error": "must have required property 'roomId'..."}` (schema validation)
  - `{"success": false, "error": "Unauthorized", "status": "error", "message": "Unauthorized"}`, HTTP 401
    (wrong password, bad resume token)
  - `{"success": false, "error": "You must be logged in to do this.", "status": "error", "message": "..."}`, HTTP 401
    (missing or revoked token)
  - `{"success": false, "error": "unauthorized"}` for a permission error (`rooms.hide` on a foreign room). It is
    not mapped to `Error::Unauthorized`.
  - Some endpoints answer HTTP 200 with `success: false`, so the client checks the body as well as the status.
- `POST /api/v1/logout` answers HTTP 200 with an **empty body** (`Content-Length: 0`).
  The token is invalidated anyway (`me` → 401 afterwards).
- Rate limiting is disabled on the dev server (12 fast logins and 30 fast `sendEmailCode` calls all
  succeeded), so the 429 mapping (`x-ratelimit-reset` = epoch ms, body `... [error-too-many-requests]`)
  is only unit-tested.

## Login (`POST /api/v1/login`)

- Password: `{"user": "alice" | "alice@rustket.local", "password": "..."}`. A Meteor digest
  `{"digest": sha256_hex, "algorithm": "sha-256"}` is accepted in place of the plain password.
  The client sends the digest. `{"hashed": ...}` from rest-typings returns HTTP 500.
- Success: `data = {userId, authToken, me}`. `me` is the full `IUser` with
  `settings.preferences`, `services.password.exists`, `services.email2fa.enabled`, `email`,
  `avatarUrl`, `isOAuthUser`.
- Email 2FA (carol), first attempt: HTTP 401
  `{"error":"totp-required","details":{"method":"email","emailOrUsername":"carol","codeGenerated":true,"availableMethods":["email"]},"message":"TOTP Required"}`.
  If an earlier code is still valid, `codeGenerated` is `false` and `codeExpires` is set. No new
  e-mail is sent in that case; the code from the previous e-mail still works.
- Retry with `"code": "435478"` in the same body. The e-mail shows the code as `435-478`, and
  `TwoFactorCode` removes the dash. A wrong code returns HTTP 401 `totp-invalid` with the same
  `details`. Each code works once.
- `POST users.2fa.sendEmailCode {"emailOrUsername"}` is public. It answers `{"success":true}` even for
  unknown users.
- Resume: `{"resume": "<token>"}`. 8.9 returns the **same** token. An invalid token returns 401 `Unauthorized`.
- LDAP: `{"ldap": true, "username", "ldapPass", "ldapOptions": {}}` is accepted. With LDAP disabled
  the server falls back to a password login.
- OAuth (legacy): `{"oauth": {"credentialToken", "credentialSecret"}}`. SAML: `{"saml": true, "credentialToken"}`.
  CAS: `{"cas": {"credentialToken"}}`.
- 2FA with non-password logins: a top-level `code` is **ignored** for LDAP/OAuth/SAML/CAS. The
  server only honours `{"totp": {"login": <original login body>, "code": "123456"}}`. Verified live
  for LDAP and for OAuth. With email 2FA enabled for the Keycloak user, the first
  `{"oauth": {credentialToken, credentialSecret}}` answers `totp-required`, and repeating the
  wrapped request with the **same** credentialToken/credentialSecret succeeds, because the pending
  credential is not consumed by the failed attempt. `LoginRequest::body_with_code` builds this
  form. Password logins keep the top-level `code`, which works.
- Disabling email 2FA (`users.2fa.disableEmail`) answers `errorType: totp-required` and needs the
  `x-2fa-*` headers with an e-mailed code.
- 2FA on protected endpoints: send the headers `x-2fa-code` / `x-2fa-method` (`totp`, `email`, or
  `password` with the SHA-256 hex of the password). On 8.9, `users.generatePersonalAccessToken` did
  not ask alice (no 2FA) for a code shortly after login. A personal access token is used exactly
  like a login token (`X-User-Id` + `X-Auth-Token`).

## Login services (`GET /api/v1/settings.oauth`, public)

- Built-in OAuth providers (google, github, gitlab, …) carry **`name`**, not `service`, and only
  `clientId` plus button fields. Custom OAuth, SAML and CAS carry `service`.
- Custom OAuth (`keycloak`): `serverURL`, `authorizePath` (relative), `tokenPath`, `identityPath`,
  `scope`, `loginStyle`, `showButton`, `custom: true`, button colours.
- SAML: `service: "saml"`, `clientConfig.provider`, `entryPoint`, `buttonLabelText`. It only
  appears once the provider config is valid (a certificate is set).
- CAS: `service: "cas"`, `login_url`, `base_url`, `width`, `height`, `autoclose`. The public settings
  `CAS_enabled` and `CAS_login_url` hold the same information.
- The legacy authorize URL `…/auth?client_id&redirect_uri={server}/_oauth/keycloak&response_type=code&state&scope`
  works with an unpadded base64url `state` (the decoder is Node's `Buffer`). The `/_oauth/keycloak`
  page does not issue an HTTP redirect. It renders
  `<div id="config">{"credentialToken":…,"credentialSecret":…,"redirectUrl":"rocketchat:&#x2F;&#x2F;auth?host=http:&#x2F;&#x2F;localhost:3000&#x2F;&type=oauth&credentialToken=…&credentialSecret=…"}</div>`
  and JS navigates there. `/` is HTML-escaped, and `host` has a **trailing slash**.
- The public setting `Accounts_OAuth_Use_Modern_Flow` defaults to `false`.
- Rocket.Chat 9.0 removed the legacy Meteor OAuth flow and that setting (PR #42369): Apple,
  GitHub, GitLab, Google, Nextcloud and custom OAuth always go through Passport
  (`/oauth/{service}?loginClient=desktop`, state/PKCE in the server session). A legacy authorize
  URL ends on the server's `/login` → `/home`, never on `rocketchat://`. With 2FA the server
  redirects to `/2fa/{method}/{challengeId}?loginClient=desktop`; the web app asks for the code
  and then deep-links a Token link (`?host&token&userId`). The client uses the modern flow for
  every server ≥ 9.0.

## Settings

- `settings.public` is paginated, and `count` is capped at 100 (`API_Upper_Count_Limit`). `count=0`
  returned everything on the dev server, but that depends on `API_Allow_Infinite_Count`, so the client
  fetches pages of 100. 393 public settings on the dev server.
- Filtering by ids: `settings.public?_id=Site_Name,CAS_login_url` (comma separated).
- Entries: `{_id, value, enterprise, invalidValue?, modules?}`. `type` is not included.

## History

- The client uses `loadHistory` through `POST /api/v1/method.call/loadHistory` with params
  `[rid, end|null, limit, ls|null, showThreadMessages]` (`showThreadMessages` defaults to `false`
  like the web client's main timeline, user preference `showThreadsInMainChannel`; thread replies
  sent with `tshow` are still included). Reasons:
  - it works for every room type; `channels.history`, `groups.history` and `im.history` are
    type-specific;
  - it returns `firstUnread` / `unreadNotLoaded` when `ls` is given;
  - it returns the same messages with `md` as `channels.history`.
  Dates in the result are EJSON `{"$date": ms}`, so `RcDate` accepts both forms. Messages come
  newest first. On the server `end` is exclusive, so messages that share the boundary millisecond
  would be skipped. The client sends `end = before + 1 ms`, which makes `before` inclusive, and
  callers drop `_id`s they already have. Use `HistoryCursor`, which dedups and decides the end on
  the raw page length. When a whole page shares one millisecond it doubles the limit (up to 400)
  and, as a last resort, steps past that millisecond, skipping the rest of that millisecond.
- `loadHistory` returns `result: false` (not an error) for a missing room or a room the user cannot
  access. The client maps it to `Error::Api{error_type: "error-not-allowed"}`.
- `chat.syncMessages?roomId&lastUpdate=ISO` → `{result: {updated: [...], deleted: [{_id, _deletedAt}]}}`.
- `method.call` body: `{"message": "<stringified {msg:'method', id, method, params}>"}`. The response
  is `{"message": "<stringified {msg:'result', id, result|error}>", "success": bool}`. Errors look like
  `{"isClientSafe":true,"error":404,"reason":"Method 'x' not found","errorType":"Meteor.Error"}`.
  `error` can be a number.

## Messages

- `chat.sendMessage {message: {_id, rid, msg, tmid?, tshow?}}` keeps the client `_id`.
- An edit removes mentions that are no longer in the text (`chat.getMentionedMessages` drops the message).
- Reactions: `{":thumbsup:": {"usernames": [...]}}`, without `names` unless real-name display is on.
- A quote is a message whose text starts with `[ ](<permalink>)`. The server adds an attachment
  `{text, md, message_link, author_name, author_icon, ts, attachments: []}`, and the URL entry gets
  `ignoreParse: true`.
- Pinning a message sets `pinned`, `pinnedAt` and `pinnedBy` on it and creates a `message_pinned`
  system message that carries the pinned text as an attachment.
- `md` output on 8.9 includes `SPOILER`, `SPOILER_BLOCK`, `TIMESTAMP` (with `fallback: [start,end]`),
  `TABLE` / `TABLE_ROW` / `TABLE_CELL` (with `align`), `HORIZONTAL_RULE`, `KATEX` / `INLINE_KATEX`
  (`\[ \]` and `\( \)`; `$$` stays plain text), and `LINE_BREAK` (`value: null`).
  `color:#ff0000` stays plain text (the colors option is off). Emoji nodes are either
  `{value: {PLAIN_TEXT}, shortCode}` or `{value: null, unicode}`. A message of up to 3 emoji is
  sent as `BIG_EMOJI`. `IMAGE.label` is a single node, while `LINK.label` is an array.

## Uploads and downloads

- Step 1: `POST /api/v1/rooms.media/{rid}`, multipart field `file`. It returns
  `{file: {_id, url: "/file-upload/<id>/<name>"}}`, and the file is not yet visible in the room.
- Step 2: `POST /api/v1/rooms.mediaConfirm/{rid}/{fileId}` with JSON `{msg?, description?, tmid?}`.
  It returns `{message}` with `file`, `files[]` and `attachments[]`:
  - images get `image_url`, `image_type`, `image_size`, `image_dimensions`, `image_preview`
    (a base64 JPEG thumbnail) and `image_alt` (= description);
  - other files get `type: "file"`, `format` and `descriptionMd`;
  - `title_link_download: true`.
- `/file-upload/...` without auth → 403. It works with the `X-User-Id` / `X-Auth-Token` headers,
  with `rc_uid` / `rc_token` cookies, or with `?rc_uid=&rc_token=` query params. The client sends the
  headers, and only to its own server.
- `/avatar/{username}` and `/avatar/room/{rid}` are public. Default avatars are SVG even with `format=png`.

## Misc

- `GET /api/info` returns `version: "8.9"` (without the patch number) even when authenticated.
- `rooms.membersOrderedByRole` works for any room type and includes `roles` per member.
- `spotlight?query=dev` returned no rooms for alice, who is a member of `dev-public`. Users come
  back with `outside: true`.
- `emoji-custom.list` → `{emojis: {update, remove}}`. `custom-user-status.list` is paginated
  (`statuses`). `permissions.listAll` → `{update, remove}`.

## Client safety rules (enforced in `crates/rocketchat`)

- **Redirects**
  - API calls follow only same-origin redirects (same scheme, host and port). A cross-origin
    redirect becomes `Error::Redirect`, so the auth headers and login bodies are never re-sent.
    reqwest strips only `Authorization`/`Cookie` across origins, not `X-Auth-Token`.
  - `download()` follows a cross-origin 3xx (S3/GCS file storage) with a separate client: no
    auth headers, no cookies, no `Referer`, at most 5 hops, and never from https to http.
- **Timeouts**
  - Every request has a 20 s connect timeout and HTTP/2 keep-alive pings.
  - API calls have only a 60 s total limit. A read timeout would also cap the time to the first
    byte, so API calls don't use one.
  - Downloads have a 30 s read timeout (time to first byte and between reads) and no total limit.
  - Uploads are guarded by a watchdog: they abort after 60 s without sending progress, or 120 s
    without a response once the whole body is sent (`Error::Timeout`).
  - All values are configurable in `RestConfig`.
- **TLS pins**
  - A pin is bound to a host: `TlsConfig::trust(host, sha256)`. The pinned leaf is accepted only
    when the connection's server name equals that host, regardless of SAN or expiry. This is the
    usual "trust this certificate" behaviour for a self-signed server the user approved.
  - Rejected chains are recorded per host: `TlsContext::take_rejected_chain(host)`, where `host`
    comes from `Error::Certificate { host, .. }`.
  - Certificate errors are never treated as transient.
- **Deep links**
  - `DeepLink::matches(&ServerUrl)` compares the full origin (scheme, host, port; a missing
    scheme means https) and requires the **same** path prefix (`host` is `Meteor.absoluteUrl()`). `http://chat.corp.com:80` does **not** match
    `https://chat.corp.com`.
  - After a match the app keeps using its stored `ServerUrl`. `untrusted_server_url()` is only for
    an "add this server?" prompt.
  - `Token` links (`?host&userId&token`, from the modern OAuth flow or browser login) carry no nonce
    the app could check, so they are a login-CSRF vector. The app must show which account would be
    added and ask for explicit confirmation. OAuth/SAML links are matched against the pending
    `credentialToken`.
- **Redirect errors**: `Error::Redirect { cross_origin }` tells a refused cross-origin redirect
  apart from a same-origin loop, too many hops, or a 307 that would have to replay a streamed body.
- **Tolerance**: an invalid nested value (attachment, reaction, URL, date, `lastMessage`) is dropped
  and logged at debug level instead of failing the message. Invalid items in `rooms.get`,
  `subscriptions.get`, history and paginated lists are skipped, logged at warn level with the `_id`
  only, and counted (`dropped`, `raw_len`). Decide pagination on `raw_len`, not on the number of
  parsed items. `RcDate` also accepts numeric strings
  and bare numbers: values below 10^11 are seconds, larger values are milliseconds.

## Realtime (DDP session)

Implemented in `crates/rocketchat/src/session` on top of `meteor-ddp`; verified by
`crates/rocketchat/tests/session_live.rs`. Captured stream messages:
`crates/rocketchat/tests/fixtures/ddp/` (`synthetic_*` files follow `streams.ts` for events a
test cannot trigger without changing global server state).

- Endpoint `ws(s)://host[/prefix]/websocket`, same rustls config as REST (no ALPN), header
  `User-Agent: Rustket/<version>`.
- Login after every (re)connect: method `login` with `[{"resume": token}]` →
  `{id, token, tokenExpires, type: "resume"}`. The session checks `id` against its user id.
  A rejected token answers `error: 401` (`User not found`) or `403`.
- Stream subscriptions: `sub` name `stream-<name>`, params `[eventName, {"useCollection": false, "args": []}]`
  (what the web client sends). Events arrive as `changed` with
  `fields: {eventName, args: [...]}`.
- Subscriptions accepted for a normal user on 8.9: `stream-room-messages` `__my_messages__`;
  every `stream-notify-user` `{uid}/…` event in `streams.ts`; every `stream-notify-logged`
  event except **`private-settings-changed` → `nosub` `not-allowed`**; every `stream-notify-all`
  event; `stream-roles` `roles`; every `stream-notify-room` `{rid}/…` event (including the
  legacy `typing`).
- `__my_messages__`: args `[IMessage, {roomParticipant, roomType, roomName}]` for new and edited
  messages (and system messages such as `ul`) of every room the user **can access**: joined rooms
  and also public channels the user never joined (`roomParticipant: false`; private groups the
  user is not in are not delivered). `streams.ts` types it as `[IMessage]` only. The session
  exposes the flag as `SessionEvent::Message { room_participant }`; the state layer must ignore
  non-participant messages unless that room is open (previewed) or watched. Edits carry
  `editedAt`/`editedBy`.
- `{uid}/subscriptions-changed`: `[action, ISubscription]`, action `inserted|updated|removed`.
  On 8.9 `removed` carried the full subscription (leaving a channel, deleting a DM partner);
  `streams.ts` allows `{_id, u?, rid?, t?}` only, which also decodes. Leaving a channel sent no
  `rooms-changed`.
- `{uid}/rooms-changed`: `[action, IRoom]`. A new DM carries `uids`, `usernames` and
  `_USERNAMES`. Every new message also sends `updated` with `lastMessage`.
- `{uid}/notification`: `[{title, text, payload: {_id, rid, sender, type, name, message: {msg}}}]`
  (sent with the default preferences for every message in a channel, not only mentions).
- `{uid}/userData`: `[{id, type: "updated", diff: {...}, unset: {...}}]` on changes of the
  user's own document (e.g. status).
- `user-status` (notify-logged): args `[[uid, username, statusCode, statusText, name, roles,
  statusSource, statusExpiresAt]]`, codes 0 offline, 1 online, 2 away, 3 busy. `statusText`
  is `null` when unset and `""` after clearing. A user without any DDP connection is reported as
  0 even right after `users.setStatus busy`. Initial statuses: `users.presence`.
- `updateAvatar`: `[{username}]` after `users.resetAvatar` (no `etag`), or `{rid, etag}` for rooms.
- `roles-change`: `[{_id: roleId, type: "added"|"removed", u: {_id, username}, scope?}]`.
- `updateCustomUserStatus`: `[{userStatusData: {_id, name, statusType}}]`;
  `deleteCustomUserStatus` additionally has `_updatedAt`.
- `Users:Deleted`: `[{userId, messageErasureType: "Delete"}]`. `Users:NameChanged` was not sent
  when a user changed only their real name with `users.updateOwnBasicInfo`.
- `{rid}/deleteMessage`: `[{_id}]`. `{rid}/deleteMessageBulk` (e.g. `rooms.cleanHistory`):
  `[{rid, excludePinned, ignoreDiscussion, ts: {"$gt": date, "$lt": date}, users: []}]`
  (`ids` takes priority when present; `$gt`/`$lt` are exclusive). REST `rooms.cleanHistory`
  defaults to `inclusive: false`; the retention cron uses `inclusive` (`$gte`/`$lte`) and may
  send `filesOnly: true` with `replaceFileAttachmentsWith` (the messages stay, only their files
  are replaced). Shapes from `cleanRoomHistory.ts`, not captured.
- `{rid}/messagesImported`: args `[null]` after an import into the room; the web client clears
  and reloads the room history (imported messages never go through `__my_messages__`).
- `userData` diffs can contain `services.*` (hashed login tokens, 2FA data); the session strips
  them.
- **`userData` is untrusted**: `stream-notify-room-users` relays `{rid}/userData` from any member
  of `rid` verbatim to every other member's `{uid}/userData` (`notifications.module.ts`
  `streamRoomUsers.allowWrite`; verified live, docs/teams-videoconf.md §7.5), so a co-member can
  send `{"type":"updated","id":<victim>,"diff":{"name":"SPOOF"}}`. The session never delivers
  `SessionEvent::UserData` nor copies its fields: each one is a hint that schedules a
  `GET /api/v1/me` (debounced 250 ms, at most one per 2 s, bursts coalesced); a user that differs
  from the cached one is sent as `SessionEvent::MeChanged`. `stream-notify-logged`
  (`Users:NameChanged`) is server-only (`allowWrite('none')`). `roles-change` `u.name` is the acting user's name, not the target's.
- `{rid}/messagesRead` was not sent by `subscriptions.read` on the dev server (read receipts off).
- **Typing**: the web client calls the DDP method `stream-notify-room` with params
  `["{rid}/user-activity", username, ["user-typing"], {}]` (extras `{tmid}` in a thread). The
  server (`notifications.module.ts` `canType`) relays it only if the identity equals the user's
  current `name` when the public setting `UI_Use_Real_Name` is on, else the current `username`;
  otherwise it drops the publish silently. The result is `null` either way. The session reads
  `UI_Use_Real_Name` at start, tracks `public-settings-changed`, and keeps its user current from
  `/me` re-reads after `userData` hints and from `Users:NameChanged`.
  Stop: same call with `[]`. Receivers get args `[username, activities, extras]`. The web client
  debounces by 500 ms, renews every 5 s and expires remote activities after 15 s. Other
  activities: `user-recording`, `user-uploading`, `user-playing`. The legacy
  `["{rid}/typing", username, true]` publish returns `null` but nothing is relayed, so the
  session does not subscribe `{rid}/typing` by default.
- Token revocation: after `POST /logout` from another client with the same token, the server
  closes the socket; the resume login on reconnect fails with `401`, and the session emits
  `LoggedOut` and stops.
- Rate limiting: Rocket.Chat's DDP rate limiter (on by default in production, off on the dev
  server) also counts `sub` messages: per stream *name* (e.g. all `stream-notify-user`
  subscriptions together) 80 per 40 s per user and 40 per 40 s per connection. A refused or
  later stopped sub gets `nosub` `{error: "too-many-requests", details: {timeToReset: ms}}`, and
  meteor-ddp never renews a stopped sub. The session therefore supervises every subscription:
  a stopped one is re-subscribed with per-stream jittered backoff (never before `timeToReset`),
  reported through `SessionEvent::Degraded { streams }` while down, and followed by
  `Resync { StreamsRestored }` once back. A required stream (`__my_messages__`, `{uid}/message`,
  `notification`, `rooms-changed`, `subscriptions-changed`, `force_logout`, `user-status`) that
  fails at start fails `Session::start`. Misspelled stream names are *not* refused (the server
  answers `ready` and never delivers), so the subscribed names are pinned by a golden test.
- Ordering: all stream subscriptions are keep-alive handles and one task decodes the
  client-wide data feed, so events keep the server's order. Control events (connection state,
  `Resync`, `Degraded`) are never dropped and keep their order; when the event channel is full,
  stream events are dropped and a `Resync { Overflow }` is queued. `Resync { Reconnected }` is
  emitted only while connected, once every subscription (global and watched rooms) is renewed;
  if that takes longer than `resync_wait` (15 s) it is emitted anyway and again once they are.
  The terminal event (`Connection(Closed)` or `LoggedOut`) is always the last one.
- Limits: DDP frames are capped at 16 MiB. Any user can submit a message with an empty `msg` and
  an arbitrary `md`, which the server relays verbatim (also through REST history). The models
  drop any `md` / `descriptionMd` tree over 32 JSON levels, 20,000 values or 1 MiB of text
  before decoding it (decoding cost grows with depth times size); `msg` stays.
- Offline: streams are only (re)subscribed on a live connection; a subscribe that times out
  because the connection dropped is retried as soon as it is back (no backoff, not reported as
  degraded). `watch_room` called offline returns at once and `RoomWatch::missing()` lists the
  streams until they are subscribed. After every reconnect the session re-reads `me` and
  `UI_Use_Real_Name` (changes while offline are not streamed).
- Logging: tungstenite logs every WebSocket frame at TRACE, including the `login` frame with the
  resume token and its result. **Never enable `tungstenite=trace` in production**; the app appends
  `rocketchat::LOG_FILTER_PINS` to any user filter (default: `rocketchat::RECOMMENDED_LOG_FILTER`).

## Session device info

- The server fills `rocketchat_sessions.device` from the `User-Agent` of the **DDP** login (REST
  logins leave it out) with `ua-parser-js` 1.0.x plus two custom parsers: `RC Mobile; …` (mobile
  app) and any UA containing ` Electron/` (name is always `Rocket.Chat`). Any other app gets an
  empty `name`/`version`; only the OS is kept, e.g. `Rustket/0.2.0 (X11; Linux x86_64)` →
  `os.name: "Linux"`, `(Windows NT 10.0; Win64; x64)` → `Windows 10`, `(Macintosh; Mac OS X)` →
  `Mac OS`. The security screen therefore names its own session itself.
