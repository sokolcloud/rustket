# Rustket policy reference

> Generated in the source repository (https://github.com/sokolcloud/rustket) from the key registry (`crates/rustket-core/src/policy/registry.rs`) and the admin texts (`crates/rustket-core/tests/admin_templates/texts.rs`), like the templates in `packaging/policy/` there. Do not edit: run `cargo run -p rustket-core --example policy_templates -- .` from the root of that repository.

Every key below can be set by an administrator; a key that is not set changes nothing, and a forced setting is never written to the user's configuration (`LaunchAtLogin` and `ManagedServers` say what stays when the policy is removed). Policy is configuration management, not a security boundary: enforcement that must hold belongs to the Rocket.Chat server and the identity provider. The engineering specification is https://github.com/sokolcloud/rustket/blob/main/docs/policy.md.

## Where policy is read

| System | Source | Templates (`packaging/policy/` in the repository, next to this file in the release archive) |
|---|---|---|
| Windows | registry `HKLM\Software\Policies\Rustket` (computer), then `HKCU\Software\Policies\Rustket` (user) | `rustket.admx`, `en-US/rustket.adml`, `ru-RU/rustket.adml` (namespace `Rustket.Policies.R1`) |
| macOS | forced managed preferences of the domain `io.github.sokolcloud.rustket` | `rustket.mobileconfig`, `rustket-intune-macos.plist`, `rustket-jamf-schema.json` |
| Linux | root-owned JSON files `/etc/rustket/policies/managed/*.json` | `policy.schema.json`, `50-example.json` |

Key names are identical on every system and matched exactly (the Windows registry ignores case). An invalid value of a restrictive key applies its strict value instead, so a mistake does not lift the restriction; `MinimumVersion` is the exception (an invalid value is ignored). The explanations use the Group Policy terms: Enabled is `true`, Disabled is `false` (for a key with a value, Disabled removes the value), and Not configured means that the key is absent.

## Keys

| Key | Type | Since | Takes effect |
|---|---|---|---|
| [`ManagedServers`](#managedservers) | string list | 0.4.0 | live |
| [`AllowUserServers`](#allowuserservers) | bool | 0.4.0 | live |
| [`AllowedServers`](#allowedservers) | string list | 0.4.0 | live |
| [`RequireHttps`](#requirehttps) | bool | 0.4.0 | live |
| [`AllowCertificateExceptions`](#allowcertificateexceptions) | bool | 0.4.0 | see the key |
| [`UpdateMode`](#updatemode) | enum(prompt, notify, off) | 0.4.0 | live |
| [`AllowPasswordLogin`](#allowpasswordlogin) | bool | 0.4.0 | next sign-in |
| [`AllowTokenLogin`](#allowtokenlogin) | bool | 0.4.0 | next sign-in |
| [`AllowSecretsFile`](#allowsecretsfile) | bool | 0.4.0 | live |
| [`NotificationPreview`](#notificationpreview) | bool | 0.4.0 | live |
| [`HardwareAcceleration`](#hardwareacceleration) | bool | 0.4.0 | next start |
| [`LaunchAtLogin`](#launchatlogin) | bool | 0.4.0 | see the key |
| [`OrganizationName`](#organizationname) | string | 0.4.0 | live |
| [`SupportUrl`](#supporturl) | string | 0.4.0 | live |
| [`MinimumVersion`](#minimumversion) | string | 0.4.0 | live |

## ManagedServers

**Managed servers**

Servers added to every user's server list; users cannot remove them. One item per server: its URL with the scheme, optionally followed by a space and a display name (https://chat.example.com Example Chat). "Allowed servers" never blocks a managed server, but "Require HTTPS for servers" still refuses an http:// one. The certificate of a managed server must be trusted by the system: accepted certificate exceptions are not used for it. Disabled or not configured: no server is added. A server taken off the list stays as an ordinary server. Applies on all systems. A change applies while Rustket runs, within a few minutes.

Group Policy template: Only the computer setting counts (HKLM): a user-scope value is ignored.

| | |
|---|---|
| Type | string list |
| Values | a list of non-empty strings (a single string is a one-item list) |
| Not set | No server is added. |
| Invalid value | ignored; lower-priority sources are not consulted |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | While Rustket runs, within a few minutes (the policy is read every five minutes; a source that empties or becomes unreadable is confirmed by one more read first). |
| Windows | `Software\Policies\Rustket`: subkey `ManagedServers` with `REG_SZ` values named 1, 2, 3, ... (a `REG_MULTI_SZ` value `ManagedServers` also works), under `HKLM` only (a value under `HKCU` is ignored); ADMX: computer configuration |
| macOS | array of strings; a forced value applies whether its profile is for the device or the user |
| Linux | JSON array of strings; ignored in a file that fails the ownership check |
| Example | `["https://chat.example.com Example Chat"]` |

## AllowUserServers

**Allow users to add and remove servers**

Enabled or not configured: users can add and remove servers. Disabled: the add and remove actions are hidden and rocketchat:// links to servers that are not configured yet are refused; servers already in the list stay usable while they are allowed. When no server is configured and users may not add one, Rustket shows a screen that says so, with the organization name and the support link. Managed servers can never be removed. Applies on all systems. A change applies while Rustket runs, within a few minutes.

| | |
|---|---|
| Type | bool |
| Values | true or false (the integers 1 and 0 are accepted) |
| Not set | Users can add and remove servers. |
| Invalid value | treated as `false` |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | While Rustket runs, within a few minutes (the policy is read every five minutes; a source that empties or becomes unreadable is confirmed by one more read first). |
| Windows | `Software\Policies\Rustket`: value `AllowUserServers`, `REG_DWORD` 1 or 0, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | boolean |
| Linux | JSON boolean |
| Example | `false` |

## AllowedServers

**Allowed servers**

Limits the servers Rustket may use. One pattern per item: \[scheme://\]host\[:port\]\[/path-prefix\]. The host ignores case; \*. at its start means any subdomain (not the domain itself). No scheme: http and https; no port: the default port; no path: any path. Unreadable patterns are skipped. Managed servers always pass this list, but "Require HTTPS for servers" still applies. Other servers are suspended (kept, not connected). Disabled or not configured: every server is allowed. An empty list allows managed servers only. Applies on all systems. A change applies while Rustket runs, within a few minutes.

Group Policy template: In this template an empty list counts as not configured: list the managed servers instead.

| | |
|---|---|
| Type | string list |
| Values | a list of non-empty strings (a single string is a one-item list) |
| Not set | Every server is allowed. |
| Invalid value | treated as an empty list (managed servers only) |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | While Rustket runs, within a few minutes (the policy is read every five minutes; a source that empties or becomes unreadable is confirmed by one more read first). |
| Windows | `Software\Policies\Rustket`: subkey `AllowedServers` with `REG_SZ` values named 1, 2, 3, ... (a `REG_MULTI_SZ` value `AllowedServers` also works), under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | array of strings |
| Linux | JSON array of strings |
| Example | `["https://chat.example.com","*.example.com"]` |

## RequireHttps

**Require HTTPS for servers**

Enabled: servers with an http:// address are refused, managed or not, and configured ones are suspended (kept, never connected). Disabled or not configured: http:// servers may be used. Applies on all systems. A change applies while Rustket runs, within a few minutes.

| | |
|---|---|
| Type | bool |
| Values | true or false (the integers 1 and 0 are accepted) |
| Not set | http:// servers may be used. |
| Invalid value | treated as `true` |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | While Rustket runs, within a few minutes (the policy is read every five minutes; a source that empties or becomes unreadable is confirmed by one more read first). |
| Windows | `Software\Policies\Rustket`: value `RequireHttps`, `REG_DWORD` 1 or 0, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | boolean |
| Linux | JSON boolean |
| Example | `true` |

## AllowCertificateExceptions

**Allow accepting untrusted certificates**

Enabled or not configured: when a server presents a certificate the system does not trust, the user may accept it and Rustket keeps the exception. Disabled: there is no accept action and stored exceptions are ignored. Once Rustket reads the change (within a few minutes) the accept action disappears; a signed-in server keeps its connection settings until its session next starts (after a sign-in or Retry, at the latest at the next start of Rustket), and Rustket shows one notice about it. Managed servers never use stored exceptions. Applies on all systems.

| | |
|---|---|
| Type | bool |
| Values | true or false (the integers 1 and 0 are accepted) |
| Not set | Users may accept an untrusted certificate; stored exceptions are used. |
| Invalid value | treated as `false` |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | The accept action follows within a few minutes; a signed-in server stops using a stored exception when its session next starts, at the latest at the next start of Rustket (specification section 9.2, https://github.com/sokolcloud/rustket/blob/main/docs/policy.md). |
| Windows | `Software\Policies\Rustket`: value `AllowCertificateExceptions`, `REG_DWORD` 1 or 0, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | boolean |
| Linux | JSON boolean |
| Example | `false` |

## UpdateMode

**Update mode**

How the built-in updater behaves. prompt: check on schedule, install when the user clicks. notify: report new versions with a link to the release page, never download or install. off: no checks and no request to the update feed; the app says that the organization manages updates. Disabled or not configured: prompt, except notify for a Windows installation for all users and for future MSI and macOS installer packages. Only builds with the built-in updater read it; the Mac App Store version and builds without the updater ignore it. A change applies while Rustket runs, within a few minutes; leaving prompt cancels a download in progress.

| | |
|---|---|
| Type | enum(prompt, notify, off) |
| Values | one of `prompt`, `notify`, `off` (case does not matter) |
| `prompt` | prompt: check and install when the user agrees |
| `notify` | notify: only report new versions |
| `off` | off: no update checks |
| Not set | prompt, except notify for a Windows installation for all users and for future MSI and macOS installer packages. |
| Invalid value | treated as `off` |
| Applies to | builds with the built-in updater only (the release packages); not the Mac App Store version |
| Since | 0.4.0 |
| Takes effect | While Rustket runs, within a few minutes (the policy is read every five minutes; a source that empties or becomes unreadable is confirmed by one more read first). |
| Windows | `Software\Policies\Rustket`: value `UpdateMode`, `REG_SZ`, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | string |
| Linux | JSON string |
| Example | `"notify"` |

## AllowPasswordLogin

**Allow sign-in with a password**

Enabled or not configured: the sign-in screen offers the user name and password form. Disabled: the form is hidden, also when the server uses it for LDAP sign-in. This shapes the client only: the server still accepts passwords, and signing in with the browser opens the server's own login page. Applies on all systems. A change applies at the next sign-in; signed-in sessions are not affected.

| | |
|---|---|
| Type | bool |
| Values | true or false (the integers 1 and 0 are accepted) |
| Not set | The password form is offered. |
| Invalid value | treated as `false` |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | At the next sign-in; signed-in sessions are not affected. |
| Windows | `Software\Policies\Rustket`: value `AllowPasswordLogin`, `REG_DWORD` 1 or 0, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | boolean |
| Linux | JSON boolean |
| Example | `true` |

## AllowTokenLogin

**Allow sign-in with a personal access token**

Enabled or not configured: the sign-in screen offers signing in with a personal access token. Disabled: that option is hidden. This shapes the client only: the server still accepts tokens, and a token link pasted or opened during a browser sign-in still signs in, because Rustket cannot tell a personal access token from the token a browser sign-in returns; to forbid personal access tokens, remove the permission create-personal-access-tokens on the Rocket.Chat server (see the deployment guide). Applies on all systems. A change applies at the next sign-in; signed-in sessions are not affected.

| | |
|---|---|
| Type | bool |
| Values | true or false (the integers 1 and 0 are accepted) |
| Not set | The personal access token sign-in is offered. |
| Invalid value | treated as `false` |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | At the next sign-in; signed-in sessions are not affected. |
| Windows | `Software\Policies\Rustket`: value `AllowTokenLogin`, `REG_DWORD` 1 or 0, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | boolean |
| Linux | JSON boolean |
| Example | `false` |

## AllowSecretsFile

**Allow storing sign-in secrets in a file**

Enabled or not configured: when no system keyring is usable, Rustket may keep sign-in secrets in a plain file, and users may choose memory-only storage in Settings. Disabled: secrets are never written to a file; without a keyring they are kept in memory only, and after each start users sign in again and enter their E2E password again. The Mac App Store version ignores it: it never writes such a file. A change applies while Rustket runs, within a few minutes.

| | |
|---|---|
| Type | bool |
| Values | true or false (the integers 1 and 0 are accepted) |
| Not set | The user's own setting; without a keyring secrets may go to a file. |
| Invalid value | treated as `false` |
| Applies to | Windows, macOS and Linux; not the Mac App Store version |
| Since | 0.4.0 |
| Takes effect | While Rustket runs, within a few minutes (the policy is read every five minutes; a source that empties or becomes unreadable is confirmed by one more read first). |
| Windows | `Software\Policies\Rustket`: value `AllowSecretsFile`, `REG_DWORD` 1 or 0, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | boolean |
| Linux | JSON boolean |
| Example | `false` |

## NotificationPreview

**Show message text in notifications**

Enabled: notifications show the message text. Disabled: notifications do not show the message text. Not configured: the user's own setting decides. Applies on all systems. A change applies while Rustket runs, within a few minutes.

| | |
|---|---|
| Type | bool |
| Values | true or false (the integers 1 and 0 are accepted) |
| Not set | The user's own setting. |
| Invalid value | treated as `false` |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | While Rustket runs, within a few minutes (the policy is read every five minutes; a source that empties or becomes unreadable is confirmed by one more read first). |
| Windows | `Software\Policies\Rustket`: value `NotificationPreview`, `REG_DWORD` 1 or 0, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | boolean |
| Linux | JSON boolean |
| Example | `false` |

## HardwareAcceleration

**Use hardware acceleration**

Enabled: Rustket draws its window with the graphics processor. Disabled: it uses the software (CPU) renderer, which helps with faulty graphics drivers and remote sessions. Not configured: the user's own setting decides. The ICED\_BACKEND environment variable still takes priority. The Mac App Store version ignores it. Settings show a change at once; it takes effect at the next start of Rustket.

| | |
|---|---|
| Type | bool |
| Values | true or false (the integers 1 and 0 are accepted) |
| Not set | The user's own setting. |
| Invalid value | ignored; lower-priority sources are not consulted |
| Applies to | Windows, macOS and Linux; not the Mac App Store version |
| Since | 0.4.0 |
| Takes effect | At the next start of Rustket; Settings show the forced value at once. |
| Windows | `Software\Policies\Rustket`: value `HardwareAcceleration`, `REG_DWORD` 1 or 0, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | boolean |
| Linux | JSON boolean |
| Example | `true` |

## LaunchAtLogin

**Start Rustket when the user signs in**

Enabled: Rustket registers itself to start when the user signs in to the computer. Disabled: it removes that registration. Rustket applies the value when it starts and when this setting changes, once the terms are accepted and no blocking screen is shown, so it cannot act before its first start; a change the user makes outside Rustket (also in Task Manager or GNOME) stays until then. On a Mac, Rustket cannot undo a switch-off under Login Items ("Allow in the Background"). Not configured: the user decides; removing the policy leaves the registration as it is. On the Mac App Store version, macOS may ask the user to approve the registration in System Settings. Applies on all systems.

| | |
|---|---|
| Type | bool |
| Values | true or false (the integers 1 and 0 are accepted) |
| Not set | The user decides; removing the policy leaves the registration as it is. |
| Invalid value | ignored; lower-priority sources are not consulted |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | At the start of Rustket and when the key changes, once the terms are accepted and no blocking state is in force (specification section 9.6, https://github.com/sokolcloud/rustket/blob/main/docs/policy.md). |
| Windows | `Software\Policies\Rustket`: value `LaunchAtLogin`, `REG_DWORD` 1 or 0, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | boolean |
| Linux | JSON boolean |
| Example | `true` |

## OrganizationName

**Organization name**

The name shown as "Managed by ..." on locked settings, on the blocking screen and for suspended servers. One line of at most 80 characters; a longer value, or one with control, line-break or text-direction characters, is ignored. Disabled or not configured: Rustket says "your organization". Applies on all systems. A change applies while Rustket runs, within a few minutes.

| | |
|---|---|
| Type | string |
| Values | one line of at most 80 characters (trimmed) |
| Not set | Rustket says "your organization". |
| Invalid value | ignored; lower-priority sources are not consulted |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | While Rustket runs, within a few minutes (the policy is read every five minutes; a source that empties or becomes unreadable is confirmed by one more read first). |
| Windows | `Software\Policies\Rustket`: value `OrganizationName`, `REG_SZ`, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | string |
| Linux | JSON string |
| Example | `"Example Corp"` |

## SupportUrl

**Support link**

A help link for users, shown on the blocking screen, for suspended servers and when users may not add a server. It must be an absolute https:// URL; any other value is ignored. Disabled or not configured: no link is shown. Applies on all systems. A change applies while Rustket runs, within a few minutes.

Group Policy template: Only the computer setting counts (HKLM): a user-scope value is ignored.

| | |
|---|---|
| Type | string |
| Values | an absolute `https://` URL (trimmed) |
| Not set | No support link is shown. |
| Invalid value | ignored; lower-priority sources are not consulted |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | While Rustket runs, within a few minutes (the policy is read every five minutes; a source that empties or becomes unreadable is confirmed by one more read first). |
| Windows | `Software\Policies\Rustket`: value `SupportUrl`, `REG_SZ`, under `HKLM` only (a value under `HKCU` is ignored); ADMX: computer configuration |
| macOS | string; a forced value applies whether its profile is for the device or the user |
| Linux | JSON string; ignored in a file that fails the ownership check |
| Example | `"https://help.example.com/chat"` |

## MinimumVersion

**Minimum Rustket version**

The oldest Rustket version that may connect, as MAJOR.MINOR.PATCH (for example 0.4.0). An older client connects to no server and shows a screen saying that an update is required, with the updater where users may install updates (update mode prompt). The client version is compared in semver order: a pre-release such as 0.6.0-rc.1 is older than 0.6.0; build metadata is ignored. A value that is not three numbers is ignored, so a typo cannot lock out every client. Disabled or not configured: any version may connect. Versions before 0.4.0 do not know this setting. Applies on all systems. A change applies while Rustket runs, within a few minutes.

| | |
|---|---|
| Type | string |
| Values | `MAJOR.MINOR.PATCH`, three non-negative integers (trimmed) |
| Not set | Any version may connect. |
| Invalid value | ignored; lower-priority sources are not consulted |
| Applies to | Windows, macOS and Linux, every build |
| Since | 0.4.0 |
| Takes effect | While Rustket runs, within a few minutes (the policy is read every five minutes; a source that empties or becomes unreadable is confirmed by one more read first). |
| Windows | `Software\Policies\Rustket`: value `MinimumVersion`, `REG_SZ`, under `HKLM` or `HKCU` (`HKLM` wins); ADMX: computer or user configuration |
| macOS | string |
| Linux | JSON string |
| Example | `"0.4.0"` |
