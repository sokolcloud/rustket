# End-to-end encryption (Rocket.Chat 8.9 E2EE) — implementation spec

Target: byte-compatible with the Rocket.Chat 8.9.1 web client and the official React Native
client (RN), so that messages, room keys, uploads and the server-side private key backup
written by Rustket are readable by them and vice versa.

Path abbreviations used for citations:

- `RC:` = `.ref/rocketchat/` (Rocket.Chat 8.9.1, tag `Release 8.9.1`)
- `RN:` = `.ref/rn/` (Rocket.Chat.ReactNative, commit `1b487ca`)
- `WEB:` = `RC:apps/meteor/client/lib/e2ee/` (the web E2EE implementation)

Verification legend:

- **[live]** — verified against the dev server (8.9.1, `http://localhost:3000`) on 2026-11-07 with
  throwaway users/rooms (created, exercised, deleted). The Node 24 WebCrypto API (same API as the
  browser) was used to produce real keys and ciphertexts.
- **[src]** — taken from source; not exercisable live.
- **[vec]** — checked with a test vector (WebCrypto and cross-checked with Python `cryptography`).
- **UNVERIFIED:** — a claim that could not be checked against the sparse `.ref/` sources or the dev
  server; treat it as a hint, not a fact.

Independent re-check on 2026-11-08: every vector in §13 (and the word-list hash in §15) was
re-derived with Python `cryptography`; the REST behaviours of §5 (key upload, `setRoomKeyID` race,
`updateGroupKey`, accept/reject, `getUsersOfRoomWithoutKey`, `fetchUsersWaitingForGroupKey` query
forms, `resetRoomKey` errors and lock, `chat.sendMessage`/`chat.update` with `content`, pin,
`rooms.media`/`rooms.mediaConfirm`/download, `users.resetE2EKey`) were re-run live with throwaway
users and rooms (deleted afterwards). DDP stream behaviour (§6) was checked against the source only.

Second skeptical pass, also 2026-11-08: all §13 vectors, upstream spec decryptions and the
word-list hash re-derived again (Python `cryptography`); every `file:line` citation re-checked
against `.ref/`; the permission defaults, the settings of §2.1, the stored `e2e` field, plaintext
`chat.update {text}`, the `resetRoomKey` 403/400/lock sequence and `fetchUsersWaitingForGroupKey`
re-run live. Corrections from that pass: the web's CBC-garbage and "Do it later" paths (§7.1),
`suggestedOldRoomKeys` visibility (§4, §6), the web's handling of a stored `e2e: 'done'` (§4),
megolm handling (§3.7), room-body/header gating and the header icon source (§7.2, §10), hide-type
keys (§9), `E2E_Enable` off (§11.12), passphrase character classes (§3.3.4, read from the dev
server's bundled `@rocket.chat/password-policies`). Added from live checks: plaintext leaks via
`description`/`fileName` on `rooms.mediaConfirm` and via a client-supplied `md` (§4, §5.7, §8, §11).

**`E2E_Enable` is `false` on the dev server** (public setting, read live) and was not changed.
Consequences for verification: everything that does not check `E2E_Enable` was run live (key
upload/fetch, room key id, `updateGroupKey`, suggested key accept/reject, encrypted messages,
threads, edits, encrypted uploads/downloads, pin, room key reset, user key reset, `requestSubscriptionKeys`
and the `e2e.keyRequest` stream). `e2e.fetchUsersWaitingForGroupKey` / `e2e.provideUsersSuggestedGroupKeys`
short-circuit to empty/no-op when E2E is disabled (verified), so the queue-based distribution and
`rooms.saveRoomSettings {encrypted}` are documented from source. Encrypted rooms were created with
`groups.create {extraData: {encrypted: true}}`, which works with E2E disabled.

---

## 1. Overview and trust model

```
user (per account)                       room (per encrypted room: t = 'p' or 'd')
------------------                       -----------------------------------------
RSA-OAEP-2048/SHA-256 key pair           room key: AES-256-GCM (legacy: AES-128-CBC), JWK string
  public JWK  -> users.e2e.public_key    room key id ("kid"): UUID v4 -> rooms.e2eKeyId
  private JWK -> encrypted with a        per member: RSA-OAEP(member public key, room JWK string)
    passphrase (PBKDF2 + AES-GCM)          -> subscriptions.E2EKey = kid + base64(256 bytes)
    -> users.e2e.private_key             older room keys -> subscriptions.oldRoomKeys[]

message: {t:'e2e', msg:'', content:{algorithm:'rc.v2.aes-sha2', kid, iv, ciphertext}}
           ciphertext = AES-GCM(room key, iv, UTF-8(JSON({msg, attachments?, files?, file?})))
file:    bytes = AES-256-CTR(random file key, random 16-byte counter block, 64-bit counter)
           file key + iv + sha256 live inside the encrypted message/upload content
```

- The passphrase ("E2E password") never leaves the client; the server stores only the encrypted
  private key. A random 12-word passphrase is generated on first use (§3.4).
- Room keys are distributed client-to-client through the server: any member that holds the key
  wraps it with the other member's public key and stores it as that member's `E2ESuggestedKey`;
  the receiver verifies it can decrypt it and accepts (→ `E2EKey`) or rejects it.
- There is **no authentication** of keys or senders: the server (or any room member) can hand out
  a different public key or room key. The design protects message content from server storage /
  DB access and passive observers only. Rustket should not claim more than that in its UI.

---

## 2. Gating: settings, permissions, room types

### 2.1 Settings (`RC:apps/meteor/server/settings/e2e.ts`; all `public: true`)

| Setting | Default | Dev server [live] | Effect |
|---|---|---|---|
| `E2E_Enable` | `false` | `false` | Master switch. Web starts the E2E client only if `true` (`RC:apps/meteor/client/views/root/hooks/loggedIn/useE2EEncryption.ts`). Server: `fetchUsersWaitingForGroupKey` returns `{}` and `provideUsersSuggestedGroupKeys` is a no-op when off (`RC:apps/meteor/server/api/v1/e2e.ts`); `allowRoomSettingChange(E2E)` is false for `p`/`d` when off (`RC:apps/meteor/server/lib/rooms/roomTypes/{private,direct}.ts`); unencrypted-message enforcement only when on. |
| `E2E_Allow_Unencrypted_Messages` | `false` (migration v314 forced `value` and `packageValue` to `false` on existing installs) | `false` | When `false` and the room is encrypted and E2E is on, `sendMessage` rejects any message with `t !== 'e2e'` (`RC:apps/meteor/server/meteor-methods/messages/sendMessage.ts:100`). Web: slash-command popup items are disabled, slash commands are **not executed at all** (`isSlashCommandAllowed = !E2E_Enable \|\| !room.encrypted \|\| E2E_Allow_Unencrypted_Messages`, `RC:apps/meteor/client/views/room/composer/messageBox/MessageBox.tsx:75`), composer shows no "unencrypted" hint. |
| `E2E_Enabled_Default_DirectRooms` | `false` | `false` | Only when `E2E_Enable`: new DMs get `encrypted: true` unless the caller passed `encrypted` (`RC:apps/meteor/server/lib/e2e/beforeCreateRoom.ts`). |
| `E2E_Enabled_Default_PrivateRooms` | `false` | `false` | Same for new private rooms and private teams (only when `E2E_Enable`). **Not for discussions**: `createDiscussion` always passes a boolean `encrypted` (explicit, else inherited from the parent room), so the `?? true` default never applies. Web create dialogs pre-check the toggle. |
| `E2E_Force_Encryption_For_Private_Rooms` | `false` | `false` | Only when `E2E_Enable`: new non-federated private rooms are always encrypted; `encrypted: false` → `error-encrypted-private-rooms-enforced`; discussion of type `p` with `encrypted: false` → `error-encrypted-private-rooms-enforced-discussion` (`RC:apps/meteor/server/meteor-methods/messages/createDiscussion.ts:154-163`). Not enforced on later toggling. |
| `E2E_Enable_Encrypt_Files` | `true` | `true` | Web encrypts uploads in encrypted rooms; if off and unencrypted messages are not allowed, uploads are refused client-side with `You_cant_send_unencrypted_files_in_an_encrypted_room` (`RC:apps/meteor/client/lib/chats/flows/uploadFiles.ts:27`; this check ignores `E2E_Enable`, and the key is missing from `en.i18n.json`, so the web shows the raw key). |
| `E2E_Enabled_Mentions` | `true` | `true` | Client sends plaintext `e2eMentions` so the server can notify mentioned users (§7.6). |

Related settings read by the E2E code: `UTF8_User_Names_Validation` (mention regex, dev: `[0-9a-zA-Z-_.]+`),
`UI_Use_Real_Name`, `Message_QuoteChainLimit` (dev: `2`), `Site_Url` (dev: `http://localhost:3000`),
`FileUpload_MaxFileSize` (dev: 104857600), `Message_AllowUnrecognizedSlashCommand` (public, dev:
`false`; decides whether an unknown `/…` text is sent at all, §7.6) [live]. For `users.resetE2EKey`
(§5.1): `Accounts_TwoFactorAuthentication_Enabled` (dev: `true`),
`Accounts_TwoFactorAuthentication_Enforce_Password_Fallback` (dev: `true`),
`Accounts_TwoFactorAuthentication_RememberFor` (private, dev: 1800 s) [live].

### 2.2 Permissions [live: `permissions.listAll`]

| Permission | Default roles | Used by |
|---|---|---|
| `toggle-room-e2e-encryption` | `owner`, `admin` | `saveRoomSettings {encrypted}` for non-DM rooms (`RC:apps/meteor/server/meteor-methods/rooms/saveRoomSettings.ts:186`), `e2e.resetRoomKey` (all room types, including DMs) |
| `edit-room` | `admin`, `owner`, `moderator` | `saveRoomSettings`; **not** required for DMs when only `encrypted` is changed (same file, lines 497-504); the web toolbox action also requires it for non-DM rooms (§7.9) |
| `edit-other-user-e2ee` | `admin` | `users.resetE2EKey {userId\|username\|user}` (reset someone else's keys) |

There is no `reset-other-user-e2e-key` permission in 8.9 (only its i18n string remains).

### 2.3 Room types

- Only `p` (private groups, private teams, discussions of private rooms) and `d` (DMs incl.
  multi-user DMs) can be encrypted. Public `c` rooms: `allowRoomSettingChange(E2E)` is always false
  (`RC:apps/meteor/server/lib/rooms/roomTypes/public.ts:29`). The web client creates E2E room
  instances only for `room.t === 'd' || room.t === 'p'` with `room.encrypted`
  (`WEB:rocketchat.e2e.ts:250`).
- Federated rooms: E2E toggle disabled in the web UI (`core.E2E_unavailable_for_federation`,
  `RC:apps/meteor/client/hooks/roomActions/useE2EERoomAction.ts`).
- `room.encrypted` is the source of truth (room field; DDP subscriptions carry it merged as
  `sub.encrypted` in the web store).

---

## 3. Primitives and byte formats

### 3.1 Encodings

- **base64 (standard alphabet, padded)**: IVs, ciphertexts, the 256-byte RSA blocks, `$binary`.
- **base64url without padding**: JWK members (`n`, `e`, `d`, `p`, `q`, `dp`, `dq`, `qi`, `k`).
  Emit without `=`; accept with or without on input.
- **"Binary" codec** (`WEB:binary.ts`): JS string ↔ bytes, one char = one byte (Latin-1); any
  char > U+00FF throws `RangeError`. The web uses it for the PBKDF2 password and salt, for the
  private-key plaintext, and for the room-key JWK string fed to RSA. All of these are ASCII in
  practice except the user-chosen password (see §3.3.4).
- **UTF-8**: message plaintext (`TextEncoder`/`TextDecoder`), file name hashing.
- JSON key order is irrelevant everywhere (all parsers are order-agnostic); browser-exported JWKs
  in the upstream fixtures (`WEB:keychain.spec.ts`) have alphabetical keys, Node exports
  `key_ops, ext, alg, kty, …` [live], RN builds `{kty, alg, k, ext, key_ops}` in that order. Only the
  legacy key-id derivation (§3.5) hashed a specific serialization.

### 3.2 User key pair (RSA)

`WEB:crypto/rsa.ts`:

- `RSA-OAEP`, `modulusLength: 2048`, `publicExponent: 0x010001`, `hash: SHA-256`. OAEP uses
  SHA-256 for both the label hash and MGF1, empty label (WebCrypto semantics). Ciphertext is always
  exactly 256 bytes. Maximum plaintext: 256 − 2·32 − 2 = **190 bytes** (an A256GCM JWK string is
  122 bytes in the alphabetical form, an RN A128CBC one 101).
- Stored/transported as `JSON.stringify(jwk)`:
  - public: `{"kty":"RSA","alg":"RSA-OAEP-256","e":"AQAB","n":…,"ext":true,"key_ops":["encrypt"]}`
  - private: same plus `"d","p","q","dp","dq","qi"` and `"key_ops":["decrypt"]`.
- WebCrypto **import rules** that Rustket's output must satisfy (other clients import with
  `extractable: true`; each member is only checked when present, Rustket emits all of them):
  `alg` must be `RSA-OAEP-256`, `ext` must not be `false`, `key_ops` must contain the requested
  usage (`encrypt` for public, `decrypt` for private).
- No SPKI/PKCS#8 formats are used anywhere on the wire; only JWK strings. (Internally Rustket may
  convert to whatever its RSA library needs.)

### 3.3 Private key backup ("keychain" envelope)

`WEB:keychain.ts`, upstream vectors in `WEB:keychain.spec.ts` [vec].

The server stores the string in `users.e2e.private_key`; `e2e.fetchMyKeys` returns it verbatim.

#### 3.3.1 Reading (two versions)

```
json = JSON.parse(private_key)
if json.$binary is string:                       # v1 (legacy, web/RN before 7.13)
    bin  = base64(json.$binary)
    iv   = bin[0..16]; ct = bin[16..]
    salt = <userId>                              # the user's _id, as a string
    iterations = 1000
elif json has iv:string, ciphertext:string, salt:string, iterations:number:   # v2
    iv = base64(json.iv)  (12 bytes); ct = base64(json.ciphertext)
    salt = json.salt; iterations = json.iterations
else: error "Invalid private key format"

key  = PBKDF2-HMAC-SHA256(password_bytes, salt_bytes, iterations, dkLen = 32)
mode = (len(iv) == 16) ? AES-256-CBC (PKCS#7) : AES-256-GCM (128-bit tag appended, no AAD)
plaintext = decrypt(mode, key, iv, ct)       -> private JWK JSON string
```

The web picks the AES mode by IV length (`WEB:keychain.ts:decryptKey`); RN by shape
(`RN:app/lib/encryption/utils.ts:parsePrivateKey`). Equivalent for well-formed data. Differences in
leniency: the web requires `salt` to be a string for v2; RN accepts a v2 envelope without `salt`
(falls back to the user id) and checks the v2 shape before `$binary`. Rustket: accept what RN
accepts.

Wrong password: GCM tag failure / CBC padding failure → WebCrypto `OperationError`, which the web
shows as `Incorrect_encryption_password` (`RC:apps/meteor/client/views/e2e/EnterE2EPasswordModal/EnterE2EPasswordModal.tsx`).
With CBC ~1/256 wrong passwords produce valid padding and garbage; treat a JSON/JWK parse
failure after decryption as "wrong password" too.

#### 3.3.2 Writing (always v2)

Web (`WEB:keychain.ts:encryptKey`):

