# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Slack-style keyboard shortcuts: Escape marks the open room as read when
  there is nothing left to close (can be turned off in Settings → General),
  Shift+Escape marks every room of the server as read after a confirmation,
  Ctrl+Shift+\ reacts to the last message, Ctrl+, opens the preferences, and
  the message field gets Ctrl+Shift+X / C / U / 9 and Ctrl+Alt+Shift+C for
  strikethrough, inline code, link, quote and code block (Cmd on macOS).
- A keyboard shortcuts overview: Ctrl+/ (Cmd+/ on macOS), the user menu or
  Settings → General. The formatting buttons show their shortcut.
- Marking many rooms read goes through one paced queue over the realtime
  connection (much faster than the per-request web route on servers with the
  default rate limits), falling back to the web API when needed.
- Long messages and code blocks longer than 15 lines are folded with
  "Show more"; Copy still copies the whole block.
- Who reacted: a button after the reactions and a "Who reacted" menu entry
  open a list of everyone per emoji; a click opens the person's profile.
- Sending `@all` or `@here` to a room with 5 or more other members (also as
  an upload's message) asks for a confirmation first, with the number of
  people it notifies (can be turned off in Settings → General).
- 1:1 direct messages show the other person's local time above the message
  field when it differs from yours ("It's 03:12 for Bob").
- Forward a message, or select up to 20 messages and forward them or copy
  their links: pick a room or person, add an optional comment and send. The
  old "forward" entry of 0.1 meant copying a link.
- All unreads (Ctrl+Shift+A): every room with unread messages in one list,
  direct mentions and DMs first, then @all/@here, then the rest, with the
  unread messages of each room; reply, reply in thread, show in the room,
  mark a room read or skip it. "Mark all as read" there marks the listed
  rooms and leaves unread threads for Activity. Catch up goes through the
  rooms one at a time (Enter: read and next, →: skip, ←: back).
- Activity (Ctrl+Shift+M): unread mentions, @all/@here and thread mentions,
  then unread threads across rooms. Both views have pinned rows at the top of
  the sidebar (Settings → Sidebar can hide them); only mentions get a loud
  badge.
- Message reminders: "Remind me…" in a message's menu (in 20 minutes, 1 or
  3 hours, tomorrow, next week or a custom time). Reminders are synced
  between your Rustket devices and show a desktop notification while
  Rustket runs (Open, Snooze 1 hour, Done); ones missed while it was closed
  are summed up at the next start. Settings → Notifications has their
  switches and the morning hour of "Tomorrow" and "Next week".
- Later (Ctrl+Shift+S, a third pinned sidebar row with a count of overdue
  reminders): your starred messages from every room as a to-do list with In
  progress, Reminders, Done and Archived tabs, due dates and "Remove done
  items". Stars that existed before go to Archived. Status, due dates and
  reminders sync through your hidden service room as ids and times only,
  never message text.

### Changed

- Like the web client, Rustket now stores your UTC offset on the server
  after sign-in so others see your local time; Settings → Privacy & storage
  can turn this off.
- Message menus are grouped with separators (respond, share, organise,
  delete).

## [0.2.6] - 2026-11-15

### Security

Fixes from a full security review of the code base.

- A message with an extreme date could crash the client whenever its room
  was shown; such dates are now rejected.
- A message with a crafted type, or an external translation file with a
  malformed placeholder, could corrupt text in memory and crash the app.
- End-to-end encryption: a server could set a username pattern that made
  the client send the text of encrypted messages in clear as "mentions";
  mentions are now found by the client itself and only short `@name` /
  `#channel` tokens can leave. Encrypted file uploads no longer reveal a
  hash of the real file name.
- For administrators, private server settings (passwords, secrets) that
  changed during a session were written to the offline cache; they no
  longer are, and settings cached by earlier versions are dropped once.
- Images from messages are no longer fetched from your own machine or
  local network addresses, nor over plain http when the server uses https;
  a crafted image path can no longer make the client send authenticated
  requests to other server endpoints behind a normalising proxy.
- Windows: sign-in links forwarded to the running instance are now
  authenticated, so another local user cannot intercept them.
- Synced flags only use the flag room bound to your own account; a room
  another user creates in your name is ignored.
- Opening a browser sign-in link with someone else's SAML token now always
  asks to confirm the account, also when two-factor authentication is on.
