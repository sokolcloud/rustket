# Teams, discussions and video conferencing / calls (spec)

Scope: `docs/PLAN.md` Phase 9 ("Teams (team channels, add/remove rooms, autojoin, members,
convert), video conferences (start/join/ring via external browser, `videoconf` message blocks,
incoming call popup)") plus the discussion management of Phase 6/7 (list, create, delete, header
navigation) and what Rocket.Chat 8.9.1 ships as "voice calls" (`media-signal`, `media-calls.*`,
`call-history.*`). Rendering of the `video_conf` UIKit block itself is specified in
`docs/uikit.md` §3.4; this document owns everything around it (call state, ringing, joining).

Conventions:

- `RC:` = `.ref/rocketchat/` (8.9.1), `SRV:` = `RC:apps/meteor/server/`, `WEB:` =
  `RC:apps/meteor/client/`, `EE:` = `RC:apps/meteor/ee/server/`, `RN:` = `.ref/rn/`.
  `RC:ee/packages/media-calls/src` was added to the sparse checkout for §8.
- **[live]** = verified against the dev server on 2026-11-08 with throwaway users (`tvc-*`,
  `tvd-*`, `tvs-*`, `tvx-*`, `tve-*`, `tvj-*`, `tvm-*`), teams and rooms, all deleted afterwards
  (re-checked with `users.list` / `rooms.adminRooms` / `teams.listAll`: nothing left).
  **[code]** = read from the sources, not exercised. **[unverified]** = needs an Enterprise
  license or an installed provider app, which the dev server does not have.
- Probe scripts (not part of the repo, for reproduction only):
  `/tmp/claude-1000/-home-sokol-vibecodes-rustket/1286654a-3c6a-458e-84e4-a93ea5a03064/scratchpad/{teams_probe,ddp_probe,ddp_spoof,disc_probe,empty_probe,join_probe,misc_probe}.py`.
- Checker pass (2026-11-08): claims re-read against the sources and a sample re-run live with
  `chk_probe.py` / `chk_probe2.py` (same scratchpad, users `tvk-*` / `tvk2-*`, all deleted). Facts
  first found in that pass are marked **[live, checker]**; corrections are inline.
- REST rate limiting is off on the dev server (`API_Enable_Rate_Limiter = false`, read via
  `GET settings/API_Enable_Rate_Limiter` as admin) **[live, checker]**; it defaults to on in
  production, so the per-endpoint limits in §7.3 apply there.
- Dev server facts **[live]**: no license (`GET /api/v1/licenses.info` →
  `license.activeModules: []`, `hasValidLicense: false`, readable by a plain user), no app
  installed, `video-conference.providers` → `{"data":[]}`, `Discussion_enabled = true`,
  `API_User_Limit = 500`, `UI_DisplayRoles = true`, `Omnichannel_call_provider = "none"`,
  `VideoConf_Default_Provider = ""`. The EE-only settings `VideoConf_Enable_DMs/Channels/Groups/Teams`,
  `VideoConf_Enable_Persistent_Chat`, `VideoConf_Conference_Window_Enabled`,
  `VideoConf_Persistent_Chat_Mode` are **absent** (`settings.public?_id=…` does not return them);
  `VideoConf_Mobile_Ringing` exists with `enterprise: true, modules: ["videoconference-enterprise"],
  invalidValue: false`.

## 0. Live verification summary

Teams (all **[live]**, details in §2):

| Call | Observed |
|---|---|
| `POST teams.create {name,type:0,members:[uid]}` | `{"team":{_id,name,type,createdAt,createdBy:{_id,username},_updatedAt,roomId}}` |
| same name again / missing `type` / name `"bad name x!"` | 400 `{"error":"team-name-already-exists"}` / 400 `invalid-params` "must have required property 'type'" / 400 `{"error":"error-team-creation"}` (nothing is left behind) |
| `members: ["<username>"]` | accepted (ids **or** usernames) |
| `owner: <other uid>` | team owned by that user; the caller is **not** a member |
| `room: {readOnly:true, extraData:{topic,broadcast}}` | applied to the main room (`ro:true`, `topic`) |
| `GET rooms.info` on the main room | room has `teamId`, `teamMain:true`; response also has top-level `team:{_id,name,type,roomId}` |
| subscriptions of team rooms (`subscriptions.get`, `getOne`, `subscriptions-changed`) | **never** carry `teamMain`/`teamId` (also for members added later and for auto-joined child rooms) |
| `GET teams.info` member / outsider of public team (by name) / outsider of private team / unknown / no params | 200 / 200 / 403 `{"error":"unauthorized"}` / 400 `{"error":"Team not found"}` / 400 `error-invalid-params` |
| `GET teams.list` | own teams with extra `rooms` (child room count) and `numberOfUsers`; outsider → `[]` |
| `GET teams.listAll` as user / admin | 403 `"User does not have the permissions required for this action [error-unauthorized]"` / 200 |
| `GET teams.autocomplete?name=` | `{"teams":[…]}` of main **rooms** `{_id (room id), fname, name, t, teamId}`; private teams only for members |
| `POST teams.addRooms` ok / again / room not owned / caller lacks perm / bad team / bad room | `{"rooms":[…]}` / 400 `room-already-on-team` / 400 `error-no-owner-channel` / 403 `error-no-permission-team-channel` / 400 `team-does-not-exist` / 400 `invalid-room` |
| `channels.create {extraData:{teamId}}` by owner / `groups.create` in team by plain member | 200 room with `teamId` / 400 `error-not-allowed` (`details.method: createPrivateGroup`) |
| `GET teams.listRooms` owner / member / outsider with no rooms at all / outsider on private team | all child rooms / public + joined / **all, private groups included** (server bug, §2.8) / 400 `user-not-on-private-team` |
| `POST teams.updateRoom {roomId,isDefault:true}` | `{"room":{…,teamDefault:true}}`; team members get a subscription + `au` system message; by member → 403 `unauthorized`; room outside a team → 400 `room-not-on-team` |
| `GET teams.listRoomsOfUser` | rooms with `isLastOwner`; for another user without perm → 403 |
| `GET teams.listChildren` | main room + visible child rooms + main-room discussions; outsider → 400 `error-invalid-team-not-a-member`; unknown `teamId` → **404** `"Resource not found"`; `roomId` of a non-team room → 400 `room-not-on-team` **[live, checker]** |
| `GET teams.members` | `{members:[{user:{_id,username,name,status,settings:{}},roles?,createdBy:{_id,username},createdAt}],total,count,offset}`; outsider of public team → empty list (not an error); `status=offline` and `status[]=offline` both work |
| `POST teams.addMembers` with `roles:["moderator"]` | subscription `roles:["moderator"]`; new member auto-joined to autojoin rooms; by member → 403 |
| `POST teams.updateMember` `["owner"]` / `["admin"]` / `[]` | 200 / 403 `unauthorized` / 200, subscription `roles: []` |
| `POST teams.removeMember` with `rooms` / non-member / last owner | removed from team and listed rooms / 400 `member-does-not-exist` / 400 `last-owner-can-not-be-removed` |
| `POST teams.leave` last owner / with `rooms` / not a member | 400 `last-owner-can-not-be-removed` / 200 / 400 `member-does-not-exist` |
| `rooms`/`roomsToRemove: []` on `leave`, `removeMember`, `delete` | 400 `invalid-params` "must NOT have fewer than 1 items…" (omit the key instead); `convertToChannel` accepts `[]` |
| `POST teams.removeRoom` / again | `{"room":{… without teamId}}` / 400 `room-not-on-that-team` |
| `POST teams.update {data:{name,type:1}}` / `data:{}` / by member | renames main room, makes it `t:"p"` / 400 `invalid-params` / 403 |
| `channels.convertToTeam {channelId}` / again / plain user on own channel / `groups.convertToTeam {roomName}` | `{"team":…}` / 400 `team-name-already-exists` / 200 / `{"team":…}` |
| `POST teams.convertToChannel` by member / owner | 403 / 200, room loses `teamMain`/`teamId` |
| `POST teams.convertToChannel {roomsToRemove:[<main room id>]}` | 200, and the **main room is erased** (team and channel both gone) **[live, checker]**, §2.7 |
| `POST teams.delete` by member / unknown | 403 / 400 `team-does-not-exist` |
| `rooms.delete` on a main room | 400 `error-cannot-delete-team-channel` |
| `channels.join` on public team main room | becomes team member (`createdBy` = self), auto-joined to default rooms, `ujt` message |
| `channels.leave` on main room / as last owner (`rooms.leave` too) | removed from team, child rooms kept / 400 `error-you-are-last-owner` |
| `rooms.autocomplete.availableForTeams?name=` | `{"items":[{_id,fname,name,t}]}` (rooms the caller owns, not in a team) |
| `directory?type=teams` | team main rooms with `teamId`, `teamMain`, `roomsCount` |

Discussions (§6), video conferences (§7) and media calls (§8) have their own live notes inline.
Permission defaults were read with `permissions.listAll` **[live]** (§4).

## 1. Teams: data model

### 1.1 Documents

`RC:packages/core-typings/src/ITeam.ts`:

```ts
enum TeamType { PUBLIC = 0, PRIVATE = 1 }
interface ITeam { _id; name; type: TeamType; roomId /* main room */; createdBy: {_id, username}; createdAt; _updatedAt }
interface ITeamMember { teamId; userId; roles?: string[]; createdBy: {_id, username}; createdAt }
```

A team is a separate collection (`rocketchat_team`) plus a **main room**: an ordinary channel
(`t:"c"`, public team) or private group (`t:"p"`, private team) with `teamMain: true` and
`teamId`. Room fields (`RC:packages/core-typings/src/IRoom.ts`):

| Field | On | Meaning |
|---|---|---|
| `teamMain: true` | main room | this room *is* the team |
| `teamId` | main room and child rooms | owning team |
| `teamDefault: true` | child room | "auto-join": new team members are subscribed automatically |
| `prid` | discussions | a discussion of the main room is a team "child" in `teams.listChildren`, but has **no** `teamId` |

`ISubscription` declares `teamMain?` / `teamId?` (`RC:packages/core-typings/src/ISubscription.ts`),
but 8.9 never writes them **[live]**: everything team-related must be read from the **room**
document. The web client does the same: `WEB:cachedStores/SubscriptionsCachedStore.ts` copies
`teamId`/`teamMain` from the room into its merged subscription record. Rustket's
`RoomEntry::is_team()` (`crates/rustket-core/src/workspace.rs`) already falls back to the room;
any "is this room in a team" check must use `room.team_id`.

The team doc and the main room are kept in sync both ways: `teams.update` renames/retypes the
main room (`SRV:services/team/service.ts` `update` → `saveRoomName`/`saveRoomType`), and
`rooms.saveRoomSettings` `roomName`/`roomType` on a main room updates the team with
`updateRoom:false` (`SRV:meteor-methods/rooms/saveRoomSettings.ts` `settingSavers.roomName/roomType`).
So the team name equals the main room `name`; display the room `fname`/`name` like any room.

Team membership (`ITeamMember`) mirrors the main-room subscription:

- `addUserToRoom` on a main room creates the team membership (`Team.addMember`, which also
  subscribes the user to every `teamDefault` room) — `SRV:lib/rooms/addUserToRoom.ts` (end).
  So `channels.invite`, `groups.invite`, `channels.join` and invite links all make team members **[live]**.
- `removeUserFromRoom` on a main room deletes the membership (`SRV:lib/rooms/removeUserFromRoom.ts`);
  child-room subscriptions are kept **[live]**.
- Owner/moderator/leader set through `channels.addOwner` etc. on the main room are mirrored into
  `ITeamMember.roles` (`SRV:meteor-methods/rooms/addRoomOwner.ts` → `Team.addRolesToMember`, same
  for moderator/leader and the remove variants).

### 1.2 Room type mapping

| Team | Main room `t` | Created through |
|---|---|---|
| `type: 0` | `c` | `Room.create(…, type 'c')` in `TeamService.create` |
| `type: 1` | `p` | same with `'p'` |

Child rooms can be `c` or `p`, independent of the team type. DMs and omnichannel rooms are never
team rooms. Discussions cannot be added to a team (`rooms.autocomplete.availableForTeams` excludes
them; `Rooms.findPaginatedRoomsWithoutDiscussionsByRoomIds`).

## 2. Teams REST API

All under `/api/v1/`, `authRequired`. Handler: `SRV:api/v1/teams.ts`; service:
`SRV:services/team/service.ts`; params: `RC:packages/rest-typings/src/v1/teams/*.ts`.

Almost every endpoint takes **either** `teamId` **or** `teamName` (JSON-schema `oneOf`, so sending
both fails validation). Rustket should always send `teamId` (`TeamRef::Id`); names change.

Error envelope quirk: service errors are thrown as `new Error('code')` and come back as
`{"success":false,"error":"<code>"}` **without `errorType`** (HTTP 400), e.g.
`team-does-not-exist`, `room-already-on-team`, `last-owner-can-not-be-removed` **[live]**. Schema
errors carry `errorType: "invalid-params"` (body) or `"error-invalid-params"` (query). Permission
failures are 403 `{"error":"unauthorized"}` (`API.v1.forbidden()`), or a custom string
(`error-no-permission-team-channel`, `Not allowed`). The REST error mapper must expose `error`
as the machine code when `errorType` is missing and `error` matches `^[a-z0-9-]+$`.

