# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Self-update for the packages published on GitHub (deb, rpm, AppImage,
  Windows installer and portable zip, macOS app): checks for new releases,
  verifies the download against the release checksums and installs it the way
  Rustket was installed. Builds without the `self-update` Cargo feature (the
  default) contain no updater.
- Cap on the sidebar Threads section (5, 10, 20, 50, 100 or unlimited,
  20 by default), in Settings and the Threads header menu. Background
  loading stops once the cap is filled.

## [0.2.2] - 2026-11-12

### Added

- Threads section in the sidebar with the modes off, unread, unread and kept,
  and all followed threads. Followed threads are remembered in the offline
  cache and loaded in the background at a pace that stays within the server's
  rate limits.
- "Keep in sidebar" for threads, available from the thread panel, the message
  menu and thread rows; kept threads are stored per server and account.
- User-defined sidebar groups that combine room types (teams, discussions,
  channels, private groups, direct messages); omnichannel stays on top.
- Custom order of sidebar sections, set in Settings, from a section header's
  menu, or by dragging section headers.
- Per-section sorting (newest, oldest, A-Z) and "unread first", with a global
  default. The open room or thread keeps its place until another one is opened.
- Sidebar card in Settings (English and Russian).

### Fixed

- A cleared server setting is no longer restored when the configuration is
  saved.

## [0.2.1] - 2026-11-10

### Added

- Thread replies that are also sent to the room now show a "Replied in a
  thread" line that opens the thread.
- Active sessions list the operating system of Rustket sessions; the security
  screen labels the current session "Rustket <version>" and other unnamed
  ones "Unknown app".

### Fixed

- OAuth and OpenID sign-in on Rocket.Chat 9.0, which removed the legacy flow:
  the browser no longer ends on the server's home page without returning to
  the app.
- SAML sign-in on servers with the Rocket.Chat 8.9.1 security hotfix, which
  stores the login under a server-generated token. Where the server does not
  hand SAML logins to apps, the pending view offers to continue with the
  browser session.

## [0.2.0] - 2026-11-09

### Fixed

- Room history on Rocket.Chat 9.0 servers (for example open.rocket.chat):
  rooms no longer stay empty. Older servers keep working as before.
- The "Open browser again" button no longer overflows on the browser sign-in
  screen in Russian.

## [0.1.0] - 2026-11-09

First public release.

### Added

- Native Rocket.Chat desktop client written in Rust with iced: no Electron and
  no embedded web view. Tested against Rocket.Chat 8.9.
- Multiple workspaces (servers) side by side, with unread badges, a custom
  title bar (or the system one) and pinned certificates for self-signed
  servers.
- Sign-in with password, LDAP, two-factor authentication (TOTP and e-mail
  codes) and personal access tokens; OAuth, OpenID Connect, SAML and CAS
  through the default browser, returning via `rocketchat://` links.
- Sidebar with sections (unread, favorites, teams, discussions, channels,
  private groups, direct messages, omnichannel), presence, unread and mention
  counters, filter and a quick switcher (Ctrl+K).
- Rich message rendering: markdown, mentions, emoji and custom emoji,
  attachments with image previews, link previews, quotes, reactions, read
  receipts, system messages and mouse-selectable message text.
- Composer with autocomplete for users, channels, emoji and slash commands,
  an emoji picker, uploads (file picker, drag and drop, pasted images), edits,
  quotes and drafts.
- Message actions: reply in thread, react, pin, star, forward, copy link,
  mark unread, report, delete and start a discussion.
- Threads, discussions, and room panels: room info, members, pinned, starred,
  mentions, files, search, notification preferences, invites, prune and
  export.
- Rooms, directory and account: create channels, groups, direct messages,
  discussions and teams; directory of users, channels and teams; status menu
  with custom statuses; profile and avatar; 2FA setup; active sessions.
- Teams management and video conferences (started and joined in the
  browser, with incoming call popups).
- Omnichannel agent UI: queue, taking and closing chats, transfers, visitor
  info, canned responses, availability and on-hold.
- Apps (UIKit): blocks in messages, modals, contextual bars, banners and app
  action buttons.
- End-to-end encryption compatible with Rocket.Chat 8.9: key setup and
  backup, room keys, encrypted messages and uploads, key reset.
- Native administration panel: workspace info, users, rooms, invites, custom
  emoji, sounds and statuses, integrations, OAuth apps, permissions and roles,
  a settings editor, apps, email inboxes, LDAP and moderation.
- Desktop integration: notifications honouring server preferences, sounds,
  tray icon with unread badge, single instance with link forwarding, start on
  login, idle detection and proxy settings.
- Offline cache for instant start and offline reading, with an outgoing queue
  that sends messages written offline once the connection is back.
- Keyboard navigation: focus rings, Tab order, F6 region jumps, Escape and
  Alt+Up/Down room switching.
- Hardware acceleration toggle with a CPU renderer fallback.
- English and Russian interface; light and dark themes.
- Packages for Linux (.deb, .rpm, AppImage), Windows (installer and portable
  zip, x64 and ARM64) and macOS (universal .dmg).

### Security

- Auth tokens and end-to-end encryption keys are kept in the desktop keyring,
  with a warned-about private file fallback when no keyring is available.

[Unreleased]: https://github.com/sokolcloud/rustket/compare/v0.2.2...HEAD
[0.2.2]: https://github.com/sokolcloud/rustket/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/sokolcloud/rustket/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/sokolcloud/rustket/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/sokolcloud/rustket/releases/tag/v0.1.0
