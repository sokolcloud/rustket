# Managing Rustket in an organization

This guide is for administrators who deploy Rustket on managed computers and want to
enforce settings centrally. It tells you what to install, where each system reads the
policy, and how to check the result on a machine. The meaning of every key (values,
what "not set" means, when a change takes effect) is in the generated reference,
[docs/policies.md](policies.md); this guide does not repeat it.

Everything you load into a management tool ships in one archive per release,
`Rustket-<version>-policy-templates.zip`, on the
[releases page](https://github.com/sokolcloud/rustket/releases):

| File in the archive | For |
|---|---|
| `rustket.admx`, `en-US/rustket.adml`, `ru-RU/rustket.adml` | Group Policy, Intune "Import ADMX", other Windows MDMs |
| `rustket.mobileconfig` | any macOS MDM (also a Jamf Pro managed with Terraform) |
| `rustket-intune-macos.plist` | Intune "Preference file" profile (macOS) |
| `rustket-jamf-schema.json` | Jamf Pro "Application & Custom Settings" |
| `policy.schema.json`, `50-example.json` | Linux policy files |
| `policies.md` | the key reference |
| `scripts/rustket-policy-check.ps1`, `scripts/rustket-policy-check.sh` | inventory and compliance checks (section 6) |

The deb and rpm packages also install `policy.schema.json` and `50-example.json` in
`/usr/share/doc/rustket/policy/`.

## 1. What can be managed and what it means

**Do this first:** decide which keys you need from [the reference](policies.md), and
set only those. A key that is not set changes nothing; there is no "recommended"
level: a key is either enforced or absent.

There is one policy model for all systems: the same key names (PascalCase, for
example `AllowUserServers`) and the same meaning everywhere. Each system has exactly
one place where Rustket reads it, and every management tool of that system writes
into that place:

| System | Where Rustket reads policy | What delivers it |
|---|---|---|
| Windows | registry: `HKLM\Software\Policies\Rustket` (computer), then `HKCU\Software\Policies\Rustket` (user) | Group Policy with the ADMX template; Intune (imported ADMX, custom OMA-URI profile or a PowerShell script); other MDMs through the same Policy CSP; any script that writes the registry |
| macOS | managed preferences of the domain `io.github.sokolcloud.rustket`, forced values only | a configuration profile from any MDM: Jamf Pro, Intune, Kandji and others |
| Linux | root-owned JSON files in `/etc/rustket/policies/managed/` | configuration management (Ansible, Puppet, Salt and the like), packages, scripts; Samba Group Policy and Intune scripts as untested recipes (section 4) |

What a user sees:

- A setting forced by policy is locked in Settings and shows the visible text
  "Managed by your organization", or "Managed by" followed by the `OrganizationName`
  you set. A forced setting is never written to the user's configuration file:
  removing the policy restores the user's own choice. Two exceptions: removing
  `LaunchAtLogin` leaves the launch-at-login registration as it is, and a server
  taken off `ManagedServers` stays in the user's list as an ordinary server.
  `LaunchAtLogin` repairs a registration the user switched off in Windows Task
  Manager or GNOME's Startup Applications at the next start of Rustket; on a Mac
  (dmg build) a switch-off under System Settings → Login Items ("Allow in the
  Background") must be allowed again by the user or by a managed login-items
  payload.
- Settings get a **Policies** tab that lists every key that was found, where it
  came from and whether it applies (section 6).
- Servers that the policy does not allow are kept but suspended, and a policy that
  cannot be read stops Rustket from connecting (section 6).

The honest limits:

- **Policy is configuration management, not a security boundary.** A local
  administrator can change or delete the policy sources, and a user can run a copy
  of Rustket older than 0.4.0, a modified build of their own, or another client.
  Even an unmodified copy of 0.4.0 or later can be started outside the policy: on
  macOS the executable copied or linked out of the app bundle
  (`Rustket.app/Contents/MacOS/rustket`) reads no managed preferences, and on Linux
  a process started in a user namespace or container (`unshare`, `bwrap`, rootless
  Podman) can be shown its own `/etc/rustket/policies/managed`. Policy shapes the
  installed app as it is normally started. Enforcement that must hold belongs to
  the Rocket.Chat server (which sign-in methods it accepts, who may join which
  room, session lifetime) and to your identity provider.
- **No secrets in policy.** Every channel is readable by local users.
- **Rustket 0.4.0 and later only.** Older copies read no policy at all, and
  `MinimumVersion` cannot reach them, because they do not know that key either.
  Replace older copies first (section 6 has scripts that report the version).

## 2. Windows

### Install and uninstall silently

**Do this:** deploy the installer of the release (`Rustket-<version>-x64-setup.exe`,
or `Rustket-<version>-arm64-setup.exe` for ARM64 devices) with exactly this command
line, run as SYSTEM or as an administrator:

```bat
Rustket-0.4.0-x64-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /ALLUSERS /CLOSEAPPLICATIONS /LOG="C:\Windows\Temp\rustket-setup.log"
```

Why each switch matters (the installer is an Inno Setup installer, `packaging/windows/rustket.iss`):

- `/ALLUSERS`: the installer installs **for the current user by default** (it needs
  no administrator rights). Run as SYSTEM without this switch, it installs into
  SYSTEM's own profile, where no user ever sees it. With it, Rustket goes to
  `C:\Program Files\Rustket` for every user.
- `/SUPPRESSMSGBOXES`: on a clean machine the installer asks in a message box
  whether to install for all users or for the current user. A deployment as SYSTEM
  runs in a session no one can see, so without `/ALLUSERS` **and** this switch the
  installation waits on an invisible dialog until your management tool gives up.
  Pass both.
- `/VERYSILENT`: no wizard and no progress window. A silent installation does not
  start Rustket afterwards.
- `/NORESTART`: never restart the computer.
- `/SP-`: no "This will install..." question at the start.
- `/CLOSEAPPLICATIONS`: lets the installer close a running `rustket.exe` that it
  must replace (Rustket's own updater passes it too). Whether this works across
  sessions when the installer runs as SYSTEM is not yet verified (section 8); a
  computer startup script avoids the question, because no user is signed in.
- `/LOG="..."`: a log file to read when something fails.

The installer has two optional tasks: `urlhandler` (checked by default; it makes
Rustket open `rocketchat://` links, taking them over from the official Rocket.Chat
app) and `desktopicon` (unchecked). To change them, add `/MERGETASKS="!urlhandler"`
or `/MERGETASKS="desktopicon"`. Without `urlhandler`, sign-in through the browser
(OAuth, OpenID Connect, SAML, CAS) does not return to Rustket until the user
registers Rustket as the handler from the login screen, which takes the links over
for that user.

To uninstall, run the `UninstallString` from the uninstall key below (normally
`C:\Program Files\Rustket\unins000.exe`) with the same silence switches:

```bat
"C:\Program Files\Rustket\unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
```

A silent uninstallation does not ask to close a running Rustket, and it removes the
autostart entry, the notification registration and a `rocketchat://` registration
that Rustket wrote for a user only from the profile of the account that runs it
(SYSTEM's own, when your tool runs it as SYSTEM). In other users' profiles a
`rocketchat://` registration they accepted from the login screen stays behind and
points at the removed `rustket.exe`; as a per-user setting it takes precedence over
the official app's registration for all users. Remove
`HKCU\Software\Classes\rocketchat` in those profiles if you hand the links back.

### Detect the installation

- **Registry** (preferred): the key
  `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{77810904-1236-4291-B123-CCC8D0A6CCF1}_is1`
  in the **64-bit view**, value `DisplayVersion` (for example `0.4.0`), value
  `InstallLocation`. The key name is the installer's fixed AppId plus `_is1`. In
  Intune, leave "Associated with a 32-bit app on 64-bit clients" at **No**. A per-user
  installation writes the same key under `HKCU` instead.
- **Per-user copies.** Because the installer installs for the current user by
  default, users who installed Rustket themselves have their own copy in
  `%LOCALAPPDATA%\Programs\Rustket` (uninstall key under their `HKCU`), next to the
  copy for all users. Such a copy also reads the computer policy, but keeps the
  update mode `prompt` unless you set `UpdateMode`. Look for it in each profile if
  you want one copy per machine.
- **File**: `C:\Program Files\Rustket\rustket.exe`; its file and product version is
  the release version.
- **Script**: `scripts/rustket-policy-check.ps1` from the templates archive (section 6).

### Group Policy

**Do this:**

1. Copy `rustket.admx` into the Central Store
   (`\\<domain>\SYSVOL\<domain>\Policies\PolicyDefinitions\`), and `en-US\rustket.adml`
   and `ru-RU\rustket.adml` into the language folders of the same name there. On a
   single machine without a domain, use `C:\Windows\PolicyDefinitions\` instead.
   Always copy `en-US`: it is the fallback language.
2. In the Group Policy Management Editor the policies appear under
   **Computer Configuration → Policies → Administrative Templates → Rustket** and
   **User Configuration → Policies → Administrative Templates → Rustket** (in the
   Local Group Policy Editor, `gpedit.msc`, without the Policies level).
3. Set what you need. "Enabled" and "Disabled" are explained in each policy's help
   text; "Not configured" means the key is absent.

The facts:

- Prefer **Computer Configuration**. When both are set, the computer value wins (the
  `HKLM` source ranks above `HKCU`) and the user value is reported as overridden.
- Two policies exist only under Computer Configuration: `ManagedServers` and
  `SupportUrl`. They make Rustket trust something new (a server, a link), and a user
  value of them is ignored even when a script writes one.
- A **list policy** (`ManagedServers`, `AllowedServers`) that is switched off, or
  enabled without items, counts as **not configured**. For `AllowedServers` this
  matters: to allow only your managed servers, list the managed servers in it.
- Rustket reads the registry every five minutes, so a change arrives within a few
  minutes after Windows applies it (Group Policy refreshes in the background about
  every 90 minutes; `gpupdate /force` applies it at once). Section 6 has the details.

### Intune: imported ADMX

**Do this:** in the Intune admin center, go to **Devices → Configuration → Import
ADMX**, upload `rustket.admx` with `en-US/rustket.adml`, then create a profile of the
type **Imported Administrative templates (Preview)** and set the Rustket policies in
it.

The facts and limits (from Microsoft's documentation at the time of writing):

- The feature is still a **preview**. It takes the en-US ADML only, at most 1 MB per
  file and 20 imported ADMX files; it refuses templates with combo boxes, and a
  template that refers to another one needs that one imported first. Rustket's
  template has no combo box and refers to no other template.
- An imported ADMX **cannot be replaced**. When a later Rustket release raises the
  template revision (the namespace `Rustket.Policies.R1` becomes `R2` when keys are
  added), import the new file **next to** the old one, move your profiles to it, and
  delete the old profiles and file afterwards. Policy names never change between
  revisions.

### Intune: custom OMA-URI profile

**Do this** when the import is not an option: create a **Custom** configuration
profile (Windows 10 and later) with two kinds of rows. All rows have the data type
**String**.

1. Ingest the template once. OMA-URI:

   ```
   ./Device/Vendor/MSFT/Policy/ConfigOperations/ADMXInstall/Rustket/Policy/RustketR1
   ```

   Value: the complete content of `rustket.admx`. `Rustket` is the application name
   you choose here; `RustketR1` is a free identifier of the file.
2. Set each policy. The URI is built from that application name, the template's
   category (`Rustket`) and the policy name:

   ```
   ./Device/Vendor/MSFT/Policy/Config/Rustket~Policy~Rustket/RequireHttps
   ```

   Use `./User/...` instead of `./Device/...` for the user configuration (not for
   `ManagedServers` and `SupportUrl`, which are computer-only).

When a later release raises the template revision, the policy URIs stay the same
(they depend on the application name, the category and the policy name only). Do
not ingest the new file under a second identifier with the same application name,
which would define the same URIs twice: put the new `rustket.admx` into the value of
the existing `ADMXInstall` row. This is not yet verified (section 8).

Value formats:

| Kind | Example value |
|---|---|
| on/off policy (for example `RequireHttps`) | `<enabled/>` or `<disabled/>` |
| drop-down (`UpdateMode`) | `<enabled/><data id="UpdateMode" value="notify"/>` |
| text (for example `OrganizationName`) | `<enabled/><data id="OrganizationName" value="Example Corp"/>` |
| list (for example `AllowedServers`) | `<enabled/><data id="AllowedServers" value="1&#xF000;chat.example.com&#xF000;2&#xF000;*.example.org"/>` |

A list value alternates item numbers and items, separated by the character U+F000
(written `&#xF000;`). These formats follow Microsoft's documentation for
ADMX-backed policies; with Rustket's template they are not yet verified (section 8).

**Other MDMs** (Workspace ONE, Fleet and others) use the same Policy CSP: the same
two URIs and values.

### PowerShell fallback

**Do this** when your tool can only run scripts: write the registry values directly.
Run the script as SYSTEM in **64-bit** PowerShell (in Intune: "Run this script using
the logged on credentials" = No, and "Run script in 64 bit PowerShell Host" = Yes).
An Intune platform script runs once, so a value removed later is not written again;
unlike Group Policy, it does not keep enforcing. To rewrite values that drift, use a
Remediations script pair.

```powershell
# One on/off key and one list under the computer policy key.
$key = 'HKLM:\SOFTWARE\Policies\Rustket'
if (-not (Test-Path $key)) { New-Item -Path $key -Force | Out-Null }
New-ItemProperty -Path $key -Name RequireHttps -PropertyType DWord -Value 1 -Force | Out-Null

# A list is a subkey named like the key, with REG_SZ values named 1, 2, 3, ...
# Overwritten in place: deleting and recreating it would lift the list for a moment.
$list = Join-Path $key 'AllowedServers'
$items = @('chat.example.com', '*.example.org')
if (-not (Test-Path $list)) { New-Item -Path $list -Force | Out-Null }
for ($n = 1; $n -le $items.Count; $n++) {
    New-ItemProperty -Path $list -Name "$n" -PropertyType String -Value $items[$n - 1] -Force | Out-Null
}
# Items left over from a longer list.
(Get-Item $list).Property | Where-Object { $_ -match '^\d+$' -and [int]$_ -gt $items.Count } |
    ForEach-Object { Remove-ItemProperty -Path $list -Name $_ }
```

The formats per key (`REG_DWORD` 1 or 0 for on/off keys, `REG_SZ` for text and the
drop-down, a subkey of `REG_SZ` values or one `REG_MULTI_SZ` value for lists) are in
the reference. Change values in place rather than deleting and rewriting a key:
Rustket waits for a second read only when the whole key becomes empty, not when a
single value or list disappears for a moment (section 6, "How long a change takes").

### A domain without Intune

Deploy the installer with a **computer startup script** (Group Policy: Computer
Configuration → Policies → Windows Settings → Scripts → Startup). It runs as SYSTEM
before anyone signs in, so the installer can replace files freely. The share must be
readable by the computer accounts (Domain Computers).

```bat
@echo off
rem Installs exactly this version of Rustket for all users; does nothing when it is installed.
set VERSION=0.4.0
set SETUP=\\fileserver\software\Rustket\Rustket-%VERSION%-x64-setup.exe
set INSTALLED=
for /f "tokens=2,*" %%A in ('reg query "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{77810904-1236-4291-B123-CCC8D0A6CCF1}_is1" /v DisplayVersion /reg:64 2^>nul ^| find "DisplayVersion"') do set INSTALLED=%%B
if "%INSTALLED%"=="%VERSION%" exit /b 0
"%SETUP%" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /ALLUSERS /LOG="%WINDIR%\Temp\rustket-setup.log"
```

The script reads `DisplayVersion` from the 64-bit uninstall key of the installation
for all users into `INSTALLED` (empty when Rustket is not installed for all users) and
exits when that whole value equals `VERSION`; `0.4.1` therefore does not match an
installed `0.4.10`. Otherwise it runs the installer, so it installs the version in
`VERSION` over any other installed version, also a newer one: change only that line
when you roll out a release.

### Updates

An installation for all users **reports new versions and does not install them**:
without `UpdateMode`, its effective update mode is `notify`. Users see the new
version and a link to the release page; you roll out the new installer. Set
`UpdateMode` to `off` to stop all update checks (no network request to the update
feed), or to `prompt` to let users install updates themselves (for an all-users
installation that can ask them for administrator rights). A per-user installation keeps
`prompt` unless you set the key.

## 3. macOS

**Do this:** deploy a configuration profile that forces the keys you need in the
preference domain **`io.github.sokolcloud.rustket`**.

- **Only forced values count.** Rustket reads the managed preferences of its own
  domain and ignores a value that is present but not forced (set with
  `defaults write`, or as a "set once" default). Use the `Forced` form, as the
  example profile does.
- **The example profile** `rustket.mobileconfig` sets every key to an example value.
  Before you use it: replace both `PayloadUUID` values (for example with `uuidgen`)
  and both `PayloadIdentifier` values with your own, set `PayloadOrganization`, and
  **keep only the keys you mean to enforce**, with your values.

### Jamf Pro

- **Application & Custom Settings → External Applications → Custom Schema**: preference
  domain `io.github.sokolcloud.rustket`, schema `rustket-jamf-schema.json`. Jamf then
  shows a form with every key and its description; set only what you need.
- Or upload a plist with your keys (the content of `rustket-intune-macos.plist`,
  wrapped in `<plist><dict>...</dict></plist>`) as a custom settings payload of that
  domain.
- If your Jamf Pro configuration is kept in a Terraform-managed repository, keep the
  edited `rustket.mobileconfig` there and upload it as a profile.

### Intune

- **Devices → Configuration → Create → macOS → Templates → Preference file**: preference
  domain name `io.github.sokolcloud.rustket`, file: your edited copy of
  `rustket-intune-macos.plist` (keys and values only, without the plist wrapper, as
  Intune wants it). Intune applies preference files to the device.
- Or a **Custom** profile with your edited `rustket.mobileconfig`.

### Other MDMs

Kandji and every other MDM that accepts a custom profile take the edited
`rustket.mobileconfig`.

### Both builds

The disk image from the releases page and the Mac App Store build share the bundle
identifier and the preference domain, so one profile serves both. The Mac App Store
build ignores three keys, because it has no renderer switch, no updater and never
writes the secrets file: `HardwareAcceleration`, `UpdateMode` and `AllowSecretsFile`.
Its Policies page leaves them out; `--dump-policy` reports them as not applicable.

### Check on a Mac

- In Rustket: **Settings → Policies**.
- In Terminal, run the executable **inside the app bundle**; a copy outside the bundle
  sees no managed preferences, so no policy applies to it:

  ```sh
  /Applications/Rustket.app/Contents/MacOS/rustket --dump-policy > ~/Desktop/rustket-policy.json
  ```

  Standard output avoids a path that the Mac App Store build's sandbox may refuse;
  `--dump-policy` in the Mac App Store build is not yet verified (section 8). Run as
  root (as Jamf runs scripts), it may not see a profile that was
  installed for a user rather than for the device.

## 4. Linux

**Do this:** place one or more JSON files in **`/etc/rustket/policies/managed/`**,
owned by root, not writable by group or others, and readable by every user (0644,
in directories with 0755):

```sh
sudo install -d -o root -g root -m 755 /etc/rustket /etc/rustket/policies /etc/rustket/policies/managed
sudo install -o root -g root -m 644 50-corp.json /etc/rustket/policies/managed/50-corp.json
```

The facts:

- **Who must own what.** A file has full trust when it is a regular file owned by
  root (uid 0) and not writable by group or others, **and** the directory
  `/etc/rustket/policies/managed` is owned by root and not writable by group or
  others. Otherwise the file is still read, with **limited trust**, and flagged on the
  Policies page. A file with limited trust may not set the keys that make Rustket
  trust something new, `ManagedServers` and `SupportUrl`: those values are ignored.
- **Every user must be able to read it.** Rustket runs as the signed-in user. The
  file must be readable by everyone (0644, not 0600 or 0640), and `/etc/rustket`,
  `/etc/rustket/policies` and the managed directory must be readable and traversable
  by everyone (0755). A parent directory created implicitly under a restrictive
  root umask (077 or 027, for example by `mkdir -p` or by `install -d` given only
  the last path) is not: create each of them with its mode, as below. A
  file Rustket cannot open, or a directory it cannot list, blocks Rustket like a
  broken file. A check run as root (`--dump-policy` or the sample script from your
  management tool) does not show this; `sudo -u <user> rustket --dump-policy` does.
- **Which files.** Regular files named `*.json`, not starting with `.`, directly in
  the directory, at most 1 MiB each. Symbolic links are followed. A missing directory
  means "no policy".
- **How files merge.** Per key: a file with full trust overrides a file with limited
  trust, and within the same trust the later file name (byte order) wins, so
  `90-site.json` overrides `50-corp.json`. Lists are replaced, never merged.
- **A broken file blocks Rustket.** A file that is not valid JSON, whose top level is
  not an object, that is too large, or that the user cannot read (as can a
  directory the user cannot list) makes the policy unreadable: Rustket shows the
  blocking screen until the file is fixed (section 6). Validate before you deploy,
  and write files atomically (a temporary name starting with `.`, then rename).
- **The example** `50-example.json` sets **every** key to an example value. Copy it,
  keep only the keys you want, and set your values.
- **Validation.** `python3 -m json.tool 50-corp.json` checks the syntax;
  `policy.schema.json` (JSON Schema draft 2020-12) checks the types of the known
  keys with any schema validator, for example `check-jsonschema --schemafile
  policy.schema.json 50-corp.json`. It allows other names, so a misspelled key
  (`RequireHTTPS`) passes it and silently enforces nothing: Rustket only lists it as
  unknown on the Policies page. Check the names against the reference.
- **Packages.** The deb and rpm own the empty directory `/etc/rustket/policies/managed`
  (root, 0755), so you only add files. The AppImage reads the same directory, but it
  is neither installed nor updated by your fleet tooling: users can run any copy.

### Ansible

```yaml
- name: Rustket policy directories
  ansible.builtin.file:
    path: "{{ item }}"
    state: directory
    owner: root
    group: root
    mode: "0755"
  loop:
    - /etc/rustket
    - /etc/rustket/policies
    - /etc/rustket/policies/managed

- name: Rustket policy
  ansible.builtin.copy:
    src: files/rustket/50-corp.json
    dest: /etc/rustket/policies/managed/50-corp.json
    owner: root
    group: root
    mode: "0644"
    validate: python3 -m json.tool %s
```

### Samba Group Policy (untested recipe)

Samba can deliver a file to domain members as machine policy:

```sh
samba-tool gpo manage files add <GPO> ./50-corp.json /etc/rustket/policies/managed/50-corp.json root root 644 -H ldap://dc.example.com -U Administrator
```

Caveats: it needs Samba 4.15 or later, and `samba-gpupdate` must be enabled on the
clients (it is off by default: winbind needs `apply group policies = yes`, SSSD
needs `oddjob-gpupdate`). The target directory must already exist (the packages
create it). Check the argument order and mode format with
`samba-tool gpo manage files add --help` for your version. Clients refresh about
every 90 to 120 minutes, or at once with `samba-gpupdate --force`.

### Intune for Linux (untested recipe)

An Intune platform script for Linux (Devices → Scripts and remediations → Platform
scripts → Linux) with execution context **Root** can write the file:

```sh
#!/bin/sh
set -eu
dir=/etc/rustket/policies/managed
install -d -o root -g root -m 755 /etc/rustket /etc/rustket/policies "$dir"
tmp="$(mktemp "$dir/.rustket.XXXXXX")"
cat >"$tmp" <<'EOF'
{
  "AllowUserServers": false,
  "ManagedServers": ["https://chat.example.com Example Chat"]
}
EOF
chmod 644 "$tmp"
mv -f "$tmp" "$dir/50-intune.json"
```

Caveats: a script in root context needs an administrator to authenticate on the
device (Microsoft says the user might have to consent), so it is not silent; a
user-context script cannot write `/etc`. Keep secrets out of it.

## 5. Sign-in with an identity provider

**Do this:** enforce sign-in rules in your identity provider and on the Rocket.Chat
server, not in Rustket.

- Single sign-on (OAuth, OpenID Connect, SAML, CAS) happens in the **system browser**:
  Rustket opens the server's login page there and receives the result through a
  `rocketchat://` link. Conditional access, multi-factor authentication and device
  trust are therefore evaluated **in that browser, by your identity provider**
  (for example Microsoft Entra ID or Okta), with whatever device identity that
  browser presents. Rustket receives only the Rocket.Chat server's own login token.
- `AllowPasswordLogin` = Disabled hides the user name and password form (on a server
  with LDAP turned on this is also the LDAP sign-in; users can still sign in with the
  browser), and `AllowTokenLogin` = Disabled hides the personal access token sign-in.
  They shape the client only: the server still accepts those methods, and another
  client can use them. The server setting **Show Default Login Form**
  (Administration → Settings → Layout → Login) hides the password form in the
  server's own web client; neither it nor Rustket's key makes the server refuse a
  password sign-in. A token link (`rocketchat://auth?...&token=...`) pasted or
  opened during a browser sign-in still signs in, because Rustket cannot tell a
  personal access token from the token a browser sign-in returns. To forbid
  personal access tokens, remove the permission `create-personal-access-tokens`
  from the roles that have it (Administration → Permissions; by default `admin`
  and `user`). Tokens that already exist stay valid until they are removed: the
  server checks the token, not the permission, when it is used. The account's Personal Access Tokens page, where
  users remove their tokens, is shown only to users who hold the permission, so have
  users remove their tokens there **before** the permission is taken away, or remove
  them on the server side: `POST /api/v1/users.logout` with `{"userId": "<user id>"}`,
  called by an account with the permission `logout-other-user` (by default `admin`),
  removes all login tokens of that user, personal access tokens included, and signs
  the user out everywhere. This guide does not cover removing a single token of
  another user. A change applies at the next sign-in; signed-in sessions are not
  affected.
- To end a session, act on the Rocket.Chat server (log the user out, deactivate the
  account, shorten the session lifetime). Policy does not sign anyone out: even a
  server that the policy suspends keeps its sign-in in Rustket.

## 6. What users see and how to diagnose

**Do this:** to check one machine, open **Settings → Policies** in Rustket or run
`--dump-policy` as the signed-in user; for many machines, use the sample scripts at
the end of this section. No Policies tab means that Rustket found no key, source
problem or blocking state at all. Other builds list keys that do not apply to them as
"not applicable"; only the Mac App Store build leaves them off the page (section 3),
so there a policy that sets only such keys shows no tab. `--dump-policy` lists every
key that was read, applicable or not.

### In the app

Locked settings look as described in section 1.

- **The managed notice.** While the policy sets anything (a key is applied, or a
  restrictive key's strict value is in force), every Settings tab starts with "Some
  settings are managed by your organization", or "Some settings are managed by"
  followed by your `OrganizationName`. It has "Contact support", which opens your
  `SupportUrl`, when that key is set, and "Details", which opens the Policies tab.
- **Settings → Policies** lists every policy setting found, its value, its source and
  its status (applied, overridden, invalid, unknown key, not applicable to this
  build, ignored because the source has limited trust), every source with its trust
  and errors, and any blocking problem. "Copy report" copies the full report. The tab
  exists only when there is something to show.
- **Suspended workspaces.** A configured server that `AllowedServers` or `RequireHttps`
  does not allow stays in the list, marked "suspended by policy", and is never
  connected; its sign-in and data are kept. Selecting it says why, with your
  organization name and support link. It becomes active again when the policy allows
  it.
- **The blocking screen** replaces the whole window and stops all connections. It has
  two causes:
  - *The policy cannot be read*: a source exists but cannot be opened or parsed (a
    JSON syntax error, a file over 1 MiB, a registry key that denies access). Fix the
    source; the screen goes away at the next read.
  - *An update is required*: the version is older than `MinimumVersion` (in semver
    order: a pre-release such as 0.6.0-rc.1 is older than 0.6.0; build metadata is
    ignored). Install a newer Rustket or lower the key. The screen offers the updater only when the
    effective update mode is `prompt`.

  The screen shows your organization name and support link, a "Details" action with
  the sources and their errors, "Copy report" and the `--dump-policy` command.
- **Certificates.** With `AllowCertificateExceptions` = Disabled the accept action
  disappears at once; a signed-in server keeps its connection until its session next
  starts, at the latest at the next start of Rustket, and Rustket shows one notice
  naming such servers.
- **Updates.** With `UpdateMode` = `notify` users see new versions with a link to the
  release page; with `off` the updater says that the organization manages updates.

### The diagnostic log file

Rustket logs to standard output, which the Windows build (no console) and a macOS app
started from the Finder or the Dock do not keep. For a problem report, a user turns on
**Settings → Privacy & storage → Storage → Keep a diagnostic log file** and restarts Rustket;
**Show log folder** next to it opens the folder. The file is `rustket.log`; before it
would exceed 2 MiB it is renamed to `rustket.1.log` (replacing the previous one), so
at most about 4 MiB are kept. The folder is per user and not roaming:

| System | Folder |
|---|---|
| Windows | `%LOCALAPPDATA%\rustket\logs` |
| macOS, disk image | `~/Library/Logs/rustket` |
| macOS, Mac App Store | `~/Library/Containers/io.github.sokolcloud.rustket/Data/Library/Logs/rustket` |
| Linux | `$XDG_STATE_HOME/rustket/logs`, normally `~/.local/state/rustket/logs` |

The log never contains passwords, login or access tokens, encryption keys or message
text: it carries the lines of standard output from the `info` level up, with the
libraries that could log credentials held at a level where they do not, and its first
line names only the Rustket version, the system and the build. The file never takes
`debug` or `trace` lines, even when the `RUST_LOG` environment variable asks for them:
that detail, meant for developers, goes to standard output only. On Linux and macOS
the folder and the file are readable by the user only. The setting is the user's own,
not a policy key.

### `--dump-policy`

`rustket --dump-policy [FILE]` writes the same report as the Policies page as JSON,
to `FILE` or to standard output, and exits without opening a window. The Windows build
has no console, so give it a file there; from PowerShell:

```powershell
& 'C:\Program Files\Rustket\rustket.exe' --dump-policy "$env:TEMP\rustket-policy.json"
```

In a script use `Start-Process -Wait`, because PowerShell does not wait for a program
without a console. Run as SYSTEM or root, the report shows the computer's policy and
the policy of the account that runs it, not the signed-in user's `HKCU`. Copies older
than 0.4.0 do not know the option and open their window instead; the sample scripts
check the version first.

The JSON (format 1; fields are only ever added, a change of meaning raises `format`):

| Field | Meaning |
|---|---|
| `format` | version of this shape, currently `1` |
| `client_version` | the Rustket version that wrote the report |
| `blocked` | `true` when a blocking state is in force |
| `blocking` | the blocking states: `{"kind": "source_unreadable", "source": ..., "error": {...}}` or `{"kind": "below_minimum_version", "required": "1.0.0", "client": "0.4.0"}` |
| `sources` | every source read: `id` (`windows:HKLM`, `windows:HKCU`, `macos:managed`, `linux:<file name>`, `linux:directory`), `trust` (`full` or `limited`), `trust_problems` (why a Linux file is limited: `code` and English `text`), `error` |
| `error` (in `sources` and `blocking`, not at the top level) | `null`, or `code` (`cannot_open`, `cannot_read_metadata`, `cannot_read`, `too_large`, `invalid_json`, `not_an_object`, `cannot_list_directory`, `unstable`), `detail` (the system's or parser's own text, or `null`) and the English `text` |
| `rows` | one row per key and source: `key` as written, `source`, `status` (`applied`, `overridden`, `invalid`, `unknown`, `not_applicable`, `ignored_limited_trust`), `reason` and `strict_applied` (for `invalid`: why, and whether the key's strict value applies instead), `value` (display form, cut at 200 characters), `dropped` (at most 50 list items that were dropped, with `position`, `item` and `reason`) and `dropped_omitted` (how many more were dropped) |

A key is in force when one of its rows has the status `applied`, or `invalid` with
`strict_applied` true (a restrictive key with a wrong value applies its strictest
value, so a typo does not lift the restriction).

### How long a change takes

Rustket reads the policy at start and then every five minutes. Two kinds of change
are applied only when the next read, five minutes later, shows them again: a source
that had values and is empty now, and a source that was readable and cannot be read
now. Group Policy deletes and rewrites its keys during a refresh, and a file can be
caught half-written. A single key or list that disappears while the source keeps
other values is not held: it is lifted at once, until the next read restores it.
This comes on top of your management tool's own delay (Group Policy: section 2;
Intune devices check in every few hours). Some keys apply
only at the next start (`HardwareAcceleration`) or the next sign-in (the sign-in
keys); the reference says which.

### Inventory and compliance scripts

The templates archive has two short scripts that find the installed copy, check that
it is 0.4.0 or later, run `--dump-policy` into a temporary file and report whether a
given key is applied. They print a one-line summary and exit with 0 when Rustket
0.4.0 or later is installed and, if a key is given, that key is applied; with 1
otherwise, as detection and compliance tools expect. Without a key, 0 means only
"installed, 0.4.0 or later". A key with an invalid value counts as not applied, so
a typo shows up even when a strict value is in force.

**Intune detection and remediation scripts and Jamf Pro extension attributes pass no
arguments.** For them, edit the defaults at the top of the copy you upload instead
of passing parameters.

- `scripts/rustket-policy-check.ps1 [-Key RequireHttps] [-Exe <path>]` (Windows
  PowerShell 5.1 or 7): finds the copy through the uninstall key; usable as an Intune
  detection script, a remediation detection script or a compliance check of other
  tools. For Intune, set the key in the param block: `[string]$Key = 'RequireHttps'`.
  Run as SYSTEM, the script starts the copy it checks: `-Exe` must name a copy that
  only administrators can change, never a portable copy in a user's profile; to check
  such a copy, run the script as that user.
- `scripts/rustket-policy-check.sh [--jamf] [KEY]` (macOS and Linux): finds
  `/Applications/Rustket.app` or `rustket` on the `PATH` (`RUSTKET_BIN` overrides
  it). With `--jamf` it prints `<result>...</result>` for a Jamf Pro extension
  attribute and always exits with 0. Without arguments it uses `DEFAULT_KEY` and
  `JAMF` at the top of the script: for an extension attribute set `JAMF=1` and,
  for example, `DEFAULT_KEY="RequireHttps"`. It reads the JSON with `osascript`
  on macOS, which every Mac has (it never runs `python3` there: without the
  developer tools that is a stub that asks the user to install them), and with
  `python3` on Linux; without `python3` it stops with a message. Run as root, the
  script executes the copy it checks: `RUSTKET_BIN` must name a copy that only root
  can change (the script refuses others), never an AppImage in a user's home; to
  check such a copy, run the script as that user.

## 7. Coming from the official Rocket.Chat desktop app

**Do this:** translate your existing files into policy keys. Rustket does not read the
official app's files (`servers.json`, `overridden-settings.json`, `update.json`) or
its installer switches.

| Official app | Rustket |
|---|---|
| `servers.json` (`{"Name": "https://url"}`) | `ManagedServers`, one item per server: `https://url Name`. Unlike `servers.json`, which only fills an empty server list, managed servers are always present and cannot be removed |
| `isAddNewServersEnabled` | `AllowUserServers`; it also hides removing servers and refuses `rocketchat://` links to servers that are not configured |
| `isUpdatingEnabled`, `doCheckForUpdatesOnStartup` | `UpdateMode` (`off`: no checks at all; `notify`: report only; `prompt`: check and install on request) |
| `isHardwareAccelerationEnabled` | `HardwareAcceleration` |
| `update.json`, the installers' option to disable automatic updates | `UpdateMode` |

Every other key of `overridden-settings.json` (for example `isReportEnabled`,
`isTrayIconEnabled`, `isMinimizeOnCloseEnabled`, `isMenuBarEnabled`,
`isFlashFrameEnabled`, `isInternalVideoChatWindowEnabled`) has **no equivalent** in
Rustket. Rustket's keys that the official app has no counterpart for include
`AllowedServers`, `RequireHttps`, `AllowCertificateExceptions` and `MinimumVersion`.
Most of the official app's overrides are re-applied at each start and can be changed
by the user until then; Rustket's keys stay locked while the policy is in force.

## 8. What has been verified

"Automated test" means a test that runs in this project's continuous integration on a
real machine of that system; "added, first run pending" means such a test exists but
has not yet passed there. "Unit test" means the logic was tested with simulated
inputs. Nothing below has been checked by an administrator on a managed fleet yet.

| What | State |
|---|---|
| Templates match what the readers accept (ADMX, profile, Intune plist, Jamf schema, Linux schema and example) | unit test |
| Windows registry reader: reading `HKLM` and `HKCU` | automated test in CI on Windows (x64 and ARM64), read-only: added, first run pending |
| Windows registry reader: values written under `HKCU` (DWORD, string, multi-string, list subkey) and read back | automated test in CI on Windows (x64 and ARM64): added, first run pending |
| Loading the templates into the Group Policy editor | not yet verified on a real system |
| Intune "Import ADMX" and the custom OMA-URI profile | not yet verified on a real system |
| A new template revision through the custom OMA-URI profile | not yet verified on a real system |
| The PowerShell fallback script of section 2 | not yet verified on a real system |
| The installer run as SYSTEM (`/ALLUSERS`, upgrade while Rustket runs in a user session, uninstall) | not yet verified on a real system |
| The computer startup script of section 2 | not yet verified on a real system |
| macOS managed-preferences reader | automated test in CI on macOS, read-only, outside an app bundle (where it sees no profile): added, first run pending |
| A profile on a managed Mac, disk image build | not yet verified on a real system |
| A profile on a managed Mac, Mac App Store build | not yet verified on a real system |
| `--dump-policy` in the Mac App Store build | not yet verified on a real system |
| Linux: a root-owned file in `/etc/rustket/policies/managed` read with full trust | automated test in CI on Ubuntu: added, first run pending |
| Linux: trust checks, merging, broken files | unit test |
| Linux: the deb and rpm packages own the empty directory | packaging tests |
| Ansible, Samba and Intune recipes of section 4 | not yet verified on a real system |
| `rustket-policy-check.sh` | unit test with a simulated Rustket (Linux reading with `python3`; the macOS reading with `osascript` runs in CI on macOS: added, first run pending) |
| `rustket-policy-check.ps1` | not yet verified on a real system |
| Sign-in through an identity provider with conditional access | not yet verified on a real system |