Pagination: `offset`/`count` with the server defaults (`getPaginationItems`; page cap
`API_Upper_Count_Limit`, see `docs/protocol.md` "Settings"). `teams.listRoomsOfUser` always answers
`offset: 0` regardless of the request (handler bug, `SRV:api/v1/teams.ts`).

Response documents of mutating endpoints are **pre-update copies** patched in memory
(`addRooms` returns the rooms read before `setTeamByIds`, `updateRoom`/`removeRoom` return the
room read before the write with the field patched): their `_updatedAt` is stale **[live]**. Do not
feed them into the store as authoritative; the realtime `rooms-changed` events (§3) carry the
real documents.

### 2.1 `POST teams.create`

Params (`TeamsCreateProps`, `additionalProperties: false`):

```jsonc
{ "name": "dev-team", "type": 0,              // 0 public, 1 private — required
  "members": ["uid-or-username", …],           // optional; resolved with Users.findActiveByIdsOrUsernames
  "owner": "uid",                              // optional; default = caller
  "room": {                                    // optional, forwarded to Room.create
    "readOnly": false,
    "extraData": { "topic": "…", "broadcast": false, "encrypted": false },
    "id": "<existing rid>"                     // convert an existing room (needs create-team + edit-room on it)
  } }
```

Response: `{"team": ITeam}` (no main-room document; fetch it with `rooms.info` or wait for
`rooms-changed`). Permission: `create-team` (global, `permissionsRequired`). Errors:
`team-name-already-exists` (`checkUsernameAvailability(name,'room')`), `room-name-already-exists`
(a room with that name exists and is not `room.id`), `invalid-user`, and **any** failure inside
room creation (invalid name per `UTF8_Channel_Names_Validation`, E2E policy, …) collapses into
`error-team-creation` **[live]** — validate the name client-side with the same regex the
create-channel dialog uses so the user gets a precise message.

Semantics (`TeamService.create`): the creator (or `owner`) gets `roles:["owner"]` in the team and
in the main-room subscription; `members` are added without roles; no system message for initial
members; `user-converted-to-team` is posted only when `room.id` converts an existing room.
With `owner` set, the caller is not added at all **[live]**.

Web dialog (`WEB:navbar/NavBarPagesGroup/actions/CreateTeamModal.tsx`): name, topic, private
toggle (default private), read-only (disabled unless `set-readonly` for role `owner`, forced on when
broadcast), encrypted (only for private; default from `E2E_Enabled_Default_PrivateRooms`, forced by
`E2E_Force_Encryption_For_Private_Rooms`), broadcast, members autocomplete; sends
`{name, members, type: isPrivate?1:0, room:{readOnly, extraData:{topic, broadcast, encrypted}}}`.
RN sends the same minus `topic` (`RN:app/lib/services/restApi.ts` `createTeam`).

### 2.2 Converting a room to a team

| Endpoint | Body | Permission (server) |
|---|---|---|
| `POST channels.convertToTeam` | `{channelId}` or `{channelName}` (exactly one) | `create-team` global + `create-team` **and** `edit-room` scoped to the room |
| `POST groups.convertToTeam` | `{roomId}` or `{roomName}` | `create-team` + `edit-room` scoped to the room |

Both build `{team:{name: room.name, type: c→0 / p→1}, members: all subscribers, room:{name, id}}`
and call `Team.create` (`SRV:api/v1/channels.ts`, `SRV:api/v1/groups.ts`). Response `{"team": ITeam}`.
Posts `user-converted-to-team` in the room. Errors: `team-name-already-exists` (already a team)
**[live]**, `Channel not found` / `Private group not found`, 403.

Web gate (`WEB:views/room/contextualBar/Info/hooks/actions/useRoomConvertToTeam.tsx`): shown in
the room info kebab when `!room.teamId && !room.prid && !federated && create-team && canEditRoom`,
with a confirmation modal (`Converting_channel_to_a_team`). The server does not reject a
discussion (`prid`) **[code]** — keep the web gate.

### 2.3 `GET teams.info`

Query `teamId` | `teamName`. Response `{"teamInfo": ITeam}`. Allowed when the caller can access
the main room or has `view-all-teams`. Errors **[live]**: unknown → 400 `"Team not found"`; main
room missing → 400 `"Room not found"`; private team for an outsider → 403 `unauthorized`.

`rooms.info` on a team room is usually enough: for the main room it adds `team:{_id,name,type,roomId}`
**[live]**, and for a child room it adds `team` and `parent:{_id,name,fname,t}` (= the main room)
(`TeamService.getRoomInfo`, used by `SRV:api/v1/rooms.ts` `rooms.info`) **[code]**.

### 2.4 Listing teams

| Endpoint | Returns | Gate |
|---|---|---|
| `GET teams.list` | teams the caller is a member of, each with `rooms` (number of child rooms, main room excluded, `Rooms.countByTeamId`) and `numberOfUsers` (team members); `sort` JSON works; the `query` JSON param is **ignored** unless the server runs with `ALLOW_UNSAFE_QUERY_AND_FIELDS_API_PARAMS=true` (`SRV:api/lib/parseJsonQuery.ts`) | any user |
| `GET teams.listAll` | every team, same extras | `view-all-teams` (admin) |
| `GET teams.autocomplete?name=` | `{teams:[…]}`: ≤10 main rooms whose `name`/`fname` starts with `name` (case-insensitive), public ones or ones the caller is subscribed to; fields `_id` (**room id**), `fname`, `name`, `t`, `teamId`, `avatarETag` | any user |
| `GET users.listTeams?userId=` | `{teams: (ITeam & {isOwner})[]}`, unpaginated: teams `userId` belongs to; without `view-all-teams` private teams are only listed if the caller is in them too (`Team.findBySubscribedUserIds`, `SRV:api/v1/users.ts`). The web's membership check for the "Back to team" button (§5.2) | any user |
| `GET directory?type=teams&text=` | main rooms with `roomsCount` (owned by the directory screen, `docs/` account agent) | any user |

### 2.5 `POST teams.update`

```json
{ "teamId": "…", "data": { "name": "new-name", "type": 1 } }
```

`data` needs at least one of `name`/`type` (`anyOf`). The schema also accepts a top-level
`updateRoom`/`name` (ignored by the handler). Permission: `edit-team` in the main room. The main
room is renamed (system message `r`) and/or retyped (`room_changed_privacy`) **[live]**. The web
does not call it; it edits the main room through `rooms.saveRoomSettings` (Edit in Team info),
which syncs the team (§1.1). Rustket should do the same (one edit form for rooms and teams).

### 2.6 `POST teams.delete`

```json
{ "teamId": "…", "roomsToRemove": ["rid", …] }   // omit roomsToRemove when empty
```

Permission checked by the handler: `delete-team` in the main room. `eraseTeam`
(`SRV:api/lib/eraseTeam.ts`): erase the listed child rooms (main room filtered out), detach every
other room of the team **including the main room** (`unsetTeamIdOfRooms` → `Rooms.unsetTeamId`
unsets `teamId`/`teamDefault`/`teamMain`; posts `user-converted-to-channel` in the main room first),
erase the main room, delete memberships and the team. Deleting the main room also deletes its
discussions (`afterDeleteRoom` `DeleteDiscussionChain`, `SRV:hooks/messages/propagateDiscussionMetadata.ts`).
`roomsToRemove: []` → 400 invalid-params **[live]**.

Hidden permission requirements **[code]**: every erase goes through `eraseRoom` (`SRV:lib/eraseRoom.ts`),
which needs `delete-c|p` scoped to the room being erased and, for a room that still has a `teamId`,
`delete-team-channel|group` in the main room; it throws `error-not-allowed` (Meteor error, has
`errorType`) otherwise. So `teams.delete` really needs `delete-team` **and** `delete-c|p` on the main
room, plus both child-room permissions for every room in `roomsToRemove`. The steps are not
transactional: a refusal while erasing a listed child aborts after the earlier children are gone; a
refusal on the main room aborts after every room was already detached (main room no longer
`teamMain`, team document and memberships left behind). With the default roles an owner has all of
them; Rustket must still gate on the full set (§5.3) and offer only deletable child rooms.

Web flow (`WEB:views/hooks/roomActions/useDeleteRoom.tsx`, `WEB:views/teams/contextualBar/info/DeleteTeam/*`):
step 1 lists `teams.listRooms` (all child rooms) with checkboxes "rooms to delete too"
(none selected by default, select-all toggle), step 2 confirms listing deleted and kept rooms;
toast `Team_has_been_deleted`, navigate home. `rooms.delete` on a main room is refused with
`error-cannot-delete-team-channel` **[live]**.

### 2.7 `POST teams.convertToChannel`

```json
{ "teamId": "…", "roomsToRemove": ["rid", …] }   // [] allowed here
```

Permission: `convert-team` in the main room. Erases `roomsToRemove` (each through `eraseRoom`, so the
§2.6 per-room delete permissions apply and a refusal aborts half-way), detaches the other child
rooms, deletes memberships and the team; the main room stays as a plain channel/group without
`teamMain`/`teamId` **[live]**. Unlike `teams.delete`, `roomsToRemove` is **not** filtered for the
main room (`Team.getMatchingTeamRooms` matches any room with that `teamId`, main room included):
sending the main room id erases it **[live, checker]**. Rustket must never put the main room in
`roomsToRemove` (nor in `rooms` of `leave`/`removeMember`). Web
(`WEB:views/teams/contextualBar/info/useConvertToChannel.tsx`, shown with `edit-team-channel` on the
main room): modal lists `teams.listRoomsOfUser?userId=me&canUserDelete=true` (rooms the user may
delete) to pick rooms to delete, toast `Team_converted_to_channel`.

### 2.8 Rooms of a team

**`GET teams.listRooms`** — query `teamId|teamName`, `filter` (case-insensitive substring of
`name`, not `fname`), `type` (`"autoJoin"` = only `teamDefault` rooms; any other value = all),
`offset`, `count`. Response `{rooms: IRoom[], total, count, offset}` (child rooms only, never the
main room; no defined sort). Rules (`TeamService.listRooms`, `Rooms.findPaginatedByTeamIdContainingNameAndDefault`):

- private team: caller must be a member or have `view-all-teams`, else 400 `user-not-on-private-team`;
- `view-all-team-channels` in the main room (owners, admins) → every child room;
- otherwise public rooms plus rooms in the caller's `__rooms` list. **Bug [live]:** a user whose
  `__rooms` is unset (never joined any room) gets *all* child rooms, private groups included,
  because `ids === undefined` drops the restriction. Never assume every listed room is openable;
  opening a private one fails with `error-not-allowed` / `result:false` from `loadHistory`.

**`GET teams.listChildren`** — `teamId|teamName|roomId` (`roomId` = any room of the team, resolved
through its `teamId`; a room outside a team → 400 `room-not-on-team`), `filter` (`name`/`fname`
substring), `type` (`channels`|`discussions`), `offset`, `count`, `sort` (default `{ts: 1}`, i.e.
creation order). Response `{data: IRoom[], total, offset, count}` (`count` echoes the request, not
the page size). Unknown team → **404** `"Resource not found"` **[live, checker]**. Requires team
membership (`error-invalid-team-not-a-member`, 400) **[live]**. Returns the **main room itself**
(also with `type=channels`, it matches `{teamId}`), child rooms (public, or private with a
subscription) and discussions whose `prid` is the main room (`Rooms.findChildrenOfTeam`) **[live]**.
This is the source for a "team tree" view.

**`GET teams.listRoomsOfUser`** — `teamId|teamName`, `userId` (required), `canUserDelete`
(`"true"` string), `offset`, `count`. Rooms of the team the given user is subscribed to, each with
`isLastOwner` (the user is the only owner there). `canUserDelete=true` keeps only rooms where
`userId` has `delete-team-channel|group` (main room scope) and `delete-c|p` (room scope). Caller
must be `userId` or have `view-all-team-channels`, else 403 **[live]**. Used by the leave and
remove-member dialogs.

**`POST teams.addRooms`** — `{teamId, rooms: [rid, …]}` (≥1). Permission `move-room-to-team` in
the main room (403 `error-no-permission-team-channel`). Each room must exist and be accessible
(`invalid-room`), not already in a team (`room-already-on-team`), and the caller must be its
**owner** (`error-no-owner-channel`) **[live]**. Posts `user-added-room-to-team` in the main room
for `c` rooms only. Response `{rooms}`. Candidate list:
`GET rooms.autocomplete.availableForTeams?name=` → `{items:[{_id,fname,name,t,avatarETag?}]}`: up to
10 rooms the caller **owns**, without `teamId`, not discussions, name prefix match, `name` optional
(`SRV:api/lib/rooms.ts` `findRoomsAvailableForTeams`) **[live]**.

