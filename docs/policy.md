# Enterprise policy

Engineering specification of the policy layer: how an administrator's settings reach Rustket on
each OS, how they are validated and merged, and how the app enforces them. `docs/policies.md`
and `packaging/policy/` are generated from the key registry (section 14); `docs/enterprise.md`
is the hand-written administrator guide.

Status: set A, implemented. All sections are normative.

## 1. Principles

- One policy model, three OS-native channels. Group Policy, Intune and every other MDM write into
  the same place on a given OS, so there is exactly one reader per OS.
- Policy is configuration management, not a security boundary. A local administrator can change the
  sources and a user can run another build, or the macOS executable outside its bundle. Enforcement
  that must hold belongs to the Rocket.Chat server and the identity provider; the docs say so.
- Mandatory values only. There is no "recommended" tier: a key is either enforced or absent.
- A key that is not set changes nothing.
- No secrets in policy: every channel is readable by local users.
- A forced preference (a `UiPrefs` field, section 7) is never written to `config.toml`; removing
  the policy restores the user's own choice. Managed servers (section 9.1) are added to
  `config.toml` as ordinary entries and stay there, and `LaunchAtLogin` (section 9.6) leaves the
  registration as it is.
- No new `unsafe`, no new dependencies outside the lock file.

## 2. Channels and permanent identifiers

These identifiers are a public contract and never change.

| OS | Source | Identifier |
|---|---|---|
| Windows | Registry, machine then user | `HKLM\Software\Policies\Rustket`, `HKCU\Software\Policies\Rustket` |
| macOS | Managed preferences (forced values only) | preference domain `io.github.sokolcloud.rustket` (`rustket::BUNDLE_ID`) |
| Linux | Root-owned JSON files | `/etc/rustket/policies/managed/*.json` |

Key names are PascalCase and identical on all three OSes.

## 3. Keys (set A)

Classes: **R** restrictive (limits the user), **T** trust-expanding (adds something the user will
trust), **C** cosmetic. "Strict" is the value used when the highest-priority source sets the key
to something invalid (section 6).

