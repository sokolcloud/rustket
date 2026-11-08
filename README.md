# Rustket

A native Rocket.Chat desktop client in Rust ([iced](https://iced.rs) 0.14). There is
no Electron and no embedded web view: rooms, threads, end-to-end encryption, apps
(UIKit), omnichannel, teams and administration are rendered natively. Browser
sign-in (OAuth, OpenID, SAML, CAS) and video calls open in the default browser
and come back through `rocketchat://` links.

Tested against Rocket.Chat 8.9. Linux is the primary platform.

## Install

### Debian / Ubuntu (.deb)

```sh
sudo apt install ./rustket_0.1.0-1_amd64.deb
```

The package installs `/usr/bin/rustket`, the desktop entry
`/usr/share/applications/rustket.desktop` (it handles `rocketchat://` links), icons
under `/usr/share/icons/hicolor/` and AppStream metadata. The binary links only
glibc and libgcc_s; X11 (also used through XWayland), xkbcommon, Wayland,
Vulkan/EGL and D-Bus are loaded at runtime and pulled in as
dependencies/recommendations. File dialogs need an XDG desktop portal or
`zenity`; notification sounds need `pw-play`, `paplay` or `canberra-gtk-play`.
Third-party licenses: `/usr/share/doc/rustket/THIRD-PARTY-NOTICES.gz`.

### AppImage

```sh
chmod +x Rustket-x86_64.AppImage
./Rustket-x86_64.AppImage
```

To make `rocketchat://` links open the AppImage, use the "Open rocketchat:// links
with Rustket" action in the app (it registers the AppImage file itself, so keep
it at a stable path). Third-party licenses are in
`usr/share/doc/rustket/THIRD-PARTY-NOTICES` inside the image.

The AppImage uses the glibc of the system it runs on and needs at least the
version it was built against (2.38 for a build on Ubuntu 24.04 or newer); build
it on the oldest distribution you want to support.

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
packaging/appimage/build.sh    # packaging/out/Rustket.AppDir, and the .AppImage
                               # when appimagetool is on PATH
packaging/test.sh              # builds both and checks the package contents
python3 -I packaging/notices.py >THIRD-PARTY-NOTICES   # license notices only
```

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

### Hardware acceleration

Rendering uses the GPU (wgpu: Vulkan or OpenGL/EGL) by default. If the window is
blank, flickers or the GPU driver misbehaves, turn off **Settings → Hardware
acceleration** (it takes effect after a restart: the next start uses the CPU
renderer), or start it once with:

```sh
ICED_BACKEND=tiny-skia rustket
```

## Where data lives

| What | Where |
|---|---|
| Servers and preferences | `~/.config/rustket/config.toml` (`$XDG_CONFIG_HOME`) |
| Downloaded images and media | `~/.cache/rustket/images/`, `~/.cache/rustket/media/` (`$XDG_CACHE_HOME`); safe to delete while the app is closed |
| Decrypted E2E attachments opened in another app | `~/.cache/rustket/media/e2e/`, private to the user, removed after 24 hours |
| Auth tokens | the desktop keyring (Secret Service: GNOME Keyring, KWallet, KeePassXC) |
| Token fallback | `~/.local/share/rustket/secrets.json` (mode 0600), only when no keyring is available; the app warns about it |
| Start on login | `~/.config/autostart/rustket.desktop` |
| Link handler (source builds) | `~/.local/share/applications/rustket-url-handler.desktop` |

End-to-end encryption keys go through the same secret store as the tokens.

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