**Create a room inside the team** — `channels.create` / `groups.create` with
`extraData: {teamId}`; requires `create-team-channel` / `create-team-group` in the main room
(`SRV:meteor-methods/rooms/createChannel.ts`, `createPrivateGroup.ts`; `error-team-not-found` for a
bad id). Channels post `user-added-room-to-team` (`SRV:lib/rooms/createRoom.ts`) **[live]**.

**`POST teams.removeRoom`** — `{teamId, roomId}`. Permission `remove-team-channel` in the main
room; without `view-all-team-channels` the caller must have access to the room. Unsets
`teamId`/`teamDefault`, posts `user-removed-room-from-team` for `c` rooms. Errors
`room-not-on-that-team`, `invalid-room` **[live]**. The room survives as a normal room.

**`POST teams.updateRoom`** (auto-join) — `{roomId, isDefault}` (`teamId` accepted and ignored).
Team is looked up from the room (`room-not-on-team`). Permission `edit-team-channel` in the **main
room**. With `ABAC_Enabled` and an ABAC-managed private room, `isDefault:true` fails with
`error-room-is-abac-managed`. Setting `true` immediately subscribes up to `API_User_Limit` (500)
team members (excluding the room creator) with the caller as inviter: each gets an `au` system
message in that room, which also counts as a mention for them (`alert:true, unread:1,
userMentions:1` on the new subscription) **[live]**. Setting `false` removes nobody. Response
`{room}` (stale `_updatedAt`, `usersCount` patched).

Web toast after enabling (`WEB:views/teams/contextualBar/channels/hooks/useToggleAutoJoin.ts`):
`Team_Auto-join_exceeded_user_limit` (info) when the main room's `usersCount > API_User_Limit`,
else `Team_Auto-join_updated` with the new member count.

**Deleting a child room** — `rooms.delete {roomId}`; the web requires `delete-c|p` in the room and
`delete-team-channel|group` in the main room (`WEB:views/hooks/roomActions/useDeleteRoom.tsx`).
Rustket already implements this rule (`rustket_core::permissions::can_delete_room`).

### 2.9 Members

**`GET teams.members`** — `teamId|teamName`, `status` (array; `status=x` and `status[]=x` both
work **[live]**), `username`, `name` (case-insensitive substrings), `offset`, `count`.
Response:

```json
{ "members": [ { "user": {"_id":"…","username":"…","name":"…","status":"offline","settings":{}},
                 "roles": ["owner"],                       // absent when none
                 "createdBy": {"_id":"…","username":"…"},  // who added them
                 "createdAt": "2026-11-07T23:07:50.718Z" } ],
  "total": 2, "count": 2, "offset": 0, "success": true }
```

Non-members without `view-all-teams` get an empty list, not an error **[live]**. Users hidden by
status visibility rules get a redacted `status` (`SRV:api/v1/teams.ts`). The web does **not** use
this endpoint for the members panel; it uses `rooms.membersOrderedByRole` on the main room
(`WEB:views/hooks/useMembersList.ts`), which already exists in Rustket (`room_members`), with the
title `Teams_members`. Use `teams.members` only where team-level `roles`/`createdBy` matter.

**Adding members** — the web adds through `channels.invite` / `groups.invite` on the main room
(`WEB:views/room/contextualBar/RoomMembers/AddUsers/AddUsers.tsx`), which creates the membership
(§1.1) and posts `added-user-to-team`. `POST teams.addMembers {teamId, members:[{userId, roles?}]}`
(≥1, unique) does the same plus optional roles; permission: at least one of `add-team-member`,
`edit-team-member` in the main room, and for every requested role `canChangeTeamRoles`
(`owner`→`set-owner`, `moderator`→`set-moderator`, `leader`→`set-leader` in the main room; any other
role → global `access-permissions`, plus `assign-admin-role` for `admin`) **[live]**. A
`roles-change` (`user.roleUpdate`) is broadcast only when `UI_DisplayRoles` is on.

**Changing roles** — prefer `channels|groups.addOwner/removeOwner/addModerator/removeModerator/
addLeader/removeLeader` on the main room (what the web member actions do): they post
`subscription-role-added/removed`, broadcast `roles-change` and mirror into the team membership
(§1.1). `POST teams.updateMember {teamId, member:{userId, roles}}` **replaces** both the team
member roles and the main-room subscription roles with `roles` (`[]` when omitted), posts nothing
and broadcasts no `roles-change` — only `subscriptions-changed` to that user (`TeamService.updateMember`).
Permission `edit-team-member` + `canChangeTeamRoles` on the symmetric difference **[live]**.

**`POST teams.removeMember {teamId, userId, rooms?}`** — permission `edit-team-member` in the main
room (note: moderators have it and can remove an owner who is not the last one). Fails with
`invalid-user` (inactive/unknown), `member-does-not-exist`, `last-owner-can-not-be-removed`
**[live]**. Removes the main-room subscription (`removed-user-from-team` system message by the
caller) and then the user from the listed child rooms (`ru` messages). Omit `rooms` when empty.
Web (`WEB:views/room/hooks/useUserInfoActions/actions/useRemoveUserAction.tsx`, gate `remove-user`):
on a main room opens `RemoveUsersModal` — step 1 lists `teams.listRoomsOfUser?userId=<target>`
to choose child rooms to remove them from too, step 2 confirms; toast `User_has_been_removed_from_team`.

**`POST teams.leave {teamId, rooms?}`** — no permission; fails for the last owner
(`last-owner-can-not-be-removed`) **[live]**. Posts `ult` in the main room; then leaves the listed
child rooms (`ul`). Web (`WEB:views/teams/contextualBar/info/useLeaveTeam.tsx`, `LeaveTeam/*`):
modal step 1 lists `teams.listRoomsOfUser?userId=me` with checkboxes ("also leave these"), rooms
with `isLastOwner` are shown but not selectable; step 2 confirms; toast
`Teams_left_team_successfully`, navigate home. `rooms.leave`/`channels.leave` on the main room
also leave the team (child rooms kept) and give `error-you-are-last-owner` for the last owner **[live]**.

**Joining** — a public team is joined like a channel (`channels.join` on the main room) **[live]**;
a private team only by invitation.

### 2.10 Error catalogue (teams)

| `error` (no `errorType`) | Endpoints | User text (en) |
|---|---|---|
| `team-does-not-exist` | most | "The team does not exist." |
| `Team not found` / `Room not found` | `teams.info` | "Team not found." |
| `team-name-already-exists`, `room-name-already-exists` | create, convertToTeam | "A room or team with this name already exists." |
| `error-team-creation` | create | "Could not create the team (check the name)." |
| `room-already-on-team` | addRooms | "This room already belongs to a team." |
| `error-no-owner-channel` | addRooms | "You must be an owner of the room to move it." |
| `error-no-permission-team-channel` | addRooms (403) | permission text |
| `invalid-room`, `room-not-on-team`, `room-not-on-that-team` | add/remove/updateRoom | "The room is not part of this team." |
| `user-not-on-private-team` | listRooms, listRoomsOfUser | "You are not a member of this private team." |
| `error-invalid-team-not-a-member` | listChildren | same |
| `member-does-not-exist` | removeMember, leave | "Not a team member." |
| `last-owner-can-not-be-removed` | removeMember, leave | "Set another owner before leaving." |
| `error-you-are-last-owner` (has `errorType`) | rooms/channels.leave | same |
| `error-cannot-delete-team-channel` (has `errorType`) | rooms.delete on main room | "Delete the team instead." |
| `error-room-is-abac-managed` | updateRoom | "Auto-join is not available for this room." |
| `could-not-remove-member` | removeMember (defensive, `removeMembers` returned false) | generic error |
| `Resource not found` (404) | listChildren (unknown team) | "Team not found." |
| `error-not-allowed` (has `errorType`) | delete, convertToChannel (`eraseRoom` refused a room, §2.6) | "You are not allowed to delete some of these rooms." |
| `unauthorized` (403) | any | generic permission text |

## 3. Teams: realtime behaviour

There is no team-specific stream. Everything arrives through the per-user streams the session
already subscribes (`docs/protocol.md` "Realtime"). Observed as a team member **[live]**
(`ddp_probe.py`):

| Action | Events for an affected member |
|---|---|
| `teams.create` with the user in `members` | `subscriptions-changed inserted` (main room, no team fields), then `rooms-changed inserted` **twice**: first without `teamId`/`teamMain`, then with them (`Rooms.setTeamMainById` after creation) |
| `teams.addRooms` | `rooms-changed updated` for the main room (new `user-added-room-to-team` message) — members not subscribed to the moved room get nothing about it |
| `teams.updateRoom isDefault:true` | `subscriptions-changed inserted` for the child room, `rooms-changed inserted`/`updated` (several, same `_updatedAt` while `msgs`/`usersCount` change) with `teamDefault: true` |
| `teams.removeRoom` | `rooms-changed updated` for the child room **without** `teamId`/`teamDefault` keys, then `updated` for the main room (system message) |
| `teams.delete` | `rooms-changed updated` (main room, last system message), then `subscriptions-changed removed` for the main room (no `rooms-changed removed`); child rooms stay, detached |
| member added / removed / left | `subscriptions-changed inserted` / `removed` for the main room + system messages via `__my_messages__` |

Reducer rules:

