# Wraps the tree staged by packaging/rpm/build.sh (the `dist` binary is built
# outside rpmbuild); this is not a source RPM spec. build.sh defines
# rustket_version, rustket_release and rustket_stage.

# The dist profile keeps the symbol table on purpose (RUST_BACKTRACE=1 crash
# reports name functions): no debuginfo subpackage, no brp-strip and no other
# post-install processing (build.sh compresses the man page itself).
%global debug_package %{nil}
%global _enable_debug_packages 0
%global _build_id_links none
%global __os_install_post %{nil}
%global __strip /bin/true

Name:           rustket
Version:        %{rustket_version}
Release:        %{rustket_release}
Summary:        Native Rocket.Chat desktop client
# Rustket itself; the statically linked crates and bundled assets are listed
# with their licenses in /usr/share/doc/rustket/THIRD-PARTY-NOTICES.
License:        MIT
URL:            https://github.com/sokolcloud/rustket

# Libraries loaded with dlopen (not in the binary's NEEDED entries, so the
# automatic generator does not see them). Mirrors packaging/deb/depends,
# recommends and suggests. Libraries are required by soname, which Fedora and
# openSUSE both provide under different package names. winit's X11 backend
# needs these to open a window at all (also under XWayland).
Requires:       libxkbcommon.so.0()(64bit)
Requires:       libxkbcommon-x11.so.0()(64bit)
Requires:       libX11.so.6()(64bit)
Requires:       libX11-xcb.so.1()(64bit)
Requires:       libXcursor.so.1()(64bit)
Requires:       libXi.so.6()(64bit)
# Wayland and Vulkan/EGL for the GPU renderer (the CPU renderer works without
# them), D-Bus plus a portal backend or zenity for file dialogs, a sound player
# and theme for notification sounds, xdg-utils for links.
Recommends:     libwayland-client.so.0()(64bit)
Recommends:     libwayland-egl.so.1()(64bit)
Recommends:     libEGL.so.1()(64bit)
Recommends:     libvulkan.so.1()(64bit)
Recommends:     libdbus-1.so.3()(64bit)
Recommends:     (xdg-desktop-portal or zenity)
Recommends:     (pipewire-utils or pipewire-tools or pulseaudio-utils)
Recommends:     sound-theme-freedesktop
Recommends:     xdg-utils
Suggests:       (gnome-keyring or kwalletmanager5 or keepassxc)

%description
Rustket is a lightweight Rocket.Chat client written in Rust that renders
rooms, threads, end-to-end encrypted chats, apps, omnichannel and
administration natively instead of embedding a web browser.

Browser sign-in (OAuth, OpenID, SAML, CAS) and video calls use the default
browser and return through rocketchat:// links, which this package registers.

%prep

%build

%install
cp -a "%{rustket_stage}/." "%{buildroot}/"

%files
/usr/bin/rustket
/usr/share/applications/rustket.desktop
/usr/share/metainfo/rustket.metainfo.xml
/usr/share/icons/hicolor/*/apps/rustket.png
/usr/share/man/man1/rustket.1.gz
%dir /usr/share/doc/rustket
%doc /usr/share/doc/rustket/THIRD-PARTY-NOTICES
%dir /usr/share/licenses/rustket
%license /usr/share/licenses/rustket/LICENSE