- Signing out reliably forgets the session even when the system keyring was
  unavailable at some start.
- App buttons in modals, side panels and banners can only reach the app
  that showed them; blocks and admin translations from one server can no
  longer appear in another server's views.
- Linux updates: the downloaded package is verified again by the privileged
  installer step itself, so it cannot be swapped while the password prompt
  is open.
- The untrusted-certificate dialog shows certificate names on one line,
  without control characters, so they cannot imitate the fingerprint.
- Server-supplied text is escaped in logs.

## [0.2.5] - 2026-11-13

### Added

- External themes: TOML files in the `themes` folder of the config
  directory, picked in Settings → Appearance → Theme (with "Open themes
  folder" and "Reload"). A theme sets a few palette seeds (accent,
  background, text, danger, success, warning) and/or exact colour tokens
  for a light and/or dark variant, and optionally fonts and a corner-radius
  scale. Problems and low-contrast colour pairs are listed as warnings.
  `rustket --dump-theme` prints a complete template; see
  `docs/themes/README.md` and the example themes Forest and Graphite.
- Languages: Spanish, Thai and Simplified Chinese join English and Russian,
  and translations can be added or fixed with TOML files in the `locales`
  folder of the config directory (a file named after a bundled language
  overrides it string by string; missing strings show in English).
  Settings → Language lists every language with "Open languages folder"
  and "Reload", shows how complete an external translation is and what is
  wrong with its file. Plural rules per language (one-other, east-slavic,
  none). The system language now also matches Spanish, Thai, Chinese and
  external languages. `rustket --dump-locale [CODE]` prints a complete
  template for translators; see `docs/locales/README.md`. Thai and Chinese
  text needs Thai/CJK fonts installed (on Linux).

### Changed

- The light/dark/system control in the settings is now called "Mode".
- The language setting is a list instead of a segmented control; the
  config stores a locale code (`ui.language`, old values keep working).
- The admin screen shows server setting labels in the interface language
  (not only English or Russian).

### Fixed

- Medium and semibold text in scripts whose installed fonts only have
  Regular and Bold (for example Chinese and Thai Noto fonts) no longer
  falls back to unrelated fonts.

## [0.2.4] - 2026-11-13

### Added

- A collapsed sidebar section shows a dot in the color of its most important
  unread item (rooms and threads) and their number in a tooltip, so you no
  longer have to expand every section to find what is new.
- Screenshots of the light and dark themes in the README.

### Changed

- Quieter unread colors: plain unread counters (no mention) use a muted blue,
  rooms where only threads are unread a darker slate, and thread rows with new
  replies a grey dot. Mentions and @all/@here keep their red and orange.
- A room whose only unread items are threads gets the thread color, not the
  unread one (its counter includes the thread replies).

### Fixed

- Rooms whose unread status is hidden in their notification preferences no
  longer show a thread or mention badge.

## [0.2.3] - 2026-11-12

### Added

- Self-update for the packages published on GitHub (deb, rpm, AppImage,
  Windows installer and portable zip, macOS app): checks for new releases,
  verifies the download against the release checksums and installs it the way
  Rustket was installed. Builds without the `self-update` Cargo feature (the
  default) contain no updater.
- Cap on the sidebar Threads section (5, 10, 20, 50, 100 or unlimited,
  20 by default), in Settings and the Threads header menu. Background
  loading stops once the cap is filled.
- Threads whose parent message you starred are listed in Favorites, in every
  Threads mode; star and unstar them from thread rows and the thread panel.
- Colored flags on rooms and threads: one of six colors with your own names,
  each used color gets its own sidebar section. Set them from the new room
  context menu in the sidebar, the room header menu, thread rows or the thread
  panel. Flags sync between your devices through a hidden private group on
  the server.

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

[Unreleased]: https://github.com/sokolcloud/rustket/compare/v0.2.6...HEAD
[0.2.6]: https://github.com/sokolcloud/rustket/compare/v0.2.5...v0.2.6
[0.2.5]: https://github.com/sokolcloud/rustket/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/sokolcloud/rustket/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/sokolcloud/rustket/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/sokolcloud/rustket/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/sokolcloud/rustket/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/sokolcloud/rustket/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/sokolcloud/rustket/releases/tag/v0.1.0