1. `rooms-changed` carries the full room: **replace**, never merge, otherwise a removed `teamId`
   lingers (Rustket's `Workspace::apply_room` replaces; keep it that way). Accept events whose
   `_updatedAt` equals the stored one (several real updates share a timestamp).
2. A repeated `inserted` for a known room is an update.
3. Team indexes (main room by `teamId`, children by `teamId`) are **derived** from the room map on
   every change, not stored separately, so detaching/deleting needs no extra bookkeeping.
4. A child room that loses `teamId` moves from its team group back to Channels in the sidebar; a
   main room that loses `teamMain` (convert to channel) becomes a normal channel.
5. After a reconnect `rooms.get?updatedSince` returns rooms whose `_updatedAt` moved; every team
   mutation goes through `updateOne`/`updateMany`, which set `_updatedAt`
   (`RC:packages/models/src/models/BaseRaw.ts` `setUpdatedAt`), so delta sync sees them **[code]**.
   Team *documents* themselves (name/type/members) have no delta API: refetch `teams.info` for an
   open team panel after `Resync`.

System messages relevant to teams (`RC:packages/message-types/src/registrations/common.ts`, texts
from `RC:packages/i18n/src/locales/{en,ru}.i18n.json`); `msg` holds the username/room name:

| `t` | Posted in | en text |
|---|---|---|
| `added-user-to-team` | main room | "added @{msg} to this team" |
| `removed-user-from-team` | main room | "removed @{msg} from this team" |
| `ujt` | main room | "joined this team" (`u` = the joiner) |
| `ult` | main room | "left this team" |
| `user-converted-to-team` | converted room | "converted #{msg} to a team" |
| `user-converted-to-channel` | main room | "converted #{msg} to channel" |
| `user-added-room-to-team` | main room (only for `c` rooms) | "added #{msg} to this team" |
| `user-removed-room-from-team` | main room (only for `c` rooms) | "removed #{msg} from this team" |
| `user-deleted-room-from-team` | main room | "deleted #{msg}" |
| `au`, `ru`, `ul`, `r`, `room_changed_privacy`, `subscription-role-added/removed` | as usual | existing keys |

Current locale state (checked): `crates/rustket/locales/room_render.yml` already has timeline keys
for all of them (`room_render.sys.ujt`, `ult`, `added_user_to_team`, `removed_user_from_team`,
`converted_to_team`, `converted_to_channel`, `added_room_to_team`, `removed_room_from_team`,
`deleted_room_from_team`, plus `discussion_created` and `videoconf`); `preview.yml` only has
`preview.system.ujt`/`ult`/`discussion-created`, so the sidebar-preview keys for the other team
types are what is missing (en + ru).

## 4. Permissions

Defaults on the dev server **[live]** (`permissions.listAll`, identical to
`SRV:lib/authorization/constant/permissions.ts`). "Scope" = the room id the server passes to
`hasPermission`; room-scoped permissions are satisfied by global roles or by roles on the
subscription of that room (`docs/protocol.md`, `rustket_core::permissions::has_permission`).

| Permission | Default roles | Server scope | Used by (server) | Web gate (where it differs) |
|---|---|---|---|---|
| `create-team` | admin, user | global (+ room for convert) | `teams.create`, `channels/groups.convertToTeam` (+ `edit-room` on the room) | convert also needs `canEditRoom` |
| `delete-team` | admin, owner | main room | `teams.delete` (which also needs `delete-c\|p` on the main room via `eraseRoom`, §2.6) | web gates Delete on `delete-c\|p` + `delete-team-channel\|group` (`useDeleteRoom.tsx`) and never checks `delete-team` |
| `convert-team` | admin, owner | main room | `teams.convertToChannel` | web gates on `edit-team-channel` in the main room instead (`useConvertToChannel.tsx`) |
| `edit-team` | admin, owner | main room | `teams.update` | web edits via `rooms.saveRoomSettings` (server: `edit-room`); the Team info "Edit" item is gated on `edit-team-channel` in the main room (`TeamsInfoWithData.tsx`) |
| `add-team-member` | admin, owner, moderator | main room | `teams.addMembers` (or `edit-team-member`) | web adds via `channels/groups.invite` (`add-user-to-*`) |
| `edit-team-member` | admin, owner, moderator | main room | `teams.addMembers`, `updateMember`, `removeMember` | web remove gated on `remove-user` |
| `move-room-to-team` | admin, owner, moderator | main room | `teams.addRooms` | same |
| `create-team-channel` / `create-team-group` | admin, owner, moderator | main room | `channels.create`/`groups.create` with `extraData.teamId`; `saveRoomSettings` type change of a child room | same |
| `delete-team-channel` / `delete-team-group` | admin, owner, moderator | main room | `rooms.delete` of a child room (+ `delete-c\|p`), `listRoomsOfUser?canUserDelete` | same |
| `edit-team-channel` | admin, owner, moderator | main room | `teams.updateRoom` | web checks it scoped to the **child** room id (`useToggleAutoJoin.ts`) |
| `remove-team-channel` | admin, owner, moderator | main room | `teams.removeRoom` | web checks the child room id (`useRemoveRoomFromTeam.tsx`) |
| `view-all-team-channels` | admin, owner | main room | `listRooms` (all rooms), `removeRoom`/`updateRoom` on rooms without access, `listRoomsOfUser` of others | — |
| `view-all-teams` | admin | global / main room | `teams.listAll`, `info`, `members`, `listRooms` of private teams | — |
| `set-owner` / `set-moderator` / `set-leader` | admin, owner | main room | team role changes (`canChangeTeamRoles`) | — |
| `access-permissions`, `assign-admin-role` | admin | global | team roles other than owner/moderator/leader | — |
| `call-management` | admin, owner, moderator, user | room | `video-conference.start` (§7.3) | "Video call" button (§7.9) |
| `videoconf-ring-users` | admin, owner, moderator, user | global (`start`) / route-level (`ring`) | `allowRinging` on `start`, `video-conference.ring`, `ring` of `add-participants` | — |

Rustket rule: always evaluate team permissions **scoped to the main room** (the server truth),
found via `team_main_room(ws, team_id)` (already in `crates/rustket-core/src/permissions.rs`). When
the user is not subscribed to the main room (possible for admins or users who left), only global
roles apply; that matches the server.

## 5. Teams: UI to copy

### 5.1 Sidebar

- Grouping (legacy sidebar `WEB:sidebar/hooks/useCategoryList.ts`): order Incoming calls →
  Unread → custom category → Favorites → **Teams** (`room.teamMain`) → Discussions (`room.prid`) →
  omnichannel → Channels (`c`/`p`, child rooms included) → DMs. Rustket's `room_list.rs` already
  has `SectionKind::Teams` driven by `is_team()`.
- Icons (`WEB:hooks/useRoomIcon.tsx`): main room `team` (public) / `team-lock` (private);
  discussion `baloons`; ABAC rooms `team-shield` / `hash-shield`.
- The new navigation (`WEB:views/navigation/sidepanel/hooks/useChannelsChildrenList.ts`) has a
  "Teams" filter that, when a team is selected, lists the main room first and then every
  subscription with that `teamId` or `prid = main room`, sorted by `lm` desc (unread first when
  "unread only"). Rustket option (later): expandable team rows in the Teams section showing the
  subscribed child rooms and discussions with the same ordering.

### 5.2 Room header

- Main room: team icon; toolbox entries (group `team`, `WEB:views/room/lib/getRoomGroup.ts`):
  Team info (`useTeamInfoRoomAction`, order 1), Team channels (`useTeamChannelsRoomAction`,
  order 2), Discussions (order 3, if `Discussion_enabled`), Teams members (`useMembersListRoomAction`,
  order 7), Video call (featured, §7.9), Calls (EE, §7.11), plus the common ones.
- Child room: a "Back to {team} team" button (`Back_to__roomName__team`) before the title
  (`WEB:views/room/Header/ParentRoom/ParentTeam.tsx`), shown when the team is public or the user
  is a member (`teams.info` + `users.listTeams?userId=<me>` for membership, `WEB:views/room/hooks/useUserTeamsQuery.ts`;
  hide on `unauthorized`); click opens the main room. Rustket can decide from the store: show when
  the main room is in the store (member) or `teams.info` succeeds and `type == 0`. `rooms.info` of
  the child room already returns `team:{_id,name,type,roomId}` and `parent:{_id,name,fname,t}` (the
  main room) **[live, checker]**, so one call is enough when the main room is not in the store.
- Discussion: "Back to {parent} channel" (`Back_to__roomName__channel`,
  `WEB:views/room/Header/ParentRoom/ParentDiscussion/*`): the web uses the user's subscription to
  `prid` when it has one, else `rooms.info?roomId=<prid>`; `rooms.info` of the discussion itself also
  carries `parent:{_id,name,fname,t,prid,u}` (`SRV:api/v1/rooms.ts`). Name = `fname` for
  discussions/federated, else `name` (`fname` when `UI_Allow_room_names_with_special_chars`).

### 5.3 Team info panel (`WEB:views/teams/contextualBar/info/*`)

Avatar, name, topic, announcement, description (same as room info), and a menu
(`useTeamActions.ts`): Hide (`rooms.hide`), Edit (room edit form → `rooms.saveRoomSettings`),
Leave (§2.9 flow), Convert to channel (§2.7 flow), Delete (§2.6 flow, danger). Web gates: Edit =
`edit-team-channel`, Convert = `edit-team-channel`, Delete = `delete-c|p` + `delete-team-channel|group`
(all main-room scoped). Rustket gates (server truth): Edit = `can_edit_room`, Leave = always (server
decides last owner), Convert = `convert-team`, Delete = `delete-team` **and** `delete-{t}` on the
main room (§2.6: the server needs both, and fails half-way without the second). In the delete and
convert dialogs only offer child rooms that pass `can_delete_room`.

### 5.4 Team channels panel (`WEB:views/teams/contextualBar/channels/*`, `Kind::TeamChannels`)

- Header actions: "Create new" (gate `create-team-channel` or `create-team-group`; opens the
  create-room dialog with the team preset → `channels|groups.create` with `extraData.teamId`) and
  "Add existing" (gate `move-room-to-team`; multi-select from
  `rooms.autocomplete.availableForTeams`, debounced, then `teams.addRooms`).
- Filter select All / Auto-join (persisted, `localStorage channels-list-type`), search box
  (800 ms debounce, `filter=`), infinite list of 10 per page (`teams.listRooms`, next offset while
  `offset + count < total`).
- Row: room avatar/icon, name, topic; click opens the room (join preview for public rooms the user
  is not in). Kebab: Auto-join checkbox (`edit-team-channel` → `teams.updateRoom` toggle + toast),
  Remove from team (`remove-team-channel` → confirmation `Team_Remove_from_team_modal_content` →
  `teams.removeRoom`), Delete (`can_delete_room`).
- Refresh after each mutation and on `rooms-changed` for any room with this `teamId`.

### 5.5 Members panel

Same component as rooms (`Kind::Members`), title "Teams Members", list from
`rooms.membersOrderedByRole` on the main room; add users via `channels/groups.invite`; "Remove from
team" (label `Remove_from_team`) opens the two-step remove dialog (§2.9). Role actions call the
room role endpoints on the main room.

### 5.6 Strings

New i18n keys (en/ru) under the owning component's prefix; reference texts from
`RC:packages/i18n/src/locales/{en,ru}.i18n.json`: `Teams_Info` ("Team info" / "Информация о
команде"), `Team_Channels` ("Team Channels" / "Чаты команды"), `Teams_members` ("Teams Members" /
"Участники команды"), `Team_Auto-join` ("Auto-join" / "Автодобавление"), `Team_Remove_from_team`
("Remove from team" / "Удалить из команды"), `Convert_to_channel`, `Back_to__roomName__team`,
`Teams_left_team_successfully`, `Team_Auto-join_updated`, `Team_Auto-join_exceeded_user_limit`
(several have no Russian upstream; write them).

## 6. Discussions management

Rustket already has `RestClient::rooms_create_discussion` and `permissions::can_create_discussion` /
`can_start_discussion_from`; this section fixes the remaining behaviour.

### 6.1 Data model

- A discussion is a `c` or `p` room with `prid` (parent room id), a random `name`
  (`Random.id()`), the user-visible title in `fname`, `topic` = parent room name (kept in sync by
  `afterRoomNameChange` `updateTopicDiscussion`), `description` = text of the source message
  (empty for encrypted parents) **[live]**. Type follows the parent
  (`roomCoordinator.getRoomDirectives(parent.t).getDiscussionType`; a DM parent gives `p`).
  Subscriptions carry `prid` **[live]**. Discussions cannot be nested (`error-nested-discussion`).
- In the parent, creation posts a system message `t: "discussion-created"`, `msg` = discussion
  title, `drid` = discussion id, `attachments: [quote of the source message]` when created from a
  message (`SRV:meteor-methods/messages/createDiscussion.ts` `createDiscussionMessage`). The
  message gets `dcount` (message count) and `dlm` (last message date) once the discussion has
  messages (`incrementAndNotifyParentRoomWithParentMessage`,
  `SRV:hooks/messages/propagateDiscussionMetadata.ts`); updates arrive as message edits through
  `__my_messages__`. The **source** message (`pmid`) is not marked at all **[live]**.
- When created from a message, the discussion gets a first message by the creator with `msg: ""`
  and the quote attachment (`mentionMessage`), and the source message author is invited.

### 6.2 Create — `POST rooms.createDiscussion`

```jsonc
{ "prid": "<parent rid>", "t_name": "Title",      // both required
  "pmid": "<message id>",                          // optional: start from a message
  "users": ["username", …],                        // invited (usernames)
  "reply": "first message",                        // optional; forbidden when encrypted
  "topic": "…",                                    // optional, default parent name
  "encrypted": false }                             // default: parent.encrypted
```

Response `{"discussion": IRoom & {rid, inserted: true}}` (`usersCount: 0` at that moment) **[live]**.
Gates: setting `Discussion_enabled`; `start-discussion` or `start-discussion-other-user` scoped to
`prid`; the caller must be able to send in the parent (`canSendMessage`). Errors **[live]**:

| Case | Response |
|---|---|
| nested | 400 `errorType: error-nested-discussion` |
| missing `t_name` | 400 `invalid-params` |
| unknown/inaccessible `prid` | 400 `{"error":"[error-invalid-room]","errorType":"error-invalid-room"}` |
| `encrypted` + `reply` | 400 `error-invalid-arguments` "Encrypted discussions must not receive an initial reply." |
| E2E forced for private rooms and unencrypted `p` discussion | `error-encrypted-private-rooms-enforced-discussion` **[code]** |
| `pmid` from another room | `error-invalid-arguments` "Root message room ID does not match parent room ID" **[code]** |

Dedup bug **[live]**: the code intends one discussion per `pmid` (`Rooms.findOne({prid, pmid})`),
but `pmid` is never stored on the room, so a second call creates a second discussion. Rustket
should warn ("a discussion for this message already exists") when the timeline already has a
`discussion-created` message whose attachment `message_link` points at that message.

Web form (`WEB:components/CreateDiscussion/CreateDiscussion.tsx`): parent room (fixed when started
from a room/message, otherwise an autocomplete), name, topic, members (usernames), first message
(disabled when encrypted), encrypted toggle (locked on when the parent is encrypted). Sends
`{prid, t_name, users, reply: encrypted ? undefined : firstMessage, topic, encrypted, pmid?}`.

### 6.3 Listing

| Endpoint | Returns | Notes |
|---|---|---|
| `GET chat.getDiscussions?roomId&text&offset&count&sort` | `{messages, count, offset, total}` **[live, checker]**: `discussion-created` messages (`_id, rid, t, msg, drid, dcount?, dlm?, attachments, u, ts`), query `{rid, drid: {$exists: true}, msg: /text/i}`, default sort `ts` desc (`SRV:api/lib/messages.ts` `findDiscussionsFromRoom`) | what the web Discussions panel uses (`WEB:views/room/contextualBar/Discussions/useDiscussionsList.ts`, page 10, `text` filters `msg`); kept live by applying message events whose `drid` is set |
| `GET rooms.getDiscussions?roomId\|roomName&offset&count&sort` | `{discussions: IRoom[], count, offset, total}`, default sort `fname` **[live]** | room documents; includes discussions the caller cannot open (no access filter beyond the parent). `fields`/`query` are ignored unless `ALLOW_UNSAFE_QUERY_AND_FIELDS_API_PARAMS=true`. This is what Rustket's `room/panel/rooms.rs` `Mode::Discussions` uses today |
| `GET teams.listChildren?type=discussions` | discussions of a team's main room (only those the caller can see) | §2.8 |

The panel (`Kind::Discussions`) should list the `discussion-created` messages (title, author,
`dcount` "N messages", `dlm` relative time) because they carry the counters. A deleted discussion
does **not** stay in that list: deletion unsets `drid` on the message, and `chat.getDiscussions`
only returns messages with `drid` **[live, checker]**. The "Discussion deleted" (not clickable) state
belongs to the `discussion-created` message in the **timeline**, which survives with `drid` unset.
The Discussions toolbox entry is hidden inside a discussion (`room.prid`) and when
`Discussion_enabled` is false (`WEB:hooks/roomActions/useDiscussionsRoomAction.ts`).

### 6.4 Deleting and cascades

- `rooms.delete {roomId: <discussion>}` (gate `delete-c|p` in the discussion) deletes it; the
  parent's `discussion-created` message stays with `drid`/`dcount`/`dlm` **unset**
  (`afterDeleteRoom` `CleanDiscussionMessage`) **[live]**. That unset is a bare
  `Messages.updateMany` with **no realtime notification**: a client watching the parent room gets
  no `stream-room-messages` event **[live, checker]**. The message's `_updatedAt` is bumped
  (`BaseRaw.updateMany`), so the next history delta sync picks it up **[code]**. Until then Rustket
  keeps a stale `drid`; opening it fails (`rooms.info` → `not-allowed`), which is the moment to
  show "Discussion deleted" and patch the cached message locally. The same hook also unsets
  `discussionRid` on video conferences that pointed at the deleted room.
- Deleting the `discussion-created` message (`chat.delete`) deletes the discussion room
  (`afterDeleteMessage` → `deleteRoom(message.drid)`) **[live]**. The delete confirmation for such a
  message must say so.
- Deleting a room deletes all its discussions (`DeleteDiscussionChain`) **[code]**, so team deletion
  and `teams.delete … roomsToRemove` remove their discussions too.
- `rooms.cleanHistory` accepts `ignoreDiscussion` to keep `discussion-created` messages
  (`RC:packages/rest-typings/src/v1/rooms.ts`; bulk-delete event in `docs/protocol.md`).

## 7. Video conferencing (8.9)

### 7.1 Architecture and editions

- Rocket.Chat does not run media itself. A **provider** is registered by an Apps-Engine app
  (Jitsi, Pexip, Google Meet, BigBlueButton, …) through the video-conference bridge
  (`RC:apps/meteor/app/apps/server/bridges/videoConferences.ts` `registerProvider` →
  `SRV:lib/videoConfProviders.ts`). The server creates the call record and the room message and
  asks the app for URLs (`generateUrl`, `customizeUrl`).
- Active provider (`videoConfProviders.getActiveProvider`): none registered → no provider;
  `VideoConf_Default_Provider` set → that one if registered, else none; unset → the only one if
  exactly one is registered, else none.
- Provider capabilities (`RC:packages/core-typings/src/VideoConferenceCapabilities.ts`):
  `mic`, `cam`, `title`, `persistentChat`, `embedded`. `embedded: true` means the call renders
  inside Rocket.Chat's own page (LiveKit-style SFU) and `join` returns an empty URL; RN treats
  `providerName === "livekit"` that way (`RN:app/lib/methods/videoConf.ts`: it checks
  `capabilities` and opens the conference page without calling `join`). In the 8.9.1 sources the
  only registration path is the Apps-Engine bridge (`registerProvider(info.name, info.capabilities)`),
  so `embedded` is whatever the app declares; there is no built-in LiveKit provider in the checkout.
  UNVERIFIED: which published provider app (if any) declares `embedded: true`.
- **Community (no license, the dev server):** every call is a group conference
  (`type: "videoconference"`, no ringing) — `videoConfTypes.getTypeForRoom` falls through to the
  default (`SRV:lib/videoConfTypes.ts`); only `livechat` for `t: "l"` rooms. No call history panel,
  no call window, `VideoConf_Enable_*` settings absent (treat as `true`, the web defaults,
  `WEB:hooks/roomActions/useVideoCallRoomAction.ts`). Joined users are recorded only if the app
  reports them through the bridge (`update` → `VideoConf.addUser`). `start` never rings on CE, but
  `video-conference.ring` / `add-participants {ring:true}` are not license-gated, so with a provider
  app a CE server can still emit `ring` events and conference desktop notifications (§7.5); the web
  ignores `ring` there because the conference-window setting does not exist on CE.
- **Enterprise module `videoconference-enterprise`** (`EE:configuration/videoConference.ts`,
  `EE:settings/video-conference.ts`): with `allowRinging`, a 2-person DM starts a `direct` call in
  status `CALLING` (ringing handshake, §7.6), a group DM starts a `videoconference` with
  `ringing: true` (server rings all other members); `onJoinVideoConference` records joins; adds the
  per-room-type enable settings, persistent chat, the conference window, mobile ringing, and the
  "Calls" history tab. Detect it with `licenses.info` → `license.activeModules` contains
  `"videoconference-enterprise"` (readable by any user **[live]**).

### 7.2 Data model

`RC:packages/core-typings/src/IVideoConference.ts`:

```ts
enum VideoConferenceStatus { CALLING = 0, STARTED = 1, EXPIRED = 2, ENDED = 3, DECLINED = 4 }
type VideoConferenceType = 'direct' | 'videoconference' | 'livechat' | 'voip';
interface IVideoConference {
  _id; type; rid; status;
  users: IVideoConferenceUser[];        // conference *members*, not "currently in the call"
  messages: { started?: mid; ended?: mid };
  url?: string;                         // provider base URL (not per-user)
  createdBy: {_id, username, name}; createdAt;
  endedBy?: {_id, username, name}; endedAt?;
  providerName: string; providerData?: object;   // providerData is stripped by every REST endpoint
  ringing?: boolean; discussionRid?: string;
  // group only: title: string; anonymousUsers: number
}
interface IVideoConferenceUser { _id; username; name; avatarETag: string|null; ts;
  joined?: boolean; joinedAt?; declined?: boolean; declinedAt?; leftAt?; leftReason?: 'reported'|'timeout';
  lastSeenAt?; ringingAt? }
```

- "Has joined" = `joined !== false` (absent means joined: old entries were written only on join);
  "in the call now" = has joined and no `leftAt` (`hasJoinedVideoConference`, `isInVideoConference`).
- "Ringing now" = `ringingAt` within `VIDEO_CONF_RINGING_WINDOW_MS = 15000` and not declined after it
  (`isRingingVideoConferenceMember`).
- `RING_RECIPIENTS_LIMIT = 10`: `add-participants` accepts at most 10 usernames and only rings when
  1..10 were actually added (`shouldRingRecipients`). The start-time `ring` of a group-DM call
  (`startGroup` → `notifyUsersOfRoom`) is **not** capped.
- `VideoConferenceChatAccess = {rid, name, type, membersWithoutAccess: uid[], canInvite}`.
- `JoinableVideoConference = {callId, name, createdAt, usersCount, participants (≤2, CALL_FACES_SHOWN), joined, declined, ringingAt?}`.
- `type: "voip"` records are internal (VoIP history) and never managed by apps (`isCallManagedByApp`).

Instructions returned by `start` (`DirectCallInstructions | ConferenceInstructions | LivechatInstructions`):
`{type:"direct", callId, calleeId}`, `{type:"videoconference", callId, rid}`, `{type:"livechat", callId}`,
each plus `providerName`.

### 7.3 REST endpoints

Handler `SRV:api/v1/videoConference.ts`, params `RC:packages/rest-typings/src/v1/videoConference/*.ts`,
service `SRV:services/video-conference/service.ts`. Rate limits are per user (`rateLimiterOptions`;
disabled on the dev server, active in production by default — cache `providers`/`capabilities`).

| Endpoint | Body / query | Success | Rate limit | Notes |
|---|---|---|---|---|
| `GET video-conference.providers` | — | `{data:[{key,label}]}` | 3/1 s | `[]` on dev **[live]** |
| `GET video-conference.capabilities` | — | `{providerName, capabilities}` | 3/1 s | dev: 400 `no-videoconf-provider-app` **[live]**; also `no-active-video-conf-provider`, `video-conf-provider-not-configured` |
| `POST video-conference.start` | `{roomId, title?, allowRinging?}` | `{data: Instructions & {providerName}}` | 3/60 s | needs `call-management` (room scope) and `canSendMessage`, else 403 `"Not allowed"` (also for an unknown room **[live]**); `allowRinging` honoured only with `videoconf-ring-users`; any failure → 400 with `error` = diagnosed provider error **[live]** |
| `POST video-conference.join` | `{callId, state?: {mic?, cam?}}` | `{url, providerName, callId, rid}` | 2/5 s | `authOrAnonRequired`; access = room access to `call.rid` for URL providers, `canAccessConference` (membership or chat-room access) only for embedded ones, so a member added by `add-participants` without room access cannot join a URL-provider call **[code]**; unknown/inaccessible → 400 `invalid-params` **[live]**; `url: ""` for embedded providers; `failed-to-get-url`; extra `state` keys → `invalid-params` **[live]** |
| `POST video-conference.cancel` | `{callId}` | `{}` | 3/60 s | caller gives up a ringing **direct** call (`status → DECLINED`, `endedAt`); unknown → 400 `invalid-params` **[live]**; not a ringing direct call → 400 `{"error":"invalid-call"}` / `invalid-call-status` (plain `Error` → `API.v1.failure(e.message)`, `SRV:api/ApiClass.ts`) **[code]** |
| `POST video-conference.decline` | `{callId}` | `{}` | 10/60 s | records the caller's decline on their member entry (never ends the call); unknown → 404 `"Resource not found"` **[live]** |
| `POST video-conference.leave` | `{callId}` | `{}` | 20/60 s | marks the caller as left; empties end the call after 10 s grace; 404 unknown **[live]** |
| `POST video-conference.heartbeat` | `{callId}` | `{}` | 20/60 s | presence lease for **embedded** calls only; 404 unknown **[live]** |
| `POST video-conference.ring` | `{callId, userId}` | `{rang: bool}` | 5/60 s | `videoconf-ring-users`; re-rings one member not in the call; 404 unknown **[live]** |
| `POST video-conference.add-participants` | `{callId, users: [username] (1..10), ring?}` | `{added: [uid]}` | 5/60 s | membership only, no room access; 404 unknown **[live]** |
| `POST video-conference.rename` | `{callId, title (1..200)}` | `{}` | 10/60 s | creator of a running group call only (`error-not-allowed`, `error-invalid-video-conf`); 404 **[live]** |
| `POST video-conference.share-chat` | `{callId, mode: "invite"\|"discussion"}` | `{rid}` | 5/60 s | gives members without room access the chat; 404 **[live]** |
| `GET video-conference.info?callId=` | — | `VideoConference` (no `providerData`) + `capabilities` + `chatAccess` | 15/3 s | unknown/inaccessible → 400 `invalid-params`; missing `callId` → 400 `error-invalid-params` **[live]** |
| `GET video-conference.joinable` | — | `{calls: JoinableVideoConference[]}` | 30/60 s | running calls with somebody in them, in rooms the user is subscribed to or where they are a member; `{calls:[]}` on dev **[live]** |
| `GET video-conference.list?roomId&offset&count` | — | `{data: VideoConference[], offset, count, total}` | 3/1 s | history of a room, newest first; also returns calls whose `discussionRid` is that room (`VideoConference.findPaginatedByRoomId`); unknown/inaccessible room → 400 `invalid-params`; empty on dev **[live]** |

Access rule for the 404 family and `info`: `Authorization.canAccessConference(call, uid)` =
conference membership or access to the room / discussion the chat lives in
(`RC:packages/core-services/src/types/IAuthorization.ts`; comments in
`RC:packages/streamer/src/notifications.module.ts` `streamVideoConference.allowRead`).

Rustket needs `providers`, `capabilities`, `start`, `join`, `cancel`, `decline`, `info`, `list`,
`joinable`; `ring`/`add-participants`/`rename` are cheap optional extras; `leave`/`heartbeat`/
`share-chat` belong to the in-product call window and must **not** be called by Rustket (the
browser page that runs an embedded call reports its own presence).

### 7.4 No provider configured

**[live]** on the dev server:

- `GET video-conference.providers` → `{"data":[],"success":true}`.
- `GET video-conference.capabilities` → 400 `{"success":false,"error":"no-videoconf-provider-app"}`
  (no side effect).
- `POST video-conference.start` (team main room, `GENERAL` as admin, a DM with `allowRinging`) →
  400 `{"success":false,"error":"no-videoconf-provider-app"}` **and** an ephemeral message pushed
  on `stream-notify-user` `{uid}/message` (`VideoConf.diagnoseProvider` → `createEphemeralMessage`
  → `notify.ephemeralMessage`):

```json
{ "_id": "1794092973243", "rid": "<room>", "private": true, "groupable": false,
  "u": {"_id": "rocket.cat", "username": "rocket.cat"},
  "ts": {"$date": 1794092973243}, "_updatedAt": {"$date": 1794092973243},
  "msg": "**Conference call not available**: Conference call apps can be installed in the Rocket.Chat marketplace by a workspace admin.",
  "md": [ … ] }
```

  The text is the i18n key named by the error, prefixed `admin-` for admins, in the user's
  language. Error codes (`RC:apps/meteor/lib/videoConference/constants.ts`) and texts
  (`RC:packages/i18n/src/locales/en.i18n.json`, ru exists for all six):

| Error | User | Admin (`admin-…`) |
|---|---|---|
| `no-videoconf-provider-app` | "Conference call not available: Conference call apps can be installed in the Rocket.Chat marketplace by a workspace admin." | "Conference call not enabled: Conference call apps are available in the Rocket.Chat marketplace." |
| `no-active-video-conf-provider` (several apps, no default) | "Conference call not enabled: A workspace admin needs to enable the conference call feature first." | "…Configure conference calls in order to make it available on this workspace." |
| `video-conf-provider-not-configured` (app not fully configured) | same as above | same as above |

Other errors seen through `start`/`join`: `video-conf-provider-unavailable`, `apps-engine-not-loaded`,
`invalid-call`, `failed-to-get-url`; web client-side codes `error-videoconf-unexpected`,
`error-videoconf-join-failed`, `error-videoconf-missing-url`,
`error-videoconf-cant-start-call-with-manager-busy`, `error-videoconf-direct-call-accept-timeout`
(en only upstream).

Web behaviour (`useVideoCallRoomAction.ts`): the "Video call" button stays visible; clicking calls
`capabilities` first and on failure opens `VideoConfConfigModal`
(`WEB:views/room/contextualBar/VideoConference/VideoConfConfigModal.tsx`): "App not enabled" /
"Missing configuration" (admin), a list of provider apps (Jitsi included with Community; Pexip and
Google Meet premium), and for admins an "Open settings" button to the Video_Conference admin group.
Rustket: same order (capabilities before start, so no ephemeral spam), a dialog with the localized
error text, for admins an "Open admin settings" button (`OpenUrl {server}/admin/settings/Video_Conference`).

### 7.5 Realtime

| Stream / event | Payload | Who | Use |
|---|---|---|---|
| `stream-notify-room` `{rid}/videoconf` | `[callId]` | room watchers (already in `NOTIFY_ROOM_EVENTS`) | refetch `video-conference.info` for blocks with that `callId` (web `useVideoConfDataStream`) |
| `stream-notify-user` `{uid}/video-conference` | `[{action, params:{callId, uid, rid}}]` | the user (subscribed globally, `NOTIFY_USER_EVENTS`) | ringing handshake and call lifecycle (§7.6) |
| `stream-video-conference` `{callId}/updated` | `[]` | conference members / chat room members | conference doc changed (rename, roster); optional |
| `stream-notify-user` `{uid}/notification` | `INotificationDesktop`: `title` = ringer's name (or room name for chat invites), `text` "You were invited to a conference", `requireInteraction: true`, `actions:[{action:"join", title:"Join call"}]`, `payload: {_id: <callId>, rid, sender, conferenceId: <callId>, message:{msg}, audioNotificationValue:"none", type?, name?}` (`type`/`name` only when the recipient can open the room; note `payload._id` is the **call** id, not a message id) | users rung through `ringUsers` (`video-conference.ring`, `add-participants` with `ring`, caller arrival in conference-window mode) or invited by `share-chat` (`SRV:services/video-conference/service.ts` `notifyUsersAboutConference`). `ring`/`add-participants` are **not** license-gated: with any provider app this can arrive on CE | persistent desktop notification with a Join button (§7.10) |
| `stream-notify-user` `{uid}/message` | ephemeral message | the caller | provider errors (§7.4) |

**[live]**: subscribing another user's `{uid}/video-conference` → `nosub not-allowed`;
`stream-video-conference` for an unknown call → `nosub not-allowed` (a refused subscription is
never retried by the server, so subscribe only after `info` succeeded); `stream-notify-room-users`
cannot be subscribed (`nosub not-allowed`, write-only).

Publishing (`RC:packages/streamer/src/notifications.module.ts`):

- `stream-notify-user` method with params `["{targetUid}/video-conference", {action, params:{callId, uid, rid}}]`
  is relayed only if `VideoConf.validateAction` passes: caller and `uid` can access `rid`, the call
  exists, and (unless `action == "end"`) it is not ended/expired/declined. **[live]**: fake
  `callId`, impersonated `uid`, missing fields, non-object `params` → method result `null`,
  nothing delivered. A `call` about an already ended call makes the server send `end` back to the
  creator.
- **`stream-notify-room-users` `["{rid}/video-conference", <anything>]` is relayed verbatim to the
  `{uid}/video-conference` stream of every other member of `rid`** as long as the sender is
  subscribed to `rid`, with no validation of the payload **[live]** (`ddp_spoof.py`: a fake `call`
  and `{"anything":1}` were delivered; nothing when the sender was not in the room). It is a
  legacy path the current clients do not use.
- The same relay also accepts **`{rid}/userData`** and forwards it verbatim to every other member's
  `{uid}/userData` stream (`streamRoomUsers.allowWrite` takes `video-conference` and `userData`)
  **[live, checker]**: a member of a shared room published `{"type":"updated","id":<victim uid>,
  "diff":{"name":"SPOOF"}}` and the victim's `{uid}/userData` subscription received it. Rustket
  subscribes `userData` (`NOTIFY_USER_EVENTS`); `Session::observe` used to copy
  `diff.username`/`diff.name` into the cached own user, so any co-member could rename the user
  inside Rustket. Fixed in the session layer: `userData` is only a hint for a rate-limited `/me`
  re-read (`SessionEvent::MeChanged`), see `docs/protocol.md`.

Consequence for Rustket: every `{uid}/video-conference` event is untrusted input. Accept an
incoming `call`/`ring` only after (1) shape validation (`action` a known string, `callId`/`uid`/`rid`
non-empty strings ≤ 64 chars), (2) `rid` is a room in the store, (3) `GET video-conference.info`
confirms `_id == callId`, `rid == params.rid`, `endedAt` absent, and for `call`: `type == "direct"`,
`status == CALLING`, `createdBy._id == params.uid`; for `ring`: the current user is in `users`
(cache the result per `callId`). Rate-limit popups per sender (e.g. one new call per sender per 5 s)
and drop events while the session is not logged in. Lifecycle events (`canceled`, `end`, `join`,
`accepted`, `rejected`, `confirmed`) only act on calls already in the local state with a matching
`callId`, and `accepted`/`rejected` only when `params.uid` is the callee of our outgoing call.

### 7.6 Direct-call ringing (EE `videoconference-enterprise`)

Constants (`WEB:lib/VideoConfManager.ts`, mirrored by `RN:app/sagas/videoConf.ts`):
`CALL_INTERVAL = 3000` ms (caller re-sends `call`), `CALL_ATTEMPT_LIMIT = 10`, `CALL_TIMEOUT = 10000`
ms (callee drops a ring without refresh), `ACCEPT_TIMEOUT = 5000` ms (callee waits for `confirmed`),
dismissed call ids remembered `CALL_TIMEOUT × 20` = 200 s. Server: 40 s after creation
(`startDirect` `setTimeout(…, 40000)`) it **always** runs `endDirectCall` — `end` to the caller and to
every room member who has not joined — and then cancels the call (`DECLINED`) if it is still
`CALLING`. So an `end` arrives 40 s after start even for a call that was accepted and is running:
`end` means "stop ringing", not "the call is over" (the web only clears ringing/outgoing state on
it and ignores it for calls it is not ringing about) **[code]**.

Caller (A) in a DM with B (`VideoConfManager.startCall` / `callUser`, conference window off):

```
A: POST video-conference.start {roomId, allowRinging:true}  → {type:"direct", callId, calleeId:B}
   (server: call CALLING, message `videoconf` posted, push to B if VideoConf_Mobile_Ringing)
A: every 3 s, up to 10 times: publish {B}/video-conference {action:"call", params:{uid:A, rid, callId}}
   show "Calling…" popup + dialtone
B: shows incoming popup (refreshes its 10 s timeout on every `call`)
B accepts: publish {A}/video-conference {action:"accepted", params:{uid:B, rid, callId}}; start 5 s accept timer
A on `accepted` for its current call: stop ringing; POST video-conference.join → open URL;
   publish {B}/video-conference {action:"confirmed", params:{uid:A, rid, callId}}
B on `confirmed`: POST video-conference.join → open URL
B rejects: publish {A}/… {action:"rejected", …}; (call-window mode also POST video-conference.decline)
A on `rejected`: stop; POST video-conference.cancel unless A already joined
A gives up (10 attempts or user hangs up): publish {B}/… {action:"canceled"}; POST video-conference.cancel unless joined
Server → both: `join` when a member's join is recorded (EE `onJoinVideoConference` → `addUser` →
   `updateDirectCall`; to the caller with `uid` = joiner, and to the joiner; stops ringing; the caller
   treats a foreign `join` as accepted, its own as "joined elsewhere"),
   `end` from `endDirectCall`: when the provider app ends the call and at the 40 s timer (see above)
   — `POST video-conference.cancel` itself sends no `end`; the callee learns about a cancel from
   the caller's `canceled` publish and from the `{rid}/videoconf` refresh
```

Callee rules (`onVideoConfNotification`): `call` → new incoming call (rejecting any older ringing
call from the same user) or refresh; ignored if the call id was dismissed. `canceled`/`end` → drop
the incoming call (an in-flight accept fails with `error-videoconf-direct-call-accept-canceled|ended`).
`join` with `params.uid == me` → the user joined from another device: dismiss locally.
"Mute and dismiss" only silences locally (no event). Accept-timeout without `confirmed` →
`error-videoconf-direct-call-accept-timeout`. The user can be busy with only one outgoing call
(`isBusy`); starting another fails locally (`error-videoconf-cant-start-call-with-manager-busy`).

Group-DM ringing (`type:"videoconference"`, `ringing:true`): the server sends `ring` to all other
subscribers when starting (`startGroup` → `notifyUsersOfRoom(…,'ring',…)`). The web **ignores**
`ring` unless `VideoConf_Conference_Window_Enabled` (`VideoConfManager.onVideoConfNotification`);
in that mode `ring` is a one-shot, non-handshake ring: accept = join directly, decline =
`video-conference.decline`, and it may ring again after a dismissal (`allowRingingAgain`).
Recommendation: follow the web exactly (honour `ring` only when the setting is `true`), so users
are not rung twice by Rustket and silently by the web.

Conference-window mode (EE setting on): `start` for a DM does not ring from the client; the server
rings the callee when the caller arrives (`ringCalleeOnCallerArrival`, sends `ring` + desktop
notification + push). Rustket cannot host the window, so in that mode it should behave like the web
when the user starts a call: open `{server}/conference/new?rid=<rid>` in the browser (the web's
start path, `WEB:providers/VideoConfProvider.tsx` `startCall`) and join existing calls by opening
`{server}/conference/<callId>`.

### 7.7 The `videoconf` message

Messages with `t: "videoconf"` carry `blocks: [{type:"video_conf", callId, blockId: callId,
appId:"videoconf-core"}]`, `groupable: false`, and `msg` = localized "Conference Call" (or the
thread title) so previews and notifications are not empty (`VideoConfService.createMessage`). The
sender is the creator (`start` passes the user) or the provider app user. Livechat calls instead
carry a `section` block and an `actions` block with a `joinLivechat` button holding the URL
(`createLivechatMessage`). Any user can forge `t:"videoconf"` and blocks (`docs/uikit.md` §3.2), so
the block's data always comes from `video-conference.info`, never from the message.

Rendering states (`RC:packages/fuselage-ui-kit/src/blocks/VideoConferenceBlock/VideoConferenceBlock.tsx`,
also `docs/uikit.md` §3.4); `joined` = members with a username and `joined !== false`:

| State (from `info`) | Title | Footer |
|---|---|---|
| loading / `info` failed | skeleton (web never resolves errors) — Rustket: "Call details unavailable" after an error | — |
| `endedAt` set, `type == direct` | "Call ended" | button "Call again" (I created it) / "Call back"; "Call was not answered" if status EXPIRED/DECLINED |
| `endedAt` set, other types | "Call ended" | avatar stack (max 3) + "joined" / "+N joined" (or "N joined" when avatars are off), else "Call was not answered" for EXPIRED/DECLINED |
| `type == direct`, status CALLING | "Calling" (incoming icon) | "Waiting for answer" |
| otherwise (ongoing) | "Call ongoing" | primary "Join", avatar stack + joined text |

Header actions: info icon (sends the UIKit block action `info` with `blockId` = callId to
`videoconf-core`; the core app `SRV:modules/core-apps/videoconf.module.ts` answers with a
`modal.open` titled `Video_Conference_Info` ("Meeting Information") showing the provider's info
blocks, or a single section `**Meeting URL**: <call.url>` (i18n `Video_Conference_Url`) when the app
returns none; handled by the UIKit layer, `docs/uikit.md` §3.4) and a discussion icon when
`discussionRid` (opens that room). `join` and `callBack` are handled locally
(`WEB:uikit/hooks/useMessageBlockContextValue.ts`, keyed on the action's `blockId`: callId for
`join`, rid for `callBack`; it first resets the preferences to `{mic:true, cam:false}`): `join` →
`video-conference.join` → open URL; `callBack` → the same flow as the header "Video call" button for
that room. Refetch `info` on `{rid}/videoconf` for that `callId`; once `endedAt` is set the data is
final (web stops refetching on mount, `useVideoConfData`).

### 7.8 Opening a call

`POST video-conference.join {callId, state:{mic, cam}}` → `{url, providerName, callId, rid}`:

- `url` non-empty: open it with the system browser (`Effect::OpenUrl`). Accept only absolute
  `https:` URLs, plus `http:` when the server itself is `http:` (dev); refuse other schemes. The URL
  is per user (`customizeUrl` adds name/mic/cam/title, e.g. Jitsi config fragments), so never cache
  it across users or reuse `call.url` from `info`. The web opens it in a new tab, severing
  `window.opener` (`WEB:views/room/contextualBar/VideoConference/hooks/useVideoConfOpenCall.tsx`);
  the Electron desktop app instead opens an internal window
  (`window.RocketChatDesktop.openInternalVideoChatWindow`) — not applicable to Rustket.
- `url == ""` (embedded provider): open `{server}/conference/<callId>` in the browser (the web route
  `/conference/:id`, `WEB:startup/routes.tsx`; RN builds the same, `RN:app/lib/methods/helpers/buildConferenceUrl.ts`).
  The page needs a logged-in browser session; tell the user so on first use. RN refuses this for
  non-https servers (plain `http:` is allowed only for `localhost`/`127.0.0.1`/`[::1]`,
  `RN:app/lib/methods/helpers/isConferenceUrl.ts` `isSecureHttpUrl`) because its window carries the
  login token; Rustket passes no token.
- mic/cam: the web sends the user's last choice from the start/incoming popup (default
  `{mic:true, cam:false}`), only when `capabilities.mic`/`cam`. Rustket keeps the same two toggles in
  its popups and remembers them per server in `UiState`.
