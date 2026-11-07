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

## Settings

- `settings.public` is paginated, and `count` is capped at 100 (`API_Upper_Count_Limit`). `count=0`
  returned everything on the dev server, but that depends on `API_Allow_Infinite_Count`, so the client
  fetches pages of 100. 393 public settings on the dev server.
- Filtering by ids: `settings.public?_id=Site_Name,CAS_login_url` (comma separated).
- Entries: `{_id, value, enterprise, invalidValue?, modules?}`. `type` is not included.

## History

- The client uses `loadHistory` through `POST /api/v1/method.call/loadHistory` with params
  `[rid, end|null, limit, ls|null, showThreadMessages]`. Reasons:
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