```
salt       = "v2:" + userId + ":" + crypto.randomUUID()   # e.g. v2:BFqA4FAHKX4qEywfJ:5b36c4cc-4e7a-497d-b4b0-a1df863c65c4
iterations = 100000
iv         = 12 random bytes
ct         = AES-256-GCM(PBKDF2-SHA256(password, salt, 100000, 32), iv, privateJwkString)
stored     = JSON.stringify({"iv": b64(iv), "ciphertext": b64(ct), "salt": salt, "iterations": 100000})
```

RN writes the same structure with `salt = "v2:" + userId + ":mobile"` on servers ≥ 7.13
(`RN:app/lib/encryption/encryption.ts:205`). Readers must take the salt from the JSON. Rustket
should write the web form (random UUID per write).

There is **no automatic v1 → v2 migration** in either client; v1 is rewritten as v2 only when the
user changes the passphrase (`changePassword` → `persistKeys(..., {force: true})`).

#### 3.3.3 Upload

`POST /api/v1/e2e.setUserPublicAndPrivateKeys {public_key: <public JWK string>, private_key: <stored envelope string>, force?: bool}` (§5).

#### 3.3.4 Password encoding pitfall

Web encodes the password with the Latin-1 "Binary" codec (`Binary.decode(password)`), RN with
UTF-8 (`utf8ToBuffer`). Identical for ASCII; **different for U+0080…U+00FF** (web 1 byte, RN 2
bytes) and the web **throws** for anything above U+00FF ("illegal char code", [vec]).
Rustket policy:

- When setting a passphrase: require printable ASCII (reject otherwise with a clear message);
  this keeps it decryptable by every client.
- When decrypting: if all chars ≤ U+00FF try Latin-1 bytes first, then UTF-8 bytes if different;
  otherwise UTF-8 only.