- After opening, Rustket's part is done; it does not track presence in the external call.

### 7.9 "Video call" room action

Web gate (`WEB:hooks/roomActions/useVideoCallRoomAction.ts`):

- room groups enabled by settings: DMs and group DMs (`VideoConf_Enable_DMs`), private groups
  (`…_Groups`), teams (`…_Teams`), channels (`…_Channels`), omnichannel when
  `Omnichannel_call_provider == "default-provider"` (all default `true`; absent on CE → `true`);
- visible when `call-management` (room scope) and the user is not muted in the room and the DM is
  not with oneself (`uids.length == 1`);
- disabled (with tooltip "Video call unavailable for this type of room") when federated, read-only
  without `post-readonly`, or archived;
- click (ignored while already calling or ringing): `capabilities` → error dialog (§7.4) or the
  start popup (`StartCallPopup`: "Start a call", mic/cam toggles if supported, "Start call"); in
  conference-window mode go straight to the browser. After `start`: `direct` → outgoing popup
  ("Calling…", cancel); other types → `join` immediately.

Rustket: same rules (`rustket_core::permissions::can_start_video_call(ws, rid)` plus the settings),
the button in `room/header.rs`; capabilities cached per session (TTL 5 min, invalidated by
`stream-apps` `apps` events `app/added|removed|statusUpdate`, `docs/uikit.md` §6.5) so the dialog can
also be shown without a round trip. User-info panel of a DM partner offers the same action
(`WEB:views/room/hooks/useUserInfoActions/actions/useVideoCallAction.ts`).