| Key | Type | Class | Meaning when set | Strict |
|---|---|---|---|---|
| `ManagedServers` | string list | T | Servers present for every user; the user cannot remove them. Item: `URL` or `URL<whitespace>Display name` | ignored |
| `AllowUserServers` | bool | R | `false`: the user cannot add or remove servers | `false` |
| `AllowedServers` | string list | R | Only matching servers may be used (section 9.1). Managed servers are always allowed by this list (`RequireHttps` still applies). An empty list allows managed servers only | empty list |
| `RequireHttps` | bool | R | `true`: `http://` servers are refused | `true` |
| `AllowCertificateExceptions` | bool | R | `false`: an untrusted certificate cannot be accepted and stored exceptions are ignored | `false` |
| `UpdateMode` | enum `prompt`, `notify`, `off` | R | `prompt`: check on schedule, install when the user clicks (today's behaviour). `notify`: report a new version, never install. `off`: no check, no network request | `off` |
| `AllowPasswordLogin` | bool | R | `false`: the password form is hidden (on a server with `LDAP_Enable` it is also the LDAP form) | `false` |
| `AllowTokenLogin` | bool | R | `false`: the personal-access-token login is hidden | `false` |
| `AllowSecretsFile` | bool | R | `false`: secrets are never written to a plain file when the keyring is unusable | `false` |
| `NotificationPreview` | bool | R | Forces whether notifications show the message text | `false` |
| `HardwareAcceleration` | bool | C | Forces the renderer choice (`false`: CPU renderer) | ignored |
| `LaunchAtLogin` | bool | C | Forces the launch-at-login registration | ignored |
| `OrganizationName` | string | C | Shown in "Managed by …"; one line, at most 80 characters | ignored |
| `SupportUrl` | string | T | Help link for users; must be an absolute `https://` URL | ignored |
| `MinimumVersion` | string | R | `MAJOR.MINOR.PATCH`; an older client does not connect and shows "update required" | ignored |

`MinimumVersion` is the one restrictive key whose invalid value is ignored: a typo must not lock
out every client.

Applicability: `UpdateMode` applies only to builds with the `self-update` feature;
`HardwareAcceleration`, `UpdateMode` and `AllowSecretsFile` do not apply to the Mac App Store
flavor (it has no renderer switch and no updater, and never writes the secrets file). A key that
does not apply is reported as such and otherwise ignored.

Details of single keys:

- `ManagedServers`: the URL must name its scheme (`http://` or `https://`). Two items with the
  same normalised URL are one server; the first wins. In the display name, control, line-breaking
  and bidirectional-override characters become spaces and runs of whitespace collapse to one
  space; the name is then trimmed and capped at 80 characters.
- `OrganizationName`: a value that is longer than 80 characters or contains such characters is
  invalid.
- `SupportUrl` is kept in its normalised form.
- `MinimumVersion`: three non-negative integers, no suffix.

## 4. Raw values and coercion

Readers produce raw values; the core coerces them to the key's type.

```
RawValue = Bool(bool) | Integer(i64) | String(String) | List(Vec<RawValue>) | Unsupported(kind)
```

`Unsupported` carries a native value the reader cannot map (a JSON float, object or null, binary
registry data, any other preference type). It is always invalid, so the key's strict value
applies instead of the key being silently skipped.

| Key type | Accepted raw values |
|---|---|
| bool | `Bool`; `Integer` 0 or 1 |
| enum | `String` equal to a variant, ASCII case-insensitive |
| string | `String` (trimmed; empty is invalid) |
| string list | `List` of `String`; a single `String` is a one-item list |

Anything else is invalid. In a string list, an item that is not a string or is empty after
trimming is dropped and reported; the key stays applied with the remaining items. Lists keep
their order and drop exact duplicates.

How each source maps its native types:

- **Windows.** Values directly under the key: `REG_DWORD`/`REG_QWORD` → `Integer`,
  `REG_SZ`/`REG_EXPAND_SZ` → `String` (no variable expansion), `REG_MULTI_SZ` → `List` (empty
  strings dropped). A subkey named like a policy key is a list: its `REG_SZ` and `REG_EXPAND_SZ`
  values (no variable expansion), ordered by value name (numeric names numerically, then the
  others in byte order). This is the shape an ADMX `list` element writes. A value of another
  type in that subkey is an unsupported item, dropped and reported; subkeys below it are
  ignored. When both a value and a subkey of the same name exist, the subkey wins.
  A list subkey without values is treated as absent: an ADMX list policy that is switched off
  leaves such a key, and the administrator then means "no restriction". Such an empty subkey
  does not hide a value of the same name: the value applies. An explicitly empty `REG_MULTI_SZ`
  value is an empty list; only a script or a .reg file writes one. With the ADMX template, the
  effect of an empty `AllowedServers` (managed servers only) is reached by listing the managed
  servers in it, and the admin documentation says so.
  Registry names are case-insensitive: a name that equals a policy key ignoring ASCII case is
  read as that key and reported under the key's own spelling. Other value types are
  `Unsupported`.
- **macOS.** A number with an integral value → `Integer` (a plist boolean arrives as 1 or 0 and
  coerces to bool); any other number is `Unsupported`. `NSString` → `String`; `NSArray` → `List`.
  Only keys that are forced in the app's own preference domain are read; a value that is present
  but not forced is ignored (it can come from `defaults write` or a launch argument). Forced keys
  of that domain with other names are reported as unknown, except names that are also forced in
  the global domain (another profile's system settings). Other types are `Unsupported`. Key
  names are matched exactly. A binary that runs outside the app bundle sees no managed
  preferences.
- **Linux.** JSON object per file; JSON booleans, integers, strings and arrays map directly.
  Floats, objects, nulls and integers outside `i64` are `Unsupported`. A name that appears twice
  at the top level of one file is `Unsupported` for that key (the file stays readable). Key names
  are matched exactly.

## 5. Sources, trust and precedence

Each reader returns one snapshot per source: its id, its trust level, its raw values, and an
error if the source exists but could not be read.

| Source | Trust | Priority |
|---|---|---|
| Windows machine (`HKLM`) | full | highest |
| Windows user (`HKCU`) | limited | below machine |
| macOS forced preferences | full | single |
| Linux file | full, or limited when the ownership check fails | full trust above limited; within a trust level the later file name wins |

- A **limited** source may not set class T keys: such a value is reported as ignored.
- For every key the value comes from the highest-priority source that sets it. Lower sources are
  reported as overridden. Lists are replaced, never merged.
- Linux files: regular files named `*.json` (not starting with `.`) directly in the directory.
  A file with full trust overrides a file with limited trust; within a trust level a later file
  name (byte order) overrides an earlier one, per key. Symbolic links are followed and the checks
  apply to the opened file. A name that is not a regular file (a directory, a socket, a FIFO) or
  that vanishes between listing and opening is skipped. A file is trusted when it is a
  regular file owned by uid 0 and not writable by group or others, and the directory is owned by
  uid 0 and not writable by group or others. Otherwise the file is read with limited trust and
  flagged. Size limit: 1 MiB per file. A missing directory means "no policy".
- The Linux directory is a compile-time constant in release code. No environment variable or
  command-line flag can move or disable any source. Tests pass the directory and the expected
  owner as parameters.

## 6. Resolution

Input: the snapshots and a context (client version, OS, flavor: `mas`, `self_update`). Output: an
immutable `Policy`.

For each key, in this order:

1. Unknown name → reported as unknown, not applied.
2. Not applicable to this OS or flavor → reported, not applied.
3. From a limited source and class T → reported as ignored, the next source is considered.
4. Valid after coercion → applied.
5. Invalid → the key's strict value is applied and the row is reported as invalid; where the
   table says "ignored", nothing is applied and lower sources are **not** consulted (their rows
   are reported as overridden).

Blocking states. A `Policy` can carry reasons that stop the app from connecting:

- **Source unreadable**: a source exists but cannot be read or parsed (a JSON syntax error, a
  top-level value that is not an object, a file over the size limit, a registry key that exists
  but denies access). The content is unknown, so no restriction can be assumed away.
- **Below minimum version**: `MinimumVersion` is valid and greater than the client version.
  The client version is compared in semver order: a pre-release such as 0.6.0-rc.1 is older
  than 0.6.0; build metadata is ignored.

The app shows a blocking screen for these (section 9.7). Keys from the readable sources are still
resolved and reported. Errors and trust problems are typed (a kind the UI can translate, plus the
operating system's or parser's own text as an untranslated detail).

Report. `Policy` keeps one row per (source, key name) seen, in a stable order, for the diagnostics
page and `--dump-policy`: key name as written, source, status (applied, overridden, invalid with
the reason and whether a strict value was applied, unknown, not applicable, ignored because the
source is limited), and a display form of the value (long values truncated). It also lists every
source with its trust level and error.

## 7. Preferences overlay

Some keys act on fields of `UiPrefs`:

| Key | Field | Forced value |
|---|---|---|
| `NotificationPreview` | `notification_preview` | the key's value |
| `HardwareAcceleration` | `hardware_acceleration` | the key's value |
| `AllowSecretsFile` = `false` | `secrets_memory_only` | `true` |
| `UpdateMode` (any value) | `check_updates` | `mode != off` |
| `UpdateMode` (any value) | `update_prereleases` | `false` |

`AllowSecretsFile = true` forces nothing.

The core offers three pure functions on a `Policy`:

- `apply(user: &UiPrefs) -> UiPrefs`: the effective preferences.
- `unapply(effective: &UiPrefs, user: &UiPrefs) -> UiPrefs`: `effective` with every forced field
  replaced by `user`'s value. This is the user's view after code wrote into the effective copy.
- `locks(field) -> bool`: whether a field is forced.

The app keeps `config.ui` as the **effective** copy, so no reading code changes, and a second
`UiPrefs` with the user's own values. After every `update` it reconciles: when `config.ui`
differs from the last effective snapshot, the user copy becomes `unapply(config.ui, user)` and
`config.ui` becomes `apply(user)`. `unapply` always uses the policy that produced the effective
copy: when the policy itself is replaced, the user copy is recovered with the old policy and the
new one is applied to it. A write to a free field is therefore kept and saved; a write to
a forced field is undone. Saving always writes the user's view, never a forced value. Handlers
that have side effects before the reconcile (the secrets switch) check `locks` first.

When the policy is replaced, every lockable field whose effective value changes gets the side
effects its handler has: the secret store follows `secrets_memory_only` (also when the store
opens later, or after a switch still running), an open memory-only confirmation closes when the
field becomes forced, an offered pre-release is withdrawn when `update_prereleases` turns off.
`HardwareAcceleration` changes the displayed preference at once and the renderer at the next
start. A store move that fails is reported and not retried: the store keeps its backend until the
next start, which opens it under the forced value. A failed move that the policy started (not the
user) never changes or saves the user's own preference.

## 8. Startup and refresh

- The policy is resolved at the top of `main`, after the command line is parsed and before the
  software-renderer restart, so before logging exists. Resolution is cheap, has no side effects
  and keeps its own error list. When the renderer restart happens `main` runs twice.
- `ICED_BACKEND` keeps priority over `HardwareAcceleration`, as it has over the preference today.
- `App` receives the resolved policy through `Flags`; tests inject one the same way. Its policy
  setter returns the follow-up task of the side effects of section 7.
- Refresh: one timer for all sources re-reads them in the background and replaces the policy
  when it changed. Two kinds of change are applied only when the next poll shows them again,
  because a Group Policy refresh deletes and rewrites its keys and a file can be caught
  half-written; at start-up there is no such delay:
  - a source that had values and has none now. A source counts as emptied only when it was read
    successfully; one with an unreadable part is never emptied, because its content is unknown.
    On Linux this concerns the directory as a whole: all files together, and only when every
    file of the listed directory was read. A missing Windows key is a readable, empty source.
  - a source that was readable and is unreadable now. On Linux the directory counts as readable
    at a poll that listed it or found it missing, so a directory that cannot be listed later is
    held too.
- Any other change is applied at once, including one that removes protection: a single key or
  list subkey that disappears, or whose value weakens, while its source keeps other values (for
  example a Group Policy refresh caught between deleting and rewriting a list subkey) is lifted
  until the next poll restores it, at most five minutes later. This per-key gap is accepted for
  set A; the admin guide tells script authors to change values in place.
- A held change that the next poll does not show again is dropped. Changes of different sources
  are confirmed independently, with one exception: when the Linux directory is held as a whole
  (it emptied, it cannot be listed, or a file in it became unreadable with a trust different
  from its last readable state), all changes in it are accepted together once every one of them
  was held at the previous poll. A readable file whose trust changes is an ordinary change,
  applied at once. A new Linux file has no readable state before it: when it cannot be read it
  is applied at once, also while the rest of the directory is held.
- The timer runs every five minutes, only in a real desktop run, and never starts a read while
  one is in flight. Each key has a refresh class: live, next start (`HardwareAcceleration`), or
  next sign-in.
- The Windows reader reads a key until two consecutive reads agree (at most four reads), because
  the registry can be rewritten while it is enumerated; reads that never agree make the source
  unreadable with the error kind "unstable" (no detail text).

## 9. Enforcement

### 9.1 Servers

- `ManagedServers` entries are reconciled with `config.servers` at start and on refresh by
  normalised URL: a missing one is added as an ordinary entry, an existing one is used as it is.
  Whether a server is managed is never stored: it is asked of the policy each time. A managed
  server cannot be removed by the user. An entry is added with the display name of its policy
  item, or with the host when the item has none; an entry whose name is still the host takes the
  server's site name when the server first reports it. An entry that already exists keeps its
  name, and a later change of the policy item's name does not rename it. Its certificate must be
  trusted by the OS: stored pins of a managed server are never used, whatever
  `AllowCertificateExceptions` says, and apply again once it leaves `ManagedServers`.
  A server that leaves the policy stays in the user's list as an ordinary server.
- A server is **allowed** when it is managed, or `AllowedServers` is not set, or it matches a
  pattern. Pattern: `[scheme://]host[:port][/path-prefix]`. `host` is compared case-insensitively
  after URL normalisation and may start with `*.` (any subdomain, not the bare domain). No scheme
  matches both; no port means the scheme's default port; no path matches any path. A pattern that
  does not parse is dropped and reported.
- `RequireHttps = true` makes every `http://` server not allowed, managed or not.
- Checks happen at every entry: the add-server screen before it contacts the host,
  `rocketchat://` links before the offer, and the servers loaded from `config.toml`.
- A configured server that is not allowed is **suspended**: listed, marked "suspended by
  policy", never connected, its sign-in and its data kept. Selecting it shows why, with the
  organization name and support link, and "Remove workspace" while the user may remove it (it
  is not managed and `AllowUserServers` is not `false`). It becomes active again when the policy
  allows it. When a refresh suspends a server that is running, its connection is closed without
  signing out, and nothing new is sent to it or fetched from it (queued messages, images); a
  download already running may finish, while uploads still running are cancelled and named at
  once in a warning (not later as interrupted by quitting). At start, when the last
  active server is suspended and another one is not, the other one is shown.
- `AllowUserServers = false` hides the add and remove actions and refuses links to servers that
  are not already configured. Servers already present stay usable if they are allowed.
- When no server is configured and the user may not add one, the app shows a plain screen that
  says so (with the organization name, the support link and the diagnostics of section 10)
  instead of the add-server screen.

### 9.2 Certificates

`AllowCertificateExceptions = false`: no accept action on the certificate screen, and stored pins
are ignored where the TLS configuration is built, so a pin already in `config.toml` has no effect.
The pins in effect of a server are decided whenever a session of that server is about to start (at
start-up, after a sign-in, when a suspended server is allowed again, at "Retry" after a failed
start or an ended session) and for its login screen. A session that is running keeps the TLS
configuration it started with. So when a refresh takes the pins of a signed-in server out of effect
(the key turns off, or the server becomes managed, section 9.1), nothing of that server is
restarted: the accept action disappears at once, and the session stops using the pin when it next
starts, at the latest when Rustket is started again. The app says so once, in one notice that names
every such server; a server that waits for "Retry" (a failed start, an ended session) gets no
notice, because "Retry" applies the rule. A server without stored pins is not affected.

### 9.3 Updater

The updater has one **effective mode**, computed in one place:

- `UpdateMode` when the key is set;
- else `notify` for these install kinds, and only these: an Inno Setup installation for all users
  on Windows, and the install kinds reserved for the MSI and the macOS installer package;
- else `prompt`, today's behaviour, for every other copy. Deb and rpm installs stay `prompt` as
  today, although their install asks for an administrator password; an administrator who wants
  otherwise sets `UpdateMode`.

Until the install kind is detected (right after start), checks follow the key, or `prompt` without
it; nothing installs, and the blocking screen (section 9.7) offers no update unless the key is
`prompt`.

`prompt`: check at start and on the timer, offer the update, install when the user clicks.
`notify`: the same checks, but the app never downloads or installs: it reports the new version
and offers the release page, as a copy without an installer does today. `off`: no check of any
kind, no network request to the update feed, no manual check; the updater card says that the
organization manages updates. The mode gates every entry of the updater: the start-up check, the
timer, the manual check and the install; an install already running when the mode changes is not
interrupted. The Mac App Store flavor has no updater and never shows the key.

When a refresh or the detected install kind takes the mode out of `prompt`, a download in
progress is cancelled. In `notify` what was on offer (an offered release, a downloaded file, a
failed install) becomes the report of its release; in `off` it goes, together with a check under
way and its status. A failure that an earlier run left behind, known only by its version, is
dropped outside `prompt`; the next check reports its release. A running install that ends
afterwards shows its outcome, except that a cancelled administrator password in `off` offers
nothing. The change itself requests nothing: a mode that allows checks again checks at the next
tick or start.

### 9.4 Sign-in

`AllowPasswordLogin = false` and `AllowTokenLogin = false` hide the form and the token login and
refuse the matching messages. The form is also the LDAP sign-in when the server has `LDAP_Enable`,
so `AllowPasswordLogin = false` hides LDAP too; the server's login page in the browser still
offers it. They shape the client only: the server still accepts those methods, and "sign in with
the browser" opens the server's own login page. The client cannot tell a personal access token
from the token a browser sign-in returns, so a `rocketchat://auth` token link pasted or opened
during a browser sign-in still signs in under `AllowTokenLogin = false`; to forbid personal access
tokens, remove the permission `create-personal-access-tokens` on the server.

### 9.5 Secrets

`AllowSecretsFile = false` forces `secrets_memory_only` (section 7) at start and at runtime.

### 9.6 Desktop

`LaunchAtLogin` is OS state, not a preference: at start and when a refresh changes the key, the
app brings the user's launch-at-login registration to the forced value (one attempt each time,
plus one when a click on the switch that was still running ends against it) and locks the
switch. The registration is set once the terms are accepted and no blocking state (section 6) is
in force; a start or a change during a block is applied when the block ends. A change the user
makes outside Rustket stays until the next start or change of the key; that includes the startup
switches of Windows Task Manager and Settings (the `StartupApproved` mark) and GNOME's Startup
Applications (`X-GNOME-Autostart-enabled=false`), which count as off. On the macOS dmg build, a
switch-off under System Settings → Login Items ("Allow in the Background") cannot be undone by
Rustket: the user or the MDM (a managed login-items payload) must allow it. The key cannot act
before the first launch, and on the Mac App Store flavor enabling can stay "needs approval".
Removing the policy leaves the registration as it is.

### 9.7 Blocking screen, organization and support

A blocking state replaces the whole window, like the terms screen: no sessions, notifications or
links. A block that starts at runtime brings a window hidden to the tray back, since the tray
falls silent. It names the reason (policy cannot be read, or an update is required), shows
`OrganizationName` and `SupportUrl` when set, and for an update offers the updater if
the effective update mode (section 9.3) is `prompt`. Settings cannot be opened then, so the
screen offers the diagnostics of section 10 itself: the blocking states are always shown, a
"Details" action shows the policy sources with their trust and errors as on the Policies page,
and there are "Copy report" (the full report of section 6) and the `--dump-policy` hint. When
a block that was in force at start ends, the server to show is chosen again by the start rule
of section 9.1.

Locked settings show "Managed by your organization" (or the organization name) as visible text,
not only as a tooltip.

## 10. Diagnostics

Settings → Policies lists the report of section 6 and the blocking states. `--dump-policy` prints
the same as JSON before any GUI work and takes an output file, because Windows release builds have
no console. The tab exists only when there is something to show. The Mac App Store flavor leaves
the rows of keys that do not apply to it (section 3) off the page, and shows neither the tab nor
the notice when nothing else remains; `--dump-policy` keeps those rows. The hint on the page
names the running executable's own path, quoted when it needs it; on Windows a quoted path uses
single quotes with PowerShell's call operator (`& '…\rustket.exe' --dump-policy policy.json`).

## 11. Build flavors

The registry is the same in every build; applicability is data (section 3). The Mac App Store
build fails if its binary contains `api.github.com`, `install-kind`, `SHA256SUMS` or
`didDismissAlert`. Locale files are embedded in every flavor, so policy names, descriptions and
UI strings must not contain those substrings; a unit test checks the registry and the locale
file.

## 12. Code layout and tests

- `crates/rustket-core/src/policy/`: registry, raw values and coercion, resolution and report,
  preferences overlay, server entries and patterns, the JSON directory source. Pure data and
  `std`; no OS access beyond reading the policy directory (`std::fs`, `libc` open flags).
- `crates/rustket/src/policy/`: start-up loading, the Windows registry reader, the macOS managed
  preferences reader, refresh, server standing (`servers.rs`) and the Policies page model
  (`page.rs`).
- Readers decode behind small traits so the decoding is tested on Linux with fakes; fixtures live
  in `crates/<crate>/tests/fixtures/policy/`.
- Platform code is type-checked with `dev/cross-check.sh` for all four foreign targets, with and
  without `--features rustket/mas`.

## 13. Lifecycle of keys

Keys are only added. A key's type and meaning never change; a replacement gets a new name and
the old one stays as an alias. A snapshot test fails when a shipped key is removed or retyped.
Every key records the version that introduced it. Older clients ignore keys they do not know,
which is why `MinimumVersion` exists.

Whoever adds a user preference decides at the same time whether an administrator must be able to
force it; if so, the key, its texts and the regenerated templates are part of the same change,
and the template revision of section 14 goes up.

## 14. Templates for administrators

Everything an administrator loads into a management tool is generated from the key registry plus
one table of admin-facing texts (a display name and an explanation per key, in English and
Russian), by code that is not part of the app binary (an example that writes the files, and a
test that compares them with the checked-in copies under `packaging/policy/`). A key without
texts fails the test. The texts are for administrators, not UI strings: they are not in
`locales/`.

| File | For | Notes |
|---|---|---|
| `rustket.admx`, `en-US/rustket.adml`, `ru-RU/rustket.adml` | Group Policy, Intune import, other Windows MDMs | see below |
| `rustket.mobileconfig` | any macOS MDM (also a Terraform-managed Jamf) | one `com.apple.ManagedClient.preferences` payload for the domain `io.github.sokolcloud.rustket` with `Forced` settings; example values; fixed example UUIDs to replace |
| `rustket-intune-macos.plist` | Intune "Preference file" | keys and values only, without the plist wrapper, as Intune wants it |
| `rustket-jamf-schema.json` | Jamf Pro "Application & Custom Settings" | a JSON Schema with title and description per key |
| `policy.schema.json`, `50-example.json` | Linux | JSON Schema of a policy file, and an example that sets every key: adapt it (keep only the keys to enforce) before placing it in `/etc/rustket/policies/managed/` |
| `docs/policies.md` | the reference | one section per key: type, values, what "not set" means, where it applies, since which version, when a change takes effect |
| `scripts/rustket-policy-check.ps1`, `scripts/rustket-policy-check.sh` | inventory and compliance checks in management tools | hand-written, not generated (`packaging/policy-scripts/`); they read the `--dump-policy` report, format 1, so a change of its fields or status codes changes them too |

ADMX rules, fixed because Intune's import is strict and a published template cannot be changed in
place:

- Target namespace `Rustket.Policies.R<n>` with prefix `rustket`; `<n>` is the template revision
  and goes up only when keys are added. No `using` element: the template defines its own
  `supportedOn` and its own category "Rustket".
- Registry key `Software\Policies\Rustket` for every policy. Class `Both` (computer and user
  configuration), except class T keys (`ManagedServers`, `SupportUrl`), which are `Machine` only
  because a user-scope value of them is ignored (section 5).
- bool: `enabledValue` decimal 1, `disabledValue` decimal 0, value name = key name. The display
  name is the key's question in positive form (Enabled = `true`); every explanation says what
  Enabled and Disabled do. For the `Allow*` keys and `NotificationPreview` Disabled is the
  restriction, for `RequireHttps` Enabled is.
- enum: an `enum` element (a drop-down list). No `comboBox` element anywhere.
- string: a `text` element.
- string list: a `list` element with `valuePrefix=""` whose key is
  `Software\Policies\Rustket\<KeyName>` (numbered `REG_SZ` values, the shape section 4 reads).
- Applicability to an OS or flavor is said in the explanation text, not by leaving a key out:
  every key is in every template.
- `en-US` is complete and is the fallback; `ru-RU` has the same strings in Russian.

The templates ship in the release as one archive, and the deb and rpm packages own
`/etc/rustket/policies/managed/` (empty) and install `policy.schema.json` and `50-example.json`
with the documentation.
