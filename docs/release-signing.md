# Release signing (macOS)

The `macos` job of `.github/workflows/release.yml` signs `Rustket.app` and the
dmg with the Developer ID Application certificate of team `D6JXBXV9Q5`
(SOKOL CLOUD & PRODUCTION COMPANY LIMITED), with the hardened runtime and a
secure timestamp, and has both notarized by Apple and stapled. Everything runs
in CI; no Mac is needed except to create the certificate.

## Repository settings

Settings → Environments → **New environment** `macos-signing`:

- Deployment branches and tags → **Selected branches and tags**: add the tag
  rule `v*` and the branch rule `release-dry-run`. Nothing else may use the
  environment, so a workflow run on any other branch (an edited workflow, a
  manual run) never sees the secrets.
- Add the secrets and the `APPLE_TEAM_ID` variable below as environment
  secrets and variables, not repository ones.

`MACOS_ALLOW_UNSIGNED` stays a repository variable (Settings → Secrets and
variables → Actions → Variables).

| Name | Kind | Value |
| --- | --- | --- |
| `MACOS_CERTIFICATE_P12` | environment secret | base64 of a `.p12` with the Developer ID Application certificate and its private key |
| `MACOS_CERTIFICATE_PASSWORD` | environment secret | password of that `.p12` |
| `APPLE_API_KEY_P8` | environment secret | base64 of an App Store Connect API key (`AuthKey_<id>.p8`) |
| `APPLE_API_KEY_ID` | environment secret | the key's ID (10 characters) |
| `APPLE_API_ISSUER_ID` | environment secret | the issuer ID (UUID) shown above the key list |
| `APPLE_TEAM_ID` | environment variable | `D6JXBXV9Q5` |
| `MACOS_ALLOW_UNSIGNED` | repository variable, optional | `true` only as an escape hatch, see below |

Base64 without line breaks: `base64 -i file | tr -d '\n' | pbcopy` on a Mac,
`base64 -w0 file` on Linux. With the GitHub CLI:
`gh secret set MACOS_CERTIFICATE_P12 --env macos-signing < <(base64 -i cert.p12 | tr -d '\n')`.
Secrets already added at the repository level must be added again to the
environment and then deleted from the repository.

## What the jobs do

`macos-build` compiles both slices, assembles the unsigned `Rustket.app`,
runs `rustket --version` as a smoke test and uploads the app as a tar
artifact (kept for a day; the `publish` job leaves it out of the release). It
has no access to the signing secrets.

`macos` runs on a fresh VM with the `macos-signing` environment (only on `v*`
tags and the `release-dry-run` branch; other refs run it without the
environment). It runs no cargo and none of the app's code, so build scripts
of dependencies and the built binary never share a machine with the
unlocked signing key:

1. *Signing policy* checks that every secret and `APPLE_TEAM_ID` is set. On a
   `v*` tag a missing one fails the job, unless `MACOS_ALLOW_UNSIGNED` is
   `true`. Other runs (`release-dry-run` without the secrets, manual runs on a
   branch) build an ad hoc signed dmg with a warning.
2. *Download* and *unpack* the app from `macos-build` (only paths under
   `Rustket.app/` are accepted).