### 7.10 Popups, sounds, notifications

- Popups (`WEB:views/room/contextualBar/VideoConference/VideoConfPopups/*`): modal-ish cards stacked
  bottom-right over everything (one per incoming call, not dismissed; plus the outgoing/start
  popup). Incoming: title "Incoming call from", room avatar/name (or the caller for members without
  room access), mic/cam toggles (from `info.capabilities`), buttons Accept / Decline and an
  "×" ("Mute and dismiss", local only).
- Sounds while ringing / calling: `ringtone` / `dialtone`, looped, volume = user preference
  `voipRingerVolume` (`WEB:providers/CustomSoundProvider/CustomSoundProvider.tsx`); files
  `{server}/sounds/ringtone.mp3`, `{server}/sounds/dialtone.mp3` (`RC:apps/meteor/public/sounds/`).
  Rustket: play through the desktop sound player (Phase 8), stop on accept/decline/timeout.
- Desktop notification for a ring when the window is not focused: Rustket creates one itself for a
  validated incoming `call` (title caller, body "Incoming call", actions Accept/Decline,
  `requireInteraction`), closed when the call is resolved. Server-originated conference
  notifications (§7.5, payload `conferenceId`, action `join`) are shown by the desktop component
  with a "Join call" action → `video-conference.join` → open URL; do not play the message sound
  (`audioNotificationValue: "none"`). Current state: `crates/rustket/src/desktop/notifications.rs`
  already adds the `join` action (`ACTION_JOIN`) when `payload.conference_id` is set, but activating
  it does the same as the default action (open the room); the join itself is still to do, and
  `payload._id` there is the call id, so it must not be used as a message id to jump to.
