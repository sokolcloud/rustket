# Administration panel — implementation spec (Phase 12)

Native replacement for the web client's `/admin/*` pages. Target: Rocket.Chat 8.9.1.

Sources used (paths are relative to `.ref/rocketchat/` unless prefixed):

- Web admin UI: `apps/meteor/client/views/admin/**` (sidebar `sidebarItems.ts`, routes `routes.tsx`).
- REST handlers: in 8.9 they live in `apps/meteor/server/api/v1/*.ts` (**not** `app/api/server`),
  EE handlers in `apps/meteor/ee/server/api/**`, Apps-Engine REST in
  `apps/meteor/ee/server/apps/communication/rest.ts`.
- Typings: `packages/rest-typings/src/v1/**`, `packages/core-typings/src/**`,
  `packages/ddp-client/src/types/streams.ts`, `packages/apps-engine/src/definition/**`.
- RN client: `.ref/rn/app/views/AdminPanelView/index.tsx` (only in the git tree, not checked
  out; read with `git -C .ref/rn show HEAD:app/views/AdminPanelView/index.tsx`) just embeds
  `${baseUrl}/admin/info?layout=embedded` in a WebView and logs it in by injecting
  `Meteor.loginWithToken(<token>)` (`buildLoginScript.ts`). Its sidebar entry is shown with any of
  `view-statistics`, `view-room-administration`, `view-user-administration`,
  `view-privileged-setting` (`SidebarView/components/Admin.tsx`). There is no native admin code in
  the official clients that we could copy.

**Verification legend.** "Verified" means the call was made against the dev server (8.9.1,
`admin`) on 2026-11-08 and the shape below is copied from the real JSON. Mutating calls were made
only on throwaway objects named `rkadm-*` (user, channel, emoji, sound, status, incoming and
outgoing webhook, OAuth app, invite, moderation reports), and every one of them was deleted
afterwards. Calls that would change global state (settings, permissions, roles, assets, apps,
licenses) are specified from source only and marked "not verified" / `UNVERIFIED:` (grep for
`UNVERIFIED` to find every claim that was not exercised live).

A second, independent check (also 2026-11-08) re-ran most calls with throwaway `rkadm-chk-*`
objects (all deleted, temporary login token logged out) and corrected the claims that turned out
wrong; those corrections are noted inline ("re-checked"). The dev server is shared and other
agents create/delete objects concurrently, so absolute counts in this document (users, rooms,
messages, `activeUsers`, JSON sizes) are snapshots, not invariants — never assert them in tests.

A third, skeptical check (also 2026-11-08, throwaway `rkadm-chk3-*` objects, all deleted, both
temporary admin login tokens and a temporary `alice` token logged out) re-read the sources behind
most claims and re-ran the cheap ones live; its corrections are marked "third check". A bulk
`POST settings` probe that would have failed type checks before any write was not run (blocked as a
shared-resource change), so the error format of those checks stays `UNVERIFIED:` (14.7).

---

## 1. Sections, gating and entry point

The admin entry must be shown when the user has any permission of the table below.
**Re-checked: the existing `rustket_core::permissions::can_view_admin` does not do that yet.** Its
`ADMIN_PERMISSIONS` list (`crates/rustket-core/src/permissions.rs`) contains ids that do not exist
on 8.9 (`manage-integrations`, `manage-own-integrations`, `manage-moderation` — absent from
`permissions.listAll`) and misses `manage-cloud`, `view-moderation-console`, `create-invite-links`,
`edit-privileged-setting`, `manage-selected-settings`, `manage-own-incoming-integrations`,
`manage-own-outgoing-integrations`, `view-device-management`, `access-mailer`, `run-import`,
`manage-apps`. Replace it with the union of the table's permissions, but **do not** include
`access-marketplace`: the `user` role has it on the dev server, so every user would see the entry.
Each section is gated exactly as in `apps/meteor/client/views/admin/sidebarItems.ts`:

| Section (web route) | Permission(s) to show | License | Primary endpoints |
|---|---|---|---|
| Workspace (`/admin/info`) | `view-statistics` | – | `GET /api/info`, `statistics`, `instances.get` |
| Subscription (`/admin/subscription`) | `manage-cloud` | – | `licenses.info`, `cloud.*` (read-only in Rustket) |
| Engagement (`/admin/engagement/users`) | `view-engagement-dashboard` | `engagement-dashboard` | `engagement-dashboard/*` |
| Moderation (`/admin/moderation`, tag "Beta") | `view-moderation-console` | – | `moderation.*` |
| Rooms (`/admin/rooms`) | `view-room-administration` | – | `rooms.adminRooms*`, `rooms.saveRoomSettings`, `rooms.delete` |
| Users (`/admin/users`) | `view-user-administration` | – | `users.listByStatus`, `users.create/update/delete`, … |
| AI Center (`/admin/ai-center`, tag "Beta") | `view-privileged-setting` ∨ `edit-privileged-setting` ∨ `manage-selected-settings` | EE modules | out of scope |
| Invites (`/admin/invites`) | `create-invite-links` | – | `listInvites`, `removeInvite/:_id` |
| User status (`/admin/user-status`) | `manage-user-status` | – | `custom-user-status.*` |
| Permissions (`/admin/permissions`) | `access-permissions` ∨ `access-setting-permissions` | custom roles: `custom-roles` | `permissions.*`, `roles.*` |
| ABAC | `abac-management` ∧ … | EE | out of scope |
| Device management (`/admin/device-management`) | `view-device-management` | `device-management` | `sessions/list.all`, `sessions/info.admin`, `sessions/logout` |
| Email inboxes (`/admin/email-inboxes`, tag "Alpha") | `manage-email-inbox` | – | `email-inbox*` |
| Mailer (`/admin/mailer`) | `access-mailer` | – | `POST mailer` |
| Third-party login (`/admin/third-party-login`) | `manage-oauth-apps` | – | `oauth-apps.*` |
| Integrations (`/admin/integrations`) | any of `manage-outgoing-integrations`, `manage-own-outgoing-integrations`, `manage-incoming-integrations`, `manage-own-incoming-integrations` | – | `integrations.*` |
| Import | `run-import` | – | out of scope |
| Reports (`/admin/analytic-reports`) | `view-logs` | – | shows the raw `statistics` JSON (`viewLogs/AnalyticsReports.tsx`); the endpoint itself needs `view-statistics` |
| Sounds (`/admin/sounds`) | `manage-sounds` | – | `custom-sounds.*` |
| Emoji (`/admin/emoji`) | `manage-emoji` | – | `emoji-custom.*` |
| Feature preview (`/admin/feature-preview`) | shown when the client has `defaultFeaturesPreview` entries (client-side flags, no permission) | – | out of scope |
| Settings (`/admin/settings`) | `view-privileged-setting` ∨ `edit-privileged-setting` ∨ `manage-selected-settings` | some settings EE | DDP `private-settings/get`, `POST settings` |
| Apps (marketplace UI, `/marketplace/installed`) | browsing/installed/private: `access-marketplace` ∨ `manage-apps`; "Requested": `manage-apps` (`views/marketplace/sidebarItems.tsx`); every mutation needs `manage-apps` | limits from license | `/api/apps/*` |

On the dev server all of these permissions belong to `admin` only, except `create-invite-links`
(admin, owner, moderator) and `access-marketplace` (admin, user) (re-checked).
`can-audit`, `can-audit-log`, `view-members-list-all-rooms` and
`abac-management` do not exist without an EE license (verified: absent from
`permissions.listAll`).

License state: `GET /api/v1/licenses.info` (verified, CE dev server):

```json
{"license":{"activeModules":[],"externalModules":[],"preventedActions":{"activeUsers":false,"guestUsers":false,
 "roomsPerGuest":false,"privateApps":true,"marketplaceApps":false,"monthlyActiveContacts":false},
 "limits":{"activeUsers":{"max":-1},"guestUsers":{"max":-1},"privateApps":{"max":0},"marketplaceApps":{"max":5},
 "monthlyActiveContacts":{"max":-1}},"tags":[],"trial":false,"hasValidLicense":false},"success":true}
```

With `?loadValues=true` every limit also carries `value` (current usage). `limits` (and the
`license` object itself) are filled only for `view-privileged-setting` (`License.getInfo({limits:
unrestrictedAccess, license: unrestrictedAccess, …})`): third check, as `alice` the same call
returned `"limits":{}` and no `license.license`, so never read limits for a non-privileged user.
`cloudSyncAnnouncement` is added for `manage-cloud`
(`apps/meteor/ee/server/api/licenses.ts`). An EE-only section is shown only when
`license.activeModules` contains its module; otherwise show it disabled with a "Premium" tag
(like the web) or hide it. `GET licenses.maxActiveUsers` → `{"maxActiveUsers":null,"activeUsers":<n>}`
(verified; `activeUsers` changes with the shared server's state). `licenses.get` and
`licenses.isEnterprise` answer 404 on 8.9 (verified).

Fallback for everything not implemented natively: an "Open in browser" action that opens
`{server}/admin/<route>` with `Effect::OpenUrl`; the browser uses its own login. Re-checked: the web
client *does* support a token hand-over — `AppLayout.tsx` → `useLoginViaQuery` logs in with
`?resumeToken=<authToken>` on any route — but do **not** use it: the token would land in browser
history and server access logs, and the browser would share Rustket's login token, so logging out
in the browser would kill Rustket's session. (UNVERIFIED: `?resumeToken=` was only read in
source, not exercised.)

---

## 2. Cross-cutting protocol rules

### 2.1 Envelopes that differ from `{…, success: true}`

The existing (private) `RestClient::send_value` accepts any JSON value, so `RestClient::send::<T>`
works for bare arrays/booleans too (it only rejects bodies with `success: false` or
`status: "error"`), but the models must expect:

| Endpoint | Body on success (verified) |
|---|---|
| `GET /api/v1/listInvites` | bare array `[IInvite…]`, no `success`; `url` is `""` in stored invites |
| `DELETE /api/v1/removeInvite/:_id` | bare `true` |
| `POST /api/v1/oauth-apps.delete` | bare `true` |
| `POST /api/v1/oauth-apps.update` | the app's fields at the top level (`{_id, name, active, redirectUri, clientId, clientSecret, …}`) plus `success` |
| `GET /api/v1/rooms.adminRooms.getRoom` | room fields at the top level plus `success` |
| `GET /api/v1/email-inbox/:_id` | inbox fields at the top level plus `success` (source); missing id → HTTP 404 `{"success":false,"error":"Resource not found"}` (third check, verified) |
| `GET /api/v1/settings/:_id` | `{_id, value, success}` |
| `GET /api/apps/actionButtons` | bare `[]` |
| `GET /api/apps/categories`, `GET /api/apps/marketplace` | bare arrays |
| `GET /api/apps/*` in general | base path is `/api/apps/…`, **not** `/api/v1/…`. Third check: no new URL builder is needed — `ServerUrl::api_root("apps/installed")` already yields `{base}/api/apps/installed`; build the request with `RestClient::request(Method, &url, two_factor)` and decode with `RestClient::send` (the path-based `get`/`post` helpers always use `ServerUrl::api`, i.e. `/api/v1`). `installed`, `count`, `languages` are normal objects with `success` (verified) |

Some failures are not `Meteor.Error`s and carry only `error` (an i18n key, no `errorType`), e.g.
`{"success":false,"error":"LDAP_disabled"}` and `{"success":false,"error":"MIME type not allowed"}`
(both verified). Show `t(error)` from the server translations (section 14.12) when it exists, else
the raw string.

Body/query schema violations (ajv) answer HTTP 400
`{"success":false,"errorType":"invalid-params","error":"must NOT have additional properties"}`
(verified, e.g. `roleName` sent to `roles.addUserToRole`, or `{"settings":[]}` →
`must NOT have fewer than 1 items`). They are checked before 2FA, so no `totp-required` comes first.

Permission failures of a route-level `permissionsRequired` look like HTTP **403**
`{"success":false,"error":"User does not have the permissions required for this action [error-unauthorized]"}`
(verified, no `errorType`). Permission checks made inside handlers have other shapes, all HTTP 400:
`Meteor.Error('not_authorized')` → `{"success":false,"error":"[not_authorized]","errorType":"not_authorized"}`
(third check: `listInvites` as `alice`), and plain `throw new Error('error-not-allowed')` /
`Error('error-not-authorized')` (statistics, `findAdminRooms`, `ldap.syncNow`) →
`{"success":false,"error":"error-not-allowed"}` without `errorType` (source). Map all of them to one
"not allowed" error. EE gating answers
`{"success":false,"error":"This is an enterprise feature [error-action-not-allowed]","errorType":"error-action-not-allowed"}`
(verified on `engagement-dashboard/users/active-users` and `sessions/list.all`).

### 2.2 Pagination, sorting, filters

- `count`/`offset`; `count` is capped by `API_Upper_Count_Limit` (100 on the dev server); omitted
  → `API_Default_Count` (50); `count=0` means "no limit" when `API_Allow_Infinite_Count` is true
  (true on dev), else the default (`apps/meteor/server/api/lib/getPaginationItems.ts`).
  `total` is the full match count. Decide "has more" on `offset + raw_len < total`.
- `sort` is a JSON **string**: `sort={"name":1}` (values must be `1`/`-1`, else
  `error-invalid-sort`; `apps/meteor/server/api/lib/parseJsonQuery.ts`).
- `query` and `fields` are ignored unless the server runs with
  `ALLOW_UNSAFE_QUERY_AND_FIELDS_API_PARAMS=true` (`parseJsonQuery.ts`). Verified:
  `users.list?query={"username":"admin"}` returns the same `total` as without `query` (all users).
  Never rely on `query`; use the
  endpoints' explicit filters (`searchTerm`, `filter`, `name`, `selector`, …).
- Array query parameters: `key[]=a&key[]=b`. Verified for `roles[]`, `types[]`,
  `inactiveReason[]`; a single `roles=admin` also works. `Query::push("types[]", t)` repeated is
  the right encoding.

### 2.3 Two-factor confirmation on admin actions

Endpoints with `twoFactorRequired: true` (grep of `apps/meteor/server/api/v1/*.ts`,
`apps/meteor/ee/server/api/*.ts`): `users.update`, `users.resetTOTP`, `users.resetE2EKey`,
`settings` (bulk POST), `settings/:_id` (POST), `settings.addCustomOAuth`,
`settings.removeCustomOAuth`, `settings.refreshOAuthServices`, `presence.enableBroadcast`,
`ldap.syncNow` (EE file `apps/meteor/ee/server/api/ldap.ts`), `sessions/list.all`,
`sessions/info.admin`, `sessions/logout`, plus the personal endpoints
`users.generatePersonalAccessToken`, `users.regeneratePersonalAccessToken`,
`users.removePersonalAccessToken`, `users.2fa.disableEmail`, `users.enableTotp`,
`users.validateTotp`. `ldap.syncNow` also has `forceTwoFactorAuthenticationForNonEnterprise`:
without a valid license (`ee/server/api/api.ts`) it becomes `disableRememberMe` +
`requireSecondFactor` (password fallback forced) — re-checked: asked every time on the CE dev
server, even right after a remembered call.

Flow (`apps/meteor/server/lib/2fa/code/index.ts`), verified with `admin` (password user, no 2FA
configured; `Accounts_TwoFactorAuthentication_Enforce_Password_Fallback = true`):

1. First call → HTTP 400
   `{"success":false,"error":"TOTP Required [totp-required]","errorType":"totp-required","details":{"method":"password","codeGenerated":false,"availableMethods":[]}}`.
   This already maps to `Error::TwoFactorRequired(TwoFactorChallenge)`.
2. Ask the user: `method: "password"` → password field, send `x-2fa-code: sha256_hex(password)`
   (lower case) and `x-2fa-method: password`; `totp` → 6-digit code; `email` → code from the
   e-mail (`codeGenerated` says whether a new one was sent). Retry the same request with the
   headers (`RestClient::post_with(path, body, Some(&code))`; add `put_with`/`delete_with`/
   multipart variants). Always send the challenge's `method` (or one of `availableMethods`):
   the server only knows `totp` and `email` by name, so a user who has TOTP or e-mail 2FA and sends
   `x-2fa-method: password` gets the hash checked as a TOTP/e-mail code and fails
   (`getMethodByNameOrFirstActiveForUser`, source). A wrong code → HTTP 400 `errorType: "totp-invalid"` with the same
   `details` (verified); too many wrong TOTP/e-mail codes → `totp-max-attempts`. Keep the prompt
   open on `totp-invalid`.
