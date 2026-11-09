# Rustket

A native Rocket.Chat desktop client in Rust ([iced](https://iced.rs) 0.14). There is
no Electron and no embedded web view: rooms, threads, end-to-end encryption, apps
(UIKit), omnichannel, teams and administration are rendered natively. Browser
sign-in (OAuth, OpenID, SAML, CAS) and video calls open in the default browser
and come back through `rocketchat://` links.

Tested against Rocket.Chat 8.9. Linux is the primary platform.

## Install

Packages for every release are on the
[releases page](https://github.com/sokolcloud/rustket/releases), with a
`SHA256SUMS` file. The Windows and macOS builds are not code-signed yet.

### Debian / Ubuntu (.deb)

```sh
sudo apt install ./rustket_0.2.0-1_amd64.deb
```

The package installs `/usr/bin/rustket`, the desktop entry
`/usr/share/applications/rustket.desktop` (it handles `rocketchat://` links), icons
under `/usr/share/icons/hicolor/` and AppStream metadata. The binary links only
glibc and libgcc_s; X11 (also used through XWayland), xkbcommon, Wayland,
Vulkan/EGL and D-Bus are loaded at runtime and pulled in as
dependencies/recommendations. File dialogs need an XDG desktop portal or
`zenity`; notification sounds need `pw-play`, `paplay` or `canberra-gtk-play`.
Third-party licenses: `/usr/share/doc/rustket/THIRD-PARTY-NOTICES.gz`.

### Fedora / openSUSE (.rpm)

```sh
sudo dnf install ./rustket-0.2.0-1.x86_64.rpm       # Fedora
sudo zypper install ./rustket-0.2.0-1.x86_64.rpm    # openSUSE
```

Same layout and runtime dependencies as the .deb (libraries are required by
soname, so both distributions resolve them); the license file is in
`/usr/share/licenses/rustket/`, the third-party notices in
`/usr/share/doc/rustket/THIRD-PARTY-NOTICES`. The packages are built on Ubuntu
22.04 and need glibc 2.35 or newer.

### AppImage

```sh
chmod +x Rustket-0.2.0-x86_64.AppImage
./Rustket-0.2.0-x86_64.AppImage
```

To make `rocketchat://` links open the AppImage, use the "Open rocketchat:// links
with Rustket" action in the app (it registers the AppImage file itself, so keep
it at a stable path). Third-party licenses are in
`usr/share/doc/rustket/THIRD-PARTY-NOTICES` inside the image.

The AppImage uses the glibc of the system it runs on and needs at least the
version it was built against: 2.35 for the release builds (Ubuntu 22.04). When
building it yourself, build on the oldest distribution you want to support.

### Windows

Two downloads for x64 and ARM64 (Windows 10 or 11):

- `Rustket-0.2.0-x64-setup.exe` / `Rustket-0.2.0-arm64-setup.exe`: installer.
  It installs for the current user without administrator rights (the first page
  offers an all-users install), adds a Start menu shortcut, optionally a
  desktop icon, and registers `rocketchat://` links (a checked option, since it takes them over from the official Rocket.Chat app). Uninstall from Settings →
  Apps.
- `rustket-0.2.0-windows-x64.zip` / `rustket-0.2.0-windows-arm64.zip`: portable.
  Extract anywhere and run `rustket.exe`; use the "Open rocketchat:// links with
  Rustket" action in the app to register the links for that copy.

The builds are unsigned, so SmartScreen may warn "Windows protected your PC":
click **More info → Run anyway**. Third-party licenses:
`THIRD-PARTY-NOTICES.txt` next to `rustket.exe`.

### macOS

`Rustket-0.2.0-macos-universal.dmg` runs natively on Apple Silicon and Intel
Macs with macOS 11 or newer. Open it and drag Rustket to Applications.

The app is not notarized, so Gatekeeper blocks the first start. Either
right-click (Control-click) Rustket in Applications and choose **Open**, then
**Open** again; or try to open it once, then use **System Settings → Privacy &
Security → Open Anyway**. Alternatively remove the quarantine flag:

```sh
xattr -dr com.apple.quarantine /Applications/Rustket.app
```

Third-party licenses are in `Rustket.app/Contents/Resources/THIRD-PARTY-NOTICES`.

### Build from source

Rust 1.90 or newer (rustup). No system `-dev` packages are needed (TLS is rustls,
D-Bus is pure Rust); a C compiler is used for bundled sources.

```sh
cargo build --release              # target/release/rustket, fast daily build
cargo build --profile dist -p rustket   # smaller/faster binary for distribution (thin LTO)
```

Packages are built from the `dist` profile with scripts that need only tools
from the distribution:

```sh
packaging/deb/build.sh         # packaging/out/rustket_<version>-1_<arch>.deb (dpkg-deb)
LINT=1 packaging/deb/build.sh  # ... and run lintian
packaging/rpm/build.sh         # packaging/out/rustket-<version>-1.<arch>.rpm (rpmbuild)
packaging/appimage/build.sh    # packaging/out/Rustket.AppDir, and the .AppImage
                               # when appimagetool is on PATH
packaging/test.sh              # builds the .deb and AppDir and checks their contents
python3 -I packaging/notices.py >THIRD-PARTY-NOTICES   # license notices only
packaging/macos/build.sh       # macOS: universal Rustket.app and .dmg
pwsh packaging/windows/build.ps1 -Arch x64   # Windows: setup.exe (Inno Setup 6.3+) and .zip
```

Releases are built by `.github/workflows/release.yml` on a `v<version>` tag
(pushing the `release-dry-run` branch builds the same packages without
publishing).

`SKIP_BUILD=1` reuses an existing `target/dist/rustket`. The scripts need
`python3` for the third-party notices. cargo-deb users can run
`python3 -I packaging/notices.py >packaging/out/THIRD-PARTY-NOTICES` and then
`cargo deb -p rustket --profile dist --no-strip` instead (metadata in
`crates/rustket/Cargo.toml`, kept in sync but not exercised; the scripts are
the maintained path).

To run a binary from the source tree without installing, register it as the
`rocketchat://` handler from inside the app. When the packaged desktop file is
installed and starts the same executable, the app makes `rustket.desktop` the
default handler instead of writing its own `rustket-url-handler.desktop`.
If an older registration still points at another executable (for example a
source build registered before the package was installed), the app shows the
register action again; using it switches to the packaged entry.

## Usage

```
rustket [--help] [--version] [LINK...]
```

Only one instance runs per user; `rocketchat://` and `https://go.rocket.chat/`
links on the command line are forwarded to the running instance.

### Window title bar

By default Rustket draws its own title bar: the workspaces (servers), the "add
server" and settings buttons and the minimize/maximize/close buttons share one
row at the top. Drag the empty part of the bar to move the window, double-click
it to maximize, right-click the title for a window menu, and drag the window
borders to resize. Dialogs cover only the area below the bar, so the window can
still be moved and closed with one open.

If you prefer your window manager's decorations, turn on **Settings → Appearance
→ Use system window title bar**: the window gets the system title bar right
away and the workspaces move back to a column on the left.

### Hardware acceleration

Rendering uses the GPU (wgpu: Vulkan or OpenGL/EGL) by default. If the window is
blank, flickers or the GPU driver misbehaves, turn off **Settings → Appearance →
Hardware acceleration (GPU)** (it takes effect after a restart: the next start
uses the CPU renderer), or start it once with:

```sh
ICED_BACKEND=tiny-skia rustket
```

### Keyboard

- Tab / Shift+Tab move through every button, row and field; the focused one
  gets a focus ring and Enter or Space activates it. The title bar comes first.
- F6 / Shift+F6 jump between the main regions: sidebar search, timeline,
  composer and the right panel (the thread composer when a thread is open).
  The title bar is not one of them.
- Escape closes the innermost thing: a menu or dialog, then a field's focus,
  then the right panel or the open settings/account/directory/administration
  page.
- Alt+Up / Alt+Down open the previous / next room.
- Message text can be selected with the mouse (double-click: word,
  triple-click: paragraph); Ctrl+A selects the whole message last clicked and
  Ctrl+C copies the selection. While a text field has the focus, these keys
  belong to the field.

## Where data lives

| What | Where |
|---|---|
| Servers and preferences | `~/.config/rustket/config.toml` (`$XDG_CONFIG_HOME`) |
| Offline cache: rooms, settings and the newest messages per room, plus messages not sent yet | `~/.local/share/rustket/<server id>/cache.sqlite` (`$XDG_DATA_HOME`), private to the user; one per server, deleted when you log out of or remove that server. **Settings → Storage → Offline cache → Clear** empties it but keeps unsent messages. Encrypted rooms are stored as ciphertext only and their unsent messages are not kept |
| Downloaded images | `~/.cache/rustket/images/` (`$XDG_CACHE_HOME`); the oldest files are removed past the limit in **Settings → Storage** (500 MB by default); safe to delete while the app is closed |
| Media opened in another app (video, audio, files) | `~/.cache/rustket/media/`, removed after 24 hours |
| Decrypted E2E attachments opened in another app | `~/.cache/rustket/media/e2e/`, private to the user, removed after 24 hours |
| Auth tokens | the desktop keyring (Secret Service: GNOME Keyring, KWallet, KeePassXC) |
| Token fallback | `~/.local/share/rustket/secrets.json` (mode 0600), only when no keyring is available; the app warns about it |
| Start on login | `~/.config/autostart/rustket.desktop` |
| Link handler (source builds) | `~/.local/share/applications/rustket-url-handler.desktop` |

End-to-end encryption keys go through the same secret store as the tokens.

Do not delete `cache.sqlite` by hand while it may hold messages written offline
that were not sent yet: they are sent from there once the connection is back.
Use the Clear button instead, or log out to remove everything of a server.

## Reporting crashes

Run Rustket from a terminal with a backtrace, then attach the output. At the
default log level it never contains passwords, tokens or message text; check
before posting anyway:

```sh
RUST_BACKTRACE=1 rustket 2> rustket.log
```

If more detail is needed, use
`RUST_LOG=info,rustket=debug,rustket_core=debug`. Debug logs can contain message
contents, room names, user names and e-mail addresses (the DDP protocol frames),
so review and redact them before posting them anywhere public.

Include `rustket --version`, the distribution, the desktop session (X11 or
Wayland) and whether hardware acceleration is on.

## Development

See `CLAUDE.md` (toolchain, dev server, tests), `docs/PLAN.md` and
`crates/rustket/COMPONENTS.md`.

## License

Rustket is licensed under the MIT License; see `LICENSE`. The bundled
third-party crates, fonts, icons and data tables keep their own licenses
(collected in `THIRD-PARTY-NOTICES` in the packages, generated by
`packaging/notices.py`).