Web passphrase policy for user-chosen passphrases (`RC:apps/meteor/client/views/account/security/ChangePassphrase.tsx`,
`PASSPHRASE_POLICY` with `enabled: true`): min length 30 (JS `length`, i.e. UTF-16 code units), at
least one lowercase, one uppercase, one digit, one special character, no repeating-character rule.
Character classes (`@rocket.chat/password-policies` 0.1.1 `PasswordPolicy.ts`; not in `.ref/`, read
from the dev server's bundle at `/app/bundle/programs/server/npm/node_modules/@rocket.chat/password-policies/src/`):
lowercase `[a-z]`, uppercase `[A-Z]`, digit `[0-9]`, special `[^A-Za-z0-9 ]` (anything except ASCII
letters, digits and space, so non-ASCII characters count as "special"). The policy applies only to
the change-passphrase form; the generated 12-word passphrase (§3.4) does not satisfy it and is not
checked.

### 3.4 Generated passphrase

`WEB:helper.ts:generatePassphrase`, identical in `RN:app/lib/encryption/utils.ts`:

- Word list: BIP-39 English, 2048 words (`WEB:wordList.ts`; web and RN lists are identical [vec];
  first `abandon`, last `zoo`; all `[a-z]{3,8}`).
- 12 words joined by single spaces.
- Each word: draw a uniform 32-bit integer `v` from a CSPRNG (RN: big-endian from 4 random bytes);
  reject while `v >= floor(0xFFFFFFFF / 2048) * 2048`; word = `list[v % 2048]`. (Entropy 132 bits.)

### 3.5 Room key and key id

`WEB:crypto/aes.ts`, `WEB:rocketchat.e2e.room.ts:createNewGroupKey`.

- New room keys (8.9 web, RN on servers ≥ 7.13): AES-256-GCM, 32 random bytes, exported as
  `{"kty":"oct","alg":"A256GCM","k":<b64url(32)>,"ext":true,"key_ops":["encrypt","decrypt"]}`.
  The **room key string** is `JSON.stringify(jwk)`; it is what gets RSA-wrapped.
- Legacy room keys: AES-128-CBC, `alg: "A128CBC"`, 16-byte `k`. Must be readable forever (old
  rooms keep their key until reset) **and may still be used for writing** (§3.7).
- Accepted `alg` values: exactly `A256GCM` and `A128CBC`; anything else is an error
  ("Unrecognized algorithm name", `WEB:crypto/aes.spec.ts`).
- Key id (`kid`, stored in `rooms.e2eKeyId`, used as prefix of wrapped keys and in v2 contents):
  - 8.9 web and RN ≥ 7.13: `crypto.randomUUID()` (lowercase v4 UUID, 36 chars) [live].
  - RN on 7.0–7.12: first 12 hex chars of SHA-256 of the room key string. The upstream web v1
    fixture (`WEB:content.spec.ts` `msgv1web`) carries such a kid, so the older web did the same
    (UNVERIFIED: which web versions).
    Verified: `sha256('{"alg":"A128CBC","ext":true,"k":"qb8In0Rpa9nwSusvxxDcbQ","key_ops":["encrypt","decrypt"],"kty":"oct"}')[0..12] = 32c9e7917b78`, the kid of the upstream v1 messages [vec].
  - RN on < 7.0: `base64(roomKeyString)[0..12]` (`RN:app/lib/encryption/room.ts:284`) — a constant
    per serializer (`eyJrdHkiOiJv` for RN's own key order, `eyJhbGciOiJB` for alphabetical), so
    useless as an identifier.
  Treat kids as opaque strings of any length.

### 3.6 Wrapped room key (`E2EKey`, `E2ESuggestedKey`, `oldRoomKeys[].E2EKey`)

`WEB:prefixed.ts` ("PrefixedBase64"), `RN:app/lib/encryption/utils.ts:decodePrefixedBase64`.

```
wrapped = kid + base64(RSA-OAEP(memberPublicKey, bytes(roomKeyString)))   # base64 part is always 344 chars
decode:  len >= 344 required; kid = wrapped[..len-344]; blob = base64(wrapped[len-344..]) must be 256 bytes
```

UUID kid → 380 chars total [live]. The kid of a wrapped key is not authenticated; the room key id
of the room is `rooms.e2eKeyId`.

### 3.7 Message content envelopes

Types in `RC:packages/core-typings/src/IMessage/IMessage.ts:121-143`, decoding in `WEB:content.ts`.

| `content.algorithm` | Shape | Written by |
|---|---|---|
| `rc.v1.aes-sha2` | `{algorithm, ciphertext}`; `ciphertext = kid12 + base64(iv16 ‖ ct)` | RN whenever the room key is A128CBC (any server version, incl. 8.9: the choice follows the key's `alg`, `RN:app/lib/encryption/room.ts:469`); older web (UNVERIFIED: up to which version; `msg` field also used, see below) |
| `rc.v2.aes-sha2` | `{algorithm, kid, iv: base64, ciphertext: base64}` | web 8.9 (always, also over A128CBC keys), RN when the room key is A256GCM |
| `m.megolm.v1.aes-sha2` | `{algorithm, ciphertext}` | Matrix federation; not decryptable by RC clients. The 8.9 web's `decryptMessage` does not even try: `isEncryptedMessageContent` (`RC:packages/core-typings/src/IMessage/IMessage.ts:249`) accepts only v1/v2, so such a message falls through to the no-`content` path (empty `msg` → shown empty and marked done). "Unsupported encryption algorithm" is only thrown where `decrypt(content)` is called directly (pinned message, desktop notification) |

Decoding:

- A message with `t: 'e2e'` but no `content` and a non-empty `msg` (old mobile/web): treat
  `msg` as a v1 ciphertext string (`WEB:content.ts:normalizePayload`).
- v1: `kid = ciphertext[0..12]` (**always exactly 12 chars**), rest base64 → `iv = first 16 bytes`,
  `ct = remainder`. Errors if shorter than 12 chars / 16 bytes.
- v2: all of `kid`, `iv`, `ciphertext` must be non-empty.

**The AES mode comes from the key, not from the envelope version** (`WEB:crypto/aes.ts:encrypt/decrypt`):

- key `A256GCM` → AES-256-GCM, 12-byte IV, 16-byte tag appended to `ciphertext`.
- key `A128CBC` → AES-128-CBC, 16-byte IV, PKCS#7.
- The 8.9 web **writes `rc.v2.aes-sha2` with a 16-byte IV and AES-128-CBC ciphertext** when the
  room still has a legacy A128CBC key (`encryptText` uses `Aes.encrypt(groupSessionKey, …)`).
  RN writes v1 strings for A128CBC rooms and v2 only for A256GCM rooms (`RN:app/lib/encryption/room.ts:462`).
  Rustket: write v2 always (like web); read both.

Key selection when decrypting (`WEB:rocketchat.e2e.room.ts:retrieveDecryptionKey`): the old room
key whose `e2eKeyId == kid` if any, otherwise the current room key (even if the kid does not
match). RN only uses the current key when `kid == keyID` and otherwise looks in `oldRoomKeys`
(`RN:app/lib/encryption/room.ts:669`). Rustket: exact kid match first (current, then old keys);
if nothing matches, try the current key as a fallback (covers rooms whose `e2eKeyId` differs from
the kid embedded in messages).

Plaintext (UTF-8 JSON, serialized by `EJSON.stringify`):

| Writer | Plaintext |
|---|---|
| web text message (`encryptMessage`) | `{"msg": string, "attachments"?: [...]}` (attachments only when editing a file message or similar) |
| web upload message (`processMessageUploads.ts:getEncryptedContent`) | `{"attachments":[…], "files":[…], "file":{…}, "msg": string}` |
| web upload metadata (`uploadFiles.ts`) | `{"type","typeGroup","name","encryption":{"key":JWK,"iv"},"hashes":{"sha256"}}` (+ `altText` if edited) |
| RN text message | `{"msg": string}` |
| RN upload message | `{"msg","attachments","files","file"}` |
| older RN releases | `{"_id","text","userId","ts":{"$date":ms}}` (the upstream fixture `msgv1mob` is dated 2025-09-29; UNVERIFIED: which RN version switched to `{msg}`) |

Interpretation (`WEB:rocketchat.e2e.room.ts:decrypt`): parse JSON; non-object → indecipherable;
if it has a string `text` and no `msg` → `msg = text`. EJSON wrappers (`{"$date": ms}`) may appear.

### 3.8 File encryption

`WEB:rocketchat.e2e.room.ts:encryptFile`, `WEB:helper.ts`, decryption in `RC:apps/meteor/public/enc.js`
(service worker), RN in `RN:app/lib/encryption/room.ts:518` and `encryption.ts:650`.

- Fresh key per file: AES-256-CTR, JWK `{"kty":"oct","alg":"A256CTR","k":<b64url(32)>,"ext":true,"key_ops":["encrypt","decrypt"]}`.
- `iv`: 16 random bytes = the initial counter block, base64 (padded).
- Mode: WebCrypto `{name:'AES-CTR', counter: iv, length: 64}` — **only the low 64 bits of the
  block are incremented (big-endian), wrapping mod 2^64 without carry** into the high half. In
  RustCrypto this is `ctr::Ctr64BE<aes::Aes256>`; a 128-bit counter gives different output after a
  low-half wrap [vec §13.6]. (RN uses native code whose counter width is not visible; a wrap needs
  the random low half within 2^23 of 2^64 for a 100 MB file, so practically never.)
- Whole-file stream cipher, no padding, no MAC; integrity via `hashes.sha256` = lowercase hex
  SHA-256 of the **plaintext** file. The web service worker does not check it; RN does and
  discards on mismatch. Rustket: verify after download, show an error on mismatch.
- Upload file name: lowercase hex SHA-256 of the UTF-8 original name; upload MIME type: none
  (the browser sends `application/octet-stream`; server stored `application/octet-stream` [live]).
  RN sends MIME `file`; the server stores that as `application/octet-stream` too (part
  `Content-Type: file` re-tested live), so every encrypted upload record has type
  `application/octet-stream`, `typeGroup: "application"`.
- `encryption = {key: <JWK object>, iv: <base64>}` appears in the encrypted attachment and in the
  encrypted upload metadata, never in plaintext on the server.

---

## 4. Server data model

| Collection / field | Type | Notes |
|---|---|---|
| `users.e2e.public_key` | string (JWK JSON) | `RC:packages/core-typings/src/IUser.ts:239` |
| `users.e2e.private_key` | string (envelope JSON) | Only returned to the owner by `e2e.fetchMyKeys` |
| `rooms.encrypted` | bool | Published room field (`RC:apps/meteor/lib/publishFields.ts:97`) |
| `rooms.e2eKeyId` | string | Set once by `e2e.setRoomKeyID` (atomic `$exists: false`), replaced by `e2e.resetRoomKey` |
| `rooms.usersWaitingForE2EKeys` | `[{userId, ts}]`, ≤ 50 (`$slice: -50`) | Published (`publishFields.ts:127`); a hint for key distribution; can remain as `[]` after a user key reset or a room key reset with no other key holders [live] (only the accept/`updateGroupKey(self)`/provide paths unset it when it becomes empty) |
| `subscriptions.E2EKey` | wrapped key string | This member's room key. **Unique sparse index** `{E2EKey: 1}` (`RC:packages/models/src/models/Subscriptions.ts:37`): two subscriptions can never hold the same string; a collision makes the write fail with a raw Mongo `E11000 duplicate key error … index: E2EKey_1` HTTP 400 [live]. Real OAEP output never collides, but test fixtures must use distinct fake keys. |
| `subscriptions.E2ESuggestedKey` | wrapped key string | Offered by another member; accept/reject |
| `subscriptions.oldRoomKeys` | `[{e2eKeyId, ts, E2EKey}]`, ≤ 10, newest first | Wrapped previous room keys (`RC:apps/meteor/server/lib/e2e/functions/resetRoomKey.ts:replicateMongoSlice`) |
| `subscriptions.suggestedOldRoomKeys` | same | Absent from `subscriptionFields` (`RC:apps/meteor/lib/publishFields.ts`), so the subscription publication, `subscriptions.get` and the projected `notifyOnSubscriptionChangedById` events never carry it. **But** `subscriptions.getOne` returns the unprojected document (`RC:apps/meteor/server/api/v1/subscriptions.ts:107`), and `notifyOnSubscriptionChanged(value)` broadcasts whole documents (used by `provideUsersSuggestedGroupKeys`, `updateGroupKey` for another user and `resetRoomKey`), so `subscriptions-changed` events right after a provide can include it [src]. Parse it tolerantly, don't rely on its absence. Copied into `oldRoomKeys` on accept only if `oldRoomKeys` is unset |
| `messages.t` | `'e2e'` | Encrypted message; `'message_pinned_e2e'` for the pin system message |
| `messages.msg` | `''` for v2 | Non-empty only for legacy v1-in-msg, or after a plaintext edit (§11) |
| `messages.content` | envelope | §3.7 |
| `messages.e2e` | `'pending'`/`'done'` | Client-side marker. RN sends `e2e: 'pending'` in `chat.sendMessage` and the server stores whatever the client sent, including `'done'` [live, re-checked]; the web strips it before sending. The 8.9 web overwrites it with `'pending'` for main-timeline history/stream messages (`RC:apps/meteor/client/lib/RoomHistoryManager.ts:22`), but every other path (thread lists, subscription `lastMessage`/sidebar preview, search, pinned/starred lists) skips decryption of a message stored with `e2e: 'done'`, so any sender can keep its message undecrypted there. Ignore the server value |
| `messages.e2eMentions` | `{e2eUserMentions?: string[], e2eChannelMentions?: string[]}` | Plaintext |
| `rocketchat_uploads.content` | envelope | Encrypted file metadata (§7.8); set by `rooms.media` (multipart `content`) and replaced by `rooms.mediaConfirm` (`fileContent`). Unconfirmed uploads get `expiresAt` = upload time + 24 h |

Live shapes (trimmed):

```jsonc
// GET rooms.info -> room  [live]
{"_id":"6aef9e12ff2338a3f2a2ea41","t":"p","encrypted":true,
 "e2eKeyId":"53f73d9a-cd00-412a-8362-af2ae0c203ca",
 "usersWaitingForE2EKeys":[{"userId":"iQyHconhczDC7pALc","ts":"2026-11-07T21:54:27.696Z"}],
 "lastMessage":{"t":"e2e","msg":"","content":{"algorithm":"rc.v2.aes-sha2","kid":"…","iv":"…","ciphertext":"…"}, …}}

// GET subscriptions.getOne -> subscription (after a room key reset)  [live]
{"rid":"…","t":"p","roles":["owner"],
 "E2EKey":"beaef529-3dfd-49d8-a160-054d8b8f87fc<344 base64 chars>",
 "oldRoomKeys":[{"e2eKeyId":"53f73d9a-…","ts":"2026-11-07T21:55:22.123Z","E2EKey":"53f73d9a-…<344>"}]}

// chat.sendMessage response / stream-room-messages  [live]
{"_id":"e2especMsgmw6xzdkt","rid":"…","t":"e2e","msg":"",
 "content":{"algorithm":"rc.v2.aes-sha2","kid":"53f73d9a-cd00-412a-8362-af2ae0c203ca",
            "iv":"HSYI29i9Vv+idkfg","ciphertext":"3YUhd2q+XpKU…"},
 "e2eMentions":{"e2eUserMentions":["@e2especmw6xy6f2a"],"e2eChannelMentions":[]},
 "mentions":[{"_id":"7b5M8XHEFcCW5kHx8","username":"e2especmw6xy6f2a","type":"user", …}],
 "u":{…},"ts":"…","_updatedAt":"…","urls":[],"channels":[]}
```

No `md` is generated for `t: 'e2e'` messages (`RC:apps/meteor/server/services/messages/hooks/BeforeSaveMarkdownParser.ts:27`);
Rustket must parse the decrypted `msg` itself (it already has a markdown parser for `md`-less messages).
The parser hook only *skips* e2e messages: an `md` sent by the client in `chat.sendMessage` (the
`message` schema has no `additionalProperties: false`) is stored and returned as is [live: a
plaintext `md` on an e2e message came back from `chat.getMessage`]. The web ignores `md` for e2e
messages and re-parses the decrypted `msg` (`RC:apps/meteor/client/lib/parseMessageTextToAstMarkdown.ts`).
Rustket: never render a server `md` for `t == 'e2e'` (it may be spoofed or leak plaintext), and never
send `md` itself.

---
## 5. REST endpoints

All under `/api/v1/`, authenticated (`X-User-Id`, `X-Auth-Token`), JSON bodies, standard
envelopes (`{"success": true, …}` / `{"success": false, "error", "errorType", "details"}`).
Handlers: `RC:apps/meteor/server/api/v1/e2e.ts` unless noted.

### 5.1 User keys

**`GET e2e.fetchMyKeys`** [live]
- Response: `{"public_key": string, "private_key": string, "success": true}`, or just
  `{"success": true}` when the user has no keys.

**`POST e2e.setUserPublicAndPrivateKeys`** [live]
- Body: `{"public_key": string, "private_key": string, "force"?: boolean}`
  (schema `RC:packages/rest-typings/src/v1/e2e/e2eSetUserPublicAndPrivateKeysParamsPOST.ts`, `additionalProperties: false`).
- Without `force`, if both keys already exist: HTTP 400
  `{"success":false,"error":"Keys already set [error-keys-already-set]","errorType":"error-keys-already-set","details":{"method":"e2e.setUserPublicAndPrivateKeys"}}` [live].
- Side effects (`RC:apps/meteor/server/meteor-methods/platform/setUserPublicAndPrivateKeys.ts`):
  stores both strings; adds the user to `usersWaitingForE2EKeys` of every encrypted room they are
  subscribed to without an `E2EKey`; notifies those rooms. Does not check `E2E_Enable`.
- `force: true` is used for passphrase changes (same public key, new envelope) [live].

**`POST users.resetE2EKey`** (`RC:apps/meteor/server/api/v1/users.ts:1771`)
- Body: `{}` (own keys) or `{"userId"|"username"|"user": …}` (other user; needs `edit-other-user-e2ee`).
- `twoFactorRequired: true`, `twoFactorOptions: {disableRememberMe: true}`
  (`RC:apps/meteor/server/lib/2fa/code/index.ts`). Expect `totp-required` and retry with the
  `x-2fa-code`/`x-2fa-method` headers as in `docs/protocol.md`: users with TOTP/email 2FA get that
  method; users **without** 2FA but with a password get `method: "password"` (code = SHA-256 hex of
  the password) because `Accounts_TwoFactorAuthentication_Enforce_Password_Fallback` is on by
  default. The check is skipped only when 2FA is globally off, for password-less (OAuth/SAML)
  users without 2FA, or during the grace window of `Accounts_TwoFactorAuthentication_RememberFor`
  seconds (dev: 1800) after the account's `createdAt` for users without 2FA — that is why fresh
  throwaway users got `{"success":true}` without headers [live]; it does **not** generalize. It is
  also skipped for a personal access token created with `bypassTwoFactor`
  (`isAuthorizedForToken`, same file). A body naming a user that does not exist → `error-invalid-user-id`.
- Effects (`RC:apps/meteor/server/lib/resetUserE2EKey.ts`): broadcasts `user.forceLogout`, unsets
  `users.e2e`, unsets `E2EKey`, `E2ESuggestedKey`, `oldRoomKeys` (not `suggestedOldRoomKeys`) on all
  the user's subscriptions, removes them from all queues (leaving `[]`), **revokes all login
  tokens** (next request → 401 [live]). For another user it first e-mails
  `Your_e2e_key_has_been_reset` / `E2E_Reset_Email_Content` to the verified addresses (a mail
  failure aborts the whole reset with `error-email-send-failed`); without `edit-other-user-e2ee`
  → 400 `error-not-allowed` [live]. Federated users → `error-not-allowed`.
- After re-login the client generates new keys (§7.1).

### 5.2 Room key bootstrap

**`POST e2e.setRoomKeyID`** [live]
- Body: `{"rid": string, "keyID": string}`.
- Needs room access (`error-invalid-room` otherwise [live]). Atomic: succeeds only if the room has
  no `e2eKeyId`; otherwise HTTP 400 `errorType: "error-room-e2e-key-already-exists"` [live] — the
  web treats that as "another member won the race" and discards its new key
  (`WEB:rocketchat.e2e.room.ts:createGroupKey`). Does not check `E2E_Enable` or `room.encrypted`.

**`POST e2e.updateGroupKey`** [live]
- Body: `{"rid", "uid", "key"}` (`key` = wrapped key, §3.6).
- Logic (`RC:apps/meteor/server/meteor-methods/platform/updateGroupKey.ts`): only if the caller has
  a subscription to `rid`. If `uid == caller`: sets the caller's `E2EKey` directly and removes the
  caller from the room queue. Otherwise sets `E2ESuggestedKey` of `uid`'s subscription (even if
  that user already has an `E2EKey` [live]). Always `{"success": true}`, even with no subscription
  [live] — except, for `uid == caller`, the raw `E11000` error of the unique `E2EKey` index (§4).
- The 8.9 web uses it only for the creator's own key; distribution to others goes through
  `provideUsersSuggestedGroupKeys`. RN on < 7.0 used it for everyone.

**`GET e2e.getUsersOfRoomWithoutKey?rid=…`** [live]
- Response: `{"users":[{"_id": string, "e2e": {"public_key": string}}], "success": true}`.
- All members whose subscription has no `E2EKey` (pending `E2ESuggestedKey` does not exclude them;
  the caller is included if they lack `E2EKey`) and who have a public key. No size limit.
- `error-invalid-room` without room access [live].

### 5.3 Suggested keys

**`POST e2e.acceptSuggestedGroupKey {rid}`** [live]
- `E2ESuggestedKey` → `E2EKey`; `suggestedOldRoomKeys` → `oldRoomKeys` only if `oldRoomKeys` is
  unset; unsets both suggested fields; removes the caller from the room queue
  (`RC:apps/meteor/server/lib/e2e/functions/handleSuggestedGroupKey.ts`).
- Errors: `error-subscription-not-found` [live]; `error-no-suggested-key-available` (HTTP 400 [live]);
  a raw `E11000 duplicate key … E2EKey_1` 400 if the suggested string equals another
  subscription's `E2EKey` (§4) — in that case nothing is unset and the suggestion stays [live].

**`POST e2e.rejectSuggestedGroupKey {rid}`** [live]
- Unsets the suggested fields (keeps an existing `E2EKey` [live]) and adds the caller to the room
  queue (if the room is `encrypted`) so someone provides a fresh key.

### 5.4 Queue-based distribution (needs `E2E_Enable`)

**`GET e2e.fetchUsersWaitingForGroupKey?roomIds[]=…&roomIds[]=…`**
- Web serializes arrays as `roomIds[]=a&roomIds[]=b` (`RC:packages/api-client` is not in `.ref/`;
  verified in the dev server's served web bundle: `getParams(e){return e?(0,s.stringify)(e,{arrayFormat:"bracket"}):""}`,
  `/app/bundle/programs/web.browser/*.js`); `roomIds[]=a`, `roomIds=a&roomIds=b` and a
  single `roomIds=a` all pass validation [live]. Missing `roomIds` → 400 `error-invalid-params` [live].
- Response: `{"usersWaitingForE2EKeys": {"<rid>": [{"_id", "public_key"}]}, "success": true}`;
  `{}` when `E2E_Enable` is off [live].
- Semantics (`RC:packages/models/src/models/Subscriptions.ts:findUsersWithPublicE2EKeyByRids`):
  per room, up to 50 members **other than the caller** whose subscription has neither `E2EKey` nor
  `E2ESuggestedKey` and whose user has a public key. It does not read `usersWaitingForE2EKeys`;
  rooms without such users are absent from the map. No access check on `roomIds`.

**`POST e2e.provideUsersSuggestedGroupKeys`**
- Body:
  ```json
  {"usersSuggestedGroupKeys": {"<rid>": [
     {"_id": "<uid>", "key": "<wrapped>",
      "oldKeys": [{"e2eKeyId": "…", "ts": "<ISO string>", "E2EKey": "<wrapped for this user>"}]}]}}
  ```
  (`oldKeys` optional; item `additionalProperties: false`.)
- No-op `{"success": true}` when `E2E_Enable` is off [live]. Otherwise, for each room the caller can
  access: sets `E2ESuggestedKey` (+ `suggestedOldRoomKeys`, `ts` parsed to Date) on each listed
  user's subscription (skips non-members), removes those users from the room queue, notifies
  (`RC:apps/meteor/server/lib/e2e/functions/provideUsersSuggestedGroupKeys.ts`). Inaccessible rooms
  are skipped silently.

**`POST e2e.requestSubscriptionKeys`** (no body) [live]
- `{"success": true}`. Broadcasts `notify.e2e.keyRequest(rid, e2eKeyId)` for every room with an
  `e2eKeyId` where the caller's subscription has no `E2EKey`
  (`RC:apps/meteor/server/meteor-methods/platform/requestSubscriptionKeys.ts`), delivered as the
  `stream-notify-room` event `{rid}/e2e.keyRequest` (§6). DDP method deprecated until 9.0; RN
  switches to REST at server 8.6 (`RN:app/lib/services/restApi.ts:72`), which suggests the REST
  endpoint appeared in 8.6 (UNVERIFIED).

### 5.5 Room key reset

**`POST e2e.resetRoomKey`** (`RC:apps/meteor/server/api/v1/e2e.ts`, `RC:apps/meteor/server/lib/e2e/functions/resetRoomKey.ts`) [live]
- Body: `{"rid", "e2eKey": <new room key wrapped for the caller>, "e2eKeyId": <new kid>}`.
- Checks in this order: `toggle-room-e2e-encryption` in the room (HTTP 403
  `{"success":false,"error":"error-not-allowed"}` otherwise [live]); the 10 s per-room lock
  (held → 400 `error-e2e-key-reset-in-progress` [live]); room access (400
  `{"success":false,"error":"error-not-allowed"}` [live] — thrown after the lock is set and outside
  the `try/finally`, so the lock is **not** released and every retry fails for 10 s [live]).
- Inside `resetRoomKey()` the function throws `error-user-not-found`, `error-user-has-no-keys`,
  `error-room-not-found` or `error-room-not-encrypted` (= no `e2eKeyId`), but the handler catches
  everything and the client only ever sees **400 `{"success":false,"error":"error-e2e-key-reset-failed"}`**
  (verified live for "caller has no keys" and "room has no `e2eKeyId`"). It does not check
  `room.encrypted` or `E2E_Enable`.
- Effects: for every subscription with `E2EKey` or `E2ESuggestedKey`: current `E2EKey` is pushed to
  the front of `oldRoomKeys` (keyed by the old `e2eKeyId`, max 10), `E2EKey`, `E2ESuggestedKey`,
  `suggestedOldRoomKeys` are unset; the room gets the new `e2eKeyId` and `usersWaitingForE2EKeys` is
  **replaced** by those other members (max 50) — members that were waiting without any key drop out
  of the queue, and with no other key holders it becomes `[]` [live]; the caller's subscription gets
  `E2EKey = e2eKey`. Verified: caller had new `E2EKey` + `oldRoomKeys[old]`, the other member had
  `oldRoomKeys[old]` and no `E2EKey`, room queue = [other member] [live].

### 5.6 Room encryption on/off and creation

**`POST rooms.saveRoomSettings {"rid", "encrypted": bool}`** (web toggle, `RC:apps/meteor/client/hooks/roomActions/useE2EERoomAction.ts`)
- Validation (`RC:apps/meteor/server/meteor-methods/rooms/saveRoomSettings.ts`): `edit-room`
  unless DM-and-only-`encrypted`; if the value changes: room type must allow E2E (requires
  `E2E_Enable`) else 400 `error-action-not-allowed` "Only groups or direct channels can enable
  encryption" (seen live because E2E is off); non-DM needs `toggle-room-e2e-encryption`.
- Effects (`RC:apps/meteor/server/lib/rooms/settings/saveRoomEncrypted.ts`): sets
  `rooms.encrypted`; posts system message `room_e2e_enabled` / `room_e2e_disabled` (msg = username)
  when the value changed; enabling disables auto-translate on all subscriptions of the room.
- It does not touch `e2eKeyId`/keys. Disabling keeps keys; old messages stay encrypted. The 8.9 web
  then stops decrypting them too (it only keeps room instances for `room.encrypted` rooms, §7.2);
  Rustket should keep decrypting with the keys it has.
- Alternative: `POST groups.setEncrypted {roomId|roomName, encrypted}` (same code path,
  `RC:apps/meteor/server/api/v1/groups.ts:1807`).

**Creating encrypted rooms**: `groups.create {name, members, extraData: {encrypted: true}}`
(web `CreateChannelModal.tsx`; works even with E2E disabled [live]), teams the same through
`teams.create` room `extraData`, `rooms.createDiscussion {…, encrypted}` (omitted `encrypted`
inherits the parent room's flag; an encrypted discussion with a `reply` → `error-invalid-arguments`
"Encrypted discussions must not receive an initial reply."; the discussion `description` is emptied
when the parent is encrypted); DMs through the `E2E_Enabled_Default_DirectRooms` default (only when
`E2E_Enable`). The room starts **without `e2eKeyId`** [live]; the first member whose client is ready
creates the key (§7.3).

### 5.7 Messages, edits, uploads, pins

- **Send**: `POST chat.sendMessage {"message": {"_id", "rid", "tmid"?, "tshow"?, "t": "e2e", "content": {…}, "e2eMentions"?: {…}}}` — no `msg` [live].
  The schema (`RC:packages/rest-typings/src/v1/chat.ts:11`) has no `additionalProperties: false`
  on `message`, so `t`/`content`/`e2eMentions` pass. Server stores `msg: ""`, computes `mentions`
  from `e2eMentions` [live]. Rate limit: 5 req/s. Rejected with `error-not-allowed` ("Not allowed
  to send un-encrypted messages in an encrypted room") if `t !== 'e2e'` while room encrypted +
  `E2E_Enable` + not `E2E_Allow_Unencrypted_Messages` (the server also sends an ephemeral message).
- **Edit**: `POST chat.update {"roomId", "msgId", "content": {…}, "e2eMentions"?}` [live]. Content
  variant schema: v1 `{algorithm, ciphertext}` or v2 `{algorithm, ciphertext, iv, kid}`, each
  `minLength 1`, `additionalProperties: false` (`RC:packages/rest-typings/src/v1/chat.ts:408-515`;
  an extra key in `content` → 400 `invalid-params` [live]).
  `content` on a non-e2e message → `"Only encrypted messages can have content updated."`
  (`RC:apps/meteor/server/api/v1/chat.ts:353`). Result: `content` replaced, `editedAt`/`editedBy`
  set [live].
- **Upload**: §7.8 (`rooms.media` multipart field `content`, `rooms.mediaConfirm` fields
  `t`, `content`, `fileContent`) [live]. `rooms.mediaConfirm` rejects `e2eMentions` (400 `Match
  error`) [live], so mentions in an encrypted upload caption never notify anyone.
  **Never send `description` or `fileName` to `rooms.mediaConfirm` for an encrypted upload**: the
  server copies `description` into the plaintext `attachments[0].description` and the upload record,
  and `fileName` replaces the hashed name in the upload record, `file`/`files`, `attachments[0].title`
  and the `title_link` path (`RC:apps/meteor/server/api/v1/rooms.ts:349-354`,
  `RC:apps/meteor/server/meteor-methods/messages/sendFileMessage.ts:parseFileIntoMessageAttachments`)
  [live: both came back in plaintext from the confirm response and `groups.files`]. The web sends
  them only on the unencrypted path (`fileName`, `description: altText`,
  `RC:apps/meteor/client/lib/chats/flows/processMessageUploads.ts:126`); the existing
  `RestClient::confirm_media` in `crates/rocketchat/src/rest/files.rs` sends `description`, so the
  encrypted variant must not.
- **Pin**: `POST chat.pinMessage {messageId}` on an e2e message creates a system message
  `t: "message_pinned_e2e"`, `msg: ""`, `attachments: [{text: <original msg, i.e. "">,
  author_name, author_icon, content: <original content>, ts, attachments: <original server-side
  attachments, [] for text>}]` [live] (`RC:apps/meteor/server/lib/messaging/pins/pinMessage.ts:112`).
  For an upload message the inner `attachments` is the server's plaintext hashed-name attachment;
  for a plaintext-edited e2e message (§8) `text` carries the leaked plaintext [live].
- **Files list**: `groups.files`/`rooms.files` entries carry `content` (encrypted metadata), hashed
  `name`, `type: application/octet-stream` [live].

### 5.8 DDP methods (legacy)

Still registered: `e2e.fetchMyKeys`, `e2e.setUserPublicAndPrivateKeys`, `e2e.requestSubscriptionKeys`
(returns `true` [live]), `e2e.resetOwnE2EKey` — all deprecated until 9.0 — and `e2e.setRoomKeyID(rid, keyID)`,
`e2e.getUsersOfRoomWithoutKey(rid)` (`RC:apps/meteor/server/meteor-methods/platform/`). There is no
DDP `e2e.updateGroupKey`. Rustket uses REST for all of them.

---

## 6. Streams and events

| Stream / event | Args | Notes |
|---|---|---|
| `stream-notify-room` `{rid}/e2e.keyRequest` | `[e2eKeyId]` | Emitted for `afterJoinRoom` (`RC:apps/meteor/server/lib/e2e/index.ts`; for every joined room, encrypted or not, with `e2eKeyId` possibly undefined) and by `e2e.requestSubscriptionKeys` [live: received twice after one REST + one DDP call]. Readable by anyone with room access (`RC:packages/streamer/src/notifications.module.ts` `streamRoom.allowRead`). **The 8.9 web client does not listen to it**; RN does not either. |
| `stream-notify-room-users` `{rid}/e2ekeyRequest` (client publish) | `[rid, e2eKeyId]` | Web publishes it on handshake when waiting for a key (`WEB:rocketchat.e2e.room.ts:355`), RN as a DDP method call `stream-notify-room-users` on a 5 s debounce (`RN:app/lib/encryption/room.ts:315`, `RN:app/lib/services/restApi.ts:93`). **8.9 drops it**: `streamRoomUsers.allowWrite` relays only `video-conference` and `userData` (`RC:packages/streamer/src/notifications.module.ts:260`, spec `notifications.module.spec.ts:105`). The method call returns `{"msg":"result"}` with no error [live] and nothing reaches other members [live]. |
| `stream-notify-user` `{uid}/e2ekeyRequest` | `[rid, keyId]` | Listened to by RN (`RN:app/lib/methods/subscriptions/rooms.ts:439`); never emitted on 8.9 (follows from the row above) [live: not received]. |
| `stream-notify-user` `{uid}/subscriptions-changed` | `['inserted'\|'updated', sub]` | Carries `E2EKey`, `E2ESuggestedKey`, `oldRoomKeys`; usually projected to `subscriptionFields`, but some E2E paths send the whole document, which may include `suggestedOldRoomKeys` (§4) — main trigger for accepting suggested keys and for "key received". |
| `stream-notify-user` `{uid}/rooms-changed` | `['updated', room]` | Carries `encrypted`, `e2eKeyId`, `usersWaitingForE2EKeys`, `lastMessage` (encrypted) [live]. A changed `e2eKeyId` means the room key was reset. |
| `stream-notify-user` `{uid}/notification` | `[{title, text, payload}]` | For an e2e message: `text: "<sender>: "` (empty body), `payload.message = {"msg":"","t":"e2e","content":{…}}` [live]; client decrypts and replaces the text (`RC:apps/meteor/client/hooks/notification/useDesktopNotification.ts`). |
| `stream-room-messages` `{rid}` | `[message]` | Encrypted messages as stored [live]. |

Rustket: there is no working push-style key request in 8.9; rely on the queue + polling loop
(§7.4). Optionally subscribe to `{rid}/e2e.keyRequest` for rooms already watched and treat it as
"run key distribution for this room now" (cheap, harmless).

---
## 7. Client algorithms (what the web client does, and what Rustket should do)

### 7.1 Account key lifecycle

Web states (`WEB:E2EEState.ts`): `NOT_STARTED`, `DISABLED`, `LOADING_KEYS`, `ENTER_PASSWORD`,
`SAVE_PASSWORD`, `READY`, `ERROR`. `READY` and `SAVE_PASSWORD` both count as ready
(`WEB:rocketchat.e2e.ts:isReady`).

Start condition: logged in and public setting `E2E_Enable == true`; otherwise `DISABLED`
(`useE2EEncryption.ts`). The web also refuses to start outside a secure context (HTTPS or
localhost) because WebCrypto needs one (`WEB:rocketchat.e2e.ts:startClient`). Rustket has no such
technical limit; show a warning for plain `http://` non-localhost servers.

`startClient(userId)` (`WEB:rocketchat.e2e.ts:345`):

1. Load cached keys from local storage (`public_key`, `private_key` = decrypted private JWK,
   keys in `RC:apps/meteor/client/lib/sdk/storage.ts`).
2. `GET e2e.fetchMyKeys` (state `LOADING_KEYS`; on network error → `ERROR` and stop, never
   generate new keys when the server could not be asked).
3. If the server has a private key but there is no cached private key → `ENTER_PASSWORD`: banner
   "Enter your E2E password / Click here to enter your password" → modal → `Keychain.decryptKey`.
   Wrong password (WebCrypto `OperationError` only) keeps the modal open with
   `Incorrect_encryption_password`; any other thrown error (e.g. the `RangeError` for a char >
   U+00FF) shows an error toast and closes the modal, state stays `ENTER_PASSWORD`, banner stays
   (`EnterE2EPasswordModal.tsx:handleValidatePassword`). A CBC "wrong password with valid padding"
   does not throw there: the modal closes with the success toast `E2E_encryption_enabled`, then
   `loadKeys` fails to parse/import the garbage JWK → `ERROR`, and `startClient` immediately sets
   `READY` anyway with no private key (`loadKeys` swallows the error), so every room then goes
   `READY` without a key (§7.2 step 2) [src]. "Do it later" closes the modal **and the banner** with
   toast `End_To_End_Encryption_Not_Enabled`; state stays `ENTER_PASSWORD` and `startClient` never
   finishes in that session; encrypted rooms still offer "Enter your E2E password" in the room body
   (`e2e.decodePrivateKeyFlow`, §10).
4. If public+private key are available → import → `READY`.
5. Otherwise (no keys anywhere) `createAndLoadKeys`: generate RSA pair, cache both,
   `POST e2e.requestSubscriptionKeys`; state `READY` (room handshakes start now, before the keys
   are on the server); then, because the server has no keys, `LOADING_KEYS`, generate a
   passphrase (§3.4), store it locally (`e2e.randomPassword`), upload with
   `setUserPublicAndPrivateKeys` (no `force`). The same upload-with-a-new-random-passphrase path
   runs whenever the server lacks keys but local storage still has a pair. (RN uploads first and
   calls `requestSubscriptionKeys` afterwards.)
6. If a generated passphrase is still stored → `SAVE_PASSWORD`: non-closable banner "Save your new
   E2EE password" → modal showing the passphrase with a Copy button, "I saved my password" (deletes
   the stored passphrase, state `READY`, toast `E2E_encryption_enabled`) / "Do it later".
7. On `READY`/`SAVE_PASSWORD` (`onE2EEReady`): handshake all room instances, process pending
   `E2ESuggestedKey`s, decrypt subscriptions' last messages, start the key distribution loop,
   observe subscriptions.

Logout (`stopClient`, on the DDP `onLogout`): clear the cached public/private keys, drop room
instances, stop the loop, `DISABLED`. It does **not** remove `e2e.randomPassword`: a pending
generated passphrase survives logout in the browser's storage (`RC:apps/meteor/client/lib/sdk/storage.ts`).

Rustket mapping:

- Persist the **decrypted private JWK** and the **pending generated passphrase** in the secret store
  (`rustket-core::secrets`, new keys `rustket-e2e:<server_id>:<user_id>:private_key` and
  `…:pending_passphrase`); never in config or the cache DB. Delete both on explicit logout and
  server removal, but first warn (and show the passphrase again) while one is still unsaved. A
  `force_logout` (only when it names this DDP session or no session) deletes the cached key and
  keeps an unsaved passphrase: the next start unlocks the key backup with it and shows it again.
  A secret store read error is never treated as "no cached key" (nothing is deleted; the start
  fails and is retried with backoff, as is a failed `e2e.fetchMyKeys`). The public key can be
  cached unencrypted.
- Until `E2E_Enable` is known (`settings.public` failed), encrypted rooms are blocked (text,
  uploads, edits, slash commands); the E2E settings are fetched again with backoff.
- Always reconcile with `e2e.fetchMyKeys`: if the server's `public_key` differs from the cached one
  (keys were reset on another device), drop the cached private key and go to `ENTER_PASSWORD`
  (or key creation if the server has none). The web does not do this (it trusts local storage).
- Passphrase change (`WEB:rocketchat.e2e.ts:changePassword`): re-encrypt the cached private JWK
  with the new passphrase (v2) and `POST setUserPublicAndPrivateKeys {public_key, private_key, force: true}`;
  if a generated passphrase was pending, replace it. Web enables the form only in state `READY` or
  `SAVE_PASSWORD` (`ChangePassphrase.tsx:useKeysExist`; otherwise "first enter your current E2EE
  password").
- Reset ("Forgot E2EE password?" link in the password modal, and Account → Security → "Reset E2EE
  password"): confirm with `Reset_E2EE_password_description`, `POST users.resetE2EKey {}`, toast
  `E2EE_password_reset`, log out (`RC:apps/meteor/client/views/hooks/useResetE2EPasswordMutation.ts`).
  The server revokes the token anyway.

### 7.2 Room crypto state machine

Web states (`WEB:E2ERoomState.ts`): `NOT_STARTED`, `ESTABLISHING`, `CREATING_KEYS`,
`WAITING_KEYS`, `KEYS_RECEIVED`, `READY`, `DISABLED`, `ERROR`. Allowed transitions
(`WEB:rocketchat.e2e.room.ts:48`): from `NOT_STARTED` → `ESTABLISHING|DISABLED|KEYS_RECEIVED`;
`ESTABLISHING` → `READY|KEYS_RECEIVED|ERROR|DISABLED|WAITING_KEYS|CREATING_KEYS`;
`READY` → `DISABLED|CREATING_KEYS|WAITING_KEYS`; `WAITING_KEYS` → `KEYS_RECEIVED|ERROR|DISABLED`;
`ERROR` → `KEYS_RECEIVED|NOT_STARTED`; states not listed (`CREATING_KEYS`, `KEYS_RECEIVED`,
`DISABLED`) may go anywhere. Every transition re-runs `handshake()`, which acts only in
`NOT_STARTED`/`KEYS_RECEIVED` and only when the account is ready.

Handshake (`WEB:rocketchat.e2e.room.ts:312`):

1. → `ESTABLISHING`.
2. If my subscription has `E2EKey`: unwrap with my private key (§3.6), import the JWK (§3.5),
   set `keyID = roomKeyId || kid || randomUUID()` (only if not already set; `roomKeyId` is the
   room's `e2eKeyId` as seen when the instance was created) → `READY`.
   Only a malformed wrapped string (PrefixedBase64 `RangeError`) → `ERROR`: `importGroupKey`
   *returns* `false` on RSA/JWK failure instead of throwing, so the 8.9 web goes to `READY`
   **without a key** (then shows `E2E_indecipherable` and fails to encrypt). Rustket: treat an
   undecryptable `E2EKey` as "no key" (→ waiting/needs reset), never as ready.
   Rustket labels the key with the wrapped **prefix** kid (the room's `e2eKeyId` only for legacy
   keys without a prefix, else a random UUID). If the prefix differs from the room's `e2eKeyId`
   the key is kept as an old key and the room waits for the room update: `resetRoomKey.ts`
   notifies the resetter's subscription (new `E2EKey`) **before** the room (new `e2eKeyId`), so a
   reset from my other device arrives subscription-first [src].
3. Else if the room has no `e2eKeyId` → `CREATING_KEYS`: generate an A256GCM key and UUID kid;
   `POST e2e.setRoomKeyID`; on `error-room-e2e-key-already-exists` discard the key → `WAITING_KEYS`;
   on success wrap for myself → `POST e2e.updateGroupKey {rid, uid: me, key}` → wrap for every
   user of `GET e2e.getUsersOfRoomWithoutKey` that has a public key (plus old keys, §7.4) →
   `POST e2e.provideUsersSuggestedGroupKeys {usersSuggestedGroupKeys: {rid: [...]}}` → `READY`.
   (With E2E on the server, that last call only works because `E2E_Enable` is true.) If wrapping
   fails for any single user, the web abandons the whole provide call for that room (the loop
   `return`s); if wrapping for myself fails, neither call is made but the room still goes `READY`.
   Rustket: if wrapping for myself or `updateGroupKey` fails, the room goes `ERROR` and nothing
   is encrypted or provided (a key held only in RAM makes every message unreadable after a
   restart); the store is retried on the next room/subscription update.
4. Else → `WAITING_KEYS` (web also publishes the dead `e2ekeyRequest`, §6). When
   `E2E_Allow_Unencrypted_Messages` is false, the UI shows "Check back later — {room} encryption
   keys need to be updated to give you access. Another room member needs to be online for this to
   happen." instead of the room body (`RC:apps/meteor/client/views/room/E2EESetup/RoomE2EESetup.tsx`,
   gated in `RC:apps/meteor/client/views/room/Room.tsx` by `room.encrypted && E2E_Enable &&
   !E2E_Allow_Unencrypted_Messages`); otherwise the normal room body and composer are shown (with
   the unencrypted-message hint, §7.6).

On entering `READY` for the first time: decrypt old room keys (`oldRoomKeys`, failures stored as
`null` and skipped), re-decrypt pending messages of the room, decrypt the last message. These are
`once('READY')` handlers: after an in-session room key reset (`onRoomKeyReset` sets `oldKeys =
undefined`) they do not run again, so the web cannot decrypt pre-reset messages loaded afterwards
until a reload [src]. Rustket: re-derive old keys whenever `oldRoomKeys` changes.

Subscription changes (`WEB:rocketchat.e2e.ts:onSubscriptionChanged`) for subs that are
`encrypted` or have `E2EKey`:

- `E2ESuggestedKey` present → `importGroupKey(suggested)`: success → `POST
  e2e.acceptSuggestedGroupKey {rid}` and `keyReceived()`; failure → `POST
  e2e.rejectSuggestedGroupKey {rid}`. (The web returns "success" without decrypting when the
  suggested kid equals the current kid and a key is loaded — Rustket must decrypt and compare,
  see §11.)
- `sub.encrypted` toggles pause/resume of encryption for sending. In practice `getInstanceByRoomId`
  returns `null` for any room whose `room.encrypted` is false, so a room with encryption disabled
  has no instance at all: the web neither encrypts nor decrypts there (old encrypted messages stay
  undecrypted), and `pause()` is effectively unreachable.
- Unsupported room type → `DISABLED`.
- `E2EKey` arrives while `WAITING_KEYS` → `KEYS_RECEIVED` → handshake → `READY`.

Room key reset detection (`WEB:rocketchat.e2e.ts:getInstanceByRoomId`): when `room.e2eKeyId`
changes and differs from the instance's `keyID` → `onRoomKeyReset(newKid)`: `WAITING_KEYS`, drop
the current key and old keys, remember the new kid. Rustket keeps the dropped key as an old key and
never reuses an `E2EKey` whose prefix is an older kid; an `E2EKey` already prefixed with the new
kid (subscription-first ordering above) stays usable. When `room.encrypted` turns false Rustket
keeps its keys for decrypting history but reports `DISABLED` and refuses to encrypt. The new key arrives as `E2ESuggestedKey`
(plus server-side `suggestedOldRoomKeys`) from whoever reset or holds it.

### 7.3 Creating the first room key — race handling

Several members may try at once; `setRoomKeyID` is the arbiter (atomic). Order matters: claim the
kid first, only then publish wrapped keys. A client that loses the race must not have written any
`E2EKey`. If the winner crashes between `setRoomKeyID` and `updateGroupKey`, the room has an
`e2eKeyId` that nobody holds: everyone waits forever; the remedy is a room key reset by a member
with `toggle-room-e2e-encryption` (web offers it in the Disable dialog when the room state is
`NOT_STARTED|DISABLED|ERROR|WAITING_KEYS`, §7.9).

### 7.4 Key distribution loop

`WEB:rocketchat.e2e.ts:initiateKeyDistribution` (RN equivalent `RN:app/lib/encryption/encryption.ts:494`):

- Run once right after the account becomes ready, then every **10 s**.
- Candidate rooms: rooms in the local store that have `usersWaitingForE2EKeys` and where I am not
  in that list.
- Sample up to `ROOM_KEY_EXCHANGE_SIZE = 10` random candidates; keep those whose room instance has a
  session key; if none, resample (up to 3 attempts in total).
- `GET e2e.fetchUsersWaitingForGroupKey?roomIds[]=…`.
- For each returned room whose instance is `READY`: wrap the current room key for each user's
  `public_key`; if I have old room keys, also wrap each decrypted old key (same 256-byte RSA,
  prefixed with that old key's `e2eKeyId`) → `oldKeys: [{e2eKeyId, ts, E2EKey}]`.
- `POST e2e.provideUsersSuggestedGroupKeys` with the map. Errors are logged and retried next tick.

Rustket: same algorithm as a `rustket-core` task per workspace, active only while the account is
ready and `E2E_Enable` is true; jitter the period (e.g. 10 s ± 2 s) and cap per tick work (≤ 10
rooms × ≤ 50 users ⇒ ≤ 500 RSA encryptions, run on a blocking thread).

### 7.5 Room key reset (initiator)

`RC:apps/meteor/client/views/room/hooks/useE2EEResetRoomKey.ts`, `WEB:rocketchat.e2e.room.ts:resetRoomKey`:

1. → `CREATING_KEYS`; generate a new A256GCM key and UUID kid; wrap for my public key → `READY`.
2. `POST e2e.resetRoomKey {rid, e2eKey, e2eKeyId}`.
3. Others get the key via the distribution loop (they are now in the queue and lack `E2EKey`).

Messages encrypted with the previous key remain readable for members whose `oldRoomKeys` contain
it. Members who had no key before the reset cannot read older messages unless someone provides
old keys (`oldKeys`), and only if their subscription has no `oldRoomKeys` at accept time.
Rustket should do step 2 before switching its local key (the web switches first); on failure keep
the old key. The initiating web client does not move the replaced key into its in-memory old keys
and its `once('READY')` handlers have already run (§7.2), so it falls back to the new key for
pre-reset messages (→ `E2E_Key_Error`) until a reload [src]. Rustket: after a successful reset, add
the previous key under its old kid to the room's old keys immediately (the server adds it to
`oldRoomKeys` as well).

### 7.6 Sending

`RC:apps/meteor/client/views/root/hooks/loggedIn/useE2EEncryption.ts`, `RC:apps/meteor/client/lib/chats/flows/sendMessage.ts`:

- Encrypt if: room instance exists, room is `READY`, room `encrypted` (not paused), and the text
  does not start with `/` (`shouldConvertSentMessages`). Slash commands run through the normal
  command flow before this point, but only if `isSlashCommandAllowed` (§2.1): with E2E on, an
  encrypted room and unencrypted messages not allowed, nothing is executed and the `/…` text is
  sent **unencrypted** and rejected by the server (`error-not-allowed` + ephemeral message). When
  commands are allowed, an unknown `/…` produces a local "No such command" ephemeral message and is
  not sent unless `Message_AllowUnrecognizedSlashCommand` (dev: `false`), in which case it goes out
  **unencrypted** (`RC:apps/meteor/client/lib/chats/flows/processSlashCommand.ts`).
- Mentions (if `E2E_Enabled_Mentions`): from the plaintext, after removing markdown links
  `\[[^\]]*\]\([^)]+\)`, collect matches of
  `(^|\s|>)@(P(@(P))?(:([0-9a-zA-Z-_.]+))?)` (users) and `(^|\s|>)#(P(@(P))?)` (channels), flags
  `gm`, `P = UTF8_User_Names_Validation`, each match `trim()`med
  (`RC:apps/meteor/app/mentions/lib/MentionsParser.ts`). Result `{e2eUserMentions: ["@alice", "@all"], e2eChannelMentions: ["#general"]}`.
  The matched prefix is part of the match, so a `>` prefix survives the trim (`>@alice` →
  `">@alice"`); the server strips only a leading `@`/`#` (`Mentions.ts:convertMentionsToUsers`), so
  such entries resolve to nobody [src]. Rustket may drop the leading `>` (harmless for interop:
  the lists are only used server-side for notifications).
  The server turns them into `mentions`/`channels` (`RC:apps/meteor/server/lib/messaging/mentions/Mentions.ts:47`)
  — this leaks mentioned usernames/channels in plaintext by design. RN builds the same lists
  without stripping markdown links first (`RN:app/lib/encryption/utils.ts:getE2EEMentions`).
  Upload messages carry no `e2eMentions` (the web computes them only in the text path and
  `rooms.mediaConfirm` rejects the field, §5.7).
- Encrypt `{msg, attachments}` (attachments omitted when undefined), v2 envelope, random 12-byte IV
  (16 for legacy CBC keys), `kid` = room `keyID`. Send `{_id, rid, tmid?, tshow?, t: 'e2e', content, e2eMentions?}`
  (no `msg`, no `e2e`).
- Threads: identical, `tmid`/`tshow` stay plaintext [live].
- Quotes: the composer prepends `[ ](<permalink>)\n` per quoted message to the text
  (`RC:apps/meteor/client/lib/utils/prependReplies.ts`), then the whole text is encrypted, so the
  server cannot build quote attachments. Permalink = `<room URL>?msg=<id>`.
- Edits: re-encrypt `{msg, attachments}` with the **current** key/kid and a fresh IV; when the
  original encrypted message had attachments, the decrypted originals are re-included
  (`sendMessage.ts:150`); `POST chat.update {msgId, roomId, content, e2eMentions}`
  (`RC:apps/meteor/client/lib/chats/data.ts:updateMessage`). Two web quirks: `files`/`file` are
  **not** re-encrypted (only `msg` and `attachments` go into `encryptMessage`), so an edited
  encrypted upload message decrypts to `{msg, attachments}` only — readers must tolerate that;
  and when the edited text starts with `/` or the room is not `READY`, the web sends
  `chat.update {text}`, i.e. a **plaintext edit** (§8). A third quirk [src]: the re-included
  "originals" are the local, already-processed `attachments`, which also contain the quote
  attachments `parseQuoteAttachment` appended on receive (§7.7), so editing a message that has any
  attachment re-encrypts those quote attachments and readers then get them twice (decrypted + rebuilt).
  Rustket: re-encrypt `{msg, attachments, files, file}` taken from the original *decrypted content*
  (never locally derived quote attachments), never fall back to `text`, and tolerate duplicate quote
  attachments when reading. A plaintext message (sent before encryption was turned on) cannot be
  edited into an encrypted one (`chat.update {content}` → "Only encrypted messages can have content
  updated." [live]; the 8.9 web hits this): Rustket edits it with `text` only when
  `E2E_Allow_Unencrypted_Messages` is on, otherwise it hides Edit and refuses before the edit text
  is dropped.
- If the room is encrypted but not `READY`, the web sends plaintext (server rejects unless
  unencrypted messages are allowed; the composer shows `E2EE_Composer_Unencrypted_Message` when
  they are allowed, `RC:apps/meteor/client/views/room/composer/messageBox/MessageBoxHint.tsx`).
  Rustket: **never downgrade silently** — disable sending while the room key is missing unless
  `E2E_Allow_Unencrypted_Messages` is true, in which case show the hint.
- Discussions from an encrypted room: the first-message field is disabled
  (`Discussion_first_message_disabled_due_to_e2e`), and the server refuses a `reply` for encrypted
  discussions (`error-invalid-arguments`); the server replaces the quoted parent text with
  the server-language string `Encrypted message` (`RC:apps/meteor/server/meteor-methods/messages/createDiscussion.ts:189`).

### 7.7 Receiving and decrypting

Entry points in the web: every message passes `onClientMessageReceived` (history, stream, thread
lists, search results, pinned/starred lists), plus subscription `lastMessage`, desktop
notifications, files lists and pinned-message attachments.

`decryptMessage` (`WEB:rocketchat.e2e.room.ts:736`):

- Only `t === 'e2e'` (web also skips messages already marked `e2e: 'done'`).
- With `content`: decrypt, then `Object.assign(message, decrypted)`; afterwards, if the
  server-side `attachments` contain `type: 'removed-file'` entries, replace the decrypted file
  attachments with the matching removed placeholder (same `fileId`, or all of them if a removed
  entry has no `fileId`). Mark `e2e: 'done'`.
- Without `content`: empty `msg` → done; otherwise decrypt `msg` as v1 and take only `msg`.
- Missing key → text `E2E_indecipherable` ("…cannot be decrypted due to multiple room key resets");
  decryption/parse error → `E2E_Key_Error` ("…cannot be decrypted due to incorrect encryption key").
- Quotes: find URLs in the decrypted `msg` with
  `getMessageUrlRegex()` (`RC:apps/meteor/lib/getMessageUrlRegex.ts`); keep those containing
  `Site_Url` with exactly one non-empty `msg` query parameter; `GET chat.getMessage?msgId=…`,
  decrypt recursively, append a quote attachment `{text, md?, message_link: url, author_name,
  author_icon, attachments, ts}` (`RC:apps/meteor/lib/createQuoteAttachment.ts`), trimmed by
  `Message_QuoteChainLimit` (`RC:apps/meteor/client/lib/limitQuoteChain.ts`).
- Pinned (`t: 'message_pinned_e2e'`): decrypt `attachments[0].content` and set
  `attachments[0].text = decrypted.msg` (`WEB:rocketchat.e2e.ts:decryptPinnedMessage`).
- Desktop notification: decrypt `payload.message.content`, `text = decrypted.msg ?? ''`.
- Files list / gallery: `file.content` decrypts to `{type, typeGroup, name, encryption, hashes}`
  which overrides the hashed name/type (`RC:apps/meteor/client/views/room/contextualBar/RoomFiles/hooks/useFilesList.ts`).
- Sidebar: `Encrypted message, preview unavailable` until the last message is decrypted
  (`RC:apps/meteor/client/lib/utils/normalizeMessagePreview/getMessagePreview.ts`); message body
  placeholder `E2E_message_encrypted_placeholder` while pending.

Rustket deviations (hardening):

- Do **not** merge arbitrary decrypted keys into the message. Whitelist `msg` (or legacy `text`),
  `attachments`, `files`, `file`; ignore `_id`, `rid`, `u`, `ts`, `t`, … from the plaintext.
- Keep the raw encrypted message and store the decrypted result next to it (e.g. a
  `DecryptedContent` in the timeline store keyed by message id + `_updatedAt`), so that messages can
  be re-decrypted when a key arrives later and edits invalidate the cache.
- Ignore the server's `e2e` field; decide by `t == 'e2e'` and content presence.
- Parse markdown from the decrypted `msg` client-side; ignore any server `md` on e2e messages (§4).
  Likewise ignore server `blocks` and `urls`, and keep server `attachments` only when the payload
  has none and only `removed-file` placeholders of them (a content-less message, §11.13, keeps its
  attachment); anything dropped marks the message as also carrying unencrypted content.
- Treat decrypted attachment URLs (`title_link`, `image_url`, …) as server-relative paths only;
  refuse absolute URLs to other origins (the plaintext is controlled by any room member).

### 7.8 Encrypted uploads and downloads

Web sequence (`RC:apps/meteor/client/lib/chats/flows/uploadFiles.ts`,
`RC:apps/meteor/client/lib/chats/uploads.ts`, `RC:apps/meteor/client/lib/chats/flows/processMessageUploads.ts`),
reproduced live:

1. Preconditions: room instance exists and `E2E_Enable_Encrypt_Files`; else plain upload (or refusal
   as in §2.1). Room must be `READY` (`Error_encrypting_file` otherwise).
2. `hash = sha256hex(plaintext)`; `iv = 16 random bytes`; random AES-256-CTR key; encrypt (§3.8);
   upload name `sha256hex(utf8(name))`.
3. `fileContentData = {type, typeGroup: type.split('/')[0], name, encryption: {key: JWK, iv: b64(iv)}, hashes: {sha256: hash}}`.
4. `POST rooms.media/{rid}` multipart: part `file` (encrypted bytes, filename = hashed name, no
   MIME → `application/octet-stream`), text part `content` = `JSON.stringify(v2 envelope of fileContentData)`.
   Response `{"file": {"_id", "url": "/file-upload/<id>/<hashed name>"}, "success": true}` [live].
   Invalid JSON in `content` → `invalid-field-content` (`RC:apps/meteor/server/api/v1/rooms.ts:298`).
   The server enforces `FileUpload_MaxFileSize` on the encrypted size (= plaintext size).
5. On send, for each upload (first one carries the composer text, the others `msg: ''`):
   - `fileContent` = v2 envelope of the (possibly edited: `name`, `altText`) metadata;
   - message `content` = v2 envelope of
     ```json
     {"attachments":[{"title":"<name>","type":"file","title_link":"<url>","title_link_download":true,
        "encryption":{"key":{JWK},"iv":"<b64>"},"hashes":{"sha256":"<hex>"},"fileId":"<id>",
        "image_url":"<url>","image_type":"<mime>","image_size":<n>,"image_dimensions":{"width":w,"height":h},"image_alt":"<alt>"}],
      "files":[{"_id":"<id>","name":"<name>","type":"<mime>","size":<n>,"format":"<EXT>"}],
      "file":{…same as files[0]},
      "msg":"<text>"}
     ```
     (`audio_*`/`video_*` instead of `image_*` for those types; for other types `size` and
     `format` = upper-cased extension or `file`, `RC:apps/meteor/lib/utils/getFileExtension.ts`).
   - `POST rooms.mediaConfirm/{rid}/{fileId}` JSON `{"tmid"?, "t":"e2e", "msg":"", "content": {…}, "fileContent": {…}}`
     (no `description`/`fileName`: they would be stored in plaintext, §5.7).
     The server allows only `avatar, emoji, alias, groupable, msg, tmid, customFields, t, content`
     besides `description`/`fileName`/`fileContent` (`RC:apps/meteor/server/meteor-methods/messages/sendFileMessage.ts:205`);
     `content` must include `algorithm` and `ciphertext`; any other key (e.g. `e2eMentions`) → 400
     `Match error` [live]. It stores `fileContent` as the upload's `content`, and builds
     **plaintext** `file`/`files` (`{_id, name: <hashed>, type: "application/octet-stream", size,
     format: "", typeGroup: "application"}`) and `attachments` (`[{title: <hashed>, title_link,
     title_link_download: true, type: "file", format: "file", size, fileId}]`) [live]. The
     decrypted `content` replaces them on the client. Response `{"message": {…}, "success": true}`.
6. If the room is not ready to encrypt messages at confirm time, or the caption starts with `/`
   (`shouldConvertSentMessages`), the web still sends `t: 'e2e'` without `content` (message shows
   only the server's hashed-name attachment; a `/…` caption is lost). Rustket: don't.
7. An upload that is never confirmed stays as a temporary file with `expiresAt` = upload + 24 h
   (`RC:apps/meteor/server/api/v1/rooms.ts:289`); a retry after that must re-upload.

Download/decrypt: `GET <title_link>` with auth headers (or `rc_uid`/`rc_token` cookies) returns the
ciphertext with `Content-Type: application/octet-stream`,
`Content-Disposition: attachment; filename*=UTF-8''<hashed name>` [live]; decrypt with the JWK `k`
and `iv` from the decrypted attachment (`Ctr64BE`), verify `hashes.sha256`, then use the decrypted
`name`/`type` for saving and previews. The server generates no thumbnails for encrypted images; the
web shows the full image (`image_url` = file URL). Decrypt streaming (CTR allows it) and keep only
decrypted images in memory caches, never write decrypted files except where the user saves them.

### 7.9 Enabling/disabling encryption on a room (web UI)

`RC:apps/meteor/client/hooks/roomActions/useE2EERoomAction.ts`:

- Toolbox action `e2e` (icon `key`, groups `direct`, `direct_multiple`, `group`, `team`) shown if
  `E2E_Enable` and (`room.t === 'd'` or (`edit-room` and `toggle-room-e2e-encryption`)) and
  (account ready or room already encrypted). Disabled with tooltip for federated rooms.
  Title `Enable_E2E_encryption` / `Disable_E2E_encryption`.
- Enable dialog: `E2E_enable_encryption_description`. Disable dialog:
  `E2E_disable_encryption_description`, plus, if `canResetRoomKey`, a link
  `E2E_disable_encryption_reset_keys_description` leading to the reset dialog
  (`E2E_reset_encryption_keys_modal_description`). `canResetRoomKey = E2E_Enable && account ready
  && (room.t === 'd' || toggle-room-e2e-encryption) && room state ∈ {NOT_STARTED, DISABLED, ERROR, WAITING_KEYS}`
  (note: the server still requires the permission for DMs, §5.5).
- Confirm → `POST rooms.saveRoomSettings {rid, encrypted: !room.encrypted}` → toast
  `E2E_Encryption_enabled_for_room` / `E2E_Encryption_disabled_for_room`; if auto-translate was on,
  also `AutoTranslate_Disabled_for_room`.
- Create dialogs: "Encrypted" toggle only for private rooms, disabled when E2E is off or forced;
  pre-set from `E2E_Enabled_Default_PrivateRooms`/forced setting
  (`RC:apps/meteor/client/navbar/NavBarPagesGroup/actions/CreateChannelModal.tsx`).

---

## 8. What is encrypted and what is not

| Data | Encrypted? |
|---|---|
| Message text (`msg`), quotes (as permalinks inside `msg`) | yes (`content`) |
| Attachments of uploads (title = real file name, MIME, dimensions, alt text, file key/iv/hash), `files`, `file` | yes (`content`) |
| Upload metadata on the upload record | yes (`rocketchat_uploads.content`) |
| File bytes | yes (AES-CTR) |
| File size, upload time, uploader, hashed name | no |
| Thread structure (`tmid`, `tshow`, `tcount`, `replies`), reactions, pins/stars, read receipts, `editedAt`/`editedBy`, `ts`, sender | no |
| `e2eMentions` and the server-computed `mentions`/`channels` | no |
| Pin system message | references the same `content` (still encrypted) |
| Discussion created from a message | parent text replaced by `Encrypted message`; discussion name/topic plaintext |
| Room name, topic, announcement, description, avatar, members | no |
| Slash commands (text starting with `/`) | no |
| Message edited with `text` instead of `content` | **no** — the server accepts `chat.update {text}` on an e2e message (no `E2E_Enable`/room check on edits) and stores the plaintext in `msg` next to the old `content` [live, re-checked]; the 8.9 web itself does this when the edited text starts with `/` or the room key is not loaded (§7.6); never do this from Rustket |
| Upload caption mentions | not sent at all (no `e2eMentions` on uploads, §5.7) |
| `description` / `fileName` sent to `rooms.mediaConfirm` | **no** — stored and published in plaintext (attachment, `file`/`files`, upload record) [live]; the web never sends them for encrypted uploads; never send them from Rustket (§5.7) |
| `md` sent by the client with an e2e message | **no** — stored and returned as sent [live]; never send it, never render a server `md` for e2e messages (§4) |
| Push notifications | text is `<sender>: ` with empty body; payload contains `msg`, `content`, `messageType: 'e2e'` (`RC:apps/meteor/server/lib/notifications/message/mobile.js:55`); for legacy v1-in-`msg` messages the text is `Encrypted_message` (`RC:apps/meteor/server/lib/notifications/message/index.ts:26`) |
| Search | encrypted content is not searchable (`Encrypted_content_cannot_be_searched`) |

---

## 9. System and notification texts

| Event | Server data | Display (en / ru where available, `RC:packages/i18n/src/locales/{en,ru}.i18n.json`) |
|---|---|---|
| Encryption enabled | `t: room_e2e_enabled`, `msg: <username>` | `Enabled_E2E_Encryption_for_this_room` "enabled E2E encryption for this room" / "включено шифрование E2E для этой комнаты" |
| Encryption disabled | `t: room_e2e_disabled` | `Disabled_E2E_Encryption_for_this_room` "disabled E2E encryption for this room" / "отключено шифрование E2E для этой комнаты" |
| Pin of e2e message | `t: message_pinned_e2e` | `Pinned_a_message` |
| Desktop notification | text `"<sender>: "` | replace with decrypted `msg` |
| Undecryptable | — | `E2E_indecipherable`, `E2E_Key_Error` (no ru) |
| Pending | — | `E2E_message_encrypted_placeholder` "This message is end-to-end encrypted. To view it, you must enter your encryption key in your account settings." |
| Sidebar preview | — | `Encrypted_message_preview_unavailable` "Encrypted message, preview unavailable" |
| Generic | — | `Encrypted_message` "Encrypted message" / "Зашифрованное сообщение" |

Message types registered in `RC:packages/message-types/src/registrations/e2ee.ts`; in the
hide-system-messages lists the values are the message types `room_e2e_enabled` / `room_e2e_disabled`
(labels `Message_HideType_room_enabled_encryption` / `Message_HideType_room_disabled_encryption`,
`RC:apps/meteor/app/lib/lib/MessageTypes.ts:101`). Most E2E strings have no Russian translation
upstream; Rustket must provide its own.

---

## 10. Web UI behaviours worth copying

- Global banner (id `e2e`) with three variants: "Enter your E2E password" (closable=false),
  "Save your new E2EE password" (closable=false), "Wasn't possible to decode your encryption key
  to be imported" (danger, untranslated; click retries) (`WEB:rocketchat.e2e.ts`). The last one sits
  in `startClient`'s `catch`, but the password promise there never rejects (modal errors are
  handled inside the modal), so in 8.9 it is effectively unreachable [src].
- Enter-password modal: password field, "Enable encryption" / "Do it later", "Forgot E2EE password?"
  → reset confirmation (`EnterE2EPasswordModal.tsx`).
- Save-password modal: explanation `E2E_password_reveal_text`, bold `E2E_password_save_text`
  ("This will only be displayed once, please save it now."), code block with Copy button,
  "I saved my password" / "Do it later", annotation "You can do it from account preferences"
  (`RC:apps/meteor/client/views/e2e/SaveE2EPasswordModal.tsx`).
- Room body replacement (`RoomE2EESetup.tsx`; only for `room.encrypted && E2E_Enable &&
  !E2E_Allow_Unencrypted_Messages`, `Room.tsx`): `SAVE_PASSWORD` → "{room} is encrypted / Save your
  end-to-end encryption password to access" + button; `ENTER_PASSWORD` → "… Enter your end-to-end
  encryption password to access" + button (`e2e.decodePrivateKeyFlow`); room `WAITING_KEYS` → clock
  icon "Check back later". In the same states the header toolbox is reduced to room info, members and
  the E2E action (`RC:apps/meteor/client/views/room/Header/RoomHeaderE2EESetup.tsx`, chosen in
  `Header.tsx` when `room.encrypted && !E2E_Allow_Unencrypted_Messages`).
- Account → Security → End-to-end encryption: change passphrase with live policy checklist
  (§3.3.4) and confirmation field; reset section (`RC:apps/meteor/client/views/account/security/`).
- Message actions disabled for e2e messages with tooltip `Action_not_available_encrypted_content`:
  Copy link, Reply in direct message, Forward, Apps menu
  (`RC:apps/meteor/client/components/message/toolbar/`).
- Slash-command popup items disabled (`Unavailable_in_encrypted_channels`) when the room is
  encrypted, E2E on and unencrypted messages not allowed (`RC:apps/meteor/client/views/room/providers/ComposerPopupProvider.tsx:103`).
- Search tab hint `Encrypted_content_cannot_be_searched`.
- Header key icon (title `Encrypted`, shown when `E2E_Enable && room.encrypted`,
  `RC:apps/meteor/client/views/room/Header/icons/Encrypted.tsx`), create-room hint
  `Encrypted_messages`/`Encrypted_not_available` (`useEncryptedRoomDescription.ts`).

---
## 11. Edge cases and pitfalls

1. **Suggested key with the current kid** — web `importGroupKey` returns `true` without decrypting
   when `kid == keyID` and a key is loaded, then accepts; a garbage suggested key with the right
   prefix would overwrite a good `E2EKey`. Rustket: always unwrap; if it fails → reject; if it
   decrypts to a key different from the working one with the same kid → reject and log at warn
   (kid only).
2. **Suggested kid ≠ `room.e2eKeyId`** — likely a stale key from before a reset. Reject (the server
   re-queues us and a current key will be provided).
3. **`updateGroupKey`/`provideUsersSuggestedGroupKeys` can target users who already have
   `E2EKey`** [live]; rejecting such a suggestion also re-queues a user who has a valid key — harmless.
4. **Legacy rooms**: A128CBC room key + v1 messages (kid 12 chars) + possibly v2 envelopes with a
   16-byte IV and CBC ciphertext written by the 8.9 web (§3.7). Never infer the mode from the
   algorithm string.
5. **Room key reset during composition**: encrypt at send time with the then-current key; on a
   `rooms-changed` `e2eKeyId` change mid-send, the server still accepts the message (kid is not
   validated); recipients find the old key in `oldRoomKeys`.
6. **Old keys missing after reset** for members who had no key before: show `E2E_indecipherable`
   for older messages; nothing else to do.
7. **`oldRoomKeys` entries that fail to unwrap** (user key reset since): skip silently.
8. **User key reset** (`users.resetE2EKey`) revokes tokens: expect 401 + `force_logout`; clear E2E
   secrets; on next login create new keys and wait for room keys.
9. **Passphrase charset** (§3.3.4).
10. **Unknown `alg` in a room JWK / unknown envelope algorithm** (`m.megolm.v1.aes-sha2`): show
    "cannot be decrypted", do not crash, do not retry in a loop.
11. **v1 parsing**: kid is always the first 12 chars even when the real kid is a UUID.
12. **`E2E_Enable` turned off while rooms are encrypted**: the web sets the account state to
    `DISABLED` (`useE2EEncryption.ts`; it does not call `stopClient`, so cached keys stay in local
    storage), detaches its decrypt/encrypt hooks and shows raw messages (empty `msg`). Rustket: keep
    decrypting with cached keys (read-only) but do not send encrypted messages (server would accept
    plaintext then); show a banner "encryption disabled by the administrator" (the upstream i18n key
    `Admin_disabled_encryption` exists but the 8.9 web does not use it). This holds after a restart
    too: with `E2E_Enable` off, `start()` loads a cached private key that still matches the
    server's public key (account state `DisabledReadOnly`) and handshakes read-only (no key
    creation, accept/reject or distribution).
13. **Encrypted message without `content` and empty `msg`** (web confirm-upload edge, §7.8 step 6):
    render as empty encrypted message with the server's attachment.
14. **Plaintext edits of e2e messages** (§8): if `msg` is non-empty and `content` exists, prefer the
    decrypted content and mark the message as "contains unencrypted text" (web silently prefers
    the decrypted `msg` because `Object.assign` overrides it). Not when `msg` equals
    `content.ciphertext`: older RN releases stored the v1 ciphertext in both (upstream fixture
    `msgv1mob`).
15. **EJSON**: decrypted plaintext may contain `{"$date": ms}`; never emit top-level keys starting
    with `$` when encrypting.
16. **`usersWaitingForE2EKeys: []`** can remain after a user key reset or a room key reset without
    other key holders [live]; the web treats an empty list as "needs distribution"
    (`[].every(...) === true`) — harmless extra polling.
17. **`resetRoomKey` lock leak** on the access-denied path (§5.5): wait 10 s before retrying.
18. **DM reset permission mismatch** (§7.9): hide the reset option in DMs unless the user has
    `toggle-room-e2e-encryption` there.
19. **Concurrency in Rustket**: serialize per-room key operations (handshake, accept/reject, reset,
    distribution) through one actor/mutex per room; the server operations are not idempotent
    (accept twice → `error-no-suggested-key-available` [live]).
20. **RSA cost**: UNVERIFIED estimates (not measured for RustCrypto `rsa`): generating a 2048-bit
    key ~50–500 ms in release builds (much slower in debug), decrypt ~1–5 ms; run in
    `spawn_blocking`. PBKDF2 with 100 000 iterations ~50–100 ms: also blocking.
21. **Logging**: never log passphrases, JWKs (`k`, `d`, …), wrapped keys, plaintext or ciphertext;
    log only rid, kid, message id, state transitions (debug level).
22. **Client-controlled `e2e` field** (§4): the server stores `e2e` as sent [live]; never use it to
    decide whether to decrypt.
23. **Undecryptable own `E2EKey`** (e.g. wrapped for an old key pair): the web silently goes
    `READY` without a key (§7.2); Rustket must surface "key missing" and keep the room in the
    waiting state (a `rejectSuggestedGroupKey`-style re-queue is not available for `E2EKey`; the
    remedy is another member's suggestion or a room key reset).
24. **Unique `E2EKey` index** (§4): a duplicate wrapped string makes `acceptSuggestedGroupKey` /
    `updateGroupKey(self)` fail with a raw Mongo `E11000` error; map it to a generic failure.
25. **`resetRoomKey` hides its reason** (§5.5): `error-e2e-key-reset-failed` covers "caller has no
    keys", "room has no `e2eKeyId`" and "room not found"; check those client-side first to show a
    useful message.
26. **`users.resetE2EKey` 2FA** (§5.1): be ready for `totp-required` with `method: "password"` even
    for users who never enabled 2FA.
27. **Plaintext side channels on uploads** (§5.7): `description`/`fileName` on `rooms.mediaConfirm`
    are stored in plaintext; the encrypted upload path must send only `t`, `msg: ""`, `tmid?`,
    `content`, `fileContent`.
28. **Client-supplied `md`/`e2e` on e2e messages** (§4): both are stored as sent; ignore them when
    rendering/deciding, never send them.

---

## 12. Comparison with RN (`RN:app/lib/encryption/`)

| Topic | Web 8.9 | RN | Rustket |
|---|---|---|---|
| Crypto backend | WebCrypto | `@rocket.chat/mobile-crypto` (native) | RustCrypto |
| Private key envelope write | v2, salt `v2:<uid>:<uuid>` | v2 salt `v2:<uid>:mobile` (≥ 7.13), else v1 | v2 web form |
| Password bytes | Latin-1 (throws > U+00FF) | UTF-8 | ASCII on write; Latin-1 then UTF-8 on read |
| New room key | A256GCM, UUID kid | same on ≥ 7.13; A128CBC + sha256-12 kid on 7.0–7.12; base64-12 kid < 7.0 | A256GCM, UUID |
| Creator's own key | `updateGroupKey(self)` then suggest to others | only `provideUsersSuggestedGroupKeys` (includes self via `getUsersOfRoomWithoutKey`), accepts later | web flow |
| Message plaintext | `{msg, attachments?}` | `{msg}` | `{msg, attachments?}` |
| Envelope for CBC keys | v2 with 16-byte IV | v1 string | v2 (like web) |
| Decrypt key choice | old key by kid, else current | current only if kid matches, else old by kid | exact match, then current fallback |
| Suggested key that fails to import | reject | logs, no reject (`room.ts:166`) | reject |
| Key request | publishes dead `e2ekeyRequest` | publishes dead request, listens on `{uid}/e2ekeyRequest` (never fires on 8.9) | queue + polling only |
| Key distribution loop | 10 s, ≤ 10 rooms, 3 sampling tries | same (`encryption.ts:494`; resamples only while `roomIds.length > remaining tries`) | same |
| File cipher | AES-256-CTR, 64-bit counter (WebCrypto `length: 64`) | native AES-CTR | `Ctr64BE<Aes256>` |
| File hash check | no (service worker) | yes, deletes on mismatch | yes |
| Upload MIME | none → `application/octet-stream` | `file` (server stores `application/octet-stream` either way [live]) | `application/octet-stream` |
| Edit of an encrypted message | re-encrypts `{msg, attachments}` (drops `files`/`file`), sends `e2eMentions` | re-encrypts `{msg}` only — an edited upload loses its decrypted attachment (file key) — and sends no `e2eMentions` (`RN:app/lib/methods/editMessage.ts`) | `{msg, attachments, files, file}` + `e2eMentions` |
| Undecryptable own `E2EKey` | room goes `READY` without a key | handshake falls through to request/create | treat as missing key |
| Decrypted fields merged | everything (`Object.assign`) | everything + marks attachments `e2e: 'pending'` | whitelist |
| `requestSubscriptionKeys` | REST | REST on ≥ 8.6, DDP before | REST |
| Passphrase generator / wordlist | BIP-39, 12 words, rejection sampling | identical | identical |

---

## 13. Test vectors

All vectors below were produced with Node 24 WebCrypto using a line-by-line port of `WEB:`
(`lib.mjs` in that session's scratchpad, not kept) and cross-checked with Python `cryptography`;
all of them were re-derived independently with Python `cryptography` on 2026-11-08 (including the
upstream `keychain.spec.ts`/`content.spec.ts` decryptions and the 128-bit-counter divergence of
§13.6). They are deterministic unless marked decrypt-only.

### 13.1 Upstream vectors (copy verbatim into fixtures)

`RC:apps/meteor/client/lib/e2ee/keychain.spec.ts` — both verified to decrypt to the given
`private_key` [vec]:

- `pwdv1web`: userId `userE2EE`, passphrase `minus mobile dexter forest elvis`, v1 `$binary` envelope.
- `pwdv2web`: userId `BFqA4FAHKX4qEywfJ`, passphrase
  `prize fluid crystal small jaguar lunar bonus absent destroy settle carbon ignore`, v2 envelope with
  salt `v2:BFqA4FAHKX4qEywfJ:5b36c4cc-4e7a-497d-b4b0-a1df863c65c4`.

`RC:apps/meteor/client/lib/e2ee/content.spec.ts` [vec]:

- A128CBC key `{"alg":"A128CBC","ext":true,"k":"qb8In0Rpa9nwSusvxxDcbQ","key_ops":["encrypt","decrypt"],"kty":"oct"}`:
  - `{"algorithm":"rc.v1.aes-sha2","ciphertext":"32c9e7917b78LHjHfqLMeDn+2UK1PhD/soFe8CVwvFdLkslcfxNHby4="}` → `{"msg":"hello"}`
  - v1 mobile (in `content` and in `msg`):
    `32c9e7917b783JpM8aOVludqIRzx+DOqjEU9Mj3NUWb+/GLRl7sdkvTtCMChH1LBjMjJJvVJ6Rlw4dI8BYFftZWiCOiR7TPwriCoSPiZ7dY5C4H2q8MVSdR95ZiyG7eWQ5j5/rxzAYsSWDA9LkumW8JBb+WQ1hD9JMfQd4IXtlFMnaDgEhZhe/s=`
    → `{"_id":"AZF8Myj605B3f7ZPL","text":"world","userId":"RQTYT5RJoDKZFwDhk","ts":{"$date":1759174115076}}`
- A256GCM key `k = 9o1xoHt4OamRJvnaLna-5akUb5L98S_iWYGGaXPZ1Yg`:
  `{"algorithm":"rc.v2.aes-sha2","kid":"f46d2864-0384-4a87-8815-51fba2cad216","iv":"wXbYQ8q9sYRCHtNp","ciphertext":"cIDO9mXzCCrrl/wORP0Jf6oWeusqzSCXVGGvY7CHrA=="}`
  → `{"msg":"hello"}` (encrypting the same plaintext with the same IV reproduces the ciphertext).

`RC:apps/meteor/client/lib/e2ee/prefixed.spec.ts`: prefix `32c9e7917b78` + 344-char base64 roundtrip.

### 13.2 PBKDF2-HMAC-SHA256, dkLen 32

| password | salt | iterations | derived key (hex) |
|---|---|---|---|
| `minus mobile dexter forest elvis` | `userE2EE` | 1000 | `7feb6c85cd105a8bb51c01a8604d357143b1b251ef6b854051c3c90cae233070` |
| `prize fluid crystal small jaguar lunar bonus absent destroy settle carbon ignore` | `v2:BFqA4FAHKX4qEywfJ:5b36c4cc-4e7a-497d-b4b0-a1df863c65c4` | 100000 | `8443db571f0dfbfd1acfa2770a84bd76402ce02daad360fad086cdc75ca578ec` |

### 13.3 Private key envelope (deterministic)

Plaintext `{"kty":"RSA","test":"rustket"}`, userId `UserIdRustket0001`.

- v2 write: passphrase `Rustket-E2EE-Passphrase-#2026-very-long`, UUID `00000000-0000-4000-8000-000000000001`,
  IV `000102030405060708090a0b` →
  `{"iv":"AAECAwQFBgcICQoL","ciphertext":"vYyaUw2oCZd6wrL7t5nLq2SFVWUeo0H/mUfquMt7gkVLIyt0qisdf/pWrCbf8w==","salt":"v2:UserIdRustket0001:00000000-0000-4000-8000-000000000001","iterations":100000}`
- v1 read: passphrase `legacy-pass`, IV `07`×16 (salt = userId, 1000 iterations, AES-256-CBC) →
  `{"$binary":"BwcHBwcHBwcHBwcHBwcHB+YPOUcoV6V49mKRuFiH/WtWzRsivURxbCgtArMK4g72"}`

### 13.4 Messages

Room key A256GCM `k = 9o1xoHt4OamRJvnaLna-5akUb5L98S_iWYGGaXPZ1Yg`:

- IV `a0a1a2a3a4a5a6a7a8a9aaab` (`oKGio6Slpqeoqaqr`), plaintext
  `{"msg":"Привет, Rustket! 🔐 @alice"}` (UTF-8) → ciphertext
  `qFgwu7iEzf4femB1smKZ16vs4ZGy52gaBRZ3KsolzKz+ChmhHkIm1RtTLrqJwTY8ySJoDZQIRzA+KWQP`

Room key A128CBC `k = qb8In0Rpa9nwSusvxxDcbQ`:

- v1 write/read: IV `00112233445566778899aabbccddeeff`, plaintext `{"msg":"legacy v1"}` →
  `32c9e7917b78ABEiM0RVZneImaq7zN3u/5uyUeK+TssJ4yOk/QrZdAkh1KxP799yTru4sAL5hSLB`
- v2 envelope over a CBC key (as the 8.9 web writes it): IV `42`×16, plaintext `{"msg":"v2 over cbc"}` →
  `{"algorithm":"rc.v2.aes-sha2","kid":"32c9e7917b78","iv":"QkJCQkJCQkJCQkJCQkJCQg==","ciphertext":"ovPPRuVf0amt9ULYGp+hxT1FRGzLv3u3mQH/q0lky7g="}`

Legacy kid: `sha256('{"alg":"A128CBC","ext":true,"k":"qb8In0Rpa9nwSusvxxDcbQ","key_ops":["encrypt","decrypt"],"kty":"oct"}')` hex prefix `32c9e7917b78`.

### 13.5 Wrapped room key (decrypt-only; OAEP is randomized)

Unwrap with the `pwdv2web.private_key` JWK from `keychain.spec.ts`:

```
f46d2864-0384-4a87-8815-51fba2cad216WDf45Ovvni38XEDTqhsnVknrvpMEb9KtezR5DuQ5+mBRLKZHWfE1BjeMUsNh/EbcayHsGVzwEjbiIA+ziIVKgKBjABRVn41g6PbgGNem+XpjJ9Jpl48xcqWtFffCrTBXdWZKao7+8iLAUHKo/6dSQ9cXESHDOeGkPmLdyAtwDqUQ8JnxQRVwKSpLnKJg0SrFLAVA7lKqfv7/5N1hySKoP7MOC6d2WQl0kyiBgySzfHeNHezAmnQFLgG5ZE9+cGwX5unTl/KBcFp3unKYh6CP0rnGav6Hdf0kvuTY8q3j2P+WOr045RfZeGlHGzMouUCprsIcvdu69lU4q9R0QB9YKA==
```

→ kid `f46d2864-0384-4a87-8815-51fba2cad216`, plaintext
`{"alg":"A256GCM","ext":true,"k":"9o1xoHt4OamRJvnaLna-5akUb5L98S_iWYGGaXPZ1Yg","key_ops":["encrypt","decrypt"],"kty":"oct"}`.
For the encrypt direction, test roundtrip (wrap with `pwdv2web.public_key`, unwrap with the private key)
and that the output is `kid` + exactly 344 base64 chars.

### 13.6 Files (AES-256-CTR, 64-bit counter)

- JWK `{"alg":"A256CTR","ext":true,"k":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","key_ops":["encrypt","decrypt"],"kty":"oct"}` (key bytes `00..1f`)
- IV `AAECAwQFBgf//////////g==` = `0001020304050607fffffffffffffffe` (low half wraps after the 2nd block)
- plaintext (52 bytes) `0123456789abcdef0123456789abcdef0123456789abcdeftail`, SHA-256
  `27ba3f32aa22e58169e21a5ccee9ef04a6a8aafb9e610897ebd1186c05f24fd4`
- ciphertext hex
  `232636a6270bb2add7899c6c17e5971a1241b249201d3c216446084c91cf37f42596457bd8510c2d120c730325276cc1ab3a7c56`
- A 128-bit counter yields identical first two blocks and a different third block — the test that
  catches `Ctr128BE`.

File names: `sha256("notes.txt") = e39538e7f27a7bf579cd9b85a103c0f0b86b60b788534295538d0301a9c5dce6`
(matches the live upload), `sha256(utf8("Отчёт.pdf")) = 2e2bc3fdfa1c462abcadcd3ca0ac2065699f7aa10076e3f9a71c485224c84e78`.

### 13.7 Live-captured error bodies [live]

```json
{"success":false,"error":"Keys already set [error-keys-already-set]","errorType":"error-keys-already-set","details":{"method":"e2e.setUserPublicAndPrivateKeys"}}
{"success":false,"error":"E2E Key ID already exists [error-room-e2e-key-already-exists]","errorType":"error-room-e2e-key-already-exists","details":{"method":"e2e.setRoomKeyID"}}
{"success":false,"error":"No suggested key available [error-no-suggested-key-available]","errorType":"error-no-suggested-key-available","details":{"method":"e2e.acceptSuggestedGroupKey"}}
{"success":false,"error":"Invalid room [error-invalid-room]","errorType":"error-invalid-room","details":{"method":"e2e.getUsersOfRoomWithoutKey"}}
{"success":false,"error":"Only groups or direct channels can enable encryption [error-action-not-allowed]","errorType":"error-action-not-allowed","details":{"method":"saveRoomSettings","action":"Change_Room_Encrypted"}}
{"success":false,"errorType":"error-invalid-params","error":"must have required property 'roomIds'"}
```

Capture the success payloads of §4/§5 as fixtures under `crates/rocketchat/tests/fixtures/e2e/`
when implementing (the live tests can recreate them; see §15).

---

## 14. Proposed Rust layout

### 14.1 Dependencies (`crates/rocketchat`)

Versions checked on crates.io on 2026-11-08; the symmetric crates match what `Cargo.lock` already
contains (`aes 0.9.4`, `cbc 0.2.1`, `cipher 0.5`, `hmac 0.13`, `sha2 0.11`, `zeroize 1.9`):

| Crate | Use |
|---|---|
| `aes = "0.9"`, `cbc = "0.2"` (PKCS#7), `ctr = "0.10"`, `aes-gcm = "0.11"` | AES-CBC/CTR/GCM |
| `pbkdf2 = "0.13"` (feature `hmac`) + `hmac = "0.13"` + `sha2 = "0.11"` | PBKDF2, SHA-256 |
| `rsa = { version = "0.9.10", features = ["sha2", "getrandom"] }` | RSA-OAEP-SHA256, keygen, JWK components. 0.9 is on the previous `digest 0.10`/`rand_core 0.6` generation: use `rsa::sha2::Sha256` for `Oaep::new` and `rsa::rand_core::OsRng` for its RNG (0.10 is still `rc`). Note RUSTSEC-2023-0071 (timing side channel in RSA decryption, no fixed 0.9 release); exposure is low because decryption runs locally on data fetched from the server, with no remote timing oracle. Keep RSA behind a small internal trait so it can move to `rsa 0.10` or `aws-lc-rs` (already in the tree via rustls) later. |
| `zeroize = "1.9"` | wipe keys/passphrases (`Zeroizing<…>`) |
| `uuid = { version = "1", features = ["v4"] }` | key ids, envelope salts |
| `base64` (already), `rand` (already), `serde_json` (already), `regex` (in lockfile) | encodings, CSPRNG, mentions |

All pure Rust (RustCrypto), no system libraries.

### 14.2 `crates/rocketchat` — protocol and crypto (no state, no persistence)

```
src/e2e/mod.rs        pub use …; E2eError (thiserror): Base64, Json, Format(&'static str),
                      UnsupportedAlgorithm(String), WrongPassword, Decrypt, Rsa, PasswordCharset
src/e2e/codec.rs      b64/b64url helpers; latin1 codec; WrappedKey { kid: String, blob: [u8; 256] }
                      with parse(&str)/to_string(); EncryptedContent enum:
                        V1 { kid: String, iv: Vec<u8>, ciphertext: Vec<u8> },
                        V2 { kid: String, iv: Vec<u8>, ciphertext: Vec<u8> },
                        Unsupported { algorithm: String }
                      EncryptedContent::from_message(content: Option<&Value>, msg: &str) -> Option<Self>,
                      to_json(&self) -> Value   (V2 only on write)
src/e2e/user_key.rs   UserKeyPair::generate() -> Result<UserKeyPair>   (blocking)
                      PublicJwk / PrivateJwk serde structs (exact field names, ext: true, key_ops)
                      UserPublicKey::from_jwk_str(&str); encrypt_oaep(&self, &[u8]) -> [u8; 256]
                      UserPrivateKey::from_jwk_str(&str); decrypt_oaep(&self, &[u8; 256]) -> Zeroizing<Vec<u8>>
                      to_jwk_string(&self) -> Zeroizing<String>
src/e2e/keychain.rs   PrivateKeyEnvelope::parse(stored: &str, user_id: &str) -> Result<Self>  (V1/V2)
                      decrypt(&self, password: &str) -> Result<Zeroizing<String>>   (Latin-1 then UTF-8)
                      encrypt_v2(private_jwk: &str, password: &str, user_id: &str) -> Result<String>
                      validate_new_passphrase(&str) -> Result<()>   (ASCII + web policy flags)
src/e2e/passphrase.rs BIP39_ENGLISH: [&str; 2048] (include_str of english.txt); generate() -> Zeroizing<String>
src/e2e/room_key.rs   RoomKeyAlg { A256Gcm, A128Cbc }; RoomKey { kid: String, alg, key: Zeroizing<Vec<u8>> }
                      RoomKey::generate() (A256GCM + UUID); from_jwk_str(kid, &str); to_jwk_string()
                      wrap_for(&self, &UserPublicKey) -> String;  unwrap(&UserPrivateKey, &str) -> Result<RoomKey>
                      encrypt_json(&self, &Value) -> EncryptedContent   (V2, random IV of the right size)
                      decrypt_json(&self, &EncryptedContent) -> Result<Value>
src/e2e/payload.rs    DecryptedMessage { msg: Option<String>, attachments: Option<Vec<MessageAttachment>>,
                      files: Option<Vec<FileProp>>, file: Option<FileProp> } (legacy `text` → msg, whitelist)
                      FileMetadata { type_, type_group, name, encryption, hashes, alt_text }
src/e2e/file.rs       FileKey { key: Zeroizing<[u8; 32]>, iv: [u8; 16] }; AttachmentEncryption { key: OctJwk, iv: String }
                      FileKey::generate(); from_attachment(&AttachmentEncryption)
                      encrypt_stream / decrypt_stream adapters over Stream<Item = Result<Bytes>> (Ctr64BE)
                      Sha256 streaming hasher -> lowercase hex; hashed_upload_name(&str) -> String
src/e2e/mentions.rs   e2e_mentions(text, username_pattern) -> E2eMentions
src/rest/e2e.rs       impl RestClient:
                        fetch_my_keys() -> MyKeys { public_key: Option<String>, private_key: Option<String> }
                        set_user_keys(public_key, private_key, force: bool)
                        request_subscription_keys()
                        set_room_key_id(rid, key_id) -> Result<SetKeyIdOutcome { Set, AlreadyExists }>
                        update_group_key(rid, uid, key)
                        users_of_room_without_key(rid) -> Vec<UserPublicKeyEntry>
                        accept_suggested_group_key(rid) / reject_suggested_group_key(rid)
                        users_waiting_for_group_key(&[rid]) -> HashMap<String, Vec<UserPublicKeyEntry>>
                        provide_users_suggested_group_keys(&HashMap<String, Vec<SuggestedKey>>)
                        reset_room_key(rid, e2e_key, e2e_key_id)
                        reset_own_e2e_key(two_factor: Option<TwoFactorCode>)
                        set_room_encrypted(rid, bool)        (rooms.saveRoomSettings)
                        send_encrypted_message(rid, id, tmid, tshow, content, mentions)
                        update_encrypted_message(rid, id, content, mentions)
                      files.rs: upload_media gains `content: Option<String>` (extra multipart text part);
                      confirm_media gains an options struct with `t`, `content`, `file_content`.
models                Subscription: old_room_keys: Option<Vec<OldRoomKey>> (E2EKey/E2ESuggestedKey exist)
                      Room: users_waiting_for_e2e_keys: Option<Vec<WaitingUser { user_id, ts: RcDate }>>
                      Message: e2e_mentions: Option<E2eMentions>; content typed via EncryptedContent::from_message
                      MessageAttachment: encryption/hashes typed (keep tolerant: fall back to Value)
                      FileRecord (rooms.files): content
session               NOTIFY_ROOM_EVENTS += "e2e.keyRequest" → a new variant, e.g.
                      SessionEvent::RoomE2EKeyRequest { rid, key_id: Option<String> }. Note the
                      existing SessionEvent::E2EKeyRequest { rid, key_id } (crates/rocketchat/src/
                      session/events.rs) is the user-stream `{uid}/e2ekeyRequest`, which 8.9 never
                      emits (§6); do not reuse that name for the room event.
```

**As implemented (2026-11-08)** — deviations from the sketch above, all to stay source-compatible
with `crates/rustket` (which matches `SessionEvent` exhaustively and calls the existing upload API):

- REST methods are named `RestClient::e2e_*` (`e2e_fetch_my_keys`, `e2e_set_user_keys`,
  `e2e_request_subscription_keys`, `e2e_set_room_key_id`, `e2e_update_group_key`,
  `e2e_users_of_room_without_key`, `e2e_accept_suggested_group_key`,
  `e2e_reject_suggested_group_key`, `e2e_users_waiting_for_group_key`,
  `e2e_provide_users_suggested_group_keys`, `e2e_reset_room_key`, `e2e_reset_own_key`,
  `e2e_set_room_encrypted`, `e2e_send_message`, `e2e_update_message`).
- Uploads: `upload_media`/`confirm_media` keep their signatures; new
  `upload_media_with_fields` (extra multipart text parts), `e2e_upload_media` (hashed name,
  `application/octet-stream`, `content` part) and `e2e_confirm_media(EncryptedMediaConfirm)`
  (`t: 'e2e'`, `msg: ''`, `content`, `fileContent`, optional `tmid`; never `description`/`fileName`).
- Models: `Message.content`, `MessageAttachment.encryption`/`hashes` stay `Value`; typed access
  via `Message::encrypted_content()`, `MessageAttachment::e2e_encryption()`/`e2e_sha256()`.
  Added `MessageAttachment.content` (the encrypted content inside `message_pinned_e2e`
  attachments, otherwise dropped by the tolerant parser), `Subscription.suggested_old_room_keys`.
  There is no `rooms.files` model yet: `E2eManager::decrypt_file_metadata(rid, &content)` takes the
  raw `content` value.
- `{rid}/e2e.keyRequest` is subscribed by `watch_room` but no `SessionEvent` variant was added
  (it would break exhaustive matches downstream): it arrives as `SessionEvent::Unknown` and is
  decoded with `rocketchat::e2e::RoomE2eKeyRequest::from_event`.
- Core: the decrypt cache is keyed by `(rid, message id, _updatedAt)`; instead of
  `Decrypted{rid, ids}` the manager emits `E2eEvent::KeysChanged { rid, pending }` (ids that came
  back `Pending` before) and the UI re-runs `decrypt_message`. The REST calls the manager needs are
  behind the `E2eApi` trait (implemented by `RestClient`) for the state machine tests.

All functions that do RSA keygen/decrypt or PBKDF2 are synchronous and documented as blocking.

### 14.3 `crates/rustket-core` — state and orchestration

```
src/e2e/mod.rs        E2eManager (one per workspace/account):
                        state: AccountE2eState { Disabled, DisabledReadOnly, LoadingKeys,
                                                 EnterPassword { failed: bool }, SavePassword, Ready,
                                                 Error(String) }
                        stop()/logout() cancel in-flight start/unlock/handshakes (results dropped,
                        secrets written after a logout deleted again)
                        start(session, settings) / stop()
                        submit_password(String) -> Result<(), WrongPassword>
                        confirm_passphrase_saved(); pending_passphrase() -> Option<Zeroizing<String>>
                        change_passphrase(new) ; reset_own_keys()
                        room_state(rid) -> RoomE2eState
                        encrypt_outgoing(rid, text, attachments) -> Result<OutgoingEncrypted>
                        decrypt_message(&Message) -> DecryptResult { Decrypted(DecryptedMessage), Pending, Failed(Reason) }
                        decrypt_file_metadata(&FileRecord); toggle_room(rid, bool); reset_room_key(rid)
                        prepare_upload_async / decrypt_download_async (whole-file CTR + SHA-256 on a
                        blocking thread; the sync variants block the caller)
                        events: watch/broadcast channel of E2eEvent { AccountState, RoomState{rid}, Decrypted{rid, ids} }
src/e2e/store.rs      SecretStore keys: rustket-e2e:<server_id>:<user_id>:private_key / :pending_passphrase
src/e2e/room.rs       RoomCrypto { state, current: Option<RoomKey>, old: Vec<(kid, Option<RoomKey>)> }
                      handshake / on_subscription / on_room / accept-or-reject; one tokio Mutex per room
src/e2e/distribution.rs   10 s loop (§7.4), started on Ready, stopped on stop()
src/e2e/cache.rs      decrypted content keyed by (message id, _updatedAt); invalidated on key arrival/edit
```

Integration points: `sync.rs`/`timeline.rs` call `decrypt_message` for `t == 'e2e'` and render
`Pending` with the placeholder; the room list uses the decrypted `lastMessage`; `notify.rs`
decrypts notification payloads before showing them (fallback text `Encrypted message`);
uploads go through `encrypt_outgoing` + `file.rs` streaming encryption.

### 14.4 `crates/rustket` — UI

Banner + modals (enter passphrase, save passphrase with copy, reset confirmation), Account →
Security E2E section, room body replacement states, header key icon, room menu toggle with
enable/disable/reset dialogs, composer hint and send blocking, disabled message actions and slash
commands, decrypt-on-download for attachments with hash verification. Strings in en + ru.

---

## 15. Testing plan

- **Unit (no network)**: every vector in §13; PrefixedBase64 errors (length < 344, decoded ≠ 256);
  v1 parse errors; JWK emitted by Rustket must contain `ext: true`, correct `key_ops`, `alg`, no
  padding; Rustket-generated RSA keys imported by an independent implementation (e.g. a test-only
  vector made with WebCrypto: wrap with Rustket's public key in Node, unwrap in Rust — store the
  produced fixture); envelope roundtrip; passphrase generator distribution sanity and word-list hash
  (`sha256` of the 2048 words joined with `\n`, no trailing newline =
  `187db04a869dd9bc7be80d21a86497d692c0db6abd3aa8cb6be5d618ff757fae`); CTR wrap vector; decrypt
  whitelist ignores `_id`/`u`/`rid`.
- **Live, works with `E2E_Enable = false`** (what this research ran; each test creates its own users
  via admin `users.create` and a room via `groups.create {extraData:{encrypted:true}}`, then deletes
  them): set/fetch keys (+ `error-keys-already-set`, `force`), `setRoomKeyID` race,
  `updateGroupKey` self/other, `getUsersOfRoomWithoutKey`, accept/reject (+ queue entry),
  `chat.sendMessage`/`chat.update` with v2 content and cross-user decryption, encrypted upload →
  download → decrypt → hash, pin, `resetRoomKey` effects, `requestSubscriptionKeys` +
  `{rid}/e2e.keyRequest`, `users.resetE2EKey` → 401. Two traps: fake wrapped keys must be unique
  per subscription (unique `E2EKey` index, §4), and `users.resetE2EKey` only succeeds without
  `x-2fa-*` headers within `Accounts_TwoFactorAuthentication_RememberFor` (1800 s) of the user's
  creation — send the password fallback (`x-2fa-method: password`, SHA-256 hex) to be robust.
  `resetRoomKey` tests must not retry within 10 s after an access-denied call (lock leak, §5.5).
- **Live, needs `E2E_Enable = true`** (`fetchUsersWaitingForGroupKey`, `provideUsersSuggestedGroupKeys`,
  the distribution loop, `rooms.saveRoomSettings {encrypted}`, server rejection of plaintext):
  implemented as `crates/rustket-core/tests/e2e_live.rs::e2e_full_flow_with_e2e_enabled`, which runs
  only with `RUSTKET_TEST_E2E=1` (plus `RUSTKET_TEST_SERVER`). It is the one test allowed to flip the
  global setting: it reads `E2E_Enable`, sets it with `POST settings/E2E_Enable` as admin
  (`x-2fa-method: password`, `x-2fa-code: sha256(adminpass)`), and restores the previous value in
  its cleanup and again from a drop guard. Run once on 2026-11-08 (~25 s with E2E on): queue
  distribution to a late joiner, plaintext rejected (`error-not-allowed`), encrypted upload, room
  key reset with old keys, all passed; `E2E_Enable` was `false` afterwards.
- **WebCrypto interop** (done once, fixture `crates/rocketchat/tests/fixtures/e2e/webcrypto_interop.json`):
  Node 24 WebCrypto imported a Rustket-generated RSA key pair with the browser import rules
  (`extractable`, `RSA-OAEP`/`SHA-256`, `key_ops`), unwrapped a Rustket-wrapped room key, and wrapped
  a room key, encrypted a message and a v2 key backup that Rustket decrypts.
- **Interop (manual, once)**: with E2E enabled on a disposable server, exchange messages and files
  between Rustket and the 8.9 web client in both directions, including a room key reset and a
  passphrase change on each side.

---

## 16. Open questions

1. Whether to migrate v1 envelopes to v2 automatically after a successful unlock (better KDF; no
   client on 8.9 lacks v2 support). Recommendation: no silent rewrite; offer it via "change
   passphrase" like the web.
2. Whether Rustket should publish anything for key requests. On 8.9 the only effective mechanism is
   the queue; publishing the dead `e2ekeyRequest` adds nothing.
3. UNVERIFIED: RN's native AES-CTR counter width (64 vs 128 bits) is not visible in the sources
   (`@rocket.chat/mobile-crypto` is native code); irrelevant in practice (§3.8).
