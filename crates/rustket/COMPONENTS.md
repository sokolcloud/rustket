# Rustket components

The app crate is split into components with their own state, `Message`,
`update`, `view`, `subscription` and locale file. Several agents work on them
in parallel, so ownership matters.

## Ownership and rules

| Owner | Modules | Locale files |
|---|---|---|
| timeline agent | `src/room/timeline.rs` (+ `room/timeline/…`), `src/room/render/` (shared message rendering) | `room_timeline.yml`, `room_render.yml` |
| composer agent | `src/room/composer.rs` (+ `room/composer/…`) | `room_composer.yml` |
| panel agent | `src/room/panel.rs` (+ `room/panel/…`), `src/room/header.rs` | `room_panel.yml`, `room_header.yml` |
| desktop agent | `src/screens/settings.rs`, `src/desktop/` (notifications, tray, idle, sound, autostart), `src/workspace/status_menu.rs` | `settings.yml`, `desktop.yml`, `status_menu.yml` |
| account agent | `src/screens/account.rs`, `src/screens/directory.rs`, `src/dialogs/create_room.rs`, `src/dialogs/quick_switcher.rs` | `account.yml`, `directory.yml`, `create_room.yml`, `quick_switcher.yml` |
| teams agent | `src/teams/` (team info / channels / members / discussions panel bodies, leave / delete / convert / remove-member dialogs), `src/videoconf/` (call layer: header call button, `video_conf` blocks, popups, call dialogs, call notifications) | `teams.yml`, `videoconf.yml` |
| omnichannel agent | `src/omnichannel/` (controller, sidebar groups, room callouts/quick actions/dialogs, panels, contact center; see `docs/omnichannel.md` §22.3a) | `omnichannel.yml` |
| uikit agent | `src/uikit/` (block renderer, mrkdwn parser, modal, contextual bar, banners, app buttons, slash-command triggers), `src/app/uikit.rs` (glue) | `uikit.yml` |
| e2e agent | `src/e2e/`, `src/screens/account/e2e.rs` | `e2e.yml` |
| title bar agent | `src/workspace/titlebar.rs` (custom title bar: workspaces, gear, window controls, drag/maximize/resize logic; workspace clicks go out as `rail::Message`), `rustket-ui` `widget/window_chrome.rs`; `UiPrefs::system_title_bar` switches back to the left rail live (`App::sync_decorations`) | `titlebar.yml` |
| shell (shared) | `src/app.rs`, `src/app/*.rs`, `src/message.rs`, `src/room/mod.rs`, `src/component.rs`, `src/session.rs`, `src/workspace/{mod,sidebar,rail}.rs`, `src/screens/{login,add_server,page}.rs` | `app.yml`, `sidebar.yml`, … |

Rules:
- Edit only your modules and your locale files. Submodules under your module
  (`room/timeline/…`, `desktop/…`) are yours too.
- If you must touch a shared file (`app.rs`, `app/components.rs`,
  `message.rs`, `room/mod.rs`, `component.rs`), keep the change minimal and
  additive: new enum variants and new match arms only, no reformatting or
  renaming, so parallel branches merge cleanly.
- Every user-visible string goes through `rust_i18n::t!` with a key from your
  own file (`_version: 2`, flat dotted keys, `en` and `ru` for each key; the
  i18n test fails on a missing translation or an undefined key).
- No blocking work in `update`/`view`: use `Task`s; files and secrets in
  `spawn_blocking`.
- Tests: unit tests next to the code; app-level flows in `tests/app_flows.rs`
  (drive `App::update`); the headless screenshot example `examples/snapshot.rs`
  renders the real app against the dev server.

## App-level components (`src/component.rs`)

Screens (full page next to the rail), dialogs (modals) and the status menu
follow one pattern:

```rust
pub struct X { /* state */ }
#[derive(Debug, Clone)] pub enum Message { /* … */ }
impl X {
    pub fn new(..) -> Self;
    pub fn update(&mut self, msg: Message, ctx: &mut component::Ctx<'_>) -> component::Action<Message>;
    pub fn subscription(&self) -> Subscription<Message>;
    pub fn view<'a>(&'a self, ctx: &component::ViewCtx<'a>) -> Element<'a, Message>;
}
```