3. On success the server remembers the authorization for this login token and connection
   fingerprint for `Accounts_TwoFactorAuthentication_RememberFor` seconds (1800 on the dev
   server). Verified: a second `users.update` right after succeeded without headers. The
   fingerprint is sha256 of `{userAgent, clientAddress}`, but for REST the server passes a
   `Headers` object, so `httpHeaders['user-agent']` is undefined and only the client IP counts
   (re-checked: a call with a different `User-Agent` was still remembered).
4. Endpoints with `twoFactorOptions: { disableRememberMe: true }` ask **every time**:
   `POST settings` (bulk save), `users.resetTOTP`, `users.resetE2EKey` (and `ldap.syncNow` on CE,
   above). Verified for `users.resetTOTP`, `users.resetE2EKey` and `POST settings` right after a
   remembered `users.update` (the `POST settings` probe used a non-existent setting id and no code,
   so nothing was written).
5. Users created less than RememberFor ago with no 2FA method are not asked at all
   (`isAuthorizedForToken`; also for `disableRememberMe` endpoints, but not for
   `requireSecondFactor`).
6. Nobody is asked when `Accounts_TwoFactorAuthentication_Enabled` is false, when the login token
   is a personal access token created with `bypassTwoFactor`, or when the user has no TOTP/e-mail
   2FA and the password fallback does not apply (no password, e.g. OAuth/LDAP-only users, or
   `Accounts_TwoFactorAuthentication_Enforce_Password_Fallback` false) — except on
   `requireSecondFactor` endpoints, where such users cannot pass at all. `requireSecondFactor`
   also defeats the `bypassTwoFactor` personal access token and the fresh-user grace period of
   point 5 (`isAuthorizedForToken` returns early), so on CE `ldap.syncNow` always prompts.

UI: one reusable modal `TwoFactorPrompt { challenge, on_submit }` in the admin screen. Keep the
pending request as a closure/enum and replay it with the code. Never keep the password or the
hash after the retry, never log it.

### 2.4 Multipart uploads

All admin uploads are a single multipart request (no `rooms.media` two-step). Reuse
`files::UploadSource` and the upload client (watchdog, progress):

| Endpoint | File field | Other fields |
|---|---|---|
| `POST emoji-custom.create` | `emoji` | `name`, `aliases` (comma-separated) |
| `POST emoji-custom.update` | `emoji` (optional) | `_id`, `name`, `aliases` |
| `POST custom-sounds.create` | `sound` | `name` |
| `POST custom-sounds.update` | `sound` (optional) | `_id`, `name` |
| `POST assets.setAsset` | `asset` | `assetName`, optional `refreshAllClients` |
| `POST users.setAvatar` | `image` | `userId` or `username` |
| `POST /api/apps` (private app) | `app` (zip) | `permissions` (JSON string of granted permissions) |

The server parses with `getUploadFormData` (`apps/meteor/server/api/lib/getUploadFormData.ts`);
the size limit is `FileUpload_MaxFileSize` (100 MiB on the dev server) except sounds (5 MiB).
Send a correct `Content-Type` for the file part: emoji take their stored **extension from the
part's MIME subtype** (`image/png` → `png`, `image/jpeg` → `jpeg`, `image/svg+xml` → converted
and stored as `png`), while sounds are typed by **file name extension** (the part's
`Content-Type` is ignored; verified: a WAV sent as `s.bin` with `audio/wav` was rejected).

### 2.5 Realtime streams useful while the admin screen is open

Verified with a raw DDP client as `admin`: every subscription below answered `ready`
(re-checked for all rows except `user-status`/`Users:NameChanged`; `private-settings/get` over
DDP with a `{"$date": ms}` param also answered `{update, remove}`).

| Stream / event | Args | Use |
|---|---|---|
| `stream-notify-logged` `private-settings-changed` | `[action, partial setting]`. Third check (`packages/streamer/src/listeners.module.ts` `watch.settings`): the payload is **not** a full `ISetting`, only `{_id, value, editor? (colour only), properties, enterprise, requiredOnWizard}`; for EE settings `value` is the *effective* value (`EnterpriseSettings.changeSettingValue`, i.e. `invalidValue` without the module), unlike `private-settings/get`; hidden settings are never sent; `removed` effectively carries only `{_id}` and the record disappears only through the delta's `remove` list. The web merges these fields and then runs a delta `private-settings/get(<last _updatedAt>)` (`PrivateSettingsCachedStore.setupListener` → `CachedStore.sync`) — do the same: use the event as a trigger plus a delta fetch, otherwise newly `inserted` settings (e.g. after `settings.addCustomOAuth`) arrive without `type`/`group`/`section`. | live settings editor; normal users get `nosub not-allowed` (docs/protocol.md) |
| `stream-notify-logged` `permissions-changed` | `[action, IPermission]` (full document; typed as `ISetting` in `streams.ts`); a change of a `level: "settings"` permission also emits `private-settings-changed` (`updated`) for its setting (`notifyOnPermissionChanged`) | permission matrix |
| `stream-roles` `roles` | `{type: 'inserted'|'updated'|'removed'|'changed', …IRole}` (typing in `streams.ts`) | roles list |
| `stream-notify-logged` `roles-change` | `{_id, type: 'added'|'removed'|'changed', u?, scope?}` | users-in-role |
| `stream-notify-logged` `updateEmojiCustom` / `deleteEmojiCustom` | `[{emojiData}]` | emoji table |
| `stream-notify-all` `updateCustomSound` / `deleteCustomSound` | `[{soundData}]` | sounds table |
| `stream-notify-logged` `updateCustomUserStatus` / `deleteCustomUserStatus` | `[{userStatusData}]` | statuses |
| `stream-integrationHistory` `<integrationId>` | `{type:'inserted', data}` / `{type:'updated', id, diff}`. `{type:'removed', id}` is typed but never emitted (third check: `watch.integrationHistory` handles only `inserted`/`updated`), so `integrations.clearHistory` produces no event — refetch after clearing | outgoing webhook history page |
| `stream-apps` `apps` | `['app/added'|'app/removed'|'app/updated', appId]`, `['app/statusUpdate', {appId, status}]`, `['app/settingUpdated', {appId, setting}]`, `command/*`, `actions/changed` | apps list |
| `stream-notify-logged` `Users:NameChanged`, `Users:Deleted`, `user-status` | see docs/protocol.md | users table |
| `stream-notify-all` `license` | typed `[{preventedActions}] \| []`, but third check: the server emits it with **no arguments** (`notifyAllInThisInstance('license')` on `license.sync` / `license.actions`) — treat it as "refetch `licenses.info`" | seats cap banner |

Subscribe only while the corresponding page is open (DDP rate limiter counts `sub`s per stream
name, docs/protocol.md). Most admin pages can simply refetch after their own mutations, like the
web does.

### 2.6 Secrets and logging

Admins receive secrets in clear text: password-type and `secret: true` settings (verified:
`Federation_Service_Matrix_Signing_Key` value is returned, 44 chars, by `private-settings/get` and
`GET settings/:_id`), OAuth app `clientSecret`, incoming and outgoing webhook `token`, e-mail inbox
SMTP/IMAP passwords, custom OAuth secrets, `cloud.registrationStatus.token`. Never log
setting values at all (log setting ids only), never log request bodies of admin calls, redact
these fields in `Debug` impls, and do not persist any admin payload to disk (the Phase 13 SQLite
cache must not store private settings).

---

## 3. Workspace info and statistics

Web: `views/admin/workspace/WorkspaceRoute.tsx`, data hook `client/hooks/useWorkspaceInfo.ts`.
Three parallel queries; page buttons "Refresh" and "Download info" (saves the statistics JSON as
`statistics.json`).

- `GET /api/info` (auth) — verified: `{version: "8.9.1", info: {version, build: {date,
  nodeVersion, arch, platform, osRelease, totalMemory, freeMemory, cpus}, marketplaceApiVersion,
  commit: {hash, date, author, subject, tag, branch}}, minimumClientVersions: {desktop, mobile},
  supportedVersions: {signed: <JWT>}, workspaceUrl, hashedWorkspaceUrl, cloudWorkspaceId}`.
  Existing model: `rocketchat::ServerInfo`. Without auth the same call returns only
  `version: "8.9"` (major.minor) and no `info` (re-checked), so the admin page must send the token.
  Third check: `info.build` describes the CI build host, not the running server (live: `arch:
  "arm64"`, `osRelease: "…-azure"`, `nodeVersion: "v24.15.0"` while the instance runs x64 on
  Node v24.22.1) — label it "Build", and take runtime OS/Node data from `instances.get`
  (`extraInformation.os`, `nodeVersion`) and `statistics.process`.
- `GET /api/v1/statistics?refresh=false|true` — permission `view-statistics`
  (`apps/meteor/server/lib/statistics/functions/getLastStatistics.ts`). `refresh=false` returns
  the last stored snapshot (whatever was saved last — the server-start one, or a later
  refresh by anybody; re-checked: it was a snapshot from a later refresh);
  `refresh=true` generates and stores a new one (~0.5 s on the dev server). Verified: 137–138
  top-level keys depending on the snapshot (third check: 137). **Edge case:** the freshly generated snapshot has `"_updatedAt": null`;
  `refresh=false` has an ISO date. Keep it `Option<RcDate>`.
- `GET /api/v1/statistics.list?count&offset&sort` — stored snapshots (verified).
- `GET /api/v1/instances.get` — permission `view-statistics`
  (`apps/meteor/server/api/v1/instances.ts`). Verified:

```json
{"instances":[{"address":"192.168.1.102","currentStatus":{"connected":true,"lastHeartbeatTime":9,"local":true},
 "instanceRecord":{"_id":"ebfe…","_createdAt":"…","_updatedAt":"…","name":"rocket.chat","pid":1,
  "extraInformation":{"host":"localhost","port":"3000","tcpPort":33235,"os":{…},"nodeVersion":"v24.22.1","conns":0}},
 "broadcastAuth":true}],"success":true}
```

Cards to render (from `VersionCard/VersionCard.tsx`, `UsersUploadsCard.tsx`,
`MessagesRoomsCard.tsx`, `DeploymentCard.tsx`; label keys in parentheses come from the server
i18n file):

- Version: `Version_version` with `/api/info` `version`; support status from the payload of the
  `supportedVersions.signed` JWT (the web only base64-decodes it, no signature check; payload
  `versions[]` with `version`, `expiration`, `releaseType`, `lts`); license name and limits from
  `licenses.info?loadValues=true`; registration state from `GET cloud.registrationStatus`
  (only with `manage-cloud`) → `{registrationStatus: {workspaceRegistered, workspaceId, uniqueId,
  token, email}}` (verified, `workspaceRegistered: false` on dev). Registration actions themselves
  are out of scope (browser fallback).

- Users: `onlineUsers` (Online), `busyUsers` (Busy), `awayUsers` (Away), `offlineUsers`
  (Offline), `totalUsers` (Total); types: `totalConnectedUsers` (Users_Connected),
  `activeUsers` (Stats_Active_Users), `activeGuests`, `nonActiveUsers`, `appUsers`; uploads:
  `uploadsTotal`, `uploadsTotalSize` (bytes, format as size).
- Rooms: `totalChannels`, `totalPrivateGroups`, `totalDirect`, `totalDiscussions`,
  `totalLivechat`, `totalRooms`. Messages: `totalChannelMessages`, `totalPrivateGroupMessages`,
  `totalDirectMessages`, `totalDiscussionsMessages`, `totalLivechatMessages`, `totalMessages`.
- Deployment: `version`, Site URL / hashed site URL (from `/api/info`), `uniqueId`
  (Deployment_ID), `cloudWorkspaceId`, Apps-Engine version (`apps.engineVersion` or
  `info.marketplaceApiVersion`), `process.nodeVersion`, `migration.version` +
  `migration.lockedAt`, `mongoVersion` / `mongoStorageEngine`, commit hash (first 9) and branch,
  `process.pid`; "Instances" button → modal listing each instance's address, `connected`,
  `local`, `lastHeartbeatTime`, `instanceRecord._id`, `name`, `pid`, `_createdAt`,
  `extraInformation`.

Model: keep `Statistics` as a struct with the fields above typed and the rest in
`#[serde(flatten)] extra: serde_json::Map` so "Download info" can re-serialize everything.

---

## 4. Users

Web: `views/admin/users/**`. Server: `apps/meteor/server/api/v1/users.ts`, query builder
`findPaginatedUsersByStatus` in `apps/meteor/server/api/lib/users.ts`, validation in
`apps/meteor/server/lib/users/saveUser/{validateUserData,validateUserEditing}.ts`.

### 4.1 List

The 8.9 web uses `GET /api/v1/users.listByStatus` (not `users.list`). Permission `view-d-room`
(plus `view-outside-room` when `API_Apply_permission_view-outside-room_on_users-list`). Params
(`packages/rest-typings/src/v1/users/UsersListStatusParamsGET.ts`):

| Param | Meaning |
|---|---|
| `status` | `active` → `active: true`; `deactivated` → `active: false` |
| `inactiveReason[]` | `deactivated`, `pending_approval`, `idle_too_long`; `deactivated`/`idle_too_long` also match users without the field |
| `type` | e.g. `user`, `bot` |
| `roles[]` | `$in`; `all` disables the filter |
| `searchTerm` | case-insensitive substring on `username`, `name`, and `emails.address` (only with `view-full-other-user-info`) |
| `hasLoggedIn` | `lastLogin $exists` |
| `sort` | JSON string; `{"status":±1}` also sorts by `active`; omitted → `{username: 1}` |
| `count`, `offset` | |

Tabs (`hooks/useFilteredUsers.ts`): All `{}`; Pending `{type:'user', status:'deactivated',
inactiveReason:['pending_approval']}` (tab label shows `total` of a `count=1` query,
`hooks/usePendingUsersCount.ts`); Active `{status:'active'}`; Deactivated
`{status:'deactivated', inactiveReason:['deactivated','idle_too_long']}`. Sortable columns:
`name`, `username`, `emails.address`, `status`, `active`, `freeSwitchExtension`; switching to
Pending sorts by `active`, other tabs by `name`. Search resets the page to 0.

Projection (fixed server-side): `name, username, emails, roles, status, active, avatarETag,
lastLogin, type, reason, federated, freeSwitchExtension`. Verified row:

```json
{"_id":"2PWdEnL9JYRuMXyCF","username":"rk-acct-probe1","emails":[{"address":"…","verified":true}],"name":"Acct Probe2",
 "type":"user","roles":["user"],"status":"offline","active":true,"lastLogin":"2026-11-07T21:45:14.523Z","avatarETag":"iHXNk6aChpy6heHCD"}
```

`{"name":1}` is a plain (case-sensitive) Mongo sort. Re-checked with a user named `abc chk`: it
sorted **after** `Rocket.Cat`. The same happens with `users.list`: it adds `nameInsensitive` only
as a *secondary* key after `name` (`users.ts` ~line 734), so it is case-sensitive too (verified,
identical order). `users.list` also cannot filter (2.2), so it is not useful for the admin table.
If case-insensitive order matters, sort the current page locally and say so in the UI.

### 4.2 Details

`GET /api/v1/users.info?userId=…&includeUserRooms=true` → `{user: {…, nickname?, bio?,
canViewAllInfo: true, rooms: [{_id, rid, name, t, unread, roles}]}}` (verified; `rooms` are
partial subscriptions; extra fields such as
`statusConnection`, `statusSource`, `requirePasswordChange` appear). Show avatar (`/avatar/{username}`),
name, username, e-mails with verified flag, roles, status, `lastLogin`, `createdAt`,
custom fields, bio, nickname, and the action menu (4.4).

### 4.3 Create / edit

`POST /api/v1/users.create` (verified) body
(`packages/rest-typings/src/v1/users/UserCreateParamsPOST.ts`): required `email`, `name`,
`password`, `username`; optional `active`, `bio`, `nickname`, `statusText`, `roles[]`,
`joinDefaultChannels` (server default **true** when omitted), `requirePasswordChange`,
`setRandomPassword`, `sendWelcomeEmail`, `verified`, `customFields`, `freeSwitchExtension`,
deprecated `fields`. Third check: the TS type also lists `settings`, but the ajv schema has
`additionalProperties: false` without it — sending `settings` answers `invalid-params` "must NOT
have additional properties" (verified). Response `{user}` (verified; it may also carry an internal
`__rooms` array — ignore it; with `joinDefaultChannels: false` it was absent). Permission `create-user`; `roles` containing `admin` needs `assign-admin-role`. No
2FA. `password` is required by the schema even with `setRandomPassword: true`; the web sends the
(possibly empty) form value plus a deprecated `fields: ""`.