- Legacy sidebar "Incoming calls" group (`useCategoryList.ts` `hasIncomingCalls`): rooms with a
  ringing incoming call are listed first. Optional in Rustket.
- Mobile push for calls (`notificationType: "videoconf"`) is irrelevant for desktop.

### 7.11 Calls history panel (EE)

`useCallsRoomAction` shows a "Calls" tab only with `videoconference-enterprise`
(`WEB:hooks/roomActions/useCallsRoomAction.ts`): `video-conference.list?roomId&offset&count` (newest
first), each row: title or room, creator, start time, status, joined users, "Join" for running calls.
Rustket: optional `Kind::Calls`, hidden unless the module is active.

### 7.12 Joinable / ongoing calls

`GET video-conference.joinable` (§7.3) powers the web's ongoing-calls list in conference-window
mode, refreshed on every `{uid}/video-conference` event (`WEB:views/conference/hooks/useJoinableCalls.ts`;
embedded calls announce themselves with `started`). Rustket can show a small "Ongoing calls" list
(name, participants count, Join) refreshed the same way, on `Resync`, and at most every 60 s
(rate limit 30/min); hide it when the list is empty.

## 8. Voice calls: `media-signal`, `media-calls.*`, call history

8.9 ships a native WebRTC voice-call system ("Team voice calls", internal user↔user calls and SIP
via drachtio) that is separate from video conferences:

- Server: `SRV:services/media-call/service.ts` on top of `@rocket.chat/media-calls`
  (`RC:ee/packages/media-calls/src`), signalling types in `RC:packages/media-signaling/src/definition`.
- Web: `WEB:providers/MediaCallProvider.tsx` enables it only with the license module `teams-voip`
  **and** `allow-internal-voice-calls` or `allow-external-voice-calls` (defaults admin, user
  **[live]**); the DM header gets a "voice call" button (`WEB:hooks/roomActions/useMediaCallRoomAction.ts`,
  2-person DMs, not blocked, not federated). Settings `VoIP_TeamCollab_*` (SIP, drachtio, mobile
  ringing, screen sharing).
- Transport: client → server signals are JSON **strings** published with the DDP method
  `stream-notify-user` params `["{uid}/media-calls", "<serialized ClientMediaSignal>"]` (never
  relayed; `MediaCall.processSerializedSignal`); server → client on `stream-notify-user`
  `{uid}/media-signal` (`ServerMediaSignal` objects, broadcast to all of the user's sessions; some
  carry `toContractId` for one session).
- Client signals: `register {contractId, oldContractId?, requestSignals?}`, `request-call`,
  `answer {callId, contractId, answer: accept|reject|ack|unavailable, supportedFeatures?}`,
  `local-sdp`, `local-state`, `negotiation-needed`, `dtmf`, `hangup {reason}`, `transfer`, `error`.
  Server signals: `registered {toContractId, calls, activeCalls}`, `new {callId, service:"webrtc",
  kind:"direct", role: caller|callee, self, contact, …}`, `notification {callId, notification:
  accepted|active|hangup|trying, signedContractId?, hangupReason?}`, `remote-sdp`, `request-offer`,
  `rejected-call-request`. Feature list `audio`, `screen-share`, `transfer`, `hold`.
- REST: `POST media-calls.answer {callId, contractId, answer, supportedFeatures?}` → `{call}`,
  `GET media-calls.state` → `{calls}` (not-over calls of the user), `GET media-calls.stateSignals?contractId`,
  `GET media-calls.info?callId` (`SRV:api/v1/media-calls.ts`); `GET call-history.list
  ?filter&direction&state[]&offset&count`, `GET call-history.info?historyId|callId`
  (`SRV:api/v1/call-history.ts`). After a call, a message with an `info_card` block
  (`appId: "media-call-core"`, action `open-history`) is posted in the DM (`docs/uikit.md` §2.3).
- **[live]** on CE: the service runs without the license: publishing `register` with
  `contractId: "rustket-probe"` returned `{uid}/media-signal`
  `[{"type":"registered","toContractId":"rustket-probe","calls":[],"activeCalls":[]}]`;
  `media-calls.state` → `{"calls":[]}`; `media-calls.info?callId=nope` → 404 `"Resource not found"`;
  `call-history.list` → `{"items":[],"count":0,"offset":0,"total":0}`. No call could be placed
  (no second WebRTC peer, no license in the web UI).

What Rustket can realistically do without WebRTC (no audio stack, pure-Rust constraint):

1. **Do not `register` a contract and never answer `ack`/`accept`.** Registering announces a
   reachable agent; the callee state goes to "ringing" when any session acks
   (`RC:ee/packages/media-calls/src/internal/agents/CallSignalProcessor.ts`), and Rustket could
   not take the call.
2. Optional passive notice (later phase): subscribe `{uid}/media-signal`; on `new` with
   `role: "callee"` show "Incoming voice call from {contact.displayName} — answer it in the web app"
   (desktop notification, action "Open in browser" → `{server}/direct/<username>`); clear it on
   `notification` `hangup` for that `callId`. Treat the payload as untrusted display data only.
3. Render the `info_card` call messages read-only and offer a read-only call history panel from
   `call-history.list` (only when `teams-voip` is active).
4. Hide every "voice call" start action.

## 9. Settings the client reads

All public unless noted (values **[live]** on the dev server).

| Setting | Dev value | Effect |
|---|---|---|
| `Discussion_enabled` | `true` | discussions toolbox, create discussion, persistent chat |
| `API_User_Limit` | `500` | auto-join cap message (§2.8) |
| `UI_DisplayRoles` | `true` | `roles-change` on `teams.addMembers` with roles |
| `UI_Allow_room_names_with_special_chars`, `UTF8_Channel_Names_Validation` | — | team/room name display and validation |
| `E2E_Enable`, `E2E_Enabled_Default_PrivateRooms`, `E2E_Force_Encryption_For_Private_Rooms` | — | create team / discussion encrypted toggle |
| `VideoConf_Default_Provider` | `""` | informational |
| `VideoConf_Enable_DMs/Channels/Groups/Teams` (EE) | absent | Video call button per room type; absent → `true` |
| `VideoConf_Conference_Window_Enabled` (EE) | absent | §7.6: honour `ring`, open `/conference/…` for start/join; absent → `false` |
| `VideoConf_Enable_Persistent_Chat`, `VideoConf_Persistent_Chat_Mode` (EE) | absent | calls may get a discussion (`discussionRid`) or a thread on the call message; Rustket only follows `discussionRid` links |
| `VideoConf_Mobile_Ringing` (EE) | `false` | push only |
| `Omnichannel_call_provider` | `"none"` | video call in omnichannel rooms only when `"default-provider"` |
| `ABAC_Enabled` (private) | — | auto-join refusal (§2.8), server-side only |

License modules (from `licenses.info`): `videoconference-enterprise` (§7.1, §7.11), `teams-voip` (§8).

## 10. Proposed Rust layout

### 10.1 `crates/rocketchat` (protocol)