`screens/settings.rs` is not per server: it gets `SettingsCtx { config,
ui_state }` in `update` and `SettingsView { config, handler_registered,
secret_backend, … }` in `view`.

- `component::Ctx`: `server_id`, `server`, `workspace: &mut Workspace`,
  `session: Option<&SessionHandle>`, `images`, `config`; helpers `rest()`,
  `rest_task(..)`, `session_task(..)` (results are `Res<T>`).
- `component::ViewCtx`: the same read-only plus `connected`, `now`.
- `component::Action { task, effects }` with `component::Effect`:

| Effect | Meaning |
|---|---|
| `Toast(kind, title, body)` | transient notification |
| `Navigate(Nav)` | `Settings`, `Account`, `Directory`, `CreateRoom(Kind)`, `CreateDiscussion { rid, pmid }`, `QuickSwitcher`, `Workspace` |
| `OpenRoom(rid)` | open a room of the component's server |
| `OpenUrl(url)` | browser (absolute http/https/mailto only) |
| `Close` | close this screen/dialog |
| `SaveConfig` | persist `AppConfig`/`UiState` after changing them |
| `Logout { everywhere }` | log out of the server (`everywhere`: revoke a PAT too) |
| `RemoveServer` | remove the server (with confirmation) |
| `RegisterUrlHandler` | the `rocketchat://` handler dialog |
| `ClearImageCache` | drop every server's image cache (memory and disk) |
| `TestNotification` | show a test desktop notification |
| `RecentStatusTexts(list)` | store the status menu's recent texts in `AppConfig::recent_status_texts` and save |

Wiring (already done in `app/components.rs`, `app/view.rs`): rail gear →
settings; sidebar header avatar → status menu (`workspace/status_menu.rs`,
which renders the trigger and its popover; its "My account" entry → account);
sidebar header buttons → directory and create room; Ctrl+K → quick switcher.

### Desktop (`src/desktop/`)

`Desktop` is owned by the app (`Desktop::new()` is inert for tests and
snapshots; the binary sets `Flags::desktop` for `Desktop::system`). Hooks the
app calls:
- `on_notification(server_id, server_name, &DesktopNotification, &Workspace,
  &UiPrefs, &NotifyEnv) -> Task<desktop::Message>` for every
  `{uid}/notification` event (after it was applied); `NotifyEnv` carries the
  images, server URL, active server/room/thread and window focus;
- `on_unread_changed(unread_rooms, mentions)` whenever the totals over all
  servers change; `on_state(&[(sid, &Workspace)], active)` after every update
  (closes notifications of read rooms, tray status);
- `on_focus_changed`, `on_close_requested` (close/minimize to tray),
  `show_window` (second launch, tray), `idle_calls`, `test_notification`.

`Desktop::update` returns `(Task, Vec<Request>)`; requests:
`OpenRoom { server_id, rid }`, `OpenThread { server_id, rid, tmid }`,
`ShowWindow`, `Quit`, `IdleTime(Duration)`, `SetStatus(UserStatus)`,
`RoomStep(RoomStep)` (Alt(+Shift)+Up/Down, from `Desktop::subscription`, which
also sees keys captured by widgets such as the composer).

## Room components

The room area of the active server is one `RoomView` (`room/mod.rs`) per open
room. It owns four components, each in its own module, with its own `Message`
and its own locale file: `Header`, `Timeline`, `Composer`, `Panel` (right
contextual panel).

Do not change the signatures below; add variants to your own `Message`, fields
to your own struct, and private helpers/submodules.

### Room component interface

```rust
pub struct X { /* component state, per open room */ }

#[derive(Debug, Clone)]
pub enum Message { /* … */ }

impl X {
    pub fn new(rid: &str) -> Self;
    pub fn update(&mut self, msg: Message, ctx: &mut UpdateCtx<'_>) -> Action<Message>;
    /// Called after realtime events / syncs changed the workspace.
    pub fn on_changes(&mut self, changes: &Changes, ctx: &mut UpdateCtx<'_>) -> Action<Message>;
    pub fn subscription(&self) -> Subscription<Message>;
    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message>;
}
```