`POST /api/v1/users.update` body `{userId, data: {email?, name?, password?, username?, active?,
bio?, nickname?, statusText?, roles?, requirePasswordChange?, setRandomPassword?,
sendWelcomeEmail?, verified?, customFields?, status?, freeSwitchExtension?},
confirmRelinquish?}` → `{user}`. Third check: `settings` and `language` are in the TS type but not
in the ajv schema (`additionalProperties: false`) and are rejected with `invalid-params` before
the 2FA prompt (verified). `data.active` additionally runs `executeSetUserActiveStatus` (same
gate and last-owner handling as 4.4). 2FA required (remembered, 2.3; re-verified: password-hash
retry succeeded, the next call went through without headers).
Permissions: `edit-other-user-info`; `edit-other-user-password` for `password`; `assign-roles`
when `roles` changes (the array **replaces** the user's global roles); `assign-admin-role` for
`admin`. Federated users cannot be edited (`Edit_Federated_User_Not_Allowed`). Re-checked: the
web does **not** send only changed fields — `AdminUserForm.tsx` `handleSaveUser` sends the whole
form as `data` (with `password: ""` when untouched, which the server ignores, and always
`roles`). Third check corrected the rationale: unchanged values do **not** trip the permission
checks — `validateUserEditing.ts` compares `roles` as sets and username/name/e-mail with the stored
values, and an empty `password` is falsy. The one exception is `validateUserData.ts`: any `roles`
array containing `admin` needs `assign-admin-role` even if `admin` was already there. Rustket
should still send only changed fields (smaller, and an editor without `assign-admin-role` can then
edit an admin's other fields).

Form (from `AdminUserForm.tsx`): avatar, name, username, e-mail + verified toggle, status
message, bio, nickname, password with "set random password" radio
(`AdminUserSetRandomPasswordRadios.tsx`), require password change, roles multi-select (options
from `roles.list`), join default channels (create only), send welcome e-mail (disabled when
`GET /api/v1/smtp.check` → `{"isSMTPConfigured":false}`; verified `true` on dev), custom fields
from the public setting `Accounts_CustomFields` (JSON; empty on dev), VoIP extension (only with
`VoIP_TeamCollab_SIP_Integration_Enabled`; that setting does not exist on the CE dev server, so
treat "absent" as false). Name validation: `UTF8_User_Names_Validation`
setting regex for usernames, `validateNameChars` for names (`Name contains invalid characters`).

Avatar for another user: `POST users.setAvatar` multipart `image` + `userId`, or JSON
`{userId, avatarUrl}` (the server fetches the URL; not probed); reset `POST users.resetAvatar
{userId}` (permission `edit-other-user-avatar` or `manage-moderation-actions`). Third check,
verified on a throwaway user: multipart `image` (PNG) + `userId` → `{}` (the `userId` text part may
come after the file part — busboy fields are collected until `finish`), and `users.resetAvatar
{userId}` → `{}`. `users.setAvatar` with only `edit-other-user-avatar` works even when
`Accounts_AllowUserAvatarChange` is false (source).

Create is disabled when `licenses.info → license.preventedActions.activeUsers` is true
(`useShouldPreventAction('activeUsers')`, `AdminUsersPage.tsx`).

### 4.4 Row / detail actions (`hooks/useAdminUserInfoActions.ts`)

| Action | Call | Gate | Verified |
|---|---|---|---|
| Edit | 4.3 | `edit-other-user-info` | yes |
| Direct message | open/create DM | `create-d` | – |
| Make admin / Remove admin | `POST roles.addUserToRole {roleId:"admin", username}` / `roles.removeUserFromRole` | web: `assign-admin-role`; server additionally requires `access-permissions` (`meteor-methods/auth/addUserToRole.ts`, route permission of `removeUserFromRole`) | yes (with role `bot`, re-checked): → `{role:{_id,scope,description,mandatory2fa,name,protected,_updatedAt}}`; `roleName` is rejected with `invalid-params` |
| Activate / Deactivate | `POST users.setActiveStatus {userId, activeStatus, confirmRelinquish?}` → `{user:{_id, active}}` | `edit-other-user-active-status` (the route accepts `manage-moderation-actions` too, but `executeSetUserActiveStatus` then throws `error-not-allowed` without `edit-other-user-active-status`) | yes |
| Reset TOTP | `POST users.resetTOTP {userId}` → `{}` | `edit-other-user-totp` and setting `Accounts_TwoFactorAuthentication_Enabled` | yes (2FA every time) |
| Reset E2E key | `POST users.resetE2EKey {userId}` | `edit-other-user-e2ee` | yes (2FA every time) |
| Send welcome e-mail | `POST users.sendWelcomeEmail {email}` (third check: in the web this is not in `useAdminUserInfoActions` but the "Resend_welcome_email" button of **Pending**-tab rows, next to "Activate", in `UsersTable/UsersTableRow.tsx`; "Activate" is disabled while the seats cap is exceeded) | `send-mail`; fails with `error-email-send-failed` when SMTP is not configured, `error-invalid-user` when no user has that address | yes (→ Mailpit) |
| Delete | `POST users.delete {userId, confirmRelinquish?}` → `{deletedRooms:[]}` | `delete-user`; federated users cannot be deleted (`error-not-allowed`, deactivate instead) | yes |

Other server-side refusals to surface: deactivating the last active admin →
`error-action-not-allowed` "Leaving the app without an active admin is not allowed"
(`server/lib/users/setUserActiveStatus.ts`); removing the `admin` role from the last admin →
`error-admin-required` (`roles.ts`); adding a role the user already has →
`error-user-already-in-role`.

**Danger:** `users.resetTOTP` and `users.resetE2EKey` without `userId`/`username`/`user` reset
the **caller's own** 2FA / E2E key (`users.ts` lines ~1771–1860). The Rust API must take a
non-optional user id for the admin variants.

Delete confirmation text: `Delete_User_Warning_{Message_ErasureType}` (`Delete`, `Unlink`,
`Keep`; the public setting `Message_ErasureType` is `Delete` on dev).

Last-owner flow (`hooks/useConfirmOwnerChanges.tsx`): deactivating or deleting a user who is
the last owner of rooms fails with `errorType: "user-last-owner"` and
`details: {shouldChangeOwner: [room names], shouldBeRemoved: [room names]}`. Show both lists
(new owner will be picked / rooms will be deleted) and repeat the call with
`confirmRelinquish: true`. Re-checked live (throwaway user made sole owner of a throwaway channel
with other members): both `users.setActiveStatus` and `users.delete` answered HTTP 400
`{"success":false,"error":"[user-last-owner]","errorType":"user-last-owner","details":{"shouldBeRemoved":[],"shouldChangeOwner":["rkadm-chk-room-7383"]}}`.
The lists contain room `fname || name`, not ids. Livechat rooms are excluded on deactivate.

Other user endpoints (UNVERIFIED: source only, they mutate many users): `users.deactivateIdle {daysIdle,
role?}` (`edit-other-user-active-status`); `POST sendInvitationEmail {emails: []}`
(`bulk-register-user`, `AdminInviteUsers.tsx`).

---

## 5. Rooms

Web: `views/admin/rooms/**`; server `apps/meteor/server/api/v1/rooms.ts`,
`apps/meteor/server/api/lib/rooms.ts` (`findAdminRooms`), projection
`apps/meteor/lib/rooms/adminFields.ts`, settings method
`apps/meteor/server/meteor-methods/rooms/saveRoomSettings.ts`.

### 5.1 List

`GET /api/v1/rooms.adminRooms?filter=&types[]=…&sort=&count=&offset=` — permission
`view-room-administration`. `types` ⊂ `c`, `p`, `d`, `l`, `discussions`, `teams`; the web sends
all six when no filter is chosen (`DEFAULT_TYPES` in `RoomsTable.tsx`). `filter` is a
case-insensitive substring on `name` (non-federated), `fname`, and on DM `usernames` (re-checked:
`filter=alice&types[]=d` returns the admin–alice DM). Without `discussions` in `types` discussions
are excluded entirely, without `teams` team main rooms are excluded entirely
(`Rooms.findByNameOrFnameContainingAndTypes`). The server default sort `{default: -1, name: 1}`
applies only when `sort` is omitted; the web always sends one (initially `{"name":1}`,
`useSort('name')`). Sortable columns `name`, `t`, `usersCount`, `msgs`, `default`, `featured`,
`ts`. Verified:

```json
{"rooms":[{"_id":"GENERAL","default":true,"msgs":739,"name":"general","t":"c","ts":"…",
 "u":{"_id":"rocket.cat","username":"rocket.cat","name":"Rocket.Cat"},"usernames":[],"usersCount":7}, …],
 "count":2,"offset":0,"total":13,"success":true}
```

DM rows have no `name`; the web shows `usernames.join(" x ")`. Type icon from `t`, `teamMain`,
`prid` (discussion), `encrypted`, `federated`.

Admin projection fields: `_id prid fname name t cl u usernames ts usersCount muted unmuted ro
reactWhenReadOnly default favorite featured topic msgs archived teamId teamMain announcement
description customFields broadcast uids avatarETag federated abacAttributes`.

### 5.2 Edit

`GET /api/v1/rooms.adminRooms.getRoom?rid=` (verified) → admin fields at top level.

`POST /api/v1/rooms.saveRoomSettings` (verified) body
(`packages/rest-typings/src/v1/rooms.ts` `RoomsSaveRoomSettingsProps`): `rid` plus any of
`roomName`, `roomTopic`, `roomAnnouncement`, `roomDescription`, `roomAvatar`, `roomType`
(`c`|`p`), `readOnly`, `reactWhenReadOnly`, `default`, `favorite: {defaultValue, favorite}`,
`featured`, `roomCustomFields`, `systemMessages`, `joinCode`, `encrypted`, `retention*`. Response
`{"rid":"…","success":true}`. The admin form (`EditRoom.tsx`) sends only dirty fields plus always
`default` and `favorite: {defaultValue: isDefault, favorite}`; "favorite" is only meaningful when
"default" is on. Server checks: the whole call needs `edit-room` in the room (else
`error-action-not-allowed`, except an `encrypted`-only change of a DM); `default`/`featured` need
`view-room-administration`; `roomAvatar` needs `edit-room-avatar`; broadcast rooms reject
`readOnly`/`reactWhenReadOnly`;
`roomType` change needs `create-c`/`create-p` (team rooms: `create-team-channel`/`create-team-group`);
`encrypted` only for `p`/`d` and `toggle-room-e2e-encryption`; retention needs
`edit-room-retention-policy`; field visibility per room type comes from
`roomCoordinator.getRoomDirectives(t).allowRoomSettingChange` (`useEditAdminRoomPermissions.ts`)
— DMs allow almost nothing, ABAC-managed rooms hide topic/announcement/description.

Archive is a separate call made when the toggle changed (`useArchiveRoom.ts`):
`POST rooms.changeArchivationState {rid, action: "archive"|"unarchive"}` → `{}` (verified).
Re-checked: the web fires it **in parallel** with the save (`Promise.all` in `EditRoom.tsx`
`handleSave`), not after it; Rustket may serialize them (save first) to get clearer errors.

Owner is shown read-only (`room.u.username`).

### 5.3 Delete

`views/hooks/roomActions/useDeleteRoom.tsx`: `POST rooms.delete {roomId}` (verified) for
everything except a team's main room; the team main room fails with
`error-cannot-delete-team-channel` and must use `POST teams.delete {teamId, roomsToRemove?}`
(the web first shows a modal to choose which team rooms to delete too). Gate:
`delete-{t}` (room-scoped) and, for team rooms, `delete-team-channel` / `delete-team-group` in
the team's main room; federated rooms cannot be deleted.

---

## 6. Invites

Server: `apps/meteor/server/api/v1/invites.ts`, `apps/meteor/server/lib/rooms/invites/*.ts`.

- `GET /api/v1/listInvites` — `create-invite-links` checked **globally** (`listInvites.ts`, no
  scope), so owners/moderators who hold it only as a room role get
  `{"success":false,"error":"[not_authorized]","errorType":"not_authorized"}` (third check, as
  `alice`); `removeInvite` uses the same global check. Returns **all** invites as a bare
  array (verified):
  `[{"_id":"5BAhAq","days":7,"maxUses":5,"rid":"…","userId":"…","createdAt":"…","expires":"…","url":"","uses":0,"_updatedAt":"…"}]`.
- `DELETE /api/v1/removeInvite/:_id` → `true` (verified).
- `POST /api/v1/findOrCreateInvite {rid, days, maxUses}` → `IInvite` with `url` (verified).
  `days ∈ {0,1,7,15,30}` (0 = never, default 1), `maxUses ∈ {0,1,5,10,25,50,100}` (0 = unlimited,
  default 0); an existing invite of the same user/room/limits is returned instead of a new one.
  The caller needs `create-invite-links` in the room **and** must be subscribed to it (else
  `error-invalid-room`), so the admin page can create invites only for rooms the admin is in.
- `POST /api/v1/validateInviteToken {token}` → `{valid}` (verified).

Table (`InviteRow.tsx`): token (`_id`), created at, expiration (`Expired` / relative time /
`Never` when `expires` is null), uses, uses left (`maxUses - uses`, `Unlimited` when 0). The list
has no room names: resolve `rid` through the workspace cache or `rooms.adminRooms.getRoom`.

The stored `url` is empty, so rebuild it as the server does (`findOrCreateInvite.ts`
`getInviteUrl`): if `Accounts_Registration_InviteUrlType == "direct"` →
`{Site_Url}/invite/{_id}`; otherwise (`proxy`, the dev value) the go.rocket.chat link, e.g.
verified `https://go.rocket.chat/invite?host=localhost%3A3000&path=invite%2F5BAhAq&secure=no`
(base from `DeepLink_Url`). `Accounts_Registration_InviteUrlType` is **not public**, so take it
from `private-settings/get` (admins have it); if it is unavailable, fall back to the `url` of a
fresh `findOrCreateInvite` response or to the direct form.

---

## 7. Custom emoji

Server: `apps/meteor/server/api/v1/emoji-custom.ts`,
`apps/meteor/server/lib/media/emoji-custom/lib/insertOrUpdateEmoji.ts`. Permission
`manage-emoji` (checked inside `insertOrUpdateEmoji` / delete).

- List for the admin table: `GET emoji-custom.all?name=&count=&offset=&sort=` →
  `{emojis:[{_id,name,aliases[],extension,_updatedAt,etag}],count,offset,total}` (verified; `name`
  is a case-insensitive substring). `emoji-custom.list?updatedSince=` (`{emojis:{update,remove}}`)
  is what the workspace sync already uses.
- Image: `/emoji-custom/{urlencoded name}.{extension}?etag={etag}` (verified 200 `image/png`).
- Create: multipart `emoji`, `name`, `aliases` → `{}` (verified). Update: multipart `_id`,
  `name`, `aliases`, optional `emoji` (verified; `aliases=""` cleared the aliases). Delete:
  `POST emoji-custom.delete {emojiId}` (verified).
- Normalisation and errors: name is slugified (`limax`, replacement `_`), colons removed;
  invalid if it matches `[\s,:><&"'\/\\()]` → `error-input-is-not-a-valid-field`. Aliases use a
  **different** check, `[:><&|"'\/\\()]` (whitespace and commas allowed, `|` forbidden; re-checked:
  `aliases=a|b` → `error-input-is-not-a-valid-field` with `details.field: "Alias_Set"`), then are
  split on `\s*,\s*`, slugified (re-checked: `Rk Alias,:rk_b:` → `["rk_alias","rk_b"]`) and an
  alias equal to the name is dropped; a name/alias already used by another emoji →
  `Custom_Emoji_Error_Name_Or_Alias_Already_In_Use`; non-image files → `emoji-is-not-image`;
  SVG is stored with extension `png` (`insertOrUpdateEmoji.ts`, `uploadEmojiCustom.ts`); images
  other than gif/x-icon/bmp/webm are resized to fit 512×512. The extension comes from the
  multipart part's MIME type (2.4). Show the normalised name preview before saving.
- Permission: the REST routes have no `permissionsRequired`; `manage-emoji` is checked inside
  (`not_authorized` error otherwise).

---

## 8. Custom sounds

Server: `apps/meteor/server/api/v1/custom-sounds.ts`; constants `apps/meteor/lib/constants.ts`.
Permission `manage-sounds`.

- `GET custom-sounds.list?name=&count=&offset=&sort=` → `{sounds:[…],count,offset,total}`;
  `GET custom-sounds.getOne?_id=` → `{sound:{_id,name,extension,_updatedAt}}` (verified).
- Create: multipart `sound` + `name` → `{"sound":{"_id":"…"}}`; update: multipart `_id`, `name`,
  optional `sound` → `{}`; delete: `POST custom-sounds.delete {_id}` (all verified with a WAV).
- MIME type is derived from the **file name** and must be one of `audio/mpeg`, `audio/mp3`,
  `audio/wav`, `audio/x-wav` (`MIME type not allowed`); maximum 5,242,880 bytes; the extension
  comes from the file name.
- File URL `/custom-sounds/{_id}.{extension}` (verified 200 `audio/wav`); preview playback with
  the desktop sound player (Phase 8).

---

## 9. Custom user statuses

Server: `apps/meteor/server/api/v1/custom-user-status.ts`,
`apps/meteor/server/meteor-methods/users/insertOrUpdateUserStatus.ts`. Permission
`manage-user-status`. All verified:

- `GET custom-user-status.list?name=&count=&offset=` → `{statuses:[{_id,name,statusType,_updatedAt}],…}`.
- `POST custom-user-status.create {name, statusType?}` → `{customUserStatus}` (omitted
  `statusType` is stored as `""`).
- `POST custom-user-status.update {_id, name, statusType?}` → `{customUserStatus}`.
  Re-checked: `name` is **required** (without it: `invalid-params` "must have required property
  'name'"), and an omitted `statusType` is saved as `""` — it **clears** the type (a `busy` status
  updated with only `name` came back with `statusType: ""`). Always send both fields.
- `POST custom-user-status.delete {customUserStatusId}` → `{}`.
- `statusType ∈ online, away, busy, offline`; `name` must not contain `> < & " '`; names are
  unique (`Custom_User_Status_Error_Name_Already_In_Use`).

---

## 10. Integrations (incoming / outgoing webhooks)

Server: `apps/meteor/server/api/v1/integrations.ts`; types
`packages/rest-typings/src/v1/integrations/*.ts`, `packages/core-typings/src/IIntegration.ts`;
web `views/admin/integrations/**`; event table `apps/meteor/app/integrations/lib/outgoingEvents.ts`.

Permissions: `manage-incoming-integrations` / `manage-outgoing-integrations` (all), or the
`manage-own-*` variants (only integrations created by the caller).

### 10.1 Endpoints (all verified; replay only its error path)

| Call | Body / params | Response |
|---|---|---|
| `GET integrations.list` | `name?`, `type?` (`webhook-incoming`/`webhook-outgoing`), `count`, `offset`, `sort` | `{integrations, items, count, offset, total}` |
| `GET integrations.get` | `integrationId`, `createdBy?` | `{integration}` |
| `POST integrations.create` | see 10.2 | `{integration}` (with `token`, `scriptCompiled`) |
| `PUT integrations.update` | see 10.2, plus `integrationId` (required for incoming; for outgoing the schema makes it optional and accepts `target_url` instead — always send `integrationId`); `type`, `name`, `enabled`, `channel`, `scriptEnabled` (+ `username`, `event` for outgoing) are required by the schema; `scriptEngine` is optional (re-checked: updates without it succeeded; the server keeps the stored engine or uses `isolated-vm`); outgoing additionally needs a non-empty `token` (below) | `{integration}` |
| `POST integrations.remove` | `{type:"webhook-incoming", integrationId}` or `{type:"webhook-outgoing", integrationId? , target_url?}` | `{integration}` |
| `GET integrations.history` | `id`, `count`, `offset`, `sort` (outgoing only) | `{history, items, count, offset, total}` |
| `POST integrations.clearHistory` | `{integrationId}` | `{}` (third check, verified on a throwaway outgoing webhook; no stream event, 2.5) |
| `POST integrations.replayOutgoing` | `{integrationId, historyId}` | `{}` (source); unknown `historyId` → `error-invalid-integration-history` (third check, verified) |

Note `integrations.update` is `PUT` (`hooks/useUpdateIntegration.ts`; `POST` → 404, re-checked).
`integrations.list` `name` is a case-insensitive substring.

### 10.2 Fields

Incoming (`IIncomingIntegration`): `enabled`, `name`, `channel` (create/update take a string such
as `#channel` or `@user`; stored as `channel: string[]`), `username` (post as; becomes `userId`),
`alias`, `avatar` (URL), `emoji`, `overrideDestinationChannelEnabled`, `scriptEnabled`,
`scriptEngine` (`isolated-vm`), `script`, `skipTranspile` (deprecated, removed in 9.0); read-only
`token`, `scriptCompiled`, `scriptError {name, message, stack}`, `_createdBy`, `_createdAt`.

Verified create:

```json
{"type":"webhook-incoming","name":"…","enabled":true,"username":"rocket.cat","channel":"#rkadm-r…","scriptEnabled":true,
 "scriptEngine":"isolated-vm","script":"class Script { process_incoming_request({ request }) { return { content: { text: request.content.text } }; } }"}
→ {"integration":{"_id":"…","type":"webhook-incoming",…,"channel":["#rkadm-r…"],"skipTranspile":false,
   "overrideDestinationChannelEnabled":false,"token":"acQq8esq…","userId":"rocket.cat","scriptCompiled":"\"use strict\";…"}}
```

Webhook URL shown and copyable: `{Site_Url}/hooks/{_id}/{token}` (`IncomingWebhookForm.tsx`);
also copy "token" `{_id}/{token}` and a curl example
(`curl -X POST -H 'Content-Type: application/json' --data '{"text":"Example message"}' <url>`,
`hooks/useExampleIncomingData.ts`). Verified that posting to the URL reaches the room (it
answered `You can't send messages because the room is readonly.` for a read-only room).

Outgoing (`IOutgoingIntegration`): `event` (below), `enabled`, `name`, `channel` (only meaningful
when the event uses it, but the create/update schemas require the string, so send `""`
otherwise), `triggerWords[]` (UI: comma-separated, `helpers/triggerWords.ts`),
`triggerWordAnywhere`, `targetRoom` (only `userCreated`), `urls[]` (UI: one per line),
`impersonateUser`, `username`, `alias`, `avatar`, `emoji`, `token`, `scriptEnabled`,
`scriptEngine`, `script`, `runOnEdits`, `retryFailedCalls`, `retryCount`, `retryDelay`
(`powers-of-ten` | `powers-of-two` | `increments-of-two`, `OutgoingWebhookForm.tsx`).

`token` is optional on create but **required (non-empty) on update** — re-checked: an outgoing
webhook created without a token could not be updated (`error-invalid-token`
"Invalid token" from `updateOutgoingIntegration.ts`) until a token was sent. The web pre-fills the
(required) field with a React `useId()` value for new webhooks; Rustket should pre-fill a random
token (e.g. 24+ random alphanumerics) and keep it editable.

| `event` | label key | uses channel | trigger words | target room |
|---|---|---|---|---|
| `sendMessage` | `Integrations_Outgoing_Type_SendMessage` | ✓ | ✓ | |
| `fileUploaded` | `…_FileUploaded` | ✓ | | |
| `roomArchived` | `…_RoomArchived` | | | |
| `roomCreated` | `…_RoomCreated` | | | |
| `roomJoined` | `…_RoomJoined` | ✓ | | |
| `roomLeft` | `…_RoomLeft` | ✓ | | |
| `userCreated` | `…_UserCreated` | | | ✓ |

### 10.3 UI

- List with type icon, name, created by, created at, enabled; "New" offers incoming / outgoing
  (the web also shows Zapier and bot hints, `NewZapier.tsx`, `NewBot.tsx` — skip).
- Script editor: monospace `text_editor`, JS. After saving, show `scriptError` if present (the
  server compiles with Babel unless `skipTranspile`).
- Outgoing history page (`outgoing/history/*`): paginated `integrations.history`, live via
  `stream-integrationHistory [integrationId]`, per item `step`, `event`, `url`, `httpCallData`,
  `httpError`, `httpResult`, `error`, `errorStack`, `finished`, `triggerWord`, `ranPrepareScript`,
  … (`IIntegrationHistory`), buttons "Replay" and "Clear history".
- Tokens and scripts are secrets: no logging.

---

## 11. OAuth apps ("Third-party login")

Rocket.Chat acting as an OAuth2 provider. Server `apps/meteor/server/api/v1/oauthapps.ts`; web
`views/admin/oauthApps/*`. Permission `manage-oauth-apps`. All verified:

- `GET oauth-apps.list` → `{oauthApps: [IOAuthApps]}` (no pagination).
- `GET oauth-apps.get?_id=` or `?clientId=` → `{oauthApp}`.
- `POST oauth-apps.create {name, active, redirectUri}` → `{application:{_id, name, active,
  redirectUri, clientId, clientSecret (43 chars), _createdAt, _createdBy, _updatedAt}}`.
- `POST oauth-apps.update {appId, name, active, redirectUri}` → app fields at the top level.
- `POST oauth-apps.delete {appId}` → bare `true`.

Edit page shows read-only Client ID, Client Secret, Authorization URL `{Site_Url}/oauth/authorize`
and Access Token URL `{Site_Url}/oauth/token` (`EditOauthApp.tsx`), with copy buttons. Multiple
redirect URIs are comma-separated in `redirectUri` (`apps/meteor/server/oauth2-server/oauth.ts`
`client.redirectUri.split(',')`).

---

## 12. Mailer

Server `apps/meteor/server/api/v1/mailer.ts`,
`apps/meteor/server/lib/notifications/mail-messages/functions/sendMail.ts`. Web gate
`access-mailer`; endpoint permission `send-mail`.

`POST /api/v1/mailer {from, subject, body, dryrun?, query?}` → `{}`.

- `body` must contain the literal `[unsubscribe]` placeholder, else
  `error-missing-unsubscribe-link` (verified). `[unsubscribe]` becomes
  `{Site_Url}/mailer/unsubscribe/{_id}/{createdAt}`; other placeholders are those of the e-mail
  templates (`[name]`, `[fname]`, `[lname]`, `[email]` = `Name <address>`, `[Site_Name]`, `[Site_URL]`;
  `apps/meteor/server/lib/utils/placeholders.ts`). Newlines become `<br>`; the body is HTML.
- `from` is validated as an address. `query` is an EJSON Mongo filter on users, ANDed with
  "not unsubscribed" (the UI validates JSON). Re-checked in `sendMail.ts`: `dryrun: true` sends
  one mail to the **user whose e-mail address equals `from`** (not to the caller), and fails with
  `error-invalid-user` when no user has that address; placeholders are filled from that user.
- The web form (`MailerPage.tsx`): From, Dry run toggle, Query, Subject, Email body (multiline).
  The server loads all matching users and queues one mail per user (`setImmediate`) before
  answering, so the response comes back after queuing, not after delivery; success toast
  `The_emails_are_being_sent`.

---

## 13. Permissions and roles

Server: `apps/meteor/server/api/v1/permissions.ts`, `apps/meteor/server/api/v1/roles.ts`,
`apps/meteor/ee/server/api/roles.ts`; setting-permission ids
`apps/meteor/app/authorization/lib/index.ts`; web `views/admin/permissions/**`.

### 13.1 Data

- `GET permissions.listAll[?updatedSince=]` → `{update, remove}` (verified: 1093 permissions;
  174 ordinary, 919 with `level: "settings"`). Ordinary: `{_id, roles[], _updatedAt}`. Setting
  permissions: `{_id: "change-setting-<settingId>", level: "settings", settingId, group,
  groupPermissionId: "change-setting-<group>", section?, sectionPermissionId?, sorter, roles}`.
- `GET roles.list` → `{roles: [{_id, name, scope: "Users"|"Subscriptions", description,
  mandatory2fa, protected}]}` (verified, 12 built-in roles, all `protected`). `roles.sync
  ?updatedSince=` → `{roles: {update, remove}}`.
- `GET roles.getUsersInRole?role=&roomId=&count=&offset=` → `{users:[{_id, name, username,
  emails, avatarETag?, createdAt, _updatedAt}], total}` (verified; sorted by `username`, default
  `count` 50; `roomId` for Subscriptions-scoped roles and needs `view-other-user-channels`).

### 13.2 Matrix UI (`PermissionsPage.tsx`, `PermissionsTable/*`)

- Two tabs: "Permissions" (`level != "settings"`) and "Setting permissions" (`level ==
  "settings"`, needs `access-setting-permissions`). Rows sorted by `_id`, 25 per page.
- Columns: one per role. Cell: checkbox "role ∈ permission.roles".
- Row label: `t(_id)` for ordinary permissions; for setting permissions the setting label.
  Filter (`helpers/mapPermissionKeys.ts`): split the filter into words; a permission matches when
  every word is a case-insensitive substring of `_id` or of the joined translated labels
  `[t(group), t(section), t(settingId), t(_id)]`.
- Toggle: `POST permissions.addRole {permissionId, role}` / `POST permissions.removeRole
  {permissionId, role}` (atomic; what the web uses). Errors: `error-action-not-allowed`
  "Permission is restricted" (restricted role/permission pairs, e.g. guest without license),
  `error-invalid-permission`, `error-invalid-role`. Toggling a setting permission needs
  `access-setting-permissions` in addition to `access-permissions`, and also adds/removes the role
  on the permission's `groupPermissionId` (`change-setting-<group>`) — refetch or rely on
  `permissions-changed` for both rows (`server/lib/authorization/permissionRole.ts`).
  `POST permissions.update {permissions: [{_id, roles[]}]}` replaces whole role arrays
  (race-prone; avoid). UNVERIFIED: permission mutations were not called (global).
- Role header click → role editor; "Users in role" page with add (`roles.addUserToRole {roleId,
  username, roomId?}`) and remove (`roles.removeUserFromRole {roleId, username, roomId?/scope?}`).
  In 8.9 these take `roleId` (verified), older servers used `roleName`.

### 13.3 Role CRUD

- `POST roles.create {name, scope?, description?, mandatory2fa?}` → `{role}` — EE only
  (`license: ['custom-roles']`); duplicate names → `error-duplicate-role-names-not-allowed`. The
  web shows `CustomRoleUpsellModal.tsx` without the license.
- `POST roles.update {roleId, name, scope?, description?, mandatory2fa?}` → `{role}` — allowed
  in CE **only for protected roles** (description, mandatory 2FA; the name of a protected role
  is fixed by `updateRole`); non-protected roles need `custom-roles`. Pitfall
  (`ee/server/api/roles.ts`): the handler substitutes `scope || 'Users'` and
  `description || ''`, and `ee/server/lib/roles/updateRole.ts` rejects a changed name or scope of
  a protected role with `error-role-protected` — so always send the role's current `name`,
  `scope` and `description`; e.g. updating `owner`/`moderator`/`leader` (scope `Subscriptions`)
  without `scope` fails, and omitting `description` clears it.
- `POST roles.delete {roleId}` → `{}`; `error-role-protected`, `error-role-in-use`
  (`roles.ts` lines ~216–257).
- Permission for all of them: `access-permissions`. UNVERIFIED: no role CRUD call was made (global changes).

Keep the workspace permission store current from `permissions-changed` so the rest of the app
re-evaluates `has_permission` immediately.

---

## 14. Generic settings editor

This is the largest piece. The web implementation: `client/providers/SettingsProvider.tsx`
(load + save), `client/cachedStores/PrivateSettingsCachedStore.ts` (load + stream),
`client/views/admin/EditableSettingsContext.ts` (structure, sorting, query evaluation),
`client/views/admin/settings/**` (pages, sections, inputs).

### 14.1 Where the metadata comes from

`GET /api/v1/settings` is **not enough**: it always projects `{_id, value, enterprise,
invalidValue, modules}` (+ `packageValue` with `includeDefaults=true`), and its `fields` param is
ignored without the unsafe env var (`apps/meteor/server/api/v1/settings.ts` `fetchSettings`,
2.2). Verified: `GET /api/v1/settings?count=3` → `{settings:[{_id, enterprise, invalidValue,
modules, value}], count, offset, total: 919}`.

The full documents come from the Meteor method **`private-settings/get`**
(`apps/meteor/server/publications/settings/index.ts`), the same call the web makes through
`CachedStore` (`sdk.call('private-settings/get')`, `client/lib/cachedStores/CachedStore.ts`):

- Params `[]` → plain array of all non-hidden settings, **including** `type: "group"` records.
  Params `[{"$date": ms}]` → `{update: [...], remove: [{_id, _deletedAt}]}` changed after that
  time (verified, returned `{update:[], remove:[]}`).
- Authorisation: `view-privileged-setting` or `edit-privileged-setting` → everything;
  otherwise `manage-selected-settings` → only settings whose `change-setting-<id>` permission the
  user has; otherwise `[]` (no error).
- Works over DDP (`Session::call`) and over REST `POST /api/v1/method.call/private-settings%2Fget`
  with body `{"message": "<EJSON of {msg:'method', id, method, params}>"}` (verified via REST: 919
  entries, ~650–710 KB of JSON, the result is a string in `message` that must be parsed again).
  Dates are EJSON `{"$date": ms}` → `RcDate`.
- **Re-checked — EE values are NOT masked here.** `private-settings/get` reads the DB and, unlike
  `public-settings/get` and REST `GET /api/v1/settings`, does **not** emit the `fetch-settings`
  event on which `apps/meteor/ee/server/settings/settings.ts` replaces values with `invalidValue`.
  Live on CE: 8 EE settings come back with their stored value although it differs from
  `invalidValue` (e.g. `Federation_Service_EDU_Process_Typing` `true` vs `invalidValue: false`,
  `AI_LLM_OpenAI_Base_URL`, `Push_request_content_from_server`), while `GET /api/v1/settings`
  returns `invalidValue` for the same ids and `GET settings/:_id` returns the stored value. The
  *effective* server value of an EE setting without its module is `invalidValue`; the web shows
  the stored value (disabled, "Premium"). Rustket: show the stored value disabled and, when it
  differs, a hint with the effective `invalidValue`.
- Keep it current with `private-settings-changed` (2.5): merge the received fields into the
  record with the same `_id`, then fetch the delta `private-settings/get([{"$date": <max
  _updatedAt seen>}])` and replace the touched records with the full documents (that is what
  `PrivateSettingsCachedStore.setupListener` + `CachedStore.sync` do). Third check: the event
  payload is partial and carries the license-masked value of EE settings, so merging alone would
  show `invalidValue` where the initial load showed the stored value, and new settings would lack
  their metadata. Debounce the delta fetch (bursts of events arrive after a bulk save).
- Third check of the "otherwise `[]`" rule above: as `alice` (none of the three permissions)
  `method.call/private-settings%2Fget` answered `{"result":[]}`, no error.

Live inventory (dev server): 919 records — `boolean` 345, `string` 291, `int` 78, `select` 63,
`group` 38, `code` 34, `asset` 19, `password` 13, `relativeUrl` 11, `action` 8, `range` 5,
`color` 4, `timespan` 3, `lookup` 2, `multiSelect` 2, `roomPick` 1, `language` 1, `timezone` 1.
`font` and `date` exist in the type union but no setting uses them.

### 14.2 Fields (`packages/core-typings/src/ISetting.ts`)

| Field | Live count | Meaning for the editor |
|---|---|---|
| `_id` | 919 | key; also the fallback label and the tooltip |
| `type` | 919 | widget (14.5) |
| `group` | 878 | group id (a record with `type: "group"` and the same `_id`) |
| `section` | 614 | section name (an i18n key or free text like `Custom OAuth: Keycloak`) |
| `tab` | 73 | sub-tab inside a group (LDAP: `LDAP_Connection`, `LDAP_DataSync`, `LDAP_UserSearch`; SAML: `SAML_Connection`, `SAML_General`) |
| `i18nLabel` | 919 | label key |
| `i18nDescription` | 919 | hint key (exists in `en.json` for 374 of them) |
| `sorter` | 919 | primary sort key (−1…34002) |
| `value`, `packageValue` | 881 | current and default value (groups have none) |
| `valueSource` | 881 | `packageValue` or `processEnvValue` (13 settings, e.g. `SMTP_Host`, set by env; edits are overwritten on restart — show a note) |
| `processEnvValue` | 13 | value from the environment |
| `blocked` | 919 | listed in the `SETTINGS_BLOCKED` env var (`apps/meteor/server/settings/SettingsRegistry.ts`): always disabled (none on dev). The server does not reject a save of a blocked setting: `Settings.updateValueById` filters `blocked: {$ne: true}`, so it silently succeeds without changing anything. (The web computes `disabled` from `blocked` in the store but `Setting.tsx` then overrides it with the `enableQuery` result only, so blocked widgets are in fact editable there — a web bug; Rustket must disable them.) |
| `hidden` | 919 | hidden settings are not returned at all |
| `readonly` | 32 | display only (e.g. `Show_Setup_Wizard`, OAuth callback URLs, `Enterprise_License_Status`) |
| `enableQuery` | 373 | disables the widget unless satisfied (14.4) |
| `displayQuery` | 8 | hides the widget unless satisfied (LDAP AD vs. OpenLDAP fields) |
| `enterprise`, `modules`, `invalidValue` | 881/56/65 | EE gating (14.6) |
| `values` | 65 | options of `select`/`multiSelect`: `[{key, i18nLabel}]` (all keys are strings live) |
| `multiline` | 55 | textarea for `string` / `code` (present on 26 `string` + 29 `code`; `true` on 25 `string` + 29 `code` — third check) |
| `placeholder` | 5 | input placeholder (plain text, not a key) |
| `code` | 28 | editor mode of `code`: `css`, `text/css`, `text/html`, `text`, `application/json` |
| `editor`, `packageEditor` | 2/4 | `color` settings: `color` or `expression` |
| `minValue`, `maxValue` | 5 | `range` bounds |
| `actionText` | 8 | button label key of `action` |
| `lookupEndpoint` | 2 | `v1/video-conference.providers`, `v1/ai.llm.models` |
| `asset`, `fileConstraints` | 19 | `asset` name and `{type: "image", extensions: [...]}` |
| `alert` | 54 (39 distinct keys) | i18n key of a warning callout + save confirmation (14.7) |
| `secret`, `public`, `env`, `autocomplete`, `requiredOnWizard`, `wizard`, `persistent`, `private`, `validation`, `createdAt`, `ts`, `_updatedAt` | – | not needed for rendering; `autocomplete: false` → disable autofill; `validation` is server-side (14.7) |

Model notes: `enableQuery`/`displayQuery` are JSON **strings** on 8.9 (all 373 live values), but
the type allows an object or an array — deserialize "string containing JSON, or the JSON itself".
`group`/`section` may be `null`. Unknown `type` values must not fail decoding (render as a
read-only text field).

### 14.3 Building the group list and a group page

Group list (`settings/hooks/useSettingsGroups.ts`, `SettingsPage.tsx`):

1. Groups are the records with `type == "group"` (38 live), sorted by translated label
   (`t(i18nLabel || _id)`, locale-aware compare). Each card shows `t(i18nLabel)` and
   `t(i18nDescription)`.
2. Search (debounced 400 ms): build `Regex::new("(?i)" + filter)`; if the pattern is invalid,
   fall back to a prefix match. A setting matches when `t(i18nLabel)`, `t(_id)` or `_id`
   matches. The visible groups are the groups of matching settings (or matching group records).
3. Settings whose `group` has no group record are never reachable (live: 16 `AI_Center` settings
   — that page is a separate admin screen — and 3 with no group: `uniqueID`,
   `Deployment_FingerPrint_Hash`, `Deployment_FingerPrint_Verified`). The web also drops
   `Cloud_Workspace_AirGapped_Restrictions_Remaining_Days` (`defaultOmit` in
   `EditableSettingsProvider.tsx`).

Group page (`EditableSettingsContext.ts`, `groups/BaseGroupPage.tsx`, `TabbedGroupPage.tsx`,
`GenericGroupPage.tsx`, `SettingsSection/SettingsSection.tsx`):

```text
members  = settings where group == G (not the group record itself, not omitted)
sort key = (sorter, tab ?? "", i18nLabel)            // compareSettings; i18nLabel is the raw key
tabs     = distinct (tab ?? "") of sorted members, in order
if tabs.len() > 1: show tabs; label(tab) = t(tab), label("") = t(G)
sections(tab) = distinct (section ?? "") of sorted members with that tab, in order
section title  = t(section); "" has no title; a section is non-collapsible when it is "" or the only one
settings(section, tab) = members with that section and tab, sorted
```

Sorting uses `String.prototype.localeCompare` for `tab` and `i18nLabel` (`compareSettings`), not a
byte compare; any stable case-insensitive compare is fine. Third check: the web's section lookup
for the `""` tab of a tabbed group is broken (`useEditableSettingsGroupSections(_id, '')` filters
`x.tab === ''`, which never matches an untabbed setting), so untabbed settings of a tabbed group
would show nothing there; no live group mixes tabbed and untabbed settings (LDAP 46/46, SAML 27/27
tabbed). Rustket should treat a missing `tab` as `""` consistently.

Page header: back button, `t(group.i18nLabel)`, optional header buttons (special groups,
14.10), optional description paragraph `t(group.i18nDescription)` when the key exists. Footer:
"Cancel" (only when something changed) and "Save changes" (`Save_changes`).

### 14.4 `enableQuery` / `displayQuery`

`performSettingQuery` in `EditableSettingsContext.ts`: the query is one filter object or an array
of them; it is satisfied when **every** filter matches **at least one** setting document, using
Mongo filter semantics (`createPredicateFromFilter` from `@rocket.chat/mongo-adapter`). Filters
reference the setting document, e.g. `{"_id":"LDAP_Enable","value":true}` or
`{"_id":"Accounts_Password_Policy_Enabled","value":true,"public":true}`.

Important: the web evaluates the queries against the **edited** (unsaved) state
(`useEditableSettingVisibilityQuery`), so toggling `LDAP_Enable` enables the dependent fields at
once. Rustket must do the same.

Operators used live: equality, `$ne` (10), `$exists` (7), `$in` (2). Implement a small evaluator
(`rustket-core::admin::mongo_filter`) with: field equality (deep JSON equality; if the field is
an array, also "array contains value"), `$eq`, `$ne`, `$in`, `$nin`, `$exists`, `$gt`, `$gte`,
`$lt`, `$lte`, `$and`, `$or`, `$nor`, `$not`, dotted paths. An unknown operator evaluates to
`true` (do not lock the admin out) and is logged at debug level. Unit-test it with every live
query (fixture from `private-settings/get`).

### 14.5 Widget per `type`

Value JSON type, widget, and the server's check in `saveSettingsBulk`
(`apps/meteor/server/settings/lib/saveSettingsBulk.ts`). Web inputs:
`settings/Setting/inputs/*SettingInput.tsx`, mapping in `Setting/MemoizedSetting.tsx`.

| `type` | Value | Widget (web behaviour) | Server check |
|---|---|---|---|
| `boolean` | bool | toggle; label left, switch right | `Boolean` |
| `string` | string | single-line input; `multiline` → 4-row textarea; `placeholder`; `readonly` → read-only | `String` |
| `relativeUrl` | string (relative, e.g. `_oauth/drupal`) | URL input showing the absolute URL `Site_Url + value`. Third check: all 11 live ones are `readonly` OAuth callback URLs — show absolute, copy button. (The web input displays `getAbsoluteUrl(value)` but stores whatever is typed, so an editable one would save an absolute URL; if Rustket ever edits one, edit the relative value.) | `String` |
| `password` | string | masked input with reveal; `autocomplete: false` → no autofill | `String` |
| `int` | integer | numeric input (`parseInt`); block NaN/empty locally | `Number`, `Number.isInteger`, `checkSettingValueBounds` |
| `select` | string | dropdown of `values` labelled `t(i18nLabel)`; the web always saves `String(key)` | `String` |
| `multiSelect` | array of strings | multi-select (filterable when `autocomplete`) labelled `t(i18nLabel)` | `Array` |
| `language` | string (`""` = Default) | dropdown: "Default" + languages (14.12) | `String` |
| `color` | string | `editor == "color"` → colour picker + hex; `"expression"` → text; hint `Variable name: ` + `_id` with the first `theme-color-` replaced by `@` (`_id.replace(/theme-color-/, '@')`; ids without that prefix — all 4 live ones — are shown unchanged, no `@`). The web never passes `allowedTypes`, so the editor switch is effectively fixed. Two live colour settings (`CAS_button_color`, `CAS_button_label_color`) have no `editor` and the web renders **no input** for them — Rustket: hex picker when the value is `#rrggbb`, else text. Save sends `{_id, value, editor}` | `String` (+ `editor` saved) |
| `font` | string | text input | `String` |
| `code` | string | monospace multiline editor (`text_editor`); `code == "application/json"` → must be `""` or valid JSON, else error `Invalid_JSON` (Rustket: its own key `admin.settings.error.invalid_json`, like the other client-side checks, since `error-invalid-setting-value` is not in the server's `en.json`) and Save disabled (`client/lib/utils/getCodeSettingError.ts`) | `String`, JSON validity for `application/json` |
| `action` | method name string, or `{method, path}` | button `t(actionText)`, disabled while the section has unsaved changes (hint `Save_to_enable_this_action`); never saved (14.8) | – |
| `asset` | `{url?, defaultUrl?}` | preview + "Select file"/"Delete", immediate upload (14.9); no reset button | – |
| `roomPick` | `[{_id, name?}]` or `""` | multi room picker; the web uses `GET rooms.autocomplete.channelAndPrivate?selector={"name":"<text>"}` → `{items:[{_id,fname,name,t}]}` (verified) and saves `[{_id}]`. Third check: that endpoint only searches rooms the **caller is subscribed to** (max 10, no discussions; `findChannelAndPrivateAutocomplete`) — live, `gen` found nothing for `admin` while `rooms.autocomplete.adminRooms?selector={"name":"gen"}` returned `general`. Rustket may use `rooms.autocomplete.adminRooms` (needs `view-room-administration`) to pick any room; resolve stored ids to names via `rooms.adminRooms.getRoom` | `Object` or `""` |
| `group` | – | never rendered as a field | – |
| `date` | – | generic text input (unused) | `String` |
| `lookup` | string | dropdown filled from `GET /api/<lookupEndpoint>` → `{data:[{key,label}]}`; labels are **not** translated (verified both endpoints → `{"data":[],"success":true}` on dev) | `String` |
| `range` | integer | slider `minValue`..`maxValue` (defaults 0..100), shown with the value | `Number`, integer, bounds |
| `timespan` | integer milliseconds | number + unit select (days/hours/minutes). Initial unit (`getHighestTimeUnit`): minutes if not a whole number of hours, hours if not a whole number of days, else days. Input clamped to a non-negative integer; saved as `n × unit` ms (`client/lib/convertTimeUnit.ts`) | `Number`, integer |
| `timezone` | string (IANA name or `""`) | dropdown of `UTC` + IANA names (web: `getTimezoneNames()`); in Rust use the `chrono-tz` name list (pure Rust) | `String` |

### 14.6 Labels, hints, disabled state, reset

Per setting (`Setting/Setting.tsx`):

- **Label** = `t(i18nLabel)` if the key exists, else `t(_id)` if it exists, else the raw
  `i18nLabel`, else `_id` (885/919 labels exist in `en.json`; misses are literal texts such as
  `logo (svg, png, jpg)`). Tooltip = `_id`.
- **Hint** = `t(i18nDescription)` only when the key exists, rendered as **inline Markdown**
  (`MarkdownText variant='inline'`): code spans, bold, links (14 descriptions have links; relative
  links like `admin/settings/FileUpload` must navigate inside the admin screen). Do **not**
  interpolate `{{…}}` in descriptions (the SAML mapping description contains literal
  `{{firstName}}`).
- **Alert** = `t(alert)` (or the raw text) in a warning callout below the widget; re-checked: the
  alerts of 13 settings contain markup — `<br/>`, `<b>`, `<strong>`, `<ul>`, `<li>` and also `<a>`.
  Third check: that is 12 distinct alert values, not 6 (only `Old_Federation_Alert` is shared, by
  `FEDERATION_Enabled` and `Federation_Matrix_enabled`), and one of them is not a key at all but a
  literal English sentence with an `<a href>` (`RetentionPolicy_Enabled`). The web's `Trans` maps only b/strong/br/ul/li; map those to rich text,
  render `<a>` as a link if cheap, drop other tags.
- **Disabled** when any of: `blocked`; `enableQuery` not satisfied (edited state); EE setting
  without license (below). `readonly` → read-only input / disabled toggle.
- **Invisible** when `displayQuery` is not satisfied (edited state).
- **EE**: the web disables a setting with `enterprise: true` unless the workspace has a license
  and all `modules` are in `licenses.info.license.activeModules`
  (`settings/hooks/useHasSettingModule.ts`; note it returns false for an EE setting with an empty
  `modules` list, so such a setting would stay disabled even with a license — all 56 live EE
  settings have modules), and shows a "Premium" tag and a pricing link. The displayed value is the
  **stored** value, not `invalidValue` (re-checked, 14.1).
- **Reset button** (per setting) when: not EE-disabled, not `readonly`, `type != asset`, not
  disabled, and `JSON(value) != JSON(packageValue)` (or `editor != packageEditor` for colours).
  It sets the edited value to `packageValue` (and `editor` to `packageEditor`); it does not save.
- **Section "Restore defaults"** (`Reset_section_settings`): shown unless the group page passes
  `hasReset=false` (the `Assets` group) and when any setting of the section differs from its
  `packageValue`; sets every non-disabled setting of the section to `packageValue`.
- **Changed** = `JSON(edited) != JSON(persisted)` (or colour editor differs). The save bar and
  per-section "changed" flag derive from it.

Remote changes: the web's `sync` overwrites every editable entry with the new persisted values
and resets `changed`, i.e. it silently drops unsaved edits whenever any setting changes. Rustket
should update only entries the user has not changed, and mark changed entries whose persisted
value moved underneath ("changed on the server") so the user decides.

### 14.7 Saving

1. Collect the changed settings of the **whole group** (all tabs and sections):
   `[{_id, value}]`, colour settings `[{_id, value, editor}]` (`SettingsGroupPage.tsx` `save`).
2. Abort if a `code` JSON setting is invalid.
3. If any changed setting has `alert`, show a confirmation modal (`Confirm_settings_change`)
   listing each label with its alert text; cancel aborts (`client/hooks/useShowSettingAlerts.tsx`).
4. `POST /api/v1/settings {settings: [{_id, value, editor?}]}` (min 1 item;
   `packages/rest-typings/src/v1/settings.ts` `SettingsBulkProps`). 2FA **every time**
   (`twoFactorOptions: {disableRememberMe: true}`). Success → `{}`; toast `Settings_updated`.
5. Errors:
   - `error-action-not-allowed` "Editing settings is not allowed" with
     `details.settingIds` (no `edit-privileged-setting` and not both `manage-selected-settings`
     and `change-setting-<id>`, `saveSettingsBulk.ts`; also
     `Custom_Script_*` in cloud trials).
   - `error-setting-validation-failed` (cross-setting rules in
     `apps/meteor/server/lib/settingValidationRules.ts`, e.g. password min/max length; the rules
     are in the settings' `validation` field — live only `Accounts_Password_Policy_MinLength` /
     `_MaxLength` — and may reference other settings with `{"$setting": "<id>"}`; some
     `application/json` code settings are also checked against a JSON schema
     (`getSettingSchemaValidator`). Do not reimplement. Third check: the body is
     `{"success":false,"error":"<settingId>_Invalid","errorType":"error-setting-validation-failed"}`
     — the `error` text is an i18n key built from the first failing setting's id (source), e.g.
     `Accounts_Password_Policy_MinLength_Invalid`, which exists in `en.json`; show
     `ServerI18n::t(error)`, falling back to the raw key).
   - Meteor `check` failures (wrong JSON type; every type not listed in 14.5's checks, including
     `select`, `language`, `lookup`, `timezone`, `color`, `password`, must be a string),
     `error-invalid-setting-value` (non-integer `int`/`range`/`timespan`, or `int`/`range` outside
     `minValue`/`maxValue` — bounds are skipped for the value `0`), `Invalid JSON provided` (a
     `code` setting with `code: "application/json"`). UNVERIFIED: the exact body of a Meteor
     `check` failure; from source it should be HTTP 400 `{"success":false,"error":"Match error:
     Expected string, got number"}` without `errorType` (`Match.Error` has no `.error` field).
   - All type, permission and validation checks run for every item before the first write
     (`saveSettingsBulk`: `Promise.all` of checks, then `validateSettingRules`, then the updates),
     so a rejected save changes nothing; the writes themselves are not atomic. An unknown `_id`
     with a string value passes the checks and "succeeds" without writing anything (verified by
     the admin library's live test: `{}` with the password code, `GET settings/:_id` still
     fails afterwards; a wrong password hash answered `totp-invalid`). Even that no-op is not
     free: `updateAuditedByUser` records a `settings.changed` audit event for every accepted
     item, existing or not, so the live test now probes only the 2FA-required and wrong-code
     paths, which stop before any write.
   - Route-level: `POST settings` has no permission gate of its own (any logged-in user reaches
     the 2FA prompt first; the per-setting check above follows — third check: `alice` got
     `totp-required` for a bulk save). Body schema errors come before 2FA (2.1).
6. Side effect: saving `Organization_Name` also sets `Site_Name` when it still had its default or
   equalled the old organization name.
7. UNVERIFIED: no successful bulk save of an existing setting was made (would change global
   settings); only the 2FA-every-time behaviour, `totp-invalid` and the unknown-id no-op were
   probed (2.3).

Single-setting endpoints (not used by the web editor): `GET /api/v1/settings/:_id` →
`{_id, value}` (`view-privileged-setting`), `POST /api/v1/settings/:_id {value}` or
`{value, editor}` or `{execute: true}` (`edit-privileged-setting`, 2FA, remembered). Unlike the
bulk endpoint it does no JSON type check of `value` (only bounds and validation rules), so do not
use it for editing.

Read-only mode: a user with `view-privileged-setting` but neither `edit-privileged-setting` nor
`manage-selected-settings` plus the setting's `change-setting-<id>` sees the editor but every widget disabled and no save bar.

### 14.8 Action settings

Live: `Push_test_push` `{method:"POST", path:"/v1/push.test"}` (endpoint form) and the method
form `sendSMTPTestEmail`, `restart_server`, `OEmbedCacheCleanup`, `crowd_test_connection`,
`crowd_sync_users`, `resetIrcConnection`, `removeSlackBridgeChannelLinks`.

- Endpoint form (`EndpointActionInput.tsx`): call `method` on `/api` + `path` with an empty body
  `{}` (e.g. `POST /api/v1/push.test`).
- Method form (`MethodActionInput.tsx`): DDP method call `value` with no params (REST fallback
  `method.call/<name>`). The server alternative `POST settings/:_id {execute: true}` needs
  `edit-privileged-setting` and 2FA, so prefer the method.
- Result `{message, params?}` → success toast `t(message)` with `%s` substituted by `params`
  (sprintf); errors → error toast. `restart_server` must ask for confirmation first.

### 14.9 Assets

`AssetSettingInput.tsx`, server `apps/meteor/server/api/v1/assets.ts`, permission
`manage-assets`. Applied immediately, not through Save:

- Upload: `POST /api/v1/assets.setAsset` multipart `asset` (file), `assetName` (= the setting's
  `asset`, e.g. `logo`, `favicon_512`), optional `refreshAllClients`. The file picker filters by
  `fileConstraints.extensions`.
- Delete: `POST /api/v1/assets.unsetAsset {assetName, refreshAllClients?}`.
- Preview: `value.url` relative to the server (fetch with the image cache; append a cache
  buster after upload); without `url` show an upload placeholder (`value.defaultUrl` is the
  built-in default).
- UNVERIFIED: asset upload/delete not called (global).

### 14.10 Groups with their own page (`settings/SettingsGroupSelector/SettingsGroupSelector.tsx`)

- **OAuth** (`groups/OAuthGroupPage/OAuthGroupPage.tsx`): header buttons "Refresh OAuth
  services" (`POST settings.refreshOAuthServices`) and "Add custom OAuth" (modal for a name →
  `POST settings.addCustomOAuth {name}`); sections named `Custom OAuth: <Name>` get a help text
  `t("Custom_oauth_helper", {url: Site_Url + "_oauth/" + lowercase(name)})` (contains `<pre>`)
  and a "Remove custom OAuth" button (`POST settings.removeCustomOAuth {name: lowercase}` after a
  danger confirmation). All three need `add-oauth-service` and 2FA. On the shared dev server never
  remove or disable `Accounts_OAuth_Custom-Keycloak` (CLAUDE.md).
- **LDAP** (`groups/LDAPGroupPage.tsx`): header buttons "Test connection", "Test LDAP search",
  "Sync now", docs link; the first three are disabled unless `LDAP_Enable` (persisted) is on and
  nothing is changed (section 17).
- **SAML** (`groups/SAMLGroupPage/*`): adds a "metadata" modal. Re-checked: it posts the IdP
  metadata **URL**, not the XML — `POST saml.parseMetadata {url}` (`test-admin-options`;
  `apps/meteor/server/api/v1/saml.ts`): the server fetches the URL (SSRF-checked) and answers
  `{entryPoint?, idpSLORedirectURL?, cert?, identifierFormat?, warnings: []}`; errors
  `SAML_Metadata_url_blocked`, `SAML_Metadata_fetch_failed`, `SAML_Metadata_too_large`,
  `SAML_Metadata_invalid`. The client then writes the values into the *edited* state of
  `SAML_Custom_Default_entry_point`, `…_idp_slo_redirect_url`, `…_cert`, `…_identifier_format`
  (toast `SAML_Metadata_no_values` if none); nothing is saved until "Save changes". Third check:
  `{url: "http://127.0.0.1:1/metadata.xml"}` → HTTP 400 `{"success":false,"error":
  "SAML_Metadata_url_blocked"}` (SSRF check, nothing fetched). UNVERIFIED: the success shape (no
  IdP metadata URL on dev).
- **Assets**: normal page without section reset.
- **Enterprise**: a single section titled `Manual_license_management_deprecated` with a note that
  the license is managed on the Subscription page; saving `Enterprise_License` should also
  refresh `licenses.info`.

### 14.11 Permission checks before showing the editor

Show "Settings" when the user has any of `view-privileged-setting`, `edit-privileged-setting`,
`manage-selected-settings` (`SettingsProvider.tsx` `settingsManagementPermissions`). With only
`manage-selected-settings` the method returns just the permitted settings, so groups appear
partially; that is expected.

### 14.12 English (and Russian) strings for labels — server i18n

The web bundles `en` and fetches other languages from the server
(`client/providers/TranslationProvider.tsx`: `loadPath: 'i18n/{{lng}}.json'`). The server route
is `apps/meteor/server/routes/i18n.ts`: `GET /i18n/<lng>.json` serves the asset
`i18n/<lng>.i18n.json` (source: `packages/i18n/src/locales/<lng>.i18n.json`, 68 languages:
`af ar az be-BY bg bn-BD bn-IN bs ca cs cy da de-AT de-IN de el en eo es et eu fa fi fr gl he
hi-IN hi hr hu id it ja ka-GE km ko ku lo lt lv mn ms-MY nb nl nn pa-IN pl pt-BR pt ro ru se si
sk-SK sl-SI sq sr sv ta-IN th-TH tl tr ug uk vi-VN zh-HK zh-TW zh`). **No authentication
needed.** Unknown language → HTTP 400 with an empty body.

Verified live:

- `/i18n/en.json` 200 `application/json`, a **flat** object of 7549 keys
  (`"Hide_System_Messages": "Hide system messages"`, `"Reset_section_settings": "Restore
  defaults"`, …). `/i18n/ru.json` has 4942 keys (fall back to `en` per key).
- **Server bug:** the handler sets `Content-Length` to the JavaScript string length (UTF-16 code
  units), not the byte length, so without compression the body is cut short (533,283 of 533,423
  bytes for `en`; JSON parse fails at the end) and the extra bytes are left on the keep-alive
  connection (curl closed and reopened the connection). With `Accept-Encoding: gzip` the response
  is chunked and complete. Rustket's reqwest has the `gzip` feature (`crates/rocketchat/Cargo.toml`),
  which sends `Accept-Encoding: gzip` automatically — do not disable it for this request; treat a
  JSON parse error as "translations unavailable" and fall back.
- Key formats: some keys carry a namespace prefix (`cloud.…`, `_joined.comment`); 480 values use
  i18next `{{var}}` interpolation; 6 use sprintf `%s` (action results); 56 plural keys use
  `_one`/`_other` suffixes; some values contain `<1>…</1>` component markup or HTML (`<br/>`,
  `<pre>`).
- Coverage of setting metadata in `en.json` (re-checked): labels 885/919 (887 with the `_id`
  fallback), descriptions 374/919, distinct sections 86/116 (the rest are literal names such as
  `Custom OAuth: Keycloak`), distinct alerts 37/39.

Server-side overrides: the public setting `Custom_Translations` (type `code`, JSON
`{"<lng>": {"key": "text"}}`, empty on dev) is applied on top by the web
(`TranslationProvider.tsx` `useCustomTranslations`) — apply it too.

Apps have their own strings: `GET /api/apps/languages` → `{apps: [{id, languages: {<lng>:
{key: text}}}]}` (verified, empty list on dev) or `GET /api/apps/:id/languages`; the web namespaces
them as `app-<appId>.<key>` (`apps/meteor/ee/lib/misc/Utilities.ts` `getI18nKeyForApp`).

Lookup chain used everywhere in the admin screen (`rustket-core::admin::ServerI18n::t`):
`Custom_Translations[lang]` → server file for the UI language (`en` or `ru`) → server `en` →
not found. Callers apply their own fallback (raw key / `_id`, 14.6). Cache the parsed files per
server version (`info.version`) and language in memory; optionally on disk under the cache dir
(they are public data). The `language` setting widget lists the 68 codes above with English names
from a small static table, plus "Default" (`""`).

Rustket's own UI strings (buttons, page titles, errors it generates) stay in the rust-i18n layer
(`crates/rustket/locales/admin.yml`, en + ru); only server-defined metadata (setting
labels/hints/alerts/sections/groups/select options/action texts, permission names, outgoing event
labels, server error keys) go through `ServerI18n`.

---

## 15. Apps management (`/api/apps`)

Server: `apps/meteor/ee/server/apps/communication/rest.ts` and `…/communication/endpoints/*.ts`;
formatter `apps/meteor/ee/lib/misc/formatAppInstanceForRest.ts`; statuses
`packages/apps-engine/src/definition/AppStatus.ts`; app setting types
`packages/apps-engine/src/definition/settings/{ISetting,SettingType}.ts`. Base path `/api/apps`
(2.1). Most mutating and per-app routes need `manage-apps` (third check: so does
`bundles/:id/apps`, and `logs`, `:id/logs`, `:id/export-logs`); re-checked in `rest.ts`: `installed`,
`marketplace`, `categories`, `featured-apps`, `:id/versions` need only a login, and `count`,
`languages`, `actionButtons`, `externalComponents`, `:id/icon`, `:id/screenshots`,
`:id/languages` need no login at all (verified `count` without a token).

| Call | Notes | Verified (`no`/`–` = UNVERIFIED, source only) |
|---|---|---|
| `GET /api/apps/installed[?includeClusterStatus=true]` | `{apps: [IAppInfoRest]}` | yes (`{"apps":[]}`) |
| `GET /api/apps/count` | `{totalMarketplaceEnabled, totalPrivateEnabled, maxMarketplaceApps, maxPrivateApps}` | yes (`0, 0, 5, 0`) |
| `GET /api/apps/:id` | one app (or marketplace info with `?marketplace=true&version=`) | – |
| `POST /api/apps` | install: JSON `{appId, marketplace: true, version, permissionsGranted?}` (needs cloud registration), JSON `{url}` (zip), or multipart `app` + `permissions` (private app; limited by `license.limits.privateApps`, 0 on CE) | no (global) |
| `POST /api/apps/:id` | update (same body forms) | no |
| `DELETE /api/apps/:id` | uninstall | no |
| `GET/POST /api/apps/:id/status` | `{status}`; POST `{status: "manually_enabled"|"manually_disabled"}` → `{status}` | no |
| `GET /api/apps/:id/settings` | `{settings: {<id>: ISetting}}` (hidden ones removed) | no |
| `POST /api/apps/:id/settings` | `{settings: [ISetting…]}` → `{updated}` (only changed values) | no |
| `GET/POST /api/apps/:id/settings/:settingId` | single setting; POST `{setting}` | no |
| `GET /api/apps/:id/logs`, `GET /api/apps/logs`, `GET /api/apps/:id/export-logs` | paginated logs (`{logs, count, offset, total}`) / file export | no |
| `GET /api/apps/:id/apis` | app HTTP endpoints | no |
| `GET /api/apps/:id/icon`, `/screenshots`, `/languages`, `/versions` | public | – |
| `GET /api/apps/languages` | `{apps: [{id, languages}]}` | yes |
| `GET /api/apps/actionButtons` | bare array (UI action buttons, Phase 10) | yes |
| `GET /api/apps/marketplace`, `/categories`, `/featured-apps`, `/bundles/:id/apps` | marketplace browsing (bare arrays from the cloud) | yes for marketplace/categories |

`IAppInfoRest` = app manifest (`id`, `name`, `nameSlug`, `version`, `description`, `author
{name, homepage, support}`, `permissions`, `requiredApiVersion`, …) + `status`, `languages`,
`private`, `migrated`, optional `clusterStatus`, `licenseValidation`. Enabled statuses:
`auto_enabled`, `manually_enabled`; everything else (`*_disabled`, `initialized`, …) is disabled
(`AppStatusUtils`).

App settings use their own types: `boolean`, `code`, `color`, `font`, `int`, `select`,
`string`, `multiSelect`, `password`, `roomPick`, with `i18nLabel`, `i18nDescription`,
`i18nAlert`, `i18nPlaceholder`, `values`, `multiline`, `section`, `required`, `packageValue`.
Third check (`apps-engine/src/definition/settings/ISetting.ts`, `rest.ts`): app settings are keyed
by **`id`**, not `_id`; `GET :id/settings` returns an object map `{<id>: ISetting}`, and `POST
:id/settings {settings: [...]}` matches items by `s.id` and saves only those whose `value`
differs, answering `{updated: [...]}`. `int` is the apps-engine `NUMBER` type.
Reuse the settings widgets (14.5) and translate through `app-<appId>.<key>` (14.12).

Rustket scope: installed list with status toggle, settings form, uninstall, logs viewer;
private-app upload as an option; marketplace install via "Open in browser" (needs cloud login and
permission review UI). Live updates: `stream-apps`.

---

## 16. Email inboxes

Server `apps/meteor/server/api/v1/email-inbox.ts`, types
`packages/rest-typings/src/v1/email-inbox.ts`. Permission `manage-email-inbox` (Omnichannel; the
web marks the page "Alpha").

- `GET email-inbox.list?count&offset&sort` → `{emailInboxes, count, offset, total}` (verified, empty).
- `GET email-inbox/:_id` → inbox fields at the top level (+ `success`; UNVERIFIED: source only, no
  inbox on dev); a missing id → HTTP 404 `{"success":false,"error":"Resource not found"}` (third
  check, verified); `DELETE email-inbox/:_id` → `{_id}` (404 when missing).
- `POST email-inbox` (create, or update when `_id` is present) `{_id?, name, email, active,
  description?, senderInfo?, department?, smtp: {server, port, username, password, secure},
  imap: {server, port, username, password, secure, maxRetries?}}` → `{_id}`.
- `GET email-inbox.search?email=` → `{emailInbox: … | null}` (verified; use it to warn about a
  duplicate address).
- `POST email-inbox.send-test/:_id` → `{_id}` (sends a test mail through the inbox SMTP).
- SMTP/IMAP passwords are secrets (2.6). Department picker: `livechat/department` list (Phase 9).

---

## 17. LDAP test and sync

Server `apps/meteor/server/api/v1/ldap.ts`, EE `apps/meteor/ee/server/api/ldap.ts`; web
`settings/groups/LDAPGroupPage.tsx`, `client/hooks/useLdapSync.tsx`.

- `POST ldap.testConnection {}` (`test-admin-options`) → `{message: "LDAP_Connection_successful"}`;
  failures: `{"success":false,"error":"LDAP_disabled"}` (verified; LDAP is off on dev) or
  `"Connection_failed"`.
- `POST ldap.testSearch {username}` → `{message: "LDAP_User_Found"}`; failures `LDAP_disabled`
  (verified), `LDAP_search_failed`. The web first runs `testConnection`, then asks for a username.
- `POST ldap.syncNow {}` (EE file `apps/meteor/ee/server/api/ldap.ts`, registered on CE too) →
  `{message: "Sync_in_progress"}`; permission `sync-auth-services-users` (checked inside, plain
  `Error('error-not-authorized')`), then `LDAP_Enable` must be true (`LDAP_disabled`), then it calls
  `LDAPEnterprise.sync()` and `syncAvatarAndAbacAttributes()`. 2FA: on CE asked **every time**
  (2.3). Re-checked live (LDAP is off on dev, so nothing could sync): first call →
  `totp-required`; with the password code → `{"success":false,"error":"LDAP_disabled"}`; next call
  without code → `totp-required` again. UNVERIFIED: what a CE server answers with LDAP enabled
  (the EE LDAP service may be missing). The web runs `testConnection` first and asks for
  confirmation (`Execute_Synchronization_Now`, `LDAP_Sync_Now_Description`).
- Buttons are disabled while the LDAP group has unsaved changes. Messages are i18n keys.

---

## 18. Moderation console

Server `apps/meteor/server/api/v1/moderation.ts`, types
`packages/rest-typings/src/v1/moderation/*.ts`, web `views/admin/moderation/**`. Read needs
`view-moderation-console`, actions `manage-moderation-actions`. Verified end to end with a
throwaway user, channel and message (reports created with `POST chat.reportMessage {messageId,
description}` and `POST moderation.reportUser {userId, description}`, then dismissed).

Lists (params `latest?`, `oldest?` ISO date-times, `selector?` search text, `count`, `offset`,
`sort`):

- Tab "Messages": `GET moderation.reportsByUsers` → one row per reported **user** of messages:
  `{reports:[{userId, username, name, message, msgId, ts, count, rooms:[{_id,name,fname,t,federated}],
  roomIds, isUserDeleted}], count, offset, total}`.
- Tab "Users": `GET moderation.userReports` → `{reports:[{reportedUser:{_id, username, name, emails,
  createdAt}, count, ts}], …}`.

Details:

- `GET moderation.user.reportedMessages?userId=` → `{user:{_id,username,name}, messages:[{_id
  (report id), message (full IMessage), room, ts}], count, total, offset}`.
- `GET moderation.reports?msgId=` → reports of one message `{reports:[{_id, description,
  reportedBy:{_id,username,name,createdAt}, room, ts}]}`.
- `GET moderation.reportInfo?reportId=` → `{report: IModerationReport}`.
- `GET moderation.user.reportsByUserId?userId=` → `{user, reports:[{_id, description,
  reportedBy, reportedUser, ts, …}]}`.

Actions (`views/admin/moderation/hooks/*`):

| Action | Call |
|---|---|
| Dismiss reports of a message / of a user's messages | `POST moderation.dismissReports {msgId}` or `{userId}`, plus `reason?`, `action?` → `{}` (verified with `msgId`) |
| Dismiss user reports | `POST moderation.dismissUserReports {userId, reason?, action?}` → `{}` (verified) |
| Delete one message | `POST chat.delete {roomId, msgId}` |
| Delete all reported messages of a user | `POST moderation.user.deleteReportedMessages {userId, reason?}` |
| Deactivate user | `POST users.setActiveStatus {userId, activeStatus:false, confirmRelinquish?}` (4.4; effectively needs `edit-other-user-active-status`, so a moderator with only `manage-moderation-actions` gets `error-not-allowed` — hide or explain) |
| Reset avatar | `POST users.resetAvatar {userId}` (`edit-other-user-avatar` ∨ `manage-moderation-actions`) |

Third check — the web's actions are call sequences, not single calls
(`views/admin/moderation/hooks/*`):

- Delete one message (`useDeleteMessage.tsx`): `chat.delete {msgId, roomId, asUser: true}`, then on
  success `moderation.dismissReports {msgId}`. `asUser: true` deletes as the author and needs
  `force-delete-message` for other users' messages (`chat.ts`; admin and owner have it on dev).
- Deactivate user (`useDeactivateUserAction.tsx`): for message reports first
  `moderation.user.deleteReportedMessages {userId}`, then `users.setActiveStatus {userId,
  activeStatus: false, confirmRelinquish: true}` (always relinquishes, no last-owner prompt), then
  `moderation.dismissUserReports {userId}`.
- Dismiss for a user (`useDismissUserAction.tsx`): `moderation.dismissReports {userId}` on the
  "Messages" tab, `moderation.dismissUserReports {userId}` on the "Users" tab (one call, chosen by
  `isUserReport`).

Render reported messages with the shared message renderer (`room/render/message_body`).

---

## 19. Banners

Server `apps/meteor/server/api/v1/banners.ts`. There is **no admin CRUD** in core: banners come
from cloud announcements and apps. For completeness: `GET banners?platform=web|mobile` →
`{banners: IBanner[]}` (verified, empty), `GET banners/:id?platform=`, `POST banners.dismiss
{bannerId}`; streams `{uid}/banners` (notify-user) and `banner-changed` (notify-logged).
Rendering them (UiKit `banner` surface) belongs to Phase 10, not to the admin panel.

---

## 20. EE-only areas (gated)

All of these answer `error-action-not-allowed` "This is an enterprise feature" or
`error-unauthorized` (permission missing) on CE — verified for `engagement-dashboard/users/active-users`,
`sessions/list.all`, `audit.settings`, `audit.auditions`. Show them only with the module in
`licenses.info.license.activeModules`. Third check: the route's permission check runs before its
`license` check, which is why the audit endpoints answer HTTP 403 `error-unauthorized` on CE (their
permissions do not exist without the license) while engagement/sessions answer HTTP 400
`error-action-not-allowed`; the EE gate also comes before the 2FA prompt (`sessions/list.all`
answered the enterprise error without asking for a code).

- **Engagement dashboard** (`apps/meteor/ee/server/api/engagementDashboard/*.ts`, module
  `engagement-dashboard`, permission `view-engagement-dashboard`): `GET
  engagement-dashboard/users/new-users?start&end`, `…/users/active-users?start&end`,
  `…/users/chat-busier/hourly-data?start`, `…/users/chat-busier/weekly-data?start`,
  `…/users/users-by-time-of-the-day-in-a-week?start&end`, `…/messages/messages-sent?start&end`,
  `…/messages/origin?start&end`, `…/messages/top-five-popular-channels?start&end`,
  `…/channels/list?start&end&offset&count`. `start`/`end` are ISO strings. Charts follow the
  web's `views/admin/engagementDashboard/**`.
- **Audit** (`apps/meteor/ee/server/api/audit.ts`, module `auditing`): `GET audit.settings
  ?start&end&settingId&actor&count&offset&sort` (`can-audit`) → `{events, count, offset, total}`
  (settings change log, written by `updateAuditedByUser`); `GET audit.auditions?startDate&endDate`
  (`can-audit-log`); `POST audit.messages {rid?, startDate, endDate, users[], msg, type, visitor?,
  agent?}`, `POST audit.omnichannelMessages` (`can-audit`); `GET audit/rooms.members?roomId&filter`
  (`view-members-list-all-rooms`).
- **Device management** (`apps/meteor/ee/server/api/sessions.ts`, module `device-management`):
  `GET sessions/list.all`, `GET sessions/info.admin?sessionId=` (`view-device-management`), `POST
  sessions/logout {sessionId}` → `{sessionId}` — third check: the logout route requires
  **`logout-device-management`**, not `view-device-management` (that permission is absent from
  `permissions.listAll` on CE). All three need 2FA.
- **Custom roles** (13.3) and **ABAC**, **AI Center** (out of scope).

---

## 21. Out of scope / browser fallback

Import (`/admin/import`, `import.*`), Subscription and cloud registration (`/admin/subscription`,
`cloud.*`, `licenses.add` / `licenses.validate`), Feature preview, ABAC, AI Center, Omnichannel
admin pages. Offer "Open in browser" for these. The Workspace page may show the license summary
from `licenses.info` read-only.

---

## 22. Proposed Rust layout

### 22.1 `crates/rocketchat` (protocol)

```
src/rest/admin/
  mod.rs          // re-exports (Apps calls build URLs with ServerUrl::api_root("apps/…"))
  stats.rs        // statistics(refresh), statistics_list, instances, admin_licenses_info, licenses_max_active_users
  users.rs        // users_list_by_status(&UsersFilter, Page), user_info, user_create, user_update(tf),
                  // set_active_status, delete_user, reset_totp(user_id, tf), reset_e2e_key(user_id, tf),
                  // send_welcome_email, set_user_avatar, reset_user_avatar, smtp_check, send_invitation_email
  rooms.rs        // admin_rooms(&AdminRoomsFilter, Page), admin_room, save_room_settings, change_archivation, delete_room
  invites.rs      // list_invites, remove_invite, find_or_create_invite, validate_invite_token
  emoji.rs        // emoji_all(name, Page), emoji_create(upload, name, aliases), emoji_update, emoji_delete
  sounds.rs       // sounds_list, sound_get, sound_create(upload, name), sound_update, sound_delete
  user_status.rs  // custom_user_status_{list,create,update,delete}; update takes non-optional name AND status_type (9)
  integrations.rs // integrations_list, get, create(&NewIntegration), update(&IntegrationUpdate) [PUT], remove, history, clear_history, replay
  oauth_apps.rs   // oauth_apps_{list,get,create,update,delete}
  mailer.rs       // mailer_send(&MailerRequest)
  permissions.rs  // permissions_add_role, permissions_remove_role, permissions_update, roles_{create,update,delete},
                  // roles_get_users_in_role, roles_add_user, roles_remove_user  (listAll/roles.list already exist in rest/misc.rs)
  settings.rs     // private_settings(), private_settings_since(ts), save_settings(&[SettingChange], tf),
                  // setting_execute(id, tf), assets_set(asset, upload, refresh_all), assets_unset, settings_lookup(endpoint),
                  // add_custom_oauth/remove_custom_oauth/refresh_oauth_services (tf), saml_parse_metadata
  i18n.rs         // server_translations(lang) -> Result<HashMap<String, String>>  (GET /i18n/{lang}.json, gzip)
  apps.rs         // apps_installed, app, app_status/set_status, app_settings/save, app_logs, app_uninstall, app_install_private, apps_languages, apps_count
  email_inbox.rs  // email_inbox_{list,get,save,delete,search,send_test}
  ldap.rs         // ldap_test_connection, ldap_test_search, ldap_sync_now(tf)
  moderation.rs   // reports_by_users, user_reports, user_reported_messages, user_reports_by_user_id, reports_for_message,
                  // report_info, dismiss_reports, dismiss_user_reports, delete_reported_messages, report_user
  ee.rs           // engagement_*, audit_*, sessions_list_all/info_admin/logout (logout: logout-device-management)
src/models/admin/
  settings.rs     // SettingDef, SettingType (non_exhaustive + Unknown(String)), SelectOption, SettingQuery (string-or-json),
                  // FileConstraints, SettingChange {id, value, editor}, SettingsDelta {update, remove}
  stats.rs        // Statistics {typed fields + flatten extra}, Instance, LicenseInfo {active_modules, limits, prevented_actions, has_valid_license}
  users.rs        // AdminUser, UsersFilter {status, inactive_reason, roles, search_term, type_, sort}, NewUser, UserUpdate, LastOwnerDetails
  rooms.rs        // AdminRoom, AdminRoomType {C,P,D,L,Discussions,Teams}, RoomSettingsUpdate
  integrations.rs // Integration (enum tagged by "type"), OutgoingEvent, RetryDelay, IntegrationHistory
  misc.rs         // Invite, CustomSound, OAuthApp (secret redacted in Debug), EmailInbox (passwords redacted), ModerationReport…
  apps.rs         // AppInfo, AppStatus (+ is_enabled), AppSetting
```

Conventions: tolerant serde as everywhere (`#[serde(default)]`, `Option`), `Paginated<T>` for
paged lists, `Page {count, offset, sort: Option<Sort>}` where `Sort` serialises to the JSON string.
All 2FA-capable calls take `two_factor: Option<&TwoFactorCode>`; add `RestClient::put_with`,
`delete_with`, `upload_form` (multipart with extra text fields, reusing the upload watchdog); for
`/api/apps/…` reuse the existing `ServerUrl::api_root("apps/…")` with `RestClient::request` +
`send` (2.1) instead of a new URL helper. `reset_totp`/`reset_e2e_key` take a mandatory `&UserId` (4.4).

As implemented (admin library round): the REST functions live in
`crates/rocketchat/src/rest/admin/*.rs` and are prefixed where a generic name could collide
with other areas — `admin_server_info`, `admin_users_list(&UsersFilter, &Page)`,
`admin_user_info`, `admin_user_create`, `admin_user_update(id, &UserUpdate, confirm_relinquish,
tf)`, `admin_set_active_status`, `admin_user_delete`, `admin_reset_totp(id, tf)`,
`admin_reset_e2e_key(id, tf)` (empty id rejected client-side), `admin_send_welcome_email`,
`admin_set_user_avatar`, `admin_rooms(filter, &[AdminRoomType], &Page)`, `admin_room`,
`admin_rooms_autocomplete`, `admin_save_room_settings`, `admin_change_archivation_state`,
`admin_delete_team`, `admin_banners_list`; unprefixed: `statistics`, `statistics_list`,
`instances_get`, `admin_licenses_info`, `licenses_max_active_users`, `cloud_registration_status`,
`smtp_check`, invites (`list_invites`, `remove_invite`, `find_or_create_invite`,
`validate_invite_token`), `emoji_custom_{all,create,update,delete}`,
`custom_sound{s_list,_get,_create,_update,_delete}`, `custom_user_status_{page,create,update,delete}`,
`integration{s_list,_get,_create,_update,_remove,_history,_clear_history,_replay}`,
`oauth_app{s_list,_get,_create,_update,_delete}`, `mailer_send`,
`permissions_list_all_full` (with setting-permission fields), `permissions_{add,remove}_role`,
`permissions_update`, `roles_sync`, `role_{create,update,delete}`, `roles_get_users_in_role`,
`roles_{add,remove}_user`, `private_settings`, `private_settings_since`, `save_settings`,
`setting_value`, `setting_execute`, `setting_action_{method,endpoint}`, `settings_lookup`,
`assets_{set,unset}`, `settings_{add,remove}_custom_oauth`, `settings_refresh_oauth_services`,
`saml_parse_metadata`, `server_translations`, `email_inbox_{list,get,save,delete,search,send_test}`,
`ldap_{test_connection,test_search,sync_now}`, `moderation_*`, `engagement_dashboard`,
`sessions_{list_all,info_admin,logout}`, `audit_settings`. Models are in
`rocketchat::models::admin`. Apps (`/api/apps/*`) are left to the UiKit/apps layer. New
`RestClient` helpers: `get_with`, `put_with`, `delete_with` and the crate-private multipart
`upload_form` (shares the upload watchdog with `upload_media`). Settings search in
`GroupIndex::search` is a case-insensitive substring match, not a regex (no regex dependency).

### 22.2 `crates/rustket-core` (state)

```
src/admin/
  mod.rs           // AdminSection enum; visible_sections(&Workspace, &LicenseInfo) from section 1's table;
                   // permissions::ADMIN_PERMISSIONS / can_view_admin must be rebuilt from the same table (section 1)
  mongo_filter.rs  // evaluator for enableQuery/displayQuery (14.4), unit-tested against live queries
  settings_editor.rs
                   // SettingsStore { defs: BTreeMap<id, SettingDef> } + apply_delta/apply_stream_event
                   //   (stream event = partial merge + "delta fetch needed" flag, 2.5/14.1;
                   //   delta_request() -> DeltaRequest { since, generation }, apply_delta(req, delta)
                   //   keeps the flag when events arrived while the fetch was in flight)
                   // GroupIndex: groups(), group_page(group) -> Vec<Tab { id, sections: Vec<Section { name, setting_ids }> }>
                   // EditSession { group, edits: HashMap<id, (Value, Option<editor>)> }:
                   //   effective(id), is_changed(id), is_disabled(id), is_visible(id), can_reset(id),
                   //   reset(id), reset_section(..), cancel(), validate() -> Vec<(id, ErrorKey)>,
                   //   change_set() -> Vec<SettingChange>, alerts() -> Vec<id>, remote_conflicts(),
                   //   apply_saved(changes) (rebases edits made while a save was in flight)
                   //   ErrorKey = Rustket i18n keys admin.settings.error.* (en+ru, added with the UI)
  server_i18n.rs   // ServerI18n { custom: Map, lang: Map, en: Map }; t(key) -> Option<&str>;
                   // label_for(def), hint_for(def), alert_for(def); per-server cache by version+lang
  timespan.rs      // highest_unit(ms), to_ms(unit, n) (14.5)
  invites.rs       // invite_url(&Invite, settings) (6)
```

All of it is plain data + pure functions (testable without iced), matching the existing layering.

### 22.3 `crates/rustket` (UI)

A new full-page screen next to Settings/Account/Directory, owned by a new "admin agent":

```
src/screens/admin/
  mod.rs            // Admin { section, pages... }, left navigation (sections from visible_sections),
                    // TwoFactorPrompt modal + pending-request replay, "Open in browser" fallback
  workspace.rs  users.rs  user_form.rs  rooms.rs  room_form.rs  invites.rs  emoji.rs  sounds.rs
  user_status.rs  integrations.rs  integration_form.rs  oauth_apps.rs  mailer.rs  permissions.rs
  roles.rs  apps.rs  email_inboxes.rs  moderation.rs  ee.rs
  settings/
    mod.rs          // group grid + search
    group.rs        // tabs, accordion sections, save bar, alert confirmation
    widgets.rs      // one widget per SettingType (reused by apps settings)
locales/admin.yml   // en + ru, prefix admin.*
```

Wiring (additive changes to shared files only): `Nav::Admin(AdminSection)` in
`component::Effect::Navigate`, an entry in the status menu / rail when `can_view_admin`,
`Message::Admin(admin::Message)` in `app.rs`. Subscriptions for the streams in 2.5 are opened by
`Admin::subscription()` only while the relevant page is visible. Long lists use paged tables
(25/50 rows) with server-side search and sort, debounced 400–500 ms like the web.

---

## 23. Testing plan

- Fixtures to capture into `crates/rocketchat/tests/fixtures/admin/` (all read-only, as
  `admin`): `private-settings/get` (full, ~700 KB — or a trimmed copy with one setting per type
  plus all LDAP/OAuth/Accounts settings), `permissions.listAll`, `roles.list`, `statistics`,
  `instances.get`, `licenses.info`, `users.listByStatus`, `users.info`, `rooms.adminRooms`,
  `emoji-custom.all`, `custom-sounds.list`, `custom-user-status.list`, `integrations.list`
  (after creating throwaway webhooks), `oauth-apps.list`, `listInvites`, `moderation.*`,
  `/api/apps/installed`, `/i18n/en.json` (trimmed), a `totp-required` error body.
- Unit tests: `mongo_filter` against every live `enableQuery`/`displayQuery`; group/tab/section
  building for `LDAP` (tabs), `OAuth` (custom sections), `Accounts` (many sections); timespan
  conversion; `ServerI18n` fallback chain incl. `Custom_Translations`; invite URL builder;
  bare-array/bool envelope decoding; a truncated i18n body maps to "unavailable".
- Live tests (`crates/rocketchat/tests/live.rs`, skip without `RUSTKET_TEST_SERVER`), each with
  unique `rkadm-*` names and cleanup in all paths: user create → update (expect
  `TwoFactorRequired`, retry with password hash) → deactivate/activate → role add/remove →
  resetTOTP → delete; channel → saveRoomSettings → archive → delete; invite create/list/remove;
  emoji and sound multipart CRUD; custom status CRUD; incoming + outgoing webhook CRUD; OAuth app
  CRUD; moderation report → list → dismiss; `private-settings/get` decodes all 919 entries;
  `/i18n/en.json` parses. **Never** call `POST settings`, `permissions.*` mutations, `roles.*`
  mutations other than `addUserToRole`/`removeUserFromRole` on a throwaway user, assets, apps
  install/status, `ldap.syncNow` or `users.deactivateIdle` from tests (shared dev server).
  Do not assert absolute counts (users, rooms, `activeUsers`, statistics) — other agents change
  them concurrently. Log out the test's login token at the end (`POST logout`) so the 2FA
  "remember" state does not leak into other tests.

---

## 24. Edge cases checklist

- `users.resetTOTP` / `users.resetE2EKey` without a user id hit the caller (4.4).
- Bulk settings save, `users.resetTOTP`/`resetE2EKey` and (on CE) `ldap.syncNow` ask for 2FA every
  time; other admin 2FA calls are remembered for 30 min (2.3). Handle `totp-invalid` by
  re-prompting.
- `query`/`fields` parameters are silently ignored (2.2).
- `listInvites` returns stored invites with `url: ""` (6); several endpoints return bare
  arrays/booleans (2.1); Apps REST is under `/api/apps` (15).
- Fresh statistics have `_updatedAt: null` (3).
- `enableQuery` is a JSON string; evaluate on edited values (14.4).
- `private-settings/get` returns the **stored** value of EE settings (not `invalidValue`, unlike
  REST `GET settings`); disable them without the module and hint the effective value (14.1, 14.6).
- Saving a `blocked` setting "succeeds" without changing anything (14.2).
- Colour settings without `editor` (14.5); `select` values are saved as strings.
- `custom-user-status.update` needs `name` and clears `statusType` when omitted (9).
- Outgoing webhooks need a non-empty `token` to be updatable; `channel` must be sent (10.2).
- Mailer dry run goes to the user whose address is `from`, not to the caller (12).
- `roles.update` on protected roles must repeat name/scope/description (13.3).
- `users.setActiveStatus` effectively needs `edit-other-user-active-status`; `roles.addUserToRole`
  needs `access-permissions` besides `assign-admin-role` (4.4).
- `can_view_admin` must not include `access-marketplace` (section 1).
- `int`/`range`/`timespan` must be integers server-side; never send NaN.
- Remote setting changes must not drop local edits silently (14.6).
- `/i18n/*.json` is truncated without gzip (14.12); `ru.json` lacks ~35 % of keys.
- Setting descriptions are Markdown and contain literal `{{…}}` (14.6).
- Last-owner errors on deactivate/delete need `confirmRelinquish` (4.4).
- Team main rooms are deleted through `teams.delete` (5.3).
- Never modify or disable `Accounts_OAuth_Custom-Keycloak` on the dev server (14.10).
- Secrets are returned in clear text to admins; never log them (2.6).
- `private-settings-changed` is partial and license-masked; follow it with a delta
  `private-settings/get` (2.5, 14.1). `license` arrives without payload; refetch `licenses.info`.
  `stream-integrationHistory` never reports removals (2.5).
- `users.create` rejects `settings`, `users.update` rejects `settings` and `language`
  (`invalid-params`), although the TS types list them (4.3).
- `licenses.info` has empty `limits` for users without `view-privileged-setting` (1).
- `roomPick` autocomplete via `channelAndPrivate` only finds rooms the admin is in (14.5).
- `sessions/logout` needs `logout-device-management` (20).
- Setting validation errors are i18n keys `<id>_Invalid` (14.7).

---

## 25. Verification log (2026-11-08, dev server 8.9.1, user `admin`)

Verified read-only: `GET /api/info`, `statistics?refresh=false|true`, `statistics.list`,
`instances.get`, `licenses.info[?loadValues=true]`, `licenses.maxActiveUsers` (and 404 for
`licenses.get`, `licenses.isEnterprise`), `users.listByStatus` (all filters), `users.list`
(query ignored), `users.info`, `smtp.check`, `rooms.adminRooms`, `rooms.adminRooms.getRoom`,
`rooms.autocomplete.adminRooms`, `rooms.autocomplete.channelAndPrivate`, `listInvites`,
`emoji-custom.all`, `emoji-custom.list`, `custom-sounds.list`, `custom-sounds.getOne`,
`custom-user-status.list`, `integrations.list/get/history`, `oauth-apps.list/get`,
`permissions.listAll`, `roles.list`, `roles.getUsersInRole`, `GET settings`,
`private-settings/get` (full and delta, via `method.call`), `/i18n/{en,ru,pt-BR,de,xx}.json`
(with and without gzip), `/api/apps/{installed,languages,actionButtons,count,externalComponents,categories,marketplace}`,
`email-inbox.list`, `email-inbox.search`, `banners`, `video-conference.providers`,
`ai.llm.models`, all `moderation.*` GETs, EE gating responses (engagement, sessions, audit), DDP
subscriptions listed in 2.5.

Verified mutations on throwaway objects (all deleted afterwards): `users.create`, `users.update`
(with the 2FA password fallback), `users.setActiveStatus`, `roles.addUserToRole`,
`roles.removeUserFromRole`, `users.resetTOTP`, `users.resetE2EKey`, `users.sendWelcomeEmail`,
`users.delete`, `channels.create`, `rooms.saveRoomSettings`, `rooms.changeArchivationState`,
`rooms.delete`, `findOrCreateInvite`, `validateInviteToken`, `removeInvite`,
`emoji-custom.create/update/delete`, `custom-sounds.create/update/delete`,
`custom-user-status.create/update/delete`, `integrations.create/update/remove` (incoming with
script, outgoing), `POST /hooks/{id}/{token}`, `oauth-apps.create/update/delete`,
`chat.reportMessage`, `moderation.reportUser`, `moderation.dismissReports`,
`moderation.dismissUserReports`; validation-only calls `mailer` (missing `[unsubscribe]`),
`ldap.testConnection`, `ldap.testSearch` (`LDAP_disabled`).

UNVERIFIED (global state; specified from source): `POST settings`, `settings/:_id`,
`settings.addCustomOAuth/removeCustomOAuth/refreshOAuthServices`, `assets.setAsset/unsetAsset`,
`permissions.update/addRole/removeRole`, `roles.create/update/delete`, `mailer` (real send),
`sendInvitationEmail`, `users.deactivateIdle`, `users.setAvatar` with `avatarUrl`, `teams.delete`,
`integrations.replayOutgoing` (success path), `moderation.user.deleteReportedMessages`,
all `/api/apps` mutations, `email-inbox` create/delete/send-test, `ldap.syncNow` (only its 2FA
and `LDAP_disabled` answers were probed), `licenses.add`, EE endpoints with a license,
`saml.parseMetadata` (success path), the error body of bulk-settings type-check failures,
`?resumeToken=` browser hand-over. (`users.setAvatar` multipart / `users.resetAvatar` and
`integrations.clearHistory` were verified in the third check.)

Second check (2026-11-08, throwaway `rkadm-chk-*` objects, all deleted, token logged out),
re-verified live: `/api/info` with and without auth, `licenses.info`, `licenses.maxActiveUsers`,
404s of `licenses.get`/`isEnterprise`, `users.listByStatus` (sort, `roles[]`, `roles=`,
`inactiveReason[]`, `hasLoggedIn`, invalid sort), `users.list` (query ignored, case-sensitive name
sort), `users.info`, `smtp.check`, `users.create`, `users.update` (2FA required / `totp-invalid` /
remembered / remembered across User-Agents), `users.resetTOTP` and `users.resetE2EKey` (2FA every
time), `roles.addUserToRole`/`removeUserFromRole` (`roleId`; `roleName` rejected),
`users.setActiveStatus`, last-owner errors on deactivate and delete, `users.delete`,
`channels.create`, `rooms.adminRooms` (+ DM `usernames` filter), `rooms.adminRooms.getRoom`,
`rooms.saveRoomSettings`, `rooms.changeArchivationState`, `rooms.delete`, invites
(create/list/validate/remove), emoji multipart CRUD (+ alias rules), sounds multipart CRUD
(+ extension rule), custom user status CRUD (+ update pitfalls), incoming/outgoing integration
create/update/get/list/history/remove (+ `scriptEngine` optional, outgoing `token` required,
`POST integrations.update` → 404), OAuth app CRUD, `permissions.listAll`, `roles.list`,
`roles.sync`, `roles.getUsersInRole`, `private-settings/get` (REST `method.call` and DDP),
REST `GET settings` vs `private-settings/get` for EE values, `POST settings` 2FA prompt (no
write), `ldap.testConnection`, `ldap.syncNow` (2FA + `LDAP_disabled`), `/i18n/*.json`
(Content-Length bug, 400 for unknown language), `/api/apps/{count,installed,languages,
actionButtons,categories,marketplace}`, `email-inbox.list`/`search`, `moderation.reportsByUsers`/
`userReports`, `statistics`, `statistics.list`, `instances.get`, `cloud.registrationStatus`,
the DDP subscriptions of 2.5, EE gating answers (engagement, sessions, audit).

Third check (2026-11-08, throwaway `rkadm-chk3-*` channel, user, invite, OAuth app and outgoing
webhook, all deleted; login tokens logged out). Source re-read for sections 1–20 (admin sidebar,
RN admin view, 2FA code path, `ApiClass` success/failure, settings handlers and
`saveSettingsBulk`, streamer listeners, web settings editor, users/rooms/invites/emoji/sounds/
status/integrations/OAuth/mailer/permissions/roles/apps/email-inbox/LDAP/SAML/moderation/EE
handlers). Re-verified live: `permissions.listAll` (ids and role holders of section 1),
`licenses.info` (admin, `loadValues`, and as `alice`), `licenses.maxActiveUsers`, 404s of
`licenses.get`/`isEnterprise`, `/api/info` with auth, `statistics?refresh=false`, `instances.get`,
`statistics.list`, `cloud.registrationStatus`, pagination settings, error envelopes (`LDAP_disabled`,
`invalid-params`, EE gating, `error-unauthorized` 403, `not_authorized`), invites (create, dedup,
invalid days/uses, list, validate, remove, removal of a missing id), OAuth app CRUD,
`users.create` (incl. `settings` rejection), `users.update` (`language`/`settings` rejection,
2FA prompt, password retry, remembered), `users.resetTOTP` 2FA-every-time, `users.setAvatar`
multipart, `users.resetAvatar`, `users.delete`, outgoing webhook create/clearHistory/replay
error/history/remove, `email-inbox/:_id` 404, `email-inbox.search`/`list`, `roles.list`,
`roles.getUsersInRole`, `roles.sync`, `private-settings/get` via `method.call` (all live counts of
14.1–14.3 recomputed: types, fields, groups, orphans, operators, EE stored vs `invalidValue`),
`/i18n/en.json` (gzip and Content-Length truncation), `/i18n/ru.json`, unknown language 400,
`rooms.autocomplete.channelAndPrivate` vs `rooms.autocomplete.adminRooms`,
`video-conference.providers`, `ai.llm.models`, `saml.parseMetadata` (blocked URL), and as `alice`:
`POST settings` → `totp-required`, `private-settings/get` → `[]`, `listInvites` →
`not_authorized`.