`models/team.rs` (move `TeamInfo` from `models/room.rs`, keep a type alias):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TeamType { #[default] Public, Private, Unknown(u8) }      // serde from/to 0/1
pub struct Team { id, name, kind: TeamType, room_id, created_by: Option<UserRef>,
                  created_at: Option<RcDate>, updated_at: Option<RcDate> }
pub struct TeamListItem { #[serde(flatten)] team: Team, rooms: Option<u64>, number_of_users: Option<u64> }
pub struct TeamMember { user: TeamMemberUser /* id, username, name, status */, roles: Vec<String>,
                        created_by: Option<UserRef>, created_at: Option<RcDate> }
pub struct TeamAutocompleteItem { id /* main room id */, name, fname, t: RoomType, team_id, avatar_etag }
pub struct TeamRoomOfUser { #[serde(flatten)] room: Room, is_last_owner: bool }
```

`Room` gains `team_default: Option<bool>` (currently missing). Tolerant serde as everywhere.

`rest/teams.rs` (`impl RestClient`), every method taking `TeamRef<'_>` (`Id(&str)` /
`Name(&str)`, serialized as `teamId`/`teamName`):

```rust
teams_create(&CreateTeam) -> Result<Team>                  // CreateTeam { name, private, members, owner, read_only, topic, broadcast, encrypted }
teams_info(TeamRef) -> Result<Team>
teams_list(Page) / teams_list_all(Page) -> Result<Paged<TeamListItem>>
teams_autocomplete(&str) -> Result<Vec<TeamAutocompleteItem>>
teams_update(TeamRef, name: Option<&str>, kind: Option<TeamType>) -> Result<()>
teams_delete(TeamRef, rooms_to_remove: &[&str]) -> Result<()>          // omits the key when empty
teams_convert_to_channel(TeamRef, rooms_to_remove: &[&str]) -> Result<()>   // callers must never pass the main room (§2.7)
channels_convert_to_team(rid) / groups_convert_to_team(rid) -> Result<Team>
teams_list_rooms(TeamRef, filter: Option<&str>, auto_join_only: bool, Page) -> Result<Paged<Room>>
teams_list_children(TeamRef, filter, Option<ChildKind>, Page) -> Result<Paged<Room>>
teams_list_rooms_of_user(TeamRef, uid, can_user_delete: bool, Page) -> Result<Paged<TeamRoomOfUser>>
teams_add_rooms(TeamRef, &[&str]) -> Result<Vec<Room>>
teams_remove_room(TeamRef, rid) -> Result<Room>
teams_update_room(rid, is_default: bool) -> Result<Room>
teams_members(TeamRef, &MemberFilter, Page) -> Result<Paged<TeamMember>>
teams_add_members(TeamRef, &[(uid, &[role])]) / teams_update_member(TeamRef, uid, &[role]) -> Result<()>
teams_remove_member(TeamRef, uid, rooms: &[&str]) / teams_leave(TeamRef, rooms: &[&str]) -> Result<()>
rooms_autocomplete_available_for_teams(name: Option<&str>) -> Result<Vec<Room>>
```

Discussions (`rest/rooms.rs`, extend): `rooms_create_discussion(&CreateDiscussion)` with
`pmid`, `topic`, `encrypted`; `chat_get_discussions(rid, text, Page) -> Paged<Message>` (keys
`messages`, `count`, `offset`, `total`); `rooms_get_discussions(rid, Page) -> Paged<Room>`.
`teams_autocomplete` reads the `teams` key; add `users_list_teams(uid) -> Vec<UserTeam>`
(`users.listTeams`, key `teams`; `UserTeam { #[serde(flatten)] team: Team, is_owner: bool }`).

`models/videoconf.rs`:

```rust
pub enum VideoConfStatus { Calling, Started, Expired, Ended, Declined, Unknown(u8) }
pub enum VideoConfKind { Direct, Group, Livechat, Voip, Unknown(String) }
pub struct VideoConfUser { id, username, name, avatar_etag, ts, joined: Option<bool>, joined_at,
                           declined: Option<bool>, declined_at, left_at, ringing_at }   // + has_joined(), in_call(), ringing_now(now)
pub struct VideoConference { id, kind, rid, status, users: Vec<VideoConfUser>, messages: VideoConfMessages,
                             url: Option<String>, created_by: UserRef, created_at, ended_by, ended_at,
                             provider_name, ringing: Option<bool>, discussion_rid, title, anonymous_users,
                             capabilities: Option<VideoConfCapabilities>, chat_access: Option<Value> }
pub struct VideoConfCapabilities { mic, cam, title, persistent_chat, embedded: Option<bool> }
pub enum StartInstructions { Direct { call_id, callee_id }, Conference { call_id, rid }, Livechat { call_id } } // + provider_name
pub struct JoinResult { url: String, provider_name: String, call_id: Option<String>, rid: Option<String> }
pub struct JoinableCall { call_id, name, created_at, users_count, participants, joined, declined, ringing_at }
/// `{uid}/video-conference` payload after strict validation (§7.5); `None` for anything else.
pub struct VideoConfSignal { action: VideoConfAction, call_id, uid, rid }
pub enum VideoConfAction { Call, Ring, Canceled, Accepted, Rejected, Confirmed, Join, End, Started, Other(String) }
```

`rest/videoconf.rs`: `videoconf_providers`, `videoconf_capabilities`, `videoconf_start(rid, title,
allow_ringing)`, `videoconf_join(call_id, mic, cam)`, `videoconf_cancel`, `videoconf_decline`,
`videoconf_info`, `videoconf_list(rid, Page)`, `videoconf_joinable`, `videoconf_ring`,
`videoconf_add_participants`, `videoconf_rename`. Map the provider codes of §7.4 to
`Error::Api { error_type: Some(code) }` so the UI can match them.

Session (`session/events.rs`, `session/mod.rs`): replace `SessionEvent::VideoConference(Value)` by
`VideoConference(VideoConfSignal)` (drop and log at debug what fails validation); add
`Session::notify_video_conference(target_uid, action, call_id, rid)` = DDP method
`stream-notify-user` `["{target}/video-conference", {action, params:{callId, uid: me, rid}}]`. Do
not subscribe `{uid}/media-signal` by default (§8; a later opt-in adds it to `NOTIFY_USER_EVENTS`
with a golden-test update). Live tests in `crates/rocketchat/tests/live.rs`: the teams round trip
of §0 with throwaway users, discussions create/list/delete, `video-conference.*` no-provider error
shapes and the `{uid}/message` ephemeral, notify-user validation (fake call dropped).

### 10.2 `crates/rustket-core`

- `teams.rs` (pure, derived from `Workspace` rooms): `main_room_of(team_id)`, `team_of(rid)`,
  `children_of(team_id) -> Vec<rid>` (subscribed rooms with that `teamId` plus discussions of the
  main room, main room first, then `lm` desc), used by the sidebar and the header back button.
- `permissions.rs`: ids `DELETE_TEAM`, `CONVERT_TEAM`, `EDIT_TEAM`, `ADD_TEAM_MEMBER`,
  `EDIT_TEAM_MEMBER`, `MOVE_ROOM_TO_TEAM`, `CREATE_TEAM_CHANNEL`, `CREATE_TEAM_GROUP`,
  `EDIT_TEAM_CHANNEL`, `REMOVE_TEAM_CHANNEL`, `VIEW_ALL_TEAM_CHANNELS`, `VIEW_ALL_TEAMS`,
  `CALL_MANAGEMENT`, `VIDEOCONF_RING_USERS`; functions `can_delete_team` (= `delete-team` + `delete-{t}`
  on the main room, §2.6), `can_convert_team`,
  `can_edit_team`, `can_add_team_rooms`, `can_create_team_room(kind)`, `can_toggle_autojoin`,
  `can_remove_team_room`, `can_manage_team_members`, `can_change_team_role(role)`,
  `can_convert_to_team(rid)` (= `create-team` + `can_edit_room` + not in a team + no `prid` + not
  federated), `can_start_video_call(rid)` (§7.9) — all team ones scoped to `team_main_room`.
- `videoconf.rs`: a pure `CallManager` (no I/O, `Instant`-driven like `typing.rs`):
  state `incoming: HashMap<CallId, Incoming {uid, rid, deadline, accept_deadline, handshake, verified}>`,
  `outgoing: Option<Outgoing {call_id, rid, callee, attempts, next_ring, joined}>`,
  `dismissed: HashMap<CallId, Instant>`, `info_cache`; inputs `Signal(VideoConfSignal)`,
  `Verified(call_id, Result<VideoConference>)`, `Started(StartInstructions)`, `Accept`, `Decline`,
  `Dismiss`, `Hangup`, `Tick(now)`; outputs a `Vec<CallCommand>` (`FetchInfo`, `Publish{to, action}`,
  `RestCancel`, `RestDecline`, `Join{call_id}`, `ShowIncoming`, `HideIncoming`, `ShowOutgoing`,
  `HideOutgoing`, `PlayRinger`, `StopRinger`, `PlayDialer`, `StopDialer`, `Notify`, `Error(code)`).
  Constants of §7.6. Unit-test the handshake from both sides, the 10-attempt give-up, accept
  timeout, `join`-from-another-device, `end` during accept, `end` for an already accepted call
  (the 40 s server timer, §7.6: must be a no-op), spoofed signals, dismissal TTL.
- `Workspace`: store the license modules (`licenses.info` at sync, refreshed on `Resync`) and the
  cached capabilities; per-room `video_conferences: HashMap<CallId, VideoConference>` filled by the
  timeline and invalidated by `RoomVideoConference` events.

### 10.3 `crates/rustket` (UI)

Ownership follows `crates/rustket/COMPONENTS.md`; new shared pieces need the orchestrator to
assign an owner.

| Piece | Module (owner) | Notes |
|---|---|---|
| Team info panel | `room/panel/team_info.rs` (panel agent), new `panel::Kind::TeamInfo` | §5.3; or a team variant of `Kind::Info` |
| Team channels panel | today `room/panel/rooms.rs` `Mode::TeamChannels` (existing `Kind::TeamChannels`); split into `room/panel/team_channels.rs` (panel agent) when the actions of §5.4 land | §5.4 |
| Members panel team mode | `room/panel/members.rs` (panel agent) | §5.5, remove-from-team dialog |
| Discussions panel | today `room/panel/rooms.rs` `Mode::Discussions` on `rooms.getDiscussions`; move to `chat.getDiscussions` (§6.3), optionally as `room/panel/discussions.rs` (panel agent), `Kind::Discussions` | §6.3 |
| Calls panel (EE) | `room/panel/calls.rs`, new `Kind::Calls` | §7.11 |
| Header: team icon, back-to-team/channel, Video call button, Team toolbox entries | `room/header.rs` (panel agent) | §5.2, §7.9 |
| Create team, create discussion | `dialogs/create_room.rs` (account agent, `Kind::Team`/`Discussion` exist as stubs) | §2.1, §6.2 |
| Multi-step dialogs: leave team, delete team, convert to channel, remove member, add existing rooms, convert to team | `dialogs/team_*.rs` (new, panel agent) | §2.6–§2.9 |
| `videoconf` block | `room/render/` (timeline agent) | §7.7, emits `RenderMsg::JoinCall(call_id)`, `CallBack(rid)`, `OpenRoom(discussion_rid)` |
| Call popups (start / outgoing / incoming), ringing sounds, ongoing-calls list | new app-level component `src/calls.rs` (+ `calls/…`), driven by `CallManager`; overlay layer in `app/view.rs` | §7.6, §7.10, §7.12 |
| Conference notifications with Join, call notifications with Accept/Decline | `src/desktop/` (desktop agent) | §7.10; hook `on_call_event` next to `on_notification` |
| Sidebar team grouping / tree, call indicator | `workspace/sidebar.rs` (shell) | §5.1 |

Locale files: `room_panel.yml`, `room_header.yml`, `room_render.yml`, a new `calls.yml`, and the
system-message keys of §3 in `preview.yml` + `room_render.yml`; every key in en and ru.

### 10.4 Phasing

1. Protocol: team/discussion/videoconf models and REST, typed `VideoConfSignal`, live tests.
2. Teams read side: `Room.team_default`, team indexes, header back buttons, Team info, Team channels
   (list, filter, open), members panel title, system messages.
3. Teams write side: create (dialog), add existing / create inside, auto-join, remove room,
   leave / delete / convert flows, convert channel to team, member removal, roles via room endpoints.
4. Discussions: create (from room and from message), panel from `chat.getDiscussions`, delete
   warnings, back-to-parent header.
5. Video conferences: Video call button + capabilities dialog, start/join (external browser),
   `videoconf` block states and actions, `{rid}/videoconf` refresh.
6. Ringing (only matters with `videoconference-enterprise`): `CallManager`, popups, sounds,
   desktop notifications, conference-window mode, joinable list, Calls panel.
7. Voice calls: read-only call history and `info_card` rendering; optional passive incoming notice.

## 11. Open questions / unverified

- **[unverified]** Everything that needs a provider app or the `videoconference-enterprise` /
  `teams-voip` modules: real `start`/`join` responses and URLs, `info` documents with members, the
  ringing handshake events, `ring`, conference desktop notifications, call messages after a voice
  call. Installing an app or a license changes global server state, which this repo forbids on the
  shared dev server. When a test server with Jitsi exists, capture fixtures for `start`, `join`,
  `info` (CALLING / STARTED / ENDED, direct and group) and the `{uid}/video-conference` sequence.
- UNVERIFIED: `video-conference.cancel` on a call that is not a ringing direct call: from source a
  400 with `error: invalid-call|invalid-call-status` (§7.3), not exercised (needs a provider).
- UNVERIFIED (code only): the half-finished states of `teams.delete` / `teams.convertToChannel` when
  `eraseRoom` refuses a room (§2.6, §2.7). Reproducing needs a role-permission change, which is a
  global setting on the shared dev server.
- UNVERIFIED (code only): the 40 s `end` for an already accepted direct call (§7.6) and the server
  `join` events; both need a provider app and the `videoconference-enterprise` module.
- The `{rid}/userData` relay through `stream-notify-room-users` (§7.5) is a server weakness that
  affects Rustket's session layer, not only calls; the session now distrusts `userData` (re-reads
  `/me`), see `docs/protocol.md`.
- `teams.listRooms` leaking private child rooms to users with an empty `__rooms` (§2.8) is a server
  bug; it may be fixed in later 8.x releases. The client must not rely on either behaviour.
- `teams.members` loads every active user matching the filters on the server; prefer
  `rooms.membersOrderedByRole` for large teams.
- Whether the server accepts `channels.convertToTeam` on a discussion was not exercised (the web
  hides it); Rustket hides it too.