Exception: `Panel::view` returns `Option<Element>` (`None` = closed), and
`Panel` additionally has `open(Kind)`, `close()` and `kind()`, used by
`RoomView` for `Effect::OpenPanel` / `Effect::ClosePanel`.

Subscriptions are mapped with non-capturing functions only (iced requirement);
`RoomView`/`App` add the server id and room id with `Subscription::with`.

### Context

`ViewCtx<'a>` (read-only, for `view`):
- `server_id`, `server: &ServerUrl`, `rid`
- `workspace: &Workspace` — rooms, subscriptions, timelines (`timeline(rid)`,
  `thread(tmid)`), presence, typing (`typing_label`), settings, permissions
  (`rustket_core::permissions::*`), emoji, drafts.
- `images: &Images` — `images.avatar(server_id, server, &Avatar, name)` returns an
  `AvatarSource` (initials until loaded); `images.get(server_id, url)` for any
  other image (attachments). Request what you will show from `update` /
  `on_changes` with `ctx.images.want(ImageRequest { .. })` so it is fetched
  right away; misses hit in `view` are fetched right after the frame. Downloads
  are size-limited; SVG is never decoded (SVG avatars show initials); raster
  images are decoded with limits and downscaled (avatars to 128 px, others to
  1600 px). Never block in `view`.
- `prefs: &UiPrefs` (density, 24h clock, real names override …)
- `connected` (realtime up), `panel: Option<&panel::Kind>`, `now: Instant`

Room messages are tagged with the session epoch by the app: results of a
previous session (after logout / re-login) never reach a component.

`UpdateCtx<'a>` (for `update` / `on_changes`):
- same identifiers, `workspace: &mut Workspace` (e.g. `add_pending`,
  `set_draft`, `open_thread`, `begin_load_thread`/`finish_load_thread` —
  newest-first offsets, paired with `RestClient::thread_messages_latest`),
  `session: Option<&SessionHandle>`, `images`, `prefs`.
- `ctx.rest()` → `Option<RestClient>` (authenticated).
- `ctx.rest_task(|rest| async move { rest.send_message(&m).await }, Message::Sent)`
  runs a REST call; the result is `Res<T> = Result<T, Failure>` (cloneable,
  `Failure::user_text()` for a localised error, `failure.error()` for matching
  `rocketchat::Error` variants such as `TwoFactorRequired`).
- `ctx.session_task(|s| async move { s.session().set_user_activity(&rid, None, &["user-typing"]).await }, Message::Typed)`
  runs a DDP call (`Session::call`, `set_user_activity`, `watch_room`, …).
- Without a session both helpers resolve to an `Unauthorized` failure.

### Effects (component → parents)

Return them in `Action { task, effects }` (`Action::none()`,
`Action::task(t)`, `Action::effect(e)`, `.with(e)`):

| Effect | Handled by | Meaning |
|---|---|---|
| `Toast(BannerKind, title, body)` | App | transient notification (strings already translated) |
| `OpenRoom(rid)` | App | switch to another room of the same server |
| `LoadOlder` | RoomView | load the next older history page (`HistoryCursor`); the timeline sees the result through `on_changes` (`changes.timelines`) |
| `MarkRead` | RoomView | automatic `subscriptions.read {rid}` (unread threads stay), only while the room is readable; one request at a time, a mark asked for meanwhile runs after it |
| `MarkAllRead` | RoomView | the explicit "Mark as read": `subscriptions.read {rid, readThreads}` |
| `OpenPanel(Kind)` / `ClosePanel` | RoomView | right panel; opening the shown kind toggles it closed |
| `OpenUrl(url)` | App | external browser; only absolute `http`, `https`, `mailto` URLs (others are refused) |
| `RecentEmoji(key)` | App | an emoji was used (picker): pushed to `AppConfig::recent_emoji` and saved |
| `CloseRoom` | App | close this room's view (e.g. after hiding the room); the room stays in the workspace |
| `ShowMessage(msg)` | RoomView | a thread reply opens its thread; otherwise the timeline jumps to the message (loading older pages if needed) and highlights it |
| `MarkUnread(mid)` | App | leaves the room (like the web client), then `subscriptions.unread`; toasts the result |
| `StartDiscussion { pmid }` | App | the create dialog for a discussion of this room (`CreateRoom::discussion_in`, `from_message`) |
| `Call(CallAction)` | App | video call: start (header), join / call back / info (`video_conf` block); routed to `crate::videoconf` |

