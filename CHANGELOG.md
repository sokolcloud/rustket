# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Enterprise policy: an organization can enforce 15 settings of Rustket
  through Windows Group Policy or Intune, macOS configuration profiles, and
  JSON files under `/etc/rustket/policies/managed/` on Linux. They cover
  the workspaces every user has, whether users may add or remove
  workspaces, which servers may be used, https only,
  certificate exceptions, how updates are handled, the password and
  personal access token sign-in, keeping secrets in a file, notification
  previews, the renderer, launch at login, the organization's name and
  support link, and a minimum version. Changes are picked up while Rustket
  runs. The guide for administrators is
  [docs/enterprise.md](docs/enterprise.md), the reference of every setting
  [docs/policies.md](docs/policies.md).
- Settings → Policies lists the policy settings in force, where each one
  comes from and whether it applies, with **Copy report**; the tab is there
  only when there is something to show. A setting the policy enforces is
  marked "Managed by your organization" (or with the organization's name)
  and cannot be changed; when the policy goes away, your own choice is
  back. The login screen says when sign-in methods are turned off.
- A workspace the policy does not allow is suspended: it stays in the list,
  marked "Suspended by your organization's policy", keeps its sign-in and
  its data, and does not connect until the policy allows it again. Uploads
  to it that are still running are cancelled and named in a warning.
- When the policy does not let users add or remove workspaces, the add and
  remove actions are hidden, a link to a server that is not in the list is
  refused with a notice, and when no workspace is configured a plain screen
  says that the organization manages the list.
- When the policy cannot be read or requires a newer version of Rustket, a
  screen replaces the window and says why, with the organization's name
  and support link, the policy details and **Copy report**; for a required
  update it offers **Check for updates** where Rustket may install updates.
- `rustket --dump-policy [FILE]` writes the policy in effect as JSON to
  FILE (or to standard output) and exits.
- The release has `Rustket-<version>-policy-templates.zip` for
  administrators: the Group Policy templates (ADMX and ADML, English and
  Russian, also for Intune), a macOS configuration profile, an Intune
  preference file and a Jamf Pro schema, the JSON schema and an example
  file for Linux, the reference of every setting, and PowerShell and shell
  scripts that check the policy of a computer for inventory and compliance
  tools.
- The deb and rpm packages own the empty folder
  `/etc/rustket/policies/managed/` and install the Linux schema and example
  under `/usr/share/doc/rustket/policy/`.
- **Keep a diagnostic log file** in Settings → Privacy & storage (off by
  default): after a restart Rustket also writes its log to a file, which
  **Show log folder** opens. At 2 MiB the file is renamed and a new one is
  started; only the previous file is kept. The log holds no passwords,
  tokens or message text.

### Changed

- A Windows installation for all users reports new versions and no longer
  offers to install them, unless a policy says otherwise.
- Launch at login shows as off when you turned the entry off in the
  system's own startup settings: Task Manager or Settings → Apps → Startup
  on Windows, Startup Applications on GNOME. Turning it on in Rustket turns
  it on there again.
- The log on standard output no longer contains colour codes when it is
  redirected to a file.

## [0.3.2] - 2026-11-26

### Added

- Each thread in a room's Threads panel has a bell on the right, as in
  Rocket.Chat: it shows whether you follow the thread, and a click follows
  or unfollows it without opening the thread. The unread dot moved under
  the bell. Under the preview, the avatars of up to two followers and "+N"
  for the rest come before the reply count.

### Removed

- **Show avatars in the room list** in Settings → Appearance: it never
  changed anything.

## [0.3.1] - 2026-11-26

### Changed

- Opening a room no longer marks all its unread messages read at once. As
  in Rocket.Chat, the room stays unread until you have seen its first
  unread message, and is then marked read when you reach the bottom. While
  the first unread message is above the screen or not loaded yet, a bar at
  the top of the timeline shows the unread count with **Jump to first
  unread** and **Mark as read**; Escape marks the room read at once.
  **Settings → General → Messages → Mark rooms as read on open** brings
  back the old behaviour.
- The Threads panel of a room shows each thread's first message in at most
  two lines, ending with "…", as Rocket.Chat does.
- Sidebar search matches a thread by its own text only, not by the name of
  its room, and the section arrows collapse sections while you search.

### Fixed

- **Clear after** of your status follows the expiry kept by the server
  (Rocket.Chat 8.6 and later): a status set in Rocket.Chat web or mobile to
  clear "until Nov 28" shows that in Rustket instead of "Never", and the
  time you pick in Rustket is sent to the server. A status that expires
  while notifications are paused with **Busy while paused** goes back to
  busy, not online.

## [0.3.0] - 2026-11-25

### Added

- A terms of use screen ("Before you start") on the first launch, shown
  once to existing users too: what is not tolerated, how reports and
  moderation work, the tools you have, how your data is handled, and links
  to the full terms and the privacy policy. Rustket does not connect to
  your workspaces, show notifications or open links until you click
  **I agree**, and the window stays on screen until then even with
  **Start minimized**; **Quit** closes the app.
- Block and unblock in direct messages: **Block** / **Unblock** in the User
  info panel of the other member (Block asks first). Neither of you can
  send messages in that direct message, and their messages in it are
  collapsed behind "Message from a blocked user" (a click shows one).
- **Report user** sends a report about a person to the workspace's
  moderators, from the "…" menu of their message (next to Report) or from
  their User info panel.
- Hidden words, in Settings → Privacy & storage: messages from others that
  contain one of your words or phrases (in the text, a file's name or
  description, or a quoted message) are collapsed in every room behind
  "Hidden by your hidden words", and their notifications show "New message"
  instead of the text. **Whole words only** is on by default; case is
  always ignored.
- **Delete my account…** in My account → Security, when the workspace
  allows it. You confirm with your password (or your username for accounts
  without one); if you are the last owner of some rooms, Rustket lists which
  rooms get a new owner and which are removed, and asks again.
- **Import…** next to the theme picker (Settings → Appearance) and the
  language picker (Settings → General): copies the theme or locale `.toml`
  files you pick into the right folder, after checking them. Nothing is
  copied if one of them is not usable.
- Downloads you save to a place you choose: the "Download finished" toast
  has **Show in Finder** (macOS, the file is selected) or **Show in folder**
  (Linux, Windows).
- Settings → Help has a "Support and contact" card with links to the
  support page, the privacy policy and the terms of use.
- macOS: the menu bar follows the interface language, and Rustket adds its
  own items and menus to it.
  - **Rustket**: Settings… (⌘,); About Rustket now opens Settings → About.
  - **View**: Enter Full Screen.
  - **Window**: Minimize, Zoom, Close (⌘W, hides the window), Rustket (⌘0,
    brings it back, as does a click on the Dock icon) and Bring All to
    Front.
  - **Help**: Rustket Help (Settings → Help), Support, Privacy Policy and
    Terms of Use.
- macOS: when notifications are turned off for Rustket in System Settings,
  **Send test notification** says so instead of only playing the sound, and
  Settings → Notifications shows a row; both have **Open System Settings**,
  which opens Rustket's notification page there.
- macOS: a Mac App Store build of Rustket (in the App Store once Apple has
  reviewed it). It updates through the App Store instead of the built-in
  updater, runs in the App Sandbox, keeps login tokens only in the Keychain
  (or in memory), and "Launch at login" registers Rustket as a login item
  (macOS 13 and later), which macOS may ask you to allow in System
  Settings → General → Login Items. The dmg from the release page keeps
  the built-in updater.

### Changed

- About (Settings → About) shows the developer, links to support, the
  privacy policy and the terms of use, **Third-party notices** (where the
  package includes them; not in the .deb), the Rocket.Chat server
  documentation and a note that Rustket is an independent client, not
  affiliated with Rocket.Chat. The "Rocket.Chat website" link is gone and
  "Documentation" is now "Rocket.Chat server documentation"; the tagline
  no longer calls Rustket a Rocket.Chat client. On macOS the notices are a
  `.txt` file that opens as a read-only copy outside the app, so macOS does
  not ask for "App Management" permission.
- A file you download to a place you choose is written to Rustket's cache
  first and moved there when complete, so no `.part` file appears next to
  it while it downloads.
- macOS: the window keeps the native traffic-light buttons. Rustket's own
  title row (workspaces and the settings gear) runs next to them; with
  "Use system window title bar" on, the workspaces stay in the left column
  under a plain title strip.
- macOS: notifications go through the current macOS notification system.
  macOS asks once whether Rustket may send notifications, with the first
  one; you can change it later in System Settings → Notifications → Rustket.
- macOS: with Language set to "System" (Settings → General), the interface
  follows the macOS preferred languages (System Settings → General →
  Language & Region) when Rustket is started from Finder or the Dock.
- macOS: the app lists its five languages to the system, the local network
  permission prompt explains why Rustket asks, in those languages, and the
  app is in the Business category.
- macOS: sounds, idle detection (automatic away) and opening downloaded
  files, folders and the third-party notices no longer start helper
  programs.

### Fixed

- macOS: Quit Rustket (⌘Q) quits like the tray's Quit: settings are saved
  and unsaved administration settings are asked about first.

## [0.2.11] - 2026-11-25

### Added

- Two schemes for unread indicators, in Settings → Appearance → Unread
  indicators. **Rocket.Chat**, the new default (existing installs included),
  looks like the Rocket.Chat 8.9 web client and the official desktop app.
  **Rustket (original)** keeps the previous, quieter look unchanged.
  Notifications and sounds are the same in both.
- Under Rocket.Chat:
  - Room badges are red for mentions of you, blue for unread threads,
    orange for `@all`/`@here` and grey for everything else counted. They
    show full numbers and no dots. Unread room names are brighter, and a
    room's notification settings leave its badge alone, as in Rocket.Chat.
  - Collapsed sidebar sections show the summed counter, and their tooltip
    lists what is unread.
  - In a room:
    - The Threads button shows a counter.
    - An unread thread summary is filled blue.
    - Mentions are coloured text, without `@` unless your account's
      preferences show the symbol.
    - Highlight words are white on red.
    - A message you jump to is pale yellow for two seconds.
  - The window title, tray, Dock, menu bar and workspace icons count all
    unread messages, like Rocket.Chat. The tray, the Dock and the workspace
    icons show `•` when there are only alerts.
  - The tray icon and the window icon (X11 and Windows) get Rocket.Chat's
    red badge (1–9, `+9` or a dot). Workspace icons get a grey badge.
  - The colours come from new `rc_*` theme tokens that themes may override.
- Settings → Help, the README and the theme documentation describe both
  schemes.

### Fixed

- The "…" menu of a message in a room or thread no longer vanishes on the
  first mouse move when it opens above the message, and no longer blinks
  when it opens below it.

### Changed

- macOS: the dmg and the app are signed with a Developer ID certificate and
  notarized by Apple, so Gatekeeper no longer blocks the first start.
  Signing and notarization run in the release workflow (see
  `docs/release-signing.md`). The first start after updating from an
  unsigned version may ask once for access to the saved login in the
  keychain.

## [0.2.10] - 2026-11-24

### Fixed

- Links in messages open again. In a scrolled list, the slightest mouse
  movement during the click cancelled it, so links in the room timeline,
  in thread replies and in the Activity and All unreads feeds did nothing,
  and only links near the top of a list (such as a thread's first message)
  worked. Clickable rows in Activity and in the Pinned, Starred and
  Mentions panels missed clicks the same way.
- Jumping to a message (from Activity, All unreads, search, Later, a
  message link or the Pinned, Starred and Mentions panels) no longer shows
  a narrow coloured strip and an otherwise empty room for a few seconds;
  the highlighted message is shown at full width.

### Changed

- macOS: the menu bar icon is a monochrome glyph that follows the light or
  dark menu bar, like the system's own icons: a dot for mentions and
  `@all`/`@here`, a ring for other unread.
- macOS: the Dock icon shows the number of mentions (direct messages
  included) as a badge.
- macOS: the app icon is a rounded square on the system icon grid, so
  macOS 26 no longer puts it on a grey plate.
- Settings → Help describes the menu bar icon and the Dock badge on macOS.

## [0.2.9] - 2026-11-20

### Added

- Settings → Help, "Indicators and notifications": every kind of signal
  (mention, DM, @all/@here, highlight word, thread reply, reminder, call,
  room settings, pause, Busy, …) has a short code and live samples of how the
  sidebar, collapsed sections, workspace icon, window title, tray, toast,
  sound and feed show it, computed from your current settings. Also opened
  from the status menu and from Settings → Notifications.
- Rocket.Chat's own notification sounds are bundled and replace the system
  message sound: the room's sound wins, otherwise your account's New message
  sound (new omnichannel chats use the New room sound), at your account's
  volumes; picking a sound in Account → Preferences plays it. When the
  system shows the notification, it plays the sound itself so Do Not Disturb
  applies: GNOME gets the sound file, the macOS app a copy in
  `~/Library/Sounds` (files named `rustket-*`, old ones removed again), and
  Windows plays its own notification sound with the toast. When Rustket plays
  a sound itself, on Linux it first checks KDE Plasma's and GNOME's Do Not
  Disturb.
- Highlighted words (Account → Preferences) mark the message row and the word
  in rooms and threads, matched like Rocket.Chat does.
- The room's Threads button shows a dot when the room has unread threads (red
  or orange for mentions), and the Threads panel marks them.
- Mentions in rooms larger than the server's `Notifications_Max_Room_Members`
  now notify: the server sends no notification there, so Rustket shows it
  itself from the room's last message.

### Changed

- One unread ranking everywhere: mentions and unread direct messages (red) >
  @all/@here (orange) > new messages (muted blue) > thread replies only
  (slate) > muted rooms (grey). A room that only gets an unread dot for plain
  messages shows a blue dot, and room names are bold where Rocket.Chat makes
  them bold.
- Room notification settings apply in every view: with "Receive
  notifications" off the room is grey (direct mentions stay red; Activity
  lists only its direct mentions), "Mute @all and @here mentions" removes
  the orange and the feed highlight, and with "Show mention badges" off
  there is no red or orange (Rocket.Chat web keeps red there).