3. *Signing keychain and notary profile* creates a temporary keychain under
   `$RUNNER_TEMP` with a random password, imports the `.p12` (only `codesign`
   may use the key), picks the "Developer ID Application" identity of
   `APPLE_TEAM_ID` (and fails if the `.p12` has none), and stores the API key
   as a `notarytool` profile in the same keychain (this validates the key
   with Apple). It exports `MACOS_SIGN_IDENTITY` (the certificate's SHA-1),
   `MACOS_NOTARY_PROFILE` and `MACOS_NOTARY_KEYCHAIN` for the next step.
4. *Package, sign and notarize* runs `PREBUILT_APP=1 packaging/macos/build.sh`:
   sign the app, notarize a zip of it and staple the app, build the dmg, sign
   it, notarize and staple it, then check both with `codesign --verify`,
   `spctl` and `stapler validate`. A rejected submission prints
   `notarytool log` and fails the job.
5. *Remove the signing keychain* always runs and deletes the keychain and the
   key files.

The app has no entitlements: it uses no JIT, loads no third-party libraries,
sets no `DYLD_*` variables, does not use the camera or microphone (video calls
open in the browser) and sends no Apple Events (it only receives
`rocketchat://` URL events). Tools it runs (`afplay`, `osascript` for the
updater's admin prompt, `hdiutil`, `ditto`) are separate Apple-signed
processes.

Pushing the `release-dry-run` branch with the secrets set signs and notarizes
too: use it to test a new certificate or key before tagging.

## Developer ID Application certificate

Only the Account Holder of the Apple Developer team can create one. It is
valid for five years; apps signed before it expires keep working thanks to the
secure timestamp, and revoking it is only needed if the key leaks (revocation
breaks every app signed with it).

On a Mac:

1. Keychain Access → Certificate Assistant → Request a Certificate From a
   Certificate Authority: your email, "Saved to disk". This creates the
   private key in the login keychain.
2. developer.apple.com → Certificates → **+** → Developer ID Application
   (G2 Sub-CA), upload the request, download `developerID_application.cer`
   and open it, so it joins its private key.
3. Keychain Access → My Certificates → right-click "Developer ID Application:
   SOKOL CLOUD & PRODUCTION COMPANY LIMITED (D6JXBXV9Q5)" → Export → `.p12`
   with a strong password. Export the certificate row (with the key under
   it), not the key alone.
4. Check it: `openssl pkcs12 -info -in cert.p12 -nokeys` (OpenSSL 3 may need
   `-legacy`) shows the subject. Then set `MACOS_CERTIFICATE_P12` and
   `MACOS_CERTIFICATE_PASSWORD`, and delete the exported files.

To rotate, create the new certificate the same way, replace both secrets and
push `release-dry-run`. If the old `.p12` held several Developer ID
identities of the team, the job uses the first and warns; export only one.

## App Store Connect API key

App Store Connect → Users and Access → Integrations → App Store Connect API →
Team Keys → **+**, role **Developer** (enough for notarization). Download
`AuthKey_<KEYID>.p8` (only possible once) and set `APPLE_API_KEY_P8`,
`APPLE_API_KEY_ID` and `APPLE_API_ISSUER_ID`. The key does not expire; to
rotate, create a new key, replace the three secrets, push `release-dry-run`,
then revoke the old key.

## Escape hatch: MACOS_ALLOW_UNSIGNED

If signing is unavailable (expired certificate, Apple outage) and a release
cannot wait, set the repository variable `MACOS_ALLOW_UNSIGNED` to `true`
**and delete the environment secret `APPLE_API_KEY_ID`** (with every secret
set, the job still signs and notarizes, and a notarization failure still
fails it). Delete only that one: the key ID is not confidential, App Store
Connect keeps showing it, and it can be added again, while the `.p8` can be
downloaded only once and the `.p12` exports were deleted. The tag build then
publishes an ad hoc signed dmg with a warning; mention it in the release
notes, since Gatekeeper will block that dmg. Afterwards add `APPLE_API_KEY_ID`
again and delete the variable.

## Updates from older versions

The in-app updater copies the new bundle out of the dmg with `ditto`, which
keeps the signature and the stapled ticket, checks it with
`codesign --verify --deep --strict` and never re-signs it. Updating from an
ad hoc signed version (0.2.10 and older) works the same way. The first start
of the signed version may ask once for keychain access to the stored login
tokens, because the keychain items were created by the ad hoc signed app;
"Always Allow" keeps working for later signed versions.

## Local builds

`packaging/macos/build.sh` reads the same variables: without
`MACOS_SIGN_IDENTITY` it signs ad hoc; with it and `MACOS_NOTARY_PROFILE`
(created with `xcrun notarytool store-credentials`, optionally in
`MACOS_NOTARY_KEYCHAIN`) it notarizes and staples as CI does.