What `RoomView` already does: `workspace.open_room` on open, keeps the
`watch_room` guard (dropped with the view when the room is closed or removed),
loads the first history page (and the next one automatically when a page held
only hidden thread replies), re-subscribes the watch and reloads a missing or
failed first page after a reconnect, reloads after `MessagesImported`, marks
the room read on open and when new messages arrive while the window is focused. The app applies every realtime event to the
`Workspace` before `on_changes` runs, so components never talk to the session
stream directly.

### i18n

`rust-i18n` with one file per component in `crates/rustket/locales/`, format
`_version: 2`, flat dotted keys with `en` and `ru` for every key:

```yaml
_version: 2
room_composer.placeholder:
  en: "Message"
  ru: "Сообщение"
```

Use `rust_i18n::t!("room_composer.placeholder")` (`%{name}` placeholders:
`t!("key", name = value)`). Every user-visible string goes through it. Keys
computed by `rustket-core` (`room_list` previews, typing labels, section names,
`PostBlock::i18n_key`) live in `preview.yml`, `typing.yml`, `sidebar.yml`;
add missing ones to your own file under your own prefix if you need different
wording. Locale changes are picked up by `build.rs` (`rerun-if-changed=locales`).

### Testing

- Unit-test component logic without iced (state + `update` with a
  `Workspace` built in the test; `UpdateCtx { session: None, .. }`).
