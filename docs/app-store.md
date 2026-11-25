# Mac App Store

Rustket has a second macOS flavor for the Mac App Store ("MAS flavor"). It is
built from the same sources with the `mas` cargo feature, runs in the App
Sandbox, is signed with an **Apple Distribution** identity, carries an embedded
**Mac App Store Connect** provisioning profile, and is packaged as a
`productbuild` .pkg signed with **Mac Installer Distribution** ("3rd Party Mac
Developer Installer"). `.github/workflows/app-store.yml` uploads it with
`xcrun altool`; it is tested through TestFlight and submitted for review by
hand in App Store Connect. The Developer ID dmg (docs/release-signing.md) is
unchanged and independent.

Both flavors use the bundle ID `io.github.sokolcloud.rustket` and the category
Business (`public.app-category.business`). Store name: **Rustket**;
"Rocket.Chat" never appears in the app name, subtitle, keywords or icon.
Seller: SOKOL CLOUD & PRODUCTION COMPANY LIMITED, team `D6JXBXV9Q5`.

## The flavor

- `cargo build --features rustket/mas`. The feature is off by default and
  refuses to compile together with `self-update` (`compile_error!` in
  `crates/rustket/src/lib.rs`): store builds update only through the App Store
  (guideline 2.4.5(vii)).
- MAS-only code sits behind `cfg(feature = "mas")`; changes that help both
  flavors go into both.
- Rules for code that runs on macOS (all builds): no child processes, no
  writes outside the app container (except files the user picks in a panel),
  no downloaded or interpreted code. The App Sandbox enforces most of this in
  the MAS build; the dmg follows the same rules so the flavors do not drift,
  except for the software-rendering re-exec, the self-updater's tools and the
  autostart LaunchAgent, which only the dmg has.
- winit comes from `vendor/winit`, a copy of the crates.io release without the
  private `CGS*` symbols the store rejects (guideline 2.5.1, ITMS-90338);
  `vendor/winit/README.rustket.md` has the diff and when to drop it.
- Install one flavor or the other: both are `/Applications/Rustket.app` with
  the same bundle ID, and Launch Services picks one of them for
  `rocketchat://` links. The MAS build keeps its data in its container and
  starts fresh. A launch-at-login LaunchAgent left by a dmg install keeps
  starting the dmg copy; the MAS app cannot remove it, so turn launch at login
  off in the dmg build (or delete
  `~/Library/LaunchAgents/io.github.sokolcloud.rustket.plist`) before
  switching.

## Packaging (`packaging/macos/build.sh`)

`MAS=1` selects the store flavor; the stage directory is
`packaging/out/macos-mas` and the result `packaging/out/Rustket-<v>-mas.pkg`.

- **Info.plist** (`write_info_plist`, shared with the dmg): `CFBundleVersion`
  is `MAS_BUILD_NUMBER`, `CFBundleShortVersionString` must be a numeric
  x.y.z (pre-releases are refused), `ITSAppUsesNonExemptEncryption` false,
  `CFBundleSupportedPlatforms` MacOSX and the toolchain keys `DTCompiler`,
  `DTPlatformBuild` (the Xcode build), `DTPlatformName`, `DTPlatformVersion`,
  `DTSDKBuild`, `DTSDKName`, `DTXcode`, `DTXcodeBuild` and
  `BuildMachineOSBuild`, computed from `xcodebuild`, `xcrun` and `sw_vers` on
  the machine that packages. Both flavors: `CFBundleLocalizations`
  en ru es th zh-Hans with `<lang>.lproj/InfoPlist.strings`
  (`NSLocalNetworkUsageDescription`, from `packaging/macos/InfoPlist`),
  `CFBundleURLName` = the bundle ID, `CFBundleIconName` AppIcon next to
  `CFBundleIconFile` rustket.
- **Icons**: `Assets.car` compiled by `actool` from
  `packaging/macos/AppIcon.appiconset` (regenerate with
  `cargo run -p rustket --example gen_icons -- packaging/macos/AppIcon.appiconset --appiconset`),
  plus the existing `rustket.icns`.
- **No `install-kind` marker** in the MAS bundle.
- **Static gates** on the universal binary: `lipo -verify_arch arm64 x86_64`;
  `vtool -show-build` minos 11.0 (= `LSMinimumSystemVersion`) and the SDK of
  the pinned Xcode, per slice; `nm -u` per slice must list no `_CGS*`/`_SLS*`
  symbol except the public `_CGShieldingWindowLevel` (an `nm` failure fails
  the build); the binary must not contain `didDismissAlert` (a private
  NSUserNotificationCenter selector) or updater markers (`api.github.com`,
  `install-kind`, `SHA256SUMS`).
- **DT probe**: when `packaging/macos/xcode-probe` exists (an empty Xcode
  macOS app, bundle ID `io.github.sokolcloud.rustket.probe`, created on a Mac,
  see step 10 below), the build job builds it with the same Xcode and fails
  if its DT* keys differ from ours. Until it exists, the build prints a notice.
- **Signing**: the profile must be for `D6JXBXV9Q5.io.github.sokolcloud.rustket`
  (or `$APPLE_TEAM_ID.…`), list no devices and must not have expired
  (a warning under 30 days left). It is embedded as
  `Contents/embedded.provisionprofile`, then `xattr -cr`, then **one**
  `codesign` (no `--deep`) with `MAS_SIGN_IDENTITY`,
  `packaging/macos/rustket-mas.entitlements` and `-o runtime`; the signed
  entitlements must equal the file exactly. Then
  `productbuild --component Rustket.app /Applications --sign "$MAS_INSTALLER_IDENTITY"`,
  `pkgutil --check-signature` and a look into the payload. Not notarized.
- `rustket-mas.entitlements` holds exactly `app-sandbox`, `network.client`,
  `files.user-selected.read-write`, `com.apple.application-identifier` and
  `com.apple.developer.team-identifier`. Adding one is a review decision
  (`crates/rustket/tests/packaging.rs` lists the forbidden ones).

Local use on a Mac with Xcode:

```sh
MAS=1 APP_ONLY=1 MAS_BUILD_NUMBER=1 RUSTKET_CARGO_FEATURES=mas packaging/macos/build.sh
MAS=1 MAS_BUILD_NUMBER=1 RUSTKET_CARGO_FEATURES=mas \
  MAS_SIGN_IDENTITY="Apple Distribution: SOKOL CLOUD & PRODUCTION COMPANY LIMITED (D6JXBXV9Q5)" \
  MAS_INSTALLER_IDENTITY="3rd Party Mac Developer Installer: SOKOL CLOUD & PRODUCTION COMPANY LIMITED (D6JXBXV9Q5)" \
  MAS_PROVISIONING_PROFILE=~/Downloads/Rustket_MAS.provisionprofile packaging/macos/build.sh
```

## Apple portal setup (in order)

[AH] = Account Holder (or Admin where Apple allows it), [MAC] = on the Mac.

Before any upload:

1. [AH] developer.apple.com → Account: accept any pending Program License
   Agreement. App Store Connect → Business: no agreement pending (a free app
   without IAP needs no Paid Apps Agreement).
2. [AH] App Store Connect → Business → Compliance → Digital Services Act:
   "This is a trader account"; public role phone and email (verified by
   code), business registry document, bank details. Apple reviews the
   document, so start early.
3. [AH/Admin] Certificates, Identifiers & Profiles → Identifiers → +: explicit
   App ID `io.github.sokolcloud.rustket`, macOS, description "Rustket", prefix
   = Team ID `D6JXBXV9Q5`, no capabilities.
4. [AH/Admin/App Manager] App Store Connect → Apps → + New App: macOS, name
   **Rustket**, primary language English (U.S.), the bundle ID, SKU
   `rustket-macos`, full access. Note the numeric **Apple ID** for
   `MAS_APPLE_ID` (the API cannot create apps).
5. [MAC] Check the portal for an existing Apple Distribution certificate (a
   cloud-managed one from Xcode cannot be exported). Keychain Access →
   Certificate Assistant → Request a Certificate from a Certificate Authority →
   Saved to disk: one CSR per certificate.
6. [AH] Certificates → +: **Apple Distribution** from CSR 1, **Mac Installer
   Distribution** from CSR 2. Download both `.cer`.
7. [MAC] Import both into the login keychain. `security find-identity -v` shows
   "Apple Distribution: SOKOL CLOUD & PRODUCTION COMPANY LIMITED (D6JXBXV9Q5)"
   and "3rd Party Mac Developer Installer: SOKOL CLOUD & PRODUCTION COMPANY
   LIMITED (D6JXBXV9Q5)". Export both identities with their keys into one
   `.p12` with a generated password; keep an encrypted backup like the
   Developer ID one and delete the plaintext exports.
8. [AH] Profiles → + → Distribution → **Mac App Store Connect** → the App ID →
   the Apple Distribution certificate → name "Rustket MAS" → download. [MAC]
   `xattr -c` the file; `security cms -D -i <file>` shows
   application-identifier `D6JXBXV9Q5.io.github.sokolcloud.rustket` and no
   ProvisionedDevices.
9. GitHub → Settings → Environments → New **`app-store`**: deployment refs tag
   `v*` and branch `app-store-dry-run`; optionally the Account Holder as
   required reviewer. Set the secrets and variables below with
   `gh secret set --env app-store …` (values never pass through agents). For
   the sandbox test build, add the branch `app-store-dry-run` to
   `macos-signing`'s refs and set the repository variable
   `MAS_SANDBOX_TEST=true`.
10. [MAC] Xcode → New → macOS App, bundle ID
    `io.github.sokolcloud.rustket.probe`, no team. Commit it under
    `packaging/macos/xcode-probe/`. Build it once and record its Info.plist
    DT* keys, `codesign -d -r -` and entitlements as a reference.

After the pipeline exists:

11. Push `app-store-dry-run`: build, sign, `productbuild`, `altool
    --validate-app`. This also shows whether the Developer-role key may
    validate and upload.
12. First upload: run the workflow on `app-store-dry-run` with `upload: true`
    (or from a `v*` tag). Watch the processing email for ITMS codes.
13. [AH/Admin] On the first build, answer export compliance (below) and check
    that `ITSAppUsesNonExemptEncryption` matches the answer.
14. TestFlight → Internal Testing: a group with the user; Test Information
    (beta description, feedback email).
15. [MAC] Install from TestFlight (macOS 13+) and run the Mac checklist. Add
    the build to an **external** group early: Beta App Review runs the
    toolchain check that `--validate-app` does not.
16. App Information: subtitle, categories (Business, secondary Productivity),
    Content Rights, age rating, Privacy Policy URL. App Privacy (below).
    Pricing and Availability: Free; remove France and China mainland. DSA
    status for the app.
17. Version page per localization (en, ru, es, th, zh-Hans): screenshots,
    texts, keywords, Support and Marketing URL, Copyright, App Review
    Information (notes below). Version release: **Manual**.
18. Optional: an App Review appointment (Meet with Apple).
19. Select the build → Add for Review → Submit. Keep the review server
    unchanged until approval.
20. After approval: Release This Version.

Later and recurring:

21. France: file the ANSSI declaration, upload the receipt under App
    Information → App Encryption Documentation, then set
    `ITSAppUsesNonExemptEncryption` YES and `ITSEncryptionExportComplianceCode`
    in `write_info_plist` and add France.
22. Yearly: certificates and profile expire. New CSRs on the Mac, re-export,
    `gh secret set`, re-create the profile (it is tied to the certificate).
    Calendar reminder 30 days ahead; the build warns when the profile expires
    in under 30 days and fails once it has expired. Live apps keep working;
    uploads stop.
23. Before each yearly cycle: check
    https://developer.apple.com/news/upcoming-requirements/ for a new macOS SDK
    minimum.
24. Optional: a team API key with the App Manager role for TestFlight or
    metadata automation.

## Secrets and variables (environment `app-store`)

| Name | Kind | Value |
| --- | --- | --- |
| `MAS_CERTIFICATES_P12` | secret | base64 `.p12` with the Apple Distribution and Mac Installer Distribution identities and keys |
| `MAS_CERTIFICATES_PASSWORD` | secret | its password |
| `MAS_PROVISIONING_PROFILE` | secret | base64 of the Mac App Store Connect profile |
| `APPLE_API_KEY_P8` | secret | base64 `AuthKey_<id>.p8`: the notary key's values again (docs/release-signing.md) |
| `APPLE_API_KEY_ID` | secret | key ID |
| `APPLE_API_ISSUER_ID` | secret | issuer ID |
| `APPLE_TEAM_ID` | variable | `D6JXBXV9Q5` |
| `MAS_APPLE_ID` | variable | numeric app Apple ID from App Store Connect |
| `MAS_BUILD_OFFSET` | repository variable, optional | default 0 |
| `MAS_SANDBOX_TEST` | repository variable, optional | `true` turns on the sandbox test build |

```sh
base64 -i rustket-mas.p12 | gh secret set MAS_CERTIFICATES_P12 --env app-store
gh secret set MAS_CERTIFICATES_PASSWORD --env app-store
base64 -i Rustket_MAS.provisionprofile | gh secret set MAS_PROVISIONING_PROFILE --env app-store
base64 -i AuthKey_XXXXXXXXXX.p8 | gh secret set APPLE_API_KEY_P8 --env app-store
gh secret set APPLE_API_KEY_ID --env app-store
gh secret set APPLE_API_ISSUER_ID --env app-store
gh variable set APPLE_TEAM_ID --env app-store --body D6JXBXV9Q5
gh variable set MAS_APPLE_ID --env app-store --body <numeric id>
```

## Workflow (`.github/workflows/app-store.yml`)

| Run | What happens |
| --- | --- |
| push to `app-store-dry-run` | build, gates, sign, `productbuild`, `altool --validate-app`; no upload |
| manual run on `app-store-dry-run`, `upload: true` | the same, then `altool --upload-package` (internal TestFlight builds before a release) |
| manual run from a `v*` tag | the tag must equal `v<Cargo version>`; validate and upload |
| manual run on another ref | only the build job (no signing environment) |

Release procedure per version: tag `vX.Y.Z` (release.yml publishes the dmg
and the other packages), then Actions → App Store → Run workflow → "Use
workflow from" the tag. Then TestFlight, then submit in App Store Connect.

Jobs and trust boundary (the signing jobs run no cargo and none of the app's
code):

- `mas-build` (macos-26, no environment, no secrets):
  `MAS=1 APP_ONLY=1 build.sh` compiles with `RUSTKET_CARGO_FEATURES: mas`, runs
  `rustket --version` (the only place the binary runs), the static gates and
  the DT probe. The artifact `Rustket-<v>-mas-binary.tar` (7 days) holds only
  the universal `rustket` binary and its `THIRD-PARTY-NOTICES.txt` text.
- `mas-upload` (macos-26, environment `app-store`, 100 min): no cargo, no
  rustup, never runs the binary. Signing policy: a tag run or an upload fails
  when any secret or variable is missing; a dry run without them only
  assembles the unsigned app with a warning. The artifact must contain exactly
  those two regular files. A temporary keychain with a random password; the
  Apple Distribution SHA-1 comes from `find-identity -v -p codesigning`, the
  installer identity from `find-identity -v` (the codesigning filter hides
  it). `MAS=1 PREBUILT_APP=1 build.sh` builds the bundle around the binary
  from the checkout (Info.plist with DT* values computed on this runner,
  entitlements, icons, `.lproj`), signs and packages it. `altool` output is
  checked by `packaging/macos/altool_result.py`, which fails on any error line
  or JSON product error whatever the exit code; the upload gets 30 minutes per
  attempt and one retry after a hang. The pkg is kept 7 days. The keychain,
  key and profile are always removed.
- `mas-sandbox-test` (optional, `app-store-dry-run` with
  `MAS_SANDBOX_TEST=true`, environment `macos-signing`): the same bundle signed
  with Developer ID and `rustket-sandbox-test.entitlements` (sandbox only, no
  profile), notarized and stapled, as a zip artifact (7 days) for the Mac
  checklist before TestFlight.

The first validate run prints `xcrun altool --help` to the job log: check the
Xcode 26 flag names there (`--platform macos`, `--api-key`, `--api-issuer`,
`--apple-id`, `--bundle-version`, `--bundle-short-version-string`,
`--upload-package`, `--wait`). Never use `--upload-app` or `--auth-string`.

## Build numbers

`MAS_BUILD_NUMBER = (github.run_number + MAS_BUILD_OFFSET).github.run_attempt`,
for example `14.1`, and a re-run of the same workflow run gives `14.2`. The same
value goes to `CFBundleVersion` and `--bundle-version`. Build numbers must grow
within a version, and the first uploaded number is a permanent floor, so start
small. Raise `MAS_BUILD_OFFSET` above the last uploaded number if the workflow
is ever renamed or recreated (the run number starts again at 1). Do not use a
git commit count: the shallow checkout returns 1, and a re-spin of one commit
needs a new number.

## Xcode pin

`DEVELOPER_DIR` in `app-store.yml` (and the macOS jobs of `release.yml`) pins a
**release** Xcode of the `macos-26` image, currently Xcode 26.6 (17F113, macOS
SDK 26.5). App Store Connect rejects builds from beta Xcodes, and both the
build job and `build.sh` fail when the Xcode path or `xcodebuild -version`
says beta (the build number cannot tell: releases such as 16C5032a end in a
letter too). Moving on: when the image
lists a newer release Xcode
(https://github.com/actions/runner-images/blob/main/images/macos/macos-26-arm64-Readme.md),
change the path in both workflows, push `app-store-dry-run`, and check the DT
probe. Move to Xcode 27 once it is a release on a stable (non-preview) runner
image. Check the upcoming-requirements page yearly (step 23).

## Export compliance record

- Encryption: standard algorithms only, used in addition to the OS: TLS 1.2/1.3
  through rustls (aws-lc-rs/ring), and Rocket.Chat end-to-end encryption
  (RSA-OAEP, AES). No proprietary algorithms.
- Classification: mass-market, ECCN 5D992.c; no CCATS. (Whether an annual BIS
  report is needed for an end-user mass-market app under 740.17(e)(3) is a
  question for counsel.)
- App Store Connect answer: "standard algorithms in addition to the OS".
- France: excluded from availability until an ANSSI declaration is filed
  (step 21). China mainland: excluded.
- Info.plist: `ITSAppUsesNonExemptEncryption` is **false** for now (decision
  of 2026-11-25). If the questionnaire result requires YES, set it in
  `write_info_plist` together with `ITSEncryptionExportComplianceCode`.

## App Privacy

"Data Not Collected", no tracking. This holds only while the MAS build has no
telemetry, crash reporting or update checks (the gates check the updater
markers), and the organization does not operate the workspaces its users log
into (the review server aside). If it does, decide whether to declare data for
those users. The privacy policy must say the same.

Legal pages (placeholders until hosting is final; one module in the app,
`crates/rustket/src/legal.rs`): https://sokolcloud.github.io/rustket/privacy/ ,
https://sokolcloud.github.io/rustket/terms/ ,
https://sokolcloud.github.io/rustket/support/ .

In-app disclaimer (About): "Independent client for Rocket.Chat servers; not
affiliated with or endorsed by Rocket.Chat Technologies Corp. Rocket.Chat is a
trademark of Rocket.Chat Technologies Corp." Copyright: "Copyright © 2026
Sergey Sokolov. MIT License."

## Review server runbook

A dedicated public workspace for App Review, kept online and unchanged through
every review round (a 2.1 rejection follows downtime):

- HTTPS with a public certificate, reached by hostname, working over IPv6/NAT64;
  no bot challenge in front of it.
- Registration off, email 2FA auto opt-in off, password login only, the
  bad-words filter on.
- Fictional content (no real people, no Rocket.Chat logo in the workspace
  branding), a channel with threads and a discussion, a team, an E2E room.
- Accounts: a normal user (also in the Sign-In fields), a workspace admin and an
  omnichannel agent; none with 2FA.
- Monitoring, and a change freeze from submission to approval (including
  re-review).

## Review notes template

4000 bytes at most; do not mention external-browser SSO.

```
Rustket is a free, open-source native Mac client for an organization's own Rocket.Chat
server (self-hosted team chat). The developer operates no chat service; the server
below exists only for this review and stays online until review is complete.

Server: https://<demo-host>  (accept the terms on the first screen, then enter the server and sign in)
Accounts (no two-factor authentication):
- Regular user: <user> / <password>  (also in the Sign-In fields)
- Workspace administrator: <admin> / <password>
- Omnichannel agent: <agent> / <password>

Moderation (Guideline 1.2): accounts and content are managed by each workspace's
administrators. In Rustket:
- Report a message: right-click a message > Report.
- Report a user: right-click one of their messages > Report user, or click their
  avatar > User info > Report user.
- Block a user: open the direct message, click the User info (i) button at the top
  right > Block (their messages are collapsed).
- Ignore a user in a channel: click their avatar > User info > Ignore.
- Hidden words filter: Settings (Rustket > Settings..., Cmd-,) > Privacy & storage >
  Hidden words.
- Terms (zero tolerance for objectionable content) are accepted on first launch.
Reports go to the workspace's moderation console (sign in as the administrator >
Administration > Moderation). On this server we act on reports within 24 hours.
Contact: <support email>.

Features that depend on server role or settings (Guideline 2.5.19): Administration and
the moderation console (admin account), the Omnichannel section of the sidebar (agent account), end-to-end
encrypted rooms (room "<e2e room>"), teams and discussions (channel "<channel>"), app
message blocks (rendered natively from JSON; no code is downloaded or executed).

Video calls open in the user's default browser. Updates come only from the Mac App
Store. Launch at login is off by default (Settings > General).
Encryption: standard TLS (rustls) and Rocket.Chat end-to-end encryption (RSA-OAEP, AES).
Tested on: <Apple silicon Mac>, macOS 27.2.1; <Intel Mac>, macOS 26.x.
```

## Troubleshooting

Processing errors arrive by email and in App Store Connect; read the full
message, the codes below are the usual causes for a pipeline without Xcode.

| Code | Usual cause | Where to look |
| --- | --- | --- |
| ITMS-90296 | App Sandbox not enabled | `com.apple.security.app-sandbox` in `rustket-mas.entitlements`; `check_entitlements` |
| ITMS-90889 | no provisioning profile in the bundle | `Contents/embedded.provisionprofile` (embedded before signing) |
| ITMS-90886 / ITMS-90885 | the signature and the profile disagree on the application or team identifier | identity entitlements vs. the profile (`check_profile`); a profile for another certificate |
| ITMS-90288 | invalid code-signing entitlements or a profile mismatch | entitlements file, profile type (Mac App Store Connect, not Developer ID) |
| ITMS-90237 | the package is not signed with the installer certificate | `MAS_INSTALLER_IDENTITY`, `pkgutil --check-signature` |
| ITMS-90236 | missing or incomplete icon | `rustket.icns`, `Assets.car`, `CFBundleIconName` |
| ITMS-90249 / ITMS-90264 | bundle or executable signing problem (nested code, wrong certificate) | one `codesign` without `--deep`, Apple Distribution identity |
| ITMS-90338 | non-public API (symbols or selectors) | the `nm -u` gate; `vendor/winit/README.rustket.md` lists the undocumented selectors left in winit |
| ITMS-91109 | files with extended attributes (quarantine) in the package | `xattr -cr` before signing, the `xattr -r -l` check |
| 409 / "bundle version must be higher" | the build number was used before | re-run (new `run_attempt`) or raise `MAS_BUILD_OFFSET` |
| "built with a beta version of Xcode" (Submit, external TestFlight) | toolchain fingerprint | `DEVELOPER_DIR`, the DT* keys and the probe diff |

`altool` can exit 0 after an error and hang on a dropped part ("WILL RETRY PART
1. Checksums do not match"); the workflow parses the output and retries once.
Legacy syntax (`-t osx`, `--apiKey`) exists behind `--use-old-altool`.