- The workspace icon and window title (`Rustket (N)` / `Rustket •`) count
  mentions (direct messages included) only, and the tray dot is red only for
  them; other unread shows as a dot (orange, blue or slate on the workspace
  icon; orange or a quiet dot in the tray). The tray tooltip starts with the
  mentions: "Mentions: N · unread rooms: M".
- The tooltip of a collapsed section counts the unread rows you see, thread
  rows included and muted rooms excluded.
- The account preference "Mute the conversation you are looking at"
  (Rocket.Chat's muteFocusedConversations) is honoured: turned off, messages
  of the conversation on screen notify too. A thread reply counts as on
  screen only when its thread is open (or the reply is also shown in the
  room), so other replies in the open room now notify; such a toast may come
  about half a second later.
- Reminders play your account's New message sound instead of the system alarm
  sound. They follow "Play a sound", the account's "None" and volumes, and
  Busy: the reminder still shows, silently.
- "Play a sound" (Settings → Notifications) works without desktop
  notifications. **Upgrade note:** if you had desktop notifications off and
  "Play a sound" on (the default; it was greyed out), message sounds now play;
  switch "Play a sound" off to keep the old behaviour (this also silences
  reminder sounds).

## [0.2.8] - 2026-11-17

### Added

- Scheduled messages: the clock button next to Send (or Ctrl+Shift+Enter)
  sends the message at a time you pick. Scheduled messages are kept on this
  device and sent while Rustket runs; ones that could not be sent on time
  (Rustket closed, offline, or refused by the server) are listed at the next
  start to send now, reschedule or delete. Not available in encrypted rooms
  or for slash commands.
- Drafts & scheduled (Ctrl+Shift+D, a pinned row in the sidebar): unsent
  composer text of every room and your scheduled messages in one place, to
  open, edit, send now, reschedule or delete.
- Pause notifications for 30 minutes, 1 or 2 hours, until tomorrow or a
  custom time, from the status menu or the tray. Unread counters still
  update. By default your status is set to Busy while paused (so the server
  sends no phone pushes either) and the previous status comes back
  afterwards, unless you changed it meanwhile.
- Working hours (Settings → Notifications): notifications pause outside the
  hours set per weekday; a range may end on the next day.
- A custom status can clear itself after 30 minutes, 1 or 4 hours, today or
  this week.
- Search across rooms (Ctrl+Shift+F): words plus filters for author, room,
  date range, links, starred, pinned and mentions, picked in the filter bar
  or typed (`from:`, `mention:`, `in:`, `has:link`, `is:pinned`,
  `after:`/`before:`, …). Uses the server-wide search when the server
  allows it, otherwise searches your rooms one after another within the
  server's rate limits; messages already on this device show up at once,
  including decrypted messages of encrypted rooms.
- Room bookmarks: links of the topic, announcement and description show as
  chips under the room header; people who can edit the room can add one
  (Settings → General can hide them).
- Polls: "Create poll" in the composer sends a question with up to ten
  numbered options and adds the option reactions, so people vote with one
  click in any Rocket.Chat app; Rustket shows such messages as a poll card
  with the counts. See `docs/polls.md`.

## [0.2.7] - 2026-11-16

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
- Mention groups: define groups of people per account in Settings →
  Mentions (synced between your Rustket devices). Typing `@backend` offers
  the group; when sent it becomes `@backend (@alice @bob)`, so every member
  is notified on any client, encrypted rooms included. Editing a message does
  not expand a group twice, and a real username always wins over a group.
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

[Unreleased]: https://github.com/sokolcloud/rustket/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/sokolcloud/rustket/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/sokolcloud/rustket/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/sokolcloud/rustket/compare/v0.2.11...v0.3.0
[0.2.11]: https://github.com/sokolcloud/rustket/compare/v0.2.10...v0.2.11
[0.2.10]: https://github.com/sokolcloud/rustket/compare/v0.2.9...v0.2.10
[0.2.9]: https://github.com/sokolcloud/rustket/compare/v0.2.8...v0.2.9
[0.2.8]: https://github.com/sokolcloud/rustket/compare/v0.2.7...v0.2.8
[0.2.7]: https://github.com/sokolcloud/rustket/compare/v0.2.6...v0.2.7
[0.2.6]: https://github.com/sokolcloud/rustket/compare/v0.2.5...v0.2.6
[0.2.5]: https://github.com/sokolcloud/rustket/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/sokolcloud/rustket/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/sokolcloud/rustket/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/sokolcloud/rustket/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/sokolcloud/rustket/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/sokolcloud/rustket/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/sokolcloud/rustket/releases/tag/v0.1.0