- Visual check: `cargo run -p rustket --example snapshot -- <dir>` renders the
  real app headlessly against the dev server (see the example's doc comment);
  extend it with your component's state.

### Panel kinds (`room/panel.rs`)

`Kind::{Info, Members, Threads, Thread(tmid), Discussions, Pinned, Starred,
Mentions, Files, Search, NotificationPrefs, RoomSettings, UserInfo(username),
Invite, TeamChannels, TeamMembers, TeamInfo}` (`Discussions`, `TeamChannels`,
`TeamMembers` and `TeamInfo` are hosted by `crate::teams::TeamPanel`), opened with `Effect::OpenPanel(kind)` (opening the
shown kind closes it).

### Composer intents

`Effect::Compose(ComposeIntent)` lets any room component ask the composer for
something; `RoomView` routes it to `Composer::intent(intent, ctx)`:
`Edit(mid)`, `Quote(mid)`, `Mention(username)`, `InsertText(text)`,
`React(mid)` (emoji picker for a reaction), `ReplyInThread(tmid)` (also opens
`Kind::Thread(tmid)`), `Retry(mid)` / `Discard(mid)` (failed sends: the
single outbox path; the rows' `interact` actions only emit these, the composer
has no failed list of its own). The thread view routes its rows' intents to
its own composer, which only takes replies of its thread; the room composer
takes any pending message of the room (a thread reply shown in the room while
its thread is closed included).

### Room lifecycle (glue)

- `RoomView::close(ws, session)` stores drafts, sends the stop-typing activity
  (`Composer::leave`; `Panel::leave` closes the panel and retires its bodies:
  an open thread keeps its draft, closes its workspace timeline and stops
  typing with its `tmid`) and forgets a previewed room
  (`Workspace::forget_preview`). `RoomView::hide` stops typing (room and
  thread composer) when another server becomes active. Closing or replacing
  the thread panel stops its typing too (`Panel::retire(ws, session)`).
- The automatic read mark of the focused room (`RoomView::wants_auto_read`)
  runs only for messages from others newer than what was seen (last read
  mark, history loaded after the open's read mark, the server's `ls` of a
  read room), or when only `alert` is set without unread threads, so an
  unread mark made elsewhere is not undone. Own and pending messages never
  count. It runs only while the room is readable (`RoomView::readable`):
  the app sets `RoomView::set_visible` after every update (window focused,
  server active, `Screen::Server`, no contact center or app dialog, the
  server's active room); the timeline must be at the bottom, not under the
  E2E cover or an expanded thread. Opening an unread room marks it once
  it is readable; becoming readable again catches up; a failed mark is
  retried on the next readable transition or subscription change. The open thread reads
  itself only while visible (`Thread::set_visible`).
- Uploads live in a registry (`room::upload`) and outlive room and thread
  views; results for a view that is gone go through `room::orphan_result`
  / `composer::orphan_result` (failed upload: toast; failed thread reply:
  marked failed). Signing out (`ServerState::reset`) aborts the server's
  running uploads (`upload::cancel_server`).
- `ServerState::close_rooms` closes the room views and returns their
  stop-typing task; `ServerState::reset` (unchanged signature) drops them
  silently.
- A room without a subscription is a preview (`RoomView::is_preview`): no read
  mark, the composer shows the Join bar; without `preview-c-room` the history
  is not requested and an explanation is shown instead; once joined, the
  history is loaded (`RoomView::on_changes`). Permalinks (`?msg=`, clicked or
  `rocketchat://`) to a channel or group the workspace does not have fetch it
  by name (`open::Target::Name`, `rooms.info?roomName=`) and open it the same
  way; a clicked link the server does not resolve opens in the browser.
- The header gets the open panel (`Header::set_panel`): menu entries open or
  keep their panel; the header buttons toggle.

### Composer (`room/composer.rs`, owned by the composer agent)

`Composer::for_thread(rid, tmid)` builds a thread composer (sends with `tmid`,
own draft and typing context); call `on_open(ctx)` when it is shown and
`on_close(ws)` when it goes away (draft). `room/emoji_picker.rs` is reusable:
keep an `EmojiPicker`, forward its messages to `update`, act on
`Event::Picked(key)` and return `Effect::RecentEmoji(key)`. Dropped files
(`window::Event::FileDropped`) reach the active room's composer
(`composer::Message::FilesDropped`).
Outbox send results are recorded per process, so a result that arrives while
the room is closed is applied by `on_open` when it reopens.

### Shared message rendering (`room/render/`)

Owned by the timeline agent; the panel (threads, pinned, search results …)
renders message bodies through it:

```rust
pub fn message_body<'a>(msg: &'a Message, ctx: &ViewCtx<'a>) -> Element<'a, RenderMsg>;
pub enum RenderMsg { OpenUrl(String), OpenRoom(String), OpenUser(String), ToggleSpoiler(String, usize), Copy(String) }
```

Callers map `RenderMsg` into their own `Message` and turn it into effects
(`OpenUrl`, `OpenRoom`, `OpenPanel(UserInfo(..))`, …). New variants may be
added by the timeline agent; callers should keep a catch-all arm.

Message lists share more than the body renderer (`room/timeline/`):
- `viewer::Viewer` (+ `ViewerMsg`): interactive bodies (`Viewer::body`),
  spoilers/collapse state, downloads with progress, media opened with the
  system player, the image lightbox (`Viewer::overlay`). Every panel that
  renders messages keeps one.
- `interact::Interactions` (+ `ItemMsg`): row actions (permission-gated),
  the "more" menu, retry/discard of failed sends (`ComposeIntent::Retry` /
  `Discard`), mark unread (`Effect::MarkUnread`), start a discussion
  (`Effect::StartDiscussion`, not in the thread view, as in the web's
  `threads` context), delete/report dialogs; for `Source::Room` or
  `Source::Thread(tmid)`. `Effect::Compose` requests are routed by the owner
  to its composer.
- `list::{plan, row, Common}`, `row::{Env, RowFlags}`: grouping, day and
  unread separators and the `lazy` rows, used by the timeline and the thread.
- `list::Follow`: built window, bottom following and the frozen view while
  scrolled up (new messages wait below "jump to latest"), shared likewise.
- Dialogs and the lightbox are drawn by `RoomView` over the whole room
  (`Timeline::overlay`, `Panel::overlay`).

The thread view (`room/panel/thread.rs`) owns `Composer::for_thread` with
"Also send to channel" (`Composer::set_tshow`), pages replies newest first
(`RestClient::thread_messages_latest`), and marks the thread read
(`chat.readThread`) on open and, while focused at the bottom, when replies
arrive (`RoomView::mark_thread_read`, called by the app). Panel bodies that
hold workspace state are released through `Panel::retire`. The thread
reloads itself when its timeline is reset (resync, history import). Expanded,
it is drawn over the room column (max 855 px, like the web) and later threads
of the session open expanded too. `Effect::OpenPanel(Kind::Thread(t))` for
the open thread keeps it open (only header buttons toggle panels).

### UIKit (`src/uikit/`, owned by the uikit agent)

Message blocks render through `render::message_body_with` (blocks hide the
markdown body); their interactions come out as `RenderMsg::UiKit` and every
owner forwards them as `Effect::UiKit` (also panel, header, message menu,
composer app menu; the status menu uses `component::Effect::UiKit`). The app
runs them through `uikit::controller` (`Message::UiKit(sid, epoch, ..)`;
results of an earlier session are dropped). Slash commands from a composer are
`UiKitMsg::Command` effects, so `commands.run` finishes even when the room
view closed. Durable state lives in the workspace (`UiKitStore`, incl. the
widget `Scratch`); modal text editors in `ServerState::uikit`. Contextual bars
are `panel::Kind::App(view_id)` and close with their room view
(`controller::room_closed`); modals (keyed by view id), the confirmation
dialog, the floating busy pill and banners are drawn by the app
(`uikit::overlay`, `App::uikit_busy_over`, `uikit::strips`). `stream-apps`
events are consumed by the UIKit dispatcher arm; what other components need
comes out as `controller::Effect::AppsChanged { commands, admin }` (the glue
drops the composer's command cache; `admin` reloads an open admin Apps
page). `video_conf` blocks are left to the video-conference module
(`uikit::render` skips them).
### End-to-end encryption (`src/e2e/`)

One `rustket_core::e2e::E2eManager` per logged-in server, reachable from
every component as `ctx.workspace.e2e()`; `ServerState::e2e`
(`e2e::ServerE2e`) owns its lifecycle (`e2e::after_update`, called by the
app before the workspace changes are taken: start once the initial load is
in, feed changed rooms/subscriptions and settings, restart when `E2E_Enable`
turns on; `e2e::forget_secrets` on logout / server removal, after
`e2e::check_sign_out` asked the secret store and showed an unsaved generated
passphrase; `e2e::forget_keys` on every rejected token (failed resume,
`force_logout` aimed at this session), which keeps that passphrase).
- Encrypted messages are decrypted in place before anything is drawn
  (`e2e::shown`, `Workspace::rewrite_messages`): `msg`/`md`/attachments/files
  hold the plaintext, `content` keeps the envelope, `Message::e2e` a marker
  (`rustket_core::e2e::shown`, carrying a per-process nonce) that is only
  believed for messages this process processed. Use `e2e::shown::is_decrypted(m)` / `state(m)`; the original
  payload for an edit comes from `e2e::shown::raw_view(m)`. Lists loaded
  outside the workspace (panels) call `e2e::shown::process_list`.
- The renderer shows placeholders for messages that are not decrypted
  (`e2e::view::placeholder` in `render::message_body_with`).
- Encrypted file URLs end in `e2e::files::FRAGMENT` plus a per-message token
  (`e2e::files::is_marked`); the image loader and downloads decrypt them
  (`e2e::files`), and a marked URL without a known key is never downloaded
  as ciphertext. Never write their bytes to a cache. The files panel
  decrypts upload metadata with `e2e::files::process_file_items`.
- Sending: `e2e::outgoing::{mode, prepare_send, prepare_edit, upload_mode}`;
  an encrypted message is never sent or edited as plaintext (the composer
  keeps the message being edited and checks `e2e::room::check_edit`).
- Room UI: `e2e::room` (header key icon, menu entries and dialogs kept by
  `Header`, body replacement, composer notices, `filter_actions`). Room
  components ask for the account modal with `Effect::E2e(RoomRequest)`.
